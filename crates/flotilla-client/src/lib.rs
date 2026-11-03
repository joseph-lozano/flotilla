//! Flotilla's client: a connection to a server (`flotilla-server`) that speaks the same
//! `ClientMsg` → `ServerMsg` the app's backend does, so the app (or the CLI) can use a server
//! as if the engine's host were in its own process.
//!
//! It runs on its own thread (a small tokio runtime) and keeps an in-memory copy of the views
//! it subscribes to (`flotilla_proto::Replica`), applying the server's deltas and checking
//! their revisions; on any doubt it asks for the view whole. When the connection drops it
//! says so (`Connection::Offline`), reconnects with jittered backoff, and resumes with what it
//! holds: the server sends only what changed meanwhile, then the answers and notes it missed.
//! Once caught up it says `Connection::Online`. What a hello holds, what goes again after a
//! drop and what's skipped as had are [`session::SessionCore`]'s rules, without the socket.
//!
//! Over TCP it sends the device's token (`Authorization: Bearer`) with the upgrade; a server
//! that says the token is no good (401) is final: `Connection::Refused`, and no more tries.
//! `https://` servers (`tailscale serve`) are reached over TLS, checked against the system's
//! trusted certificates. [`pair`] swaps a pairing code for a token.
//!
//! A server in this same process (the app's own, on This Mac) is reached through a [`Pipe`]:
//! the same WebSocket and the same messages, over an in-memory stream instead of a socket.

pub mod session;
pub mod tokens;

use flotilla_proto::history::{FULL_PATH, HISTORY_PATH};
use flotilla_proto::wire::{
    Blob, Descriptor, Down, PAIR_PATH, PROTOCOL_HEADER, PairRequest, Paired, Up, WS_PATH, Welcome,
};
use flotilla_proto::{
    ClientMsg, Connection, Cut, HistoryPage, PROTOCOL, Secret, ServerMsg, ViewKey,
};
use futures_util::{SinkExt, StreamExt};
use session::{Outcome, Phase, SessionCore};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest};

/// Where a server is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `host:port`, plain.
    Tcp(String),
    /// `host:port`, over TLS (an `https://` server, as `tailscale serve` gives).
    Tls(String),
    Unix(PathBuf),
    /// A server in this process.
    Memory(Pipe),
}

/// The way into a server running in this process: each connection is a new in-memory stream,
/// whose other end the server takes as it takes its Unix socket's (this machine's user, no
/// token). None once that server has stopped.
#[derive(Clone)]
pub struct Pipe(Arc<dyn Fn() -> Option<tokio::io::DuplexStream> + Send + Sync>);

impl Pipe {
    pub fn new(open: impl Fn() -> Option<tokio::io::DuplexStream> + Send + Sync + 'static) -> Pipe {
        Pipe(Arc::new(open))
    }

    fn open(&self) -> Option<tokio::io::DuplexStream> {
        (self.0)()
    }
}

impl std::fmt::Debug for Pipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pipe")
    }
}

impl PartialEq for Pipe {
    fn eq(&self, other: &Pipe) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Pipe {}

impl Target {
    /// `unix:/path` or `/path`; else a server's address as `flotilla_proto::pair::server_url`
    /// reads it: `ws://` and `http://` are plain, `wss://` and `https://` TLS, `host:port`
    /// plain and a bare host TLS.
    pub fn parse(s: &str) -> Result<Target, String> {
        let s = s.trim();
        if let Some(path) = s.strip_prefix("unix:") {
            return Ok(Target::Unix(PathBuf::from(path)));
        }
        if s.starts_with('/') || s.starts_with('~') || s.starts_with('.') {
            return Ok(Target::Unix(PathBuf::from(s)));
        }
        let url = flotilla_proto::pair::server_url(s)?;
        Ok(match url.split_once("://") {
            Some(("https", a)) => Target::Tls(with_port(a, 443)),
            Some((_, a)) => Target::Tcp(with_port(a, 80)),
            None => return Err(format!("{s} isn't a server's address")),
        })
    }

    /// Over the network, where a device's token goes; this machine's socket and a pipe need none.
    pub fn tcp(&self) -> bool {
        matches!(self, Target::Tcp(_) | Target::Tls(_))
    }

    /// Where a token goes when there is one: the network, and a pipe (the server's MCP
    /// listener reaches its own server through one, as the device it serves). This machine's
    /// socket takes none.
    fn takes_token(&self) -> bool {
        !matches!(self, Target::Unix(_))
    }

    /// The host, for TLS's server name.
    fn host(&self) -> &str {
        match self {
            Target::Tcp(a) | Target::Tls(a) => a
                .rsplit_once(':')
                .map_or(a.as_str(), |(h, _)| h)
                .trim_start_matches('[')
                .trim_end_matches(']'),
            Target::Unix(_) | Target::Memory(_) => "localhost",
        }
    }

    fn ws_url(&self) -> String {
        match self {
            Target::Tcp(a) => format!("ws://{a}{WS_PATH}"),
            Target::Tls(a) => format!("wss://{}{WS_PATH}", a.strip_suffix(":443").unwrap_or(a)),
            Target::Unix(_) | Target::Memory(_) => format!("ws://localhost{WS_PATH}"),
        }
    }

    /// The Host header for plain HTTP.
    fn authority(&self) -> &str {
        match self {
            Target::Tcp(a) => a,
            Target::Tls(a) => a.strip_suffix(":443").unwrap_or(a),
            Target::Unix(_) | Target::Memory(_) => "localhost",
        }
    }
}

/// `host` → `host:port`, unless it says one.
fn with_port(authority: &str, port: u16) -> String {
    let has = authority.rsplit_once(':').is_some_and(|(h, p)| {
        p.bytes().all(|b| b.is_ascii_digit())
            && !p.is_empty()
            && (!h.contains(':') || h.ends_with(']'))
    });
    if has {
        authority.to_string()
    } else {
        format!("{authority}:{port}")
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Tcp(a) => write!(f, "ws://{a}"),
            Target::Tls(a) => write!(f, "wss://{}", a.strip_suffix(":443").unwrap_or(a)),
            Target::Unix(p) => write!(f, "unix:{}", p.display()),
            Target::Memory(_) => f.write_str("this app's server"),
        }
    }
}

/// Who's connecting, for the hello, and the device's token (sent over TCP and through a pipe
/// when there is one, never over the Unix socket, which needs none).
#[derive(Debug, Clone)]
pub struct Info {
    pub app: String,
    pub version: String,
    pub token: Option<Secret>,
    /// Ask the server to hold this connection to less than its device may do (`flotilla mcp`
    /// asks for an agent's scope, on this machine's socket too).
    pub scope: Option<flotilla_proto::Scope>,
}

/// How often the client pings, and how long it waits to hear anything before it reconnects.
const PING_EVERY: Duration = Duration::from_secs(20);
const SILENT_FOR: Duration = Duration::from_secs(45);
const WELCOME_WITHIN: Duration = Duration::from_secs(10);
/// A view asked for whole more than this often in this long ends the connection.
const RESYNC_MAX: u32 = 5;
const RESYNC_WINDOW: Duration = Duration::from_secs(60);
/// Reconnecting: the first wait, and the longest.
const BACKOFF_FIRST: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

enum Cmd {
    Msg(ClientMsg),
    /// Not connected: try again now, not after the wait.
    Retry,
    Stop,
}

/// What a client has been through, for tests and diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Connections that got as far as the server's welcome.
    pub connects: u64,
    /// Of those, the ones whose session the server still had.
    pub resumed: u64,
    pub snaps: u64,
    pub deltas: u64,
    /// Views asked for whole again, after a delta that didn't fit.
    pub resyncs: u64,
}

/// A connection to a server, kept up until dropped.
pub struct Client {
    tx: mpsc::UnboundedSender<Cmd>,
    stats: std::sync::Arc<std::sync::Mutex<Stats>>,
    welcome: Arc<std::sync::Mutex<Option<Welcome>>>,
}

impl Client {
    /// Connect to `target` and keep connected. What the server sends goes to `deliver` (on
    /// the client's own thread), which returns false once no one listens.
    pub fn connect(
        target: Target,
        info: Info,
        deliver: impl Fn(ServerMsg) -> bool + Send + 'static,
    ) -> Client {
        let (tx, rx) = mpsc::unbounded_channel();
        let stats = std::sync::Arc::<std::sync::Mutex<Stats>>::default();
        let welcome = Arc::<std::sync::Mutex<Option<Welcome>>>::default();
        let (counts, welcomed) = (stats.clone(), welcome.clone());
        let _ = std::thread::Builder::new()
            .name("flotilla-client".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                rt.block_on(
                    Session::new(target, info, Box::new(deliver), counts, welcomed).run(rx),
                );
            });
        Client { tx, stats, welcome }
    }

    pub fn stats(&self) -> Stats {
        *self.stats.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The server's latest welcome: which server it is, and whose databases it took in. Set
    /// before the connection says it's online.
    pub fn welcome(&self) -> Option<Welcome> {
        self.welcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn send(&self, msg: ClientMsg) {
        let _ = self.tx.send(Cmd::Msg(msg));
    }

    /// While it can't connect, try again at once (and from the shortest wait after that).
    /// Connected, nothing happens.
    pub fn retry(&self) {
        let _ = self.tx.send(Cmd::Retry);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

type Deliver = Box<dyn Fn(ServerMsg) -> bool + Send>;

/// The connection: it keeps one up, feeds the session (`SessionCore`, which has the rules)
/// what comes and goes, and writes what it says to.
struct Session {
    target: Target,
    info: Info,
    deliver: Deliver,
    core: SessionCore,
    stats: std::sync::Arc<std::sync::Mutex<Stats>>,
    welcome: Arc<std::sync::Mutex<Option<Welcome>>>,
}

/// Why a connection ended.
enum End {
    Stop,
    Lost(String),
}

impl Session {
    fn new(
        target: Target,
        info: Info,
        deliver: Deliver,
        stats: std::sync::Arc<std::sync::Mutex<Stats>>,
        welcome: Arc<std::sync::Mutex<Option<Welcome>>>,
    ) -> Session {
        Session {
            target,
            info,
            deliver,
            core: SessionCore::new(uuid::Uuid::new_v4().to_string()),
            stats,
            welcome,
        }
    }

    fn count(&self, f: impl FnOnce(&mut Stats)) {
        f(&mut self.stats.lock().unwrap_or_else(|e| e.into_inner()));
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        let mut attempt: u32 = 0;
        loop {
            let ended = match connect(&self.target, self.info.token.as_ref()).await {
                Ok(ws) => {
                    let mut unsent = vec![];
                    let (end, synced) = self.serve(ws, &mut rx, &mut unsent).await;
                    self.core.lost(unsent);
                    if synced {
                        attempt = 0;
                    }
                    end
                }
                Err(Failed::Lost(e)) => End::Lost(e),
                // Its token is no good: no try will work until it's paired again.
                Err(Failed::Unpaired(reason)) => {
                    if let Outcome::Refuse(_, Some(event)) = self.core.refuse(reason) {
                        (self.deliver)(event);
                    }
                    return;
                }
            };
            match ended {
                End::Stop => return,
                End::Lost(e) => {
                    if self.core.refused().is_none()
                        && !(self.deliver)(ServerMsg::Connection(Connection::Offline { error: e }))
                    {
                        return;
                    }
                }
            }
            // Wait before the next try, taking what the app sends meanwhile.
            let wait = backoff(attempt);
            attempt = attempt.saturating_add(1);
            let until = tokio::time::Instant::now() + wait;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => break,
                    cmd = rx.recv() => match cmd {
                        Some(Cmd::Msg(m)) => {
                            self.core.app(m);
                        }
                        Some(Cmd::Retry) => {
                            attempt = 0;
                            break;
                        }
                        Some(Cmd::Stop) | None => return,
                    },
                }
            }
        }
    }

    /// One connection, until it ends; and whether it got caught up. What was queued and not
    /// written when it ended goes in `unsent`, in order.
    async fn serve<S>(
        &mut self,
        ws: tokio_tungstenite::WebSocketStream<S>,
        rx: &mut mpsc::UnboundedReceiver<Cmd>,
        unsent: &mut Vec<Up>,
    ) -> (End, bool)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let (mut sink, mut stream) = ws.split();
        let mut hello = self
            .core
            .hello(&self.info.app, &self.info.version, std::env::consts::OS);
        if let Up::Hello(h) = &mut hello {
            h.scope = self.info.scope;
        }
        if let Err(e) = send(&mut sink, &hello).await {
            return (End::Lost(e), false);
        }
        let mut synced = false;
        let mut heard = Instant::now();
        let welcome_due = tokio::time::Instant::now() + WELCOME_WITHIN;
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        // When each view was last asked for again, and how often lately; and the views said
        // once not to read.
        let mut asked_again: HashMap<ViewKey, (Instant, u32)> = HashMap::new();
        let mut unreadable: HashSet<ViewKey> = HashSet::new();
        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    let msg = match cmd {
                        Some(Cmd::Retry) => continue,
                        Some(Cmd::Msg(m)) => m,
                        Some(Cmd::Stop) | None => {
                            let _ = sink.send(Message::Close(None)).await;
                            return (End::Stop, synced);
                        }
                    };
                    // Before the welcome the session keeps it; then it goes straight out.
                    for up in self.core.app(msg) {
                        if let Err(e) = send(&mut sink, &up).await {
                            return (End::Lost(e), synced);
                        }
                    }
                }
                frame = stream.next() => {
                    heard = Instant::now();
                    let text = match frame {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Close(_))) | None => return (End::Lost("The server closed the connection".into()), synced),
                        Some(Err(e)) => return (End::Lost(e.to_string()), synced),
                        Some(Ok(_)) => continue,
                    };
                    let Ok(down) = serde_json::from_str::<Down>(&text) else { continue };
                    if self.core.phase() == Phase::Live {
                        match &down {
                            Down::Snap { .. } => self.count(|s| s.snaps += 1),
                            Down::Delta { .. } => self.count(|s| s.deltas += 1),
                            _ => {}
                        }
                    }
                    match self.core.received(down) {
                        Outcome::Welcomed { welcome, send: ups, lost } => {
                            *self.welcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(welcome.clone());
                            self.count(|s| {
                                s.connects += 1;
                                s.resumed += u64::from(welcome.resumed);
                            });
                            for id in lost {
                                if !(self.deliver)(ServerMsg::Res { id, result: Err("Lost the connection to the server before it answered".into()) }) {
                                    return (End::Stop, synced);
                                }
                            }
                            let mut ups = ups.into_iter();
                            while let Some(up) = ups.next() {
                                if let Err(e) = send(&mut sink, &up).await {
                                    // The one being written may have gone; what's after it
                                    // waits for the next connection, in order.
                                    *unsent = ups.collect();
                                    return (End::Lost(e), false);
                                }
                            }
                        }
                        Outcome::Deliver(msgs) => {
                            for msg in msgs {
                                if matches!(msg, ServerMsg::Connection(Connection::Online)) {
                                    synced = true;
                                }
                                if !(self.deliver)(msg) {
                                    return (End::Stop, synced);
                                }
                            }
                        }
                        Outcome::Resync(view, why) => {
                            eprintln!("[client] asking for {} again: {why}", view.name());
                            self.count(|s| s.resyncs += 1);
                            // One that keeps not fitting isn't asked for over and over: the
                            // connection is let go, and tried again after a wait.
                            let (since, n) = asked_again.entry(view.clone()).or_insert((Instant::now(), 0));
                            if since.elapsed() > RESYNC_WINDOW {
                                (*since, *n) = (Instant::now(), 0);
                            }
                            *n += 1;
                            if *n > RESYNC_MAX {
                                // Not counted as synced, so the waits before trying again grow.
                                return (End::Lost(format!("The server's {} view kept not fitting", view.name())), false);
                            }
                            if let Err(e) = send(&mut sink, &Up::Resync { view }).await {
                                return (End::Lost(e), synced);
                            }
                        }
                        Outcome::Unreadable(view, why) => {
                            if unreadable.insert(view.clone()) {
                                eprintln!("[client] can't read the {} view this server sends (update Flotilla): {why}", view.name());
                            }
                        }
                        // The server won't take this client: say why once, and keep trying
                        // slowly.
                        Outcome::Refuse(reason, event) => {
                            if let Some(event) = event {
                                (self.deliver)(event);
                            }
                            return (End::Lost(reason), synced);
                        }
                        Outcome::Fail(why) => return (End::Lost(why), synced),
                        Outcome::Skipped | Outcome::Nothing => {}
                    }
                }
                _ = tokio::time::sleep_until(welcome_due), if self.core.phase() == Phase::Hello => {
                    return (End::Lost("The server didn't answer".into()), false);
                }
                _ = ping.tick() => {
                    if heard.elapsed() > SILENT_FOR {
                        return (End::Lost("The server stopped answering".into()), synced);
                    }
                    if let Err(e) = sink.send(Message::Ping(Vec::new().into())).await {
                        return (End::Lost(e.to_string()), synced);
                    }
                }
            }
        }
    }
}

/// How long to wait before try `attempt` (from 0): doubling up to the most, give or take a
/// third, so clients dropped together don't all come back at once.
fn backoff(attempt: u32) -> Duration {
    let base = BACKOFF_FIRST
        .saturating_mul(1u32 << attempt.min(8))
        .min(BACKOFF_MAX);
    let jitter = (uuid::Uuid::new_v4().as_u128() % 1000) as f64 / 1000.0;
    base.mul_f64(0.67 + 0.66 * jitter)
}

async fn send<S>(sink: &mut S, up: &Up) -> Result<(), String>
where
    S: futures_util::Sink<Message, Error = tungstenite::Error> + Unpin,
{
    let text = serde_json::to_string(up).map_err(|e| e.to_string())?;
    sink.send(Message::Text(text.into()))
        .await
        .map_err(|e| e.to_string())
}

/// Any kind of socket, so one `serve` handles them all.
enum Stream {
    Tcp(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>),
    Unix(tokio::net::UnixStream),
    Memory(tokio::io::DuplexStream),
}

impl tokio::io::AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
            Stream::Unix(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            Stream::Memory(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for Stream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Stream::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
            Stream::Unix(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            Stream::Memory(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
            Stream::Unix(s) => std::pin::Pin::new(s).poll_flush(cx),
            Stream::Memory(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
            Stream::Unix(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            Stream::Memory(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// TLS as browsers do it: the system's trusted certificates.
fn tls() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            use rustls_platform_verifier::BuilderVerifierExt;
            let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .with_platform_verifier()
                .map_err(|e| e.to_string())?
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
}

/// A socket to `target`, over TLS if it's an `https://` server.
async fn open(target: &Target) -> Result<Stream, String> {
    let unreachable = |e: std::io::Error| format!("Can't reach {target}: {e}");
    Ok(match target {
        Target::Tcp(a) => Stream::Tcp(
            tokio::net::TcpStream::connect(a)
                .await
                .map_err(unreachable)?,
        ),
        Target::Unix(p) => Stream::Unix(
            tokio::net::UnixStream::connect(p)
                .await
                .map_err(unreachable)?,
        ),
        Target::Memory(pipe) => Stream::Memory(pipe.open().ok_or("This app's server has stopped")?),
        Target::Tls(a) => {
            let tcp = tokio::net::TcpStream::connect(a)
                .await
                .map_err(unreachable)?;
            let name = rustls::pki_types::ServerName::try_from(target.host().to_string())
                .map_err(|e| format!("{target}: {e}"))?;
            let tls = tokio_rustls::TlsConnector::from(tls()?)
                .connect(name, tcp)
                .await
                .map_err(|e| format!("Can't reach {target} securely: {e}"))?;
            Stream::Tls(Box::new(tls))
        }
    })
}

/// Why a connection couldn't be made.
enum Failed {
    /// Try again later.
    Lost(String),
    /// The server doesn't know this device (401): pair it again.
    Unpaired(String),
}

async fn connect(
    target: &Target,
    token: Option<&Secret>,
) -> Result<tokio_tungstenite::WebSocketStream<Stream>, Failed> {
    let mut request = target
        .ws_url()
        .into_client_request()
        .map_err(|e| Failed::Lost(e.to_string()))?;
    request
        .headers_mut()
        .insert(PROTOCOL_HEADER, PROTOCOL.into());
    if let Some(token) = token.filter(|_| target.takes_token()) {
        let value = format!("Bearer {}", token.0).parse().map_err(|_| {
            Failed::Unpaired("This device's token isn't readable: pair it again".into())
        })?;
        request.headers_mut().insert("authorization", value);
    }
    let stream = open(target).await.map_err(Failed::Lost)?;
    match tokio_tungstenite::client_async(request, stream).await {
        Ok((ws, _)) => Ok(ws),
        Err(tungstenite::Error::Http(resp)) => {
            let body = resp
                .body()
                .as_deref()
                .map(String::from_utf8_lossy)
                .unwrap_or_default()
                .trim()
                .to_string();
            let said = if body.is_empty() {
                format!("The server said {}", resp.status())
            } else {
                body
            };
            Err(
                if resp.status() == tungstenite::http::StatusCode::UNAUTHORIZED {
                    Failed::Unpaired(said)
                } else {
                    Failed::Lost(said)
                },
            )
        }
        Err(e) => Err(Failed::Lost(e.to_string())),
    }
}

/// One HTTP/1.1 request, its status and body. Blocking: it runs on its own small runtime, so
/// it mustn't be called from inside one.
fn http(
    target: &Target,
    method: &str,
    path: &str,
    token: Option<&Secret>,
    kind: &str,
    body: &[u8],
) -> Result<(u16, String), String> {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n",
        target.authority(),
        body.len()
    );
    if let Some(t) = token.filter(|_| target.takes_token()) {
        head.push_str(&format!("Authorization: Bearer {}\r\n", t.0));
    }
    head.push_str("\r\n");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let response = rt.block_on(async {
        let mut s = open(target).await?;
        let io = |e: std::io::Error| e.to_string();
        s.write_all(head.as_bytes()).await.map_err(io)?;
        s.write_all(body).await.map_err(io)?;
        s.flush().await.map_err(io)?;
        let mut response = Vec::new();
        match tokio::time::timeout(Duration::from_secs(30), s.read_to_end(&mut response)).await {
            Ok(Ok(_)) => {}
            // A TLS server that closes without saying so, after the whole answer.
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof && !response.is_empty() => {
            }
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => return Err(format!("{target} didn't answer")),
        }
        Ok::<_, String>(response)
    })?;
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("{target} didn't answer as a Flotilla server"))?;
    Ok((status, body.to_string()))
}

/// Store a file on the server (`PUT /v1/blobs`): its id, to send with a request.
pub fn upload(target: &Target, token: Option<&Secret>, bytes: &[u8]) -> Result<Blob, String> {
    let (status, body) = http(
        target,
        "PUT",
        "/v1/blobs",
        token,
        "application/octet-stream",
        bytes,
    )?;
    if status != 200 {
        return Err(if body.trim().is_empty() {
            format!("The server said {status}")
        } else {
            body.trim().to_string()
        });
    }
    serde_json::from_str(&body).map_err(|e| format!("The server's answer didn't read: {e}"))
}

/// `s` as it may go in a URL's path or query.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn refusal(status: u16, body: &str) -> String {
    if body.trim().is_empty() {
        format!("The server said {status}")
    } else {
        body.trim().to_string()
    }
}

/// A page of a thread's older messages: the one that ends at seq `before`
/// (`GET /v1/threads/:id/history`, see `flotilla_proto::history`). Blocking.
pub fn history(
    target: &Target,
    token: Option<&Secret>,
    thread: &str,
    before: i64,
) -> Result<HistoryPage, String> {
    let path = format!(
        "{}?before={before}",
        HISTORY_PATH.replace("{id}", &encode(thread))
    );
    let (status, body) = http(target, "GET", &path, token, "application/json", &[])?;
    if status != 200 {
        return Err(refusal(status, &body));
    }
    serde_json::from_str(&body).map_err(|e| format!("The server's answer didn't read: {e}"))
}

/// A string a message carries cut short, whole (`GET /v1/threads/:id/full`). Blocking.
pub fn full(
    target: &Target,
    token: Option<&Secret>,
    thread: &str,
    seq: i64,
    cut: &Cut,
) -> Result<String, String> {
    let path = format!(
        "{}?seq={seq}&field={}&mark={}",
        FULL_PATH.replace("{id}", &encode(thread)),
        encode(&cut.field),
        encode(&cut.mark)
    );
    let (status, body) = http(target, "GET", &path, token, "text/plain", &[])?;
    if status != 200 {
        return Err(refusal(status, &body));
    }
    Ok(body)
}

/// Which server answers at `target` (`GET /.well-known/flotilla`).
pub fn describe(target: &Target) -> Result<Descriptor, String> {
    let (status, body) = http(
        target,
        "GET",
        "/.well-known/flotilla",
        None,
        "application/json",
        &[],
    )?;
    if status != 200 {
        return Err(if body.trim().is_empty() {
            format!("The server said {status}")
        } else {
            body.trim().to_string()
        });
    }
    serde_json::from_str(&body).map_err(|_| format!("{target} didn't answer as a Flotilla server"))
}

/// Swap a pairing code for this device's token at `server` (`https://…` or `http://…`): the
/// device, its token and which server it is. The error says why not, for the user.
pub fn pair(server: &str, code: &str, device_name: &str, platform: &str) -> Result<Paired, String> {
    pair_as(server, code, device_name, platform, None)
}

/// [`pair`], asking for no more than `scope` (the server gives the narrower of it and what the
/// code allows).
pub fn pair_as(
    server: &str,
    code: &str,
    device_name: &str,
    platform: &str,
    scope: Option<flotilla_proto::Scope>,
) -> Result<Paired, String> {
    let target = Target::parse(server)?;
    let code = flotilla_proto::pair::normalize_code(code)
        .ok_or("That code isn't right: it's 12 letters and digits, like ABCD-EFGH-JKMN")?;
    let req = serde_json::to_vec(&PairRequest {
        code: Secret(code),
        device_name: device_name.into(),
        platform: platform.into(),
        scope,
    })
    .map_err(|e| e.to_string())?;
    let (status, body) = http(&target, "POST", PAIR_PATH, None, "application/json", &req)?;
    if status != 200 {
        return Err(if body.trim().is_empty() {
            format!("The server said {status}")
        } else {
            body.trim().to_string()
        });
    }
    serde_json::from_str(&body).map_err(|_| format!("{server} didn't answer as a Flotilla server"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_read_as_urls_or_sockets() {
        assert_eq!(
            Target::parse("ws://127.0.0.1:7777").unwrap(),
            Target::Tcp("127.0.0.1:7777".into())
        );
        assert_eq!(
            Target::parse("http://127.0.0.1:7777/").unwrap(),
            Target::Tcp("127.0.0.1:7777".into())
        );
        assert_eq!(
            Target::parse("localhost:9").unwrap(),
            Target::Tcp("localhost:9".into())
        );
        assert_eq!(
            Target::parse("http://box").unwrap(),
            Target::Tcp("box:80".into())
        );
        assert_eq!(
            Target::parse("unix:/tmp/f.sock").unwrap(),
            Target::Unix("/tmp/f.sock".into())
        );
        assert_eq!(
            Target::parse("/tmp/f.sock").unwrap(),
            Target::Unix("/tmp/f.sock".into())
        );
        let tls = Target::parse("https://box.tail1.ts.net").unwrap();
        assert_eq!(tls, Target::Tls("box.tail1.ts.net:443".into()));
        assert_eq!(
            (tls.ws_url().as_str(), tls.host(), tls.to_string().as_str()),
            (
                "wss://box.tail1.ts.net/v1/ws",
                "box.tail1.ts.net",
                "wss://box.tail1.ts.net"
            )
        );
        assert_eq!(Target::parse("box.tail1.ts.net").unwrap(), tls);
        assert_eq!(
            Target::parse("wss://box:8443").unwrap(),
            Target::Tls("box:8443".into())
        );
        assert!(Target::parse("ftp://box").is_err());
        assert!(Target::parse("https://box/path").is_err());
        assert_eq!(
            Target::Tcp("127.0.0.1:7777".into()).ws_url(),
            "ws://127.0.0.1:7777/v1/ws"
        );
    }

    #[test]
    fn backoff_grows_to_a_ceiling_with_jitter() {
        assert!(backoff(0) < Duration::from_millis(400));
        assert!(backoff(30) <= BACKOFF_MAX.mul_f64(1.34));
        assert!(backoff(30) >= BACKOFF_MAX.mul_f64(0.66));
    }
}
