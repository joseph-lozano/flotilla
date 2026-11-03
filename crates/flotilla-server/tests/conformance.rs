//! A real session's wire, recorded for the Swift client (`ios/FlotillaKit`), whose replica
//! must end with exactly the views Rust's does.
//!
//! A real server (a stand-in engine) and a raw WebSocket, as the iPhone connects: a device's
//! token, the protocol header, a hello. The test plays the engine (writes the database, says
//! what changed) and the client (subscribes, asks, and takes what comes through
//! `flotilla_client::session::SessionCore`, as the client and the iPhone do), and records
//! every frame each way:
//!
//! - snapshots, then deltas: messages appended and changed, a field set whole, rows put,
//!   moved and gone;
//! - a delta deliberately lost on the way, so the next one doesn't fit and the view is asked
//!   for whole (`resync`);
//! - a drop and a reconnect that resumes (deltas from the ring, and answers after `got`, one
//!   of them already had: the hello says one less than it got);
//! - a drop with requests in flight, then a resumed welcome: the one the server took (its
//!   answer lost on the way) isn't sent again and its answer comes again, the one it never
//!   got (lost on the way out) goes again (`seen`);
//! - a drop with requests in flight while more was said than the server keeps, so the welcome
//!   in the same server run isn't resumed: revisions still count, a `thread.send` goes again
//!   and the rest are told lost;
//! - snapshots the server didn't send (put in by the test): one with values this version
//!   doesn't know, which reads as the iPhone reads it, and one that doesn't read at all, which
//!   isn't asked for again;
//! - a server restart (a new epoch), so everything comes whole.
//!
//! After each step the replica's views must equal the views read straight from SQLite. With
//! `FLOTILLA_WRITE_CONFORMANCE=<path>` it writes the recording and the final views there
//! (`mobile/fixtures/conformance/session.json`).
//!
//! A second recording, `history.json` (`FLOTILLA_WRITE_HISTORY=<path>`), is a long thread: its
//! view's newest pages, older pages over HTTP as `flotilla_proto::History` wants them, the view
//! moving on as the thread grows, a message changing outside it, a lost delta, tool output cut
//! (multibyte across the cut, a list too long, text that isn't JSON) and one fetched whole, and
//! a search hit on the first page opened at (`History::reach`), its pages fetched one at a time.

use flotilla_client::session::{Outcome, SessionCore};
use flotilla_core::event::RawEvent;
use flotilla_core::store::{Store, Thread, ThreadState};
use flotilla_engine::host::{Mode, Role};
use flotilla_engine::views::{self, Live};
use flotilla_engine::{Changes, Cmd, Event, Events};
use flotilla_proto::wire::{Down, PROTOCOL_HEADER, Up, WS_PATH};
use flotilla_proto::{
    ClientMsg, Connection, EngineState, GhStatus, PROTOCOL, ReqId, Request, ServerMsg, SyncStatus,
    View, ViewKey,
};
use flotilla_server::{Config, Listen, Server};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

fn s(v: &str) -> String {
    v.to_string()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A server on `db` with a stand-in engine: every action is done at once.
struct Running {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    done: tokio::task::JoinHandle<Result<(), String>>,
    events: Arc<OnceLock<Events>>,
}

async fn serve(db: &Path) -> Running {
    let events: Arc<OnceLock<Events>> = Arc::default();
    let to_host = events.clone();
    let mode = Mode::Fake(Arc::new(move |cmd| {
        if let (Cmd::Act { action, req }, Some(events)) = (cmd, to_host.get()) {
            let _ = events.send_blocking(Event::Done {
                action,
                result: Ok(flotilla_engine::Outcome::Done),
                req,
            });
        }
    }));
    let config = Config {
        db: db.to_path_buf(),
        listen: vec![Listen::tcp("127.0.0.1", 0, false).unwrap()],
        mode,
        role: Role::Server,
    };
    let server = Server::bind(config).await.unwrap();
    let _ = events.set(server.host().events());
    let addr = server.tcp().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let done = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    Running {
        addr,
        stop: Some(stop),
        done,
        events,
    }
}

impl Running {
    fn engine(&self, e: Event) {
        self.events.get().unwrap().send_blocking(e).unwrap();
    }

    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        (&mut self.done).await.unwrap().unwrap();
    }
}

/// The phone's side: a socket, its session, and what it recorded.
struct Phone {
    token: String,
    ws: Option<Ws>,
    core: SessionCore,
    next_req: u64,
    /// Lose the next delta of this view on the way in.
    lose: Option<ViewKey>,
    /// Lose the answer to this request on the way in.
    lose_answer: Option<u64>,
    steps: Vec<Value>,
    /// A thread's older pages beside its view, fed each view of it as it's shown.
    history: Option<(ViewKey, flotilla_proto::History)>,
}

impl Phone {
    fn new(token: String, session: &str) -> Phone {
        Phone {
            token,
            ws: None,
            core: SessionCore::new(s(session)),
            next_req: 1,
            lose: None,
            lose_answer: None,
            steps: vec![],
            history: None,
        }
    }

    /// Connect, say hello (`got`: what it says it got), and take the welcome (what was in
    /// flight goes again or is lost) and everything up to `synced`. Whether it resumed.
    async fn connect(&mut self, addr: SocketAddr, got: Option<u64>) -> bool {
        let mut req = format!("ws://{addr}{WS_PATH}")
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(PROTOCOL_HEADER, PROTOCOL.into());
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        self.ws = Some(ws);
        self.steps.push(json!({"connect": true}));
        let mut hello = self.core.hello("flotilla-ios", "0", "ios");
        if let Up::Hello(h) = &mut hello {
            h.got = got;
        }
        self.write(hello).await;
        let mut resumed = None;
        self.pump(|down| {
            if let Down::Welcome(w) = down {
                resumed = Some(w.resumed);
            }
            resumed.is_some()
        })
        .await;
        self.pump(|down| matches!(down, Down::Synced)).await;
        resumed.unwrap()
    }

    async fn drop_connection(&mut self) {
        if let Some(mut ws) = self.ws.take() {
            let _ = ws.close(None).await;
        }
        self.core.lost(vec![]);
        self.steps.push(json!({"disconnect": true}));
    }

    async fn write(&mut self, up: Up) {
        let text = serde_json::to_string(&up).unwrap();
        self.steps
            .push(json!({"up": serde_json::from_str::<Value>(&text).unwrap()}));
        self.ws
            .as_mut()
            .unwrap()
            .send(Message::Text(text.into()))
            .await
            .unwrap();
    }

    async fn sub(&mut self, view: ViewKey) {
        for up in self.core.app(ClientMsg::Sub { view }) {
            self.write(up).await;
        }
    }

    async fn ask(&mut self, req: Request) -> u64 {
        let id = self.next_req;
        self.next_req += 1;
        for up in self.core.app(ClientMsg::Req { id, req }) {
            self.write(up).await;
        }
        id
    }

    /// A request the client wrote that never reached the server (the socket was going).
    fn ask_dropped(&mut self, req: Request) -> u64 {
        let id = self.next_req;
        self.next_req += 1;
        for up in self.core.app(ClientMsg::Req { id, req }) {
            self.steps
                .push(json!({"up": serde_json::to_value(up).unwrap(), "dropped": true}));
        }
        id
    }

    /// A frame the server didn't send, put in to see what the client makes of it.
    fn inject(&mut self, down: Down) -> Outcome {
        let raw = serde_json::to_value(&down).unwrap();
        let outcome = self.core.received(down);
        let what = match &outcome {
            Outcome::Deliver(m) if matches!(m.as_slice(), [ServerMsg::Snap { .. }]) => {
                taken(&outcome)
            }
            Outcome::Unreadable(..) => taken(&outcome),
            other => panic!("not a snapshot's: {other:?}"),
        };
        self.steps
            .push(json!({"down": raw, "taken": what, "injected": true}));
        outcome
    }

    /// Take frames until `done` says so, through the session as the client does, and write
    /// what it says to.
    async fn pump(&mut self, mut done: impl FnMut(&Down) -> bool) {
        loop {
            let frame =
                tokio::time::timeout(Duration::from_secs(20), self.ws.as_mut().unwrap().next())
                    .await
                    .expect("the server went quiet");
            let text = match frame {
                Some(Ok(Message::Text(t))) => t.to_string(),
                Some(Ok(_)) => continue,
                other => panic!("the socket ended: {other:?}"),
            };
            let raw: Value = serde_json::from_str(&text).unwrap();
            let down: Down = serde_json::from_str(&text).unwrap();
            let stop = done(&down);
            if let Down::Delta { view, .. } = &down
                && self.lose.as_ref() == Some(view)
            {
                self.lose = None;
                self.steps.push(json!({"lost": raw}));
                continue;
            }
            if let Down::Res { id, .. } = &down
                && self.lose_answer == Some(*id)
            {
                self.lose_answer = None;
                self.steps.push(json!({"lost": raw}));
                if stop {
                    return;
                }
                continue;
            }
            let outcome = self.core.received(down);
            self.steps
                .push(json!({"down": raw, "taken": taken(&outcome)}));
            match outcome {
                // What was in flight goes again or is lost; then what waited.
                Outcome::Welcomed { send, lost, .. } => {
                    let again: Vec<ReqId> = send
                        .iter()
                        .filter_map(|u| {
                            if let Up::Req { id, .. } = u {
                                Some(*id)
                            } else {
                                None
                            }
                        })
                        .collect();
                    self.steps
                        .push(json!({"welcomed": {"again": again, "lost": lost}}));
                    for up in send {
                        self.write(up).await;
                    }
                }
                Outcome::Deliver(msgs) => {
                    for m in msgs {
                        if let (ServerMsg::Snap { view }, Some((key, history))) =
                            (m, self.history.as_mut())
                            && view.key() == *key
                            && let View::Thread(d) = &*view
                        {
                            history.view(d);
                        }
                    }
                }
                Outcome::Resync(view, _) => self.write(Up::Resync { view }).await,
                Outcome::Refuse(why, _) | Outcome::Fail(why) => panic!("the session ended: {why}"),
                Outcome::Unreadable(..) | Outcome::Skipped | Outcome::Nothing => {}
            }
            if stop {
                return;
            }
        }
    }

    /// Everything said so far has come: a request's answer comes after the views it changed,
    /// and after every change said before it.
    async fn barrier(&mut self) {
        let id = self.ask(Request::CachedRepos).await;
        self.pump(|d| matches!(d, Down::Res { id: got, .. } if *got == id))
            .await;
    }
}

/// What a frame came to, as the recording says it.
fn taken(outcome: &Outcome) -> Value {
    match outcome {
        Outcome::Welcomed { .. } => json!("welcome"),
        Outcome::Deliver(m) => match m.as_slice() {
            [ServerMsg::Snap { view }] => json!({"show": key_json(&view.key())}),
            [ServerMsg::Res { id, .. }] => json!({"res": id}),
            [ServerMsg::Live { .. }] => json!("live"),
            [ServerMsg::Note(_)] => json!("note"),
            [ServerMsg::Progress { .. }] => json!("progress"),
            [ServerMsg::Connection(Connection::Online)] => json!("synced"),
            other => panic!("delivered {other:?}"),
        },
        Outcome::Resync(view, _) => json!({"resync": key_json(view)}),
        Outcome::Unreadable(view, _) => json!({"unreadable": key_json(view)}),
        Outcome::Skipped => json!("skipped"),
        Outcome::Refuse(..) => json!("error"),
        Outcome::Fail(why) => panic!("failed: {why}"),
        Outcome::Nothing => json!("nothing"),
    }
}

fn key_json(key: &ViewKey) -> Value {
    serde_json::to_value(key).unwrap()
}

fn thread(id: &str, title: &str) -> Thread {
    Thread {
        id: s(id),
        environment_id: Some(s("e1")),
        environment_name: Some(s("base")),
        title: s(title),
        sandbox_id: Some(format!("bx_{id}")),
        conversation_id: Some(format!("c_{id}")),
        provider: s("claude-code"),
        model: s("claude-haiku-5-5"),
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

/// The test playing the engine.
struct World {
    dir: PathBuf,
    store: Store,
    sample: Vec<RawEvent>,
    /// How far each thread is through the sample turn.
    at: std::collections::HashMap<String, usize>,
    /// The account's usage as last read from boat.
    usage: Option<flotilla_proto::Usage>,
}

impl World {
    fn events(&mut self, server: &Running, t: &str, take: usize) {
        let at = self.at.entry(s(t)).or_default();
        *at = (*at + take).min(self.sample.len());
        let events = self.sample[..*at].to_vec();
        self.store.apply_events(t, &events, false).unwrap();
        self.store.rebuild_messages(t).unwrap();
        server.engine(Event::Changed(Changes::thread(t).with_sidebar()));
    }

    fn rename(&self, server: &Running, t: &str, title: &str) {
        self.store.set_thread_title(t, title).unwrap();
        server.engine(Event::Changed(Changes::thread(t).with_sidebar()));
    }

    fn add(&self, server: &Running, t: &str, title: &str) {
        self.store.insert_thread(&thread(t, title)).unwrap();
        server.engine(Event::Changed(Changes::thread(t).with_sidebar()));
    }

    /// The views as read straight from the database.
    fn view(&self, key: &ViewKey) -> Value {
        let live = Live {
            status: SyncStatus::default(),
            github: GhStatus::Unknown,
            usage: self.usage.clone(),
            engine: EngineState::Running,
        };
        let sidebar = views::sidebar(&self.store, &live).unwrap();
        let view = match key {
            ViewKey::Sidebar => View::Sidebar(sidebar),
            ViewKey::Catalog => View::Catalog(views::catalog(&self.store)),
            ViewKey::Thread(t) => View::Thread(Box::new(
                views::thread(&self.store, t, &sidebar.threads, &sidebar.parents).unwrap(),
            )),
            ViewKey::Schedules => View::Schedules(views::schedules(&self.store).unwrap()),
            other => panic!("not recorded: {other:?}"),
        };
        view.data()
    }

    fn check(&self, phone: &Phone, step: &str) {
        for key in phone.core.replica().subscribed() {
            let held = phone
                .core
                .replica()
                .data(key)
                .unwrap_or_else(|| panic!("after {step}: no {key:?}"));
            let want = self.view(key);
            assert!(
                *held == want,
                "after {step}: the replica's {key:?} isn't the database's: {:?}",
                flotilla_proto::delta::diff(key, held, &want)
            );
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_the_swift_replica_replays() {
    let dir = std::env::temp_dir().join(format!("flotilla-conformance-{}", std::process::id()));
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
    let now = flotilla_core::util::now_ms();
    let code = store
        .mint_code(flotilla_core::devices::CODE_TTL, "cli", now)
        .unwrap();
    let token = store
        .pair_device(&code.code, "iPhone", "ios", None, now)
        .unwrap()
        .token;
    let mut sample = Vec::new();
    for name in ["claude-turn-readback.jsonl", "steer-turn-readback.jsonl"] {
        let text = std::fs::read_to_string(format!(
            "{}/../../m0-samples/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        sample.extend(text.lines().filter(|l| !l.trim().is_empty()).map(|l| {
            RawEvent::from_value(serde_json::from_str::<Value>(l).unwrap()["data"].clone())
        }));
    }
    let mut world = World {
        dir,
        store,
        sample,
        at: Default::default(),
        usage: None,
    };
    for (id, title) in [
        ("t1", "Fix the flaky retry test"),
        ("t2", "Review median.py"),
    ] {
        world.store.insert_thread(&thread(id, title)).unwrap();
    }

    let server = serve(&db).await;
    let mut phone = Phone::new(token, "5e55a9b0-0000-4000-8000-000000000001");
    assert!(!phone.connect(server.addr, None).await, "a new session");
    let t1 = ViewKey::Thread(s("t1"));
    for view in [ViewKey::Sidebar, ViewKey::Catalog, t1.clone()] {
        phone.sub(view).await;
    }
    phone.barrier().await;
    world.check(&phone, "subscribing");

    // Deltas: messages appended and changed, rows moved, a new row, a rename.
    world.events(&server, "t1", 3);
    phone.barrier().await;
    world.events(&server, "t1", 4);
    world.rename(&server, "t2", "Review median.py again");
    phone.barrier().await;
    world.add(&server, "t3", "Slugify edge cases");
    phone.ask(Request::MarkUnread(s("t1"))).await;
    phone.barrier().await;
    server.engine(Event::Live {
        thread: s("t1"),
        partials: world.sample.iter().take(1).cloned().collect(),
    });
    phone.barrier().await;
    // A field set whole (the open thread renamed), a row moved to the top (an order), a row gone.
    world.rename(&server, "t1", "Fix the flaky retry test, again");
    world.events(&server, "t3", 3);
    phone.barrier().await;
    world.store.delete_thread("t2").unwrap();
    server.engine(Event::Changed(Changes::thread("t2").with_sidebar()));
    phone.barrier().await;
    // The account's usage, read from boat, near its limits (kept, so it's there after the
    // restart too).
    world.usage = Some(flotilla_proto::Usage {
        vms: Some((3, 100)),
        minute: Some((12, 12)),
        hour: Some((22, 60)),
        day: Some((187, 200)),
        vm_hours: Some((48, 556)),
        renews: Some(s("2026-11-30T01:01:38.000Z")),
        blocked: None,
        starts_free: Some(s("2026-10-31T17:04:00.000Z")),
        extra_starts: Some(flotilla_proto::ExtraStarts {
            on: true,
            used: 7,
            spend_cents: Some((29, 8000)),
            cap_reached: false,
        }),
        read_ms: Some(1_793_466_000_000),
    });
    server.engine(Event::Usage(Ok(world.usage.clone().unwrap())));
    phone.barrier().await;
    world.check(&phone, "deltas");

    // Scheduled prompts: put by the phone (each kept with its next run), a run noted as the
    // engine notes it (an entry changed in place), one paused (moved), one deleted (gone).
    phone.sub(ViewKey::Schedules).await;
    phone.barrier().await;
    let fresh = flotilla_proto::Fresh {
        environment: s("base"),
        project_id: None,
        provider: s("claude-code"),
        model: s("claude-sonnet-5-5"),
        effort: Some(s("low")),
        fast: false,
        machine_type: Some(s("small")),
        on_branch: None,
    };
    let specs = [
        flotilla_proto::ScheduleSpec {
            id: s("5c0e0000-0000-4000-8000-000000000001"),
            prompt: s("Check the overnight CI runs"),
            target: flotilla_proto::Target::Thread(s("t1")),
            when: flotilla_proto::When::Daily { at: 540 },
            zone: s("Europe/London"),
        },
        flotilla_proto::ScheduleSpec {
            id: s("5c0e0000-0000-4000-8000-000000000002"),
            prompt: s("Triage new bugs"),
            target: flotilla_proto::Target::New(fresh),
            when: flotilla_proto::When::Cron {
                expr: s("0 8 * * 1-5"),
            },
            zone: s("America/New_York"),
        },
        flotilla_proto::ScheduleSpec {
            id: s("5c0e0000-0000-4000-8000-000000000003"),
            prompt: s("Rebase onto main"),
            target: flotilla_proto::Target::Thread(s("t3")),
            when: flotilla_proto::When::Weekly {
                days: vec![1, 3],
                at: 1080,
            },
            zone: s("Europe/London"),
        },
    ];
    for spec in specs {
        phone.ask(Request::PutSchedule(Box::new(spec))).await;
    }
    phone.barrier().await;
    let run = flotilla_proto::Run {
        at_ms: 1_793_466_000_000,
        outcome: flotilla_proto::RunOutcome::Failed,
        thread: None,
        why: Some(s("boat has no VM starts to spare this hour.")),
    };
    world
        .store
        .record_run("5c0e0000-0000-4000-8000-000000000002", &run, false, None)
        .unwrap();
    world
        .store
        .note_failing("5c0e0000-0000-4000-8000-000000000002", run.why.as_deref())
        .unwrap();
    server.engine(Event::Changed(Changes {
        schedules: true,
        ..Changes::default()
    }));
    phone
        .ask(Request::PauseSchedule {
            schedule: s("5c0e0000-0000-4000-8000-000000000001"),
            paused: true,
        })
        .await;
    phone
        .ask(Request::DeleteSchedule(s(
            "5c0e0000-0000-4000-8000-000000000003",
        )))
        .await;
    phone.barrier().await;
    world.check(&phone, "schedules");

    // A delta lost on the way: the next one doesn't fit, and the thread comes whole.
    phone.lose = Some(t1.clone());
    world.events(&server, "t1", 2);
    phone.barrier().await;
    world.events(&server, "t1", 2);
    phone.barrier().await;
    // The resync's snapshot comes after the barrier's answer.
    phone.barrier().await;
    world.check(&phone, "a lost delta");

    // A drop: changes meanwhile, then a hello that resumes. It says it got one less than it
    // did, so the server sends that one again and the phone skips it.
    phone.ask(Request::MarkViewed(s("t1"))).await;
    phone.barrier().await;
    phone.drop_connection().await;
    world.events(&server, "t1", 3);
    world.rename(&server, "t3", "Slugify: the edge cases");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = phone.core.got().map(|g| g - 1);
    assert!(phone.connect(server.addr, got).await, "resumed");
    phone.barrier().await;
    world.check(&phone, "a resumed drop");

    // A drop with two requests in flight: the server took the first (its answer lost on the
    // way), and never got the second. Resumed: the first isn't sent again, and its answer
    // comes again; the second goes again (it's above `seen`).
    let took = phone.next_req;
    phone.lose_answer = Some(took);
    phone.ask(Request::MarkUnread(s("t3"))).await;
    phone
        .pump(|d| matches!(d, Down::Res { id, .. } if *id == took))
        .await;
    let never = phone.ask_dropped(Request::MarkViewed(s("t3")));
    phone.drop_connection().await;
    assert!(
        phone.connect(server.addr, phone.core.got()).await,
        "resumed with requests in flight"
    );
    assert_eq!(
        phone
            .steps
            .iter()
            .rev()
            .find_map(|s| s.get("welcomed").cloned()),
        Some(json!({"again": [never], "lost": []}))
    );
    phone.barrier().await;
    assert!(
        phone.core.inflight().is_empty(),
        "both answered: {:?}",
        phone.core.inflight()
    );
    world.check(&phone, "a resumed drop with requests in flight");

    // A drop with two requests in flight, and more said meanwhile than the server keeps for a
    // session: the welcome, in the same server run, starts afresh. What the phone holds still
    // counts; the send goes again (it has a client-made id), and the other is told lost.
    let send = phone.ask_dropped(Request::Send {
        thread: s("t1"),
        text: s("and the docs"),
        files: vec![],
        local: s("local-5e55a9b0-0000-4000-8000-0000000000aa"),
    });
    let unread = phone.ask_dropped(Request::MarkUnread(s("t1")));
    phone.drop_connection().await;
    for i in 0..=2000 {
        server.engine(Event::Summaries(Ok(format!(
            "2026-10-31T10:{:02}:00.000Z",
            i % 60
        ))));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let epoch = phone.core.replica().epoch().map(str::to_string);
    assert!(
        !phone.connect(server.addr, phone.core.got()).await,
        "not resumed"
    );
    assert_eq!(
        phone.core.replica().epoch().map(str::to_string),
        epoch,
        "the same server run"
    );
    assert_eq!(
        phone
            .steps
            .iter()
            .rev()
            .find_map(|s| s.get("welcomed").cloned()),
        Some(json!({"again": [send], "lost": [unread]}))
    );
    phone.barrier().await;
    world.check(&phone, "a welcome in the same run, not resumed");

    // Snapshots the server didn't send: values this version doesn't know read as the iPhone
    // reads them; data that doesn't read at all is kept and not asked for again. (The restart
    // next sends everything whole again.)
    let rev = phone.core.replica().rev(&ViewKey::Sidebar).unwrap();
    let mut newer = world.view(&ViewKey::Sidebar);
    newer["engine"] = json!("paused");
    newer["threads"][0]["thread"]["state"] = json!("hibernating");
    newer["threads"][0]["activity"] = json!("dreaming");
    let Outcome::Deliver(shown) = phone.inject(Down::Snap {
        view: ViewKey::Sidebar,
        rev,
        data: newer,
    }) else {
        panic!("didn't read")
    };
    let [ServerMsg::Snap { view }] = &shown[..] else {
        unreachable!()
    };
    let View::Sidebar(sb) = &**view else {
        unreachable!()
    };
    assert_eq!(
        (
            sb.engine.clone(),
            sb.threads[0].thread.state,
            sb.threads[0].activity
        ),
        (
            EngineState::Offline,
            ThreadState::Error,
            flotilla_proto::Activity::Idle
        )
    );
    let broken = json!({"threads": "not a list"});
    assert!(matches!(
        phone.inject(Down::Snap {
            view: ViewKey::Sidebar,
            rev,
            data: broken
        }),
        Outcome::Unreadable(..)
    ));
    assert_eq!(phone.core.replica().rev(&ViewKey::Sidebar), Some(rev));

    // A restart: a new epoch, so everything comes whole, with what changed meanwhile.
    phone.drop_connection().await;
    server.stop().await;
    world
        .store
        .insert_thread(&thread("t4", "Written while the server was down"))
        .unwrap();
    world.store.delete_thread("t3").unwrap();
    let server = serve(&db).await;
    assert!(
        !phone.connect(server.addr, phone.core.got()).await,
        "a new server run doesn't resume"
    );
    phone.barrier().await;
    world.check(&phone, "a restart");
    world.events(&server, "t1", 4);
    world.rename(&server, "t4", "Written while down");
    phone.barrier().await;
    world.check(&phone, "after the restart");

    let views: Vec<Value> = phone
        .core
        .replica()
        .subscribed()
        .map(|k| {
            let mut v = key_json(k);
            v["rev"] = json!(phone.core.replica().rev(k));
            v["data"] = phone.core.replica().data(k).cloned().unwrap();
            v
        })
        .collect();
    let deltas = phone
        .steps
        .iter()
        .filter(|s| s["down"]["t"] == "delta")
        .count();
    let resyncs = phone
        .steps
        .iter()
        .filter(|s| s["up"]["t"] == "resync")
        .count();
    assert!(
        deltas >= 10 && resyncs == 1,
        "{deltas} deltas, {resyncs} resyncs"
    );
    server.stop().await;
    if let Ok(path) = std::env::var("FLOTILLA_WRITE_CONFORMANCE") {
        let out = json!({
            "about": "A real session recorded by crates/flotilla-server/tests/conformance.rs: each step is what the client sent (up), what it got (down) and what Rust's replica made of it (taken), a delta lost on the way (lost), or the socket opening (connect) and closing (disconnect). views: the replica's views at the end.",
            "steps": phone.steps,
            "views": views,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
        eprintln!("wrote {path}: {} steps, {deltas} deltas", phone.steps.len());
    }
}

/// `turns` turns of a prompt and a reply (`edit` changes one reply), with tool output over the
/// cap here and there: a log with multibyte text across the cut (turn 5), text that isn't JSON
/// (turn 40), a 2,000-line read (turn 120) and a big file written (turn 140).
fn long_events(turns: usize, edit: Option<(usize, &str)>) -> Vec<RawEvent> {
    let mut events = Vec::new();
    for k in 0..turns {
        let task = format!("task-{k}");
        let ts = 1_793_000_000_000i64 + k as i64 * 10_000;
        events.push(RawEvent::from_value(json!({"id": format!("p{k}"), "taskId": task, "type": "prompt", "timestamp": ts, "conversationId": "c_long", "data": {"prompt": format!("Turn {k}: carry on"), "status": "finished"}})));
        let text = match edit {
            Some((e, t)) if e == k => t.to_string(),
            _ => format!("Reply {k}: done, é ✓ 日本"),
        };
        let mut data = json!({"content": text, "model": "claude-haiku-5-5"});
        let tool = |name: &str, input: Value, result: String| json!([{"use": {"id": format!("toolu_{k}"), "name": name, "input": input, "type": "tool_use"}, "result": {"content": result, "is_error": false, "tool_use_id": format!("toolu_{k}"), "type": "tool_result"}}]);
        match k {
            5 => {
                // The cut lands inside a multibyte character in some lines.
                let log: String = (0..4000)
                    .map(|i| format!("{}[{i:05}] backed off — ✓ 日本語\n", "é".repeat(i % 3)))
                    .collect();
                data["tools"] = tool(
                    "Bash",
                    json!({"command": "cat retry.log"}),
                    json!({"type": "bash_output", "command": "cat retry.log", "content": log})
                        .to_string(),
                );
            }
            40 => {
                data["tools"] = tool(
                    "Fetch",
                    json!({"url": "https://example.com"}),
                    "not json: ".to_string() + &"🚢 sailing ".repeat(4000),
                )
            }
            120 => {
                let lines: Vec<Value> = (1..=2000).map(|i| json!({"number": i, "content": format!("value_{i} = compute({i})  # 日本語")})).collect();
                data["tools"] = tool("Read", json!({"file_path": "/x/big.py"}), json!({"type": "file_content", "filePath": "/x/big.py", "totalLines": 2000, "lines": lines}).to_string());
            }
            140 => {
                let body = "é".repeat(16383) + "日本語" + &"ü✓".repeat(9000);
                data["tools"] = tool(
                    "Write",
                    json!({"file_path": "/x/notes.md", "content": body}),
                    json!({"type": "file_write", "filePath": "/x/notes.md", "content": "written"})
                        .to_string(),
                );
            }
            _ => {}
        }
        events.push(RawEvent::from_value(json!({"id": format!("r{k}"), "taskId": task, "type": "response", "timestamp": ts + 1000, "conversationId": "c_long", "data": data})));
    }
    events
}

/// What a client's history shows now, as the recording keeps it for Swift to compare.
fn shown(h: &flotilla_proto::History) -> Value {
    let messages: Vec<Value> = h
        .messages()
        .iter()
        .map(|m| {
            json!({
                "seq": m.seq,
                "kind": m.kind,
                "text": m.text,
                "cut": m.cut.iter().map(|c| c.field.clone()).collect::<Vec<_>>(),
                "input_bytes": m.tool_input_json.as_ref().map(String::len),
                "result_bytes": m.tool_result_json.as_ref().map(String::len),
            })
        })
        .collect();
    json!({"shown": messages, "more_above": h.more_above(), "loading": h.loading()})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_thread_the_swift_history_replays() {
    use flotilla_proto::history::{CAP, string_mark};
    let dir = std::env::temp_dir().join(format!(
        "flotilla-conformance-history-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    let store = Store::open(&db).unwrap();
    store
        .replace_environments(&[flotilla_core::boat::Environment {
            id: s("e1"),
            name: s("base"),
            is_default: true,
        }])
        .unwrap();
    let now = flotilla_core::util::now_ms();
    let code = store
        .mint_code(flotilla_core::devices::CODE_TTL, "cli", now)
        .unwrap();
    let token = store
        .pair_device(&code.code, "iPhone", "ios", None, now)
        .unwrap()
        .token;
    store
        .insert_thread(&thread("long", "A long thread"))
        .unwrap();
    let mut turns = 150;
    store
        .apply_events("long", &long_events(turns, None), false)
        .unwrap();
    store.rebuild_messages("long").unwrap();
    let world = World {
        dir,
        store,
        sample: vec![],
        at: Default::default(),
        usage: None,
    };

    let server = serve(&db).await;
    let key = ViewKey::Thread(s("long"));
    let mut phone = Phone::new(token.clone(), "5e55a9b0-0000-4000-8000-000000000002");
    phone.history = Some((key.clone(), flotilla_proto::History::new()));
    let target = flotilla_client::Target::parse(&format!("ws://{}", server.addr)).unwrap();
    let secret = flotilla_proto::Secret(token.clone());
    assert!(!phone.connect(server.addr, None).await);
    phone.sub(key.clone()).await;
    phone.barrier().await;
    world.check(&phone, "opening a long thread");
    let checkpoint = |phone: &mut Phone, name: &str| {
        let h = &phone.history.as_ref().unwrap().1;
        let mut v = shown(h);
        v["name"] = json!(name);
        phone.steps.push(json!({"history": v}));
    };
    checkpoint(&mut phone, "opened");
    {
        let View::Thread(d) = phone.core.replica().view(&key).unwrap() else {
            panic!()
        };
        assert!(d.older > 0 && d.messages.iter().all(|m| m.seq >= d.older));
    }

    // Scrolled up: the pages it wants, one at a time, over HTTP with the device's token.
    async fn fetch(
        phone: &mut Phone,
        target: &flotilla_client::Target,
        secret: &flotilla_proto::Secret,
    ) {
        let wanted = phone.history.as_mut().unwrap().1.wanted();
        phone.steps.push(json!({"wanted": wanted}));
        for page in wanted {
            let before = flotilla_proto::History::before(page);
            let (t, k) = (target.clone(), secret.clone());
            let got = tokio::task::spawn_blocking(move || {
                flotilla_client::history(&t, Some(&k), "long", before)
            })
            .await
            .unwrap()
            .unwrap();
            assert!(got.messages.iter().all(|m| {
                m.tool_input_json
                    .iter()
                    .chain(&m.tool_result_json)
                    .all(|s| s.len() <= CAP)
            }));
            phone.steps.push(
                json!({"page": {"before": before, "body": serde_json::to_value(&got).unwrap()}}),
            );
            phone.history.as_mut().unwrap().1.page(got);
        }
    }
    async fn more(
        phone: &mut Phone,
        target: &flotilla_client::Target,
        secret: &flotilla_proto::Secret,
    ) -> bool {
        let wants = phone.history.as_mut().unwrap().1.more();
        phone.steps.push(json!({"more": wants}));
        fetch(phone, target, secret).await;
        wants
    }
    assert!(more(&mut phone, &target, &secret).await);
    checkpoint(&mut phone, "a page up");
    assert!(more(&mut phone, &target, &secret).await);
    checkpoint(&mut phone, "two pages up");

    // The thread grows past a page: the view moves on, and what left it is kept.
    turns += 30;
    world
        .store
        .apply_events("long", &long_events(turns, None), false)
        .unwrap();
    world.store.rebuild_messages("long").unwrap();
    server.engine(Event::Changed(Changes::thread("long")));
    phone.barrier().await;
    world.check(&phone, "the view moved on");
    fetch(&mut phone, &target, &secret).await;
    checkpoint(&mut phone, "the view moved on");

    // A reply far up changes: only its page's mark moves, and that page comes again.
    world
        .store
        .apply_events(
            "long",
            &long_events(turns, Some((95, "Reply 95, rewritten"))),
            false,
        )
        .unwrap();
    world.store.rebuild_messages("long").unwrap();
    server.engine(Event::Changed(Changes::thread("long")));
    phone.barrier().await;
    world.check(&phone, "a change outside the view");
    checkpoint(&mut phone, "stale page shown");
    fetch(&mut phone, &target, &secret).await;
    checkpoint(&mut phone, "stale page fetched again");

    // A delta lost on the way: the view comes whole, and the pages held still count.
    phone.lose = Some(key.clone());
    turns += 2;
    world
        .store
        .apply_events(
            "long",
            &long_events(turns, Some((95, "Reply 95, rewritten"))),
            false,
        )
        .unwrap();
    world.store.rebuild_messages("long").unwrap();
    server.engine(Event::Changed(Changes::thread("long")));
    phone.barrier().await;
    turns += 25;
    world
        .store
        .apply_events(
            "long",
            &long_events(turns, Some((95, "Reply 95, rewritten"))),
            false,
        )
        .unwrap();
    world.store.rebuild_messages("long").unwrap();
    server.engine(Event::Changed(Changes::thread("long")));
    phone.barrier().await;
    phone.barrier().await;
    world.check(&phone, "a lost delta");
    fetch(&mut phone, &target, &secret).await;
    checkpoint(&mut phone, "after a resync");

    // A search (over the socket, as any request) finds a reply on the first page; the thread
    // opens at it: every page from those shown down to its own, one at a time here.
    let id = phone
        .ask(Request::Search {
            query: s("REPLY 12: done"),
            limit: Some(5),
        })
        .await;
    let mut answer = None;
    phone
        .pump(|d| match d {
            Down::Res {
                id: got, result, ..
            } if *got == id => {
                answer = Some(result.clone());
                true
            }
            _ => false,
        })
        .await;
    let Some(Ok(flotilla_proto::Reply::Hits(hits))) = answer else {
        panic!("{answer:?}")
    };
    // Reply 12 alone ("12:" starts a word, so not Reply 112's), on page 0.
    let seqs: Vec<i64> = hits.iter().filter_map(|h| h.seq).collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    let seq = *seqs.iter().min().unwrap();
    assert_eq!(
        world.store.messages_between("long", seq, seq + 1).unwrap()[0]
            .text
            .as_deref(),
        Some("Reply 12: done, é ✓ 日本")
    );
    phone.steps.push(json!({"hits": {"query": "REPLY 12: done", "seqs": seqs, "marks": hits.iter().map(|h| h.marks.clone()).collect::<Vec<_>>(), "snippets": hits.iter().map(|h| h.snippet.clone()).collect::<Vec<_>>()}}));
    {
        let h = &mut phone.history.as_mut().unwrap().1;
        assert!(!h.shows(seq));
        let reached = h.reach(seq);
        assert!(reached);
        phone
            .steps
            .push(json!({"reach": {"seq": seq, "wanted": reached}}));
    }
    loop {
        let wanted = phone.history.as_mut().unwrap().1.wanted_up_to(1);
        phone.steps.push(json!({"wanted": wanted, "room": 1}));
        if wanted.is_empty() {
            break;
        }
        for page in wanted {
            let before = flotilla_proto::History::before(page);
            let (t, k) = (target.clone(), secret.clone());
            let got = tokio::task::spawn_blocking(move || {
                flotilla_client::history(&t, Some(&k), "long", before)
            })
            .await
            .unwrap()
            .unwrap();
            phone.steps.push(
                json!({"page": {"before": before, "body": serde_json::to_value(&got).unwrap()}}),
            );
            phone.history.as_mut().unwrap().1.page(got);
        }
    }
    assert!(phone.history.as_ref().unwrap().1.shows(seq));
    checkpoint(&mut phone, "opened at a search hit");

    // Up to the very first message.
    while phone.history.as_ref().unwrap().1.more_above() {
        assert!(more(&mut phone, &target, &secret).await);
    }
    checkpoint(&mut phone, "all of it");
    let all = phone.history.as_ref().unwrap().1.messages();
    assert_eq!(
        all.iter().map(|m| m.seq).collect::<Vec<_>>(),
        (0..all.len() as i64).collect::<Vec<_>>()
    );
    assert_eq!(all.len(), world.store.messages("long").unwrap().len());

    // Every cut string, whole, by its mark.
    let cut: Vec<_> = all.iter().filter(|m| !m.cut.is_empty()).cloned().collect();
    assert!(cut.len() >= 4, "{} cut", cut.len());
    for m in &cut {
        for c in &m.cut {
            let (t, k, c2, seq) = (target.clone(), secret.clone(), c.clone(), m.seq);
            let whole = tokio::task::spawn_blocking(move || {
                flotilla_client::full(&t, Some(&k), "long", seq, &c2)
            })
            .await
            .unwrap()
            .unwrap();
            assert_eq!(string_mark(&whole), c.mark);
            assert_eq!(whole.len() as u64, c.bytes);
            phone.steps.push(json!({"full": {"seq": m.seq, "field": c.field, "mark": c.mark, "bytes": c.bytes, "body": whole}}));
        }
    }
    let resyncs = phone
        .steps
        .iter()
        .filter(|s| s["up"]["t"] == "resync")
        .count();
    let deltas = phone
        .steps
        .iter()
        .filter(|s| s["down"]["t"] == "delta")
        .count();
    let pages = phone
        .steps
        .iter()
        .filter(|s| s.get("page").is_some())
        .count();
    assert!(
        resyncs == 1 && deltas >= 3 && pages >= 5,
        "{resyncs} resyncs, {deltas} deltas, {pages} pages"
    );
    let views: Vec<Value> = phone
        .core
        .replica()
        .subscribed()
        .map(|k| {
            let mut v = key_json(k);
            v["rev"] = json!(phone.core.replica().rev(k));
            v["data"] = phone.core.replica().data(k).cloned().unwrap();
            v
        })
        .collect();
    server.stop().await;
    if let Ok(path) = std::env::var("FLOTILLA_WRITE_HISTORY") {
        let out = json!({
            "about": "A long thread recorded by crates/flotilla-server/tests/conformance.rs: the wire steps as in session.json, and flotilla_proto::History beside the view: more (the user scrolled up; whether a page was wanted), wanted (the pages to fetch; with room, at most that many), hits (a search's answer: the hits' seqs, marks and snippets), reach (opened at a hit's seq; whether pages were wanted), page (one fetched over HTTP: before, and the body), history (what it shows: each message's seq, kind, text, cut fields and tool string sizes in bytes), and full (a cut string fetched whole). views: the replica's views at the end.",
            "steps": phone.steps,
            "views": views,
        });
        std::fs::write(&path, serde_json::to_string(&out).unwrap() + "\n").unwrap();
        eprintln!(
            "wrote {path}: {} steps, {deltas} deltas, {pages} pages",
            phone.steps.len()
        );
    }
}

/// Deltas as Rust's `diff` makes them, for the Swift `apply` to replay: each case is a view's
/// old value, the ops, and the new value they must make. Random rows dropped, added, moved and
/// retitled (as `delta`'s own random test), messages appended, changed and trimmed, and ops
/// that can't apply (`new` is null: applying them must fail). With
/// `FLOTILLA_WRITE_DELTAS=<path>` it writes them there (`mobile/fixtures/conformance/deltas.json`).
#[test]
fn delta_cases_the_swift_apply_replays() {
    use flotilla_proto::delta::{apply, diff};
    use flotilla_proto::wire::Op;
    let row =
        |id: &str, title: &str| json!({"thread": {"id": id, "title": title}, "activity": "idle"});
    let msg = |seq: i64, text: &str| json!({"seq": seq, "kind": "text", "text": text});
    let mut cases = Vec::new();
    let mut case = |view: &ViewKey, old: Value, new: Value| {
        let ops = diff(view, &old, &new).unwrap();
        let mut got = old.clone();
        apply(view, &mut got, &ops).unwrap();
        assert_eq!(got, new);
        cases.push(json!({"view": key_json(view), "old": old, "ops": ops, "new": new}));
    };
    let t = ViewKey::Thread(s("t1"));
    let side = ViewKey::Sidebar;
    let old = json!({"id": "t1", "messages": [msg(0, "a"), msg(1, "b")], "start_error": null});
    case(
        &t,
        old.clone(),
        json!({"id": "t1", "messages": [msg(0, "a"), msg(1, "b!"), msg(2, "c")], "start_error": "boom"}),
    );
    case(
        &t,
        json!({"id": "t1", "messages": [msg(0, "a"), msg(1, "b"), msg(2, "c")]}),
        json!({"id": "t1", "messages": [msg(0, "a"), msg(1, "B")]}),
    );
    case(
        &t,
        old.clone(),
        json!({"id": "t1", "messages": [], "start_error": null}),
    );
    case(
        &t,
        json!({"id": "t1", "messages": []}),
        json!({"id": "t1", "messages": [msg(0, "a")]}),
    );
    case(&t, old.clone(), json!({"id": "t1", "messages": []}));
    case(
        &t,
        json!({"id": "t1", "messages": [msg(0, "1.0"), {"seq": 1, "kind": "tool", "cost": 1.5}]}),
        json!({"id": "t1", "messages": [msg(0, "1.0"), {"seq": 1, "kind": "tool", "cost": 2.0}, {"seq": 2, "kind": "text", "text": "é ✓ 日本"}]}),
    );
    // A thread's page marks go by page: one changed, one added and the view moved on, the
    // thread shrunk.
    let mark = |page: i64, m: &str| json!({"page": page, "mark": m});
    let paged = json!({"id": "t1", "older": 100, "pages": [mark(0, "a0"), mark(1, "a1"), mark(2, "a2"), mark(3, "a3")], "messages": [msg(100, "x"), msg(150, "y")]});
    case(
        &t,
        paged.clone(),
        json!({"id": "t1", "older": 100, "pages": [mark(0, "b0"), mark(1, "a1"), mark(2, "a2"), mark(3, "a3")], "messages": [msg(100, "x"), msg(150, "y")]}),
    );
    case(
        &t,
        paged.clone(),
        json!({"id": "t1", "older": 150, "pages": [mark(0, "a0"), mark(1, "a1"), mark(2, "a2"), mark(3, "b3"), mark(4, "a4")], "messages": [msg(150, "y"), msg(200, "z")]}),
    );
    case(
        &t,
        paged,
        json!({"id": "t1", "older": 0, "pages": [mark(0, "a0"), mark(1, "c1")], "messages": [msg(0, "w"), msg(50, "v")]}),
    );
    let rows = json!({"threads": [row("a", "A"), row("b", "B"), row("c", "C")], "waiting": []});
    case(
        &side,
        rows.clone(),
        json!({"threads": [row("c", "C!"), row("a", "A"), row("b", "B")], "waiting": []}),
    );
    case(
        &side,
        rows.clone(),
        json!({"threads": [row("d", "D"), row("a", "A"), row("c", "C")], "waiting": ["a"]}),
    );
    case(
        &side,
        rows.clone(),
        json!({"threads": [row("a", "A"), row("b", "B"), row("c", "C"), row("d", "D")], "waiting": []}),
    );
    case(
        &side,
        rows,
        json!({"threads": [row("a", "A"), row("a", "A2")], "waiting": []}),
    );
    // Random rows, as `delta`'s own test makes them.
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let mut ids: Vec<u64> = (0..6).collect();
    let mut old =
        json!({"threads": ids.iter().map(|i| row(&i.to_string(), "x")).collect::<Vec<_>>()});
    for round in 0..150 {
        ids.retain(|_| next(5) != 0);
        for _ in 0..next(3) {
            ids.insert(
                next(ids.len() as u64 + 1) as usize,
                100 + round * 3 + next(3),
            );
        }
        let mut seen = std::collections::HashSet::new();
        ids.retain(|i| seen.insert(*i));
        if ids.len() > 1 && next(2) == 0 {
            let a = next(ids.len() as u64) as usize;
            let b = next(ids.len() as u64) as usize;
            ids.swap(a, b);
        }
        let new = json!({"threads": ids.iter().map(|i| row(&i.to_string(), if next(4) == 0 { "y" } else { "x" })).collect::<Vec<_>>()});
        case(&side, old, new.clone());
        old = new;
    }
    // Ops that don't fit what's held: applying them must fail, so the view is asked for whole.
    let held = json!({"id": "t1", "messages": [msg(0, "a")]});
    for ops in [
        vec![Op::Del {
            field: s("messages"),
            keys: vec![json!(7)],
        }],
        vec![Op::Order {
            field: s("messages"),
            keys: vec![json!(0), json!(1)],
        }],
        vec![Op::Order {
            field: s("messages"),
            keys: vec![json!(5)],
        }],
        vec![Op::Put {
            field: s("sent"),
            v: json!({}),
        }],
        vec![Op::Put {
            field: s("messages"),
            v: json!({"kind": "text"}),
        }],
        vec![Op::Put {
            field: s("start_error"),
            v: json!({"seq": 1}),
        }],
    ] {
        let mut got = held.clone();
        assert!(apply(&t, &mut got, &ops).is_err(), "{ops:?}");
        cases.push(json!({"view": key_json(&t), "old": held, "ops": ops, "new": null}));
    }
    // One from a newer server is skipped.
    let unknown: Op = serde_json::from_str(r#"{"op":"splice","field":"messages","at":3}"#).unwrap();
    let mut got = held.clone();
    apply(&t, &mut got, std::slice::from_ref(&unknown)).unwrap();
    cases.push(json!({"view": key_json(&t), "old": held, "ops": [{"op": "splice", "field": "messages", "at": 3}], "new": got}));
    if let Ok(path) = std::env::var("FLOTILLA_WRITE_DELTAS") {
        let out = json!({"about": "Deltas made by flotilla_proto::delta::diff (crates/flotilla-server/tests/conformance.rs): applying ops to old must give new, or fail when new is null.", "cases": cases});
        std::fs::write(&path, serde_json::to_string(&out).unwrap() + "\n").unwrap();
        eprintln!("wrote {path}: {} cases", cases.len());
    }
}

/// A thread as `search.json` writes it, for `flotilla_proto::search::Subject`.
struct SearchThread {
    id: Option<String>,
    title: String,
    branch: Option<String>,
    project: Option<String>,
    prs: Vec<(i64, Option<String>, Vec<String>)>,
    activity: Option<i64>,
    opens: Option<flotilla_proto::search::Opens>,
}

impl SearchThread {
    fn of(v: &Value) -> SearchThread {
        let s = |k: &str| v[k].as_str().map(str::to_string);
        SearchThread {
            id: s("id"),
            title: s("title").unwrap_or_default(),
            branch: s("branch"),
            project: s("project"),
            prs: v["prs"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|p| {
                            (
                                p["number"].as_i64().unwrap(),
                                p["title"].as_str().map(str::to_string),
                                p["references"]
                                    .as_array()
                                    .map(|r| {
                                        r.iter()
                                            .filter_map(|x| x.as_str().map(str::to_string))
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            activity: v["activity"].as_i64(),
            opens: (!v["opens"].is_null())
                .then(|| serde_json::from_value(v["opens"].clone()).unwrap()),
        }
    }

    fn subject(&self) -> flotilla_proto::search::Subject<'_> {
        use flotilla_proto::search::{PrRef, Subject};
        Subject {
            id: self.id.as_deref(),
            title: &self.title,
            branch: self.branch.as_deref(),
            project: self.project.as_deref(),
            prs: self
                .prs
                .iter()
                .map(|(n, t, r)| PrRef {
                    number: *n,
                    title: t.as_deref(),
                    references: r.clone(),
                })
                .collect(),
            activity: self.activity,
            opens: self.opens.as_ref(),
        }
    }
}

/// Search's rules, for the Swift `Search` (ios/FlotillaKit) to follow: how queries fold and
/// split, how each thread matches a query (quality, strength, what's marked in its title, the
/// field that says why, its pick, its score), how lists of threads rank (typos only when nothing
/// is strong), picked queries and use. With `FLOTILLA_WRITE_SEARCH=<path>` it writes them there;
/// without, `mobile/fixtures/conformance/search.json` must be what these make.
#[test]
fn search_cases_the_swift_search_follows() {
    use flotilla_proto::search;
    let now: i64 = 1_792_713_600_000;
    let day: i64 = 86_400_000;
    let t = |title: &str| json!({"title": title, "activity": now - 30 * day});
    let with = |mut v: Value, k: &str, x: Value| {
        v[k] = x;
        v
    };
    let opened = |v: Value, at: i64, score: f64, picks: &[(&str, i64)]| {
        with(
            v,
            "opens",
            json!({"at": at, "score": score, "picks": picks.iter().map(|(q, a)| json!({"query": q, "at": a})).collect::<Vec<_>>()}),
        )
    };
    let pr = |v: Value, n: i64, title: Option<&str>| {
        let mut prs = v["prs"].as_array().cloned().unwrap_or_default();
        prs.push(json!({"number": n, "title": title, "references": []}));
        with(v, "prs", json!(prs))
    };
    let folds = [
        "  “Fix   the CAFÉ” ",
        "\"\"",
        "cafe\u{301}",
        "Don’t",
        "ŁÓDŹ Wrocław",
        "İstanbul",
        "Straße ﬁle",
        "ΣΊΣΥΦΟΣ",
        "Ａｂｃ",
        "tiếng Việt",
        "🚀 launch",
        "# fix",
        "´´´",
        "日本語のコメント",
        "'quoted' x",
    ];
    let token_queries = [
        "“Retry” the FTS ok 部署 x",
        "df -h ls",
        "retry",
        "部署",
        "don’t",
        "a b",
        "검색 기능",
        "\"sidebar\"",
        "İstanbul",
        "´´´",
        "  ",
    ];
    let single: Vec<(&str, Value)> = vec![
        ("search", t("useThreadSearch hook")),
        ("perf", t("fix/sidebar-perf")),
        ("12", t("Upgrade to pnpm12")),
        ("sch", t("Fix JSONSchema parsing")),
        ("ser", t("Fix HTTPServer crash")),
        (".ts", t("Rename index.ts")),
        (".ts", t("Fix parser.tsx")),
        ("githuba", t("GitHubActions setup")),
        ("ci", t("Decision log")),
        ("auth", t("Fix oauth redirect")),
        ("auth", t("Authentication cleanup")),
        ("auth", t("Share auth across worktrees")),
        ("arch", t("Research notes")),
        (
            "arch",
            with(t("Notes"), "branch", json!("research-extension-system")),
        ),
        ("vscode", t("VS Code extension settings")),
        ("vscode", t("Fixvscode and VS Code")),
        ("fixuibug", t("Fix UI bug in sidebar")),
        ("fixsid", t("Fix sidebar lag")),
        ("fixsid", t("Feature: Fix sidebar lag")),
        ("fsl", t("Fix sidebar lag")),
        ("inth", t("Show skill names in thread overview")),
        ("tos", t("Move Client State to Server")),
        ("t3co", t("T3 Code vs Gemini")),
        ("gomod", t("Migrate to Go modules")),
        ("\"sidebar lag\"", t("Fix sidebar lag")),
        ("cafe\u{301}", t("Café menu")),
        ("lodz", t("ŁÓDŹ, Wrocław")),
        ("pnpm12", t("Upgrade to pnpm 12")),
        ("gpt5", t("GPT-5 eval")),
        ("ios26", t("iOS 26 support")),
        ("pnpm13", t("Upgrade to pnpm 12")),
        ("railwya", t("Hosting alternatives to Railway")),
        ("trim mpc", t("Trim MCP model lists")),
        ("drga", t("Restore drag handles")),
        ("t3coed", t("t3code perf audit")),
        ("oath2", t("Fix oauth2 redirect")),
        ("ilde", t("Typesafe App Builder")),
        ("ilde", t("De-risk idle tunnel cleanup")),
        ("ing", t("Nightly build fix")),
        ("srv", t("Server restart")),
        ("sidb", t("Fix sidebar lag")),
        ("cron", t("Config cleanup")),
        ("fix the sidebar", t("Fix Sidebar Lag")),
        ("the", t("Fix sidebar lag")),
        ("don't", t("Don’t merge yet")),
        ("githbu", t("GitHub API usage")),
        ("macso", t("Fix macOS window")),
        ("cmd enter", t("Fix Cmd+Enter")),
        ("cmd enter", t("Open links external")),
        ("# fix", t("Fix sidebar lag")),
        ("#", t("Fix sidebar lag")),
        ("´´´", t("Fix sidebar lag")),
        ("🚀", t("🚀 Launch checklist")),
        ("搜索", t("实现全文搜索")),
        ("検索", t("新しい検索機能")),
        ("ไทย", t("ภาษาไทย")),
        ("jul", t("Julius’s notes")),
        ("s", t("Julius’s notes")),
        (
            "flotilla/cmdk",
            with(
                t("Cmd+K search"),
                "branch",
                json!("flotilla/cmdk-search-v2"),
            ),
        ),
        (
            "cleanup",
            with(
                pr(t("Trim model lists"), 22889, Some("Smaller MCP payloads")),
                "branch",
                json!("flotilla/cleanup-payloads"),
            ),
        ),
        (
            "pull request #22889",
            with(
                pr(t("Trim model lists"), 22889, Some("Smaller MCP payloads")),
                "branch",
                json!("flotilla/cleanup-payloads"),
            ),
        ),
        (
            "pr22889",
            pr(t("Trim model lists"), 22889, Some("Smaller MCP payloads")),
        ),
        (
            "payloads t3code",
            with(
                pr(t("Trim model lists"), 22889, Some("Smaller MCP payloads")),
                "project",
                json!("acme/t3code"),
            ),
        ),
        (
            "trim lakebed",
            with(
                pr(t("Trim model lists"), 22889, Some("Smaller MCP payloads")),
                "project",
                json!("acme/t3code"),
            ),
        ),
        ("22889", pr(t("Stats"), 22189, None)),
        (
            "fix ci",
            pr(
                pr(
                    t("eMudhra certificate update request"),
                    22645,
                    Some("fix model picker"),
                ),
                22649,
                Some("CI speedups"),
            ),
        ),
        (
            "lakebed perf",
            pr(
                with(t("Weekly notes"), "project", json!("lakebed")),
                243,
                Some("perf: faster cold starts"),
            ),
        ),
        (
            "5fe6f59c",
            with(t("Trim model lists"), "id", json!("thread-5fe6f59c-4976")),
        ),
        (
            "github.com/acme/t3code/pull/22889/files",
            with(
                t("Trim"),
                "prs",
                json!([{"number": 22889, "references": ["https://github.com/acme/t3code/pull/22889"]}]),
            ),
        ),
        (
            "github.com/acme/t3code/pull/2288",
            with(
                t("Trim"),
                "prs",
                json!([{"number": 22889, "references": ["https://github.com/acme/t3code/pull/22889"]}]),
            ),
        ),
        (
            "rel",
            opened(
                t("Release checklist"),
                now - 5 * day,
                1.,
                &[("rel", now - 5 * day)],
            ),
        ),
        (
            "rel",
            opened(
                t("Release checklist"),
                now - 90 * day,
                1.,
                &[("rel", now - 90 * day)],
            ),
        ),
        (
            "fix",
            opened(
                t("Fix browser"),
                now,
                2.,
                &[("fix browser", now), ("fix", now - 2 * day)],
            ),
        ),
        (
            "lakebed",
            opened(t("Lakebed perf audit"), now, 1., &[("lakebed perf", now)]),
        ),
        (
            "goldbug",
            opened(
                with(t("Goldbug notes"), "activity", json!(now - 40 * day)),
                now - day,
                12.,
                &[],
            ),
        ),
        ("x", t("")),
        ("", t("Anything")),
    ];
    let mut threads = Vec::new();
    for (query, thread) in &single {
        let st = SearchThread::of(thread);
        for typos in [false, true] {
            let m =
                search::score_thread(&search::fold_query(query), &st.subject(), now, typos, true);
            let m = m.map(|m| {
                json!({
                    "quality": m.quality, "score": m.score, "strong": m.strong,
                    "ranges": search::byte_ranges(&st.title, &m.ranges),
                    "hint": m.hint.map(|h| json!({"label": h.label, "text": h.text, "ranges": search::byte_ranges(&h.text, &h.ranges)})),
                    "picked_at": m.picked_at, "picked_exactly": m.picked_exactly,
                })
            });
            threads.push(json!({"query": query, "typos": typos, "thread": thread, "match": m}));
        }
    }
    let lists: Vec<(&str, Vec<Value>)> = vec![
        (
            "auth",
            vec![
                with(t("Fix oauth redirect"), "activity", json!(now)),
                t("Authentication cleanup"),
                t("Share auth across worktrees"),
            ],
        ),
        (
            "goldbug",
            vec![
                with(t("Goldbug epilogue"), "activity", json!(now - 20 * day)),
                with(t("Goldbug processes"), "activity", json!(now - day)),
            ],
        ),
        (
            "goldbug",
            vec![
                with(t("Goldbug processes"), "activity", json!(now - day)),
                opened(
                    with(t("Goldbug notes"), "activity", json!(now - 40 * day)),
                    now - day,
                    12.,
                    &[],
                ),
            ],
        ),
        (
            "rel",
            vec![
                opened(
                    t("Release checklist"),
                    now - 5 * day,
                    1.,
                    &[("rel", now - 5 * day)],
                ),
                with(t("Relay pairing"), "activity", json!(now)),
            ],
        ),
        (
            "re",
            vec![
                opened(
                    t("Release checklist"),
                    now - 5 * day,
                    1.,
                    &[("rel", now - 5 * day)],
                ),
                with(t("Relay pairing"), "activity", json!(now)),
            ],
        ),
        (
            "rela",
            vec![
                opened(
                    t("Release checklist"),
                    now - 5 * day,
                    1.,
                    &[("rel", now - 5 * day)],
                ),
                with(t("Relay pairing"), "activity", json!(now)),
            ],
        ),
        (
            "auth",
            vec![
                opened(
                    t("Fix oauth redirect"),
                    now - day,
                    1.,
                    &[("auth", now - day)],
                ),
                t("Share auth across worktrees"),
            ],
        ),
        (
            "rel",
            vec![
                opened(
                    t("Release checklist"),
                    now - day,
                    2.,
                    &[("rel", now - 30 * day)],
                ),
                opened(t("Relay pairing"), now - day, 1., &[("rel", now - day)]),
            ],
        ),
        (
            "deploy",
            vec![
                opened(t("Deploy"), now - day, 1., &[("deploy", now - day)]),
                opened(t("Deploy timeout on Railway"), now, 1., &[("deploy", now)]),
            ],
        ),
        (
            "fix",
            vec![
                opened(
                    t("Fix browser"),
                    now,
                    2.,
                    &[("fix browser", now), ("fix", now - 2 * day)],
                ),
                opened(t("Fix sidebar"), now - day, 1., &[("fix", now - day)]),
            ],
        ),
        (
            "lakebed",
            vec![
                opened(t("Lakebed perf audit"), now, 1., &[("lakebed perf", now)]),
                t("Lakebed"),
            ],
        ),
        (
            "deploy",
            vec![
                opened(
                    t("Deploy notes"),
                    now,
                    2.,
                    &[("scripts", now), ("deploy", now - 60 * day)],
                ),
                opened(t("Deploy timeout"), now - day, 1., &[("deploy", now - day)]),
            ],
        ),
        (".ts", vec![t("Fix parser.tsx"), t("Rename index.ts")]),
        (
            "railwya",
            vec![
                t("Hosting alternatives to Railway"),
                t("Trim MCP model lists"),
            ],
        ),
        (
            "ilde",
            vec![t("Typesafe App Builder"), t("De-risk idle tunnel cleanup")],
        ),
        ("srv", vec![t("Server restart"), t("Other")]),
        (
            "cmd enter",
            vec![t("Fix Cmd+Enter"), t("Open links external")],
        ),
        (
            "vscode",
            vec![
                t("VS Code remote support"),
                with(
                    with(t("Old work"), "branch", json!("vscode")),
                    "activity",
                    json!(now - 400 * day),
                ),
            ],
        ),
        (
            "flotilla connect",
            vec![
                opened(
                    with(
                        with(
                            with(
                                t("Diagnose Connect provisioning failures"),
                                "activity",
                                json!(now),
                            ),
                            "branch",
                            json!("flotilla/diagnose-connect"),
                        ),
                        "project",
                        json!("acme/flotilla"),
                    ),
                    now,
                    6.,
                    &[],
                ),
                with(
                    t("Flotilla Connect Tunnel Lifecycle Review"),
                    "project",
                    json!("acme/flotilla"),
                ),
            ],
        ),
        (
            "posthog",
            vec![
                t("Projectless threads without Git"),
                t("Concern about connection setup"),
                t("Design notes"),
            ],
        ),
    ];
    let ranks: Vec<Value> = lists
        .iter()
        .map(|(query, list)| {
            let parsed: Vec<SearchThread> = list.iter().map(SearchThread::of).collect();
            let q = search::fold_query(query);
            let order: Vec<String> = search::rank(parsed.iter().collect(), |t, typos| {
                search::score_thread(&q, &t.subject(), now, typos, true)
            })
            .into_iter()
            .map(|(t, _)| t.title.clone())
            .collect();
            json!({"query": query, "threads": list, "order": order})
        })
        .collect();
    let opens_json = |at: i64, score: f64| json!({"at": at, "score": score});
    let boosts: Vec<Value> = [(None, None), (Some(now), None), (Some(now - 14 * day), None), (None, Some((now, 12.))), (Some(now), Some((now - day, 3.))), (Some(now - 90 * day), Some((now - 60 * day, 40.)))]
        .into_iter()
        .map(|(activity, opens): (Option<i64>, Option<(i64, f64)>)| {
            let o = opens.map(|(at, score)| search::Opens { at, score, picks: vec![] });
            json!({"activity": activity, "opens": opens.map(|(a, s)| opens_json(a, s)), "boost": search::usage_boost(activity, o.as_ref(), now)})
        })
        .collect();
    let rocket = format!("{}🚀", "a".repeat(31));
    let picked: Vec<Value> = [
        "  Upl ",
        "investigate    workspace thread reset bug",
        rocket.as_str(),
        "“Quoted”",
    ]
    .iter()
    .map(|q| json!({"query": q, "picked": search::picked_query(q)}))
    .collect();
    let out = json!({
        "about": "flotilla_proto::search's rules (crates/flotilla-server/tests/conformance.rs), T3 Code's ported: queries folded and split, each thread's match for a query (quality, strength, title marks and a hint's as UTF-8 bytes, picks, score), lists ranked, picked queries and use.",
        "now": now,
        "folds": folds.iter().map(|q| json!({"query": q, "folded": search::fold_query(q)})).collect::<Vec<_>>(),
        "tokens": token_queries.iter().map(|q| { let t = search::message_tokens(q); json!({"query": q, "long": t.long, "short": t.short, "words": t.words, "messages": search::finds_messages(q)}) }).collect::<Vec<_>>(),
        "threads": threads,
        "ranks": ranks,
        "picked": picked,
        "boosts": boosts,
    });
    let committed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mobile/fixtures/conformance/search.json");
    match std::env::var("FLOTILLA_WRITE_SEARCH") {
        Ok(path) => {
            std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
            eprintln!(
                "wrote {path}: {} thread cases, {} lists",
                single.len() * 2,
                lists.len()
            );
        }
        // As values: the keys' order depends on serde_json's features in the build.
        Err(_) => {
            let held: Value =
                serde_json::from_str(&std::fs::read_to_string(&committed).unwrap_or_default())
                    .unwrap_or_default();
            // Through the same parse: serde_json reads a float back within a unit of its last place.
            let out: Value =
                serde_json::from_str(&serde_json::to_string_pretty(&out).unwrap()).unwrap();
            assert!(
                held == out,
                "search.json is out of date: run with FLOTILLA_WRITE_SEARCH={}",
                committed.display()
            );
        }
    }
}
