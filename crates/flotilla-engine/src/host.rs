//! The engine's side of the client seam: it runs the engine, answers clients' requests, and
//! keeps the views they subscribe to up to date. Everything the UI used to read or write in
//! SQLite goes through here, so a client needs no database. The app runs it in its own
//! process with one client; `flotilla serve` puts a WebSocket in front of it, one client per
//! connection.
//!
//! It runs on its own thread and takes what arrives in batches: clients' requests and
//! subscriptions, and the engine's events. After each batch it recomputes the views the batch
//! changed (going by the engine's scoped `Changes`), once for every client, then sends each
//! client its views (deltas from the revision it holds, or snapshots), then its answers,
//! notes and live text, so an answer always finds the views it changed already there.
//!
//! - **Views** are kept with a revision (one counter for the whole run, so a revision is
//!   never reused) and the last deltas, so a client that reconnects holding an older one
//!   gets only what it missed, or the view whole if that's no longer kept.
//! - **Sessions.** A client names its session in its hello. Its answers, progress and notes are
//!   numbered and the recent ones kept (a couple of minutes' worth): one that reconnects says
//!   the last it got, and is sent what came after.
//! - **Answers** go only to the client that asked; notes go to every client; live text to
//!   the clients with that thread open. The engine's selected threads are the union of the
//!   threads open anywhere.
//! - **Client-made ids** (`thread.new`'s thread id, `thread.send`'s `local-` id) are kept in
//!   the database, so a request sent again is a no-op even after a restart. Sent again while
//!   the first is still running, its answer goes to the one that asked last.
//! - **Setup** (`flotilla_core::doctor`): the checks run as the engine starts (and are
//!   logged), every few hours, and when a client asks; Settings shows them. Only this machine
//!   (the app's own host, or the server's Unix socket) may set the boat key: a paired device
//!   is told to run `flotilla setup` on the server.
//! - **Uploads** (`flotilla_core::blobs`): a prompt's files and skills come as blob ids, which
//!   must be uploaded already; the host notes which device sent each, and sweeps what's no
//!   longer needed every few minutes.
//! - **Devices.** A connection is this machine's (trusted: the app's own host, or a server's
//!   Unix socket) or a paired device's, whose token the server checked through
//!   [`Host::authenticate`] (the host owns the database, so a rebuild can't leave a check
//!   reading the old file). A session belongs to the device that started it: another can't
//!   resume it. Revoking a device (here, or by `flotilla devices revoke` writing the database,
//!   which the host notices within half a second) closes its connections and forgets its
//!   sessions.

use crate::views::{self, Live};
use crate::{Action, Changes, Cmd, Engine, Event, Events, Outcome};
use flotilla_core::devices::{AuthError, Device, PairError, Paired};
use flotilla_core::doctor::{Doctor, Probe};
use flotilla_core::lock::Lock;
use flotilla_core::store::{Activity, Store};
use flotilla_proto::wire::{CAPABILITIES, Down, Hello, Op, PairRequest, ServerInfo, Up, Welcome};
use flotilla_proto::{
    Catalog, DeviceRow, Devices, EngineState, GhStatus, HistoryPage, Note, PROTOCOL, PairingCode,
    RawEvent, Reply, ReqId, Request, Scope, Setting, Sidebar, SyncStatus, Usage, View, ViewKey,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Where the host sends one client's messages: its socket, or the app's channel in-process.
/// Returns false once the client is gone (or can't keep up), and the host lets it go.
#[derive(Clone)]
pub struct Outbox(Arc<dyn Fn(Down) -> bool + Send + Sync>);

impl Outbox {
    pub fn new(send: impl Fn(Down) -> bool + Send + Sync + 'static) -> Outbox {
        Outbox(Arc::new(send))
    }
}

/// What runs behind the host.
#[derive(Clone)]
pub enum Mode {
    /// The engine, with the saved boat key.
    Engine,
    /// Dev: no engine, the database as it is.
    Offline,
    /// `flotilla serve --demo` ([`crate::demo`]): scripted turns in place of the engine, and
    /// notifications through the relay. The database was reset to the demo's as it started.
    Demo,
    /// Tests: what would go to the engine goes here instead, and the engine's events come in
    /// through [`Host::events`].
    Fake(Arc<dyn Fn(Cmd) + Send + Sync>),
}

/// What the host runs for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// `flotilla serve`: its setup is `flotilla setup`'s, and it checks Tailscale.
    Server,
    /// The app on This Mac, running its own: the setup fixes are the app's (the key goes in
    /// Settings), and Tailscale is checked only while it forwards to the app.
    App,
}

/// Who's on the other end of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    /// This machine, trusted: the app's own host, or a server's Unix socket.
    Local,
    /// A paired device (by id), whose token was checked as it connected.
    Device(String),
    /// A paired device that may do only what an agent may ([`Scope::Agent`]: `flotilla mcp`).
    Agent(String),
}

impl Peer {
    fn device(&self) -> Option<&str> {
        match self {
            Peer::Local => None,
            Peer::Device(d) | Peer::Agent(d) => Some(d),
        }
    }

    /// What it may do, as paired.
    pub fn scope(&self) -> Scope {
        match self {
            Peer::Agent(_) => Scope::Agent,
            Peer::Local | Peer::Device(_) => Scope::Full,
        }
    }
}

type ConnId = u64;
type Then<T> = Box<dyn FnOnce(T) + Send>;

/// Why a read over HTTP (a history page, a whole string) got nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The device was removed since its token was checked.
    Unpaired,
    /// No such thread, page or string (a string that changed since it was cut, included).
    Missing(String),
    /// The database is being rebuilt, or the server is stopping.
    Busy(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Unpaired => f.write_str(REVOKED),
            ReadError::Missing(why) | ReadError::Busy(why) => f.write_str(why),
        }
    }
}

/// What a revoked device's connections are told as they close.
pub const REVOKED: &str = "This device was removed from the server: pair it again to connect";

/// What an offline host (dev: no engine) answers whatever needs the engine.
pub const OFFLINE: &str = "Not available offline";
/// The kv entries a server keeps: the URL it gives out in pairing links.
pub const SERVER_URL_KEY: &str = "server_url";
/// The account's usage as last read from boat (JSON): shown until the next read, and offline.
pub const USAGE_KEY: &str = "usage";

enum In {
    Connect(ConnId, Outbox, Peer),
    /// A device's token, and where it's connecting from: which device it is.
    Auth(String, Option<String>, Then<Result<Device, AuthError>>),
    /// A pairing code to spend on a new device, from `ip`.
    /// With a note to keep of the new device (`Store::pair_device_noting`).
    Pair(
        PairRequest,
        Option<String>,
        Option<&'static str>,
        Then<Result<Paired, PairError>>,
    ),
    /// A page of a thread's older messages, for `peer`.
    History(Peer, String, i64, Then<Result<HistoryPage, ReadError>>),
    /// A cut string whole (thread, seq, field, mark), for `peer`.
    Full(
        Peer,
        String,
        i64,
        String,
        String,
        Then<Result<String, ReadError>>,
    ),
    From(ConnId, Up),
    Gone(ConnId),
    Engine(Event),
    /// A request answered off the host's thread (a suggested name), by its token.
    Answer(u64, Result<Reply, String>),
    /// The setup checks begun as this run are done.
    Doctor(u64, Doctor),
    /// The server listens for devices on this TCP port (None: no longer).
    Serving(Option<u16>),
    /// Remote MCP listens on this port (None: it doesn't).
    ServingMcp(Option<u16>),
    /// APNs says this device's push token (the second) is gone.
    PushGone(String, String),
    Shutdown,
}

/// A running host. Dropping it stops the host and its engine, and lets go of the lock.
pub struct Host {
    tx: mpsc::Sender<In>,
    next: Arc<AtomicU64>,
    info: ServerInfo,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Host {
    /// Start a server's host on `db` (whose `lock` the caller took).
    pub fn start(db: PathBuf, mode: Mode, lock: Lock) -> Host {
        Host::start_as(db, mode, lock, Role::Server)
    }

    /// Start a host on `db` for `role`.
    pub fn start_as(db: PathBuf, mode: Mode, lock: Lock, role: Role) -> Host {
        let (tx, rx) = mpsc::channel();
        let info = ServerInfo {
            id: server_id(&db),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let mut state = State::new(db, mode, tx.clone(), info.clone(), Some(lock));
        state.role = role;
        let thread = std::thread::Builder::new()
            .name("flotilla-host".into())
            .spawn(move || state.run(rx))
            .ok();
        Host {
            tx,
            next: Arc::new(AtomicU64::new(1)),
            info,
            thread,
        }
    }

    /// A new client on this machine: what the host has for it goes to `out`. Its first
    /// message must be a hello.
    pub fn connect(&self, out: Outbox) -> Conn {
        self.connect_as(out, Peer::Local)
    }

    /// A new client, `peer`. A device revoked since its token was checked is closed at once.
    pub fn connect_as(&self, out: Outbox, peer: Peer) -> Conn {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(In::Connect(id, out, peer));
        Conn {
            link: Link {
                id,
                tx: self.tx.clone(),
            },
        }
    }

    /// Which device `token` is (seen now, from `ip`), or why it isn't one: `done` is told.
    pub fn authenticate(
        &self,
        token: String,
        ip: Option<String>,
        done: impl FnOnce(Result<Device, AuthError>) + Send + 'static,
    ) {
        if let Err(mpsc::SendError(In::Auth(_, _, done))) =
            self.tx.send(In::Auth(token, ip, Box::new(done)))
        {
            done(Err(AuthError::Db("the server is stopping".into())));
        }
    }

    /// Spend a pairing code on a new device: its token, or why not.
    pub fn pair(
        &self,
        req: PairRequest,
        ip: Option<String>,
        done: impl FnOnce(Result<Paired, PairError>) + Send + 'static,
    ) {
        self.pair_noting(req, ip, None, done);
    }

    /// `pair`, noting the new device under `note` in the same write (an OAuth sign-in's, till
    /// its client has the token).
    pub fn pair_noting(
        &self,
        req: PairRequest,
        ip: Option<String>,
        note: Option<&'static str>,
        done: impl FnOnce(Result<Paired, PairError>) + Send + 'static,
    ) {
        if let Err(mpsc::SendError(In::Pair(_, _, _, done))) =
            self.tx.send(In::Pair(req, ip, note, Box::new(done)))
        {
            done(Err(PairError::Db("the server is stopping".into())));
        }
    }

    /// A page of a thread's older messages (`flotilla_proto::history`), for `peer`: a device
    /// is checked again here, as a connection is, so one revoked since its token was checked
    /// reads nothing.
    pub fn history(
        &self,
        peer: Peer,
        thread: String,
        page: i64,
        done: impl FnOnce(Result<HistoryPage, ReadError>) + Send + 'static,
    ) {
        if let Err(mpsc::SendError(In::History(_, _, _, done))) =
            self.tx
                .send(In::History(peer, thread, page, Box::new(done)))
        {
            done(Err(ReadError::Busy("the server is stopping".into())));
        }
    }

    /// A string a message carries cut short, whole: the message at `seq`'s `field` if it's
    /// still the one with `mark`, else wherever that string is in the thread now.
    pub fn full(
        &self,
        peer: Peer,
        thread: String,
        seq: i64,
        field: String,
        mark: String,
        done: impl FnOnce(Result<String, ReadError>) + Send + 'static,
    ) {
        if let Err(mpsc::SendError(In::Full(_, _, _, _, _, done))) =
            self.tx
                .send(In::Full(peer, thread, seq, field, mark, Box::new(done)))
        {
            done(Err(ReadError::Busy("the server is stopping".into())));
        }
    }

    /// Where engine events go in. Only `Mode::Fake` needs it: the engine has its own.
    pub fn events(&self) -> Events {
        let tx = self.tx.clone();
        Events::new(move |e| tx.send(In::Engine(e)).is_ok())
    }

    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    /// It serves devices on TCP `port` (None: no longer): the setup checks look at Tailscale,
    /// which must forward to it when it's set up to. Checks again.
    pub fn serving(&self, port: Option<u16>) {
        let _ = self.tx.send(In::Serving(port));
    }

    /// Remote MCP listens on `port` (None: it doesn't): the setup checks look at whether it's
    /// published. Checks again.
    pub fn serving_mcp(&self, port: Option<u16>) {
        let _ = self.tx.send(In::ServingMcp(port));
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.tx.send(In::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One client's connection. Dropping it tells the host the client is gone.
pub struct Conn {
    link: Link,
}

impl Conn {
    pub fn send(&self, up: Up) {
        self.link.send(up);
    }

    /// A handle that sends for this connection without owning it.
    pub fn link(&self) -> Link {
        self.link.clone()
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let _ = self.link.tx.send(In::Gone(self.link.id));
    }
}

#[derive(Clone)]
pub struct Link {
    id: ConnId,
    tx: mpsc::Sender<In>,
}

impl Link {
    pub fn send(&self, up: Up) {
        let _ = self.tx.send(In::From(self.id, up));
    }
}

/// The server's id, kept with the database so it's the same across restarts. Its VMs carry
/// it too (`flotilla_core::owner`).
fn server_id(db: &std::path::Path) -> String {
    Store::open(db)
        .and_then(|store| flotilla_core::owner::server_id(&store))
        .unwrap_or_else(|_| "srv_unknown".into())
}

/// How long a session's answers, progress and notes are kept for it to reconnect and get, and
/// the most kept; a dropped session goes after as long. Longer than a phone usually spends in
/// the background, so an Archive or a Merge it asked for before it went is answered when it's
/// back, not said to be lost.
const AWAY_FOR: Duration = Duration::from_secs(30 * 60);
const AWAY_MAX: usize = 2000;
/// The deltas kept per view for a client that reconnects, and their size at most.
const RING: usize = 64;
const RING_BYTES: usize = 1 << 20;
/// Views no client subscribes to now, kept for one that comes back.
const KEEP_IDLE: usize = 32;
/// How long client-made ids are kept.
const RECEIPTS_DAYS: i64 = 30;
/// How often the setup checks run again on their own, and uploads no longer needed are swept.
const DOCTOR_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const SWEEP_EVERY: Duration = Duration::from_secs(10 * 60);

/// Who asked: a session (in the life it had then) and its request id.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Who {
    session: String,
    life: u64,
    id: ReqId,
}

/// A request out with the engine (or a worker): who to answer, and its client-made id.
struct Ask {
    who: Who,
    client_made: Option<String>,
}

/// Who a queued message is for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum To {
    /// A session, if it's still in this life: one that started afresh since has told its
    /// client that what was out is lost, and mustn't be answered twice.
    Session(String, u64),
    All,
    /// The clients with this thread open (live text: not kept for a dropped session).
    Watching(String),
}

/// What a session holds of a view it subscribes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Has {
    Rev(u64),
    /// Nothing usable: send it whole.
    Nothing,
}

struct Session {
    /// Which life of this session id it is (a new one each time it starts afresh).
    life: u64,
    /// The device it belongs to (none: this machine's). Only it may resume the session.
    device: Option<String>,
    /// What it may do: its device's scope, or less if its hello asked (set at each hello). An
    /// agent's requests and views are held to [`Request::agent_refusal`] and
    /// [`ViewKey::agent_may`], and the threads it reads aren't on screen: not marked read, not
    /// polled for it, no streaming text.
    scope: Scope,
    conn: Option<ConnId>,
    /// The last request id it sent.
    seen: Option<ReqId>,
    subs: BTreeMap<ViewKey, Has>,
    /// Its answers, progress and notes as numbered (`n`), the recent ones, and the last number.
    log: VecDeque<(u64, Instant, Down)>,
    last: u64,
    /// Taken from the log for a reconnect, to send after its views.
    replay: Vec<Down>,
    /// Since when it's been away.
    gone_at: Option<Instant>,
}

impl Session {
    fn new(conn: ConnId, life: u64, device: Option<String>) -> Session {
        Session {
            life,
            device,
            scope: Scope::Full,
            conn: Some(conn),
            seen: None,
            subs: BTreeMap::new(),
            log: VecDeque::new(),
            last: 0,
            replay: vec![],
            gone_at: None,
        }
    }

    /// Number an answer, progress or note for it and keep it; anything else as it is.
    fn number(&mut self, msg: &Down) -> Down {
        let mut msg = msg.clone();
        let n = match &mut msg {
            Down::Res { n, .. } | Down::Note { n, .. } | Down::Progress { n, .. } => n,
            _ => return msg,
        };
        self.last += 1;
        *n = Some(self.last);
        self.log.push_back((self.last, Instant::now(), msg.clone()));
        while self.log.len() > AWAY_MAX
            || self
                .log
                .front()
                .is_some_and(|(_, at, _)| at.elapsed() > AWAY_FOR)
        {
            self.log.pop_front();
        }
        msg
    }

    /// What it hasn't got after `got`, if all of that is still kept. (More than was ever sent
    /// isn't this session's count.)
    fn after(&self, got: Option<u64>) -> Option<Vec<Down>> {
        let from = got.unwrap_or(0) + 1;
        if from > self.last + 1 {
            return None;
        }
        if from > self.last {
            return Some(vec![]);
        }
        let kept = self.log.front().is_some_and(|(n, _, _)| *n <= from);
        kept.then(|| {
            self.log
                .iter()
                .filter(|(n, _, _)| *n >= from)
                .map(|(_, _, m)| m.clone())
                .collect()
        })
    }
}

/// A view as last computed, its revision, and the deltas that led to it.
struct Kept {
    rev: u64,
    data: Value,
    ring: VecDeque<Step>,
    ring_bytes: usize,
    /// The flush it was last computed or sent in.
    used: u64,
}

struct Step {
    base: u64,
    rev: u64,
    ops: Vec<Op>,
    bytes: usize,
}

enum Behind {
    Engine(Box<Engine>),
    Fake(Arc<dyn Fn(Cmd) + Send + Sync>),
}

struct State {
    db: PathBuf,
    mode: Mode,
    tx: mpsc::Sender<In>,
    info: ServerInfo,
    /// This run, made at random: revisions from another run mean nothing.
    epoch: String,
    _lock: Option<Lock>,
    store: Option<Store>,
    engine: Option<Behind>,
    /// Why there's no engine, when there should be one.
    engine_error: Option<String>,
    /// The saved boat key's last four characters.
    boat_key: Option<String>,
    status: SyncStatus,
    github: GhStatus,
    usage: Option<Usage>,
    /// Who asked for the usage to be read again, answered when the next read is in.
    usage_waiters: Vec<Who>,
    /// Who asked for the agents' usage to be checked, answered when the check ends.
    agent_usage_waiters: Vec<Who>,
    catalog: Option<Catalog>,
    conns: HashMap<ConnId, Connected>,
    sessions: HashMap<String, Session>,
    /// Threads marked unread: not read again by being on screen until opened again (on any
    /// client) or marked read.
    keep_unread: HashSet<String>,
    /// The threads open anywhere, as the engine was last told.
    open: Vec<String>,
    asks: HashMap<u64, Ask>,
    next_token: u64,
    /// Client-made ids whose request is still running, and its token.
    inflight: HashMap<String, u64>,
    /// What changed since the last flush, views to recompute whatever changed, and views
    /// already computed (a VM's skills and files, which come with their event).
    changes: Changes,
    dirty: BTreeSet<ViewKey>,
    ready: Vec<View>,
    queue: Vec<(To, Down)>,
    /// The sidebar as last computed (its rows and families feed thread views).
    sidebar: Option<Sidebar>,
    /// Each subscribed thread's project, from its last view.
    thread_projects: HashMap<String, Option<String>>,
    views: HashMap<ViewKey, Kept>,
    rev: u64,
    flushes: u64,
    /// Each open thread's streaming text as last sent, for a client that opens it meanwhile.
    live: HashMap<String, Vec<RawEvent>>,
    /// Why the skills couldn't go to a VM, as the settings last said. Set while a thread's VM
    /// is provisioned, with no notice of its own: looked at whenever a thread changed.
    skills_error: Option<String>,
    /// The database is being rebuilt (the store is closed meanwhile).
    rebuilding: bool,
    /// The paired devices (id, last seen) as last read, to notice changes made elsewhere, and
    /// when they were last read and connected ones last marked seen.
    device_marks: Vec<(String, i64)>,
    watched: Instant,
    touched: Instant,
    /// The setup checks as last run, whether they're running now, who asked for them, and
    /// when they last started; what they ask the world.
    doctor: Option<Doctor>,
    doctoring: bool,
    /// The checks begun last: a result from an earlier run is dropped.
    doctor_run: u64,
    /// The database for what runs off this thread (the checks, a suggested name): closed as
    /// the host goes, before the lock.
    gate: flotilla_core::lock::Gate,
    doctor_waiters: Vec<Who>,
    doctored: Option<Instant>,
    probe: Arc<dyn Probe + Send + Sync>,
    /// The TCP port it serves devices on, if it does.
    serving: Option<u16>,
    /// The port remote MCP listens on, if it does.
    serving_mcp: Option<u16>,
    role: Role,
    swept: Option<Instant>,
    /// Notifications to phones, when they're set up (`flotilla_core::push`).
    push: Option<flotilla_core::push::Pusher>,
    /// Alerts sent this run: with the epoch, each one's id.
    alerts: u64,
}

impl Drop for State {
    /// Everything that uses the database stops before the lock (a field, dropped after this)
    /// is let go, so another engine can't start beside its last writes, however the host ends:
    /// what runs off this thread (the checks, a suggested name) can't use it any more, the
    /// engine's work under way finishes, and the host's own connection closes.
    fn drop(&mut self) {
        self.gate.close();
        self.stop_engine();
        self.store = None;
    }
}

/// An open connection: where its messages go, its session once it's said hello, and who it is.
struct Connected {
    out: Outbox,
    session: Option<String>,
    peer: Peer,
}

/// How often the host looks for devices paired or revoked elsewhere (`flotilla pair`,
/// `flotilla devices revoke`), and marks connected ones seen.
const WATCH_EVERY: Duration = Duration::from_millis(500);
const TOUCH_EVERY: Duration = Duration::from_secs(10 * 60);

impl State {
    fn new(
        db: PathBuf,
        mode: Mode,
        tx: mpsc::Sender<In>,
        info: ServerInfo,
        lock: Option<Lock>,
    ) -> State {
        State {
            db,
            mode,
            tx,
            info,
            epoch: uuid::Uuid::new_v4().simple().to_string(),
            _lock: lock,
            store: None,
            engine: None,
            engine_error: None,
            boat_key: None,
            status: SyncStatus::default(),
            github: GhStatus::Unknown,
            usage: None,
            usage_waiters: Vec::new(),
            agent_usage_waiters: Vec::new(),
            catalog: None,
            conns: HashMap::new(),
            sessions: HashMap::new(),
            keep_unread: HashSet::new(),
            open: Vec::new(),
            asks: HashMap::new(),
            next_token: 1,
            inflight: HashMap::new(),
            changes: Changes::default(),
            dirty: BTreeSet::new(),
            ready: Vec::new(),
            queue: Vec::new(),
            sidebar: None,
            thread_projects: HashMap::new(),
            views: HashMap::new(),
            rev: 0,
            flushes: 0,
            live: HashMap::new(),
            skills_error: None,
            rebuilding: false,
            device_marks: Vec::new(),
            watched: Instant::now(),
            touched: Instant::now(),
            doctor: None,
            doctoring: false,
            doctor_run: 0,
            gate: flotilla_core::lock::Gate::default(),
            doctor_waiters: Vec::new(),
            doctored: None,
            probe: Arc::new(flotilla_core::doctor::System),
            serving: None,
            serving_mcp: None,
            role: Role::Server,
            swept: None,
            push: None,
            alerts: 0,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<In>) {
        self.store = Store::open(&self.db).ok();
        if let Some(store) = &self.store {
            let before = (chrono::Utc::now() - chrono::Duration::days(RECEIPTS_DAYS))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let _ = store.prune_receipts(&before);
        }
        self.catalog = self.store.as_ref().and_then(views::catalog);
        self.usage = self.store.as_ref().and_then(last_usage);
        match self.mode.clone() {
            Mode::Engine => {
                self.start_engine();
                self.run_doctor();
                self.start_push();
            }
            // Only what needs no network: the database.
            Mode::Offline => self.run_doctor(),
            Mode::Demo => {
                let tx = self.tx.clone();
                let events = Events::new(move |e| tx.send(In::Engine(e)).is_ok());
                let demo = crate::demo::start(self.db.clone(), events, crate::demo::Pace::SHOWN);
                self.engine = Some(Behind::Fake(demo));
                self.run_doctor();
                self.start_push();
            }
            Mode::Fake(f) => self.engine = Some(Behind::Fake(f)),
        }
        self.device_marks = self
            .store
            .as_ref()
            .and_then(|s| s.device_marks().ok())
            .unwrap_or_default();
        while self.turn(&rx) {}
        // No one reads what the engine says now. Dropping `self` stops it, and lets go of the
        // database, before the lock (see `Drop for State`).
        drop(rx);
    }

    /// Send notifications to the phones that register: with this server's key, or through its
    /// relay, unless push is off here.
    fn start_push(&mut self) {
        let mode = flotilla_core::push::mode(self.store.as_ref());
        self.push_as(mode);
    }

    fn push_as(&mut self, mode: Result<Option<flotilla_core::push::Mode>, String>) {
        match mode {
            Ok(None) => eprintln!("[push] notifications are off"),
            Ok(Some(mode)) => {
                let how = match &mode {
                    flotilla_core::push::Mode::Direct(c) => {
                        format!("key {}, team {}", c.key_id, c.team_id)
                    }
                    flotilla_core::push::Mode::Relay(url) => format!("through the relay at {url}"),
                };
                let tx = self.tx.clone();
                let registered = flotilla_core::push::Registrations::of(
                    &self
                        .store
                        .as_ref()
                        .map(flotilla_core::push::targets)
                        .unwrap_or_default(),
                );
                match flotilla_core::push::Pusher::start(mode, registered, move |device, id| {
                    drop(tx.send(In::PushGone(device, id)))
                }) {
                    Ok(p) => {
                        eprintln!("[push] notifications on ({how})");
                        self.push = Some(p);
                    }
                    Err(e) => eprintln!("[push] {e}"),
                }
            }
            Err(e) => eprintln!("[push] notifications are off: {e}"),
        }
    }

    /// The relay this server sends through, for `push.register`'s answer: its sender's; with
    /// no engine (dev, `--offline`), the one it's set up for, though it sends nothing itself.
    fn push_relay(&self) -> Option<String> {
        match (&self.push, &self.mode) {
            (Some(p), _) => p.relay().map(str::to_string),
            (None, Mode::Offline) => match flotilla_core::push::mode(self.store.as_ref()) {
                Ok(Some(flotilla_core::push::Mode::Relay(url))) => Some(url),
                _ => None,
            },
            _ => None,
        }
    }

    /// What the sending thread checks a queued notification's devices against: each one's
    /// registration (`flotilla_core::push::Target::id`).
    fn registered(&self, device: &str, id: Option<&str>) {
        if let Some(p) = &self.push {
            p.registrations.set(device, id);
        }
    }

    /// If `note` is one that notifies (`flotilla_core::push::alert`, the one rule): every
    /// client is told, after the note itself, and the phones that registered are pushed it.
    fn alert_for(&mut self, note: &Note) {
        let Some(alert) = self
            .store
            .as_ref()
            .and_then(|store| flotilla_core::push::alert(note, store))
        else {
            return;
        };
        if let (Some(push), Some(store)) = (&self.push, &self.store) {
            let targets: Vec<_> = flotilla_core::push::targets(store)
                .into_iter()
                .filter(|t| push.reaches(t))
                .collect();
            if !targets.is_empty() {
                push.push(flotilla_core::push::Job::new(
                    &alert,
                    &self.info.id,
                    targets,
                ));
            }
        }
        self.alerts += 1;
        let id = format!("{}-{}", self.epoch, self.alerts);
        self.queue.push((
            To::All,
            Down::Note {
                note: Note::Alert { id, alert },
                n: None,
            },
        ));
    }

    /// Stop the engine, waiting for all its work under way (see [`Engine::stop`]).
    fn stop_engine(&mut self) {
        if let Some(Behind::Engine(engine)) = self.engine.take() {
            engine.stop();
        }
    }

    /// Wait for input (until the watcher is next due), take what's there, look for devices
    /// changed elsewhere when that's due, then flush if anything happened. False to stop.
    fn turn(&mut self, rx: &mpsc::Receiver<In>) -> bool {
        let first = match rx.recv_timeout(WATCH_EVERY.saturating_sub(self.watched.elapsed())) {
            Ok(first) => Some(first),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        };
        let mut busy = first.is_some();
        if let Some(first) = first {
            if !self.take(first) {
                return false;
            }
            // A client that never lets the queue empty can't hold off the watcher (and a
            // revoke written by the CLI with it).
            while self.watched.elapsed() < WATCH_EVERY
                && let Ok(more) = rx.try_recv()
            {
                if !self.take(more) {
                    return false;
                }
            }
        }
        if self.watched.elapsed() >= WATCH_EVERY {
            busy |= self.watch_devices();
            self.chores();
        }
        if busy {
            self.flush();
        }
        true
    }

    /// What's due now and then: the setup checks again, and a sweep of uploads no longer
    /// needed.
    fn chores(&mut self) {
        if matches!(self.mode, Mode::Engine)
            && !self.doctoring
            && self.doctored.is_some_and(|t| t.elapsed() >= DOCTOR_EVERY)
        {
            self.run_doctor();
        }
        if self.swept.is_none_or(|t| t.elapsed() >= SWEEP_EVERY)
            && let Some(store) = self.store.as_ref()
        {
            self.swept = Some(Instant::now());
            match flotilla_core::blobs::Blobs::beside(&self.db)
                .sweep(store, std::time::SystemTime::now())
            {
                Ok(0) => {}
                Ok(n) => eprintln!(
                    "[host] swept {n} uploaded file{} no longer needed",
                    if n == 1 { "" } else { "s" }
                ),
                Err(e) => eprintln!("[host] couldn't sweep uploaded files: {e}"),
            }
        }
    }

    /// Run the setup checks, off this thread (they ask boat, GitHub and claude), unless
    /// they're running already.
    fn run_doctor(&mut self) {
        if self.doctoring {
            return;
        }
        self.doctoring = true;
        self.doctored = Some(Instant::now());
        self.doctor_run += 1;
        let at = self.doctor_at();
        let offline = matches!(self.mode, Mode::Offline | Mode::Demo);
        let (probe, tx, run) = (self.probe.clone(), self.tx.clone(), self.doctor_run);
        let spawned = std::thread::Builder::new()
            .name("flotilla-doctor".into())
            .spawn(move || {
                let doctor = if offline {
                    flotilla_core::doctor::offline(&at)
                } else {
                    flotilla_core::doctor::run(probe.as_ref(), &at)
                };
                let _ = tx.send(In::Doctor(run, doctor));
            });
        if spawned.is_err() {
            self.doctoring = false;
        }
    }

    /// Where the checks look. A server checks Tailscale while it serves; the app only while it
    /// forwards through Tailscale to the devices it lets in, and its fixes stay its own.
    fn doctor_at(&self) -> flotilla_core::doctor::At {
        let tailscale = match self.role {
            Role::Server => self.serving.is_some(),
            Role::App => {
                let on = self
                    .store
                    .as_ref()
                    .and_then(|s| s.kv_get(flotilla_core::tailscale::KEY).ok().flatten());
                self.serving.is_some() && on.as_deref() == Some("on")
            }
        };
        flotilla_core::doctor::At {
            db: self.db.clone(),
            tailscale,
            serving: self.serving,
            mcp: self.serving_mcp,
            app: self.role == Role::App,
            gate: self.gate.clone(),
        }
    }

    /// Run the checks afresh, even if some are under way: what they find is dropped.
    fn rerun_doctor(&mut self) {
        self.doctoring = false;
        self.run_doctor();
    }

    fn doctored(&mut self, run: u64, doctor: Doctor) {
        if run != self.doctor_run {
            return;
        }
        self.doctoring = false;
        for line in flotilla_core::doctor::lines(&doctor) {
            eprintln!("[doctor] {line}");
        }
        self.doctor = Some(doctor);
        self.dirty.insert(ViewKey::Settings);
        for who in std::mem::take(&mut self.doctor_waiters) {
            self.answer(&who, Ok(Reply::Done));
        }
    }

    /// The paired device a request came from (none: this machine).
    fn device_of(&self, who: &Who) -> Option<String> {
        self.sessions
            .get(&who.session)
            .and_then(|s| s.device.clone())
    }

    /// The device's name as paired, or this machine's.
    fn device_name(&self, device: Option<&str>) -> String {
        match device {
            None => flotilla_core::util::machine_name(),
            Some(id) => self
                .store
                .as_ref()
                .and_then(|s| s.devices(flotilla_core::util::now_ms()).ok())
                .and_then(|d| d.into_iter().find(|d| d.id == id).map(|d| d.name))
                .unwrap_or_else(|| "another device".into()),
        }
    }

    /// A prompt's files: each uploaded already, and noted as from the device that sent it.
    fn uploaded(&self, who: &Who, files: &mut [flotilla_proto::Attached]) -> Result<(), String> {
        flotilla_core::attach::check_uploaded(
            files,
            &flotilla_core::blobs::Blobs::beside(&self.db),
        )?;
        let from = self.device_of(who);
        for f in files {
            f.from = from.clone();
        }
        Ok(())
    }

    /// Devices paired or revoked elsewhere: the view again, and a revoked one's connections
    /// closed. True if anything changed.
    fn watch_devices(&mut self) -> bool {
        self.watched = Instant::now();
        let Some(store) = self.store.as_ref() else {
            return false;
        };
        if self.touched.elapsed() >= TOUCH_EVERY {
            self.touched = Instant::now();
            let now = flotilla_core::util::now_ms();
            for d in self.connected_devices() {
                let _ = store.touch_device(&d, now);
            }
        }
        let Ok(marks) = store.device_marks() else {
            return false;
        };
        if marks == self.device_marks {
            return false;
        }
        let paired: HashSet<&str> = marks.iter().map(|(id, _)| id.as_str()).collect();
        let gone: BTreeSet<String> = self
            .connected_devices()
            .into_iter()
            .filter(|d| !paired.contains(d.as_str()))
            .collect();
        self.device_marks = marks;
        self.dirty.insert(ViewKey::Devices);
        for d in gone {
            self.revoked(&d);
        }
        true
    }

    /// The devices with a connection open.
    fn connected_devices(&self) -> BTreeSet<String> {
        self.conns
            .values()
            .filter_map(|c| c.peer.device().map(str::to_string))
            .collect()
    }

    /// A device is no longer paired: close its connections and forget its sessions.
    fn revoked(&mut self, device: &str) {
        let conns: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.peer.device() == Some(device))
            .map(|(id, _)| *id)
            .collect();
        for conn in conns {
            self.kick(conn, REVOKED);
        }
        self.sessions
            .retain(|_, s| s.device.as_deref() != Some(device));
        // No more notifications for it, queued ones included.
        if let Some(store) = &self.store {
            let _ = flotilla_core::push::unregister(store, device);
        }
        self.registered(device, None);
        self.dirty.insert(ViewKey::Devices);
        self.sync_open();
    }

    /// Close a connection, saying why: nothing more from it is taken.
    fn kick(&mut self, conn: ConnId, why: &str) {
        if let Some(c) = self.conns.get(&conn) {
            (c.out.0)(Down::Error {
                msg: why.to_string(),
            });
        }
        self.gone(conn);
    }

    /// (Re)start the engine with the saved boat key.
    fn start_engine(&mut self) {
        self.stop_engine();
        match flotilla_core::secrets::api_key() {
            Ok(key) => {
                self.boat_key = Some(
                    key.chars()
                        .rev()
                        .take(4)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect(),
                );
                let tx = self.tx.clone();
                let events = Events::new(move |e| tx.send(In::Engine(e)).is_ok());
                match Engine::start(self.db.clone(), key, events) {
                    Ok(engine) => {
                        if !self.open.is_empty() {
                            engine.send(Cmd::Open(self.open.clone()));
                        }
                        self.engine = Some(Behind::Engine(Box::new(engine)));
                        self.engine_error = None;
                    }
                    Err(e) => self.engine_error = Some(e),
                }
            }
            Err(e) => {
                self.boat_key = None;
                self.engine_error = Some(e.to_string());
            }
        }
        // A new engine's first read answers anyone waiting; with none, nothing will.
        if self.engine.is_none() {
            let why = self
                .engine_error
                .clone()
                .unwrap_or_else(|| "Flotilla isn't connected to boat".into());
            let waiting: Vec<Who> = std::mem::take(&mut self.usage_waiters)
                .into_iter()
                .chain(std::mem::take(&mut self.agent_usage_waiters))
                .collect();
            for who in waiting {
                self.answer(&who, Err(why.clone()));
            }
        }
        self.dirty.insert(ViewKey::Sidebar);
        self.dirty.insert(ViewKey::Settings);
    }

    fn offline(&self) -> bool {
        matches!(self.mode, Mode::Offline)
    }

    fn engine_state(&self) -> EngineState {
        match (&self.engine, &self.engine_error) {
            (Some(_), _) => EngineState::Running,
            (None, _) if self.offline() => EngineState::Offline,
            (None, e) => EngineState::Stopped(e.clone().unwrap_or_else(|| "not started".into())),
        }
    }

    /// Every view some connected client subscribes to.
    fn subscribed(&self) -> BTreeSet<ViewKey> {
        self.sessions
            .values()
            .filter(|s| s.conn.is_some())
            .flat_map(|s| s.subs.keys().cloned())
            .collect()
    }

    /// The threads open on screen anywhere: an agent reading one doesn't count.
    fn open_threads(&self) -> Vec<String> {
        let open: BTreeSet<String> = self
            .sessions
            .values()
            .filter(|s| s.conn.is_some() && s.scope == Scope::Full)
            .flat_map(|s| s.subs.keys())
            .filter_map(|k| match k {
                ViewKey::Thread(id) => Some(id.clone()),
                _ => None,
            })
            .collect();
        open.into_iter().collect()
    }

    /// The threads whose view some connected client subscribes to: those open on screen, and
    /// those an agent reads.
    fn viewed_threads(&self) -> Vec<String> {
        let viewed: BTreeSet<String> = self
            .subscribed()
            .into_iter()
            .filter_map(|k| match k {
                ViewKey::Thread(id) => Some(id),
                _ => None,
            })
            .collect();
        viewed.into_iter().collect()
    }

    /// Tell the engine which threads are open, if that changed.
    fn sync_open(&mut self) {
        let open = self.open_threads();
        if open != self.open {
            self.open = open.clone();
            self.send_engine(Cmd::Open(open));
        }
        let viewed: HashSet<String> = self.viewed_threads().into_iter().collect();
        self.thread_projects.retain(|t, _| viewed.contains(t));
        let open: HashSet<&String> = self.open.iter().collect();
        self.live.retain(|t, _| open.contains(t));
    }

    fn send_engine(&self, cmd: Cmd) -> bool {
        match &self.engine {
            Some(Behind::Engine(e)) => {
                e.send(cmd);
                true
            }
            Some(Behind::Fake(f)) => {
                f(cmd);
                true
            }
            None => false,
        }
    }

    /// A token for a request out with the engine or a worker.
    fn ask(&mut self, who: &Who, client_made: Option<String>) -> u64 {
        let token = self.next_token;
        self.next_token += 1;
        if let Some(id) = &client_made {
            self.inflight.insert(id.clone(), token);
        }
        self.asks.insert(
            token,
            Ask {
                who: who.clone(),
                client_made,
            },
        );
        token
    }

    /// Run an action for a request: its answer comes with the engine's `Done`. With no
    /// engine, it's refused at once.
    fn act(&mut self, who: &Who, action: Action, client_made: Option<String>) {
        if self.engine.is_none() {
            self.no_engine(who);
            return;
        }
        let token = self.ask(who, client_made);
        self.send_engine(Cmd::Act {
            action,
            req: Some(token),
        });
    }

    /// An engine command with nothing to wait for: accepted.
    fn command(&mut self, who: &Who, cmd: Cmd) {
        if self.send_engine(cmd) {
            self.answer(who, Ok(Reply::Done));
        } else {
            self.no_engine(who);
        }
    }

    /// A command whose findings come back as `progress` for this request.
    fn pieced(&mut self, who: &Who, cmd: impl FnOnce(Option<u64>) -> Cmd) {
        if self.engine.is_none() {
            self.no_engine(who);
            return;
        }
        let token = self.ask(who, None);
        self.send_engine(cmd(Some(token)));
        self.answer(who, Ok(Reply::Done));
    }

    /// A request that needs the engine, with none: refused, so no client waits on it.
    fn no_engine(&mut self, who: &Who) {
        let why = if self.offline() {
            OFFLINE.to_string()
        } else {
            self.engine_error
                .clone()
                .unwrap_or_else(|| "Flotilla isn't connected to boat".into())
        };
        self.answer(who, Err(why));
    }

    fn answer(&mut self, who: &Who, result: Result<Reply, String>) {
        self.queue.push((
            To::Session(who.session.clone(), who.life),
            Down::Res {
                id: who.id,
                result,
                n: None,
            },
        ));
    }

    fn note(&mut self, note: Note) {
        self.queue.push((
            To::All,
            Down::Note {
                note: note.clone(),
                n: None,
            },
        ));
        self.alert_for(&note);
    }

    /// A worker's finding for request `token` (or, with none, for everyone); `last` when no
    /// more will come for it.
    fn progress(&mut self, token: Option<u64>, note: Note, last: bool) {
        let Some(token) = token else {
            return self.note(note);
        };
        let Some(ask) = (if last {
            self.asks.remove(&token)
        } else {
            self.asks.get(&token).map(|a| Ask {
                who: a.who.clone(),
                client_made: None,
            })
        }) else {
            return;
        };
        self.queue.push((
            To::Session(ask.who.session, ask.who.life),
            Down::Progress {
                id: ask.who.id,
                note,
                n: None,
            },
        ));
    }

    /// Take one input; false to stop.
    fn take(&mut self, input: In) -> bool {
        match input {
            In::Shutdown => return false,
            In::Connect(id, out, peer) => self.connected(id, out, peer),
            In::Auth(token, ip, done) => match self.store.as_ref() {
                Some(store) => {
                    let r =
                        store.authenticate(&token, ip.as_deref(), flotilla_core::util::now_ms());
                    if r.is_ok() {
                        self.dirty.insert(ViewKey::Devices);
                    }
                    done(r)
                }
                None => done(Err(AuthError::Db("the database is being rebuilt".into()))),
            },
            In::Pair(mut req, ip, note, done) => match self.store.as_ref() {
                Some(store) => {
                    let asked = req.scope.unwrap_or_default();
                    let now = flotilla_core::util::now_ms();
                    // The demo's standing code: a fresh code of its own each time.
                    let standing = matches!(self.mode, Mode::Demo)
                        .then(|| crate::demo::standing(store, &req.code.0, now))
                        .flatten();
                    if let Some(c) = &standing {
                        req.code.0 = c.code.clone();
                    }
                    let r = store.pair_device_noting(
                        &req.code.0,
                        &req.device_name,
                        &req.platform,
                        asked,
                        ip.as_deref(),
                        now,
                        note,
                    );
                    if r.is_ok() {
                        self.dirty.insert(ViewKey::Devices);
                    } else if let Some(c) = &standing {
                        let _ = store.revoke_code(&c.id, now);
                    }
                    done(r)
                }
                None => done(Err(PairError::Db("the database is being rebuilt".into()))),
            },
            In::History(peer, thread, page, done) => done(self.read(&peer, |store| {
                views::history(store, &thread, page)
                    .map_err(|e| ReadError::Busy(e.plain()))?
                    .ok_or_else(|| {
                        ReadError::Missing(format!("Thread {thread} has no page {page}"))
                    })
            })),
            In::Full(peer, thread, seq, field, mark, done) => done(self.read(&peer, |store| {
                views::uncut(store, &thread, seq, &field, &mark)
                    .map_err(|e| ReadError::Busy(e.plain()))?
                    .ok_or_else(|| {
                        ReadError::Missing(
                            "That output has changed since: it's no longer in the thread".into(),
                        )
                    })
            })),
            In::From(id, up) => self.from(id, up),
            In::Gone(id) => self.gone(id),
            In::Engine(e) => self.event(e),
            In::Answer(token, result) => {
                if let Some(ask) = self.asks.remove(&token) {
                    self.answer(&ask.who, result);
                }
            }
            In::Doctor(run, d) => self.doctored(run, d),
            In::PushGone(device, id) => {
                if let Some(store) = &self.store {
                    flotilla_core::push::forget(store, &device, &id);
                    self.registered(
                        &device,
                        flotilla_core::push::registration_of(store, &device).as_deref(),
                    );
                }
            }
            In::Serving(port) => {
                self.serving = port;
                if matches!(self.mode, Mode::Engine) {
                    // The checks under way began without it.
                    self.rerun_doctor();
                }
            }
            In::ServingMcp(port) => {
                self.serving_mcp = port;
                if matches!(self.mode, Mode::Engine) {
                    self.rerun_doctor();
                }
            }
        }
        true
    }

    /// Read for `peer`, unless it's a device that's no longer paired.
    fn read<T>(
        &self,
        peer: &Peer,
        f: impl FnOnce(&Store) -> Result<T, ReadError>,
    ) -> Result<T, ReadError> {
        let Some(store) = self.store.as_ref() else {
            return Err(ReadError::Busy(
                "The server is rebuilding its database: try again in a moment".into(),
            ));
        };
        if let Some(device) = peer.device()
            && !store.device_paired(device).unwrap_or(false)
        {
            return Err(ReadError::Unpaired);
        }
        f(store)
    }

    /// A new connection. A device revoked since its token was checked (here, before this) is
    /// closed at once.
    fn connected(&mut self, id: ConnId, out: Outbox, peer: Peer) {
        if let Some(device) = peer.device() {
            let paired = self
                .store
                .as_ref()
                .is_some_and(|s| s.device_paired(device).unwrap_or(false));
            if !paired {
                (out.0)(Down::Error {
                    msg: if self.store.is_some() {
                        REVOKED.into()
                    } else {
                        "The server is rebuilding its database: try again in a moment".into()
                    },
                });
                return;
            }
            self.dirty.insert(ViewKey::Devices);
        }
        self.conns.insert(
            id,
            Connected {
                out,
                session: None,
                peer,
            },
        );
    }

    fn from(&mut self, conn: ConnId, up: Up) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        let session = c.session.clone();
        match (up, session) {
            (Up::Hello(hello), _) => self.hello(conn, hello),
            (Up::Req { id, req }, Some(session)) => {
                let Some(s) = self.sessions.get_mut(&session) else {
                    return;
                };
                s.seen = Some(id);
                let life = s.life;
                let refused = (s.scope == Scope::Agent)
                    .then(|| req.agent_refusal(s.device.as_deref()))
                    .flatten();
                let who = Who { session, life, id };
                match refused {
                    Some(why) => self.answer(&who, Err(why)),
                    None => self.request(who, req),
                }
            }
            (Up::Sub { view }, Some(session)) => self.subscribe(&session, view),
            (Up::Unsub { view }, Some(session)) => self.unsubscribe(&session, view),
            (Up::Resync { view }, Some(session)) => {
                if let Some(s) = self.sessions.get_mut(&session)
                    && let Some(has) = s.subs.get_mut(&view)
                {
                    *has = Has::Nothing;
                    if !self.views.contains_key(&view) {
                        self.dirty.insert(view);
                    }
                }
            }
            // Before its hello: not a client yet.
            (_, None) => {}
        }
    }

    fn hello(&mut self, conn: ConnId, hello: Hello) {
        let Some((out, device, paired)) = self.conns.get(&conn).map(|c| {
            (
                c.out.clone(),
                c.peer.device().map(str::to_string),
                c.peer.scope(),
            )
        }) else {
            return;
        };
        let scope = paired.narrowest(hello.scope.unwrap_or_default());
        if hello.proto != PROTOCOL {
            (out.0)(Down::Error {
                msg: format!(
                    "This server speaks protocol {PROTOCOL}, not {}: update Flotilla",
                    hello.proto
                ),
            });
            return;
        }
        let sid = hello.session.clone();
        // A session is its device's alone: no other may resume it (or take it over).
        if self.sessions.get(&sid).is_some_and(|s| s.device != device) {
            self.kick(conn, "That session belongs to another device");
            return;
        }
        // This connection's earlier session, if it said hello as another: that one's away now,
        // as if its connection had closed, so it's let go in time and its views aren't kept open.
        if let Some(earlier) = self
            .conns
            .get(&conn)
            .and_then(|c| c.session.clone())
            .filter(|e| *e != sid)
            && let Some(s) = self.sessions.get_mut(&earlier)
            && s.conn == Some(conn)
        {
            s.conn = None;
            s.subs.clear();
            s.gone_at = Some(Instant::now());
        }
        // A session already here on another connection: that one's superseded.
        if let Some(old) = self
            .sessions
            .get(&sid)
            .and_then(|s| s.conn)
            .filter(|c| *c != conn)
            && let Some(owner) = self.conns.get_mut(&old)
        {
            owner.session = None;
        }
        // Resumed if everything it hasn't got is still kept; otherwise it starts afresh.
        let (mut session, resumed) = match self
            .sessions
            .remove(&sid)
            .and_then(|s| s.after(hello.got).map(|replay| Session { replay, ..s }))
        {
            Some(s) => (s, true),
            None => {
                self.next_token += 1;
                (Session::new(conn, self.next_token, device.clone()), false)
            }
        };
        let life = session.life;
        let current = hello.epoch.as_deref() == Some(self.epoch.as_str());
        let subs: BTreeMap<ViewKey, Has> = hello
            .subs
            .into_iter()
            .filter(|h| scope == Scope::Full || h.view.agent_may())
            .map(|h| {
                (
                    h.view,
                    h.rev.filter(|_| current).map_or(Has::Nothing, Has::Rev),
                )
            })
            .collect();
        // What it holds may be behind: read each again (a client that's current gets nothing).
        self.dirty.extend(subs.keys().cloned());
        let watching: Vec<String> = subs
            .keys()
            .filter_map(|k| match k {
                ViewKey::Thread(t) if scope == Scope::Full => Some(t.clone()),
                _ => None,
            })
            .collect();
        session.conn = Some(conn);
        session.scope = scope;
        session.subs = subs;
        session.gone_at = None;
        let seen = session.seen;
        self.sessions.insert(sid.clone(), session);
        if let Some(owner) = self.conns.get_mut(&conn) {
            owner.session = Some(sid.clone());
        }
        let adopted = self
            .store
            .as_ref()
            .and_then(|s| flotilla_core::owner::adopted(s).ok())
            .unwrap_or_default();
        (out.0)(Down::Welcome(Welcome {
            proto: PROTOCOL,
            server: self.info.clone(),
            epoch: self.epoch.clone(),
            resumed,
            seen,
            caps: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
            device,
            adopted,
        }));
        // After its views (and what it missed, kept in `replay`): the open threads' streaming
        // text, then that it's caught up.
        for t in watching {
            if let Some(partials) = self.live.get(&t).filter(|p| !p.is_empty()) {
                self.queue.push((
                    To::Session(sid.clone(), life),
                    Down::Live {
                        thread: t,
                        partials: partials.clone(),
                    },
                ));
            }
        }
        self.queue.push((To::Session(sid, life), Down::Synced));
        self.dirty.insert(ViewKey::Sidebar);
        self.sync_open();
    }

    /// A connection closed (or couldn't keep up). Its session waits a while for it.
    fn gone(&mut self, conn: ConnId) {
        let Some(c) = self.conns.remove(&conn) else {
            return;
        };
        if let Some(device) = c.peer.device() {
            // Seen until now; and it's no longer connected.
            if let Some(store) = &self.store {
                let _ = store.touch_device(device, flotilla_core::util::now_ms());
            }
            self.dirty.insert(ViewKey::Devices);
        }
        let Some(sid) = c.session else { return };
        if let Some(s) = self.sessions.get_mut(&sid)
            && s.conn == Some(conn)
        {
            s.conn = None;
            s.subs.clear();
            s.gone_at = Some(Instant::now());
        }
        self.sync_open();
    }

    fn subscribe(&mut self, session: &str, view: ViewKey) {
        let Some(s) = self.sessions.get_mut(session) else {
            return;
        };
        // Not an agent's to see: nothing is sent.
        if s.scope == Scope::Agent && !view.agent_may() {
            return;
        }
        s.subs.insert(view.clone(), Has::Nothing);
        // An agent reading a thread doesn't have it on screen (`open_threads`).
        if s.scope == Scope::Agent {
            self.dirty.insert(view);
            return;
        }
        if let ViewKey::Thread(id) = &view {
            // Opened again: read as things arrive, even if it was marked unread before.
            self.keep_unread.remove(id);
            self.dirty.insert(ViewKey::Sidebar);
            if let Some(partials) = self.live.get(id).filter(|p| !p.is_empty()) {
                self.queue.push((
                    To::Session(session.to_string(), s.life),
                    Down::Live {
                        thread: id.clone(),
                        partials: partials.clone(),
                    },
                ));
            }
        }
        // Opened: check the VMs, unless that was done in the last five minutes.
        if view == ViewKey::AgentUsage {
            self.send_engine(Cmd::ReadAgentUsage { force: false });
        }
        self.dirty.insert(view);
        self.sync_open();
    }

    fn unsubscribe(&mut self, session: &str, view: ViewKey) {
        if let Some(s) = self.sessions.get_mut(session) {
            s.subs.remove(&view);
        }
        if matches!(view, ViewKey::Thread(_)) {
            self.sync_open();
        }
    }

    fn request(&mut self, who: Who, req: Request) {
        // Writes need the store; while the database is rebuilt there's none.
        let Some(store) = self.store.as_ref() else {
            self.answer(&who, Err("The database is being rebuilt".into()));
            return;
        };
        let wrote = |r: flotilla_core::Result<()>| r.map(|_| Reply::Done).map_err(|e| e.plain());
        match req {
            Request::NewThread(mut new) => match self.uploaded(&who, &mut new.attachments) {
                Ok(()) => self.new_thread(who, *new),
                Err(e) => self.answer(&who, Err(e)),
            },
            Request::Send {
                thread,
                text,
                mut files,
                local,
            } => match self.uploaded(&who, &mut files) {
                Ok(()) => self.send(who, thread, text, files, local),
                Err(e) => self.answer(&who, Err(e)),
            },
            Request::Answer {
                thread,
                ask,
                answers,
                local,
            } => self.answer_ask(who, thread, ask, answers, local),
            Request::DiscardUnsent { thread, id: prompt } => {
                // An answer it carried waits again.
                let _ = flotilla_core::asks::drop_answer_of(store, &thread, &prompt);
                let r = store
                    .delete_sent_prompt(&thread, &prompt)
                    .and_then(|_| store.rebuild_messages(&thread));
                self.changes.merge(Changes::thread(&thread).with_sidebar());
                self.answer(&who, wrote(r));
            }
            Request::Archive(thread) => {
                // It moves under Archived at once; the engine runs `.agents/archive` (waking a
                // stopped VM for it if the repo has one) and stops the VM. One still starting
                // too: its start, seeing the mark, sends nothing, and the archive takes its turn
                // after the VM is made. (Read after the mark: a thread written meanwhile sees it.)
                let r = store.set_archived(&thread, true);
                let state = store.thread(&thread).ok().flatten().map(|t| t.state);
                self.changes.merge(Changes::thread(&thread).with_sidebar());
                match r {
                    Err(e) => self.answer(&who, Err(e.plain())),
                    // Offline, it's archived all the same; no VM is stopped.
                    Ok(())
                        if self.offline()
                            || matches!(
                                state,
                                None | Some(flotilla_core::store::ThreadState::Expired)
                            ) =>
                    {
                        self.answer(&who, Ok(Reply::Done))
                    }
                    Ok(()) => self.act(&who, Action::Archive(thread), None),
                }
            }
            Request::Unarchive(thread) => {
                let r = store.set_archived(&thread, false);
                self.changes.merge(Changes::thread(&thread).with_sidebar());
                self.answer(&who, wrote(r));
            }
            Request::Forget(thread) => {
                // Its parent's card goes too, and its schedules.
                if let Some(parent) = flotilla_core::orchestrate::parents(store).remove(&thread) {
                    self.changes.add_thread(&parent);
                }
                self.changes.schedules = true;
                // Its VM is left as it is, so its links stay, due now, for the worker to end
                // through boat (refused while one is being changed, rather than wait here).
                let r = flotilla_core::public::forgetting(
                    store,
                    &thread,
                    flotilla_core::util::now_ms(),
                    Some(std::time::Duration::ZERO),
                    |s| s.delete_thread(&thread),
                );
                self.keep_unread.remove(&thread);
                self.changes.merge(Changes::thread(&thread).with_sidebar());
                self.answer(&who, wrote(r));
            }
            Request::MarkViewed(thread) => {
                self.keep_unread.remove(&thread);
                let r = store.mark_viewed(&thread);
                self.changes.sidebar = true;
                self.answer(&who, wrote(r));
            }
            Request::Opened { thread, query } => {
                let r = store.record_open(&thread, flotilla_core::util::now_ms(), query.as_deref());
                self.changes.sidebar = true;
                self.answer(&who, wrote(r));
            }
            Request::MarkUnread(thread) => {
                // Wherever it's on screen, it stays unread until it's opened again.
                self.keep_unread.insert(thread.clone());
                let r = store.mark_unread(&thread);
                self.changes.sidebar = true;
                self.answer(&who, wrote(r));
            }
            // Its name comes from a model, which a demo has none of.
            Request::SuggestName(_) if matches!(self.mode, Mode::Demo) => {
                self.answer(&who, Err(crate::demo::refused("suggested names")))
            }
            Request::SuggestName(thread) => {
                let (db, tx, gate) = (self.db.clone(), self.tx.clone(), self.gate.clone());
                let model = store
                    .kv_get(crate::SUMMARY_MODEL_KEY)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| flotilla_core::summarize::DEFAULT_MODEL.to_string());
                let token = self.ask(&who, None);
                // The CLI takes a couple of seconds: off this thread, on its own connection.
                let _ = std::thread::Builder::new()
                    .name("flotilla-name".into())
                    .spawn(move || {
                        let name = (|| {
                            let model = model.trim();
                            if model.is_empty() {
                                return Err("Needs Titles and previews in Settings".to_string());
                            }
                            let messages = gate
                                .with(|| {
                                    Store::open(&db)
                                        .and_then(|s| s.messages(&thread))
                                        .map_err(|e| e.plain())
                                })
                                .unwrap_or_else(|| Err("the server is stopping".to_string()))?;
                            flotilla_core::summarize::Summarizer::new(model)
                                .and_then(|s| s.name(&messages))
                                .map_err(|e| e.to_string())
                        })();
                        let _ = tx.send(In::Answer(token, name.map(Reply::Name)));
                    });
            }
            Request::OnBranch { thread, prompt } => self.on_branch(who, &thread, prompt),
            Request::ProjectMentions(project) => {
                let m = flotilla_core::mentions::stored_for_project(store, &project);
                self.answer(&who, Ok(Reply::Mentions(m)));
            }
            Request::CachedRepos => {
                let repos = self
                    .github
                    .login()
                    .and_then(|l| store.kv_get(&repo_cache_key(l)).ok().flatten())
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default();
                self.answer(&who, Ok(Reply::Repos(repos)));
            }
            Request::SetScripts {
                project,
                scripts,
                run,
            } => {
                let r = store.set_scripts(&project, &scripts);
                self.changes.add_project(&project);
                if r.is_ok() && run {
                    self.send_engine(Cmd::RunSetup {
                        project,
                        fresh: false,
                    });
                }
                self.answer(&who, wrote(r));
            }
            Request::Set(setting) => {
                let r = match &setting {
                    Setting::Defaults(s) => (|| {
                        let [p, m, e, f] = views::DEFAULTS_KEYS;
                        store.kv_set(p, &s.provider)?;
                        store.kv_set(m, &s.model)?;
                        store.kv_set(e, s.effort.as_deref().unwrap_or(""))?;
                        store.kv_set(f, if s.fast { "true" } else { "false" })
                    })(),
                    Setting::IdleMinutes(m) => {
                        store.kv_set(flotilla_core::lifecycle::IDLE_MINUTES_KEY, &m.to_string())
                    }
                    Setting::BranchPrefix(p) => store.kv_set(
                        flotilla_core::lifecycle::BRANCH_PREFIX_KEY,
                        &flotilla_core::lifecycle::clean_branch_prefix(p),
                    ),
                    Setting::SummaryModel(m) => store.kv_set(crate::SUMMARY_MODEL_KEY, m.trim()),
                    Setting::ArchiveOnMerge(on) => store.kv_set(
                        flotilla_core::tidy::ON_MERGE_KEY,
                        if *on { "1" } else { "0" },
                    ),
                    Setting::ArchiveIdleDays(d)
                        if flotilla_core::tidy::IDLE_DAY_CHOICES.contains(d) =>
                    {
                        store.kv_set(flotilla_core::tidy::IDLE_DAYS_KEY, &d.to_string())
                    }
                    Setting::ArchiveIdleDays(d) => Err(flotilla_core::Error::Invalid(format!(
                        "{d} isn't one of the idle days offered"
                    ))),
                };
                // A model chosen is tested at once.
                if let (Ok(()), Setting::SummaryModel(m)) = (&r, &setting)
                    && !m.trim().is_empty()
                {
                    self.send_engine(Cmd::TestSummaries);
                }
                self.dirty.insert(ViewKey::Settings);
                self.answer(&who, wrote(r));
            }
            Request::SetInstructions(text) => {
                let r = flotilla_core::skills::set_instructions(store, &text);
                self.dirty.insert(ViewKey::Settings);
                self.answer(&who, wrote(r));
            }
            Request::PutSkill { skill, blob } => {
                let blobs = flotilla_core::blobs::Blobs::beside(&self.db);
                let size = match blobs.size(&blob) {
                    _ if !flotilla_core::blobs::valid_id(&blob) => {
                        Err(format!("{} wasn't uploaded", skill.name))
                    }
                    Ok(Some(n)) if n as usize > flotilla_core::skills::MAX_PACKED => Err(format!(
                        "{} comes to over {} MB packed",
                        skill.name,
                        flotilla_core::skills::MAX_PACKED / (1024 * 1024)
                    )),
                    Ok(Some(n)) => Ok(n),
                    Ok(None) => Err(format!(
                        "{} is no longer on the server: add it again",
                        skill.name
                    )),
                    Err(e) => Err(e.plain()),
                };
                let from = self.device_of(&who);
                let from_name = self.device_name(from.as_deref());
                let r = size.and_then(|size| {
                    let kept = flotilla_core::skills::UserSkill {
                        name: skill.name,
                        description: skill.description,
                        blob,
                        size,
                        folder: skill.folder,
                        from,
                        from_name,
                        added_ms: flotilla_core::util::now_ms(),
                    };
                    let Some(store) = self.store.as_ref() else {
                        return Err("The database is being rebuilt".to_string());
                    };
                    flotilla_core::skills::put(store, kept).map_err(|e| e.plain())
                });
                self.dirty.insert(ViewKey::Settings);
                self.answer(&who, r.map(|_| Reply::Done));
            }
            Request::RemoveSkill(name) => {
                let r = match flotilla_core::skills::remove(store, &name) {
                    Ok(true) => Ok(Reply::Done),
                    Ok(false) => Err("There's no such skill".to_string()),
                    Err(e) => Err(e.plain()),
                };
                self.dirty.insert(ViewKey::Settings);
                self.answer(&who, r);
            }
            Request::RunDoctor => {
                self.doctor_waiters.push(who);
                self.run_doctor();
            }
            Request::ReadUsage => {
                if self.send_engine(Cmd::ReadUsage) {
                    self.usage_waiters.push(who);
                } else {
                    self.no_engine(&who);
                }
            }
            Request::ReadAgentUsage => {
                if self.send_engine(Cmd::ReadAgentUsage { force: true }) {
                    self.agent_usage_waiters.push(who);
                } else {
                    self.no_engine(&who);
                }
            }
            // Every connection is this machine's or a paired device's, so any may make one.
            Request::PairCode { ttl_secs } => {
                let made_by = self
                    .sessions
                    .get(&who.session)
                    .and_then(|s| s.device.clone())
                    .unwrap_or_else(|| "local".into());
                let ttl = ttl_secs
                    .map(Duration::from_secs)
                    .unwrap_or(flotilla_core::devices::CODE_TTL);
                let result = match store.kv_get(SERVER_URL_KEY).ok().flatten() {
                    None => Err(match self.role {
                        Role::Server => {
                            "Flotilla isn't serving other devices here: run `flotilla serve`"
                                .to_string()
                        }
                        Role::App => "Let other devices connect first".to_string(),
                    }),
                    Some(url) => store
                        .mint_code(ttl, &made_by, flotilla_core::util::now_ms())
                        .map_err(|e| e.plain())
                        .map(|c| {
                            let link = flotilla_proto::pair::link(
                                flotilla_core::flavor::get().url_scheme(),
                                &url,
                                &c.code,
                            );
                            Reply::Code(PairingCode {
                                code: c.code,
                                expires_ms: c.expires_ms,
                                link,
                            })
                        }),
                };
                self.answer(&who, result);
            }
            // A read of every thread: a device is checked again here, as a page of history
            // is, so one revoked since it connected (and not yet closed) reads nothing.
            Request::PutSchedule(_) if matches!(self.mode, Mode::Demo) => {
                self.answer(&who, Err(crate::demo::refused("schedules")))
            }
            Request::PutSchedule(spec) => self.put_schedule(who, *spec),
            Request::PauseSchedule { schedule, paused } => {
                self.pause_schedule(who, &schedule, paused)
            }
            Request::DeleteSchedule(id) => {
                let r = store.delete_schedule(&id).map(|_| ());
                self.changes.schedules = true;
                self.answer(&who, wrote(r));
            }
            Request::RunSchedule(id) => match store.schedule(&id) {
                Ok(Some(_)) if self.send_engine(Cmd::RunSchedule(id)) => {
                    self.answer(&who, Ok(Reply::Done))
                }
                Ok(Some(_)) => self.no_engine(&who),
                Ok(None) => self.answer(&who, Err("That schedule was deleted".into())),
                Err(e) => self.answer(&who, Err(e.plain())),
            },
            Request::Search { query, limit } => {
                let r = match self.device_of(&who) {
                    Some(device) if !store.device_paired(&device).unwrap_or(false) => {
                        Err(REVOKED.to_string())
                    }
                    _ => store
                        .search(&query, limit)
                        .map(Reply::Hits)
                        .map_err(|e| e.plain()),
                };
                self.answer(&who, r);
            }
            Request::RevokeDevice(device) => match store.revoke_device(&device) {
                Ok(true) => {
                    self.device_marks.retain(|(d, _)| *d != device);
                    // Answered first: the device revoked may be the one asking.
                    self.answer(&who, Ok(Reply::Done));
                    self.revoked(&device);
                }
                Ok(false) => self.answer(&who, Err("There's no such device".into())),
                Err(e) => self.answer(&who, Err(e.plain())),
            },
            Request::RenameDevice { device, name } => {
                let r = match store.rename_device(&device, &name) {
                    Ok(true) => {
                        self.dirty.insert(ViewKey::Devices);
                        Ok(Reply::Done)
                    }
                    Ok(false) => Err("There's no such device".to_string()),
                    Err(e) => Err(e.plain()),
                };
                self.answer(&who, r);
            }
            // A phone's address for notifications: only a paired device's, one per device.
            Request::RegisterPush(token) => {
                let r =
                    match self.device_of(&who) {
                        None => Err("Notifications go to paired devices; this machine isn't one"
                            .to_string()),
                        Some(device) => flotilla_core::push::register(
                            store,
                            &device,
                            &token,
                            flotilla_core::util::now_ms(),
                        )
                        .map(|id| {
                            self.registered(&device, Some(&id));
                            // How this server sends: the phone registers with a relay only if
                            // it's this server's.
                            Reply::Push(flotilla_proto::PushMode {
                                relay: self.push_relay(),
                            })
                        }),
                    };
                self.answer(&who, r);
            }
            Request::UnregisterPush => {
                let r = match self.device_of(&who) {
                    None => Ok(Reply::Done),
                    Some(device) => {
                        self.registered(&device, None);
                        flotilla_core::push::unregister(store, &device)
                            .map(|_| Reply::Done)
                            .map_err(|e| e.plain())
                    }
                };
                self.answer(&who, r);
            }
            // Secrets stay where the engine runs: only this machine sets them.
            Request::SetBoatKey(_) if self.device_of(&who).is_some() => {
                self.answer(
                    &who,
                    Err("Set the boat key on the server: run `flotilla setup` there".into()),
                );
            }
            Request::SetBoatKey(key) => match flotilla_core::secrets::set_api_key(&key.0) {
                Ok(()) if !matches!(self.mode, Mode::Engine) => {
                    self.dirty.insert(ViewKey::Settings);
                    self.answer(&who, Ok(Reply::Done));
                }
                Ok(()) => {
                    // The new key may be another account's.
                    self.usage = None;
                    if let Some(store) = &self.store {
                        let _ = store.kv_delete(USAGE_KEY);
                    }
                    self.start_engine();
                    let result = match &self.engine_error {
                        None => Ok(Reply::Done),
                        Some(e) => Err(e.clone()),
                    };
                    self.answer(&who, result);
                    // Checks under way began with the old key.
                    self.rerun_doctor();
                }
                Err(e) => self.answer(&who, Err(e.to_string())),
            },
            Request::Rebuild => {
                if self.engine.is_none() {
                    self.no_engine(&who);
                    return;
                }
                // Close this connection first: the engine renames the file.
                self.store = None;
                self.rebuilding = true;
                self.sidebar = None;
                self.send_engine(Cmd::Rebuild);
                self.answer(&who, Ok(Reply::Done));
            }
            Request::RemoveProject(project) => {
                self.send_engine(Cmd::CancelSetup(project.clone()));
                self.act(&who, Action::RemoveProject(project), None);
            }
            // The engine's actions: answered when they end.
            Request::Retry(t) => {
                // Yours: it brings the thread back from the archive and ends your stop now, as
                // your message does, so an archive or a stop after it stays (the start, seeing
                // it, sends nothing).
                if self.engine.is_some() {
                    let _ = store.set_archived(&t, false);
                    let _ = store.kv_delete(&flotilla_core::lifecycle::stopped_by_user_key(&t));
                    self.changes.merge(Changes::thread(&t).with_sidebar());
                }
                self.act(&who, Action::Retry(t), None)
            }
            Request::Interrupt(t) => self.act(&who, Action::Interrupt(t), None),
            Request::Stop(t) => self.act(&who, Action::Stop(t), None),
            Request::Resume(t) => self.act(&who, Action::Resume(t), None),
            Request::Rename { thread, title } => {
                self.act(&who, Action::Rename(thread, title), None)
            }
            Request::KeepRunning { thread, on } => {
                self.act(&who, Action::KeepRunning(thread, on), None)
            }
            Request::Preview { thread, port } => {
                self.act(&who, Action::Preview(thread, port), None)
            }
            Request::MakePublic { thread, port, for_ } => {
                self.act(&who, Action::MakePublic { thread, port, for_ }, None)
            }
            Request::MakePrivate { thread, port } => {
                self.act(&who, Action::MakePrivate { thread, port }, None)
            }
            Request::Delete(t) => self.act(&who, Action::Delete(t), None),
            Request::MergePr(t) => self.act(&who, Action::MergePr(t), None),
            Request::MarkReady(t) => self.act(&who, Action::MarkReady(t), None),
            Request::LinkPr { thread, number } => {
                self.act(&who, Action::LinkPr { thread, number }, None)
            }
            Request::ReviewPrompt(t) => self.act(&who, Action::ReviewPrompt(t), None),
            Request::AddProject { repo, scripts } => {
                self.act(&who, Action::AddProject { repo, scripts }, None)
            }
            Request::SetupFixPrompt { project, run } => {
                self.act(&who, Action::SetupFixPrompt { project, run }, None)
            }
            // The engine's commands: accepted now; what they find comes as notes and views.
            Request::ReadMentions(t) => self.command(&who, Cmd::ReadMentions(t)),
            Request::RunSetup { project, fresh } => {
                self.command(&who, Cmd::RunSetup { project, fresh })
            }
            Request::CancelSetup(p) => self.command(&who, Cmd::CancelSetup(p)),
            Request::Services(c) => self.command(&who, Cmd::Services(c)),
            Request::CheckGitHub => self.command(&who, Cmd::CheckGitHub),
            Request::SyncNow => self.command(&who, Cmd::SyncNow),
            Request::TestSummaries => self.command(&who, Cmd::TestSummaries),
            // Accepted now; what they find comes to this client as progress.
            Request::ListRepos => self.pieced(&who, |req| Cmd::ListRepos { req }),
            Request::Suggest(repo) => self.pieced(&who, |req| Cmd::Suggest { repo, req }),
            Request::ListBranches(project) => {
                self.pieced(&who, |req| Cmd::ListBranches { project, req })
            }
        }
    }

    /// A client-made id seen before: answered `Already`, or, if its request is still running,
    /// that one's answer now goes to this asker. True if it was. With `keep`, a new one is kept
    /// as seen; without, it's only looked up (for a request about to be refused).
    fn seen(&mut self, who: &Who, id: &str, keep: bool) -> bool {
        if let Some(token) = self.inflight.get(id)
            && let Some(ask) = self.asks.get_mut(token)
        {
            ask.who = who.clone();
            return true;
        }
        let first = self.store.as_ref().is_none_or(|s| {
            if keep {
                s.first_receipt(id).unwrap_or(true)
            } else {
                !s.has_receipt(id).unwrap_or(false)
            }
        });
        if !first {
            self.answer(who, Ok(Reply::Already));
        }
        !first
    }

    /// `thread.new`: write the thread (still `creating`, its first prompt sending) so it shows
    /// at once, then have the engine make its VM and send the prompt. Without what that needs
    /// here (a catalog, the environment), the engine's own start writes it a moment later.
    fn new_thread(&mut self, who: Who, req: flotilla_proto::NewThread) {
        let Some(thread) = req.id.clone() else {
            self.answer(&who, Err("thread.new needs the new thread's id".into()));
            return;
        };
        // Asked before (a client unsure it went): answered as then, engine or not.
        if self.seen(&who, &thread, self.engine.is_some()) {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if store.thread(&thread).ok().flatten().is_some() {
            self.answer(&who, Ok(Reply::Already));
            return;
        }
        // Nothing to run it: refused before it's written or its id kept, so it never shows as
        // starting, and asked again it's refused again.
        if self.engine.is_none() {
            return self.no_engine(&who);
        }
        let begun = self
            .catalog
            .as_ref()
            .and_then(|c| flotilla_core::lifecycle::begin_thread(store, c, &req).ok());
        match begun {
            Some((t, local)) => {
                let mut c = Changes::thread(&t.id).with_sidebar();
                if let Some(p) = &req.project_id {
                    c.add_project(p);
                }
                if let Some(parent) = &req.parent {
                    c.add_thread(parent);
                }
                self.changes.merge(c);
                self.act(
                    &who,
                    Action::Launch {
                        thread: t.id,
                        req: Box::new(req),
                        local,
                    },
                    Some(thread),
                );
            }
            None => self.act(&who, Action::New(Box::new(req)), Some(thread)),
        }
    }

    /// `thread.send`: show the message at once, marked as sending, then send it. Resuming a
    /// stopped VM can take seconds; the placeholder takes boat's id when the send goes through
    /// and goes away (the text back to the client) if it fails.
    fn send(
        &mut self,
        who: Who,
        thread: String,
        text: String,
        files: Vec<flotilla_proto::Attached>,
        local: String,
    ) {
        if !local.starts_with(flotilla_core::lifecycle::LOCAL_PREFIX) {
            self.answer(&who, Err("thread.send needs a local- id".into()));
            return;
        }
        // Asked before (a client unsure it went): answered as then, engine or not.
        if self.seen(&who, &local, self.engine.is_some()) {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if store
            .sent_prompts(&thread)
            .map(|s| s.iter().any(|p| p.id == local))
            .unwrap_or(false)
        {
            self.answer(&who, Ok(Reply::Already));
            return;
        }
        // Nothing to send it: refused before it shows as sending or its id is kept.
        if self.engine.is_none() {
            return self.no_engine(&who);
        }
        self.send_new(who, thread, text, files, local);
    }

    /// `thread.answer`: the answers to a question the thread's agent waits on, sent to it as
    /// `thread.send` sends a message. Kept with the question at once (so it shows answered
    /// wherever the thread is open), and let go if the message doesn't go (`done`).
    fn answer_ask(
        &mut self,
        who: Who,
        thread: String,
        ask: String,
        answers: Vec<Vec<String>>,
        local: String,
    ) {
        use flotilla_core::asks;
        if !local.starts_with(flotilla_core::lifecycle::LOCAL_PREFIX) {
            self.answer(&who, Err("thread.answer needs a local- id".into()));
            return;
        }
        if self.seen(&who, &local, self.engine.is_some()) {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if store
            .sent_prompts(&thread)
            .map(|s| s.iter().any(|p| p.id == local))
            .unwrap_or(false)
        {
            self.answer(&who, Ok(Reply::Already));
            return;
        }
        let msg = store.messages(&thread).ok().and_then(|m| {
            m.into_iter()
                .find(|m| m.kind == asks::KIND && m.tool_id.as_deref() == Some(ask.as_str()))
        });
        let answered = asks::answers(store, &thread).contains_key(&ask);
        let checked = match &msg {
            None => Err("That question isn't in the thread any more".to_string()),
            Some(m) if answered || m.status.as_deref() == Some(asks::ANSWERED) => {
                Err("That question was answered already".into())
            }
            Some(m) if m.status.as_deref() != Some(asks::WAITING) => {
                Err("The thread moved on from that question".into())
            }
            Some(m) => {
                let questions = flotilla_proto::ask::questions(m);
                asks::tidy(&questions, &answers).map(|a| (asks::reply_text(&questions, &a), a))
            }
        };
        let (text, answers) = match checked {
            Ok(x) => x,
            Err(e) => return self.answer(&who, Err(e)),
        };
        if self.engine.is_none() {
            return self.no_engine(&who);
        }
        // The answer and the message that takes it are kept together; then it's sent as any
        // message is (its placeholder is already there).
        let working = store
            .thread_summaries()
            .ok()
            .and_then(|s| s.into_iter().find(|s| s.thread.id == thread))
            .is_some_and(|s| s.activity == Activity::Working);
        let answer = asks::Answer {
            answers,
            local: local.clone(),
        };
        if let Err(e) = asks::answer_with_message(
            store,
            &thread,
            &ask,
            answer,
            if working { "steer" } else { "prompt" },
            &text,
        ) {
            return self.answer(&who, Err(e.plain()));
        }
        self.send_new(who, thread, text, Vec::new(), local);
    }

    /// Show a message as sending, then have the engine send it (a steer while a turn runs).
    fn send_new(
        &mut self,
        who: Who,
        thread: String,
        text: String,
        files: Vec<flotilla_proto::Attached>,
        local: String,
    ) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let working = store
            .thread_summaries()
            .ok()
            .and_then(|s| s.into_iter().find(|s| s.thread.id == thread))
            .is_some_and(|s| s.activity == Activity::Working);
        let shown = store
            .insert_sent_prompt(
                &thread,
                &local,
                if working { "steer" } else { "prompt" },
                &text,
            )
            .is_ok()
            && store.set_sent_attachments(&thread, &local, &files).is_ok()
            && store.rebuild_messages(&thread).is_ok();
        self.changes.merge(Changes::thread(&thread).with_sidebar());
        let action = Action::Send {
            thread,
            text,
            files,
            local: shown.then(|| local.clone()),
        };
        self.act(&who, action, Some(local));
    }

    /// `schedule.put`: check it, then keep it. Its next time counts from now when it's new or
    /// its timing changed; an edit of only its prompt or target keeps it (and whether it's
    /// paused or waiting on its thread). The engine looks at once.
    fn put_schedule(&mut self, who: Who, spec: flotilla_proto::ScheduleSpec) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let now = flotilla_core::util::now_ms();
        let existing = store.schedule(&spec.id).ok().flatten();
        // Sent again after a drop: what's kept is it.
        if existing.as_ref().is_some_and(|e| e.spec() == spec) {
            return self.answer(&who, Ok(Reply::Done));
        }
        let was = existing.as_ref().map(|e| e.spec());
        let checked = flotilla_model::schedule::check(&spec, now, was.as_ref()).and_then(|()| {
            match &spec.target {
                flotilla_proto::Target::Thread(t) => match store.thread(t).ok().flatten() {
                    None => Err("That thread was deleted".to_string()),
                    Some(t) if t.vm_gone() => Err("That thread's VM is gone".to_string()),
                    Some(_) => Ok(()),
                },
                flotilla_proto::Target::New(f) => {
                    if f.project_id
                        .as_deref()
                        .is_some_and(|p| store.project(p).ok().flatten().is_none())
                    {
                        return Err("That project was removed".to_string());
                    }
                    match &self.catalog {
                        Some(c) => c
                            .validate(&f.provider, &f.model, f.effort.as_deref(), f.fast)
                            .map(|_| ()),
                        None => Ok(()),
                    }
                }
            }
        });
        if let Err(e) = checked {
            return self.answer(&who, Err(e));
        }
        let retimed = existing
            .as_ref()
            .is_none_or(|e| e.when != spec.when || e.zone != spec.zone);
        let paused = existing.as_ref().is_some_and(|e| e.paused);
        let next_ms = match (&existing, retimed, paused) {
            (_, _, true) => None,
            (Some(e), false, false) => e.next_ms,
            // A one-off goes at its time, even one a moment ago (`check` gives a minute's grace).
            _ => match &spec.when {
                flotilla_proto::When::Once { at_ms } => Some(*at_ms),
                when => flotilla_model::schedule::next_after(when, &spec.zone, now)
                    .ok()
                    .flatten(),
            },
        };
        let from = existing
            .as_ref()
            .map(|e| e.from.clone())
            .unwrap_or_else(|| self.device_name(self.device_of(&who).as_deref()));
        let schedule = flotilla_proto::Schedule {
            id: spec.id,
            prompt: spec.prompt,
            target: spec.target,
            when: spec.when,
            zone: spec.zone,
            paused,
            next_ms,
            waiting_ms: existing
                .as_ref()
                .filter(|_| !retimed)
                .and_then(|e| e.waiting_ms),
            last: existing.as_ref().and_then(|e| e.last.clone()),
            runs: existing.as_ref().map_or(0, |e| e.runs),
            from,
            created_ms: existing.as_ref().map_or(now, |e| e.created_ms),
            // Moved on from what the engine last read, so its next write can't undo this.
            updated_ms: now.max(existing.as_ref().map_or(0, |e| e.updated_ms + 1)),
        };
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let r = store
            .put_schedule(&schedule, retimed)
            .map_err(|e| e.plain());
        self.changes.schedules = true;
        if r.is_ok() {
            self.send_engine(Cmd::LookSchedules);
        }
        self.answer(&who, r.map(|()| Reply::Done));
    }

    /// `schedule.pause`: paused, it doesn't run; resumed, it counts from now (not one that runs
    /// more often than once a day, set before that was the rule: it's edited first).
    fn pause_schedule(&mut self, who: Who, id: &str, paused: bool) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let now = flotilla_core::util::now_ms();
        let Some(s) = store.schedule(id).ok().flatten() else {
            return self.answer(&who, Err("That schedule was deleted".into()));
        };
        if !paused && let Err(why) = flotilla_model::schedule::often_enough(&s.when) {
            return self.answer(&who, Err(why));
        }
        let next_ms = match (&s.when, paused) {
            (_, true) => None,
            (flotilla_proto::When::Once { at_ms }, false) if *at_ms <= now => {
                return self.answer(
                    &who,
                    Err("Its time has passed: edit it to pick another".into()),
                );
            }
            (flotilla_proto::When::Once { at_ms }, false) => Some(*at_ms),
            (when, false) => flotilla_model::schedule::next_after(when, &s.zone, now)
                .ok()
                .flatten(),
        };
        let r = store
            .set_schedule_paused(id, paused, next_ms, now.max(s.updated_ms + 1))
            .map(|_| ())
            .map_err(|e| e.plain());
        self.changes.schedules = true;
        if r.is_ok() && !paused {
            self.send_engine(Cmd::LookSchedules);
        }
        self.answer(&who, r.map(|()| Reply::Done));
    }

    /// `thread.on_branch`: New thread on the thread's branch (its PR's base, else the branch
    /// its work merges into), once GitHub says the branch is there.
    fn on_branch(&mut self, who: Who, thread: &str, prompt: Option<String>) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let found = (|| {
            let t = store.thread(thread).ok().flatten()?;
            let (project, name) = (t.repo_project_id.clone()?, t.branch.clone()?);
            let p = store.project(&project).ok().flatten()?;
            let pr = store.pull_requests().ok()?.remove(thread).map(|(pr, _)| pr);
            let base = match pr {
                Some(pr) if pr.head == name && !pr.base.is_empty() => pr.base,
                _ => flotilla_core::lifecycle::base_branch(store, thread, &p.default_branch),
            };
            Some((project, flotilla_core::lifecycle::OnBranch { name, base }))
        })();
        match found {
            Some((project, branch)) => self.pieced(&who, |req| Cmd::CheckBranch {
                project,
                branch,
                prompt,
                req,
            }),
            None => self.answer(
                &who,
                Err("That thread isn't on a branch of a project".into()),
            ),
        }
    }

    fn event(&mut self, e: Event) {
        match e {
            Event::Changed(c) => self.changes.merge(c),
            Event::Live { thread, partials } => {
                if self.open.contains(&thread) {
                    self.live.insert(thread.clone(), partials.clone());
                    self.queue.push((
                        To::Watching(thread.clone()),
                        Down::Live { thread, partials },
                    ));
                }
            }
            Event::Done {
                action,
                result,
                req,
            } => self.done(action, result, req),
            Event::Status(s) => {
                self.status = s;
                self.dirty.insert(ViewKey::Sidebar);
            }
            Event::Usage(read) => {
                let answer = read.as_ref().map(|_| Reply::Done).map_err(Clone::clone);
                if let Ok(u) = read {
                    if let (Some(store), Ok(json)) = (&self.store, serde_json::to_string(&u)) {
                        let _ = store.kv_set(USAGE_KEY, &json);
                    }
                    self.usage = Some(u);
                    self.dirty.insert(ViewKey::Sidebar);
                }
                for who in std::mem::take(&mut self.usage_waiters) {
                    self.answer(&who, answer.clone());
                }
            }
            Event::GitHub(g) => {
                self.github = g.clone();
                self.dirty.insert(ViewKey::Sidebar);
                self.note(Note::GitHub(g));
            }
            Event::Catalog(c) => {
                self.catalog = Some(c);
                self.dirty.insert(ViewKey::Catalog);
            }
            Event::Summaries(result) => {
                self.dirty.insert(ViewKey::Settings);
                self.note(Note::Summaries(result));
            }
            Event::Rebuilt(result) => {
                self.store = Store::open(&self.db).ok();
                self.rebuilding = false;
                self.changes.merge(Changes::everything());
                self.dirty.extend(self.subscribed());
                self.note(Note::Rebuilt(result.map(|p| p.display().to_string())));
            }
            Event::Asked(id) => self.note(Note::Asked(id)),
            Event::TurnFinished(id) => {
                // The agent may have pushed or opened a pull request: look now, not on the
                // schedule.
                let in_project = self
                    .store
                    .as_ref()
                    .and_then(|s| s.thread(&id).ok().flatten())
                    .is_some_and(|t| t.repo_project_id.is_some());
                if in_project {
                    self.send_engine(Cmd::CheckPr(id.clone()));
                }
                // A scheduled prompt may be waiting for it.
                self.send_engine(Cmd::LookSchedules);
                self.note(Note::TurnFinished(id));
            }
            Event::Unsent(threads) => self.note(Note::Unsent(threads)),
            Event::Repos {
                result,
                done,
                login,
                req,
            } => {
                // Kept under the account the list was fetched as, which gh may have switched to.
                if done
                    && let (Ok(repos), Some(store), Some(login)) =
                        (&result, &self.store, login.as_deref())
                {
                    let _ = store.kv_set(
                        &repo_cache_key(login),
                        &serde_json::to_string(repos).unwrap_or_default(),
                    );
                }
                self.progress(
                    req,
                    Note::Repos {
                        result,
                        done,
                        login,
                    },
                    done,
                );
            }
            Event::Suggested { repo, result, req } => {
                self.progress(req, Note::Suggested { repo, result }, true)
            }
            Event::SetupDone {
                project,
                name,
                ok,
                error,
                recovered,
            } => {
                // A script failed, so a code change can fix it: say which run.
                let fixable = (!ok)
                    .then(|| {
                        let store = self.store.as_ref()?;
                        let run = store.setup_runs(&project, 1).ok()?.into_iter().next()?;
                        flotilla_core::fix::setup_fixable(&run, &store.scripts(&project).ok()?)
                            .then_some(run.id)
                    })
                    .flatten();
                self.note(Note::SetupDone {
                    project,
                    name,
                    ok,
                    error,
                    recovered,
                    fixable,
                });
            }
            Event::PrMerged {
                thread,
                number,
                title,
                branch,
                archived,
            } => self.note(Note::PrMerged {
                thread,
                number,
                title,
                branch,
                archived,
            }),
            Event::IdleArchived(threads) => self.note(Note::IdleArchived(threads)),
            Event::AgentUsage { checked } => {
                self.dirty.insert(ViewKey::AgentUsage);
                if let Some(result) = checked {
                    let answer = result.map(|_| Reply::Done);
                    for who in std::mem::take(&mut self.agent_usage_waiters) {
                        self.answer(&who, answer.clone());
                    }
                }
            }
            Event::Branches {
                project,
                result,
                req,
            } => self.progress(req, Note::Branches { project, result }, true),
            Event::BranchChecked {
                project,
                branch,
                prompt,
                result,
                req,
            } => self.progress(
                req,
                Note::BranchChecked {
                    project,
                    branch,
                    prompt,
                    result,
                },
                true,
            ),
            Event::ScheduleFailed {
                schedule,
                prompt,
                thread,
                why,
            } => self.note(Note::ScheduleFailed {
                schedule,
                prompt,
                thread,
                why,
            }),
            Event::Mentions { thread, mentions } => {
                if self
                    .subscribed()
                    .contains(&ViewKey::Mentions(thread.clone()))
                {
                    self.ready.push(View::Mentions {
                        thread,
                        mentions: Some((*mentions).clone()),
                    });
                }
            }
        }
    }

    /// An action ended: answer its request, or tell every client if the engine started it.
    fn done(&mut self, action: Action, result: Result<Outcome, String>, req: Option<u64>) {
        match (&action, &result) {
            // Set a new project up straight away, so its first thread starts ready.
            (Action::AddProject { .. }, Ok(Outcome::Created(project))) => {
                self.send_engine(Cmd::CheckProject(project.clone()));
            }
            // GitHub changed: look at the PRs now (a merge sets the project up at once).
            (Action::MergePr(_) | Action::MarkReady(_) | Action::LinkPr { .. }, Ok(_)) => {
                self.send_engine(Cmd::CheckPrs);
            }
            // An answer that didn't go: its question waits again.
            (
                Action::Send {
                    thread,
                    local: Some(local),
                    ..
                },
                Err(_),
            ) => {
                if let Some(store) = &self.store
                    && flotilla_core::asks::drop_answer_of(store, thread, local).unwrap_or(false)
                {
                    let _ = store.rebuild_messages(thread);
                    self.changes.merge(Changes::thread(thread).with_sidebar());
                }
            }
            _ => {}
        }
        let result = result.map(reply);
        // A start refused because it was archived or stopped as it started: you put it away, so
        // no alert.
        let put_away = result.as_ref().is_err_and(|e| {
            e == flotilla_core::lifecycle::SCHEDULED_ARCHIVED
                || e == flotilla_core::lifecycle::START_STOPPED
        });
        let of = started(&action);
        match req {
            Some(token) => {
                // A start that failed is news wherever whoever started it is now.
                let failed = (result.is_err()
                    && !put_away
                    && matches!(
                        action,
                        Action::New(_) | Action::Launch { .. } | Action::Retry(_)
                    ))
                .then(|| {
                    request_of(action).map(|request| Note::Done {
                        request,
                        result: result.clone(),
                    })
                })
                .flatten();
                if let Some(ask) = self.asks.remove(&token) {
                    if let Some(id) = &ask.client_made {
                        self.inflight.remove(id);
                    }
                    // The app that asked hears it as a start that went as far as it could: the
                    // thread is written and says why itself ("Archived before it started.",
                    // with Try again), so there's no error for it to show.
                    let answer = match of {
                        Some(thread) if put_away => Ok(Reply::Created(thread)),
                        _ => result,
                    };
                    self.answer(&ask.who, answer);
                }
                if let Some(note) = failed {
                    self.alert_for(&note);
                }
            }
            None => {
                // A scheduled thread's start that failed is its schedule's to say, once for
                // failures alike (`ScheduleFailed`): not a start-failed alert for each.
                let scheduled = matches!(action, Action::StartLater { .. });
                if let Some(request) = request_of(action) {
                    let note = Note::Done { request, result };
                    if scheduled || put_away {
                        self.queue.push((To::All, Down::Note { note, n: None }));
                    } else {
                        self.note(note);
                    }
                }
            }
        }
    }

    /// Keep a view just computed: a new revision (and its delta) if it changed.
    fn keep(&mut self, view: View) {
        let key = view.key();
        let data = view.data();
        let flush = self.flushes;
        match self.views.get_mut(&key) {
            Some(k) if k.data == data => k.used = flush,
            Some(k) => {
                self.rev += 1;
                match flotilla_proto::delta::diff(&key, &k.data, &data) {
                    Some(ops) => {
                        let bytes = serde_json::to_vec(&ops).map_or(0, |b| b.len());
                        k.ring.push_back(Step {
                            base: k.rev,
                            rev: self.rev,
                            ops,
                            bytes,
                        });
                        k.ring_bytes += bytes;
                        while k.ring.len() > RING || (k.ring_bytes > RING_BYTES && k.ring.len() > 1)
                        {
                            if let Some(old) = k.ring.pop_front() {
                                k.ring_bytes -= old.bytes;
                            }
                        }
                    }
                    // Only whole: what was kept before can't lead here.
                    None => {
                        k.ring.clear();
                        k.ring_bytes = 0;
                    }
                }
                k.rev = self.rev;
                k.data = data;
                k.used = flush;
            }
            None => {
                self.rev += 1;
                self.views.insert(
                    key,
                    Kept {
                        rev: self.rev,
                        data,
                        ring: VecDeque::new(),
                        ring_bytes: 0,
                        used: flush,
                    },
                );
            }
        }
    }

    /// Recompute the views this batch changed, then send each client its views caught up,
    /// then what's queued for it.
    fn flush(&mut self) {
        self.flushes += 1;
        let changes = std::mem::take(&mut self.changes);
        let dirty = std::mem::take(&mut self.dirty);
        let mut fresh = std::mem::take(&mut self.ready);
        // While the database is rebuilt there's nothing to read; `Rebuilt` sends it all.
        if !self.rebuilding {
            fresh.extend(self.compute(&changes, &dirty));
        }
        for view in fresh {
            self.keep(view);
        }
        let queue = std::mem::take(&mut self.queue);
        let mut dead = Vec::new();
        let mut dropped = Vec::new();
        for (sid, s) in &mut self.sessions {
            let Some(conn) = s.conn else {
                // Away: what's for it (not live text) is kept, numbered, for when it's back.
                for (to, msg) in &queue {
                    if (matches!(to, To::All) && s.scope == Scope::Full)
                        || matches!(to, To::Session(x, life) if x == sid && *life == s.life)
                    {
                        s.number(msg);
                    }
                }
                if s.gone_at.is_some_and(|t| t.elapsed() > AWAY_FOR) {
                    dropped.push(sid.clone());
                }
                continue;
            };
            let mut out = Vec::new();
            for (key, has) in &mut s.subs {
                if let Some(kept) = self.views.get_mut(key) {
                    kept.used = self.flushes;
                    catch_up(key, has, kept, &mut out);
                }
            }
            out.append(&mut s.replay);
            for (to, msg) in &queue {
                let mine = match to {
                    // Notes are for the apps: an agent isn't told of schedules, setup runs or
                    // anything else outside its scope.
                    To::All => s.scope == Scope::Full,
                    To::Session(x, life) => x == sid && *life == s.life,
                    // On screen: an agent reading it isn't sent the streaming text.
                    To::Watching(t) => {
                        s.scope == Scope::Full && s.subs.contains_key(&ViewKey::Thread(t.clone()))
                    }
                };
                if mine {
                    out.push(s.number(msg));
                }
            }
            if let Some(c) = self.conns.get(&conn)
                && !out.into_iter().all(|m| (c.out.0)(m))
            {
                dead.push(conn);
            }
        }
        for sid in dropped {
            self.sessions.remove(&sid);
        }
        for conn in dead {
            self.gone(conn);
        }
        self.forget_idle_views();
    }

    /// Keep only so many views no one subscribes to.
    fn forget_idle_views(&mut self) {
        let subscribed = self.subscribed();
        let mut idle: Vec<(u64, ViewKey)> = self
            .views
            .iter()
            .filter(|(k, _)| !subscribed.contains(*k))
            .map(|(k, v)| (v.used, k.clone()))
            .collect();
        if idle.len() <= KEEP_IDLE {
            return;
        }
        idle.sort();
        for (_, key) in idle.into_iter().rev().skip(KEEP_IDLE) {
            self.views.remove(&key);
        }
    }

    /// The subscribed views `changes` touched (or `dirty` asks for). A named thread's view is
    /// recomputed, and so are its parent's and children's (they show its title and card), and
    /// a named project's threads'.
    fn compute(&mut self, changes: &Changes, dirty: &BTreeSet<ViewKey>) -> Vec<View> {
        let Some(store) = self.store.as_ref() else {
            return vec![];
        };
        let subs = self.subscribed();
        let mut out = Vec::new();
        let threads = self.viewed_threads();
        // Who handed whom work before this batch too: a child that moves still shows (or
        // stops showing) on the card of the one it left.
        let before = self
            .sidebar
            .as_ref()
            .map(|s| s.parents.clone())
            .unwrap_or_default();
        let sidebar_due = changes.all
            || changes.sidebar
            || !changes.projects.is_empty()
            || dirty.contains(&ViewKey::Sidebar)
            || self.sidebar.is_none();
        if sidebar_due {
            // Threads on screen are read as things arrive (unless marked unread).
            for id in self
                .open
                .iter()
                .filter(|id| !self.keep_unread.contains(*id))
            {
                let _ = store.mark_viewed(id);
            }
            let live = Live {
                status: self.status.clone(),
                github: self.github.clone(),
                usage: self.usage.clone(),
                engine: self.engine_state(),
            };
            match views::sidebar(store, &live) {
                Ok(s) => {
                    if subs.contains(&ViewKey::Sidebar) {
                        out.push(View::Sidebar(s.clone()));
                    }
                    self.sidebar = Some(s);
                }
                Err(e) => eprintln!("[host] couldn't read the sidebar: {e}"),
            }
        }
        let empty = Sidebar {
            threads: vec![],
            parents: Default::default(),
            waiting: vec![],
            projects: vec![],
            environments: vec![],
            status: SyncStatus::default(),
            github: GhStatus::Unknown,
            usage: None,
            engine: EngineState::Offline,
            limits: vec![],
        };
        let sidebar = self.sidebar.as_ref().unwrap_or(&empty);
        for id in &threads {
            let family = |t: &String| {
                [&sidebar.parents, &before]
                    .iter()
                    .any(|p| p.get(t) == Some(id) || p.get(id) == Some(t))
            };
            let project = self.thread_projects.get(id).cloned().flatten();
            let due = dirty.contains(&ViewKey::Thread(id.clone()))
                || changes.has_thread(id)
                || changes.threads.iter().any(family)
                || project.is_some_and(|p| changes.has_project(&p));
            if !due {
                continue;
            }
            match views::thread(store, id, &sidebar.threads, &sidebar.parents) {
                Ok(d) => {
                    self.thread_projects.insert(
                        id.clone(),
                        d.thread.as_ref().and_then(|t| t.repo_project_id.clone()),
                    );
                    out.push(View::Thread(Box::new(d)));
                }
                Err(e) => eprintln!("[host] couldn't read thread {id}: {e}"),
            }
        }
        // A prompt's provisioning may have changed the skills' error.
        let skills_moved = (changes.all || !changes.threads.is_empty())
            && flotilla_core::skills::error(store) != self.skills_error;
        for key in &subs {
            let view = match key {
                ViewKey::Sidebar | ViewKey::Thread(_) => continue,
                ViewKey::Catalog if dirty.contains(key) => View::Catalog(self.catalog.clone()),
                ViewKey::Mentions(t) if dirty.contains(key) => View::Mentions {
                    thread: t.clone(),
                    mentions: views::mentions(store, t),
                },
                ViewKey::Project(p) if dirty.contains(key) || changes.has_project(p) => {
                    match views::project(store, p) {
                        Ok(v) => View::Project(v),
                        Err(e) => {
                            eprintln!("[host] couldn't read project {p}: {e}");
                            continue;
                        }
                    }
                }
                ViewKey::Devices if dirty.contains(key) => {
                    let connected = self.connected_devices();
                    match store.devices(flotilla_core::util::now_ms()) {
                        Ok(devices) => View::Devices(Devices {
                            devices: devices
                                .into_iter()
                                .map(|d| DeviceRow {
                                    connected: connected.contains(&d.id),
                                    id: d.id,
                                    name: d.name,
                                    platform: d.platform,
                                    paired_ms: d.created_ms,
                                    last_seen_ms: d.last_seen_ms,
                                    scope: d.scope,
                                })
                                .collect(),
                        }),
                        Err(e) => {
                            eprintln!("[host] couldn't read the devices: {e}");
                            continue;
                        }
                    }
                }
                ViewKey::AgentUsage if dirty.contains(key) || changes.all => {
                    match views::agent_usage(store) {
                        Ok(v) => View::AgentUsage(v),
                        Err(e) => {
                            eprintln!("[host] couldn't read the agents' usage: {e}");
                            continue;
                        }
                    }
                }
                ViewKey::Schedules if dirty.contains(key) || changes.all || changes.schedules => {
                    match views::schedules(store) {
                        Ok(v) => View::Schedules(v),
                        Err(e) => {
                            eprintln!("[host] couldn't read the schedules: {e}");
                            continue;
                        }
                    }
                }
                ViewKey::Settings
                    if dirty.contains(key)
                        || changes.all
                        || !changes.projects.is_empty()
                        || skills_moved =>
                {
                    match views::settings(store, self.boat_key.clone(), self.doctor.clone()) {
                        Ok(v) => {
                            self.skills_error = v.skills_error.clone();
                            View::Settings(v)
                        }
                        Err(e) => {
                            eprintln!("[host] couldn't read the settings: {e}");
                            continue;
                        }
                    }
                }
                _ => continue,
            };
            out.push(view);
        }
        out
    }
}

/// Bring a client's copy of a view up to `kept`: nothing if it's current, the deltas it
/// missed if they're all still kept, else the view whole.
fn catch_up(key: &ViewKey, has: &mut Has, kept: &Kept, out: &mut Vec<Down>) {
    if *has == Has::Rev(kept.rev) {
        return;
    }
    let from = match *has {
        Has::Rev(r) => kept.ring.iter().position(|s| s.base == r),
        Has::Nothing => None,
    };
    match from {
        Some(i) => {
            for s in kept.ring.iter().skip(i) {
                out.push(Down::Delta {
                    view: key.clone(),
                    base: s.base,
                    rev: s.rev,
                    ops: s.ops.clone(),
                });
            }
        }
        None => out.push(Down::Snap {
            view: key.clone(),
            rev: kept.rev,
            data: kept.data.clone(),
        }),
    }
    *has = Has::Rev(kept.rev);
}

/// An action's outcome as its request's answer.
/// The usage last read from boat, as kept.
fn last_usage(store: &Store) -> Option<Usage> {
    store
        .kv_get(USAGE_KEY)
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
}

fn reply(o: Outcome) -> Reply {
    match o {
        Outcome::Done => Reply::Done,
        Outcome::Created(id) => Reply::Created(id),
        Outcome::Sent(s) => Reply::Sent(s),
        Outcome::Url(u) => Reply::Url(u),
        Outcome::Prompt(p) => Reply::Prompt(p),
    }
}

/// The thread a start (a new thread, its launch, Try again) is of.
fn started(action: &Action) -> Option<String> {
    match action {
        Action::New(req) => req.id.clone(),
        Action::Launch { thread, .. }
        | Action::StartLater { thread, .. }
        | Action::Retry(thread) => Some(thread.clone()),
        _ => None,
    }
}

/// The request an action the engine started itself would have been, for `Note::Done`.
fn request_of(action: Action) -> Option<Request> {
    Some(match action {
        Action::New(req) => Request::NewThread(req),
        Action::Launch {
            thread, mut req, ..
        } => {
            req.id = Some(thread);
            Request::NewThread(req)
        }
        Action::StartLater {
            thread, mut req, ..
        } => {
            req.id = Some(thread);
            Request::NewThread(req)
        }
        Action::Retry(t) => Request::Retry(t),
        Action::Send {
            thread,
            text,
            files,
            local,
        } => Request::Send {
            thread,
            text,
            files,
            local: local.unwrap_or_default(),
        },
        Action::SendLater {
            thread,
            text,
            local,
        } => Request::Send {
            thread,
            text,
            files: vec![],
            local,
        },
        // Flotilla's own, tried again until it goes: no one asked for it.
        Action::Tell { .. } => return None,
        Action::Interrupt(t) => Request::Interrupt(t),
        Action::Stop(t) => Request::Stop(t),
        Action::Resume(t) => Request::Resume(t),
        Action::Rename(thread, title) => Request::Rename { thread, title },
        Action::KeepRunning(thread, on) => Request::KeepRunning { thread, on },
        Action::Preview(thread, port) => Request::Preview { thread, port },
        Action::MakePublic { thread, port, for_ } => Request::MakePublic { thread, port, for_ },
        Action::MakePrivate { thread, port } => Request::MakePrivate { thread, port },
        Action::Archive(t) | Action::ArchiveIdle { thread: t, .. } => Request::Archive(t),
        Action::Delete(t) => Request::Delete(t),
        Action::AddProject { repo, scripts } => Request::AddProject { repo, scripts },
        Action::RemoveProject(p) => Request::RemoveProject(p),
        Action::MergePr(t) => Request::MergePr(t),
        Action::MarkReady(t) => Request::MarkReady(t),
        Action::LinkPr { thread, number } => Request::LinkPr { thread, number },
        Action::ReviewPrompt(t) => Request::ReviewPrompt(t),
        Action::SetupFixPrompt { project, run } => Request::SetupFixPrompt { project, run },
    })
}

/// The last repo list Add project showed, per GitHub login.
fn repo_cache_key(login: &str) -> String {
    format!("repo_cache:{}", login.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::store::{Project, Thread, ThreadState};
    use flotilla_proto::wire::Held;
    use flotilla_proto::{Activity, NewThread, Replica, ServerMsg, Taken, through_json};
    use std::sync::Mutex;

    fn s(v: &str) -> String {
        v.to_string()
    }

    /// The host lets go of the database only once its engine's work under way has ended, so
    /// no other engine can start beside an action or sync that's still writing.
    #[test]
    fn the_lock_is_kept_until_the_engines_work_under_way_ends() {
        use flotilla_core::lock::{Busy, Holder};
        let dir = std::env::temp_dir().join(format!("flotilla-host-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        drop(Store::open(&db).unwrap());
        let lock = Lock::take(&db, &Holder::new("app")).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut state = State::new(
            db.clone(),
            Mode::Offline,
            tx.clone(),
            ServerInfo {
                id: s("srv_test"),
                version: s("0"),
            },
            Some(lock),
        );
        let (engine, _) = crate::stopping::busy_engine(&db, Duration::from_millis(1500));
        state.engine = Some(Behind::Engine(Box::new(engine)));
        tx.send(In::Shutdown).unwrap();
        let started = Instant::now();
        let host = std::thread::spawn(move || state.run(rx));
        std::thread::sleep(Duration::from_millis(300));
        // Still finishing: no other engine can take the database.
        assert!(matches!(
            Lock::take(&db, &Holder::new("server")),
            Err(Busy::Held(_))
        ));
        host.join().unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(1100),
            "{:?}",
            started.elapsed()
        );
        assert!(Lock::take(&db, &Holder::new("server")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A client of the fixture's host: what it was sent, and its copy of the views.
    struct Client {
        conn: ConnId,
        session: String,
        got: Arc<Mutex<Vec<Down>>>,
        replica: Replica,
    }

    /// A host on a fresh database, with a stand-in engine that records what it's asked.
    struct Fixture {
        state: State,
        cmds: Arc<Mutex<Vec<Cmd>>>,
        clients: Vec<Client>,
        dir: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("flotilla-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("db");
        let store = Store::open(&db).unwrap();
        let catalog = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../m0-samples/provider-models.json"
        ))
        .unwrap();
        store.kv_set("catalog", &catalog).unwrap();
        let env = |id: &str, name: &str| flotilla_core::boat::Environment {
            id: id.into(),
            name: name.into(),
            is_default: id == "e1",
        };
        store.replace_environments(&[env("e1", "base")]).unwrap();
        drop(store);
        let mut f = Fixture {
            state: restart(&db, None),
            cmds: Arc::default(),
            clients: vec![],
            dir,
        };
        f.state = restart(&db, Some(f.cmds.clone()));
        f.connect();
        f
    }

    /// A host on `db` as it starts (with a stand-in engine that records into `cmds`).
    fn restart(db: &std::path::Path, cmds: Option<Arc<Mutex<Vec<Cmd>>>>) -> State {
        let (tx, _rx) = mpsc::channel();
        let mode = match cmds {
            Some(cmds) => Mode::Fake(Arc::new(move |c| cmds.lock().unwrap().push(c))),
            None => Mode::Offline,
        };
        let mut state = State::new(
            db.to_path_buf(),
            mode.clone(),
            tx,
            ServerInfo {
                id: s("srv_test"),
                version: s("0"),
            },
            None,
        );
        state.store = Some(Store::open(db).unwrap());
        state.catalog = state.store.as_ref().and_then(views::catalog);
        state.usage = state.store.as_ref().and_then(last_usage);
        if let Mode::Fake(f) = mode {
            state.engine = Some(Behind::Fake(f));
        }
        state
    }

    impl Fixture {
        fn store(&self) -> &Store {
            self.state.store.as_ref().unwrap()
        }

        /// Another client, said hello; its index.
        fn connect(&mut self) -> usize {
            let n = self.clients.len();
            self.connect_as(&format!("session-{n}"), None, vec![], None)
        }

        fn connect_as(
            &mut self,
            session: &str,
            epoch: Option<String>,
            subs: Vec<Held>,
            last: Option<u64>,
        ) -> usize {
            self.connect_peer(session, Peer::Local, epoch, subs, last)
        }

        fn connect_peer(
            &mut self,
            session: &str,
            peer: Peer,
            epoch: Option<String>,
            subs: Vec<Held>,
            last: Option<u64>,
        ) -> usize {
            self.connect_scoped(session, peer, None, epoch, subs, last)
        }

        /// A client whose hello asks for `scope`.
        fn connect_scoped(
            &mut self,
            session: &str,
            peer: Peer,
            scope: Option<Scope>,
            epoch: Option<String>,
            subs: Vec<Held>,
            last: Option<u64>,
        ) -> usize {
            let conn = 100 + self.clients.len() as u64;
            let got: Arc<Mutex<Vec<Down>>> = Arc::default();
            let sink = got.clone();
            self.state.take(In::Connect(
                conn,
                Outbox::new(move |m| {
                    // Every message must survive the wire.
                    sink.lock().unwrap().push(through_json(&m).unwrap());
                    true
                }),
                peer,
            ));
            let mut replica = Replica::new();
            for h in &subs {
                replica.subscribe(h.view.clone());
            }
            let hello = Hello {
                proto: PROTOCOL,
                app: s("test"),
                version: s("0"),
                platform: s("test"),
                session: s(session),
                epoch,
                subs,
                got: last,
                scope,
            };
            self.state.take(In::From(conn, Up::Hello(hello)));
            self.clients.push(Client {
                conn,
                session: s(session),
                got,
                replica,
            });
            self.clients.len() - 1
        }

        fn up(&mut self, c: usize, up: Up) {
            let conn = self.clients[c].conn;
            self.state.take(In::From(conn, up));
        }

        fn sub_on(&mut self, c: usize, view: ViewKey) {
            self.clients[c].replica.subscribe(view.clone());
            self.up(c, Up::Sub { view });
        }

        fn sub(&mut self, view: ViewKey) {
            self.sub_on(0, view);
        }

        fn unsub(&mut self, view: ViewKey) {
            self.clients[0].replica.unsubscribe(&view);
            self.up(0, Up::Unsub { view });
        }

        fn ask_on(&mut self, c: usize, id: ReqId, req: Request) {
            self.up(c, Up::Req { id, req });
        }

        fn ask(&mut self, id: ReqId, req: Request) {
            self.ask_on(0, id, req);
        }

        fn engine(&mut self, e: Event) {
            self.state.take(In::Engine(e));
        }

        /// Flush, and what each client was sent, as the app sees it.
        fn flush_all(&mut self) -> Vec<Vec<ServerMsg>> {
            self.state.flush();
            self.clients
                .iter_mut()
                .map(|c| {
                    let got = std::mem::take(&mut *c.got.lock().unwrap());
                    got.into_iter()
                        .filter_map(|d| match c.replica.take(d) {
                            Taken::View(view) => Some(ServerMsg::Snap { view }),
                            Taken::Pass(Down::Res { id, result, .. }) => {
                                Some(ServerMsg::Res { id, result })
                            }
                            Taken::Pass(Down::Live { thread, partials }) => {
                                Some(ServerMsg::Live { thread, partials })
                            }
                            Taken::Pass(Down::Note { note, .. }) => Some(ServerMsg::Note(note)),
                            Taken::Pass(Down::Progress { id, note, .. }) => {
                                Some(ServerMsg::Progress { id, note })
                            }
                            Taken::Resync(k, why) => panic!("lost track of {k:?}: {why}"),
                            _ => None,
                        })
                        .collect()
                })
                .collect()
        }

        fn flush(&mut self) -> Vec<ServerMsg> {
            self.flush_all().remove(0)
        }

        /// The raw messages client `c` was sent at the next flush.
        fn flush_raw(&mut self, c: usize) -> Vec<Down> {
            self.state.flush();
            let got = std::mem::take(&mut *self.clients[c].got.lock().unwrap());
            for d in got.clone() {
                self.clients[c].replica.take(d);
            }
            got
        }

        /// The views a batch with `changes` would recompute.
        fn recomputed(&mut self, changes: Changes) -> Vec<ViewKey> {
            self.state
                .compute(&changes, &BTreeSet::new())
                .iter()
                .map(View::key)
                .collect()
        }

        /// The token the stand-in engine was handed with the last action.
        fn last_act(&self) -> u64 {
            self.cmds
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find_map(|c| match c {
                    Cmd::Act { req, .. } => *req,
                    _ => None,
                })
                .unwrap()
        }

        fn opened(&self) -> Option<Vec<String>> {
            self.cmds
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find_map(|c| match c {
                    Cmd::Open(ids) => Some(ids.clone()),
                    _ => None,
                })
        }
    }

    fn thread(id: &str, project: Option<&str>) -> Thread {
        Thread {
            id: s(id),
            environment_id: Some(s("e1")),
            environment_name: Some(s("base")),
            title: format!("Thread {id}"),
            sandbox_id: Some(format!("bx_{id}")),
            conversation_id: Some(format!("c_{id}")),
            provider: s("claude-code"),
            model: s("opus-5-5"),
            effort: None,
            fast: false,
            state: ThreadState::Idle,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: s("2026-10-31T10:00:00.000Z"),
            updated_at: s("2026-10-31T10:00:00.000Z"),
            repo_project_id: project.map(str::to_string),
            branch: project.map(|_| format!("branch-{id}")),
            base_sha: None,
        }
    }

    fn project(id: &str) -> Project {
        Project {
            id: s(id),
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
        }
    }

    fn new_thread(id: &str) -> NewThread {
        NewThread {
            environment: s("base"),
            project_id: None,
            provider: s("claude-code"),
            model: s("opus-5-5"),
            effort: None,
            fast: false,
            title: None,
            machine_type: None,
            prompt: s("Write a haiku"),
            attachments: vec![],
            wait_ready: false,
            on_branch: None,
            id: Some(s(id)),
            parent: None,
            branch_tag: None,
        }
    }

    fn sidebar(out: &[ServerMsg]) -> Option<&Sidebar> {
        out.iter().rev().find_map(|m| match m {
            ServerMsg::Snap { view } => match view.as_ref() {
                View::Sidebar(s) => Some(s),
                _ => None,
            },
            _ => None,
        })
    }

    fn detail<'a>(out: &'a [ServerMsg], id: &str) -> Option<&'a flotilla_proto::ThreadDetail> {
        out.iter().rev().find_map(|m| match m {
            ServerMsg::Snap { view } => match view.as_ref() {
                View::Thread(d) if d.id == id => Some(d.as_ref()),
                _ => None,
            },
            _ => None,
        })
    }

    fn answer(out: &[ServerMsg], id: ReqId) -> Option<&Result<Reply, String>> {
        out.iter().find_map(|m| match m {
            ServerMsg::Res { id: i, result } if *i == id => Some(result),
            _ => None,
        })
    }

    fn activity(out: &[ServerMsg], id: &str) -> Option<Activity> {
        sidebar(out)?
            .threads
            .iter()
            .find(|t| t.thread.id == id)
            .map(|t| t.activity)
    }

    #[test]
    fn subscribing_sends_each_view_and_reads_the_open_thread() {
        let mut f = fixture("subscribe");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.store().mark_unread("t1").unwrap();
        f.store()
            .insert_sent_prompt("t1", "boat-1", "prompt", "Hello")
            .unwrap();
        f.store().rebuild_messages("t1").unwrap();
        for view in [ViewKey::Sidebar, ViewKey::Catalog, ViewKey::Settings] {
            f.sub(view);
        }
        let out = f.flush();
        assert_eq!(activity(&out, "t1"), Some(Activity::Unread));
        assert!(out.iter().any(|m| matches!(m, ServerMsg::Snap { view } if matches!(view.as_ref(), View::Catalog(Some(_))))));
        assert!(out.iter().any(
            |m| matches!(m, ServerMsg::Snap { view } if matches!(view.as_ref(), View::Settings(_)))
        ));
        // Opened: it's read, and its page and its skills and files come.
        f.sub(ViewKey::Thread(s("t1")));
        f.sub(ViewKey::Mentions(s("t1")));
        let out = f.flush();
        assert_eq!(activity(&out, "t1"), Some(Activity::Idle));
        let d = detail(&out, "t1").unwrap();
        assert_eq!(
            d.messages
                .iter()
                .filter_map(|m| m.text.as_deref())
                .collect::<Vec<_>>(),
            ["Hello"]
        );
        assert!(out.iter().any(|m| matches!(m, ServerMsg::Snap { view } if matches!(view.as_ref(), View::Mentions { .. }))));
        // Nothing changed: nothing is sent again.
        f.engine(Event::Changed(Changes::thread("t1").with_sidebar()));
        assert!(f.flush().is_empty());
        assert!(f.flush_raw(0).is_empty());
    }

    /// A thread of `turns` turns (a prompt and a reply each, so twice as many messages), as
    /// boat's events; `edit` changes one turn's reply, `big` gives one turn a tool result over
    /// the cap.
    fn long_thread(
        store: &Store,
        id: &str,
        turns: usize,
        edit: Option<(usize, &str)>,
        big: Option<usize>,
    ) {
        let mut events = Vec::new();
        for k in 0..turns {
            let task = format!("task-{k}");
            let ts = 1_793_000_000_000i64 + k as i64 * 10_000;
            events.push(RawEvent::from_value(serde_json::json!({"id": format!("p{k}"), "taskId": task, "type": "prompt", "timestamp": ts, "conversationId": "c", "data": {"prompt": format!("prompt {k}"), "status": "finished"}})));
            let text = match edit {
                Some((e, t)) if e == k => t.to_string(),
                _ => format!("reply {k}"),
            };
            let mut data = serde_json::json!({"content": text, "model": "m"});
            if big == Some(k) {
                let result = serde_json::json!({"type": "bash_output", "command": "cat big.log", "content": "é✓ log line\n".repeat(20_000)}).to_string();
                data["tools"] = serde_json::json!([{"use": {"id": "toolu_big", "name": "Bash", "input": {"command": "cat big.log"}, "type": "tool_use"}, "result": {"content": result, "is_error": false, "tool_use_id": "toolu_big", "type": "tool_result"}}]);
            }
            events.push(RawEvent::from_value(serde_json::json!({"id": format!("r{k}"), "taskId": task, "type": "response", "timestamp": ts + 1000, "conversationId": "c", "data": data})));
        }
        store.apply_events(id, &events, false).unwrap();
        store.rebuild_messages(id).unwrap();
    }

    #[test]
    fn a_long_threads_view_holds_its_newest_pages_and_the_rest_come_a_page_at_a_time() {
        use flotilla_proto::history::{CAP, PAGE, WINDOW};
        let mut f = fixture("long");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        long_thread(f.store(), "t1", 100, None, Some(97));
        let t1 = ViewKey::Thread(s("t1"));
        f.sub(t1.clone());
        let out = f.flush();
        let d = detail(&out, "t1").unwrap().clone();
        // 201 messages: the big turn has its tool call too.
        assert_eq!((d.older, d.messages.len(), d.pages.len()), (100, 101, 5));
        assert!(d.messages.len() as i64 >= WINDOW && d.messages.iter().all(|m| m.seq >= d.older));
        // The big result goes out cut, still JSON, and says so.
        let big = d
            .messages
            .iter()
            .find(|m| !m.cut.is_empty())
            .expect("a cut message");
        assert!(big.tool_result_json.as_ref().unwrap().len() <= CAP);
        assert!(serde_json::from_str::<Value>(big.tool_result_json.as_ref().unwrap()).is_ok());
        let straight = |f: &Fixture| {
            let side = views::sidebar(
                f.store(),
                &Live {
                    status: SyncStatus::default(),
                    github: GhStatus::Unknown,
                    usage: None,
                    engine: EngineState::Running,
                },
            )
            .unwrap();
            View::Thread(Box::new(
                views::thread(f.store(), "t1", &side.threads, &side.parents).unwrap(),
            ))
            .data()
        };
        // It grows past a page: the view moves on, by deltas, and is what the database says.
        long_thread(f.store(), "t1", 125, None, Some(97));
        f.engine(Event::Changed(Changes::thread("t1")));
        let raw = f.flush_raw(0);
        assert!(
            raw.iter()
                .any(|d| matches!(d, Down::Delta { view, .. } if *view == t1)),
            "a delta, not a snapshot"
        );
        assert_eq!(f.clients[0].replica.data(&t1), Some(&straight(&f)));
        let View::Thread(d) = f.clients[0].replica.view(&t1).unwrap() else {
            panic!()
        };
        assert_eq!((d.older, d.messages.len()), (150, 101));
        // A message outside the view changes: only its page's mark moves.
        long_thread(f.store(), "t1", 125, Some((3, "changed")), Some(97));
        f.engine(Event::Changed(Changes::thread("t1")));
        let raw = f.flush_raw(0);
        let ops: Vec<&Op> = raw
            .iter()
            .filter_map(|d| match d {
                Down::Delta { ops, .. } => Some(ops),
                _ => None,
            })
            .flatten()
            .collect();
        assert!(
            matches!(&ops[..], [Op::Put { field, v }] if field == "pages" && v["page"] == 0),
            "{ops:?}"
        );
        assert_eq!(f.clients[0].replica.data(&t1), Some(&straight(&f)));
        // Pages as a phone asks for them: the changed one, with the view's mark.
        let read = |f: &mut Fixture, peer: Peer, thread: &str, page: i64| {
            let (tx, rx) = mpsc::channel();
            f.state.take(In::History(
                peer,
                s(thread),
                page,
                Box::new(move |r| {
                    let _ = tx.send(r);
                }),
            ));
            rx.recv().unwrap()
        };
        let View::Thread(d) = f.clients[0].replica.view(&t1).unwrap().clone() else {
            panic!()
        };
        let page = read(&mut f, Peer::Local, "t1", 0).unwrap();
        assert_eq!(page.messages.len() as i64, PAGE);
        assert_eq!(page.messages[7].text.as_deref(), Some("changed"));
        assert_eq!(page.mark, d.pages[0].mark);
        assert_eq!(
            read(&mut f, Peer::Local, "t1", 4).unwrap().messages[0].seq,
            200
        );
        // None past the end, nor of a thread that isn't; nothing for a device that's gone.
        assert_eq!(
            read(&mut f, Peer::Local, "t1", 5).unwrap().messages.len(),
            1
        );
        assert!(matches!(
            read(&mut f, Peer::Local, "t1", 6),
            Err(ReadError::Missing(_))
        ));
        assert!(matches!(
            read(&mut f, Peer::Local, "nope", 0),
            Err(ReadError::Missing(_))
        ));
        assert!(matches!(
            read(&mut f, Peer::Local, "t1", -1),
            Err(ReadError::Missing(_))
        ));
        assert_eq!(
            read(&mut f, Peer::Device(s("dev_gone")), "t1", 0),
            Err(ReadError::Unpaired)
        );
        // The cut string whole, by its mark; once changed, it's gone.
        let View::Thread(d) = f.clients[0].replica.view(&t1).unwrap().clone() else {
            panic!()
        };
        let big = d
            .messages
            .iter()
            .find(|m| !m.cut.is_empty())
            .unwrap()
            .clone();
        let full = |f: &mut Fixture, peer: Peer, seq: i64, mark: &str| {
            let (tx, rx) = mpsc::channel();
            f.state.take(In::Full(
                peer,
                s("t1"),
                seq,
                s("tool_result_json"),
                s(mark),
                Box::new(move |r| {
                    let _ = tx.send(r);
                }),
            ));
            rx.recv().unwrap()
        };
        let whole = full(&mut f, Peer::Local, big.seq, &big.cut[0].mark).unwrap();
        assert_eq!(whole.len() as u64, big.cut[0].bytes);
        assert!(whole.contains("é✓ log line"));
        // Found by its mark if the message moved a little; not looked for further away.
        assert_eq!(
            full(&mut f, Peer::Local, big.seq - 7, &big.cut[0].mark).unwrap(),
            whole
        );
        assert!(matches!(
            full(&mut f, Peer::Local, big.seq - 120, &big.cut[0].mark),
            Err(ReadError::Missing(_))
        ));
        assert!(matches!(
            full(&mut f, Peer::Local, big.seq, "0000000000000000"),
            Err(ReadError::Missing(_))
        ));
        assert_eq!(
            full(
                &mut f,
                Peer::Device(s("dev_gone")),
                big.seq,
                &big.cut[0].mark
            ),
            Err(ReadError::Unpaired)
        );
    }

    #[test]
    fn marked_unread_while_open_stays_unread_until_opened_again() {
        let mut f = fixture("unread");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.sub(ViewKey::Sidebar);
        f.sub(ViewKey::Thread(s("t1")));
        f.flush();
        f.ask(1, Request::MarkUnread(s("t1")));
        let out = f.flush();
        assert_eq!(answer(&out, 1), Some(&Ok(Reply::Done)));
        assert_eq!(activity(&out, "t1"), Some(Activity::Unread));
        // More arrives for it while it's open: still unread.
        f.store()
            .insert_sent_prompt("t1", "boat-2", "prompt", "More")
            .unwrap();
        f.engine(Event::Changed(Changes::thread("t1").with_sidebar()));
        let out = f.flush();
        assert_eq!(activity(&out, "t1"), None, "the sidebar didn't change");
        assert_eq!(
            f.state.sidebar.as_ref().unwrap().threads[0].activity,
            Activity::Unread
        );
        // Opened again: read.
        f.unsub(ViewKey::Thread(s("t1")));
        f.sub(ViewKey::Thread(s("t1")));
        assert_eq!(activity(&f.flush(), "t1"), Some(Activity::Idle));
        // Marked unread, then read from the menu.
        f.ask(2, Request::MarkUnread(s("t1")));
        assert_eq!(activity(&f.flush(), "t1"), Some(Activity::Unread));
        f.ask(3, Request::MarkViewed(s("t1")));
        assert_eq!(activity(&f.flush(), "t1"), Some(Activity::Idle));
    }

    #[test]
    fn a_named_child_parent_or_project_recomputes_the_views_that_show_it() {
        let mut f = fixture("family");
        f.store().insert_project(&project("p1"), &[]).unwrap();
        for (id, p) in [("t1", Some("p1")), ("t2", None), ("t3", None)] {
            f.store().insert_thread(&thread(id, p)).unwrap();
        }
        flotilla_core::orchestrate::set_parent(f.store(), "t2", "t1").unwrap();
        f.sub(ViewKey::Sidebar);
        f.sub(ViewKey::Thread(s("t1")));
        f.sub(ViewKey::Project(s("p1")));
        let out = f.flush();
        assert_eq!(
            detail(&out, "t1")
                .unwrap()
                .children
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            ["Thread t2"]
        );
        let t1 = ViewKey::Thread(s("t1"));
        // Its child is named (a new title on its card): its view is read again.
        assert!(f.recomputed(Changes::thread("t2")).contains(&t1));
        // Its project is named.
        let project = f.recomputed(Changes::project("p1"));
        assert!(project.contains(&t1) && project.contains(&ViewKey::Project(s("p1"))));
        // Another thread: not.
        assert!(!f.recomputed(Changes::thread("t3")).contains(&t1));
        // The child's own view reads again when its parent is named (its title shows there).
        f.unsub(t1.clone());
        f.sub(ViewKey::Thread(s("t2")));
        f.flush();
        assert!(
            f.recomputed(Changes::thread("t1"))
                .contains(&ViewKey::Thread(s("t2")))
        );
        // A renamed child shows on the parent's card.
        f.unsub(ViewKey::Thread(s("t2")));
        f.sub(t1.clone());
        f.flush();
        f.store().set_thread_title("t2", "Reviewer").unwrap();
        f.engine(Event::Changed(Changes::thread("t2").with_sidebar()));
        let out = f.flush();
        assert_eq!(detail(&out, "t1").unwrap().children[0].title, "Reviewer");
    }

    #[test]
    fn a_send_shows_at_once_and_sent_again_is_a_no_op() {
        let mut f = fixture("send");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.sub(ViewKey::Thread(s("t1")));
        f.flush();
        let send = Request::Send {
            thread: s("t1"),
            text: s("Also add tests"),
            files: vec![],
            local: s("local-abc"),
        };
        f.ask(1, send.clone());
        let out = f.flush();
        let d = detail(&out, "t1").unwrap();
        assert_eq!(
            d.sent.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["local-abc"]
        );
        let shown = d
            .messages
            .iter()
            .find(|m| m.text.as_deref() == Some("Also add tests"))
            .unwrap();
        assert_eq!(shown.status.as_deref(), Some("sending"));
        // It's with the engine: answered when it's done. Sent again meanwhile, the answer goes
        // to the one that asked last.
        assert_eq!(answer(&out, 1), None);
        let token = f.last_act();
        f.ask(2, send.clone());
        assert!(f.flush().is_empty());
        f.engine(Event::Done {
            action: Action::Send {
                thread: s("t1"),
                text: s("x"),
                files: vec![],
                local: None,
            },
            result: Ok(Outcome::Sent(flotilla_proto::Sent::Prompt(s("p1")))),
            req: Some(token),
        });
        let out = f.flush();
        assert_eq!(answer(&out, 1), None);
        assert!(matches!(answer(&out, 2), Some(Ok(Reply::Sent(_)))));
        // Once done, the same message again is a no-op.
        f.ask(3, send);
        assert_eq!(answer(&f.flush(), 3), Some(&Ok(Reply::Already)));
        assert_eq!(f.store().sent_prompts("t1").unwrap().len(), 1);
        // Without a `local-` id it's refused.
        f.ask(
            4,
            Request::Send {
                thread: s("t1"),
                text: s("x"),
                files: vec![],
                local: s("mine"),
            },
        );
        assert!(matches!(answer(&f.flush(), 4), Some(Err(_))));
    }

    /// A Codex question as read in its VM, in a turn whose reply shows it as text.
    fn asked_question(f: &Fixture, at: i64) {
        use flotilla_core::asks::Asked;
        use flotilla_proto::ask::{Choice, Question};
        let shown = "Which file name should I create?\n- alpha.txt (recommended)\n- beta.txt";
        let events = [
            flotilla_core::event::RawEvent::from_value(
                serde_json::json!({"id": "p1", "type": "prompt", "timestamp": at - 5000, "taskId": "p1", "data": {"prompt": "Ask me, then make it.", "status": "finished"}}),
            ),
            flotilla_core::event::RawEvent::from_value(
                serde_json::json!({"id": "r1", "type": "response", "timestamp": at + 3, "taskId": "p1", "data": {"content": format!("{shown}\nWhich one?")}}),
            ),
        ];
        f.store().apply_events("t1", &events, true).unwrap();
        // Its turn is over, as a poll settles it.
        for mut t in f.store().unsettled_turns("t1").unwrap() {
            (t.done, t.settled) = (true, true);
            f.store().update_turn(&t).unwrap();
        }
        let q = Question {
            header: None,
            question: s("Which file name should I create?"),
            options: vec![
                Choice {
                    label: s("alpha.txt (recommended)"),
                    description: None,
                },
                Choice {
                    label: s("beta.txt"),
                    description: None,
                },
            ],
            multi: false,
        };
        let asked = Asked {
            id: s("call_1"),
            at_ms: at,
            text: s(shown),
            tool: s(flotilla_core::asks::CODEX_TOOL),
            questions: vec![q],
            prompt: None,
            followed: false,
            call: None,
            turn: None,
        };
        assert_eq!(
            flotilla_core::asks::store(f.store(), "t1", &[asked]).unwrap(),
            Some(vec![s("call_1")])
        );
        f.store().rebuild_messages("t1").unwrap();
    }

    #[test]
    fn an_answer_goes_as_a_message_and_shows_answered_at_once() {
        let mut f = fixture("answer");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.store().mark_viewed("t1").unwrap();
        // Asked after it was last seen: it needs you, as a finished turn does.
        asked_question(&f, flotilla_core::util::now_ms() + 60_000);
        f.sub(ViewKey::Sidebar);
        f.sub(ViewKey::Thread(s("t1")));
        let out = f.flush();
        assert_eq!(activity(&out, "t1"), Some(Activity::Unread));
        let ask = |out: &[ServerMsg]| {
            detail(out, "t1")
                .unwrap()
                .messages
                .iter()
                .find(|m| m.kind == "ask")
                .cloned()
                .unwrap()
        };
        assert_eq!(ask(&out).status.as_deref(), Some("waiting"));
        // A question left unanswered, or two picks for a single-choice one: refused, nothing sent.
        let answer_req = |answers: Vec<Vec<String>>, local: &str| Request::Answer {
            thread: s("t1"),
            ask: s("call_1"),
            answers,
            local: s(local),
        };
        f.ask(1, answer_req(vec![vec![]], "local-a1"));
        f.ask(
            2,
            answer_req(
                vec![vec![s("alpha.txt (recommended)"), s("beta.txt")]],
                "local-a2",
            ),
        );
        let out = f.flush();
        assert!(matches!(answer(&out, 1), Some(Err(_))) && matches!(answer(&out, 2), Some(Err(_))));
        assert!(f.store().sent_prompts("t1").unwrap().is_empty());
        // Answered: it goes as the question and the answer, and shows answered at once.
        f.ask(3, answer_req(vec![vec![s(" beta.txt ")]], "local-a3"));
        let out = f.flush();
        let a = ask(&out);
        assert_eq!(a.status.as_deref(), Some("answered"));
        assert_eq!(
            flotilla_proto::ask::answers(&a),
            Some(vec![vec![s("beta.txt")]])
        );
        let sent = f.store().sent_prompts("t1").unwrap();
        assert_eq!(
            (sent[0].id.as_str(), sent[0].text.as_str()),
            ("local-a3", "Which file name should I create?\nbeta.txt")
        );
        let acted = f.cmds.lock().unwrap().iter().rev().find_map(|c| match c {
            Cmd::Act {
                action: Action::Send { text, local, .. },
                ..
            } => Some((text.clone(), local.clone())),
            _ => None,
        });
        assert_eq!(
            acted,
            Some((
                s("Which file name should I create?\nbeta.txt"),
                Some(s("local-a3"))
            ))
        );
        // Answered already: a second answer is refused.
        f.ask(
            4,
            answer_req(vec![vec![s("alpha.txt (recommended)")]], "local-a4"),
        );
        assert!(matches!(answer(&f.flush(), 4), Some(Err(e)) if e.contains("answered already")));
        // The message didn't go (the engine drops its placeholder): the question waits again.
        let token = f.last_act();
        f.store().delete_sent_prompt("t1", "local-a3").unwrap();
        f.engine(Event::Done {
            action: Action::Send {
                thread: s("t1"),
                text: s("x"),
                files: vec![],
                local: Some(s("local-a3")),
            },
            result: Err(s("boat said no")),
            req: Some(token),
        });
        let out = f.flush();
        assert!(matches!(answer(&out, 3), Some(Err(_))));
        assert_eq!(ask(&out).status.as_deref(), Some("waiting"));
        assert!(flotilla_core::asks::answers(f.store(), "t1").is_empty());
    }

    /// A Claude Code thread whose agent asked with `flotilla ask`: its question shows waiting
    /// and needs you; answered, it goes as the next message, and once boat has taken that, the
    /// message says it took the answer (the apps show the card, not it, under its turn).
    #[test]
    fn a_flotilla_ask_in_a_claude_thread_is_answered_as_codexs_are() {
        use flotilla_core::asks;
        use flotilla_proto::ask::{Choice, Question};
        let mut f = fixture("flotilla-ask");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.store().mark_viewed("t1").unwrap();
        let at = flotilla_core::util::now_ms() + 60_000;
        let call = serde_json::json!([{"use": {"id": "toolu_1", "name": "Bash", "input": {"command": "flotilla ask …"}}, "result": {"content": "FLOTILLA …", "is_error": false}}]);
        let events = [
            flotilla_core::event::RawEvent::from_value(
                serde_json::json!({"id": "p1", "type": "prompt", "timestamp": at - 5000, "taskId": "p1", "data": {"prompt": "Ask me, then make it.", "status": "finished"}}),
            ),
            flotilla_core::event::RawEvent::from_value(
                serde_json::json!({"id": "r1", "type": "response", "timestamp": at, "taskId": "p1", "data": {"content": "", "tools": call}}),
            ),
            flotilla_core::event::RawEvent::from_value(
                serde_json::json!({"id": "r2", "type": "response", "timestamp": at + 900, "taskId": "p1", "data": {"content": "I've asked which name to use."}}),
            ),
        ];
        f.store().apply_events("t1", &events, true).unwrap();
        for mut t in f.store().unsettled_turns("t1").unwrap() {
            (t.done, t.settled) = (true, true);
            f.store().update_turn(&t).unwrap();
        }
        let q = Question {
            header: None,
            question: s("Which file name should I create?"),
            options: vec![
                Choice {
                    label: s("alpha.txt (recommended)"),
                    description: None,
                },
                Choice {
                    label: s("beta.txt"),
                    description: None,
                },
            ],
            multi: false,
        };
        let ask_id = s("7c11d703-6d90-4aba-a93a-a86fd03a90f7");
        let asked =
            asks::from_request(&ask_id, vec![q], at, Some(s("toolu_1")), Some(s("p1"))).unwrap();
        asks::store(f.store(), "t1", &[asked]).unwrap();
        f.store().rebuild_messages("t1").unwrap();
        f.sub(ViewKey::Sidebar);
        f.sub(ViewKey::Thread(s("t1")));
        let out = f.flush();
        assert_eq!(activity(&out, "t1"), Some(Activity::Unread));
        let kinds = |out: &[ServerMsg]| {
            detail(out, "t1")
                .unwrap()
                .messages
                .iter()
                .map(|m| m.kind.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(kinds(&out), ["prompt", "ask", "text"]);
        let ask = |out: &[ServerMsg]| {
            detail(out, "t1")
                .unwrap()
                .messages
                .iter()
                .find(|m| m.kind == "ask")
                .cloned()
                .unwrap()
        };
        assert_eq!(
            (ask(&out).tool_id, ask(&out).status.as_deref()),
            (Some(ask_id.clone()), Some("waiting"))
        );
        // Answered from its card.
        f.ask(
            1,
            Request::Answer {
                thread: s("t1"),
                ask: ask_id.clone(),
                answers: vec![vec![s("beta.txt")]],
                local: s("local-a1"),
            },
        );
        let out = f.flush();
        assert_eq!(ask(&out).status.as_deref(), Some("answered"));
        let acted = f.cmds.lock().unwrap().iter().rev().find_map(|c| match c {
            Cmd::Act {
                action:
                    Action::Send {
                        thread,
                        text,
                        local,
                        ..
                    },
                ..
            } => Some((thread.clone(), text.clone(), local.clone())),
            _ => None,
        });
        assert_eq!(
            acted,
            Some((
                s("t1"),
                s("Which file name should I create?\nbeta.txt"),
                Some(s("local-a1"))
            ))
        );
        // Sending, then taken: its turn, the message marked as the answer's.
        let answer = |out: &[ServerMsg]| {
            detail(out, "t1")
                .unwrap()
                .messages
                .iter()
                .find(|m| m.kind == "prompt" && m.task_id.as_deref() != Some("p1"))
                .cloned()
                .unwrap()
        };
        assert_eq!(answer(&out).answer_to.as_deref(), Some(ask_id.as_str()));
        f.store()
            .record_sent_prompt(
                "t1",
                Some("local-a1"),
                "pr-2",
                "prompt",
                "Which file name should I create?\nbeta.txt",
            )
            .unwrap();
        assert_eq!(
            asks::answers(f.store(), "t1")[&ask_id].local,
            "pr-2",
            "the answer went with its message"
        );
        let taken = flotilla_core::event::RawEvent::from_value(
            serde_json::json!({"id": "p2", "type": "prompt", "timestamp": at + 20_000, "taskId": "pr-2", "data": {"prompt": "Which file name should I create?\nbeta.txt", "status": "running"}}),
        );
        f.store().apply_events("t1", &[taken], true).unwrap();
        f.store().rebuild_messages("t1").unwrap();
        f.engine(Event::Changed(Changes::thread("t1").with_sidebar()));
        let out = f.flush();
        let a = answer(&out);
        assert_eq!(
            (
                a.task_id.as_deref(),
                a.status.as_deref(),
                a.answer_to.as_deref()
            ),
            (Some("pr-2"), Some("running"), Some(ask_id.as_str()))
        );
        // The thread's row says what you chose (shown while its turn runs), not the question.
        let row = sidebar(&out)
            .unwrap()
            .threads
            .iter()
            .find(|t| t.thread.id == "t1")
            .cloned()
            .unwrap();
        assert_eq!(row.last_prompt.as_deref(), Some("beta.txt"));
    }

    #[test]
    fn a_question_asked_while_the_agent_works_leaves_it_working() {
        let mut f = fixture("answer-working");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.store().mark_viewed("t1").unwrap();
        asked_question(&f, flotilla_core::util::now_ms() + 60_000);
        // Its turn goes on: still working (what follows a running turn reads it so); unread
        // once it's over.
        f.store().insert_turn("t1", "p2", "running").unwrap();
        f.sub(ViewKey::Sidebar);
        assert_eq!(activity(&f.flush(), "t1"), Some(Activity::Working));
    }

    #[test]
    fn a_question_not_answered_passes_when_something_else_is_sent() {
        let mut f = fixture("answer-passed");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        asked_question(&f, flotilla_core::util::now_ms() - 60_000);
        f.sub(ViewKey::Thread(s("t1")));
        f.flush();
        f.ask(
            1,
            Request::Send {
                thread: s("t1"),
                text: s("Use gamma.txt instead"),
                files: vec![],
                local: s("local-p1"),
            },
        );
        let out = f.flush();
        let a = detail(&out, "t1")
            .unwrap()
            .messages
            .iter()
            .find(|m| m.kind == "ask")
            .cloned()
            .unwrap();
        assert_eq!(a.status.as_deref(), Some("passed"));
        f.ask(
            2,
            Request::Answer {
                thread: s("t1"),
                ask: s("call_1"),
                answers: vec![vec![s("beta.txt")]],
                local: s("local-p2"),
            },
        );
        assert!(matches!(answer(&f.flush(), 2), Some(Err(e)) if e.contains("moved on")));
    }

    #[test]
    fn a_new_thread_is_written_at_once_and_asked_again_is_a_no_op() {
        let mut f = fixture("new");
        f.sub(ViewKey::Sidebar);
        f.flush();
        let id = s("7f3e0000-0000-4000-8000-000000000001");
        let req = new_thread(&id);
        f.ask(1, Request::NewThread(Box::new(req.clone())));
        let out = f.flush();
        let row = sidebar(&out)
            .unwrap()
            .threads
            .iter()
            .find(|t| t.thread.id == id)
            .unwrap()
            .clone();
        assert_eq!(
            (row.thread.state, row.activity),
            (ThreadState::Creating, Activity::Working)
        );
        let token = f.last_act();
        f.engine(Event::Done {
            action: Action::Retry(id.clone()),
            result: Ok(Outcome::Created(id.clone())),
            req: Some(token),
        });
        f.flush();
        f.ask(2, Request::NewThread(Box::new(req.clone())));
        assert_eq!(answer(&f.flush(), 2), Some(&Ok(Reply::Already)));
        assert_eq!(f.store().threads().unwrap().len(), 1);
        // The client must make the id.
        f.ask(
            3,
            Request::NewThread(Box::new(NewThread { id: None, ..req })),
        );
        assert!(matches!(answer(&f.flush(), 3), Some(Err(_))));
    }

    fn schedules_of(out: &[ServerMsg]) -> Option<&flotilla_proto::Schedules> {
        out.iter().rev().find_map(|m| match m {
            ServerMsg::Snap { view } => match view.as_ref() {
                View::Schedules(s) => Some(s),
                _ => None,
            },
            _ => None,
        })
    }

    /// Scheduled prompts through the protocol: kept with their next time and who set them,
    /// checked first, sent again harmlessly, paused and resumed, run now and deleted, each
    /// change in the view; a thread forgotten takes its schedules with it.
    #[test]
    fn schedules_are_set_paused_run_and_deleted_through_the_view() {
        use flotilla_proto::{ScheduleSpec, Target, When};
        let mut f = fixture("schedules");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.sub(ViewKey::Schedules);
        assert_eq!(schedules_of(&f.flush()).map(|s| s.schedules.len()), Some(0));
        let now = flotilla_core::util::now_ms();
        let spec = ScheduleSpec {
            id: s("s1"),
            prompt: s("Check the nightly CI"),
            target: Target::Thread(s("t1")),
            when: When::Daily { at: 9 * 60 },
            zone: s("Europe/London"),
        };
        f.ask(1, Request::PutSchedule(Box::new(spec.clone())));
        let out = f.flush();
        assert_eq!(answer(&out, 1), Some(&Ok(Reply::Done)));
        let kept = schedules_of(&out).unwrap().schedules[0].clone();
        let nine = flotilla_model::schedule::next_after(&spec.when, "Europe/London", now).unwrap();
        assert!(
            kept.next_ms >= nine
                && kept.next_ms
                    <= flotilla_model::schedule::next_after(
                        &spec.when,
                        "Europe/London",
                        flotilla_core::util::now_ms()
                    )
                    .unwrap()
        );
        assert_eq!(
            (kept.paused, kept.from.as_str(), kept.runs),
            (false, flotilla_core::util::machine_name().as_str(), 0)
        );
        assert!(
            f.cmds
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, Cmd::LookSchedules))
        );
        // Sent again (a drop): nothing changes.
        f.ask(2, Request::PutSchedule(Box::new(spec.clone())));
        let out = f.flush();
        assert_eq!(answer(&out, 2), Some(&Ok(Reply::Done)));
        assert!(schedules_of(&out).is_none());
        // Checked: a day apart, a thread that's there, a zone that's real, a model there is.
        let fresh = flotilla_proto::Fresh {
            environment: s("base"),
            project_id: None,
            provider: s("claude-code"),
            model: s("claude-sonnet-5-5"),
            effort: None,
            fast: false,
            machine_type: None,
            on_branch: None,
        };
        for (id, bad) in [
            (
                3,
                ScheduleSpec {
                    id: s("s2"),
                    target: Target::New(fresh.clone()),
                    when: When::Cron {
                        expr: s("*/30 * * * *"),
                    },
                    ..spec.clone()
                },
            ),
            (
                4,
                ScheduleSpec {
                    id: s("s2"),
                    target: Target::Thread(s("nope")),
                    ..spec.clone()
                },
            ),
            (
                5,
                ScheduleSpec {
                    id: s("s2"),
                    zone: s("Mars/Olympus"),
                    ..spec.clone()
                },
            ),
            (
                6,
                ScheduleSpec {
                    id: s("s2"),
                    target: Target::New(flotilla_proto::Fresh {
                        model: s("no-such-model"),
                        ..fresh.clone()
                    }),
                    ..spec.clone()
                },
            ),
        ] {
            f.ask(id, Request::PutSchedule(Box::new(bad)));
            assert!(matches!(answer(&f.flush(), id), Some(Err(_))), "{id}");
        }
        assert_eq!(f.store().schedules().unwrap().len(), 1);
        // Only the prompt changes: the next time stays.
        f.ask(
            7,
            Request::PutSchedule(Box::new(ScheduleSpec {
                prompt: s("Check CI and the docs"),
                ..spec.clone()
            })),
        );
        let edited = schedules_of(&f.flush()).unwrap().schedules[0].clone();
        assert_eq!(
            (edited.prompt.as_str(), edited.next_ms),
            ("Check CI and the docs", kept.next_ms)
        );
        assert!(edited.updated_ms > kept.updated_ms);
        // Paused: no next time; resumed: one again.
        f.ask(
            8,
            Request::PauseSchedule {
                schedule: s("s1"),
                paused: true,
            },
        );
        let paused = schedules_of(&f.flush()).unwrap().schedules[0].clone();
        assert!(paused.paused && paused.next_ms.is_none());
        f.ask(
            9,
            Request::PauseSchedule {
                schedule: s("s1"),
                paused: false,
            },
        );
        let resumed = schedules_of(&f.flush()).unwrap().schedules[0].clone();
        assert!(!resumed.paused && resumed.next_ms.is_some());
        // A one-off whose time has passed won't resume.
        let once = ScheduleSpec {
            id: s("s3"),
            when: When::Once {
                at_ms: now + 60_000,
            },
            ..spec.clone()
        };
        f.ask(10, Request::PutSchedule(Box::new(once)));
        f.flush();
        f.store()
            .set_schedule_paused("s3", true, None, now + 1)
            .unwrap();
        f.store()
            .put_schedule(
                &flotilla_proto::Schedule {
                    when: When::Once {
                        at_ms: now - 60_000,
                    },
                    updated_ms: now + 2,
                    ..f.store().schedule("s3").unwrap().unwrap()
                },
                true,
            )
            .unwrap();
        f.ask(
            11,
            Request::PauseSchedule {
                schedule: s("s3"),
                paused: false,
            },
        );
        assert!(matches!(answer(&f.flush(), 11), Some(Err(_))));
        // Every 4 hours into a thread: refused with why. One kept from before that was the
        // rule (paused for it) won't resume, nor keep a new prompt, until it's retimed.
        let often = ScheduleSpec {
            id: s("s4"),
            when: When::Cron {
                expr: s("0 */4 * * 1-5"),
            },
            ..spec.clone()
        };
        f.ask(15, Request::PutSchedule(Box::new(often.clone())));
        assert_eq!(
            answer(&f.flush(), 15),
            Some(&Err(s(flotilla_model::schedule::TOO_OFTEN)))
        );
        let kept = flotilla_proto::Schedule {
            paused: true,
            next_ms: None,
            ..f.store().schedule("s1").unwrap().unwrap()
        };
        f.store()
            .put_schedule(
                &flotilla_proto::Schedule {
                    id: s("s4"),
                    when: often.when.clone(),
                    ..kept
                },
                true,
            )
            .unwrap();
        f.ask(
            16,
            Request::PauseSchedule {
                schedule: s("s4"),
                paused: false,
            },
        );
        assert_eq!(
            answer(&f.flush(), 16),
            Some(&Err(s(flotilla_model::schedule::TOO_OFTEN)))
        );
        f.ask(
            17,
            Request::PutSchedule(Box::new(ScheduleSpec {
                prompt: s("Check CI twice"),
                ..often.clone()
            })),
        );
        assert_eq!(
            answer(&f.flush(), 17),
            Some(&Err(s(flotilla_model::schedule::TOO_OFTEN)))
        );
        f.ask(
            18,
            Request::PutSchedule(Box::new(ScheduleSpec {
                when: When::Daily { at: 9 * 60 },
                ..often
            })),
        );
        assert_eq!(answer(&f.flush(), 18), Some(&Ok(Reply::Done)));
        assert!(
            f.store()
                .schedule("s4")
                .unwrap()
                .is_some_and(|s| s.paused && s.when == When::Daily { at: 9 * 60 })
        );
        f.ask(19, Request::DeleteSchedule(s("s4")));
        f.flush();
        // Run now goes to the engine.
        f.ask(12, Request::RunSchedule(s("s1")));
        assert_eq!(answer(&f.flush(), 12), Some(&Ok(Reply::Done)));
        assert!(
            f.cmds
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, Cmd::RunSchedule(id) if id == "s1"))
        );
        f.ask(13, Request::DeleteSchedule(s("s3")));
        assert_eq!(schedules_of(&f.flush()).unwrap().schedules.len(), 1);
        // Forgetting the thread takes its schedule.
        f.ask(14, Request::Forget(s("t1")));
        assert_eq!(schedules_of(&f.flush()).unwrap().schedules.len(), 0);
    }

    /// A demo refuses what would reach a model or run later (a suggested name, a schedule),
    /// and its standing code pairs any number of devices; elsewhere it's no code at all.
    #[test]
    fn a_demo_refuses_a_model_and_schedules_and_pairs_with_its_code() {
        use flotilla_proto::{ScheduleSpec, Target, When};
        let mut f = fixture("demo");
        crate::demo::reset(&f.dir.join("db")).unwrap();
        let code = crate::demo::code(f.store()).unwrap();
        f.state.mode = Mode::Demo;
        f.ask(1, Request::SuggestName(s("t1")));
        let spec = ScheduleSpec {
            id: s("s1"),
            prompt: s("Check the nightly CI"),
            target: Target::Thread(s("t1")),
            when: When::Daily { at: 9 * 60 },
            zone: s("Europe/London"),
        };
        f.ask(2, Request::PutSchedule(Box::new(spec)));
        let out = f.flush();
        let refused = |what| Some(Err(crate::demo::refused(what)));
        assert_eq!(answer(&out, 1).cloned(), refused("suggested names"));
        assert_eq!(answer(&out, 2).cloned(), refused("schedules"));
        assert!(f.store().schedules().unwrap().is_empty());
        let pair = |state: &mut State| {
            let got = Arc::new(Mutex::new(None));
            let to = got.clone();
            let req = PairRequest {
                code: flotilla_proto::Secret(code.clone()),
                device_name: s("iPhone"),
                platform: s("ios"),
                scope: None,
            };
            state.take(In::Pair(
                req,
                None,
                None,
                Box::new(move |r| *to.lock().unwrap() = Some(r.is_ok())),
            ));
            got.lock().unwrap().take().unwrap()
        };
        assert!(pair(&mut f.state));
        assert!(pair(&mut f.state));
        assert_eq!(
            f.store()
                .devices(flotilla_core::util::now_ms())
                .unwrap()
                .len(),
            2
        );
        // Not a demo: it's no code.
        f.state.mode = Mode::Offline;
        assert!(!pair(&mut f.state));
    }

    /// Offline (dev: no engine), whatever needs the engine is refused at once, so no client
    /// spins on it; a new thread or a message is refused before it's written, so nothing shows
    /// as starting or sending; what the host does itself still works.
    #[test]
    fn offline_refuses_what_needs_the_engine_at_once() {
        let mut f = fixture("offline");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.state = restart(&f.dir.join("db"), None);
        f.clients.clear();
        f.connect();
        f.sub(ViewKey::Sidebar);
        f.flush();
        let id = s("7f3e0000-0000-4000-8000-0000000000ff");
        let send = Request::Send {
            thread: s("t1"),
            text: s("Hello"),
            files: vec![],
            local: s("local-offline"),
        };
        let asks = [
            Request::NewThread(Box::new(new_thread(&id))),
            send.clone(),
            Request::ReviewPrompt(s("t1")),
            Request::ListBranches(s("p1")),
            Request::Rename {
                thread: s("t1"),
                title: s("New"),
            },
            Request::ReadUsage,
            Request::SyncNow,
        ];
        let n = asks.len() as ReqId;
        for (i, req) in asks.into_iter().enumerate() {
            f.ask(i as ReqId + 1, req);
        }
        let out = f.flush();
        for i in 1..=n {
            assert_eq!(answer(&out, i), Some(&Err(s(OFFLINE))), "request {i}");
        }
        assert!(f.store().thread(&id).unwrap().is_none());
        assert!(f.store().sent_prompts("t1").unwrap().is_empty());
        // Asked again (a client unsure it went), it's refused again, not taken as done.
        f.ask(n + 1, send);
        assert_eq!(answer(&f.flush(), n + 1), Some(&Err(s(OFFLINE))));
        // The host's own writes go on.
        f.ask(n + 2, Request::Archive(s("t1")));
        f.ask(n + 3, Request::MarkUnread(s("t1")));
        let out = f.flush();
        assert_eq!(
            (answer(&out, n + 2), answer(&out, n + 3)),
            (Some(&Ok(Reply::Done)), Some(&Ok(Reply::Done)))
        );
        assert_eq!(activity(&out, "t1"), Some(Activity::Archived));
    }

    #[test]
    fn a_client_made_id_is_remembered_across_a_restart() {
        let mut f = fixture("receipts");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        let send = Request::Send {
            thread: s("t1"),
            text: s("Once"),
            files: vec![],
            local: s("local-once"),
        };
        f.ask(1, send.clone());
        f.flush();
        // boat took it: the placeholder has boat's id now, so only the receipt remembers.
        f.store().delete_sent_prompt("t1", "local-once").unwrap();
        // The server restarts; the client, not knowing whether it went, sends it again.
        let db = f.dir.join("db");
        f.state = restart(&db, Some(f.cmds.clone()));
        f.clients.clear();
        f.connect();
        f.ask(2, send.clone());
        assert_eq!(answer(&f.flush(), 2), Some(&Ok(Reply::Already)));
        assert!(f.store().sent_prompts("t1").unwrap().is_empty());
        // Back with no engine (offline, or a key that stopped working): what was taken before
        // is still `Already`, not refused (the client would put the prompt back as unsent).
        let id = s("7f3e0000-0000-4000-8000-0000000000aa");
        f.ask(3, Request::NewThread(Box::new(new_thread(&id))));
        f.flush();
        f.state = restart(&db, None);
        f.clients.clear();
        f.connect();
        f.ask(4, send);
        f.ask(5, Request::NewThread(Box::new(new_thread(&id))));
        let out = f.flush();
        assert_eq!(
            (answer(&out, 4), answer(&out, 5)),
            (Some(&Ok(Reply::Already)), Some(&Ok(Reply::Already)))
        );
    }

    #[test]
    fn writes_land_where_the_engine_reads_them_and_show_in_the_views() {
        let mut f = fixture("writes");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.store()
            .insert_thread(&Thread {
                state: ThreadState::Expired,
                ..thread("t2", None)
            })
            .unwrap();
        flotilla_core::orchestrate::set_parent(f.store(), "t2", "t1").unwrap();
        f.sub(ViewKey::Sidebar);
        f.sub(ViewKey::Settings);
        f.sub(ViewKey::Thread(s("t1")));
        f.flush();
        f.ask(1, Request::Set(Setting::BranchPrefix(s("my work"))));
        f.ask(2, Request::Set(Setting::IdleMinutes(45)));
        f.ask(3, Request::SetInstructions(s(" Use pnpm. ")));
        f.ask(
            4,
            Request::Set(Setting::Defaults(flotilla_proto::ThreadSettings {
                provider: s("codex"),
                model: s("gpt"),
                effort: None,
                fast: true,
            })),
        );
        let out = f.flush();
        let settings = out
            .iter()
            .find_map(|m| match m {
                ServerMsg::Snap { view } => match view.as_ref() {
                    View::Settings(s) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (
                settings.branch_prefix.as_str(),
                settings.idle_minutes,
                settings.instructions.as_str()
            ),
            ("my-work/", 45, "Use pnpm.")
        );
        assert_eq!(settings.defaults.unwrap().provider, "codex");
        assert_eq!(
            flotilla_core::lifecycle::branch_prefix(f.store()),
            "my-work/"
        );
        // Answers come after the views they changed.
        let last_view = out
            .iter()
            .rposition(|m| matches!(m, ServerMsg::Snap { .. }))
            .unwrap();
        let first_answer = out
            .iter()
            .position(|m| matches!(m, ServerMsg::Res { .. }))
            .unwrap();
        assert!(last_view < first_answer);
        // Archiving an expired thread needs no VM stopped: it's answered at once.
        f.ask(5, Request::Archive(s("t2")));
        let out = f.flush();
        assert_eq!(answer(&out, 5), Some(&Ok(Reply::Done)));
        assert!(f.store().is_archived("t2").unwrap());
        f.ask(6, Request::Unarchive(s("t2")));
        f.flush();
        assert!(!f.store().is_archived("t2").unwrap());
        // A message boat never got, let go of.
        f.store()
            .insert_sent_prompt("t1", "unsent-1", "prompt", "Lost")
            .unwrap();
        f.store().rebuild_messages("t1").unwrap();
        f.engine(Event::Changed(Changes::thread("t1")));
        assert!(
            detail(&f.flush(), "t1")
                .unwrap()
                .sent
                .iter()
                .any(|p| p.id == "unsent-1")
        );
        f.ask(
            7,
            Request::DiscardUnsent {
                thread: s("t1"),
                id: s("unsent-1"),
            },
        );
        assert!(detail(&f.flush(), "t1").unwrap().sent.is_empty());
        // Forgetting the expired child takes its card off the parent.
        f.ask(8, Request::Forget(s("t2")));
        let out = f.flush();
        assert!(detail(&out, "t1").unwrap().children.is_empty());
        assert!(
            sidebar(&out)
                .unwrap()
                .threads
                .iter()
                .all(|t| t.thread.id != "t2")
        );
    }

    #[test]
    fn an_action_answers_its_request_and_one_the_engine_started_is_a_note() {
        let mut f = fixture("done");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.sub(ViewKey::Sidebar);
        f.flush();
        f.ask(
            9,
            Request::Rename {
                thread: s("t1"),
                title: s("Renamed"),
            },
        );
        f.flush();
        let token = f.last_act();
        f.store().set_thread_title("t1", "Renamed").unwrap();
        f.engine(Event::Changed(Changes::thread("t1").with_sidebar()));
        f.engine(Event::Done {
            action: Action::Rename(s("t1"), s("Renamed")),
            result: Ok(Outcome::Done),
            req: Some(token),
        });
        let out = f.flush();
        assert!(matches!(out.first(), Some(ServerMsg::Snap { .. })));
        assert_eq!(answer(&out, 9), Some(&Ok(Reply::Done)));
        // A wake message the engine sent itself that failed: told as a note, as a request.
        f.engine(Event::Done {
            action: Action::Send {
                thread: s("t1"),
                text: s("[flotilla] done"),
                files: vec![],
                local: None,
            },
            result: Err(s("409")),
            req: None,
        });
        let out = f.flush();
        assert!(matches!(
            &out[..],
            [ServerMsg::Note(Note::Done {
                request: Request::Send { .. },
                result: Err(_)
            })]
        ));
        // A delegate's start is told as `thread.new` with the child's id.
        let req = Box::new(NewThread {
            provider: s("codex"),
            model: s("gpt"),
            prompt: s("Review"),
            id: None,
            parent: Some(s("t1")),
            ..new_thread("x")
        });
        f.engine(Event::Done {
            action: Action::Launch {
                thread: s("t9"),
                req,
                local: s("local-9"),
            },
            result: Err(s("no VM")),
            req: None,
        });
        let out = f.flush();
        assert!(
            matches!(&out[..], [ServerMsg::Note(Note::Done { request: Request::NewThread(r), .. })] if r.id.as_deref() == Some("t9"))
        );
    }

    #[test]
    fn live_text_goes_only_to_a_thread_on_screen() {
        let mut f = fixture("live");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.engine(Event::Live {
            thread: s("t1"),
            partials: vec![],
        });
        assert!(f.flush().is_empty());
        f.sub(ViewKey::Thread(s("t1")));
        f.flush();
        f.engine(Event::Live {
            thread: s("t1"),
            partials: vec![],
        });
        assert!(matches!(&f.flush()[..], [ServerMsg::Live { .. }]));
    }

    #[test]
    fn answers_go_to_who_asked_notes_to_everyone_and_live_text_to_who_watches() {
        let mut f = fixture("clients");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        let b = f.connect();
        f.sub(ViewKey::Sidebar);
        f.sub_on(b, ViewKey::Sidebar);
        f.sub_on(b, ViewKey::Thread(s("t1")));
        f.flush_all();
        // A writes; both see the change, only A the answer.
        f.ask(1, Request::MarkUnread(s("t1")));
        let [a_out, b_out] = <[_; 2]>::try_from(f.flush_all()).unwrap();
        assert_eq!(answer(&a_out, 1), Some(&Ok(Reply::Done)));
        assert_eq!(answer(&b_out, 1), None);
        // Marked unread on A while open on B: it stays unread there too.
        assert_eq!(activity(&a_out, "t1"), Some(Activity::Unread));
        assert_eq!(activity(&b_out, "t1"), Some(Activity::Unread));
        // A note goes to both, with its alert; live text only to B, which has the thread open.
        f.engine(Event::TurnFinished(s("t1")));
        f.engine(Event::Live {
            thread: s("t1"),
            partials: vec![],
        });
        let [a_out, b_out] = <[_; 2]>::try_from(f.flush_all()).unwrap();
        assert!(matches!(
            &a_out[..],
            [
                ServerMsg::Note(Note::TurnFinished(_)),
                ServerMsg::Note(Note::Alert { .. })
            ]
        ));
        assert!(matches!(
            &b_out[..],
            [
                ServerMsg::Note(Note::TurnFinished(_)),
                ServerMsg::Note(Note::Alert { .. }),
                ServerMsg::Live { .. }
            ]
        ));
        // A list of repos asked for by B comes only to B.
        f.ask_on(b, 2, Request::ListRepos);
        f.flush_all();
        let token = f.cmds.lock().unwrap().iter().rev().find_map(|c| match c {
            Cmd::ListRepos { req } => *req,
            _ => None,
        });
        f.engine(Event::Repos {
            result: Ok(vec![]),
            done: true,
            login: None,
            req: token,
        });
        let [a_out, b_out] = <[_; 2]>::try_from(f.flush_all()).unwrap();
        assert!(a_out.is_empty());
        assert!(matches!(
            &b_out[..],
            [ServerMsg::Progress {
                id: 2,
                note: Note::Repos { done: true, .. }
            }]
        ));
    }

    #[test]
    fn the_engine_polls_the_threads_open_anywhere() {
        let mut f = fixture("open");
        for id in ["t1", "t2"] {
            f.store().insert_thread(&thread(id, None)).unwrap();
        }
        let b = f.connect();
        f.sub(ViewKey::Thread(s("t1")));
        f.sub_on(b, ViewKey::Thread(s("t2")));
        assert_eq!(f.opened(), Some(vec![s("t1"), s("t2")]));
        f.sub_on(b, ViewKey::Thread(s("t1")));
        f.unsub(ViewKey::Thread(s("t1")));
        // Still open on B.
        assert_eq!(f.opened(), Some(vec![s("t1"), s("t2")]));
        // B goes: nothing is open.
        let conn = f.clients[b].conn;
        f.state.take(In::Gone(conn));
        assert_eq!(f.opened(), Some(vec![]));
    }

    /// A connection that says hello again as another session leaves the first away, as if it
    /// had closed: its views aren't kept open, and it's let go in time.
    #[test]
    fn a_second_hello_on_one_connection_retires_the_first_session() {
        let mut f = fixture("rehello");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.sub(ViewKey::Thread(s("t1")));
        assert_eq!(f.opened(), Some(vec![s("t1")]));
        let conn = f.clients[0].conn;
        let hello = Hello {
            proto: PROTOCOL,
            app: s("test"),
            version: s("0"),
            platform: s("test"),
            session: s("another"),
            epoch: None,
            subs: vec![],
            got: None,
            scope: None,
        };
        f.state.take(In::From(conn, Up::Hello(hello)));
        assert_eq!(f.opened(), Some(vec![]));
        let first = &f.state.sessions["session-0"];
        assert!(first.conn.is_none() && first.subs.is_empty() && first.gone_at.is_some());
        f.state.take(In::Gone(conn));
        assert!(
            f.state
                .sessions
                .values()
                .all(|s| s.conn.is_none() && s.gone_at.is_some())
        );
    }

    /// A phone that asked for something, then spent a few minutes in the background, is told
    /// how it went when it's back, not that it was lost.
    #[test]
    fn a_client_away_for_minutes_still_gets_its_answers() {
        let mut f = fixture("away");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.flush();
        let epoch = f.state.epoch.clone();
        let session = f.clients[0].session.clone();
        f.ask(1, Request::Archive(s("t1")));
        f.flush();
        let token = f.last_act();
        let conn = f.clients[0].conn;
        f.state.take(In::Gone(conn));
        f.engine(Event::Done {
            action: Action::Archive(s("t1")),
            result: Ok(Outcome::Done),
            req: Some(token),
        });
        f.flush();
        // Five minutes on.
        let ago = Instant::now()
            .checked_sub(Duration::from_secs(5 * 60))
            .unwrap();
        let away = f.state.sessions.get_mut(&session).unwrap();
        away.gone_at = Some(ago);
        away.log.iter_mut().for_each(|(_, at, _)| *at = ago);
        f.flush();
        let c = f.connect_as(&session, Some(epoch), vec![], None);
        let raw = f.flush_raw(c);
        assert!(
            matches!(&raw[0], Down::Welcome(w) if w.resumed && w.seen == Some(1)),
            "{raw:?}"
        );
        assert!(
            raw.iter().any(|d| matches!(
                d,
                Down::Res {
                    id: 1,
                    result: Ok(Reply::Done),
                    ..
                }
            )),
            "{raw:?}"
        );
    }

    #[test]
    fn a_client_that_reconnects_gets_what_it_missed() {
        let mut f = fixture("resume");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.sub(ViewKey::Sidebar);
        f.sub(ViewKey::Thread(s("t1")));
        f.flush();
        let epoch = f.state.epoch.clone();
        let session = f.clients[0].session.clone();
        // It asks, then drops before the answer.
        f.ask(
            1,
            Request::Rename {
                thread: s("t1"),
                title: s("Renamed"),
            },
        );
        f.flush();
        let token = f.last_act();
        let conn = f.clients[0].conn;
        f.state.take(In::Gone(conn));
        // Meanwhile: the answer, a note, and changes to the views it held.
        f.store().set_thread_title("t1", "Renamed").unwrap();
        f.store()
            .insert_sent_prompt("t1", "boat-1", "prompt", "Hello")
            .unwrap();
        f.store().rebuild_messages("t1").unwrap();
        f.engine(Event::Changed(Changes::thread("t1").with_sidebar()));
        f.engine(Event::Done {
            action: Action::Rename(s("t1"), s("Renamed")),
            result: Ok(Outcome::Done),
            req: Some(token),
        });
        f.engine(Event::TurnFinished(s("t1")));
        f.flush();
        // Nothing open while it's away; the views it held were kept.
        assert_eq!(f.opened(), Some(vec![]));
        // It reconnects, holding what it had: only the deltas it missed come, then the answer
        // and the note, then that it's caught up.
        let mut old = std::mem::replace(&mut f.clients[0].replica, Replica::new());
        let held = old.held();
        let c = f.connect_as(&session, Some(epoch.clone()), held, None);
        f.clients[c].replica = std::mem::take(&mut old);
        let raw = f.flush_raw(c);
        assert!(matches!(&raw[0], Down::Welcome(w) if w.resumed && w.seen == Some(1)));
        assert!(raw.iter().any(|d| matches!(d, Down::Delta { .. })));
        assert!(
            !raw.iter().any(|d| matches!(d, Down::Snap { .. })),
            "{raw:?}"
        );
        let tail: Vec<&Down> = raw
            .iter()
            .filter(|d| !matches!(d, Down::Delta { .. } | Down::Welcome(_)))
            .collect();
        assert!(
            matches!(
                &tail[..],
                [
                    Down::Res {
                        id: 1,
                        result: Ok(Reply::Done),
                        n: Some(1)
                    },
                    Down::Note {
                        note: Note::TurnFinished(_),
                        n: Some(2)
                    },
                    Down::Note {
                        note: Note::Alert { .. },
                        n: Some(3)
                    },
                    Down::Synced
                ]
            ),
            "{tail:?}"
        );
        let View::Thread(d) = f.clients[c]
            .replica
            .view(&ViewKey::Thread(s("t1")))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(d.messages.len(), 1);
        assert_eq!(f.opened(), Some(vec![s("t1")]));
        // From another server run: everything comes whole.
        let held = f.clients[c].replica.held();
        let c2 = f.connect_as("another", Some(s("an-old-epoch")), held, None);
        let raw = f.flush_raw(c2);
        assert!(matches!(&raw[0], Down::Welcome(w) if !w.resumed));
        assert_eq!(
            raw.iter()
                .filter(|d| matches!(d, Down::Snap { .. }))
                .count(),
            2
        );
        assert!(!raw.iter().any(|d| matches!(d, Down::Delta { .. })));
        // A gap the ring no longer covers: whole too.
        let held = vec![Held {
            view: ViewKey::Thread(s("t1")),
            rev: Some(1),
        }];
        let c3 = f.connect_as("third", Some(epoch), held, None);
        let raw = f.flush_raw(c3);
        assert!(raw.iter().any(|d| matches!(
            d,
            Down::Snap {
                view: ViewKey::Thread(_),
                ..
            }
        )));
    }

    #[test]
    fn an_answer_sent_as_the_connection_dropped_comes_again() {
        let mut f = fixture("again");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        let session = f.clients[0].session.clone();
        f.flush_raw(0);
        // Answered and handed to the socket, which drops before the client reads it.
        f.ask(1, Request::MarkUnread(s("t1")));
        let raw = f.flush_raw(0);
        assert!(
            matches!(
                &raw[..],
                [Down::Res {
                    id: 1,
                    n: Some(1),
                    ..
                }]
            ),
            "{raw:?}"
        );
        let conn = f.clients[0].conn;
        f.state.take(In::Gone(conn));
        f.engine(Event::TurnFinished(s("t1")));
        f.flush_raw(0);
        // Back, having got nothing: the answer, the note and its alert come again, in order.
        let c = f.connect_as(&session, None, vec![], None);
        let raw = f.flush_raw(c);
        assert!(
            matches!(&raw[..], [Down::Welcome(w), Down::Res { id: 1, n: Some(1), .. }, Down::Note { n: Some(2), .. }, Down::Note { note: Note::Alert { .. }, n: Some(3) }, Down::Synced] if w.resumed && w.seen == Some(1)),
            "{raw:?}"
        );
        // Back again having got them: nothing more.
        let conn = f.clients[c].conn;
        f.state.take(In::Gone(conn));
        let c = f.connect_as(&session, None, vec![], Some(3));
        let raw = f.flush_raw(c);
        assert!(
            matches!(&raw[..], [Down::Welcome(w), Down::Synced] if w.resumed),
            "{raw:?}"
        );
        // Claiming more than was ever sent can't be resumed: a fresh session.
        let conn = f.clients[c].conn;
        f.state.take(In::Gone(conn));
        let c = f.connect_as(&session, None, vec![], Some(9));
        assert!(matches!(&f.flush_raw(c)[..], [Down::Welcome(w), Down::Synced] if !w.resumed));
    }

    /// Each alert's id, as each client got it (in order).
    fn alerts(out: &[ServerMsg]) -> Vec<(String, flotilla_proto::Alert)> {
        out.iter()
            .filter_map(|m| match m {
                ServerMsg::Note(Note::Alert { id, alert }) => Some((id.clone(), alert.clone())),
                _ => None,
            })
            .collect()
    }

    /// Forgetting a thread leaves its VM as it is, so its public links aren't dropped with it:
    /// each stays, due now, for the worker to end through boat, and then goes. One whose VM is
    /// gone is forgotten all the same, its link going once boat says so.
    #[test]
    fn forgetting_a_thread_keeps_its_public_links_till_boat_ends_them() {
        use flotilla_core::public;
        let mut f = fixture("forget-public");
        let (boat, held) = public::testing::serve();
        let now = flotilla_core::util::now_ms();
        for id in ["t1", "t2"] {
            f.store().insert_thread(&thread(id, None)).unwrap();
            public::make_public(
                &boat,
                f.store(),
                &thread(id, None),
                3000,
                flotilla_proto::PublicFor::Hour,
                now,
            )
            .unwrap();
        }
        held.lock().unwrap().gone.insert(s("bx_t2"));
        f.connect();
        f.flush();
        f.ask(1, Request::Forget(s("t1")));
        f.ask(2, Request::Forget(s("t2")));
        let out = f.flush();
        assert_eq!(
            (answer(&out, 1), answer(&out, 2)),
            (Some(&Ok(Reply::Done)), Some(&Ok(Reply::Done)))
        );
        assert!(
            f.store().thread("t1").unwrap().is_none() && f.store().thread("t2").unwrap().is_none()
        );
        let now = flotilla_core::util::now_ms();
        let kept = public::records(f.store(), None);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept.iter().all(|r| r.due(now)), "{kept:?}");
        assert!(held.lock().unwrap().public("bx_t1", 3000));
        public::sweep(&boat, f.store(), now);
        assert!(!held.lock().unwrap().public("bx_t1", 3000));
        assert!(public::records(f.store(), None).is_empty());
    }

    /// A thread archived as it started: the archive's action is queued all the same (to stop
    /// the VM once its start has made it), and the start that then didn't happen alerts no
    /// one, yours or a delegate's.
    #[test]
    fn a_start_archived_as_it_started_alerts_no_one() {
        let mut f = fixture("archived-start");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.connect();
        f.connect();
        f.flush_all();
        let id = s("7f3e0000-0000-4000-8000-0000000000ab");
        f.ask(3, Request::NewThread(Box::new(new_thread(&id))));
        f.flush_all();
        let token = f.last_act();
        f.ask(4, Request::Archive(id.clone()));
        assert!(
            matches!(f.cmds.lock().unwrap().last(), Some(Cmd::Act { action: Action::Archive(t), .. }) if *t == id),
            "its VM is stopped once made"
        );
        assert!(f.store().is_archived(&id).unwrap());
        let archived = flotilla_core::lifecycle::SCHEDULED_ARCHIVED.to_string();
        f.engine(Event::Done {
            action: Action::Launch {
                thread: id.clone(),
                req: Box::new(new_thread(&id)),
                local: s("local-3"),
            },
            result: Err(archived.clone()),
            req: Some(token),
        });
        let out = f.flush_all();
        // Nor an error for the app that asked to show: the thread says why.
        assert!(
            matches!(answer(&out[0], 3), Some(Ok(Reply::Created(t))) if *t == id),
            "{:?}",
            answer(&out[0], 3)
        );
        assert!(out.iter().all(|o| alerts(o).is_empty()), "{out:?}");
        let req = Box::new(NewThread {
            parent: Some(s("t1")),
            ..new_thread("child")
        });
        f.engine(Event::Done {
            action: Action::Launch {
                thread: s("child"),
                req,
                local: s("local-9"),
            },
            result: Err(archived),
            req: None,
        });
        assert!(f.flush_all().iter().all(|o| alerts(o).is_empty()));
        // Try again brings it back as it's asked, so an archive after it stays (its start,
        // seeing that, sends nothing).
        f.ask(5, Request::Retry(id.clone()));
        assert!(!f.store().is_archived(&id).unwrap());
        assert!(
            matches!(f.cmds.lock().unwrap().last(), Some(Cmd::Act { action: Action::Retry(t), .. }) if *t == id)
        );
        f.ask(6, Request::Archive(id.clone()));
        assert!(f.store().is_archived(&id).unwrap());
    }

    /// A thread stopped as it started: the start that then didn't happen alerts no one, and
    /// Try again ends the stop as it's asked, so a Stop after it stays.
    #[test]
    fn a_start_stopped_as_it_started_alerts_no_one() {
        let mut f = fixture("stopped-start");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.connect();
        f.connect();
        f.flush_all();
        let id = s("7f3e0000-0000-4000-8000-0000000000cd");
        f.ask(3, Request::NewThread(Box::new(new_thread(&id))));
        f.flush_all();
        let token = f.last_act();
        f.store()
            .kv_set(
                &flotilla_core::lifecycle::stopped_by_user_key(&id),
                "stopped 1 2",
            )
            .unwrap();
        let stopped = flotilla_core::lifecycle::START_STOPPED.to_string();
        f.engine(Event::Done {
            action: Action::Launch {
                thread: id.clone(),
                req: Box::new(new_thread(&id)),
                local: s("local-3"),
            },
            result: Err(stopped),
            req: Some(token),
        });
        let out = f.flush_all();
        assert!(
            matches!(answer(&out[0], 3), Some(Ok(Reply::Created(t))) if *t == id),
            "{:?}",
            answer(&out[0], 3)
        );
        assert!(out.iter().all(|o| alerts(o).is_empty()), "{out:?}");
        f.ask(5, Request::Retry(id.clone()));
        assert!(
            !flotilla_core::lifecycle::stopped_by_user(f.store(), &id),
            "Try again ends the stop as it's asked"
        );
        assert!(
            matches!(f.cmds.lock().unwrap().last(), Some(Cmd::Act { action: Action::Retry(t), .. }) if *t == id)
        );
    }

    #[test]
    fn alerts_go_to_every_client_once_with_the_rule_phones_get() {
        let mut f = fixture("alerts");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.store().insert_thread(&thread("child", None)).unwrap();
        flotilla_core::orchestrate::set_parent(f.store(), "child", "t1").unwrap();
        let b = f.connect();
        let session = f.clients[0].session.clone();
        f.flush_all();
        // A finished turn: one alert, the same id on every client, after the note.
        f.engine(Event::TurnFinished(s("t1")));
        let [a_out, b_out] = <[_; 2]>::try_from(f.flush_all()).unwrap();
        let (a, b_alerts) = (alerts(&a_out), alerts(&b_out));
        assert_eq!(a.len(), 1);
        assert_eq!(a, b_alerts);
        assert_eq!(
            (a[0].1.kind, a[0].1.thread.as_deref(), a[0].1.title.as_str()),
            (
                flotilla_proto::AlertKind::TurnFinished,
                Some("t1"),
                "Thread t1"
            )
        );
        assert!(a[0].0.starts_with(&f.state.epoch));
        // A delegated thread's turn: its parent hears, not you; a note that isn't news, nothing.
        f.engine(Event::TurnFinished(s("child")));
        f.engine(Event::Summaries(Ok(s("now"))));
        assert!(f.flush_all().iter().all(|out| alerts(out).is_empty()));
        // Your own start: written at once, then its VM fails. You're answered; every client is
        // alerted.
        let id = s("7f3e0000-0000-4000-8000-0000000000aa");
        f.ask(3, Request::NewThread(Box::new(new_thread(&id))));
        f.flush_all();
        let token = f.last_act();
        assert!(matches!(
            f.cmds.lock().unwrap().last(),
            Some(Cmd::Act {
                action: Action::Launch { .. },
                ..
            })
        ));
        f.engine(Event::Done {
            action: Action::Launch {
                thread: id.clone(),
                req: Box::new(new_thread(&id)),
                local: s("local-3"),
            },
            result: Err(s("no VM")),
            req: Some(token),
        });
        let [a_out, b_out] = <[_; 2]>::try_from(f.flush_all()).unwrap();
        assert!(matches!(answer(&a_out, 3), Some(Err(_))));
        assert_eq!(
            alerts(&b_out)
                .iter()
                .map(|(_, a)| (a.kind, a.thread.clone()))
                .collect::<Vec<_>>(),
            [(flotilla_proto::AlertKind::StartFailed, Some(id.clone()))]
        );
        // A retry that failed too.
        f.ask(1, Request::Retry(s("t1")));
        f.flush_all();
        let token = f.last_act();
        f.engine(Event::Done {
            action: Action::Retry(s("t1")),
            result: Err(s("boat said 402")),
            req: Some(token),
        });
        let [a_out, b_out] = <[_; 2]>::try_from(f.flush_all()).unwrap();
        assert!(matches!(answer(&a_out, 1), Some(Err(_))) && answer(&b_out, 1).is_none());
        let failed = alerts(&b_out);
        assert_eq!(failed.len(), 1);
        assert_eq!(
            (failed[0].1.kind, failed[0].1.body.as_str()),
            (
                flotilla_proto::AlertKind::StartFailed,
                "Couldn't start: boat said 402"
            )
        );
        assert_ne!(failed[0].0, a[0].0);
        // A delegate whose VM didn't start: its parent waits on it, so you're told.
        let req = Box::new(NewThread {
            parent: Some(s("t1")),
            ..new_thread("child")
        });
        f.engine(Event::Done {
            action: Action::Launch {
                thread: s("child"),
                req,
                local: s("local-9"),
            },
            result: Err(s("no VM")),
            req: None,
        });
        let out = f.flush_all().remove(b);
        assert_eq!(
            alerts(&out)
                .iter()
                .map(|(_, a)| (a.kind, a.thread.clone()))
                .collect::<Vec<_>>(),
            [(flotilla_proto::AlertKind::StartFailed, Some(s("child")))]
        );
        // A reconnect that got everything is sent no alert again; one that missed the last
        // gets only that one, with the id it had (so a device that did show it won't again).
        let conn = f.clients[0].conn;
        f.state.take(In::Gone(conn));
        let got = f.state.sessions.get(&session).map(|s| s.last).unwrap();
        let c = f.connect_as(&session, None, vec![], Some(got));
        assert!(!f.flush_raw(c).iter().any(|d| matches!(
            d,
            Down::Note {
                note: Note::Alert { .. },
                ..
            }
        )));
        let conn = f.clients[c].conn;
        f.state.take(In::Gone(conn));
        let c = f.connect_as(&session, None, vec![], Some(got - 2));
        let again: Vec<String> = f
            .flush_raw(c)
            .into_iter()
            .filter_map(|d| match d {
                Down::Note {
                    note: Note::Alert { id, .. },
                    ..
                } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(again.len(), 1);
        assert_eq!(
            again,
            alerts(&out)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn what_was_out_when_a_session_started_afresh_is_answered_once() {
        let mut f = fixture("afresh");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        let session = f.clients[0].session.clone();
        f.ask(
            1,
            Request::Rename {
                thread: s("t1"),
                title: s("Renamed"),
            },
        );
        f.flush();
        let rename = f.last_act();
        let send = Request::Send {
            thread: s("t1"),
            text: s("Hi"),
            files: vec![],
            local: s("local-afresh"),
        };
        f.ask(2, send.clone());
        f.flush();
        let sent = f.last_act();
        let conn = f.clients[0].conn;
        f.state.take(In::Gone(conn));
        // Away long enough to miss more than is kept: it can't resume.
        for _ in 0..=AWAY_MAX {
            f.engine(Event::TurnFinished(s("t1")));
        }
        f.flush();
        let c = f.connect_as(&session, None, vec![], None);
        assert!(matches!(&f.flush_raw(c)[..], [Down::Welcome(w), Down::Synced] if !w.resumed));
        // The client has failed the rename itself and sends the message again (its id is its own).
        f.ask_on(c, 2, send);
        f.engine(Event::Done {
            action: Action::Rename(s("t1"), s("Renamed")),
            result: Ok(Outcome::Done),
            req: Some(rename),
        });
        f.engine(Event::Done {
            action: Action::Send {
                thread: s("t1"),
                text: s("Hi"),
                files: vec![],
                local: None,
            },
            result: Ok(Outcome::Sent(flotilla_proto::Sent::Prompt(s("p1")))),
            req: Some(sent),
        });
        let raw = f.flush_raw(c);
        let answers: Vec<ReqId> = raw
            .iter()
            .filter_map(|d| {
                if let Down::Res { id, .. } = d {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(answers, [2], "{raw:?}");
    }

    #[test]
    fn a_hello_from_another_protocol_is_refused() {
        let mut f = fixture("proto");
        let got: Arc<Mutex<Vec<Down>>> = Arc::default();
        let sink = got.clone();
        f.state.take(In::Connect(
            7,
            Outbox::new(move |m| {
                sink.lock().unwrap().push(m);
                true
            }),
            Peer::Local,
        ));
        let hello = Hello {
            proto: PROTOCOL + 1,
            app: s("x"),
            version: s("9"),
            platform: s("x"),
            session: s("x"),
            epoch: None,
            subs: vec![],
            got: None,
            scope: None,
        };
        f.state.take(In::From(7, Up::Hello(hello)));
        assert!(matches!(&got.lock().unwrap()[..], [Down::Error { .. }]));
        // And what it sends after is ignored.
        f.state.take(In::From(
            7,
            Up::Req {
                id: 1,
                req: Request::SyncNow,
            },
        ));
        f.state.flush();
        assert_eq!(got.lock().unwrap().len(), 1);
    }

    #[test]
    fn phones_register_for_notifications_and_get_the_notes_that_push() {
        let mut f = fixture("push");
        let jobs: Arc<Mutex<Vec<flotilla_core::push::Job>>> = Arc::default();
        let sink = jobs.clone();
        f.state.push = Some(flotilla_core::push::Pusher::with(move |j| {
            sink.lock().unwrap().push(j)
        }));
        f.store().insert_thread(&thread("t1", None)).unwrap();
        let (phone, _) = paired(&f, "iPhone");
        let c = f.connect_peer("phone", Peer::Device(phone.clone()), None, vec![], None);
        f.flush_raw(c);
        let token = |t: &str| {
            Request::RegisterPush(flotilla_proto::PushToken {
                token: flotilla_proto::Secret(t.into()),
                env: s("sandbox"),
                topic: s("com.useflotilla.flotilla"),
                relay: None,
            })
        };
        // This machine isn't a phone; a token that isn't one is refused.
        f.ask(1, token(&"ab".repeat(32)));
        assert!(
            matches!(&f.flush()[..], [ServerMsg::Res { id: 1, result: Err(e) }] if e.contains("paired devices"))
        );
        f.ask_on(c, 2, token("nope"));
        f.ask_on(c, 3, token(&"ab".repeat(32)));
        let raw = f.flush_raw(c);
        assert!(
            raw.iter().any(|d| matches!(
                d,
                Down::Res {
                    id: 2,
                    result: Err(_),
                    ..
                }
            )),
            "{raw:?}"
        );
        assert!(
            raw.iter().any(|d| matches!(
                d,
                Down::Res {
                    id: 3,
                    result: Ok(Reply::Push(flotilla_proto::PushMode { relay: None })),
                    ..
                }
            )),
            "{raw:?}"
        );
        assert_eq!(flotilla_core::push::targets(f.store()).len(), 1);
        // What the sending thread checks a queued notification against follows.
        let first = flotilla_core::push::targets(f.store()).remove(0);
        let live = |t: &flotilla_core::push::Target, f: &Fixture| {
            f.state.push.as_ref().unwrap().registrations.current(t)
        };
        assert!(live(&first, &f));
        // A turn that finished goes to it, with this server's id; a note that isn't pushed doesn't.
        f.engine(Event::TurnFinished(s("t1")));
        f.engine(Event::Summaries(Ok(s("now"))));
        f.flush();
        {
            let jobs = jobs.lock().unwrap();
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].kind, "turn_finished");
            assert_eq!(
                jobs[0]
                    .targets
                    .iter()
                    .map(|t| t.device.as_str())
                    .collect::<Vec<_>>(),
                [phone.as_str()]
            );
            assert!(
                jobs[0].payload.contains(r#""server":"srv_test""#)
                    && jobs[0].payload.contains(r#""thread":"t1""#),
                "{}",
                jobs[0].payload
            );
        }
        // APNs says the token is gone: forgotten.
        f.state.take(In::PushGone(phone.clone(), "ab".repeat(32)));
        assert!(flotilla_core::push::targets(f.store()).is_empty());
        // Unregistering, and revoking the device, leave nothing to send to.
        f.ask_on(c, 4, token(&"cd".repeat(32)));
        f.flush_raw(c);
        f.ask_on(c, 5, Request::UnregisterPush);
        f.flush_raw(c);
        assert!(flotilla_core::push::targets(f.store()).is_empty());
        assert!(!live(&first, &f));
        f.ask_on(c, 6, token(&"cd".repeat(32)));
        f.flush_raw(c);
        assert_eq!(flotilla_core::push::targets(f.store()).len(), 1);
        let second = flotilla_core::push::targets(f.store()).remove(0);
        assert!(live(&second, &f));
        f.ask(7, Request::RevokeDevice(phone.clone()));
        f.flush();
        assert!(flotilla_core::push::targets(f.store()).is_empty());
        assert!(
            !live(&second, &f),
            "a revoked device's queued notifications don't go"
        );
        assert!(f.store().kv_with_prefix("push_token:").unwrap().is_empty());
        f.engine(Event::TurnFinished(s("t1")));
        f.flush();
        assert_eq!(jobs.lock().unwrap().len(), 1);
    }

    #[test]
    fn through_the_relay_only_the_phones_that_registered_with_it_and_off_sends_nothing() {
        use flotilla_core::push::{self, Mode};
        let key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
        let (relay, sent) = push::testing::fake_relay(Vec::<u16>::new());
        let mut f = fixture("relay");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        // Off (`flotilla setup --no-push-relay`) with no key: no sender at all.
        f.store().kv_set(push::RELAY_KEY, "off").unwrap();
        f.state.push_as(push::mode_given(None, Some(f.store())));
        assert!(f.state.push.is_none());
        let (phone, _) = paired(&f, "iPhone");
        let (older, _) = paired(&f, "Old iPhone");
        let c = f.connect_peer("phone", Peer::Device(phone.clone()), None, vec![], None);
        let o = f.connect_peer("older", Peer::Device(older.clone()), None, vec![], None);
        f.flush_raw(c);
        f.flush_raw(o);
        let register = |token: &str, relay: Option<flotilla_proto::PushRelay>| {
            Request::RegisterPush(flotilla_proto::PushToken {
                token: flotilla_proto::Secret(token.into()),
                env: s("production"),
                topic: s("com.useflotilla.flotilla"),
                relay,
            })
        };
        f.ask_on(
            c,
            1,
            register(
                &"ab".repeat(32),
                Some(flotilla_proto::PushRelay {
                    url: relay.clone(),
                    handle: s("giLjuhbbFalUBROEtNqwBg"),
                    secret: flotilla_proto::Secret("S".repeat(43)),
                    key: flotilla_proto::Secret(key.into()),
                }),
            ),
        );
        f.ask_on(o, 2, register(&"cd".repeat(32), None));
        // Off: the phone hears this server uses no relay.
        let answered = |raw: Vec<Down>, id: u64| {
            raw.into_iter().find_map(|d| match d {
                Down::Res {
                    id: got, result, ..
                } if got == id => Some(result),
                _ => None,
            })
        };
        assert_eq!(
            answered(f.flush_raw(c), 1),
            Some(Ok(Reply::Push(flotilla_proto::PushMode { relay: None })))
        );
        f.flush_raw(o);
        assert_eq!(push::targets(f.store()).len(), 2);
        f.engine(Event::TurnFinished(s("t1")));
        f.flush();
        std::thread::sleep(Duration::from_millis(300));
        assert!(sent.lock().unwrap().is_empty(), "off sends nothing");

        // Through the relay: the phone that registered with it gets the turn, sealed for it,
        // and a phone registering hears which relay.
        f.state.push_as(Ok(Some(Mode::Relay(relay.clone()))));
        assert!(f.state.push.is_some());
        f.ask_on(o, 3, register(&"cd".repeat(32), None));
        assert_eq!(
            answered(f.flush_raw(o), 3),
            Some(Ok(Reply::Push(flotilla_proto::PushMode {
                relay: Some(relay.clone())
            })))
        );
        f.engine(Event::TurnFinished(s("t1")));
        f.flush();
        let until = Instant::now() + Duration::from_secs(5);
        while sent.lock().unwrap().is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));
        let sent = sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["handle"], "giLjuhbbFalUBROEtNqwBg");
        // The key above: bytes 0 to 31.
        let raw: [u8; 32] = std::array::from_fn(|i| i as u8);
        let opened = push::open(&raw, sent[0]["ciphertext"].as_str().unwrap()).unwrap();
        assert!(
            opened.contains(r#""server":"srv_test""#) && opened.contains(r#""thread":"t1""#),
            "{opened}"
        );
    }

    /// A device paired as an agent in the fixture's database: its id.
    fn paired_agent(f: &Fixture, name: &str) -> String {
        let now = flotilla_core::util::now_ms();
        let code = f
            .store()
            .mint_code_as(flotilla_core::devices::CODE_TTL, "cli", Scope::Agent, now)
            .unwrap();
        f.store()
            .pair_device(&code.code, name, "macos", None, now)
            .unwrap()
            .device
    }

    /// An agent's requests: those outside its scope are refused before anything is done (the
    /// stand-in engine is asked nothing, the database is as it was), and those inside go on as
    /// anyone's do. A connection on this machine that asks for an agent's scope is held to it
    /// too; one that doesn't isn't.
    #[test]
    fn an_agent_is_refused_everything_outside_its_scope() {
        let mut f = fixture("agent-scope");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        let me = paired_agent(&f, "Claude Code");
        let (phone, _) = paired(&f, "iPhone");
        let agent = f.connect_peer("agent", Peer::Agent(me.clone()), None, vec![], None);
        let narrowed = f.connect_scoped(
            "narrowed",
            Peer::Local,
            Some(Scope::Agent),
            None,
            vec![],
            None,
        );
        f.flush_all();
        let forbidden = [
            Request::Delete(s("t1")),
            Request::Forget(s("t1")),
            Request::MergePr(s("t1")),
            Request::MarkReady(s("t1")),
            Request::Unarchive(s("t1")),
            Request::Stop(s("t1")),
            Request::Rename {
                thread: s("t1"),
                title: s("x"),
            },
            Request::MarkViewed(s("t1")),
            Request::Set(Setting::IdleMinutes(5)),
            Request::SetInstructions(s("be evil")),
            Request::SetBoatKey(flotilla_proto::Secret(s("boat_x"))),
            Request::RemoveSkill(s("council")),
            Request::PairCode { ttl_secs: None },
            Request::RevokeDevice(phone.clone()),
            Request::RenameDevice {
                device: phone.clone(),
                name: s("mine"),
            },
            Request::RemoveProject(s("p1")),
            Request::RunSetup {
                project: s("p1"),
                fresh: true,
            },
            Request::DeleteSchedule(s("s1")),
            Request::Rebuild,
            Request::SyncNow,
            Request::RunDoctor,
            Request::CachedRepos,
            Request::RegisterPush(flotilla_proto::PushToken {
                token: flotilla_proto::Secret("ab".repeat(32)),
                env: s("sandbox"),
                topic: s("x"),
                relay: None,
            }),
            Request::Send {
                thread: s("t1"),
                text: s("x"),
                files: vec![flotilla_proto::Attached {
                    path: "/x".into(),
                    name: s("x"),
                    size: 1,
                    kind: Default::default(),
                    blob: format!("b_{}", "ab".repeat(32)),
                    from: None,
                }],
                local: s("local-9"),
            },
            Request::NewThread(Box::new(NewThread {
                parent: Some(s("t1")),
                ..new_thread("11111111-0000-4000-8000-000000000000")
            })),
            // Never a public link, for any time, nor one extended or ended.
            Request::MakePublic {
                thread: s("t1"),
                port: 3000,
                for_: flotilla_proto::PublicFor::Hour,
            },
            Request::MakePublic {
                thread: s("t1"),
                port: 3000,
                for_: flotilla_proto::PublicFor::Week,
            },
            Request::MakePublic {
                thread: s("t1"),
                port: 3000,
                for_: flotilla_proto::PublicFor::UntilStopped,
            },
            Request::MakePrivate {
                thread: s("t1"),
                port: 3000,
            },
        ];
        for c in [agent, narrowed] {
            let before = f.cmds.lock().unwrap().len();
            for (i, req) in forbidden.iter().enumerate() {
                f.ask_on(c, i as ReqId + 1, req.clone());
            }
            let out = f.flush_all().remove(c);
            for (i, req) in forbidden.iter().enumerate() {
                let got = answer(&out, i as ReqId + 1);
                assert!(
                    matches!(got, Some(Err(why)) if why.starts_with("Agents can't")),
                    "{req:?}: {got:?}"
                );
            }
            assert_eq!(
                f.cmds.lock().unwrap().len(),
                before,
                "the engine was asked something"
            );
        }
        assert!(f.store().thread("t1").unwrap().is_some());
        assert!(f.store().device_paired(&phone).unwrap());
        assert_eq!(
            f.store()
                .kv_get(flotilla_core::lifecycle::IDLE_MINUTES_KEY)
                .unwrap(),
            None
        );
        assert!(flotilla_core::public::records(f.store(), None).is_empty());
        // Inside its scope: on to the engine, as anyone's.
        for (i, req) in [
            Request::Interrupt(s("t1")),
            Request::Archive(s("t1")),
            Request::Preview {
                thread: s("t1"),
                port: 3000,
            },
        ]
        .into_iter()
        .enumerate()
        {
            f.ask_on(agent, 100 + i as ReqId, req);
        }
        f.ask_on(
            agent,
            110,
            Request::Search {
                query: s("Thread"),
                limit: None,
            },
        );
        f.ask_on(
            agent,
            111,
            Request::NewThread(Box::new(new_thread("22222222-0000-4000-8000-000000000000"))),
        );
        let out = f.flush_all().remove(agent);
        assert!(
            matches!(answer(&out, 110), Some(Ok(Reply::Hits(_)))),
            "{out:?}"
        );
        let acts = f
            .cmds
            .lock()
            .unwrap()
            .iter()
            .filter(|c| matches!(c, Cmd::Act { .. }))
            .count();
        assert_eq!(acts, 4, "{:?}", f.cmds.lock().unwrap());
        // This machine, not narrowed, may do anything.
        f.ask_on(0, 200, Request::Set(Setting::IdleMinutes(5)));
        assert_eq!(answer(&f.flush(), 200), Some(&Ok(Reply::Done)));
        f.ask_on(
            0,
            201,
            Request::MakePublic {
                thread: s("t1"),
                port: 3000,
                for_: flotilla_proto::PublicFor::Hour,
            },
        );
        f.flush();
        assert!(
            f.cmds.lock().unwrap().iter().any(|c| matches!(
                c,
                Cmd::Act {
                    action: Action::MakePublic { port: 3000, .. },
                    ..
                }
            )),
            "{:?}",
            f.cmds.lock().unwrap()
        );
        // It may sign itself out: its connection closes at once.
        f.ask_on(agent, 300, Request::RevokeDevice(me.clone()));
        let out = f.flush_raw(agent);
        assert!(
            out.iter()
                .any(|d| matches!(d, Down::Error { msg } if msg == REVOKED)),
            "{out:?}"
        );
        assert!(!f.store().device_paired(&me).unwrap());
    }

    /// An agent sees the threads, each thread and the models; it isn't sent settings, devices
    /// or anything else, whether it subscribes or holds them in its hello. Its reading a thread
    /// doesn't mark it read or open it on screen, and it isn't sent the streaming text.
    #[test]
    fn an_agent_sees_only_its_views_and_its_reading_marks_nothing_read() {
        let mut f = fixture("agent-views");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        f.store()
            .insert_sent_prompt("t1", "boat-1", "prompt", "Hello")
            .unwrap();
        f.store().rebuild_messages("t1").unwrap();
        f.store().mark_unread("t1").unwrap();
        let me = paired_agent(&f, "Hermes");
        let held = vec![
            Held {
                view: ViewKey::Settings,
                rev: None,
            },
            Held {
                view: ViewKey::Devices,
                rev: None,
            },
            Held {
                view: ViewKey::Sidebar,
                rev: None,
            },
        ];
        let agent = f.connect_peer("agent", Peer::Agent(me), None, held, None);
        for view in [
            ViewKey::Catalog,
            ViewKey::Thread(s("t1")),
            ViewKey::Settings,
            ViewKey::Devices,
            ViewKey::Schedules,
            ViewKey::AgentUsage,
            ViewKey::Mentions(s("t1")),
            ViewKey::Project(s("p1")),
        ] {
            f.sub_on(agent, view);
        }
        f.engine(Event::Live {
            thread: s("t1"),
            partials: vec![],
        });
        let out = f.flush_all().remove(agent);
        let kinds: BTreeSet<&str> = out
            .iter()
            .filter_map(|m| match m {
                ServerMsg::Snap { view } => Some(view.key().name()),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, BTreeSet::from(["catalog", "sidebar", "thread"]));
        assert_eq!(
            detail(&out, "t1")
                .unwrap()
                .messages
                .iter()
                .filter_map(|m| m.text.as_deref())
                .collect::<Vec<_>>(),
            ["Hello"]
        );
        // Still unread, for you; nothing opened for the engine to poll.
        assert_eq!(activity(&out, "t1"), Some(Activity::Unread));
        assert_eq!(f.opened().unwrap_or_default(), Vec::<String>::new());
        f.engine(Event::Changed(Changes::thread("t1").with_sidebar()));
        f.flush_all();
        assert_eq!(
            f.store()
                .thread_summaries()
                .unwrap()
                .iter()
                .find(|t| t.thread.id == "t1")
                .map(|t| t.activity),
            Some(Activity::Unread)
        );
        // Opened on screen by you, it's read.
        f.sub_on(0, ViewKey::Thread(s("t1")));
        f.sub_on(0, ViewKey::Sidebar);
        assert_eq!(activity(&f.flush(), "t1"), Some(Activity::Idle));
        assert_eq!(f.opened(), Some(vec![s("t1")]));
        // Notes (a schedule's failure, a setup run, alerts) go to you, not to the agent, nor are
        // they kept for it while it's away.
        f.state.note(Note::ScheduleFailed {
            schedule: s("s1"),
            prompt: s("secret plan"),
            thread: None,
            why: s("no starts"),
        });
        let out = f.flush_all();
        assert!(
            out[0]
                .iter()
                .any(|m| matches!(m, ServerMsg::Note(Note::ScheduleFailed { .. })))
        );
        assert!(
            !out[agent].iter().any(|m| matches!(m, ServerMsg::Note(_))),
            "{:?}",
            out[agent]
        );
        // Its streaming text goes to you, not to the agent reading it too.
        let partial = RawEvent::from_value(
            serde_json::json!({"id": "e", "type": "response", "data": {"content": "typing", "is_streaming": true}}),
        );
        f.engine(Event::Live {
            thread: s("t1"),
            partials: vec![partial],
        });
        let out = f.flush_all();
        assert!(out[0].iter().any(|m| matches!(m, ServerMsg::Live { .. })));
        assert!(
            !out[agent]
                .iter()
                .any(|m| matches!(m, ServerMsg::Live { .. })),
            "{:?}",
            out[agent]
        );
    }

    /// A device paired in the fixture's database: its id and token.
    fn paired(f: &Fixture, name: &str) -> (String, String) {
        let now = flotilla_core::util::now_ms();
        let code = f
            .store()
            .mint_code(flotilla_core::devices::CODE_TTL, "cli", now)
            .unwrap();
        let p = f
            .store()
            .pair_device(&code.code, name, "macos", None, now)
            .unwrap();
        (p.device, p.token)
    }

    #[test]
    fn a_search_hit_opens_on_the_page_that_holds_it_and_a_removed_device_finds_nothing() {
        use flotilla_proto::history::{PAGE, page_of};
        let mut f = fixture("search");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        // 160 turns: 320 messages, the view holds 200.., the hit is on page 0.
        long_thread(
            f.store(),
            "t1",
            160,
            Some((3, "the zebra crossing is fixed")),
            None,
        );
        let search = || Request::Search {
            query: s("Zebra CROSSING"),
            limit: None,
        };
        f.ask(1, search());
        let out = f.flush();
        let Some(Ok(Reply::Hits(hits))) = answer(&out, 1) else {
            panic!("{out:?}")
        };
        assert_eq!(hits.len(), 1);
        let hit = hits[0].clone();
        assert_eq!(
            (hit.thread.as_str(), hit.kind, hit.seq),
            ("t1", flotilla_proto::HitKind::Text, Some(7))
        );
        assert_eq!(
            &hit.snippet[hit.marks[0][0] as usize..hit.marks[1][1] as usize],
            "zebra crossing"
        );
        // The page a client fetches to show it holds it, at the seq the hit says.
        let seq = hit.seq.unwrap();
        let (tx, rx) = mpsc::channel();
        f.state.take(In::History(
            Peer::Local,
            s("t1"),
            page_of(seq),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        ));
        let page = rx.recv().unwrap().unwrap();
        assert_eq!(page.page, 0);
        let m = page.messages.iter().find(|m| m.seq == seq).unwrap();
        assert_eq!(m.text.as_deref(), Some("the zebra crossing is fixed"));
        assert!(
            seq < flotilla_proto::history::window_start(320) && page.messages.len() as i64 == PAGE
        );
        // A paired device searches too; once it's removed (from the CLI, before the host has
        // closed it) it finds nothing.
        let (phone, _) = paired(&f, "iPhone");
        let c = f.connect_peer("phone", Peer::Device(phone.clone()), None, vec![], None);
        f.flush_raw(c);
        f.ask_on(c, 2, search());
        let raw = f.flush_raw(c);
        assert!(
            raw.iter().any(
                |d| matches!(d, Down::Res { id: 2, result: Ok(Reply::Hits(h)), .. } if h.len() == 1)
            ),
            "{raw:?}"
        );
        assert!(f.store().revoke_device(&phone).unwrap());
        f.ask_on(c, 3, search());
        let raw = f.flush_raw(c);
        assert!(
            raw.iter()
                .any(|d| matches!(d, Down::Res { id: 3, result: Err(e), .. } if e == REVOKED)),
            "{raw:?}"
        );
    }

    fn welcome(raw: &[Down]) -> Option<&Welcome> {
        raw.iter().find_map(|d| {
            if let Down::Welcome(w) = d {
                Some(w)
            } else {
                None
            }
        })
    }

    #[test]
    fn a_session_is_its_devices_alone() {
        let mut f = fixture("device-sessions");
        let (a, _) = paired(&f, "A");
        let (b, _) = paired(&f, "B");
        let ca = f.connect_peer("shared", Peer::Device(a.clone()), None, vec![], None);
        let raw = f.flush_raw(ca);
        assert_eq!(
            welcome(&raw).and_then(|w| w.device.clone()),
            Some(a.clone())
        );
        f.ask_on(ca, 1, Request::MarkViewed(s("t1")));
        f.flush();
        let conn = f.clients[ca].conn;
        f.state.take(In::Gone(conn));
        // Another device, and this machine, can't take the session over.
        for peer in [Peer::Device(b.clone()), Peer::Local] {
            let c = f.connect_peer("shared", peer, None, vec![], None);
            let raw = f.flush_raw(c);
            assert!(
                matches!(&raw[..], [Down::Error { msg }] if msg.contains("another device")),
                "{raw:?}"
            );
            assert!(!f.state.conns.contains_key(&f.clients[c].conn));
            // And what it sends after is ignored.
            f.ask_on(c, 2, Request::SyncNow);
            assert!(f.flush_raw(c).is_empty());
        }
        // The device itself resumes it.
        let c = f.connect_peer("shared", Peer::Device(a.clone()), None, vec![], Some(1));
        let raw = f.flush_raw(c);
        assert!(
            welcome(&raw).is_some_and(|w| w.resumed && w.seen == Some(1)),
            "{raw:?}"
        );
    }

    #[test]
    fn revoking_a_device_closes_its_connections_and_forgets_its_sessions() {
        let mut f = fixture("revoke");
        let (a, _) = paired(&f, "A");
        let (b, _) = paired(&f, "B");
        f.sub(ViewKey::Devices);
        let ca = f.connect_peer("a1", Peer::Device(a.clone()), None, vec![], None);
        let ca2 = f.connect_peer("a2", Peer::Device(a.clone()), None, vec![], None);
        let cb = f.connect_peer("b1", Peer::Device(b.clone()), None, vec![], None);
        let shown = f.flush();
        let devices = shown
            .iter()
            .rev()
            .find_map(|m| {
                if let ServerMsg::Snap { view } = m {
                    if let View::Devices(d) = &**view {
                        Some(d.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap();
        assert!(devices.devices.iter().all(|d| d.connected), "{devices:?}");
        for c in [ca, ca2, cb] {
            f.flush_raw(c);
        }
        f.ask(1, Request::RevokeDevice(a.clone()));
        // Closed at once, before anything else is sent.
        for c in [ca, ca2] {
            assert!(
                matches!(&f.clients[c].got.lock().unwrap()[..], [Down::Error { msg }] if msg == REVOKED)
            );
            assert!(!f.state.conns.contains_key(&f.clients[c].conn));
        }
        let shown = f.flush();
        assert!(
            shown.iter().any(|m| matches!(
                m,
                ServerMsg::Res {
                    id: 1,
                    result: Ok(Reply::Done)
                }
            )),
            "{shown:?}"
        );
        let devices = shown
            .iter()
            .find_map(|m| {
                if let ServerMsg::Snap { view } = m {
                    if let View::Devices(d) = &**view {
                        Some(d.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(
            devices
                .devices
                .iter()
                .map(|d| d.id.clone())
                .collect::<Vec<_>>(),
            vec![b.clone()]
        );
        assert!(!f.state.sessions.contains_key("a1") && !f.state.sessions.contains_key("a2"));
        assert!(f.state.sessions.contains_key("b1"));
        assert!(
            f.flush_raw(cb)
                .iter()
                .all(|d| !matches!(d, Down::Error { .. }))
        );
        assert!(!f.store().device_paired(&a).unwrap());
        f.ask(2, Request::RevokeDevice(a));
        assert!(f.flush().iter().any(|m| matches!(
            m,
            ServerMsg::Res {
                id: 2,
                result: Err(_)
            }
        )));
    }

    #[test]
    fn renaming_a_device_shows_in_devices() {
        let mut f = fixture("rename-device");
        let (a, _) = paired(&f, "iPhone");
        f.sub(ViewKey::Devices);
        f.flush();
        let devices = |shown: &[ServerMsg]| {
            shown.iter().rev().find_map(|m| {
                if let ServerMsg::Snap { view } = m {
                    if let View::Devices(d) = &**view {
                        Some(d.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        };
        f.ask(
            1,
            Request::RenameDevice {
                device: a.clone(),
                name: s(" Work phone "),
            },
        );
        let shown = f.flush();
        assert!(
            shown.iter().any(|m| matches!(
                m,
                ServerMsg::Res {
                    id: 1,
                    result: Ok(Reply::Done)
                }
            )),
            "{shown:?}"
        );
        assert_eq!(
            devices(&shown)
                .unwrap()
                .devices
                .iter()
                .map(|d| (d.id.clone(), d.name.clone()))
                .collect::<Vec<_>>(),
            vec![(a.clone(), s("Work phone"))]
        );
        // A blank name, or a device that isn't paired: refused, saying why.
        f.ask(
            2,
            Request::RenameDevice {
                device: a,
                name: s("  "),
            },
        );
        f.ask(
            3,
            Request::RenameDevice {
                device: s("0123456789abcdef"),
                name: s("Other"),
            },
        );
        let shown = f.flush();
        assert!(
            shown.iter().any(
                |m| matches!(m, ServerMsg::Res { id: 2, result: Err(e) } if e.contains("name"))
            ),
            "{shown:?}"
        );
        assert!(shown.iter().any(|m| matches!(m, ServerMsg::Res { id: 3, result: Err(e) } if e.contains("no such device"))), "{shown:?}");
    }

    #[test]
    fn a_device_revoked_in_the_database_is_noticed_and_cant_connect_again() {
        let mut f = fixture("revoke-elsewhere");
        let (a, _) = paired(&f, "A");
        f.state.device_marks = f.store().device_marks().unwrap();
        let ca = f.connect_peer("a1", Peer::Device(a.clone()), None, vec![], None);
        f.flush_raw(ca);
        // As `flotilla devices revoke` does, from another process.
        Store::open(&f.state.db).unwrap().revoke_device(&a).unwrap();
        assert!(f.state.watch_devices());
        assert!(
            matches!(&f.clients[ca].got.lock().unwrap()[..], [Down::Error { msg }] if msg == REVOKED)
        );
        assert!(f.state.conns.is_empty() || !f.state.conns.contains_key(&f.clients[ca].conn));
        // A connection whose token was checked just before the revoke isn't let in.
        let c = f.connect_peer("a2", Peer::Device(a), None, vec![], None);
        assert!(matches!(&f.flush_raw(c)[..], [Down::Error { msg }] if msg == REVOKED));
        assert!(!f.state.watch_devices());
    }

    #[test]
    fn tokens_and_codes_are_checked_on_the_hosts_own_connection() {
        let mut f = fixture("auth");
        let (a, token) = paired(&f, "A");
        let got: Arc<Mutex<Vec<Result<Device, AuthError>>>> = Arc::default();
        let (g1, g2) = (got.clone(), got.clone());
        f.state.take(In::Auth(
            token,
            Some(s("100.64.0.9")),
            Box::new(move |r| g1.lock().unwrap().push(r)),
        ));
        f.state.take(In::Auth(
            s("fdt_0123456789abcdef_nope"),
            None,
            Box::new(move |r| g2.lock().unwrap().push(r)),
        ));
        let got = got.lock().unwrap();
        assert!(
            matches!(&got[0], Ok(d) if d.id == a && d.last_ip.as_deref() == Some("100.64.0.9"))
        );
        assert_eq!(got[1], Err(AuthError::Unknown));
        // Codes: made only once the server says where it is, and they pair.
        f.ask(1, Request::PairCode { ttl_secs: None });
        assert!(f.flush().iter().any(|m| matches!(m, ServerMsg::Res { id: 1, result: Err(e) } if e.contains("flotilla serve"))));
        f.store()
            .kv_set(SERVER_URL_KEY, "https://box.tail1.ts.net")
            .unwrap();
        f.ask(2, Request::PairCode { ttl_secs: Some(60) });
        let code = f
            .flush()
            .into_iter()
            .find_map(|m| {
                if let ServerMsg::Res {
                    id: 2,
                    result: Ok(Reply::Code(c)),
                } = m
                {
                    Some(c)
                } else {
                    None
                }
            })
            .unwrap();
        let target = flotilla_proto::pair::read(&code.link).unwrap();
        assert_eq!(
            (target.server.as_str(), target.code.as_deref()),
            ("https://box.tail1.ts.net", Some(code.code.as_str()))
        );
        let pairs: Arc<Mutex<Vec<Result<Paired, PairError>>>> = Arc::default();
        for _ in 0..2 {
            let p = pairs.clone();
            let req = PairRequest {
                code: flotilla_proto::Secret(code.code.clone()),
                device_name: s("Phone"),
                platform: s("ios"),
                scope: None,
            };
            f.state.take(In::Pair(
                req,
                None,
                None,
                Box::new(move |r| p.lock().unwrap().push(r)),
            ));
        }
        let pairs = pairs.lock().unwrap();
        assert!(pairs[0].is_ok());
        assert_eq!(pairs[1], Err(PairError::Used));
    }

    #[test]
    fn a_queue_that_never_empties_cant_hold_off_the_watcher() {
        let mut f = fixture("flood");
        let (a, _) = paired(&f, "A");
        f.state.device_marks = f.store().device_marks().unwrap();
        let ca = f.connect_peer("a1", Peer::Device(a.clone()), None, vec![], None);
        f.flush_raw(ca);
        Store::open(&f.state.db).unwrap().revoke_device(&a).unwrap();
        // The device keeps sending; the watcher is due.
        let (tx, rx) = mpsc::channel();
        let conn = f.clients[ca].conn;
        for id in 1..=10 {
            tx.send(In::From(
                conn,
                Up::Req {
                    id,
                    req: Request::MarkViewed(s("t1")),
                },
            ))
            .unwrap();
        }
        f.state.watched = Instant::now() - WATCH_EVERY;
        assert!(f.state.turn(&rx));
        assert!(
            matches!(&f.clients[ca].got.lock().unwrap()[..], [Down::Error { msg }] if msg == REVOKED)
        );
        // It looked before taking the rest.
        assert_eq!(rx.try_iter().count(), 9);
    }

    /// The answer to request `id` among what a client was sent.
    fn answer_of(msgs: &[ServerMsg], id: ReqId) -> Option<Result<Reply, String>> {
        msgs.iter().find_map(|m| match m {
            ServerMsg::Res { id: i, result } if *i == id => Some(result.clone()),
            _ => None,
        })
    }

    #[test]
    fn only_this_machine_sets_the_boat_key() {
        let mut f = fixture("boat-key");
        let (a, _) = paired(&f, "A");
        let ca = f.connect_peer("a1", Peer::Device(a), None, vec![], None);
        f.ask_on(
            ca,
            1,
            Request::SetBoatKey(flotilla_proto::Secret(s("boat_from_a_client"))),
        );
        let got = f.flush_all().remove(ca);
        let e = answer_of(&got, 1).unwrap().unwrap_err();
        assert!(e.contains("flotilla setup"), "{e}");
        assert!(f.state.boat_key.is_none());
    }

    /// A prompt's files must be uploaded already; each is noted as from the device that sent
    /// it, and goes to the engine as its blob, never a path the server reads.
    #[test]
    fn files_must_be_uploaded_and_are_noted_as_from_their_device() {
        let mut f = fixture("uploads");
        f.store().insert_thread(&thread("t1", None)).unwrap();
        let blob = flotilla_core::blobs::Blobs::beside(&f.state.db)
            .put(b"hello")
            .unwrap();
        let (a, _) = paired(&f, "A");
        let ca = f.connect_peer("a1", Peer::Device(a.clone()), None, vec![], None);
        let file = |path: &str, blob: &str| flotilla_proto::Attached {
            path: path.into(),
            name: s("notes.txt"),
            size: 5,
            kind: flotilla_proto::Kind::Text,
            blob: s(blob),
            from: Some(s("someone-else")),
        };
        let send = |local: &str, files| Request::Send {
            thread: s("t1"),
            text: s("look"),
            files,
            local: format!("local-{local}"),
        };
        // A path on the server, a made-up blob, one that's gone: refused, nothing sent.
        f.ask_on(ca, 1, send("1", vec![file("/etc/passwd", "")]));
        f.ask_on(ca, 2, send("2", vec![file("/x", "b_../../etc/passwd")]));
        f.ask_on(
            ca,
            3,
            send("3", vec![file("/x", &format!("b_{}", "0".repeat(64)))]),
        );
        let mut new = NewThread {
            attachments: vec![file("/etc/hosts", "")],
            id: Some(s("t9")),
            ..fixtures_new()
        };
        f.ask_on(ca, 4, Request::NewThread(Box::new(new.clone())));
        let got = f.flush_all().remove(ca);
        assert_eq!(
            answer_of(&got, 1).unwrap().unwrap_err(),
            "notes.txt wasn't uploaded"
        );
        assert_eq!(
            answer_of(&got, 2).unwrap().unwrap_err(),
            "notes.txt wasn't uploaded"
        );
        assert_eq!(
            answer_of(&got, 3).unwrap().unwrap_err(),
            "notes.txt is no longer on the server: attach it again"
        );
        assert_eq!(
            answer_of(&got, 4).unwrap().unwrap_err(),
            "notes.txt wasn't uploaded"
        );
        assert!(
            !f.cmds
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, Cmd::Act { .. }))
        );
        assert!(f.store().thread("t9").unwrap().is_none());

        // Uploaded: sent, as from A whatever it claimed.
        f.ask_on(ca, 5, send("5", vec![file("/Users/a/notes.txt", &blob.id)]));
        f.flush_all();
        let sent = f.cmds.lock().unwrap().iter().rev().find_map(|c| match c {
            Cmd::Act {
                action: Action::Send { files, .. },
                ..
            } => Some(files.clone()),
            _ => None,
        });
        let sent = sent.unwrap();
        assert_eq!(
            (
                sent[0].blob.as_str(),
                sent[0].from.as_deref(),
                sent[0].path.to_str()
            ),
            (
                blob.id.as_str(),
                Some(a.as_str()),
                Some("/Users/a/notes.txt")
            )
        );
        let kept = f.store().sent_prompts("t1").unwrap();
        assert_eq!(kept[0].attachments[0].from.as_deref(), Some(a.as_str()));
        // From this machine: none.
        new.attachments = vec![file("/tmp/notes.txt", &blob.id)];
        f.ask(6, Request::NewThread(Box::new(new)));
        f.flush_all();
        let started = f.cmds.lock().unwrap().iter().rev().find_map(|c| match c {
            Cmd::Act {
                action: Action::Launch { req, .. } | Action::New(req),
                ..
            } => Some(req.attachments.clone()),
            _ => None,
        });
        assert_eq!(started.unwrap()[0].from, None);
    }

    fn fixtures_new() -> NewThread {
        NewThread {
            environment: s("base"),
            project_id: None,
            provider: s("claude-code"),
            model: s("opus-5-5"),
            effort: None,
            fast: false,
            title: None,
            machine_type: None,
            prompt: s("hello"),
            attachments: vec![],
            wait_ready: false,
            on_branch: None,
            id: None,
            parent: None,
            branch_tag: None,
        }
    }

    #[test]
    fn skills_come_as_blobs_and_say_where_they_came_from() {
        let mut f = fixture("skills");
        f.sub(ViewKey::Settings);
        f.flush();
        let blobs = flotilla_core::blobs::Blobs::beside(&f.state.db);
        let packed = blobs.put(b"a packed folder").unwrap();
        let (a, _) = paired(&f, "My MacBook");
        let ca = f.connect_peer("a1", Peer::Device(a.clone()), None, vec![], None);
        let skill = |name: &str| flotilla_proto::Skill {
            name: s(name),
            description: s("Review a PR"),
            folder: "/Users/you/.claude/skills/review".into(),
        };
        f.ask_on(
            ca,
            1,
            Request::PutSkill {
                skill: skill("review"),
                blob: packed.id.clone(),
            },
        );
        f.ask_on(
            ca,
            2,
            Request::PutSkill {
                skill: skill("../escape"),
                blob: packed.id.clone(),
            },
        );
        f.ask_on(
            ca,
            3,
            Request::PutSkill {
                skill: skill("gone"),
                blob: format!("b_{}", "1".repeat(64)),
            },
        );
        f.ask_on(
            ca,
            4,
            Request::PutSkill {
                skill: skill("path"),
                blob: s("/etc/passwd"),
            },
        );
        let got = f.flush_all();
        assert_eq!(answer_of(&got[ca], 1), Some(Ok(Reply::Done)));
        for id in 2..=4 {
            assert!(answer_of(&got[ca], id).unwrap().is_err(), "{id}");
        }
        let settings = got[0].iter().rev().find_map(|m| match m {
            ServerMsg::Snap { view } => match view.as_ref() {
                View::Settings(st) => Some(st.clone()),
                _ => None,
            },
            _ => None,
        });
        let kept = settings.unwrap().skills;
        assert_eq!(kept.len(), 1);
        assert_eq!(
            (
                kept[0].from.as_deref(),
                kept[0].from_name.as_str(),
                kept[0].size
            ),
            (Some(a.as_str()), "My MacBook", 15)
        );
        // From this machine: its own name.
        f.ask(
            5,
            Request::PutSkill {
                skill: skill("review"),
                blob: packed.id.clone(),
            },
        );
        f.flush();
        let kept = flotilla_core::skills::chosen(f.store());
        assert_eq!((kept.len(), kept[0].from.clone()), (1, None));
        assert_eq!(kept[0].from_name, flotilla_core::util::machine_name());
        f.ask(6, Request::RemoveSkill(s("review")));
        f.ask(7, Request::RemoveSkill(s("review")));
        let got = f.flush();
        assert_eq!(answer_of(&got, 6), Some(Ok(Reply::Done)));
        assert!(answer_of(&got, 7).unwrap().is_err());
        assert!(flotilla_core::skills::chosen(f.store()).is_empty());
    }

    /// Everything set up, as far as the stand-in says.
    struct AllGood;

    impl Probe for AllGood {
        fn boat_key(&self) -> Result<Option<(String, flotilla_core::secrets::Source)>, String> {
            Ok(Some((
                s("boat_abcdmlrf"),
                flotilla_core::secrets::Source::Env("BOAT_API_KEY"),
            )))
        }
        fn boat_me(&self, _: &str) -> Result<(), flotilla_core::doctor::Refusal> {
            Ok(())
        }
        fn boat_keys(&self, _: &str) -> Result<Value, flotilla_core::doctor::Refusal> {
            Ok(
                serde_json::json!({"apiKeys": [{"keyPrefix": "boat_abcd", "keyLastFour": "mlrf", "scope": {"actions": ["*"], "sandboxes": "*", "environments": "*"}}]}),
            )
        }
        fn gh(&self) -> GhStatus {
            GhStatus::SignedIn {
                login: s("acme"),
                version: s("2.81.0"),
                scopes: vec![s("repo")],
            }
        }
        fn github_token(&self) -> Result<String, String> {
            Ok(s("gho_x"))
        }
        fn github_user(
            &self,
            _: &str,
        ) -> Result<flotilla_core::github::TokenUser, flotilla_core::github::TokenError> {
            Ok(flotilla_core::github::TokenUser {
                login: s("acme"),
                scopes: Some(vec![s("repo")]),
            })
        }
        fn github_env(&self) -> Option<&'static str> {
            None
        }
        fn claude_token(&self) -> Result<bool, String> {
            Ok(false)
        }
        fn summaries(&self, _: &str) -> Result<(), flotilla_core::doctor::ClaudeError> {
            Ok(())
        }
        fn tailscale(&self) -> Result<flotilla_core::tailscale::Status, String> {
            Ok(flotilla_core::tailscale::Status {
                running: true,
                name: Some(s("laptop.tail1.ts.net")),
                https: true,
                id: None,
                funnel: None,
            })
        }
        fn port_use(&self, _: u16) -> Result<flotilla_core::tailscale::Use, String> {
            Ok(flotilla_core::tailscale::Use::Free)
        }
        fn vms(&self, _: &str) -> Result<Vec<flotilla_core::boat::Sandbox>, String> {
            Ok(vec![])
        }
    }

    /// Check again: answered when the checks are done, and every client's Settings shows them
    /// (a server's with Tailscale, which must forward to the port it serves).
    #[test]
    fn the_checks_run_when_asked_and_every_settings_shows_them() {
        let mut f = fixture("doctor");
        let (tx, rx) = mpsc::channel();
        f.state.tx = tx;
        f.state.probe = Arc::new(AllGood);
        f.sub(ViewKey::Settings);
        let other = f.connect();
        f.sub_on(other, ViewKey::Settings);
        f.flush_all();
        let doctor_of = |msgs: &[ServerMsg]| {
            msgs.iter().rev().find_map(|m| match m {
                ServerMsg::Snap { view } => match view.as_ref() {
                    View::Settings(st) => st.doctor.clone(),
                    _ => None,
                },
                _ => None,
            })
        };
        let wait = |f: &mut Fixture| match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            d @ In::Doctor(..) => f.state.take(d),
            _ => panic!("not the checks"),
        };
        f.ask(1, Request::RunDoctor);
        f.ask(2, Request::RunDoctor);
        assert!(answer_of(&f.flush(), 1).is_none());
        wait(&mut f);
        let got = f.flush_all();
        assert_eq!(
            (answer_of(&got[0], 1), answer_of(&got[0], 2)),
            (Some(Ok(Reply::Done)), Some(Ok(Reply::Done)))
        );
        let d = doctor_of(&got[other]).unwrap();
        assert!(d.ok());
        assert!(
            !d.checks
                .iter()
                .any(|c| c.check == flotilla_proto::CheckOf::Tailscale)
        );
        // Serving: a run begun before it is dropped; the next looks at Tailscale too.
        f.state
            .store
            .as_ref()
            .unwrap()
            .kv_set(flotilla_core::tailscale::KEY, "on")
            .unwrap();
        f.state.take(In::Serving(Some(7777)));
        f.ask(3, Request::RunDoctor);
        f.state.take(In::Doctor(
            f.state.doctor_run - 1,
            Doctor {
                checks: vec![],
                at_ms: 0,
            },
        ));
        wait(&mut f);
        let d = doctor_of(&f.flush()).unwrap();
        let ts = d
            .checks
            .iter()
            .find(|c| c.check == flotilla_proto::CheckOf::Tailscale)
            .unwrap();
        assert_eq!(
            ts.fix,
            ["tailscale serve --bg --https=443 http://127.0.0.1:7777"]
        );
    }

    /// The stand-in, except that asking boat about the key waits until the test says go on:
    /// the checks are under way, off the host's thread, for as long as it likes.
    struct Slow {
        asking: Mutex<mpsc::Sender<()>>,
        go_on: Mutex<mpsc::Receiver<()>>,
    }

    impl Probe for Slow {
        fn boat_key(&self) -> Result<Option<(String, flotilla_core::secrets::Source)>, String> {
            AllGood.boat_key()
        }
        fn boat_me(&self, key: &str) -> Result<(), flotilla_core::doctor::Refusal> {
            let _ = self.asking.lock().unwrap().send(());
            let _ = self.go_on.lock().unwrap().recv();
            AllGood.boat_me(key)
        }
        fn boat_keys(&self, key: &str) -> Result<Value, flotilla_core::doctor::Refusal> {
            AllGood.boat_keys(key)
        }
        fn gh(&self) -> GhStatus {
            AllGood.gh()
        }
        fn github_token(&self) -> Result<String, String> {
            AllGood.github_token()
        }
        fn github_user(
            &self,
            token: &str,
        ) -> Result<flotilla_core::github::TokenUser, flotilla_core::github::TokenError> {
            AllGood.github_user(token)
        }
        fn github_env(&self) -> Option<&'static str> {
            AllGood.github_env()
        }
        fn claude_token(&self) -> Result<bool, String> {
            AllGood.claude_token()
        }
        fn summaries(&self, model: &str) -> Result<(), flotilla_core::doctor::ClaudeError> {
            AllGood.summaries(model)
        }
        fn tailscale(&self) -> Result<flotilla_core::tailscale::Status, String> {
            AllGood.tailscale()
        }
        fn port_use(&self, port: u16) -> Result<flotilla_core::tailscale::Use, String> {
            AllGood.port_use(port)
        }
        fn vms(&self, key: &str) -> Result<Vec<flotilla_core::boat::Sandbox>, String> {
            AllGood.vms(key)
        }
    }

    /// Checks still under way when the host goes don't hold up its stop, and leave the database
    /// alone from then on: they never open it once its lock is free for another engine, or
    /// make its folder again once it's gone.
    #[test]
    fn checks_under_way_leave_the_database_alone_once_the_host_is_gone() {
        let dir =
            std::env::temp_dir().join(format!("flotilla-host-doctor-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("db");
        let lock = Lock::take(&db, &flotilla_core::lock::Holder::new("server")).unwrap();
        let mut state = restart(&db, Some(Arc::default()));
        state._lock = Some(lock);
        let (tx, rx) = mpsc::channel();
        state.tx = tx;
        let (asking, asked) = mpsc::channel();
        let (go_on, waiting) = mpsc::channel();
        state.probe = Arc::new(Slow {
            asking: Mutex::new(asking),
            go_on: Mutex::new(waiting),
        });
        state.run_doctor();
        asked.recv_timeout(Duration::from_secs(10)).unwrap();
        // The host goes while the checks ask boat: its lock is free at once.
        drop(state);
        drop(
            Lock::take(&db, &flotilla_core::lock::Holder::new("server"))
                .expect("the lock is still held"),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        go_on.send(()).unwrap();
        let doctor = match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            In::Doctor(_, doctor) => doctor,
            _ => panic!("not the checks"),
        };
        assert!(!dir.exists(), "the checks made the database's folder again");
        // The network's checks ran; the database's didn't.
        assert!(
            doctor
                .checks
                .iter()
                .any(|c| c.check == flotilla_proto::CheckOf::BoatKey)
        );
        assert!(
            !doctor
                .checks
                .iter()
                .any(|c| c.check == flotilla_proto::CheckOf::Database)
        );
    }

    /// The agents' usage: opening it asks for a check (the engine waits five minutes between
    /// those); Refresh asks for one now and is answered when it ends; what was read is sent.
    #[test]
    fn the_agents_usage_checks_when_opened_and_on_refresh() {
        let mut f = fixture("agent-usage");
        f.sub(ViewKey::AgentUsage);
        let got = f.flush();
        assert!(matches!(
            f.cmds.lock().unwrap().last(),
            Some(Cmd::ReadAgentUsage { force: false })
        ));
        let first = got.iter().find_map(|m| match m {
            ServerMsg::Snap { view } => match &**view {
                View::AgentUsage(u) => Some(u.clone()),
                _ => None,
            },
            _ => None,
        });
        assert_eq!(first.map(|u| u.hours.len()), Some(0));
        f.ask(1, Request::ReadAgentUsage);
        assert!(answer_of(&f.flush(), 1).is_none());
        assert!(matches!(
            f.cmds.lock().unwrap().last(),
            Some(Cmd::ReadAgentUsage { force: true })
        ));
        let rec = flotilla_core::spend::Record {
            provider: "codex",
            at_ms: flotilla_core::util::now_ms(),
            model: s("gpt-6-luna"),
            tokens: flotilla_proto::TokenTotals {
                input: 10,
                output: 5,
                ..Default::default()
            },
            cache_write_1h: 0,
            speed: flotilla_proto::Speed::Standard,
            reported_usd: None,
            key: s("k1"),
        };
        flotilla_core::spend::keep(
            f.state.store.as_ref().unwrap(),
            "t1",
            &[rec],
            &flotilla_core::spend::Cursor::default(),
            &Default::default(),
            flotilla_core::util::now_ms(),
        )
        .unwrap();
        f.engine(Event::AgentUsage {
            checked: Some(Ok(())),
        });
        let got = f.flush();
        assert_eq!(answer_of(&got, 1), Some(Ok(Reply::Done)));
        let sent = got.iter().find_map(|m| match m {
            ServerMsg::Snap { view } => match &**view {
                View::AgentUsage(u) => Some(u.clone()),
                _ => None,
            },
            _ => None,
        });
        assert_eq!(
            sent.map(|u| (u.hours.len(), u.hours[0].requests)),
            Some((1, 1))
        );
        // A read after a turn changes it without answering anyone.
        f.ask(2, Request::ReadAgentUsage);
        f.engine(Event::AgentUsage { checked: None });
        assert!(answer_of(&f.flush(), 2).is_none());
        f.engine(Event::AgentUsage {
            checked: Some(Err(s("boat is down"))),
        });
        assert_eq!(answer_of(&f.flush(), 2), Some(Err(s("boat is down"))));
    }

    /// Refresh in the usage popover: answered once boat's been read (or with why not), the
    /// numbers kept for the next start, offline included.
    #[test]
    fn a_usage_read_answers_when_its_in_and_is_kept() {
        let mut f = fixture("usage");
        f.sub(ViewKey::Sidebar);
        f.flush_all();
        f.ask(1, Request::ReadUsage);
        assert!(answer_of(&f.flush(), 1).is_none());
        assert!(matches!(
            f.cmds.lock().unwrap().last(),
            Some(Cmd::ReadUsage)
        ));
        let u = Usage {
            vms: Some((3, 100)),
            read_ms: Some(5),
            ..Default::default()
        };
        f.engine(Event::Usage(Ok(u.clone())));
        let got = f.flush();
        assert_eq!(answer_of(&got, 1), Some(Ok(Reply::Done)));
        assert_eq!(sidebar(&got).and_then(|s| s.usage.clone()), Some(u.clone()));
        // A failed read says why, and the last numbers stay.
        f.ask(2, Request::ReadUsage);
        f.engine(Event::Usage(Err(s("boat is down"))));
        let got = f.flush();
        assert_eq!(answer_of(&got, 2), Some(Err(s("boat is down"))));
        assert_eq!(f.state.usage, Some(u.clone()));
        assert_eq!(restart(&f.dir.join("db"), None).usage, Some(u));
    }
}
