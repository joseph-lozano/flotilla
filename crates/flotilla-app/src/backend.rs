//! How the UI reaches the engine: requests and subscriptions go out as `ClientMsg`s; views,
//! answers, live text and notes come back as `ServerMsg`s on the window's channel. The UI
//! reads and writes nothing else. There's one way to do that, a client of a server
//! (`flotilla_client`, which keeps a copy of the views from the server's snapshots and deltas,
//! and resumes after a drop), whichever server it is:
//!
//! - **This Mac**: the app runs the server itself (`flotilla_server::Local`), with the engine,
//!   and reaches it through an in-memory pipe, with the same WebSocket and messages as any
//!   other. It also listens on its Unix socket for the CLI, and on TCP for other devices while
//!   they're let in. It stops, and lets go of the database, when the backend goes.
//! - **A server** this Mac is paired with (or `FLOTILLA_SERVER` in dev): over its WebSocket,
//!   with this device's token. Nothing runs here.
//!
//! Files (a prompt's attachments, packed skills) go up first, through the backend's
//! [`Uploader`]: requests carry their blob ids, never a path on this Mac.
//!
//! This is the one part of the UI that uses `flotilla_core`, for This Mac's own server: its
//! blob store, and whether its database moved to a server. (`main` finds the database there
//! too.) The rest uses flotilla-proto, flotilla-model and flotilla-client only.

use flotilla_proto::{ClientMsg, Connection, ServerMsg};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

/// How many backends are still being let go of while This Mac's server finishes its engine's
/// work under way (an action or sync can take seconds), off the main thread. It holds the
/// database until then. Counted until each has finished, so every waiter sees it.
static RETIRING: Mutex<usize> = Mutex::new(0);
static RETIRED: Condvar = Condvar::new();

/// One retiring backend, counted until this is dropped.
struct Retiring;

impl Drop for Retiring {
    fn drop(&mut self) {
        let mut n = RETIRING.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.saturating_sub(1);
        RETIRED.notify_all();
    }
}

/// Where a prompt's files and packed skills go up: this Mac's own blob store when the engine
/// runs here, else the server's `PUT /v1/blobs`. Used off the main thread.
#[derive(Clone)]
pub enum Uploader {
    Local(flotilla_core::blobs::Blobs),
    Remote(flotilla_client::Target, Option<flotilla_proto::Secret>),
    /// Nowhere: why.
    None(String),
}

impl Uploader {
    pub fn put(&self, bytes: &[u8]) -> Result<flotilla_proto::wire::Blob, String> {
        match self {
            Uploader::Local(blobs) => blobs.put(bytes).map_err(|e| e.to_string()),
            Uploader::Remote(target, token) => {
                flotilla_client::upload(target, token.as_ref(), bytes)
            }
            Uploader::None(why) => Err(why.clone()),
        }
    }
}

/// Where a thread's older pages and cut strings are read from: the server's HTTP, over the
/// same way in as its WebSocket (This Mac's pipe, a socket, or TCP with this device's token).
/// Used off the main thread.
#[derive(Clone)]
pub struct Reader(Option<(flotilla_client::Target, Option<flotilla_proto::Secret>)>);

impl Reader {
    pub fn history(
        &self,
        thread: &str,
        before: i64,
    ) -> Result<flotilla_proto::HistoryPage, String> {
        let (target, token) = self.0.as_ref().ok_or("Not connected")?;
        flotilla_client::history(target, token.as_ref(), thread, before)
    }

    pub fn full(
        &self,
        thread: &str,
        seq: i64,
        cut: &flotilla_proto::Cut,
    ) -> Result<String, String> {
        let (target, token) = self.0.as_ref().ok_or("Not connected")?;
        flotilla_client::full(target, token.as_ref(), thread, seq, cut)
    }
}

pub struct Backend {
    client: Option<flotilla_client::Client>,
    uploader: Uploader,
    reader: Reader,
    /// The paired device this Mac is to the server (none: the server's own machine).
    device: Option<String>,
    /// This Mac's own server, when it's the one used. After the client, so the client goes
    /// first; dropping it stops the engine and lets go of the database before it returns.
    local: Option<flotilla_server::Local>,
    /// This Mac's server couldn't start, and something else can be used: a server on this
    /// machine holding the database, or the server the database moved to.
    instead: Option<flotilla_server::Instead>,
}

impl Backend {
    /// No backend: what the app holds for a moment while it switches to another.
    pub fn detached() -> Backend {
        Backend {
            client: None,
            uploader: Uploader::None("Not connected".into()),
            reader: Reader(None),
            device: None,
            local: None,
            instead: None,
        }
    }

    /// This Mac's own server on `db`, reached through a pipe. With `offline` (dev), there's no
    /// engine: views come from the database as it is, and only the host's own writes happen.
    /// If another Flotilla holds the database, or it moved to another, none of it runs, and
    /// the UI is told why (and [`Backend::instead`] says what it can use instead).
    pub fn this_mac(db: &Path, offline: bool, to_ui: smol::channel::Sender<ServerMsg>) -> Backend {
        // This Mac's last server, still finishing: it lets go of the database when it's done.
        wait_retired();
        let mode = if offline {
            flotilla_engine::host::Mode::Offline
        } else {
            flotilla_engine::host::Mode::Engine
        };
        let local =
            match flotilla_server::Local::start(db.to_path_buf(), Some(socket_for(db)), mode) {
                Ok(l) => l,
                Err(refused) => {
                    let _ = to_ui.send_blocking(ServerMsg::Connection(Connection::Refused {
                        reason: refused.reason.clone(),
                    }));
                    return Backend {
                        client: None,
                        uploader: Uploader::None(refused.reason),
                        reader: Reader(None),
                        device: None,
                        local: None,
                        instead: refused.instead,
                    };
                }
            };
        let door = local.doorway();
        let target =
            flotilla_client::Target::Memory(flotilla_client::Pipe::new(move || door.open()));
        let reader = Reader(Some((target.clone(), None)));
        let client = flotilla_client::Client::connect(target, info(None), move |msg| {
            to_ui.send_blocking(msg).is_ok()
        });
        Backend {
            client: Some(client),
            uploader: Uploader::Local(flotilla_core::blobs::Blobs::beside(db)),
            reader,
            device: None,
            local: Some(local),
            instead: None,
        }
    }

    /// A server's, at `target`, with this device's token for it (over TCP).
    pub fn server(
        target: &str,
        token: Option<flotilla_proto::Secret>,
        to_ui: smol::channel::Sender<ServerMsg>,
    ) -> Backend {
        let target = match flotilla_client::Target::parse(target) {
            Ok(t) => t,
            Err(reason) => {
                let _ = to_ui.send_blocking(ServerMsg::Connection(Connection::Refused {
                    reason: reason.clone(),
                }));
                return Backend {
                    client: None,
                    uploader: Uploader::None(reason),
                    reader: Reader(None),
                    device: None,
                    local: None,
                    instead: None,
                };
            }
        };
        // Over the Unix socket this machine's user needs no token, and is the server's machine.
        let tcp = target.tcp();
        let device = token
            .as_ref()
            .filter(|_| tcp)
            .and_then(|t| flotilla_model::devices::token_device(&t.0))
            .map(str::to_string);
        let uploader = Uploader::Remote(target.clone(), token.clone().filter(|_| tcp));
        let reader = Reader(Some((target.clone(), token.clone().filter(|_| tcp))));
        let client = flotilla_client::Client::connect(target, info(token), move |msg| {
            to_ui.send_blocking(msg).is_ok()
        });
        Backend {
            client: Some(client),
            uploader,
            reader,
            device,
            local: None,
            instead: None,
        }
    }

    pub fn send(&self, msg: ClientMsg) {
        if let Some(c) = &self.client {
            c.send(msg);
        }
    }

    /// Not reaching the server: try again now.
    pub fn retry(&self) {
        if let Some(c) = &self.client {
            c.retry();
        }
    }

    /// Where files go up.
    pub fn uploader(&self) -> Uploader {
        self.uploader.clone()
    }

    /// Where older pages and cut strings are read from.
    pub fn reader(&self) -> Reader {
        self.reader.clone()
    }

    /// The paired device this Mac is to the server (none: the server's own machine, as with
    /// This Mac). What it sent is noted as from it, so only it opens the files from their paths.
    pub fn device(&self) -> Option<String> {
        self.device.clone()
    }

    /// Let go of it without waiting here: This Mac's server finishes its work under way, then
    /// lets go of the database, on a thread of its own. Starting This Mac again waits for that.
    pub fn retire(self) {
        if self.local.is_none() {
            return;
        }
        *RETIRING.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        let counted = Retiring;
        let _ = std::thread::Builder::new()
            .name("flotilla-retire".into())
            .spawn(move || {
                drop(self);
                drop(counted);
            });
    }

    /// Letting other devices in to This Mac's server (None on another server, or with none).
    pub fn access(&self) -> Option<flotilla_server::Access> {
        self.local.as_ref().map(|l| l.access())
    }

    /// What This Mac can use instead of its own server, when that couldn't start.
    pub fn instead(&self) -> Option<&flotilla_server::Instead> {
        self.instead.as_ref()
    }

    /// The server's latest welcome (which server it is, and whose databases it took in).
    pub fn welcome(&self) -> Option<flotilla_proto::wire::Welcome> {
        self.client.as_ref().and_then(|c| c.welcome())
    }
}

/// A retired backend is still finishing.
pub fn retiring() -> bool {
    *RETIRING.lock().unwrap_or_else(|e| e.into_inner()) > 0
}

/// Wait until every retired backend has let go (before This Mac starts again, or the app quits).
pub fn wait_retired() {
    let mut n = RETIRING.lock().unwrap_or_else(|e| e.into_inner());
    while *n > 0 {
        n = RETIRED.wait(n).unwrap_or_else(|e| e.into_inner());
    }
}

/// Where This Mac's server listens for the CLI: `run/server.sock` beside the database, as
/// `flotilla serve` does by default.
pub fn socket_for(db: &Path) -> PathBuf {
    db.parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("run")
        .join("server.sock")
}

/// Whether This Mac's database at `db` says it moved to a server (`flotilla_core::owner`).
#[cfg(feature = "snapshot")]
pub fn moved(db: &Path) -> bool {
    flotilla_core::owner::moved(db).is_some()
}

/// Mark This Mac's database at `db` as moved to the server that `welcome` came from, if that
/// server took it in (its id is one the server adopted) and it isn't marked so already. Reads
/// and writes the database file, so off the main thread.
pub fn note_moved(db: &Path, welcome: &flotilla_proto::wire::Welcome, url: Option<String>) {
    use flotilla_core::owner;
    let Some(id) = owner::recorded_id(db) else {
        return;
    };
    if !welcome.adopted.contains(&id)
        || owner::moved(db).is_some_and(|m| m.server == welcome.server.id)
    {
        return;
    }
    match owner::mark_moved(db, &owner::Moved::to(&welcome.server.id, url)) {
        Ok(()) => eprintln!(
            "[flotilla] {} took in this database ({id}): it runs its threads now",
            welcome.server.id
        ),
        Err(e) => eprintln!(
            "[flotilla] couldn't mark {} as moved to {}: {e}",
            db.display(),
            welcome.server.id
        ),
    }
}

/// The port This Mac's server lets other devices in on (a dev build's beside a release's).
pub fn devices_port() -> u16 {
    if flotilla_model::flavor::is_dev() {
        7778
    } else {
        7777
    }
}

fn info(token: Option<flotilla_proto::Secret>) -> flotilla_client::Info {
    flotilla_client::Info {
        app: "flotilla-desktop".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        token,
        scope: None,
    }
}

/// Forget a server this device was paired with: its token goes, and it's asked to revoke
/// this device there ([`leave`]).
pub fn forget(server: crate::device::SavedServer) {
    let token = flotilla_client::tokens::device_token(&server.id)
        .ok()
        .flatten()
        .map(flotilla_proto::Secret);
    if !server.id.is_empty() {
        let _ = flotilla_client::tokens::forget_device_token(&server.id);
    }
    leave(server, token);
}

/// Ask `server` to revoke this device's pairing there (so it stops alerting it), with
/// `token` (any device paired there may), if it answers within four seconds. On a thread of
/// its own: nothing waits for it. Over its socket this machine was never paired.
pub fn leave(server: crate::device::SavedServer, token: Option<flotilla_proto::Secret>) {
    let (Some(token), Ok(target)) = (token, flotilla_client::Target::parse(&server.url)) else {
        return;
    };
    if server.device.is_empty() || !target.tcp() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("flotilla-leave".into())
        .spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            let client =
                flotilla_client::Client::connect(target, info(Some(token)), move |msg| match msg {
                    ServerMsg::Res { id: 1, .. } => {
                        let _ = tx.send(());
                        false
                    }
                    _ => true,
                });
            client.send(ClientMsg::Req {
                id: 1,
                req: flotilla_proto::Request::RevokeDevice(server.device),
            });
            let _ = rx.recv_timeout(std::time::Duration::from_secs(4));
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_proto::{View, ViewKey};
    use std::time::{Duration, Instant};

    /// The first sidebar the backend hands on, as the window would get it.
    fn sidebar(rx: &smol::channel::Receiver<ServerMsg>) -> flotilla_proto::Sidebar {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            match rx.try_recv() {
                Ok(ServerMsg::Snap { view }) => {
                    if let View::Sidebar(s) = *view {
                        return s;
                    }
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("no sidebar came");
    }

    fn thread(db: &Path, id: &str, title: &str) {
        let store = flotilla_core::store::Store::open(db).unwrap();
        let t = flotilla_proto::Thread {
            id: id.into(),
            environment_id: None,
            environment_name: None,
            title: title.into(),
            sandbox_id: None,
            conversation_id: None,
            provider: "claude-code".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state: flotilla_proto::ThreadState::Idle,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: "2026-10-31T10:00:00.000Z".into(),
            updated_at: "2026-10-31T10:00:00.000Z".into(),
            repo_project_id: None,
            branch: None,
            base_sha: None,
        };
        store.insert_thread(&t).unwrap();
    }

    /// While one waits for a backend still finishing, everyone else still sees it (quitting,
    /// switching again), and every waiter is let go when it's done.
    #[test]
    fn a_backend_still_retiring_stays_seen_by_every_waiter() {
        *RETIRING.lock().unwrap() += 1;
        let counted = Retiring;
        let waiters: Vec<_> = (0..2).map(|_| std::thread::spawn(wait_retired)).collect();
        std::thread::sleep(Duration::from_millis(100));
        assert!(retiring(), "a waiter hid it");
        assert!(waiters.iter().all(|w| !w.is_finished()));
        drop(counted);
        for w in waiters {
            w.join().unwrap();
        }
    }

    /// This Mac to a server and back, as Settings' Use does: the lock goes with This Mac's
    /// backend, the server's views are its own, and the old backend's never reach the new
    /// window; back on This Mac, the lock is taken again.
    #[test]
    fn switching_moves_the_lock_and_shows_only_the_new_servers_views() {
        let dir = std::env::temp_dir().join(format!("flotilla-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (mac, box_) = (dir.join("mac/flotilla.db"), dir.join("box/flotilla.db"));
        std::fs::create_dir_all(mac.parent().unwrap()).unwrap();
        std::fs::create_dir_all(box_.parent().unwrap()).unwrap();
        thread(&mac, "t-mac", "On This Mac");
        thread(&box_, "t-box", "On the box");
        // The box: another server, reached over its socket (as `flotilla serve` would be).
        let server = flotilla_server::Local::start(
            box_.clone(),
            Some(socket_for(&box_)),
            flotilla_engine::host::Mode::Offline,
        )
        .unwrap();

        let (tx, first) = smol::channel::unbounded();
        let backend = Backend::this_mac(&mac, true, tx);
        assert_eq!(
            flotilla_core::lock::holder(&mac).map(|h| h.kind).as_deref(),
            Some("app")
        );
        backend.send(ClientMsg::Sub {
            view: ViewKey::Sidebar,
        });
        assert_eq!(sidebar(&first).threads[0].thread.title, "On This Mac");
        assert!(matches!(backend.uploader(), Uploader::Local(_)) && backend.access().is_some());

        // To the box, as `switch` does: This Mac's server finishes off this thread, and its lock
        // goes with it before This Mac can start again.
        backend.retire();
        wait_retired();
        assert_eq!(flotilla_core::lock::holder(&mac), None);
        let (tx, second) = smol::channel::unbounded();
        let backend = Backend::server(&format!("unix:{}", socket_for(&box_).display()), None, tx);
        backend.send(ClientMsg::Sub {
            view: ViewKey::Sidebar,
        });
        assert_eq!(
            sidebar(&second)
                .threads
                .iter()
                .map(|r| r.thread.title.as_str())
                .collect::<Vec<_>>(),
            ["On the box"]
        );
        assert!(backend.access().is_none());
        // Nothing more comes from This Mac's: its channel is closed with it.
        std::thread::sleep(Duration::from_millis(200));
        while let Ok(m) = first.try_recv() {
            assert!(
                !matches!(m, ServerMsg::Snap { .. }),
                "a view after the switch: {m:?}"
            );
        }
        assert!(first.is_closed() || first.sender_count() == 0);

        // Back to This Mac: the lock is taken again.
        backend.retire();
        let (tx, third) = smol::channel::unbounded();
        let backend = Backend::this_mac(&mac, true, tx);
        assert_eq!(
            flotilla_core::lock::holder(&mac).map(|h| h.kind).as_deref(),
            Some("app")
        );
        backend.send(ClientMsg::Sub {
            view: ViewKey::Sidebar,
        });
        assert_eq!(sidebar(&third).threads[0].thread.title, "On This Mac");
        drop(backend);
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
