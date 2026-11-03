//! The app's own server (`Local`): the window through its pipe, the CLI through the socket,
//! other devices only while they're let in, and the database's lock gone as soon as it stops.

use flotilla_client::{Client, Info, Pipe, Target};
use flotilla_core::lock;
use flotilla_engine::host::Mode;
use flotilla_proto::{ClientMsg, Connection, Reply, Request, ServerMsg, View, ViewKey};
use flotilla_server::{Config, Instead, Listen, Local, Refusal, Server};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn s(v: &str) -> String {
    v.to_string()
}

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A client and what it's handed.
struct Seen {
    client: Client,
    rx: mpsc::Receiver<ServerMsg>,
}

impl Seen {
    fn connect(target: Target, token: Option<String>) -> Seen {
        let (tx, rx) = mpsc::channel();
        let client = Client::connect(
            target,
            Info {
                app: s("test"),
                version: s("0"),
                token: token.map(flotilla_proto::Secret),
                scope: None,
            },
            move |m| tx.send(m).is_ok(),
        );
        Seen { client, rx }
    }

    /// The first message `pick` takes, within a few seconds.
    fn wait<T>(&self, what: &str, mut pick: impl FnMut(ServerMsg) -> Option<T>) -> T {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(m) => {
                    if let Some(t) = pick(m) {
                        return t;
                    }
                }
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
    }

    fn sidebar(&self) -> flotilla_proto::Sidebar {
        self.client.send(ClientMsg::Sub {
            view: ViewKey::Sidebar,
        });
        self.wait("the sidebar", |m| match m {
            ServerMsg::Snap { view } => match *view {
                View::Sidebar(s) => Some(s),
                _ => None,
            },
            _ => None,
        })
    }

    fn ask(&self, id: u64, req: Request) -> Result<Reply, String> {
        self.client.send(ClientMsg::Req { id, req });
        self.wait("an answer", |m| match m {
            ServerMsg::Res { id: got, result } if got == id => Some(result),
            _ => None,
        })
    }
}

#[test]
fn the_apps_server_lets_in_its_window_the_cli_and_devices_it_allows_then_lets_go() {
    let dir = Dir(std::env::temp_dir().join(format!("flotilla-local-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let db = dir.0.join("flotilla.db");
    let store = flotilla_core::store::Store::open(&db).unwrap();
    store.insert_thread(&thread("t1", "On This Mac")).unwrap();
    // A link a past run (or `flotilla serve`) gave out doesn't lead here now.
    store
        .kv_set(
            flotilla_engine::host::SERVER_URL_KEY,
            "https://old.tail1.ts.net",
        )
        .unwrap();
    let socket = dir.0.join("run").join("server.sock");

    let local = Local::start(db.clone(), Some(socket.clone()), Mode::Offline).unwrap();
    // The lock says it's the app's, and where the CLI goes.
    let holder = lock::holder(&db).unwrap();
    assert_eq!(
        (holder.kind.as_str(), holder.address.as_deref()),
        ("app", Some(format!("unix:{}", socket.display()).as_str()))
    );
    assert_eq!(local.socket(), Some(socket.as_path()));
    // One engine per database: a second can't start, and another app is no server to use.
    assert_eq!(
        Local::start(db.clone(), None, Mode::Offline).err(),
        Some(Refusal {
            reason: s(lock::BUSY),
            instead: None
        })
    );

    // The window, through the pipe.
    let door = local.doorway();
    let window = Seen::connect(Target::Memory(Pipe::new(move || door.open())), None);
    assert_eq!(
        window
            .sidebar()
            .threads
            .iter()
            .map(|r| r.thread.title.as_str())
            .collect::<Vec<_>>(),
        ["On This Mac"]
    );
    // The CLI, through the socket: the same views, and requests that write.
    let cli = Seen::connect(Target::Unix(socket.clone()), None);
    assert_eq!(cli.sidebar().threads.len(), 1);
    assert_eq!(cli.ask(1, Request::MarkUnread(s("t1"))), Ok(Reply::Done));
    window.wait("the window to see it", |m| match m {
        ServerMsg::Snap { view } => match *view {
            View::Sidebar(s) => s
                .threads
                .iter()
                .any(|r| r.activity == flotilla_proto::Activity::Unread)
                .then_some(()),
            _ => None,
        },
        _ => None,
    });
    // No devices yet: there's nothing to pair with.
    assert_eq!(
        window
            .ask(1, Request::PairCode { ttl_secs: None })
            .err()
            .as_deref(),
        Some("Let other devices connect first")
    );

    // Let devices in, on this machine: a phone pairs and connects with its token.
    let access = local.access();
    let opened = access.open(0, false).unwrap();
    assert!(
        opened.url.starts_with("http://127.0.0.1:") && opened.tailscale.is_none(),
        "{opened:?}"
    );
    let code = match window.ask(2, Request::PairCode { ttl_secs: None }) {
        Ok(Reply::Code(c)) => c,
        other => panic!("{other:?}"),
    };
    let url = flotilla_proto::pair::read(&code.link).unwrap().server;
    assert_eq!(url, opened.url);
    let paired = flotilla_client::pair(&opened.url, &code.code, "Phone", "ios").unwrap();
    let phone = Seen::connect(
        Target::parse(&opened.url).unwrap(),
        Some(paired.token.0.clone()),
    );
    assert_eq!(phone.sidebar().threads.len(), 1);
    // Without a token, nothing.
    let stranger = Seen::connect(Target::parse(&opened.url).unwrap(), None);
    stranger.wait("a refusal", |m| {
        matches!(m, ServerMsg::Connection(Connection::Refused { .. })).then_some(())
    });

    // No more devices: the phone is dropped, and no code can be made.
    access.close();
    phone.wait("the phone dropped", |m| {
        matches!(m, ServerMsg::Connection(Connection::Offline { .. })).then_some(())
    });
    assert_eq!(
        window
            .ask(3, Request::PairCode { ttl_secs: None })
            .err()
            .as_deref(),
        Some("Let other devices connect first")
    );
    // The window and the CLI carry on.
    assert!(window.ask(4, Request::CachedRepos).is_ok());
    assert!(cli.ask(2, Request::CachedRepos).is_ok());

    // Stopped: the lock and the socket are gone the moment it returns.
    drop(local);
    assert_eq!(lock::holder(&db), None);
    assert!(!socket.exists());
    assert!(lock::Lock::take(&db, &lock::Holder::new("server")).is_ok());
    window.wait("the window dropped", |m| {
        matches!(m, ServerMsg::Connection(Connection::Offline { .. })).then_some(())
    });
}

/// A client that can't reach its server tries again at once when asked (the app's Retry),
/// rather than after a wait that has grown to seconds.
#[test]
fn retry_connects_at_once_rather_than_after_the_wait() {
    let dir = Dir(std::env::temp_dir().join(format!("flotilla-retry-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let (db, socket) = (
        dir.0.join("flotilla.db"),
        dir.0.join("run").join("server.sock"),
    );
    flotilla_core::store::Store::open(&db)
        .unwrap()
        .insert_thread(&thread("t1", "Down at first"))
        .unwrap();
    let seen = Seen::connect(Target::Unix(socket.clone()), None);
    // Five tries failed: the next waits at least 4 s × 0.67.
    for _ in 0..5 {
        seen.wait("a failed try", |m| {
            matches!(m, ServerMsg::Connection(Connection::Offline { .. })).then_some(())
        });
    }
    let _local = Local::start(db, Some(socket), Mode::Offline).unwrap();
    let asked = Instant::now();
    seen.client.retry();
    seen.wait("online", |m| {
        matches!(m, ServerMsg::Connection(Connection::Online)).then_some(())
    });
    assert!(
        asked.elapsed() < Duration::from_millis(2000),
        "took {:?}",
        asked.elapsed()
    );
}

#[test]
fn letting_devices_in_from_a_stopped_server_changes_nothing_for_the_next() {
    let dir =
        Dir(std::env::temp_dir().join(format!("flotilla-local-access-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let db = dir.0.join("flotilla.db");
    let url = || {
        flotilla_core::store::Store::open(&db)
            .unwrap()
            .kv_get(flotilla_engine::host::SERVER_URL_KEY)
            .unwrap()
    };
    let first = Local::start(db.clone(), None, Mode::Offline).unwrap();
    // Kept by something that finishes late (the app's background step).
    let late = first.access();
    late.open(0, false).unwrap();
    drop(first);
    assert_eq!(url(), None);
    let next = Local::start(db.clone(), None, Mode::Offline).unwrap();
    let opened = next.access().open(0, false).unwrap();
    // The old one can neither open nor close the next one's door.
    assert!(late.open(0, false).is_err());
    late.close();
    assert_eq!(url(), Some(opened.url));
    drop(next);
}

/// A door that can't open again (its fixed port taken meanwhile) is shut, as closing leaves
/// it: the old one closed first, so its devices are dropped, and no pairing link leads to it.
/// The next open works.
#[test]
fn a_door_that_cant_open_again_is_shut() {
    let dir =
        Dir(std::env::temp_dir().join(format!("flotilla-local-reopen-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let db = dir.0.join("flotilla.db");
    flotilla_core::store::Store::open(&db)
        .unwrap()
        .insert_thread(&thread("t1", "On This Mac"))
        .unwrap();
    let url = || {
        flotilla_core::store::Store::open(&db)
            .unwrap()
            .kv_get(flotilla_engine::host::SERVER_URL_KEY)
            .unwrap()
    };
    let local = Local::start(db.clone(), None, Mode::Offline).unwrap();
    let door = local.doorway();
    let window = Seen::connect(Target::Memory(Pipe::new(move || door.open())), None);
    let access = local.access();
    let opened = access.open(0, false).unwrap();
    let code = match window.ask(1, Request::PairCode { ttl_secs: None }) {
        Ok(Reply::Code(c)) => c,
        other => panic!("{other:?}"),
    };
    let paired = flotilla_client::pair(&opened.url, &code.code, "Phone", "ios").unwrap();
    let phone = Seen::connect(
        Target::parse(&opened.url).unwrap(),
        Some(paired.token.0.clone()),
    );
    assert_eq!(phone.sidebar().threads.len(), 1);

    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let e = access
        .open(taken.local_addr().unwrap().port(), false)
        .unwrap_err();
    assert!(e.starts_with("Couldn't listen on 127.0.0.1:"), "{e}");
    phone.wait("the phone dropped", |m| {
        matches!(m, ServerMsg::Connection(Connection::Offline { .. })).then_some(())
    });
    assert_eq!(url(), None);
    assert_eq!(
        window
            .ask(2, Request::PairCode { ttl_secs: None })
            .err()
            .as_deref(),
        Some("Let other devices connect first")
    );

    let again = access.open(0, false).unwrap();
    assert_eq!(url(), Some(again.url.clone()));
    assert!(matches!(
        window.ask(3, Request::PairCode { ttl_secs: None }),
        Ok(Reply::Code(_))
    ));
    drop(taken);
    drop(local);
}

/// This Mac's server won't start beside `flotilla serve` on the same database: the refusal
/// offers that server's socket, which the window reaches with no token, seeing the same threads
/// (and the ids the server adopted). Neither starts on a database that moved to another
/// Flotilla: each says where it went, and lets go of the lock.
#[test]
fn a_server_here_is_offered_and_a_moved_database_is_refused() {
    let dir = Dir(std::env::temp_dir().join(format!("flotilla-offer-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let db = dir.0.join("flotilla.db");
    let store = flotilla_core::store::Store::open(&db).unwrap();
    store.insert_thread(&thread("t1", "Served here")).unwrap();
    flotilla_core::owner::adopt(&store, &[s("srv_mac")]).unwrap();
    drop(store);
    let socket = dir.0.join("run").join("server.sock");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let config = |db: &std::path::Path, listen: Vec<Listen>| Config {
        db: db.to_path_buf(),
        listen,
        mode: Mode::Offline,
        role: flotilla_engine::host::Role::Server,
    };
    let server = rt
        .block_on(Server::bind(config(
            &db,
            vec![Listen::Unix(socket.clone())],
        )))
        .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = rt.spawn(server.run(async {
        let _ = stopped.await;
    }));

    let refused = Local::start(db.clone(), None, Mode::Offline).err().unwrap();
    let address = format!("unix:{}", socket.display());
    assert_eq!(
        refused,
        Refusal {
            reason: s(lock::BUSY),
            instead: Some(Instead::Socket(address.clone()))
        }
    );
    let window = Seen::connect(Target::parse(&address).unwrap(), None);
    assert_eq!(
        window
            .sidebar()
            .threads
            .iter()
            .map(|r| r.thread.title.as_str())
            .collect::<Vec<_>>(),
        ["Served here"]
    );
    assert_eq!(
        window.client.welcome().map(|w| w.adopted),
        Some(vec![s("srv_mac")])
    );
    drop(window);
    let _ = stop.send(());
    rt.block_on(serving).unwrap().unwrap();

    // Moved: `flotilla serve` and This Mac's server each refuse, saying where, and hold nothing.
    let moved = flotilla_core::owner::Moved::to("srv_box", Some(s("https://box.tail1.ts.net")));
    flotilla_core::owner::mark_moved(&db, &moved).unwrap();
    let e = rt
        .block_on(Server::bind(config(
            &db,
            vec![Listen::Unix(socket.clone())],
        )))
        .err()
        .unwrap();
    assert!(e.contains("https://box.tail1.ts.net"), "{e}");
    assert_eq!(
        Local::start(db.clone(), None, Mode::Offline).err(),
        Some(Refusal {
            reason: moved.reason(),
            instead: Some(Instead::Moved(moved))
        })
    );
    assert_eq!(lock::holder(&db), None);
}

fn thread(id: &str, title: &str) -> flotilla_core::store::Thread {
    flotilla_core::store::Thread {
        id: s(id),
        environment_id: Some(s("e1")),
        environment_name: Some(s("base")),
        title: s(title),
        sandbox_id: Some(format!("bx_{id}")),
        conversation_id: Some(format!("c_{id}")),
        provider: s("claude-code"),
        model: s("haiku"),
        effort: None,
        fast: false,
        state: flotilla_core::store::ThreadState::Idle,
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
