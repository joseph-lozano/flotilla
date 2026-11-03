//! The replay test (the plan's R2): engine events and requests go through a real server and
//! socket into a real client's copy of the views, and after every step the views the client
//! holds must equal the views computed straight from SQLite. The test plays the engine: it
//! writes the database as the engine would, then says what changed.
//!
//! On the way the connection drops (through a proxy the test can cut): briefly, so the client
//! resumes from the deltas the server kept; for longer, with more changes than it keeps, so
//! the views come whole; and the server restarts, so everything comes whole from a new run.
//!
//! It runs twice: over TCP with a device's token, as another device reaches a server, and over
//! the in-memory pipe the app on This Mac uses to reach its own.

use flotilla_client::{Client, Info, Pipe, Target};
use flotilla_core::event::RawEvent;
use flotilla_core::store::{Project, SetupRun, Store, Thread, ThreadState};
use flotilla_engine::host::{Mode, Role};
use flotilla_engine::views::{self, Live};
use flotilla_engine::{Changes, Cmd, Event, Events, Outcome};
use flotilla_proto::{
    ClientMsg, EngineState, GhStatus, Reply, ReqId, Request, ServerMsg, Setting, SyncStatus, View,
    ViewKey,
};
use flotilla_server::{Config, Doorway, Listen, Server};
use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

fn s(v: &str) -> String {
    v.to_string()
}

/// How the client reaches the server.
#[derive(Clone, Copy, Debug)]
enum Transport {
    Tcp,
    Memory,
}

/// Where the proxy forwards to.
#[derive(Clone)]
enum Upstream {
    Tcp(SocketAddr),
    Memory(Doorway),
}

/// Where the proxy forwards to, whether it's up, and the connections it carries.
type Links = Arc<Mutex<(Upstream, bool, Vec<tokio::task::JoinHandle<()>>)>>;

/// A proxy between the client and the server (a TCP one, or a pipe to a pipe), which the test
/// can cut and restore.
struct Proxy {
    target: Target,
    state: Links,
}

impl Proxy {
    fn start(rt: &tokio::runtime::Runtime, upstream: Upstream) -> Proxy {
        let tcp = matches!(upstream, Upstream::Tcp(_));
        let state: Links = Arc::new(Mutex::new((upstream, true, Vec::new())));
        let st = state.clone();
        if !tcp {
            let handle = rt.handle().clone();
            let target = Target::Memory(Pipe::new(move || {
                let mut g = st.lock().unwrap();
                let Upstream::Memory(door) = &g.0 else {
                    return None;
                };
                if !g.1 {
                    return None;
                }
                let mut out = door.open()?;
                let (client, mut inbound) = tokio::io::duplex(64 << 10);
                g.2.push(handle.spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut out).await;
                }));
                Some(client)
            }));
            return Proxy { target, state };
        }
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        rt.spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    return;
                };
                let mut g = st.lock().unwrap();
                let Upstream::Tcp(upstream) = g.0.clone() else {
                    continue;
                };
                if !g.1 {
                    continue;
                }
                g.2.push(tokio::spawn(async move {
                    if let Ok(mut out) = tokio::net::TcpStream::connect(upstream).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut out).await;
                    }
                }));
            }
        });
        Proxy {
            target: Target::parse(&format!("ws://{addr}")).unwrap(),
            state,
        }
    }

    /// Cut every connection, and refuse new ones.
    fn down(&self) {
        let mut g = self.state.lock().unwrap();
        g.1 = false;
        for h in g.2.drain(..) {
            h.abort();
        }
    }

    fn up(&self) {
        self.state.lock().unwrap().1 = true;
    }

    fn point_at(&self, upstream: Upstream) {
        self.state.lock().unwrap().0 = upstream;
    }
}

/// A server on `db` with a stand-in engine: every action is done at once.
struct Running {
    upstream: Upstream,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<Result<(), String>>,
    events: Arc<OnceLock<Events>>,
}

fn serve(rt: &tokio::runtime::Runtime, db: &Path, transport: Transport) -> Running {
    let events: Arc<OnceLock<Events>> = Arc::default();
    let to_host = events.clone();
    let mode = Mode::Fake(Arc::new(move |cmd| {
        if let (Cmd::Act { action, req }, Some(events)) = (cmd, to_host.get()) {
            let _ = events.send_blocking(Event::Done {
                action,
                result: Ok(Outcome::Done),
                req,
            });
        }
    }));
    let (listen, role) = match transport {
        Transport::Tcp => (Listen::tcp("127.0.0.1", 0, false).unwrap(), Role::Server),
        Transport::Memory => (Listen::Memory, Role::App),
    };
    let config = Config {
        db: db.to_path_buf(),
        listen: vec![listen],
        mode,
        role,
    };
    let server = rt.block_on(Server::bind(config)).unwrap();
    let _ = events.set(server.host().events());
    let upstream = match transport {
        Transport::Tcp => Upstream::Tcp(server.tcp().unwrap()),
        Transport::Memory => Upstream::Memory(server.doorway().unwrap()),
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let done = rt.spawn(server.run(async {
        let _ = stopped.await;
    }));
    Running {
        upstream,
        stop: Some(stop),
        done,
        events,
    }
}

impl Running {
    fn engine(&self, e: Event) {
        self.events.get().unwrap().send_blocking(e).unwrap();
    }

    fn stop(mut self, rt: &tokio::runtime::Runtime) {
        let _ = self.stop.take().unwrap().send(());
        rt.block_on(&mut self.done).unwrap().unwrap();
    }
}

/// What the client handed on, as the app would keep it.
struct Seen {
    rx: mpsc::Receiver<ServerMsg>,
    views: HashMap<ViewKey, View>,
    answers: HashMap<ReqId, Result<Reply, String>>,
    /// How many answers came for each request.
    answered: HashMap<ReqId, usize>,
}

impl Seen {
    fn pump(&mut self, done: impl Fn(&Seen) -> bool, what: &str) {
        let until = Instant::now() + Duration::from_secs(20);
        while !done(self) {
            let left = until.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(ServerMsg::Snap { view }) => {
                    self.views.insert(view.key(), *view);
                }
                Ok(ServerMsg::Res { id, result }) => {
                    *self.answered.entry(id).or_default() += 1;
                    self.answers.insert(id, result);
                }
                Ok(_) => {}
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }
}

/// The test, playing the engine and the user.
struct Run {
    rt: tokio::runtime::Runtime,
    dir: PathBuf,
    store: Store,
    server: Option<Running>,
    proxy: Proxy,
    client: Client,
    seen: Seen,
    next_req: ReqId,
    /// What's subscribed to, and the status the engine last reported.
    subs: BTreeSet<ViewKey>,
    status: SyncStatus,
    threads: Vec<String>,
    /// Each thread's place in the sample turn it's replaying.
    replay: HashMap<String, usize>,
    sample: Vec<RawEvent>,
    rng: u64,
    transport: Transport,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Run {
    fn new(transport: Transport) -> Run {
        let dir = std::env::temp_dir().join(format!(
            "flotilla-replay-{transport:?}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        let store = Store::open(&db).unwrap();
        let catalog = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../m0-samples/provider-models.json"
        ))
        .unwrap();
        store.kv_set("catalog", &catalog).unwrap();
        store
            .replace_environments(&[flotilla_core::boat::Environment {
                id: s("e1"),
                name: s("base"),
                is_default: true,
            }])
            .unwrap();
        // A paired device: every TCP connection needs one's token.
        let now = flotilla_core::util::now_ms();
        let code = store
            .mint_code(flotilla_core::devices::CODE_TTL, "cli", now)
            .unwrap();
        let token = store
            .pair_device(&code.code, "replay", "test", None, now)
            .unwrap()
            .token;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = serve(&rt, &db, transport);
        let proxy = Proxy::start(&rt, server.upstream.clone());
        let (tx, rx) = mpsc::channel();
        let client = Client::connect(
            proxy.target.clone(),
            Info {
                app: s("replay"),
                version: s("0"),
                token: Some(flotilla_proto::Secret(token)),
                scope: None,
            },
            move |m| tx.send(m).is_ok(),
        );
        let mut sample = Vec::new();
        for name in [
            "claude-turn-readback.jsonl",
            "codex-turn-readback.jsonl",
            "steer-turn-readback.jsonl",
        ] {
            let text = std::fs::read_to_string(format!(
                "{}/../../m0-samples/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            sample.extend(text.lines().filter(|l| !l.trim().is_empty()).map(|l| {
                RawEvent::from_value(
                    serde_json::from_str::<serde_json::Value>(l).unwrap()["data"].clone(),
                )
            }));
        }
        Run {
            rt,
            dir,
            store,
            server: Some(server),
            proxy,
            client,
            seen: Seen {
                rx,
                views: HashMap::new(),
                answers: HashMap::new(),
                answered: HashMap::new(),
            },
            next_req: 1,
            subs: BTreeSet::new(),
            status: SyncStatus::default(),
            threads: vec![],
            replay: HashMap::new(),
            sample,
            rng: 0x2545_f491_4f6c_dd1d,
            transport,
        }
    }

    fn rand(&mut self, n: usize) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % n.max(1) as u64) as usize
    }

    fn pick(&mut self) -> Option<String> {
        let n = self.threads.len();
        if n == 0 {
            return None;
        }
        let i = self.rand(n);
        Some(self.threads[i].clone())
    }

    fn changed(&self, c: Changes) {
        self.server.as_ref().unwrap().engine(Event::Changed(c));
    }

    fn sub(&mut self, view: ViewKey) {
        if self.subs.insert(view.clone()) {
            self.client.send(ClientMsg::Sub { view });
        }
    }

    fn unsub(&mut self, view: ViewKey) {
        if self.subs.remove(&view) {
            self.seen.views.remove(&view);
            self.client.send(ClientMsg::Unsub { view });
        }
    }

    /// Ask, without waiting for the answer.
    fn ask(&mut self, req: Request) -> ReqId {
        let id = self.next_req;
        self.next_req += 1;
        self.client.send(ClientMsg::Req { id, req });
        id
    }

    /// Everything sent so far has been answered, so the views it changed (and every change
    /// said before it) are here.
    fn barrier(&mut self) {
        let id = self.ask(Request::CachedRepos);
        self.seen
            .pump(|s| s.answers.contains_key(&id), "the barrier's answer");
        // A subscribe's first view comes before any answer after it.
        let subs = self.subs.clone();
        self.seen.pump(
            |s| subs.iter().all(|k| s.views.contains_key(k)),
            "every view subscribed to",
        );
    }

    /// The client's views against the ones read straight from the database.
    fn check(&mut self, step: &str) {
        self.barrier();
        let live = Live {
            status: self.status.clone(),
            github: GhStatus::Unknown,
            usage: None,
            engine: EngineState::Running,
        };
        let sidebar = views::sidebar(&self.store, &live).unwrap();
        for key in self.subs.clone() {
            let want = match &key {
                ViewKey::Sidebar => View::Sidebar(sidebar.clone()),
                ViewKey::Settings => {
                    View::Settings(views::settings(&self.store, None, None).unwrap())
                }
                ViewKey::Thread(t) => View::Thread(Box::new(
                    views::thread(&self.store, t, &sidebar.threads, &sidebar.parents).unwrap(),
                )),
                ViewKey::Project(p) => View::Project(views::project(&self.store, p).unwrap()),
                ViewKey::Catalog => View::Catalog(views::catalog(&self.store)),
                ViewKey::Mentions(t) => View::Mentions {
                    thread: t.clone(),
                    mentions: views::mentions(&self.store, t),
                },
                ViewKey::Schedules => View::Schedules(views::schedules(&self.store).unwrap()),
                // Who's connected is the host's to know, not the database's; what agents spent is
                // priced as it's sent.
                ViewKey::Devices | ViewKey::AgentUsage => continue,
            };
            let got = self.seen.views.get(&key);
            if got != Some(&want) {
                let ops =
                    got.and_then(|g| flotilla_proto::delta::diff(&key, &g.data(), &want.data()));
                panic!(
                    "after {step}: the client's {key:?} isn't the database's; to make it so: {ops:?}"
                );
            }
        }
    }

    /// One random step, as the engine or a user would make it.
    fn step(&mut self, n: usize) -> String {
        let roll = self.rand(16);
        match (roll, self.pick()) {
            (0, _) | (_, None) => {
                let id = format!("t{n:03}");
                let mut t = thread(&id);
                if self.rand(3) == 0 {
                    t.repo_project_id = Some(s("p1"));
                    t.branch = Some(format!("b-{id}"));
                }
                self.store.insert_thread(&t).unwrap();
                self.threads.push(id.clone());
                self.changed(Changes::thread(&id).with_sidebar());
                format!("new {id}")
            }
            (1..=3, Some(t)) => {
                // The next events of a sample turn, or the turn's unsettled ones dropped.
                let at = self.replay.entry(t.clone()).or_default();
                let start = *at;
                let take = 1 + (self.rng % 3) as usize;
                *at = (start + take).min(self.sample.len());
                let events: Vec<RawEvent> = self.sample[..*at].to_vec();
                let complete = start > 0 && self.rng.is_multiple_of(4);
                let events = if complete {
                    events[..events.len().saturating_sub(1)].to_vec()
                } else {
                    events
                };
                self.store.apply_events(&t, &events, complete).unwrap();
                self.store.rebuild_messages(&t).unwrap();
                self.changed(Changes::thread(&t).with_sidebar());
                format!("events {start}+{take} on {t}")
            }
            (4, Some(t)) => {
                let title = format!("Title {n}");
                self.store.set_thread_title(&t, &title).unwrap();
                self.changed(Changes::thread(&t).with_sidebar());
                format!("rename {t}")
            }
            (5, Some(t)) => {
                let states = [
                    ThreadState::Idle,
                    ThreadState::Running,
                    ThreadState::Stopped,
                    ThreadState::Starting,
                ];
                let st = states[self.rand(states.len())];
                self.store.set_thread_state(&t, st).unwrap();
                self.changed(Changes::thread(&t).with_sidebar());
                format!("{t} {st:?}")
            }
            (6, Some(t)) => {
                let req = if self.rand(2) == 0 {
                    Request::Archive(t.clone())
                } else {
                    Request::Unarchive(t.clone())
                };
                self.ask(req);
                format!("archive/unarchive {t}")
            }
            (7, Some(t)) => {
                let req = if self.rand(2) == 0 {
                    Request::MarkUnread(t.clone())
                } else {
                    Request::MarkViewed(t.clone())
                };
                self.ask(req);
                format!("unread/read {t}")
            }
            (8, Some(t)) => {
                let local = format!("local-{n}");
                self.ask(Request::Send {
                    thread: t.clone(),
                    text: format!("Message {n}"),
                    files: vec![],
                    local,
                });
                format!("send to {t}")
            }
            (9, Some(child)) => {
                let parent = self.pick().unwrap();
                if parent == child
                    || flotilla_core::orchestrate::parents(&self.store).contains_key(&parent)
                {
                    return s("no parent");
                }
                flotilla_core::orchestrate::set_parent(&self.store, &child, &parent).unwrap();
                let mut c = Changes::thread(&child).with_sidebar();
                c.add_thread(&parent);
                self.changed(c);
                format!("{parent} hands {child} work")
            }
            (10 | 11, Some(t)) => {
                // Open or close a thread.
                let key = ViewKey::Thread(t.clone());
                if self.subs.contains(&key) {
                    self.unsub(key);
                    format!("close {t}")
                } else {
                    self.sub(key);
                    format!("open {t}")
                }
            }
            (12, _) => {
                let run = SetupRun {
                    id: format!("r{n}"),
                    project_id: s("p1"),
                    kind: s("update"),
                    trigger: s("main"),
                    sha: format!("sha{n}"),
                    sandbox_id: None,
                    state: if self.rand(2) == 0 {
                        s("failed")
                    } else {
                        s("ready")
                    },
                    step: None,
                    error: None,
                    log: s("…"),
                    started_at: format!("2026-10-31T10:{:02}:00.000Z", n % 60),
                    finished_at: None,
                };
                self.store.insert_setup_run(&run).unwrap();
                self.changed(Changes::project("p1"));
                s("setup run")
            }
            (13, _) => {
                let req = if self.rand(2) == 0 {
                    Request::Set(Setting::BranchPrefix(format!("b{n}")))
                } else {
                    Request::SetInstructions(format!("Rule {n}"))
                };
                self.ask(req);
                s("a setting")
            }
            (14, _) => {
                self.status = SyncStatus {
                    error: (self.rand(3) == 0).then(|| s("offline")),
                    syncing: self.rand(2) == 0,
                };
                self.server
                    .as_ref()
                    .unwrap()
                    .engine(Event::Status(self.status.clone()));
                s("sync status")
            }
            (_, Some(t)) => {
                // Streaming text: never stored, never compared, but it mustn't upset anything.
                let partials = self.sample.iter().take(1).cloned().collect();
                self.server.as_ref().unwrap().engine(Event::Live {
                    thread: t.clone(),
                    partials,
                });
                format!("live text on {t}")
            }
        }
    }
}

fn thread(id: &str) -> Thread {
    Thread {
        id: s(id),
        environment_id: Some(s("e1")),
        environment_name: Some(s("base")),
        title: format!("Thread {id}"),
        sandbox_id: Some(format!("bx_{id}")),
        conversation_id: Some(format!("c_{id}")),
        provider: s("claude-code"),
        model: s("haiku"),
        effort: None,
        fast: false,
        state: ThreadState::Idle,
        keep_running: false,
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: s("2026-10-31T10:00:00.000Z"),
        updated_at: s("2026-10-31T10:00:00.000Z"),
        repo_project_id: None,
        branch: None,
        base_sha: None,
    }
}

#[test]
fn the_clients_views_match_the_database_through_drops_and_restarts() {
    replay(Transport::Tcp);
}

#[test]
fn the_apps_own_views_match_the_database_through_its_pipe() {
    replay(Transport::Memory);
}

fn replay(transport: Transport) {
    let mut r = Run::new(transport);
    let p1 = Project {
        id: s("p1"),
        owner: s("acme"),
        repo: s("web"),
        default_branch: s("main"),
        environment_name: s("base"),
        created_at: s("2026-10-31T10:00:00.000Z"),
        template_sandbox_id: None,
        template_sha: None,
        template_built_at: None,
        prev_template_sandbox_id: None,
        head_sha: None,
        head_etag: None,
        checked_at: None,
    };
    r.store.insert_project(&p1, &[]).unwrap();
    for view in [
        ViewKey::Sidebar,
        ViewKey::Settings,
        ViewKey::Catalog,
        ViewKey::Project(s("p1")),
    ] {
        r.sub(view);
    }
    r.check("subscribing");
    let mut n = 0;
    let mut steps = |r: &mut Run, count: usize, check: bool| {
        for _ in 0..count {
            n += 1;
            let what = r.step(n);
            if check {
                r.check(&what);
            }
        }
    };
    steps(&mut r, 120, true);

    // A short drop: the client resumes from the deltas the server kept.
    let before = r.client.stats();
    r.proxy.down();
    steps(&mut r, 6, false);
    r.proxy.up();
    r.check("a short drop");
    let after = r.client.stats();
    assert_eq!(
        (after.connects, after.resumed),
        (before.connects + 1, before.resumed + 1),
        "{after:?}"
    );
    steps(&mut r, 20, true);

    // A long one, with more changes to an open thread than the server keeps: it comes whole.
    let open = r.threads[0].clone();
    r.sub(ViewKey::Thread(open.clone()));
    r.check("opening the first thread");
    r.proxy.down();
    for i in 0..80 {
        r.store
            .set_thread_title(&open, &format!("Renamed {i}"))
            .unwrap();
        r.changed(Changes::thread(&open).with_sidebar());
    }
    steps(&mut r, 10, false);
    // Let the server take all that before the client is back.
    std::thread::sleep(Duration::from_millis(300));
    r.proxy.up();
    r.check("a long drop");
    steps(&mut r, 20, true);

    // The server restarts (a new run): everything comes whole, and what happened meanwhile
    // (written by the engine of the next run) is there.
    let rt = &r.rt;
    r.server.take().unwrap().stop(rt);
    r.proxy.down();
    let id = s("t-while-down");
    r.store.insert_thread(&thread(&id)).unwrap();
    r.threads.push(id);
    let db = r.dir.join("flotilla.db");
    let next = serve(&r.rt, &db, r.transport);
    r.proxy.point_at(next.upstream.clone());
    r.server = Some(next);
    r.status = SyncStatus::default();
    r.proxy.up();
    r.check("a restart");
    steps(&mut r, 60, true);
    // Every request was answered exactly once, through the drops and the restart.
    let wrong: Vec<(ReqId, usize)> = (1..r.next_req)
        .map(|id| (id, r.seen.answered.get(&id).copied().unwrap_or(0)))
        .filter(|(_, n)| *n != 1)
        .collect();
    assert!(
        wrong.is_empty(),
        "requests not answered exactly once (id, answers): {wrong:?}"
    );
    // Every delta fitted: none was mended by asking for a view whole.
    let stats = r.client.stats();
    eprintln!("[replay {transport:?}] {stats:?}");
    assert_eq!(stats.resyncs, 0, "{stats:?}");
    assert!(
        stats.deltas > 100 && stats.snaps > 10 && stats.connects == 4 && stats.resumed == 2,
        "{stats:?}"
    );
}
