//! Flotilla's server: the engine's host (`flotilla_engine::host`) for any number of clients,
//! each over its own WebSocket at `/v1/ws` (the protocol is `flotilla_proto::wire`). Also:
//!
//! - `GET /v1/health` and `GET /.well-known/flotilla` (its id, version, protocol and
//!   capabilities): public;
//! - `POST /v1/pair`: a one-time pairing code swapped for a device token (public, with a limit
//!   on wrong codes);
//! - `PUT /v1/blobs`: store a file (an attachment, a packed skill) and get its id back. They're
//!   kept in `blobs/` beside the database (`flotilla_core::blobs`), where the engine reads them.
//!
//! **Who gets in.** Over TCP, the WebSocket and blobs need a paired device's token
//! (`Authorization: Bearer fdt_…`), on loopback too: `tailscale serve` forwards your tailnet's
//! requests to 127.0.0.1. A device paired as an agent (`flotilla_proto::Scope::Agent`, for
//! `flotilla mcp`) is held to what an agent may do: the host refuses its other requests and
//! views, and it may not upload. Over the Unix socket (in a directory only its owner can enter, the
//! socket itself 0600, and the peer's uid checked as each connection is accepted) this
//! machine's own user needs no token. Requests from web pages (an `Origin` header) are refused:
//! there's no web client yet. TCP listens on this machine unless told `insecure`: there's no
//! TLS here, so codes and tokens would cross the network in the clear (use `tailscale serve`).
//!
//! It takes the database's lock first, so it's the only engine on it, and won't start on a
//! database that moved to another Flotilla (`flotilla import`; see `flotilla_core::owner`).
//!
//! **Remote MCP** ([`mcp`]), only when it's asked for ([`Listen::Mcp`]): a listener of its own
//! that serves the MCP tools over HTTP to agent devices, and the OAuth that signs them in, and
//! nothing else, so it can be published to the internet while the rest stays private. Its
//! sessions reach this server as their devices through a pipe that takes a token, as TCP does.
//!
//! **In the app.** The app on This Mac runs its own server, in its own process ([`Local`]): the
//! app's window reaches it through an in-memory pipe ([`Listen::Memory`], a [`Doorway`]), with
//! the same WebSocket and messages as a remote one; the CLI through its Unix socket; and other
//! devices over TCP only while the app lets them in ([`Access`]). It stops, and lets go of the
//! database, when the app switches to another server or quits.

use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{
    Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection,
};
use axum::extract::{
    ConnectInfo, DefaultBodyLimit, FromRequest, Path as UrlPath, Query, Request, State,
};
use axum::http::{Extensions, HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use flotilla_core::devices::{AuthError, PairError};
use flotilla_core::lock::{Busy, Holder, Lock};
use flotilla_core::owner::Moved;
use flotilla_engine::host::{Host, Mode, Outbox, Peer, ReadError, Role};
use flotilla_proto::history::{FULL_PATH, HISTORY_PATH};
use flotilla_proto::wire::{
    CAPABILITIES, Descriptor, Down, PAIR_PATH, PROTOCOL_HEADER, PairRequest, Paired, Up, WS_PATH,
};
use flotilla_proto::{PROTOCOL, Secret};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod mcp;

/// Where to listen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    Tcp(SocketAddr),
    Unix(PathBuf),
    /// In this process, through a [`Doorway`]: trusted as the Unix socket is.
    Memory,
    /// Remote MCP on TCP at this address (`flotilla serve --mcp-port`), for agent devices only,
    /// and the address clients reach it at if it's set (else each request's own).
    Mcp(SocketAddr, Option<String>),
}

/// What binding anywhere but this machine says.
pub const INSECURE_WARNING: &str = "Flotilla speaks plain HTTP: pairing codes and device tokens will cross the network unencrypted. Anyone who can see that traffic can take them. Prefer --tailscale-serve.";

impl Listen {
    /// `--host` and `--port`. Only this machine's own addresses unless `insecure`.
    pub fn tcp(host: &str, port: u16, insecure: bool) -> Result<Listen, String> {
        let ip: IpAddr = match host {
            "localhost" => IpAddr::from([127, 0, 0, 1]),
            h => h
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse()
                .map_err(|_| format!("{h} isn't an IP address"))?,
        };
        if !ip.is_loopback() && !insecure {
            return Err(format!(
                "Flotilla listens only on this machine (127.0.0.1 or ::1) unless you say --insecure: it speaks plain HTTP, so codes and tokens to {ip} would cross the network unencrypted. Use --tailscale-serve to reach it from your other devices."
            ));
        }
        Ok(Listen::Tcp(SocketAddr::new(ip, port)))
    }
}

pub struct Config {
    pub db: PathBuf,
    /// At least one. The CLI uses the Unix socket, the app's own window the pipe, other
    /// devices TCP.
    pub listen: Vec<Listen>,
    pub mode: Mode,
    /// `flotilla serve`, or the app on This Mac (whose lock says `app`, whose setup fixes are
    /// its own, and which runs without the Unix socket if it can't have it).
    pub role: Role,
}

/// The largest file `PUT /v1/blobs` takes: boat's limit for one prompt's files.
pub const BLOB_MAX: usize = 64 << 20;
/// A client's messages waiting to be written: one that falls this far behind, in messages or
/// in bytes, is let go (it reconnects and catches up). A message bigger than the bytes allowed
/// still goes when nothing else waits.
const OUTBOX: usize = 4096;
const OUTBOX_BYTES: usize = 32 << 20;
/// A connection with nothing from its client this long is closed (clients ping every 20 s).
const IDLE: Duration = Duration::from_secs(60);
const HELLO_WITHIN: Duration = Duration::from_secs(10);
/// What an in-memory pipe holds in each direction before a write waits for the reader.
const PIPE_BUFFER: usize = 256 << 10;
/// Wrong pairing codes from one address: at most this many a minute, then none of its codes
/// are checked until the minute is up.
pub const PAIR_TRIES: usize = 10;
/// The same across every address, so one that keeps changing its address can't try faster.
/// Far above what one address may try, so one can't lock everyone else out.
pub const PAIR_TRIES_ALL: usize = 100;
const PAIR_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct App {
    host: Arc<Host>,
    blobs: flotilla_core::blobs::Blobs,
    /// The Unix socket's: this machine's user, no token.
    trusted: bool,
    /// Set when the server stops: every connection closes.
    stop: tokio::sync::watch::Receiver<bool>,
    /// One clone per open connection, to wait for them all to close.
    open: Arc<()>,
    tries: Arc<Mutex<Tries>>,
}

/// Pairing tries in the last minute that were (or may be) wrong, each with where it came from.
#[derive(Default)]
struct Tries {
    next: u64,
    at: VecDeque<(u64, Instant, String)>,
    /// When a refusal for each address (`""` for every address) was last logged: once a
    /// minute, not once a try.
    logged: HashMap<String, Instant>,
}

impl Tries {
    /// Room for a try from `from`: its number, counted as wrong until it isn't, so tries at
    /// once can't get past the limit together. `Err` with what to log, the first time in a
    /// minute it's refused.
    fn take(&mut self, from: &str, now: Instant) -> Result<u64, Option<String>> {
        while self
            .at
            .front()
            .is_some_and(|(_, at, _)| now.duration_since(*at) > PAIR_WINDOW)
        {
            self.at.pop_front();
        }
        self.logged
            .retain(|_, at| now.duration_since(*at) <= PAIR_WINDOW);
        let mine = self.at.iter().filter(|(_, _, f)| f == from).count();
        let full = if mine >= PAIR_TRIES {
            Some((
                from.to_string(),
                format!(
                    "too many wrong pairing codes from {from}: refusing its codes for a minute"
                ),
            ))
        } else if self.at.len() >= PAIR_TRIES_ALL {
            Some((
                String::new(),
                "too many wrong pairing codes: refusing every code for a minute".to_string(),
            ))
        } else {
            None
        };
        if let Some((key, log)) = full {
            return Err(self.logged.insert(key, now).is_none().then_some(log));
        }
        self.next += 1;
        self.at.push_back((self.next, now, from.to_string()));
        Ok(self.next)
    }
}

/// Whom pairing tries are counted against: the address, or for IPv6 its /64 (one machine is
/// usually given a whole one).
fn pair_source(ip: Option<&str>) -> String {
    match ip.and_then(|ip| ip.parse::<IpAddr>().ok()) {
        Some(IpAddr::V6(v6)) if v6.to_ipv4_mapped().is_none() => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        Some(IpAddr::V6(v6)) => v6
            .to_ipv4_mapped()
            .map(|v4| v4.to_string())
            .unwrap_or_default(),
        Some(ip) => ip.to_string(),
        None => "this machine".to_string(),
    }
}

enum Bound {
    Tcp(tokio::net::TcpListener),
    Unix(tokio::net::UnixListener, PathBuf),
    Memory(Inside),
    /// Remote MCP's listener, and the pipe its sessions reach the server through.
    Mcp {
        listener: tokio::net::TcpListener,
        url: Arc<Mutex<Option<String>>>,
        door: Doorway,
        inside: Inside,
        db: PathBuf,
    },
}

/// The way into a server from inside its own process (`Listen::Memory`): each [`open`]
/// is a new connection, as a socket's would be. The client's side is `flotilla_client::Pipe`.
///
/// [`open`]: Doorway::open
#[derive(Clone)]
pub struct Doorway(tokio::sync::mpsc::UnboundedSender<tokio::io::DuplexStream>);

impl Doorway {
    /// The client's end of a new connection; None once the server has stopped.
    pub fn open(&self) -> Option<tokio::io::DuplexStream> {
        let (client, server) = tokio::io::duplex(PIPE_BUFFER);
        self.0.send(server).ok()?;
        Some(client)
    }
}

/// The pipes' listener: the server's ends, as they're opened.
struct Inside(tokio::sync::mpsc::UnboundedReceiver<tokio::io::DuplexStream>);

impl axum::serve::Listener for Inside {
    type Io = tokio::io::DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.recv().await {
            Some(io) => (io, ()),
            // No doorway left: nothing more will come (the server stops on its own signal).
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(())
    }
}

/// What can change while a server runs ([`Server::run_with`]).
pub enum Control {
    /// Listen for devices on TCP at this address too (in place of any listening now): where
    /// it's bound, or why not.
    Open(
        SocketAddr,
        tokio::sync::oneshot::Sender<Result<SocketAddr, String>>,
    ),
    /// Stop listening on TCP: the connections it took close.
    Close(tokio::sync::oneshot::Sender<()>),
    /// Run the setup checks again (Tailscale's forwarding changed).
    Recheck,
}

/// A server bound and running its host, not yet serving.
pub struct Server {
    app: App,
    bound: Vec<Bound>,
    address: Option<String>,
    tcp: Option<SocketAddr>,
    socket: Option<PathBuf>,
    doorway: Option<Doorway>,
    /// Where remote MCP listens, when it does.
    mcp: Option<SocketAddr>,
    /// The address its clients use, once it's known.
    mcp_url: Arc<Mutex<Option<String>>>,
    stop: tokio::sync::watch::Sender<bool>,
}

/// Why a server didn't start, and what can be done instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// For a person.
    pub reason: String,
    pub instead: Option<Instead>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instead {
    /// A server on this machine (`flotilla serve`, or `flotilla service`) holds the database:
    /// its Unix socket (`unix:/…`), which this machine's user reaches with no pairing.
    Socket(String),
    /// The database moved to another Flotilla, which runs its threads now.
    Moved(Moved),
}

impl From<String> for Refusal {
    fn from(reason: String) -> Refusal {
        Refusal {
            reason,
            instead: None,
        }
    }
}

impl Refusal {
    fn busy(e: Busy) -> Refusal {
        let reason = e.to_string();
        // Only a server's socket: another app, or a CLI command at work, is no server to use.
        let socket = match e {
            Busy::Held(Some(h)) if h.kind == "server" => {
                h.address.filter(|a| a.starts_with("unix:"))
            }
            _ => None,
        };
        Refusal {
            reason,
            instead: socket.map(Instead::Socket),
        }
    }
}

impl Server {
    /// Take the database's lock, listen, and start the host.
    pub async fn bind(config: Config) -> Result<Server, String> {
        Server::bind_or_refuse(config).await.map_err(|r| r.reason)
    }

    /// [`Server::bind`], saying what could be done instead when it can't.
    pub async fn bind_or_refuse(config: Config) -> Result<Server, Refusal> {
        if config.listen.is_empty() {
            return Err(Refusal::from("Nowhere to listen".to_string()));
        }
        let kind = match config.role {
            Role::Server => "server",
            Role::App => "app",
        };
        let lock = Lock::take(&config.db, &Holder::new(kind)).map_err(Refusal::busy)?;
        // Taken under the lock, so an import marking it can't land in between.
        if let Some(moved) = flotilla_core::owner::moved(&config.db) {
            return Err(Refusal {
                reason: moved.reason(),
                instead: Some(Instead::Moved(moved)),
            });
        }
        // A demo starts from its own threads again, under the lock.
        if matches!(config.mode, Mode::Demo) {
            flotilla_engine::demo::reset(&config.db).map_err(Refusal::from)?;
        }
        let mut bound = Vec::new();
        let (mut tcp, mut socket, mut doorway, mut mcp) = (None, None, None, None);
        let mut mcp_url = Arc::default();
        for listen in &config.listen {
            match listen {
                Listen::Tcp(addr) => {
                    let l = tokio::net::TcpListener::bind(addr)
                        .await
                        .map_err(|e| format!("Couldn't listen on {addr}: {e}"))?;
                    tcp = Some(l.local_addr().map_err(|e| e.to_string())?);
                    bound.push(Bound::Tcp(l));
                }
                Listen::Unix(path) => match bind_socket(path) {
                    Ok(l) => {
                        socket = Some(path.clone());
                        bound.push(Bound::Unix(l, path.clone()));
                    }
                    // The app runs without it: only the CLI goes that way.
                    Err(e) if config.role == Role::App => {
                        eprintln!("[server] no socket for the CLI: {e}")
                    }
                    Err(e) => return Err(e.into()),
                },
                Listen::Memory => {
                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                    doorway = Some(Doorway(tx));
                    bound.push(Bound::Memory(Inside(rx)));
                }
                Listen::Mcp(addr, url) => {
                    let l = tokio::net::TcpListener::bind(addr)
                        .await
                        .map_err(|e| format!("Couldn't listen for remote MCP on {addr}: {e}"))?;
                    mcp = Some(l.local_addr().map_err(|e| e.to_string())?);
                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                    mcp_url = Arc::new(Mutex::new(
                        url.as_deref().map(|u| u.trim_end_matches('/').to_string()),
                    ));
                    bound.push(Bound::Mcp {
                        listener: l,
                        url: mcp_url.clone(),
                        door: Doorway(tx),
                        inside: Inside(rx),
                        db: config.db.clone(),
                    });
                }
            }
        }
        // Where to find it, for the CLI: the socket, which needs no token.
        let address = match (&socket, &tcp) {
            (Some(p), _) => Some(format!("unix:{}", p.display())),
            (None, Some(a)) if config.role == Role::Server => Some(format!("ws://{a}")),
            _ => None,
        };
        lock.write(&Holder {
            address: address.clone(),
            ..Holder::new(kind)
        })
        .map_err(|e| e.to_string())?;
        let blobs = flotilla_core::blobs::Blobs::beside(&config.db);
        let host = Arc::new(Host::start_as(config.db, config.mode, lock, config.role));
        if let Some(addr) = tcp {
            host.serving(Some(addr.port()));
        }
        if let Some(addr) = mcp {
            host.serving_mcp(Some(addr.port()));
        }
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let app = App {
            host,
            blobs,
            trusted: false,
            stop: stopped,
            open: Arc::new(()),
            tries: Arc::default(),
        };
        Ok(Server {
            app,
            bound,
            address,
            tcp,
            socket,
            doorway,
            mcp,
            mcp_url,
            stop,
        })
    }

    /// Where the CLI reaches it: `unix:/path` (or `ws://127.0.0.1:7777` with no socket).
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    /// The way in from this process, if it listens there (`Listen::Memory`).
    pub fn doorway(&self) -> Option<Doorway> {
        self.doorway.clone()
    }

    /// The TCP address it's bound to, if any.
    pub fn tcp(&self) -> Option<SocketAddr> {
        self.tcp
    }

    /// Where remote MCP listens, if it does: only when asked ([`Listen::Mcp`]).
    pub fn mcp(&self) -> Option<SocketAddr> {
        self.mcp
    }

    /// The address remote MCP's clients use, once something publishes it (Funnel, as the server
    /// starts): what it tells them, for requests that come through it.
    pub fn publish_mcp(&self, url: &str) {
        *self.mcp_url.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(url.trim_end_matches('/').to_string());
    }

    pub fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    pub fn host(&self) -> &Host {
        &self.app.host
    }

    /// Serve until `shutdown` resolves; then close every connection and stop the host (which
    /// lets go of the database's lock) before returning.
    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), String> {
        let (_keep, control) = tokio::sync::mpsc::unbounded_channel();
        self.run_with(shutdown, control).await
    }

    /// [`run`](Server::run), taking what `control` says meanwhile: devices let in over TCP, or
    /// no longer.
    pub async fn run_with(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
        mut control: tokio::sync::mpsc::UnboundedReceiver<Control>,
    ) -> Result<(), String> {
        let Server {
            app,
            bound,
            stop,
            tcp,
            ..
        } = self;
        let mut serving = Vec::new();
        let mut sockets = Vec::new();
        for b in bound {
            let rx = app.stop.clone();
            match b {
                Bound::Tcp(l) => {
                    let router = router(App {
                        trusted: false,
                        ..app.clone()
                    })
                    .into_make_service_with_connect_info::<SocketAddr>();
                    serving.push(tokio::spawn(async move {
                        axum::serve(l, router)
                            .with_graceful_shutdown(stopping(rx))
                            .await
                            .map_err(|e| e.to_string())
                    }));
                }
                Bound::Unix(l, path) => {
                    sockets.push(path);
                    let router = router(App {
                        trusted: true,
                        ..app.clone()
                    });
                    serving.push(tokio::spawn(async move {
                        axum::serve(Owner(l), router)
                            .with_graceful_shutdown(stopping(rx))
                            .await
                            .map_err(|e| e.to_string())
                    }));
                }
                Bound::Memory(l) => {
                    let router = router(App {
                        trusted: true,
                        ..app.clone()
                    });
                    serving.push(tokio::spawn(async move {
                        axum::serve(l, router)
                            .with_graceful_shutdown(stopping(rx))
                            .await
                            .map_err(|e| e.to_string())
                    }));
                }
                Bound::Mcp {
                    listener,
                    url,
                    door,
                    inside,
                    db,
                } => {
                    // The pipe takes a device's token, as TCP does: never trusted.
                    let devices = router(App {
                        trusted: false,
                        ..app.clone()
                    });
                    let piped = rx.clone();
                    serving.push(tokio::spawn(async move {
                        axum::serve(inside, devices)
                            .with_graceful_shutdown(stopping(piped))
                            .await
                            .map_err(|e| e.to_string())
                    }));
                    let pipe = flotilla_client::Pipe::new(move || door.open());
                    let l = mcp::Listener::new(
                        App {
                            trusted: false,
                            ..app.clone()
                        },
                        pipe,
                        url,
                        db,
                    );
                    // Sign-ins not finished in time are looked for now and every so often. Waits
                    // under way end as the server stops, so their answers don't hold it up.
                    let (closing, mut stopped) = (l.clone(), rx.clone());
                    tokio::spawn(async move {
                        let mut every = tokio::time::interval(mcp::GRANT_SWEEP);
                        loop {
                            tokio::select! {
                                _ = stopped.wait_for(|stop| *stop) => break,
                                _ = every.tick() => closing.tidy(),
                            }
                        }
                        closing.close();
                    });
                    let router = mcp::router(l).into_make_service_with_connect_info::<SocketAddr>();
                    serving.push(tokio::spawn(async move {
                        axum::serve(listener, router)
                            .with_graceful_shutdown(stopping(rx))
                            .await
                            .map_err(|e| e.to_string())
                    }));
                }
            }
        }
        // Devices let in while running: their own stop, so closing them leaves the rest.
        let mut extra: Option<Extra> = None;
        let shutdown = std::pin::pin!(shutdown);
        let mut shutdown = shutdown;
        let mut listening = true;
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                c = control.recv(), if listening => {
                    // No one controls it now: serve as it is until the end.
                    let Some(c) = c else {
                        listening = false;
                        continue;
                    };
                    match c {
                        Control::Open(addr, reply) => {
                            if let Some(old) = extra.take() {
                                old.close().await;
                            }
                            let r = match tokio::net::TcpListener::bind(addr).await {
                                Ok(l) => match l.local_addr() {
                                    Ok(at) => {
                                        extra = Some(Extra::serve(l, &app));
                                        Ok(at)
                                    }
                                    Err(e) => Err(e.to_string()),
                                },
                                Err(e) => Err(format!("Couldn't listen on {addr}: {e}")),
                            };
                            let _ = reply.send(r);
                        }
                        Control::Close(reply) => {
                            if let Some(old) = extra.take() {
                                old.close().await;
                            }
                            let _ = reply.send(());
                        }
                        Control::Recheck => {}
                    }
                    // What the checks look at is what listens now, however the change went (an
                    // open that fails has closed the old door first), and they look again.
                    app.host.serving(extra.as_ref().map(|e| e.port).or(tcp.map(|a| a.port())));
                }
            }
        }
        let _ = stop.send(true);
        if let Some(old) = extra.take() {
            old.close().await;
        }
        let mut r = Ok(());
        for s in serving {
            match s.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => r = Err(e),
                Err(e) => r = Err(e.to_string()),
            }
        }
        for path in sockets {
            let _ = std::fs::remove_file(path);
        }
        // Upgraded connections outlive the server's own shutdown: they close on `stop`.
        for _ in 0..300 {
            if Arc::strong_count(&app.open) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(app);
        r
    }
}

/// Until the server's (or a listener's) stop is said.
async fn stopping(mut rx: tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

/// TCP for devices, opened while the server runs.
struct Extra {
    port: u16,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), String>>,
}

impl Extra {
    fn serve(l: tokio::net::TcpListener, app: &App) -> Extra {
        let port = l.local_addr().map(|a| a.port()).unwrap_or_default();
        let (stop, rx) = tokio::sync::watch::channel(false);
        // Its connections close on its own stop (and the server's: `run_with` says both).
        let router = router(App {
            trusted: false,
            stop: rx.clone(),
            ..app.clone()
        })
        .into_make_service_with_connect_info::<SocketAddr>();
        let task = tokio::spawn(async move {
            axum::serve(l, router)
                .with_graceful_shutdown(stopping(rx))
                .await
                .map_err(|e| e.to_string())
        });
        Extra { port, stop, task }
    }

    async fn close(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(3), self.task).await;
    }
}

/// Listen on a Unix socket at `path`, 0600, in a directory made 0700 if it's new. A stale one
/// (left by a server that's gone: the caller holds the lock) is removed, unless something
/// answers on it.
fn bind_socket(path: &Path) -> Result<tokio::net::UnixListener, String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if path.as_os_str().len() > 100 {
        return Err(format!(
            "The socket's path is too long for macOS ({}): choose a shorter one with --socket",
            path.display()
        ));
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(format!(
                "Something is already listening on {}",
                path.display()
            ));
        }
        std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if let Some(dir) = path.parent()
        && !dir.exists()
    {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let l = tokio::net::UnixListener::bind(path)
        .map_err(|e| format!("Couldn't listen on {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(l)
}

/// This process's user.
fn euid() -> u32 {
    // SAFETY: geteuid can't fail and touches nothing.
    unsafe { libc::geteuid() }
}

/// Whether a socket's peer, by its uid, may in: only the user this server runs as.
pub fn peer_allowed(uid: u32) -> bool {
    uid == euid()
}

/// The Unix socket, taking only connections from this server's own user (`getpeereid`).
struct Owner(tokio::net::UnixListener);

impl axum::serve::Listener for Owner {
    type Io = tokio::net::UnixStream;
    type Addr = tokio::net::unix::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((io, addr)) => match io.peer_cred() {
                    Ok(cred) if peer_allowed(cred.uid()) => return (io, addr),
                    Ok(cred) => eprintln!(
                        "[server] refused a socket connection from uid {}",
                        cred.uid()
                    ),
                    Err(e) => {
                        eprintln!("[server] refused a socket connection: no peer credentials ({e})")
                    }
                },
                // Out of file descriptors and the like: wait, rather than spin.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

/// Run a server until Ctrl-C (or a TERM), once `on_ready` has had a look at it.
pub fn serve(
    config: Config,
    on_ready: impl FnOnce(&Server) -> Result<(), String>,
) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let server = Server::bind(config).await?;
        on_ready(&server)?;
        server
            .run(async {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending().await } } => {}
                }
            })
            .await
    })?;
    // The connections' tasks go with the runtime, and the host with them.
    drop(rt);
    Ok(())
}

/// The server the app on This Mac runs inside itself, on its own thread: the app's window
/// reaches it through [`Local::doorway`], the CLI through its Unix socket, other devices over
/// TCP only while [`Access`] lets them. Dropping it closes all of that, stops the engine and
/// lets go of the database's lock before it returns, so another server can take it at once.
pub struct Local {
    doorway: Doorway,
    socket: Option<PathBuf>,
    access: Access,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Local {
    /// Take `db`'s lock and serve it (on `socket` too, for the CLI, if it can be had). Fails
    /// if another Flotilla holds the database, or it moved to another: the refusal says what
    /// to use instead when there's something.
    pub fn start(db: PathBuf, socket: Option<PathBuf>, mode: Mode) -> Result<Local, Refusal> {
        // A link given out by a run that's gone (or by `flotilla serve`) doesn't lead here:
        // devices are let in afresh.
        let (ready, started) =
            std::sync::mpsc::channel::<Result<(Doorway, Option<PathBuf>), Refusal>>();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (control, controlled) = tokio::sync::mpsc::unbounded_channel();
        let path = db.clone();
        let thread = std::thread::Builder::new()
            .name("flotilla-local".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("flotilla-local")
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready.send(Err(e.to_string().into()));
                        return;
                    }
                };
                rt.block_on(async move {
                    let mut listen = vec![Listen::Memory];
                    listen.extend(socket.map(Listen::Unix));
                    let server = match Server::bind_or_refuse(Config {
                        db: path,
                        listen,
                        mode,
                        role: Role::App,
                    })
                    .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = ready.send(Err(e));
                            return;
                        }
                    };
                    let Some(doorway) = server.doorway() else {
                        return;
                    };
                    let _ = ready.send(Ok((doorway, server.socket().map(Path::to_path_buf))));
                    if let Err(e) = server
                        .run_with(
                            async {
                                let _ = stopped.await;
                            },
                            controlled,
                        )
                        .await
                    {
                        eprintln!("[server] {e}");
                    }
                });
                // The connections' tasks go with the runtime, and the host (and its lock) with them.
                drop(rt);
            })
            .map_err(|e| e.to_string())?;
        let (doorway, socket) = match started.recv() {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => return Err("This app's server didn't start".to_string().into()),
        };
        if let Ok(store) = flotilla_core::store::Store::open(&db) {
            let _ = store.kv_delete(flotilla_engine::host::SERVER_URL_KEY);
        }
        let access = Access {
            control,
            db,
            state: Arc::default(),
        };
        Ok(Local {
            doorway,
            socket,
            access,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// The way in for this process's own client.
    pub fn doorway(&self) -> Doorway {
        self.doorway.clone()
    }

    /// The Unix socket the CLI reaches it on, if it has one.
    pub fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// Letting other devices in, from any thread.
    pub fn access(&self) -> Access {
        self.access.clone()
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        self.access.shut();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Where other devices reach an app's server, once it lets them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// What pairing links give out: `https://<this Mac>.<tailnet>.ts.net`, or
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    /// Asked for Tailscale, but it didn't forward: why (devices on this Mac can still connect).
    pub tailscale: Option<String>,
}

/// Letting other devices in to an app's server ([`Local::access`]), as a server lets them in:
/// over TCP on this machine, with a paired device's token, and through Tailscale's HTTPS if
/// asked. Cloneable, for a thread that can wait on Tailscale.
#[derive(Clone)]
pub struct Access {
    control: tokio::sync::mpsc::UnboundedSender<Control>,
    db: PathBuf,
    state: Arc<Mutex<Doors>>,
}

#[derive(Default)]
struct Doors {
    /// The port Flotilla had Tailscale forward to, until that's removed.
    forwarding: Option<u16>,
    /// The server is stopping: nothing opens again.
    shut: bool,
}

impl Access {
    /// Listen for devices on 127.0.0.1:`port` (in place of any listening now), and through
    /// Tailscale's HTTPS when `tailscale`. Blocks while Tailscale is asked (up to 20 s), so
    /// call it off the UI's thread. One open or close at a time, and none once the server is
    /// stopping: its links and forwarding are never written after that. One the server can't
    /// open leaves the door shut, as [`close`](Access::close) does (the old one closed first).
    pub fn open(&self, port: u16, tailscale: bool) -> Result<Opened, String> {
        let stopped = || "This app's server has stopped".to_string();
        let mut doors = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if doors.shut {
            return Err(stopped());
        }
        let store = flotilla_core::store::Store::open(&self.db).map_err(|e| e.to_string())?;
        // The checks look at Tailscale only when it's asked for.
        store
            .kv_set(
                flotilla_core::tailscale::KEY,
                if tailscale { "on" } else { "off" },
            )
            .map_err(|e| e.to_string())?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.control
            .send(Control::Open(SocketAddr::from(([127, 0, 0, 1], port)), tx))
            .map_err(|_| stopped())?;
        // A server that's gone answers nothing, and what it gave out goes with it (`shut`).
        let bound = rx.blocking_recv().map_err(|_| stopped())?;
        match bound.and_then(|at| self.settle(&mut doors, Some((at, tailscale)))) {
            Ok(Some(opened)) => Ok(opened),
            // Not listening, or its links can't be written: shut, as closing leaves it.
            other => {
                self.close_locked(&mut doors);
                Err(other.err().unwrap_or_else(stopped))
            }
        }
    }

    /// No more devices: those connected now are dropped, Tailscale stops forwarding, and
    /// pairing codes can't be made.
    pub fn close(&self) {
        let mut doors = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !doors.shut {
            self.close_locked(&mut doors);
        }
    }

    /// The server is stopping: close (after any open under way), and open nothing more.
    fn shut(&self) {
        let mut doors = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !doors.shut {
            doors.shut = true;
            self.close_locked(&mut doors);
        }
    }

    fn close_locked(&self, doors: &mut Doors) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.control.send(Control::Close(tx)).is_ok() {
            let _ = rx.blocking_recv();
        }
        let _ = self.settle(doors, None);
    }

    /// Tailscale's forward and the pairing links follow what listens now: `open` (where, and
    /// whether through Tailscale), or nothing. Every open and close ends here, so neither
    /// says a door is open once it's shut. Only a forward to what listens stays.
    fn settle(
        &self,
        doors: &mut Doors,
        open: Option<(SocketAddr, bool)>,
    ) -> Result<Option<Opened>, String> {
        let keep = open
            .filter(|&(_, tailscale)| tailscale)
            .map(|(at, _)| at.port());
        let unforwarded = if doors.forwarding != keep {
            unforward(doors)
        } else {
            Ok(())
        };
        if let Err(e) = &unforwarded {
            eprintln!("[server] {e}");
        }
        let opened = open.map(|(at, tailscale)| {
            // An old forward still there would read as someone else's: why it's there instead.
            match tailscale
                .then(|| unforwarded.and_then(|()| flotilla_core::tailscale::serve(at.port())))
            {
                Some(Ok(url)) => {
                    doors.forwarding = Some(at.port());
                    Opened {
                        url,
                        tailscale: None,
                    }
                }
                why => Opened {
                    url: format!("http://{at}"),
                    tailscale: why.and_then(Result::err),
                },
            }
        });
        let store = flotilla_core::store::Store::open(&self.db).map_err(|e| e.to_string())?;
        match &opened {
            Some(o) => store
                .kv_set(flotilla_engine::host::SERVER_URL_KEY, &o.url)
                .map_err(|e| e.to_string())?,
            None => {
                let _ = store.kv_delete(flotilla_engine::host::SERVER_URL_KEY);
            }
        }
        // The checks look at Tailscale only while devices are let in: again, now it forwards.
        if opened.is_some() {
            let _ = self.control.send(Control::Recheck);
        }
        Ok(opened)
    }
}

/// Tailscale stops forwarding, if it was. A forward that couldn't be removed is still
/// Flotilla's: kept, so the next open or close tries again.
fn unforward(doors: &mut Doors) -> Result<(), String> {
    if let Some(port) = doors.forwarding {
        flotilla_core::tailscale::off(port)?;
        doors.forwarding = None;
    }
    Ok(())
}

fn router(app: App) -> Router {
    Router::new()
        .route(WS_PATH, get(ws))
        .route("/v1/health", get(health))
        .route("/.well-known/flotilla", get(descriptor))
        .route(PAIR_PATH, post(pair).layer(DefaultBodyLimit::max(16 << 10)))
        .route(
            "/v1/blobs",
            put(blob).layer(DefaultBodyLimit::max(BLOB_MAX)),
        )
        .route(HISTORY_PATH, get(history))
        .route(FULL_PATH, get(full))
        .layer(middleware::from_fn(guard))
        .with_state(app)
}

/// No browsers (they send `Origin`) until there's a web client.
async fn guard(req: Request, next: Next) -> Response {
    if req.headers().contains_key(header::ORIGIN) {
        return (
            StatusCode::FORBIDDEN,
            "Flotilla doesn't take requests from web pages yet",
        )
            .into_response();
    }
    next.run(req).await
}

/// Where a TCP request comes from, to show beside the device and to count limits by: behind a
/// proxy that connects from loopback (`tailscale serve`, Funnel, a tunnel) the address it says it
/// forwarded for. That's the last one the header lists (in its last line), the one the proxy
/// added: a proxy that adds to the header rather than replacing it keeps what the client sent
/// before it.
fn ip_of(ext: &Extensions, headers: &HeaderMap) -> Option<String> {
    let peer = ext.get::<ConnectInfo<SocketAddr>>()?.0.ip();
    let forwarded = headers
        .get_all("x-forwarded-for")
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .map(str::trim);
    match forwarded {
        Some(f) if peer.is_loopback() && f.parse::<IpAddr>().is_ok() => Some(f.to_string()),
        _ => Some(peer.to_string()),
    }
}

/// The token in `Authorization: Bearer …`.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, token) = v.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() && token.len() <= 256)
        .then(|| token.to_string())
}

fn refused(status: StatusCode, msg: String) -> Response {
    let mut r = (status, msg).into_response();
    if status == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
    }
    r
}

/// Who a request is: this machine's user (the socket), or a paired device by its token. If
/// neither, what to answer.
async fn peer(
    app: &App,
    headers: &HeaderMap,
    ip: Option<String>,
) -> Result<Peer, (StatusCode, String)> {
    if app.trusted {
        return Ok(Peer::Local);
    }
    let Some(token) = bearer(headers) else {
        return Err((StatusCode::UNAUTHORIZED, AuthError::Unknown.to_string()));
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.host.authenticate(token, ip.clone(), move |r| {
        let _ = tx.send(r);
    });
    match rx.await {
        Ok(Ok(device)) if device.scope == flotilla_proto::Scope::Agent => {
            Ok(Peer::Agent(device.id))
        }
        Ok(Ok(device)) => Ok(Peer::Device(device.id)),
        Ok(Err(e @ (AuthError::Unknown | AuthError::Lapsed))) => {
            eprintln!(
                "[server] refused a device from {}: {e}",
                ip.as_deref().unwrap_or("?")
            );
            Err((StatusCode::UNAUTHORIZED, e.to_string()))
        }
        Ok(Err(e)) => Err((StatusCode::SERVICE_UNAVAILABLE, e.to_string())),
        Err(_) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "The server is stopping".into(),
        )),
    }
}

async fn health(State(app): State<App>) -> Json<serde_json::Value> {
    let info = app.host.info();
    Json(
        serde_json::json!({"ok": true, "server": info.id, "version": info.version, "protocol": PROTOCOL}),
    )
}

async fn descriptor(State(app): State<App>) -> Json<Descriptor> {
    Json(Descriptor {
        server: app.host.info().clone(),
        protocol: PROTOCOL,
        capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
    })
}

/// Swap a pairing code for a device token. Wrong codes are limited for each address and, more
/// loosely, across every address; while a limit is reached no code it covers is checked at all.
async fn pair(
    State(app): State<App>,
    ext: Extensions,
    headers: HeaderMap,
    body: Result<Json<PairRequest>, JsonRejection>,
) -> Response {
    let Ok(Json(req)) = body else {
        return refused(
            StatusCode::BAD_REQUEST,
            "Send {code, device_name, platform}".into(),
        );
    };
    let ip = ip_of(&ext, &headers);
    let taken = app
        .tries
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take(&pair_source(ip.as_deref()), Instant::now());
    let me = match taken {
        Ok(me) => me,
        Err(log) => {
            if let Some(log) = log {
                eprintln!("[server] {log}");
            }
            return refused(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many wrong pairing codes: wait a minute, then try again".into(),
            );
        }
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let name = req.device_name.clone();
    app.host.pair(req, ip.clone(), move |r| {
        let _ = tx.send(r);
    });
    let result = rx
        .await
        .unwrap_or_else(|_| Err(PairError::Db("the server is stopping".into())));
    if !matches!(&result, Err(e) if e.guessing()) {
        app.tries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .at
            .retain(|(n, _, _)| *n != me);
    }
    let from = ip.as_deref().unwrap_or("this machine");
    match result {
        Ok(p) => {
            let agent = if p.scope == flotilla_proto::Scope::Agent {
                " as an agent"
            } else {
                ""
            };
            eprintln!(
                "[server] paired {:?} (device {}){agent} from {from}",
                flotilla_core::util::ellipsize(&name, 40),
                p.device
            );
            let paired = Paired {
                device: p.device,
                token: Secret(p.token),
                server: app.host.info().clone(),
            };
            ([(header::CACHE_CONTROL, "no-store")], Json(paired)).into_response()
        }
        Err(e) => {
            eprintln!("[server] didn't pair a device from {from}: {e}");
            let status = match &e {
                PairError::Wrong => StatusCode::UNAUTHORIZED,
                PairError::Used | PairError::Expired | PairError::Revoked => StatusCode::GONE,
                PairError::Invalid(_) => StatusCode::BAD_REQUEST,
                PairError::Db(_) => StatusCode::SERVICE_UNAVAILABLE,
            };
            refused(status, e.to_string())
        }
    }
}

/// Keep a file, named by its contents. Who's asking is checked before the body is read, so a
/// stranger can't make the server hold a file's worth of memory per connection.
async fn blob(
    State(app): State<App>,
    ext: Extensions,
    headers: HeaderMap,
    req: Request,
) -> Response {
    match peer(&app, &headers, ip_of(&ext, &headers)).await {
        Err((status, msg)) => {
            drain_small(&headers, req.into_body()).await;
            return refused(status, msg);
        }
        // An agent sends no files.
        Ok(Peer::Agent(_)) => {
            drain_small(&headers, req.into_body()).await;
            return refused(StatusCode::FORBIDDEN, "Agents can't send files".into());
        }
        Ok(_) => {}
    }
    let body = match axum::body::Bytes::from_request(req, &app).await {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "An empty file").into_response();
    }
    let blobs = app.blobs.clone();
    match tokio::task::spawn_blocking(move || blobs.put(&body)).await {
        Ok(Ok(blob)) => Json(blob).into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Couldn't keep the file: {e}"),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Couldn't keep the file: {e}"),
        )
            .into_response(),
    }
}

/// A refused request's body (an upload's, remote MCP's), read and dropped if it's small, before
/// the answer: a connection closed on bytes not read is reset, and the reset can lose the answer.
/// A big one isn't waited for (nor is any of it ever kept).
pub(crate) async fn drain_small(headers: &HeaderMap, body: axum::body::Body) {
    const SMALL: u64 = 1 << 20;
    let len = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if !len.is_some_and(|n| n <= SMALL) {
        return;
    }
    let mut body = body.into_data_stream();
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(_)) = body.next().await {}
    })
    .await;
}

/// Why a read got nothing, as HTTP.
fn read_refused(e: ReadError) -> Response {
    let status = match e {
        ReadError::Unpaired => StatusCode::UNAUTHORIZED,
        ReadError::Missing(_) => StatusCode::NOT_FOUND,
        ReadError::Busy(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    refused(status, e.to_string())
}

/// `GET /v1/threads/:id/history?before=<seq>`: the page of older messages that ends at
/// `before`, cut as the thread's view is, with its mark (`flotilla_proto::history`). It needs a
/// device's token over TCP, as everything else does.
async fn history(
    State(app): State<App>,
    ext: Extensions,
    headers: HeaderMap,
    UrlPath(thread): UrlPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let peer = match peer(&app, &headers, ip_of(&ext, &headers)).await {
        Ok(p) => p,
        Err((status, msg)) => return refused(status, msg),
    };
    let before = query
        .get("before")
        .and_then(|b| b.parse::<i64>().ok())
        .filter(|b| *b > 0 && b % flotilla_proto::history::PAGE == 0);
    let Some(before) = before else {
        return refused(
            StatusCode::BAD_REQUEST,
            format!(
                "Say which page: ?before=<seq>, a multiple of {} above 0",
                flotilla_proto::history::PAGE
            ),
        );
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.host.history(
        peer,
        thread,
        before / flotilla_proto::history::PAGE - 1,
        move |r| {
            let _ = tx.send(r);
        },
    );
    match rx
        .await
        .unwrap_or_else(|_| Err(ReadError::Busy("The server is stopping".into())))
    {
        Ok(page) => ([(header::CACHE_CONTROL, "no-store")], Json(page)).into_response(),
        Err(e) => read_refused(e),
    }
}

/// `GET /v1/threads/:id/full?seq=<seq>&field=<field>&mark=<mark>`: a string a message carries
/// cut short, whole, as text. 404 when it's no longer in the thread.
async fn full(
    State(app): State<App>,
    ext: Extensions,
    headers: HeaderMap,
    UrlPath(thread): UrlPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let peer = match peer(&app, &headers, ip_of(&ext, &headers)).await {
        Ok(p) => p,
        Err((status, msg)) => return refused(status, msg),
    };
    let (Some(seq), Some(field), Some(mark)) = (
        query.get("seq").and_then(|s| s.parse::<i64>().ok()),
        query.get("field"),
        query.get("mark"),
    ) else {
        return refused(
            StatusCode::BAD_REQUEST,
            "Say which string: ?seq=<seq>&field=<field>&mark=<mark>".into(),
        );
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.host
        .full(peer, thread, seq, field.clone(), mark.clone(), move |r| {
            let _ = tx.send(r);
        });
    match rx
        .await
        .unwrap_or_else(|_| Err(ReadError::Busy("The server is stopping".into())))
    {
        Ok(text) => (
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            text,
        )
            .into_response(),
        Err(e) => read_refused(e),
    }
}

async fn ws(
    State(app): State<App>,
    ext: Extensions,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let proto = headers
        .get(PROTOCOL_HEADER)
        .and_then(|v| v.to_str().ok())
        .or(query.get("proto").map(String::as_str))
        .and_then(|v| v.trim().parse::<u32>().ok());
    if proto != Some(PROTOCOL) {
        let msg = match proto {
            Some(p) => format!(
                "This server speaks Flotilla protocol {PROTOCOL}, not {p}: update Flotilla here or on the server"
            ),
            None => {
                format!("Say which Flotilla protocol you speak ({PROTOCOL_HEADER}: {PROTOCOL})")
            }
        };
        return (
            StatusCode::UPGRADE_REQUIRED,
            [(PROTOCOL_HEADER, PROTOCOL.to_string())],
            msg,
        )
            .into_response();
    }
    let peer = match peer(&app, &headers, ip_of(&ext, &headers)).await {
        Ok(p) => p,
        Err((status, msg)) => return refused(status, msg),
    };
    match upgrade {
        Ok(u) => u
            .max_message_size(BLOB_MAX)
            .on_upgrade(move |socket| connection(app, socket, peer)),
        Err(e) => e.into_response(),
    }
}

/// A message waiting to be written to a client: its text (and whether it's the last, an
/// `error`), or that a thread's streaming text is due, kept beside the queue (`Outgoing`).
enum Out {
    Text { text: String, last: bool },
    Live { thread: String, version: u64 },
}

impl Out {
    fn text(d: &Down) -> Out {
        Out::Text {
            text: serde_json::to_string(d).unwrap_or_default(),
            last: matches!(d, Down::Error { .. }),
        }
    }
}

/// What waits for one client beyond the queue's count: the bytes queued, and each thread's
/// latest streaming text. That's whole each time and never replayed, so only the latest is
/// kept: one a slow client hasn't been written yet is replaced, not queued again.
#[derive(Default)]
struct Outgoing {
    bytes: std::sync::atomic::AtomicUsize,
    live: Mutex<Latest>,
}

/// The last number given, and each thread's latest streaming text: its number and its text.
type Latest = (u64, HashMap<String, (u64, String)>);

impl Outgoing {
    /// `d` counted in, as what to queue; None when the client is too far behind for it.
    fn add(&self, d: &Down) -> Option<Out> {
        use std::sync::atomic::Ordering;
        let Out::Text { text, last } = Out::text(d) else {
            return None;
        };
        let size = text.len();
        let queued = self.bytes.load(Ordering::SeqCst);
        if queued > 0 && queued + size > OUTBOX_BYTES {
            return None;
        }
        self.bytes.fetch_add(size, Ordering::SeqCst);
        let Down::Live { thread, .. } = d else {
            return Some(Out::Text { text, last });
        };
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        live.0 += 1;
        let version = live.0;
        if let Some((_, old)) = live.1.insert(thread.clone(), (version, text)) {
            self.bytes.fetch_sub(old.len(), Ordering::SeqCst);
        }
        Some(Out::Live {
            thread: thread.clone(),
            version,
        })
    }

    /// Queue `d` on `tx`, counted in: false when the client is too far behind for it (then
    /// it isn't counted).
    fn queue(&self, tx: &tokio::sync::mpsc::Sender<Out>, d: &Down) -> bool {
        let Some(out) = self.add(d) else { return false };
        match tx.try_send(out) {
            Ok(()) => true,
            Err(e) => {
                let _ = self.take(e.into_inner());
                false
            }
        }
    }

    /// What to write for `out`, counted out: None for streaming text a newer one replaced.
    fn take(&self, out: Out) -> Option<(String, bool)> {
        use std::sync::atomic::Ordering;
        let (text, last) = match out {
            Out::Text { text, .. } if text.is_empty() => return None,
            Out::Text { text, last } => (text, last),
            Out::Live { thread, version } => {
                let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
                if live.1.get(&thread).is_none_or(|(v, _)| *v != version) {
                    return None;
                }
                (live.1.remove(&thread)?.1, false)
            }
        };
        self.bytes.fetch_sub(text.len(), Ordering::SeqCst);
        Some((text, last))
    }
}

/// One client: what it sends goes to the host; what the host has for it is written out. The
/// host closes it (an `error`, then the socket) when its device is revoked.
async fn connection(app: App, socket: WebSocket, peer: Peer) {
    let _open = app.open.clone();
    let mut stop = app.stop.clone();
    if *stop.borrow() {
        return;
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Out>(OUTBOX);
    let outgoing = Arc::new(Outgoing::default());
    let (to_client, queued) = (tx.clone(), outgoing.clone());
    // A client that falls this far behind is let go: the host stops sending to it, and the
    // socket closes so the client reconnects and catches up.
    let behind = Arc::new(tokio::sync::Notify::new());
    let lag = behind.clone();
    let conn = app.host.connect_as(
        Outbox::new(move |d| {
            let ok = queued.queue(&to_client, &d);
            if !ok {
                lag.notify_one();
            }
            ok
        }),
        peer,
    );
    let replies = outgoing.clone();
    let (mut sink, mut stream) = socket.split();
    let mut writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            let Some((text, last)) = outgoing.take(out) else {
                continue;
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
            if last {
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
        }
    });
    let mut said_hello = false;
    loop {
        let wait = if said_hello { IDLE } else { HELLO_WITHIN };
        let next = tokio::select! {
            m = tokio::time::timeout(wait, stream.next()) => m,
            _ = &mut writer => break,
            _ = stop.changed() => break,
            _ = behind.notified() => break,
        };
        let text = match next {
            Ok(Some(Ok(Message::Text(t)))) => t,
            Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
            Ok(Some(Ok(_))) => continue,
        };
        match serde_json::from_str::<Up>(&text) {
            Ok(up) => {
                said_hello |= matches!(up, Up::Hello(_));
                if said_hello {
                    conn.send(up);
                }
            }
            // From a newer client: a request this server doesn't know is refused, so the
            // client isn't left waiting; anything else is skipped.
            Err(e) => {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
                if v["t"] == "req"
                    && let Some(id) = v["id"].as_u64()
                {
                    let _ = replies.queue(&tx, &Down::Res { id, result: Err(format!("The server doesn't know that request ({e}): update Flotilla on the server")), n: None });
                }
            }
        }
    }
    drop(conn);
    writer.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The OS's part in starting again on the same port right after a stop: every listener is
    /// close-on-exec, so a program another thread starts (`security`, `gh`, `git`) doesn't keep
    /// it open, and a TCP one reuses its address, so connections a stop left in TIME_WAIT don't
    /// hold the port. tokio sets both on every listener it makes; one made another way must too.
    /// (A copy of the socket a program has while it starts, before it runs, can't be helped: it
    /// lasts that moment.)
    #[test]
    fn listeners_are_close_on_exec_and_reuse_their_address() {
        use std::os::fd::AsRawFd;
        let dir = std::env::temp_dir().join(format!("flotilla-listeners-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listen = vec![
                Listen::Tcp("127.0.0.1:0".parse().unwrap()),
                Listen::Unix(dir.join("s.sock")),
                Listen::Mcp("127.0.0.1:0".parse().unwrap(), None),
            ];
            let server = Server::bind(Config {
                db: dir.join("flotilla.db"),
                listen,
                mode: Mode::Fake(Arc::new(|_| {})),
                role: Role::Server,
            })
            .await
            .unwrap();
            // As devices are let in on This Mac (`Control::Open`).
            let devices = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut fds: Vec<(i32, bool)> = server
                .bound
                .iter()
                .filter_map(|b| match b {
                    Bound::Tcp(l) | Bound::Mcp { listener: l, .. } => Some((l.as_raw_fd(), true)),
                    Bound::Unix(l, _) => Some((l.as_raw_fd(), false)),
                    Bound::Memory(_) => None,
                })
                .collect();
            fds.push((devices.as_raw_fd(), true));
            assert_eq!(fds.len(), 4);
            for (fd, tcp) in fds {
                // SAFETY: reads flags of a descriptor this test holds open.
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                assert!(
                    flags >= 0 && flags & libc::FD_CLOEXEC != 0,
                    "listener {fd} isn't close-on-exec"
                );
                if tcp {
                    let mut on: libc::c_int = 0;
                    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                    // SAFETY: `on` and `len` are a c_int and its size, as SO_REUSEADDR reads.
                    let r = unsafe {
                        libc::getsockopt(
                            fd,
                            libc::SOL_SOCKET,
                            libc::SO_REUSEADDR,
                            (&mut on as *mut libc::c_int).cast(),
                            &mut len,
                        )
                    };
                    assert!(r == 0 && on != 0, "listener {fd} doesn't reuse its address");
                }
            }
        });
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Remote MCP is off unless it's asked for: a server listens where it's told and nowhere
    /// else, and has no MCP listener (nor its pipe) at all.
    #[test]
    fn remote_mcp_listens_only_when_asked() {
        let dir = std::env::temp_dir().join(format!("flotilla-no-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listen = vec![
                Listen::Tcp("127.0.0.1:0".parse().unwrap()),
                Listen::Unix(dir.join("s.sock")),
            ];
            let server = Server::bind(Config {
                db: dir.join("flotilla.db"),
                listen,
                mode: Mode::Fake(Arc::new(|_| {})),
                role: Role::Server,
            })
            .await
            .unwrap();
            assert_eq!(server.mcp(), None);
            let kinds: Vec<&str> = server
                .bound
                .iter()
                .map(|b| match b {
                    Bound::Tcp(_) => "tcp",
                    Bound::Unix(..) => "unix",
                    Bound::Memory(_) => "memory",
                    Bound::Mcp { .. } => "mcp",
                })
                .collect();
            assert_eq!(kinds, ["tcp", "unix"]);
            drop(server);
            let listen = vec![
                Listen::Unix(dir.join("t.sock")),
                Listen::Mcp("127.0.0.1:0".parse().unwrap(), None),
            ];
            let server = Server::bind(Config {
                db: dir.join("flotilla.db"),
                listen,
                mode: Mode::Fake(Arc::new(|_| {})),
                role: Role::Server,
            })
            .await
            .unwrap();
            assert!(
                server
                    .mcp()
                    .is_some_and(|a| a.ip().is_loopback() && a.port() != 0)
            );
            // Remote MCP is no way in for the CLI: its address stays the socket.
            assert!(server.address().is_some_and(|a| a.starts_with("unix:")));
        });
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn listens_only_on_this_machine_unless_told_insecure() {
        assert!(Listen::tcp("127.0.0.1", 7777, false).is_ok());
        assert!(Listen::tcp("::1", 7777, false).is_ok());
        assert!(Listen::tcp("[::1]", 7777, false).is_ok());
        assert_eq!(
            Listen::tcp("localhost", 1, false).unwrap(),
            Listen::Tcp("127.0.0.1:1".parse().unwrap())
        );
        for h in ["0.0.0.0", "192.168.1.5", "::", "100.64.0.1"] {
            let e = Listen::tcp(h, 7777, false).unwrap_err();
            assert!(e.contains("--insecure"), "{e}");
            assert_eq!(
                Listen::tcp(h, 7777, true).unwrap(),
                Listen::Tcp(SocketAddr::new(h.parse().unwrap(), 7777))
            );
        }
        assert!(Listen::tcp("example.com", 7777, true).is_err());
    }

    #[test]
    fn only_this_servers_own_user_may_use_the_socket() {
        assert!(peer_allowed(euid()));
        assert!(!peer_allowed(euid() + 1));
        assert!(!peer_allowed(0) || euid() == 0);
    }

    #[test]
    fn wrong_pairing_codes_are_limited_by_address_then_across_every_address() {
        let mut tries = Tries::default();
        let now = Instant::now();
        for _ in 0..PAIR_TRIES {
            assert!(tries.take("203.0.113.9", now).is_ok());
        }
        // That address waits, said once in the log; another doesn't.
        assert_eq!(
            tries
                .take("203.0.113.9", now)
                .unwrap_err()
                .map(|l| l.contains("203.0.113.9")),
            Some(true)
        );
        assert_eq!(tries.take("203.0.113.9", now), Err(None));
        assert!(tries.take("198.51.100.1", now).is_ok());
        // A minute on, it's let try again.
        assert!(
            tries
                .take("203.0.113.9", now + PAIR_WINDOW + Duration::from_secs(1))
                .is_ok()
        );
        // Many addresses together reach the backstop, said once too.
        let mut tries = Tries::default();
        for i in 0..PAIR_TRIES_ALL {
            assert!(
                tries
                    .take(&format!("10.0.{}.{}", i / 200, i % 200), now)
                    .is_ok()
            );
        }
        assert!(tries.take("10.9.9.9", now).unwrap_err().is_some());
        assert_eq!(tries.take("10.9.9.10", now), Err(None));
    }

    #[test]
    fn pairing_tries_count_an_ipv6_address_by_its_64() {
        assert_eq!(
            pair_source(Some("2001:db8:1:2:aaaa::1")),
            pair_source(Some("2001:db8:1:2:bbbb::2"))
        );
        assert_ne!(
            pair_source(Some("2001:db8:1:2::1")),
            pair_source(Some("2001:db8:1:3::1"))
        );
        assert_eq!(pair_source(Some("::ffff:192.0.2.1")), "192.0.2.1");
        assert_eq!(pair_source(Some("192.0.2.1")), "192.0.2.1");
        assert_eq!(pair_source(None), "this machine");
    }

    #[test]
    fn a_request_is_from_the_address_the_proxy_in_front_added() {
        let ext = |peer: &str| {
            let mut e = Extensions::new();
            e.insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            e
        };
        let mut h = HeaderMap::new();
        assert_eq!(
            ip_of(&ext("127.0.0.1:5000"), &h).as_deref(),
            Some("127.0.0.1")
        );
        h.insert(
            "x-forwarded-for",
            header::HeaderValue::from_static("203.0.113.7"),
        );
        assert_eq!(
            ip_of(&ext("127.0.0.1:5000"), &h).as_deref(),
            Some("203.0.113.7")
        );
        // What the client said before it isn't who it is.
        h.insert(
            "x-forwarded-for",
            header::HeaderValue::from_static("198.51.100.1, 10.0.0.1,203.0.113.7"),
        );
        assert_eq!(
            ip_of(&ext("127.0.0.1:5000"), &h).as_deref(),
            Some("203.0.113.7")
        );
        // Nor is a line of its own before the proxy's.
        h.insert(
            "x-forwarded-for",
            header::HeaderValue::from_static("198.51.100.1"),
        );
        h.append(
            "x-forwarded-for",
            header::HeaderValue::from_static("203.0.113.7"),
        );
        assert_eq!(
            ip_of(&ext("127.0.0.1:5000"), &h).as_deref(),
            Some("203.0.113.7")
        );
        // Only a proxy on this machine is taken at its word.
        assert_eq!(
            ip_of(&ext("192.0.2.4:5000"), &h).as_deref(),
            Some("192.0.2.4")
        );
    }

    /// A slow client's queue holds one copy of each thread's streaming text, the latest, in the
    /// place of the newest; and it's bounded by bytes, not only by count.
    #[test]
    fn a_slow_clients_queue_keeps_only_the_latest_streaming_text_and_is_bounded_by_bytes() {
        use std::sync::atomic::Ordering;
        let live = |t: &str, text: &str| Down::Live {
            thread: t.into(),
            partials: vec![flotilla_proto::RawEvent::from_value(
                serde_json::json!({"id": "e", "type": "response", "data": {"content": text}}),
            )],
        };
        let said = |o: Option<(String, bool)>| {
            o.map(|(t, _)| serde_json::from_str::<serde_json::Value>(&t).unwrap())
        };
        let q = Outgoing::default();
        let a = q.add(&live("t1", "a")).unwrap();
        let synced = q.add(&Down::Synced).unwrap();
        let other = q.add(&live("t2", "x")).unwrap();
        let b = q.add(&live("t1", "ab")).unwrap();
        // The first is replaced; what came between stays where it was.
        assert_eq!(q.take(a), None);
        assert_eq!(said(q.take(synced)).unwrap()["t"], "synced");
        assert!(said(q.take(other)).unwrap().to_string().contains("\"x\""));
        assert!(said(q.take(b)).unwrap().to_string().contains("\"ab\""));
        assert_eq!(q.bytes.load(Ordering::SeqCst), 0);
        // Many copies of a growing text hold one.
        let mut outs: Vec<Out> = (0..1000)
            .map(|i| q.add(&live("t1", &"z".repeat(i * 100))).unwrap())
            .collect();
        assert!(
            q.bytes.load(Ordering::SeqCst) < 200_000,
            "{}",
            q.bytes.load(Ordering::SeqCst)
        );
        let last = outs.pop().unwrap();
        assert!(outs.into_iter().all(|o| q.take(o).is_none()));
        assert!(q.take(last).is_some());
        // Bytes: a message bigger than the limit goes alone, but nothing more is queued after it.
        let big = Down::Error {
            msg: "e".repeat(OUTBOX_BYTES + 1),
        };
        let first = q.add(&big).unwrap();
        assert!(q.add(&Down::Synced).is_none());
        assert_eq!(q.take(first).map(|(_, last)| last), Some(true));
        assert!(q.add(&Down::Synced).is_some());
    }

    /// Everything queued goes through `queue`, counted in and out: one the channel has no room
    /// for isn't counted, and the count comes back to nothing once all is written.
    #[test]
    fn what_a_full_queue_refuses_isnt_counted() {
        use std::sync::atomic::Ordering;
        let q = Outgoing::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        assert!(q.queue(&tx, &Down::Synced));
        let one = q.bytes.load(Ordering::SeqCst);
        assert!(!q.queue(
            &tx,
            &Down::Error {
                msg: "x".repeat(100)
            }
        ));
        assert_eq!(q.bytes.load(Ordering::SeqCst), one);
        assert!(q.take(rx.try_recv().unwrap()).is_some());
        assert_eq!(q.bytes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_bearer_token_is_read_from_its_header() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer fdt_abc".parse().unwrap());
        assert_eq!(bearer(&h).as_deref(), Some("fdt_abc"));
        h.insert(header::AUTHORIZATION, "bearer  fdt_abc ".parse().unwrap());
        assert_eq!(bearer(&h).as_deref(), Some("fdt_abc"));
        for bad in ["Basic abc", "Bearer", "Bearer ", "fdt_abc"] {
            h.insert(header::AUTHORIZATION, bad.parse().unwrap());
            assert_eq!(bearer(&h), None, "{bad}");
        }
    }
}
