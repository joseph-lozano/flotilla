//! Remote MCP: Flotilla's MCP tools (`flotilla_mcp`) over streamable HTTP, for assistants that
//! run in someone else's cloud (Grok Bot, grok.com, Muse) and so can't run `flotilla mcp` or
//! reach a tailnet. It's a listener of its own (`flotilla serve --mcp-port`, off unless turned
//! on) that serves MCP and the OAuth that lets an assistant in, and nothing of the main API (no
//! WebSocket, pairing, uploads or history), so it alone can be published to the internet
//! (Tailscale Funnel, exe.dev, a Cloudflare Tunnel) while the main API stays on the tailnet.
//!
//! **Who gets in.** An agent device's token (`Authorization: Bearer fdt_…`), checked on every
//! request: one made for it (`flotilla pair --agent --token`), or one OAuth gives a client once
//! you type a pairing code on the page it opens. A full device's token is refused here. Each
//! device's sessions reach the server as that device through a pipe inside this process, with
//! its token, so the server holds them to an agent's scope as it holds `flotilla mcp`, and
//! revoking the device closes them.
//!
//! **MCP over HTTP** (the 2025-06-18 transport, and 2025-03-26's): `POST /mcp` takes one
//! JSON-RPC message; `initialize` starts a session (`Mcp-Session-Id`) that the rest carry. A
//! tool call answers as an event stream when the client takes one, with progress and a
//! keep-alive while `wait_for_turn` waits, else as JSON. `wait_for_turn` waits at most
//! [`QUIET_WAIT`] seconds for a call that takes no progress. `DELETE /mcp` ends a session;
//! `GET /mcp` is 405 (nothing is sent unasked).
//!
//! **OAuth 2.1**, for clients that sign in rather than send a header: protected resource and
//! authorization server metadata (RFC 9728, RFC 8414), dynamic client registration (RFC 7591;
//! stateless, the client id carries the client's name and redirect URIs), the authorization
//! code grant with PKCE (S256 only), and a refresh that gives the same token back. Approval is a
//! person typing a one-time pairing code (Settings → Devices → Add device, or `flotilla pair
//! --agent`) on `/authorize`; the token is a new agent device's, named for the client, listed in
//! Settings → Devices and revocable there. A code not swapped for its token in time revokes
//! that device, looked for every [`GRANT_SWEEP`] and as the server stops; one a crash left is
//! revoked once the next server starts (it's noted in the database until it's swapped).
//!
//! **For the internet.** Rate limits on wrong tokens, codes, registrations, token requests and
//! tool calls (each address, and all of them); requests in flight, sessions and connections to
//! the server bounded; a body read only where one's taken, once it's known who sent it (MCP's
//! from an agent device), within its limit and its address's share of the room for bodies on
//! their way; tokens and codes compared by their hashes (the store's
//! check is constant time), never logged and never in a URL (an authorization code is, as OAuth
//! has it, used once within two minutes and bound to its PKCE verifier); no CORS, and requests
//! from web pages refused but for the approval page's own form; nothing cached.

// A refusal is an answer to send (a `Response`), which is large; it's made once per request.
#![allow(clippy::result_large_err)]

use crate::{App, bearer, drain_small, ip_of, pair_source};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{Extensions, HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use base64::Engine;
use flotilla_core::devices::{AuthError, PairError};
use flotilla_mcp::{Door, Link, Mcp};
use flotilla_proto::wire::PairRequest;
use flotilla_proto::{Scope, Secret};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Where MCP is, under the listener's address.
pub const MCP_PATH: &str = "/mcp";
/// The longest `wait_for_turn` for a call that takes no progress: under the two minutes some
/// clients give a tool call that says nothing meanwhile (Grok Bot's connectors).
pub const QUIET_WAIT: u64 = 100;
/// The largest message `POST /mcp` takes (a prompt is text: this is plenty), and an OAuth form.
const BODY_MAX: usize = 1 << 20;
const FORM_MAX: usize = 16 << 10;
/// Requests in flight at once, over every client.
const IN_FLIGHT: usize = 128;
/// How long a request's body may take to arrive: one that dribbles in is cut off, so it can't
/// hold anything meanwhile.
const BODY_WITHIN: Duration = Duration::from_secs(10);
/// The bodies arriving at once, in KiB, by what they say they'll be: past any of these a request
/// is refused at once, so slow bodies can't fill the memory, nor one address keep everyone else
/// out. In all, and one address's MCP messages; OAuth's forms (all that anyone may send without
/// a token) in all, and one address's.
const BODY_BUDGET_KIB: u32 = 32 << 10;
const BODY_EACH_KIB: u32 = 4 << 10;
const FORMS_BUDGET_KIB: u32 = 2 << 10;
const FORMS_EACH_KIB: u32 = 256;
/// Tool calls in flight for one device, and its requests a minute.
const CALLS_PER_DEVICE: usize = 8;
const REQUESTS_PER_DEVICE: usize = 240;
/// Sessions: for one device, in all, and how long one lasts unused. The devices with sessions
/// at once (each holds a connection to the server).
const SESSIONS_PER_DEVICE: usize = 16;
const SESSIONS_MAX: usize = 256;
const SESSION_IDLE: Duration = Duration::from_secs(30 * 60);
const DEVICES_MAX: usize = 32;
/// An event stream's keep-alive, so a proxy in front doesn't close a quiet wait.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// An authorization code works this long, once; at most this many wait at once.
const GRANT_TTL: Duration = Duration::from_secs(120);
const GRANTS_MAX: usize = 64;
/// How often codes not swapped in time are looked for, whatever comes in.
pub(crate) const GRANT_SWEEP: Duration = Duration::from_secs(30);
/// A device OAuth made whose code isn't swapped yet, by its id (`kv`): so one this server
/// forgot (a crash, a restart) is still revoked.
const PENDING: &str = flotilla_core::devices::SIGN_IN_PENDING;
/// What a client may register: redirect URIs, their length, and its name.
const REDIRECTS_MAX: usize = 8;
const REDIRECT_LEN: usize = 512;
const NAME_LEN: usize = 40;
const WINDOW: Duration = Duration::from_secs(60);
/// The scope there is: an agent's.
const SCOPE: &str = "flotilla";
/// The device platform remote MCP clients are paired as.
pub const PLATFORM: &str = "mcp";

/// What happened in the last minute, by who did it (an address or a device) and in all: at
/// most `each` for one, `all` in all.
struct Window {
    each: usize,
    all: usize,
    at: VecDeque<(Instant, String)>,
    /// When a refusal for each was last logged: once a minute, not once a try.
    logged: HashMap<String, Instant>,
}

impl Window {
    fn new(each: usize, all: usize) -> Window {
        Window {
            each,
            all,
            at: VecDeque::new(),
            logged: HashMap::new(),
        }
    }

    fn prune(&mut self, now: Instant) {
        while self
            .at
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > WINDOW)
        {
            self.at.pop_front();
        }
        self.logged
            .retain(|_, at| now.duration_since(*at) <= WINDOW);
    }

    /// Whether `who` (or everyone) has had its fill.
    fn full(&mut self, who: &str, now: Instant) -> bool {
        self.prune(now);
        self.at.len() >= self.all || self.at.iter().filter(|(_, w)| w == who).count() >= self.each
    }

    fn add(&mut self, who: &str, now: Instant) {
        self.at.push_back((now, who.to_string()));
    }

    /// Room for one more from `who`: taken if there is.
    fn take(&mut self, who: &str, now: Instant) -> bool {
        if self.full(who, now) {
            return false;
        }
        self.add(who, now);
        true
    }

    /// Whether to log that `who` was refused: the first time in a minute.
    fn log(&mut self, who: &str, now: Instant) -> bool {
        self.logged.insert(who.to_string(), now).is_none()
    }
}

/// The limits, each over the last minute.
struct Limits {
    /// Requests whose token was missing or wrong, by address.
    wrong: Window,
    /// Pages and metadata anyone may read, by address.
    open: Window,
    registrations: Window,
    /// Token requests, by address.
    tokens: Window,
    /// Requests from each device.
    requests: Window,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            wrong: Window::new(30, 300),
            open: Window::new(120, 1200),
            registrations: Window::new(10, 60),
            tokens: Window::new(30, 300),
            requests: Window::new(REQUESTS_PER_DEVICE, REQUESTS_PER_DEVICE * DEVICES_MAX),
        }
    }
}

/// One MCP session: a client of one device.
struct Session {
    device: String,
    mcp: Arc<Mcp>,
    used: Instant,
}

/// What `/authorize` gave out and `/token` takes: an agent device made for a client, its token
/// held until the client swaps its code for it.
struct Grant {
    client: String,
    redirect: String,
    challenge: String,
    device: String,
    token: Secret,
    at: Instant,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    /// Each device's connection to the server, shared by its sessions.
    links: HashMap<String, Arc<Link>>,
    /// Each device's tool calls in flight.
    calls: HashMap<String, usize>,
    /// Authorization codes not swapped yet, by their hash.
    grants: HashMap<String, Grant>,
    limits: Limits,
    /// The server is stopping: nothing new.
    closed: bool,
}

/// The listener's state, shared by its requests.
#[derive(Clone)]
pub(crate) struct Listener {
    app: App,
    /// The way into this server as a device (with its token), from this process.
    pipe: flotilla_client::Pipe,
    /// The address clients use, when it's set (`--mcp-url`, or Funnel's once it publishes);
    /// else each request's own.
    url: Arc<Mutex<Option<String>>>,
    db: PathBuf,
    inner: Arc<Mutex<Inner>>,
    in_flight: Arc<tokio::sync::Semaphore>,
    arriving: Arc<Mutex<Arriving>>,
}

/// What bodies arriving now hold, in KiB by what they say they'll be ([`BODY_BUDGET_KIB`]).
#[derive(Default)]
struct Arriving {
    all: u32,
    forms: u32,
    /// By address, and whether they're forms.
    by: HashMap<(String, bool), u32>,
}

/// Room for a body on its way, given back when it's dropped.
struct Room {
    arriving: Arc<Mutex<Arriving>>,
    key: (String, bool),
    kib: u32,
}

impl Drop for Room {
    fn drop(&mut self) {
        let mut a = self.arriving.lock().unwrap_or_else(|e| e.into_inner());
        a.all = a.all.saturating_sub(self.kib);
        if self.key.1 {
            a.forms = a.forms.saturating_sub(self.kib);
        }
        if let Some(n) = a.by.get_mut(&self.key) {
            *n = n.saturating_sub(self.kib);
            if *n == 0 {
                a.by.remove(&self.key);
            }
        }
    }
}

/// Who a `POST /mcp` is from, found before its body was read: an agent device's id and token.
#[derive(Clone)]
struct Agent {
    device: String,
    token: Secret,
}

impl Listener {
    pub(crate) fn new(
        app: App,
        pipe: flotilla_client::Pipe,
        url: Arc<Mutex<Option<String>>>,
        db: PathBuf,
    ) -> Listener {
        Listener {
            app,
            pipe,
            url,
            db,
            inner: Arc::default(),
            in_flight: Arc::new(tokio::sync::Semaphore::new(IN_FLIGHT)),
            arriving: Arc::default(),
        }
    }

    /// Room for `kib` more on its way from `from` (a form, or an MCP message), taken if there is:
    /// in all, in its address's share, and for a form in all forms'.
    fn room(&self, from: String, kib: u32, form: bool) -> Option<Room> {
        let mut a = self.arriving.lock().unwrap_or_else(|e| e.into_inner());
        let key = (from, form);
        let mine = a.by.get(&key).copied().unwrap_or(0);
        let each = if form { FORMS_EACH_KIB } else { BODY_EACH_KIB };
        if a.all + kib > BODY_BUDGET_KIB
            || mine + kib > each
            || (form && a.forms + kib > FORMS_BUDGET_KIB)
        {
            return None;
        }
        a.all += kib;
        if form {
            a.forms += kib;
        }
        *a.by.entry(key.clone()).or_insert(0) += kib;
        Some(Room {
            arriving: self.arriving.clone(),
            key,
            kib,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The server is stopping: every session ends (waits under way end at once), nothing new
    /// starts, and the devices made for codes not swapped yet are revoked (their codes go with
    /// this server).
    pub(crate) fn close(&self) {
        let devices: Vec<String> = {
            let mut inner = self.lock();
            inner.closed = true;
            for s in inner.sessions.values() {
                s.mcp.close();
            }
            inner.sessions.clear();
            inner.links.clear();
            inner.grants.drain().map(|(_, g)| g.device).collect()
        };
        // Now, as the server stops (one missed is the next start's to revoke).
        for device in devices {
            revoke_now(&self.db, &device, None);
        }
    }

    /// Codes not swapped in time, kept here or (from before this server started) only in the
    /// database: their devices are revoked. Run as the listener starts and every
    /// [`GRANT_SWEEP`].
    pub(crate) fn tidy(&self) {
        self.expire_grants();
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(store) = flotilla_core::store::Store::open(&db) else {
                return;
            };
            let before = flotilla_core::util::now_ms() - GRANT_TTL.as_millis() as i64;
            for (key, at) in store.kv_with_prefix(PENDING).unwrap_or_default() {
                if at.parse::<i64>().is_ok_and(|at| at < before) {
                    revoke_now(&db, &key[PENDING.len()..], Some(before));
                }
            }
        });
    }

    fn base(&self, headers: &HeaderMap) -> String {
        base_of(
            self.url
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_deref(),
            headers,
        )
    }

    /// Who's asking, by its token: an agent device's id and token. Else the answer to give.
    async fn agent(
        &self,
        headers: &HeaderMap,
        ip: Option<String>,
    ) -> Result<(String, Secret), Response> {
        let from = pair_source(ip.as_deref());
        let now = Instant::now();
        {
            let mut inner = self.lock();
            if inner.limits.wrong.full(&from, now) {
                if inner.limits.wrong.log(&from, now) {
                    eprintln!("[mcp] too many wrong tokens from {from}: refusing it for a minute");
                }
                return Err(too_many());
            }
        }
        let wrong = |given: bool| {
            self.lock().limits.wrong.add(&from, Instant::now());
            unauthorized(&self.base(headers), given)
        };
        let Some(token) = bearer(headers) else {
            return Err(wrong(false));
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.app.host.authenticate(token.clone(), ip, move |r| {
            let _ = tx.send(r);
        });
        match rx.await {
            Ok(Ok(device)) if device.scope == Scope::Agent => Ok((device.id, Secret(token))),
            Ok(Ok(_)) => Err(plain(
                StatusCode::FORBIDDEN,
                "Remote MCP takes an agent device's token, not a full device's: make one with `flotilla pair --agent --token`",
            )),
            Ok(Err(AuthError::Unknown | AuthError::Lapsed)) => Err(wrong(true)),
            Ok(Err(e)) => Err(plain(StatusCode::SERVICE_UNAVAILABLE, &e.to_string())),
            Err(_) => Err(plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "The server is stopping",
            )),
        }
    }

    /// Sessions unused too long go; a device with none left lets go of its connection.
    fn sweep(inner: &mut Inner, now: Instant) {
        inner.sessions.retain(|_, s| {
            let keep = now.duration_since(s.used) < SESSION_IDLE;
            if !keep {
                s.mcp.close();
            }
            keep
        });
        let devices: std::collections::HashSet<&String> =
            inner.sessions.values().map(|s| &s.device).collect();
        let gone: Vec<String> = inner
            .links
            .keys()
            .filter(|d| !devices.contains(d))
            .cloned()
            .collect();
        for d in gone {
            inner.links.remove(&d);
        }
    }

    /// End `device`'s sessions (and its connection).
    fn forget(inner: &mut Inner, device: &str) {
        inner.sessions.retain(|_, s| {
            let keep = s.device != device;
            if !keep {
                s.mcp.close();
            }
            keep
        });
        inner.links.remove(device);
    }

    /// A new session for `device`: its id. The device's oldest goes when it has too many, the
    /// oldest of all when there are, and the least used device's when too many have some.
    fn open(&self, device: &str, token: Secret) -> Result<(String, Arc<Mcp>), Response> {
        let mut inner = self.lock();
        if inner.closed {
            return Err(plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "The server is stopping",
            ));
        }
        let now = Instant::now();
        Listener::sweep(&mut inner, now);
        let oldest = |inner: &Inner, of: Option<&str>| {
            inner
                .sessions
                .iter()
                .filter(|(_, s)| of.is_none_or(|d| s.device == d))
                .min_by_key(|(_, s)| s.used)
                .map(|(id, _)| id.clone())
        };
        while inner
            .sessions
            .values()
            .filter(|s| s.device == device)
            .count()
            >= SESSIONS_PER_DEVICE
            || inner.sessions.len() >= SESSIONS_MAX
        {
            let mine = inner
                .sessions
                .values()
                .filter(|s| s.device == device)
                .count()
                >= SESSIONS_PER_DEVICE;
            let Some(id) = oldest(&inner, mine.then_some(device)) else {
                break;
            };
            if let Some(s) = inner.sessions.remove(&id) {
                s.mcp.close();
            }
        }
        if !inner.links.contains_key(device) && inner.links.len() >= DEVICES_MAX {
            // The device whose latest use is oldest.
            let mut last: HashMap<&String, Instant> = HashMap::new();
            for s in inner.sessions.values() {
                let e = last.entry(&s.device).or_insert(s.used);
                *e = (*e).max(s.used);
            }
            if let Some(d) = inner
                .links
                .keys()
                .min_by_key(|d| last.get(d).copied().unwrap_or(now))
                .cloned()
            {
                Listener::forget(&mut inner, &d);
            }
        }
        let link = inner
            .links
            .entry(device.to_string())
            .or_insert_with(|| {
                Arc::new(Link::connect(
                    Door {
                        target: flotilla_client::Target::Memory(self.pipe.clone()),
                        token: Some(token),
                    },
                    "flotilla-mcp-http",
                ))
            })
            .clone();
        let mcp = Arc::new(Mcp::shared(link, Some(QUIET_WAIT)));
        let id = random_id(16);
        inner.sessions.insert(
            id.clone(),
            Session {
                device: device.to_string(),
                mcp: mcp.clone(),
                used: now,
            },
        );
        Ok((id, mcp))
    }

    /// `device`'s session `id`, used now.
    fn session(&self, id: &str, device: &str) -> Option<Arc<Mcp>> {
        let mut inner = self.lock();
        let now = Instant::now();
        Listener::sweep(&mut inner, now);
        let s = inner.sessions.get_mut(id).filter(|s| s.device == device)?;
        s.used = now;
        Some(s.mcp.clone())
    }

    /// Room for a tool call from `device`: held until the [`Calling`] is dropped.
    fn calling(&self, device: &str) -> Option<Calling> {
        let mut inner = self.lock();
        let n = inner.calls.entry(device.to_string()).or_insert(0);
        if *n >= CALLS_PER_DEVICE {
            return None;
        }
        *n += 1;
        Some(Calling {
            inner: self.inner.clone(),
            device: device.to_string(),
        })
    }

    /// Codes given out and never swapped: gone, with their devices (revoked off this thread).
    fn expire_grants(&self) {
        let now = Instant::now();
        let stale: Vec<String> = {
            let mut inner = self.lock();
            let old: Vec<String> = inner
                .grants
                .iter()
                .filter(|(_, g)| now.duration_since(g.at) > GRANT_TTL)
                .map(|(k, _)| k.clone())
                .collect();
            old.iter()
                .filter_map(|k| inner.grants.remove(k))
                .map(|g| g.device)
                .collect()
        };
        for device in stale {
            self.revoke(device);
        }
    }

    /// Revoke a device OAuth made that its client never had (or lost to a wrong exchange).
    fn revoke(&self, device: String) {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || revoke_now(&db, &device, None));
    }
}

/// Revoke `device`, which OAuth made for a code that wasn't swapped (or was, wrongly), if its
/// sign-in is still pending (noted at or before `before_ms`, when given): one whose client has
/// its token by now keeps it.
fn revoke_now(db: &std::path::Path, device: &str, before_ms: Option<i64>) {
    // As `flotilla devices revoke` does: a rebuild can't carry it over meanwhile.
    let _held = flotilla_core::devices::hold(db);
    match flotilla_core::store::Store::open(db)
        .and_then(|s| s.revoke_noted(PENDING, device, before_ms))
    {
        Ok(true) => eprintln!("[mcp] revoked device {device}: its client didn't finish signing in"),
        Ok(false) => {}
        Err(e) => eprintln!("[mcp] couldn't revoke device {device}: {e}"),
    }
}

/// A tool call in flight for a device, counted until dropped.
struct Calling {
    inner: Arc<Mutex<Inner>>,
    device: String,
}

impl Drop for Calling {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = inner.calls.get_mut(&self.device) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                inner.calls.remove(&self.device);
            }
        }
    }
}

/// A request that the client may drop before it's answered (it went away): it's cancelled
/// then, as if the client had said so.
struct Pending {
    mcp: Arc<Mcp>,
    id: Value,
    answered: bool,
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.answered {
            self.mcp.cancel(&self.id);
        }
    }
}

pub(crate) fn router(l: Listener) -> Router {
    Router::new()
        .route(
            MCP_PATH,
            post(mcp_post)
                .get(mcp_get)
                .delete(mcp_delete)
                .layer(DefaultBodyLimit::max(BODY_MAX)),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(server_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server/mcp",
            get(server_metadata),
        )
        .route(
            "/register",
            post(register).layer(DefaultBodyLimit::max(FORM_MAX)),
        )
        .route(
            "/authorize",
            get(authorize_page)
                .post(authorize)
                .layer(DefaultBodyLimit::max(FORM_MAX)),
        )
        .route("/token", post(token).layer(DefaultBodyLimit::max(FORM_MAX)))
        .fallback(|| async { plain(StatusCode::NOT_FOUND, "Not found") })
        .layer(middleware::from_fn_with_state(l.clone(), guard))
        .with_state(l)
}

/// Every request: let in by [`admit`], then room in flight, and nothing kept by a cache or a
/// browser.
async fn guard(State(l): State<Listener>, req: axum::extract::Request, next: Next) -> Response {
    let approval_page = req.uri().path() == "/authorize";
    let (mut r, permit) = match admit(&l, req).await {
        Err(r) => (r, None),
        Ok(req) => match l.in_flight.clone().try_acquire_owned() {
            Ok(permit) => (next.run(req).await, Some(permit)),
            Err(_) => (
                plain(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Too busy: try again in a moment",
                ),
                None,
            ),
        },
    };
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // The approval page's form says where it's posted from (`no-referrer` would make its Origin
    // `null`); nothing leaves for another site.
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static(if approval_page {
            "same-origin"
        } else {
            "no-referrer"
        }),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // A streamed answer holds its room until it ends.
    if permit.is_some()
        && r.headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|t| t.as_bytes().starts_with(b"text/event-stream"))
    {
        let (parts, body) = r.into_parts();
        let body = Body::from_stream(futures_util::stream::unfold(
            (body.into_data_stream(), permit),
            |(mut s, p)| async move {
                use futures_util::StreamExt;
                s.next().await.map(|chunk| (chunk, (s, p)))
            },
        ));
        return Response::from_parts(parts, body);
    }
    drop(permit);
    r
}

/// A request let in, its body read; else the answer that turns it away. No web page but the
/// approval page's own form. A body is read only for what takes one, and only once it's known
/// who may send it (`/mcp`'s, an agent device): within that one's limit, within the room for
/// bodies on their way (in all, and its address's share), and whole within [`BODY_WITHIN`].
async fn admit(
    l: &Listener,
    req: axum::extract::Request,
) -> Result<axum::extract::Request, Response> {
    let (mut parts, body) = req.into_parts();
    let path = parts.uri.path();
    if let Some(origin) = parts.headers.get(header::ORIGIN) {
        // A form posted from the approval page itself is the one thing a browser sends here: its
        // origin ours, or `null` (an in-app browser, or a policy that hides it) when the browser
        // itself says the page posting it is ours (`Sec-Fetch-Site`, which no page can set).
        let same_site = parts
            .headers
            .get("sec-fetch-site")
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"same-origin"));
        let own = path == "/authorize"
            && parts.method == Method::POST
            && origin.to_str().ok().is_some_and(|o| {
                o.eq_ignore_ascii_case(&l.base(&parts.headers)) || (o == "null" && same_site)
            });
        if !own {
            return Err(refuse(
                plain(
                    StatusCode::FORBIDDEN,
                    "Remote MCP doesn't take requests from web pages",
                ),
                &parts.headers,
                body,
            )
            .await);
        }
    }
    // What takes a body, and how big; anything else is answered without reading one.
    let (max, form) = match (&parts.method, path) {
        (&Method::POST, MCP_PATH) => (BODY_MAX, false),
        (&Method::POST, "/register" | "/authorize" | "/token") => (FORM_MAX, true),
        _ => return Ok(axum::extract::Request::from_parts(parts, Body::empty())),
    };
    // Room for the body as it arrives, by the length it says (the most, when it doesn't).
    let said = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if said.is_some_and(|n| n > max as u64) {
        return Err(plain(StatusCode::PAYLOAD_TOO_LARGE, "Too big"));
    }
    let ip = ip_of(&parts.extensions, &parts.headers);
    if !form {
        match l.agent(&parts.headers, ip.clone()).await {
            Ok((device, token)) => {
                parts.extensions.insert(Agent { device, token });
            }
            Err(r) => return Err(refuse(r, &parts.headers, body).await),
        }
    }
    let has_body =
        said.is_some_and(|n| n > 0) || parts.headers.contains_key(header::TRANSFER_ENCODING);
    let kib = said.unwrap_or(max as u64).div_ceil(1024).max(1) as u32;
    let room = match has_body.then(|| l.room(pair_source(ip.as_deref()), kib, form)) {
        Some(None) => {
            return Err(plain(
                StatusCode::SERVICE_UNAVAILABLE,
                "Too busy: try again in a moment",
            ));
        }
        Some(room) => room,
        None => None,
    };
    let body = match tokio::time::timeout(BODY_WITHIN, axum::body::to_bytes(body, max)).await {
        Ok(Ok(b)) => b,
        Ok(Err(_)) => return Err(plain(StatusCode::PAYLOAD_TOO_LARGE, "Too big")),
        Err(_) => {
            return Err(plain(
                StatusCode::REQUEST_TIMEOUT,
                "The request took too long to arrive",
            ));
        }
    };
    drop(room);
    Ok(axum::extract::Request::from_parts(parts, Body::from(body)))
}

/// `r`, once a small body it turns away has been read (none of it kept), so closing the
/// connection on it doesn't lose the answer.
async fn refuse(r: Response, headers: &HeaderMap, body: Body) -> Response {
    drain_small(headers, body).await;
    r
}

fn plain(status: StatusCode, msg: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg.to_string(),
    )
        .into_response()
}

fn too_many() -> Response {
    let mut r = plain(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many requests: wait a minute, then try again",
    );
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    r
}

/// 401, pointing at how to sign in (RFC 9728).
fn unauthorized(base: &str, given: bool) -> Response {
    let mut r = plain(
        StatusCode::UNAUTHORIZED,
        if given {
            "That token isn't an agent device's on this server: sign in again"
        } else {
            "Send an agent device's token: Authorization: Bearer <token>"
        },
    );
    let error = if given {
        ", error=\"invalid_token\""
    } else {
        ""
    };
    if let Ok(v) = HeaderValue::from_str(&format!(
        "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"{error}"
    )) {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    r
}

/// A JSON-RPC error with no id, as HTTP `status`.
fn rpc_error(status: StatusCode, code: i64, msg: &str) -> Response {
    (
        status,
        Json(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": code, "message": msg } })),
    )
        .into_response()
}

/// The address clients reach this at, no slash at the end: the host the request was for (as
/// a proxy in front forwarded it), over HTTPS unless it's this machine; or `url` when that's
/// set (`--mcp-url`, Funnel's) and the request didn't come to this machine's own address.
fn base_of(url: Option<&str>, headers: &HeaderMap) -> String {
    let first = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let host = first("x-forwarded-host")
        .or_else(|| first(header::HOST.as_str()))
        .filter(|h| {
            h.len() <= 255
                && h.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
        })
        .unwrap_or("localhost");
    if let Some(u) = url
        && !loopback_host(host)
    {
        return u.to_string();
    }
    let proto = match first("x-forwarded-proto") {
        Some(p) if p.eq_ignore_ascii_case("http") => "http",
        Some(p) if p.eq_ignore_ascii_case("https") => "https",
        _ if loopback_host(host) => "http",
        _ => "https",
    };
    format!("{proto}://{host}")
}

fn loopback_host(host: &str) -> bool {
    let h = if host.starts_with('[') {
        host.split(']')
            .next()
            .unwrap_or_default()
            .trim_start_matches('[')
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// `n` random bytes, as URL-safe text.
fn random_id(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if getrandom::fill(&mut buf).is_err() {
        // No randomness is no way to make a secret: an id nobody can use.
        panic!("no randomness for a session or code");
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn sha256(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

async fn mcp_get() -> Response {
    let mut r = plain(
        StatusCode::METHOD_NOT_ALLOWED,
        "This server sends nothing unasked: POST to /mcp",
    );
    r.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST, DELETE"));
    r
}

async fn mcp_delete(State(l): State<Listener>, ext: Extensions, headers: HeaderMap) -> Response {
    let (device, _) = match l.agent(&headers, ip_of(&ext, &headers)).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Some(id) = header_str(&headers, "mcp-session-id") else {
        return plain(StatusCode::BAD_REQUEST, "Say which session: Mcp-Session-Id");
    };
    let mut inner = l.lock();
    match inner.sessions.get(id) {
        Some(s) if s.device == device => {
            if let Some(s) = inner.sessions.remove(id) {
                s.mcp.close();
            }
            Listener::sweep(&mut inner, Instant::now());
            StatusCode::NO_CONTENT.into_response()
        }
        _ => plain(StatusCode::NOT_FOUND, "No such session"),
    }
}

/// `POST /mcp`: one JSON-RPC message.
async fn mcp_post(
    State(l): State<Listener>,
    Extension(Agent { device, token }): Extension<Agent>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !l.lock().limits.requests.take(&device, Instant::now()) {
        return too_many();
    }
    if let Some(v) = header_str(&headers, "mcp-protocol-version")
        && !flotilla_mcp::PROTOCOL_VERSIONS.contains(&v.trim())
    {
        return plain(
            StatusCode::BAD_REQUEST,
            &format!(
                "This server speaks MCP {}, not {v}",
                flotilla_mcp::PROTOCOL_VERSIONS.join(", ")
            ),
        );
    }
    if let Some(kind) = header_str(&headers, header::CONTENT_TYPE.as_str())
        && !kind
            .trim()
            .to_ascii_lowercase()
            .starts_with("application/json")
    {
        return plain(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Send JSON: Content-Type: application/json",
        );
    }
    let accept = header_str(&headers, header::ACCEPT.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let stream_ok = accept.contains("text/event-stream");
    if !accept.is_empty()
        && !stream_ok
        && !accept.contains("application/json")
        && !accept.contains("*/*")
        && !accept.contains("application/*")
    {
        return plain(
            StatusCode::NOT_ACCEPTABLE,
            "Accept application/json or text/event-stream",
        );
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(m) => m,
        Err(e) => {
            return rpc_error(
                StatusCode::BAD_REQUEST,
                -32700,
                &format!("That isn't JSON: {e}"),
            );
        }
    };
    if !msg.is_object() {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            -32600,
            "Send one JSON-RPC message at a time, as an object",
        );
    }
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = msg.get("id").filter(|i| !i.is_null()).cloned();
    if method.as_deref() == Some("initialize") && id.is_some() {
        let (session, mcp) = match l.open(&device, token) {
            Ok(s) => s,
            Err(r) => return r,
        };
        let reply = tokio::task::spawn_blocking(move || mcp.handle_value(msg, &|_| {}))
            .await
            .ok()
            .flatten();
        let mut r = Json(reply.unwrap_or(Value::Null)).into_response();
        if let Ok(v) = HeaderValue::from_str(&session) {
            r.headers_mut().insert("mcp-session-id", v);
        }
        return r;
    }
    let Some(session) = header_str(&headers, "mcp-session-id") else {
        return rpc_error(
            StatusCode::BAD_REQUEST,
            -32600,
            "Start with initialize: this request has no Mcp-Session-Id",
        );
    };
    let Some(mcp) = l.session(session, &device) else {
        return rpc_error(
            StatusCode::NOT_FOUND,
            -32001,
            "No such session (it ended, or the server restarted): initialize again",
        );
    };
    // A notification or a response: taken, nothing to say.
    let Some(id) = id.filter(|_| method.is_some()) else {
        let _ = tokio::task::spawn_blocking(move || mcp.handle_value(msg, &|_| {})).await;
        return StatusCode::ACCEPTED.into_response();
    };
    let Some(calling) = l.calling(&device) else {
        return too_many();
    };
    let pending = Pending {
        mcp: mcp.clone(),
        id: id.clone(),
        answered: false,
    };
    if stream_ok && method.as_deref() == Some("tools/call") {
        return stream(mcp, msg, pending, calling);
    }
    // Answered as JSON, progress has nowhere to go: the call waits as one that takes none.
    let mut msg = msg;
    if let Some(meta) = msg
        .pointer_mut("/params/_meta")
        .and_then(Value::as_object_mut)
    {
        meta.remove("progressToken");
    }
    let mut pending = pending;
    let reply = tokio::task::spawn_blocking(move || {
        let _calling = calling;
        mcp.handle_value(msg, &|_| {})
    })
    .await;
    pending.answered = true;
    match reply {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(_) => rpc_error(StatusCode::INTERNAL_SERVER_ERROR, -32603, "The call failed"),
    }
}

/// What a streamed call sends: a notification while it runs, then its answer.
enum Frame {
    Note(Value),
    Last(Value),
}

/// A tool call answered as an event stream: progress as it comes, a keep-alive comment when
/// there's none for a while, then the answer. A client that goes away cancels it.
fn stream(mcp: Arc<Mcp>, msg: Value, pending: Pending, calling: Calling) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Frame>(64);
    let to = tx.clone();
    tokio::task::spawn_blocking(move || {
        let _calling = calling;
        let notify = move |v: Value| {
            let _ = to.blocking_send(Frame::Note(v));
        };
        let reply = mcp.handle_value(msg, &notify);
        let _ = tx.blocking_send(Frame::Last(reply.unwrap_or(Value::Null)));
    });
    let mut tick = tokio::time::interval(KEEP_ALIVE);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    struct Run {
        rx: tokio::sync::mpsc::Receiver<Frame>,
        tick: tokio::time::Interval,
        pending: Pending,
        first: bool,
    }
    let run = Run {
        rx,
        tick,
        pending,
        first: true,
    };
    let events = futures_util::stream::unfold(Some(run), |run| async move {
        let mut run = run?;
        if run.first {
            // The interval's first tick is at once.
            run.first = false;
            run.tick.tick().await;
        }
        let event = |v: &Value| Bytes::from(format!("event: message\ndata: {v}\n\n"));
        tokio::select! {
            f = run.rx.recv() => match f {
                Some(Frame::Note(v)) => Some((Ok::<Bytes, std::convert::Infallible>(event(&v)), Some(run))),
                Some(Frame::Last(v)) => {
                    run.pending.answered = true;
                    Some((Ok(event(&v)), None))
                }
                None => None,
            },
            _ = run.tick.tick() => Some((Ok(Bytes::from_static(b": keep-alive\n\n")), Some(run))),
        }
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(events))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// OAuth.

/// What a registered client is: its name and where its codes may go. The client id is this,
/// encoded: registering keeps nothing, so anyone may (as dynamic registration lets anyone).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Client {
    #[serde(rename = "n")]
    name: String,
    #[serde(rename = "r")]
    redirects: Vec<String>,
}

const CLIENT_PREFIX: &str = "fmc_";

impl Client {
    fn id(&self) -> String {
        format!(
            "{CLIENT_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(self).unwrap_or_default())
        )
    }

    fn parse(id: &str) -> Option<Client> {
        let enc = id.strip_prefix(CLIENT_PREFIX)?;
        if enc.len() > 8 << 10 {
            return None;
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(enc)
            .ok()?;
        let c: Client = serde_json::from_slice(&bytes).ok()?;
        (!c.redirects.is_empty()
            && c.redirects.len() <= REDIRECTS_MAX
            && c.redirects.iter().all(|r| redirect_ok(r))
            && clean_name(&c.name) == c.name)
            .then_some(c)
    }

    /// The registered redirect `asked` matches: exactly, or a loopback one on any port (an app
    /// on the client's machine picks its port as it signs in, RFC 8252).
    fn redirect(&self, asked: Option<&str>) -> Option<String> {
        match asked {
            None if self.redirects.len() == 1 => Some(self.redirects[0].clone()),
            None => None,
            Some(a) => self
                .redirects
                .iter()
                .any(|r| {
                    r == a
                        || (loopback_redirect(r)
                            && loopback_redirect(a)
                            && without_port(r) == without_port(a))
                })
                .then(|| a.to_string()),
        }
    }
}

/// A client's name, as shown and as its device is named: no control characters, not too long.
fn clean_name(s: &str) -> String {
    let s: String = s
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    let s = flotilla_core::util::truncate_chars(&s, NAME_LEN)
        .trim()
        .to_string();
    if s.is_empty() {
        "An MCP client".to_string()
    } else {
        s
    }
}

/// A scheme and what follows it.
fn scheme_of(uri: &str) -> Option<(String, &str)> {
    let (scheme, rest) = uri.split_once(':')?;
    let ok = scheme.len() >= 2
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b));
    ok.then(|| (scheme.to_ascii_lowercase(), rest))
}

/// `//host[:port]` from what follows a scheme, if it has one.
fn authority(rest: &str) -> Option<&str> {
    let a = rest.strip_prefix("//")?;
    Some(a.split(['/', '?']).next().unwrap_or_default())
}

/// Where a code may be sent: an `https` address, `http` only on the client's own machine, or
/// an app's own scheme (`cursor://…`). Never a script, data or file, nor a fragment.
fn redirect_ok(uri: &str) -> bool {
    if uri.len() > REDIRECT_LEN
        || uri.contains('#')
        || uri.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let Some((scheme, rest)) = scheme_of(uri) else {
        return false;
    };
    match scheme.as_str() {
        "https" => authority(rest).is_some_and(|a| !a.is_empty() && !a.contains('@')),
        "http" => loopback_redirect(uri),
        "javascript" | "data" | "file" | "vbscript" | "about" | "blob" | "filesystem" | "ftp"
        | "ws" | "wss" | "mailto" | "tel" | "sms" => false,
        _ => !rest.is_empty() && authority(rest).is_none_or(|a| !a.contains('@')),
    }
}

/// `http://` to 127.0.0.1, ::1 or localhost.
fn loopback_redirect(uri: &str) -> bool {
    let Some(("http", rest)) = scheme_of(uri).as_ref().map(|(s, r)| (s.as_str(), *r)) else {
        return false;
    };
    authority(rest).is_some_and(|a| !a.contains('@') && loopback_host(a))
}

fn without_port(uri: &str) -> String {
    let Some((scheme, rest)) = scheme_of(uri) else {
        return uri.to_string();
    };
    let Some(a) = authority(rest) else {
        return uri.to_string();
    };
    let host = if a.starts_with('[') {
        a.split(']')
            .next()
            .map(|h| format!("{h}]"))
            .unwrap_or_default()
    } else {
        a.rsplit_once(':').map_or(a, |(h, _)| h).to_string()
    };
    format!("{scheme}://{host}{}", &rest[2 + a.len()..])
}

/// What the approval page says a code goes to: its scheme and host.
fn destination(uri: &str) -> String {
    match scheme_of(uri) {
        Some((scheme, rest)) => match authority(rest) {
            Some(a) => format!("{scheme}://{a}"),
            None => format!("{scheme}:"),
        },
        None => uri.to_string(),
    }
}

/// Room for an open request from this address.
fn open_limit(l: &Listener, ext: &Extensions, headers: &HeaderMap) -> Result<(), Response> {
    let from = pair_source(ip_of(ext, headers).as_deref());
    if l.lock().limits.open.take(&from, Instant::now()) {
        Ok(())
    } else {
        Err(too_many())
    }
}

/// RFC 9728: what this resource is, and who signs clients in for it.
async fn resource_metadata(
    State(l): State<Listener>,
    ext: Extensions,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = open_limit(&l, &ext, &headers) {
        return r;
    }
    let base = l.base(&headers);
    Json(json!({
        "resource": format!("{base}{MCP_PATH}"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "scopes_supported": [SCOPE],
        "resource_name": "Flotilla",
    }))
    .into_response()
}

/// RFC 8414: how to sign in.
async fn server_metadata(
    State(l): State<Listener>,
    ext: Extensions,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = open_limit(&l, &ext, &headers) {
        return r;
    }
    let base = l.base(&headers);
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [SCOPE],
    }))
    .into_response()
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response()
}

/// RFC 7591: a client registers its name and redirect URIs, and is told its id. A public
/// client (PKCE, no secret), whatever it asked.
async fn register(
    State(l): State<Listener>,
    ext: Extensions,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let from = pair_source(ip_of(&ext, &headers).as_deref());
    if !l.lock().limits.registrations.take(&from, Instant::now()) {
        return too_many();
    }
    let Ok(asked) = serde_json::from_slice::<Value>(&body) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "Send the client's metadata as JSON",
        );
    };
    let redirects: Vec<String> = asked["redirect_uris"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if redirects.is_empty() || redirects.len() > REDIRECTS_MAX {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            &format!("Give 1 to {REDIRECTS_MAX} redirect_uris"),
        );
    }
    if let Some(bad) = redirects.iter().find(|r| !redirect_ok(r)) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            &format!(
                "{} can't take a code: use https, http on this machine (127.0.0.1), or an app's own scheme",
                flotilla_core::util::ellipsize(bad, 80)
            ),
        );
    }
    let name = clean_name(asked["client_name"].as_str().unwrap_or(""));
    let client = Client { name, redirects };
    eprintln!("[mcp] registered a client, {:?}, from {from}", client.name);
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client.id(),
            "client_id_issued_at": flotilla_core::util::now_ms() / 1000,
            "client_name": client.name,
            "redirect_uris": client.redirects,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "scope": SCOPE,
        })),
    )
        .into_response()
}

/// An authorization request, from the query or the approval form.
#[derive(Debug, Default, serde::Deserialize)]
struct Asked {
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    client_id: String,
    redirect_uri: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    resource: Option<String>,
    /// The form's: the pairing code typed, and which button.
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    action: Option<String>,
}

/// An authorization request checked: the client, and where its answer goes.
struct Checked {
    client: Client,
    redirect: String,
}

/// What's wrong with a request before there's anywhere safe to send it back: shown, never
/// redirected (so this is no open redirect).
fn check_client(a: &Asked) -> Result<Checked, String> {
    let client = Client::parse(&a.client_id).ok_or(
        "This sign-in link names no client this server knows. Add Flotilla to the app again.",
    )?;
    let redirect = client.redirect(a.redirect_uri.as_deref()).ok_or("This sign-in link sends the answer somewhere its app didn't register. Add Flotilla to the app again.")?;
    Ok(Checked { client, redirect })
}

/// What's wrong with the rest, to send back to the client.
fn check_request(a: &Asked) -> Result<(), (&'static str, &'static str)> {
    if a.response_type != "code" {
        return Err((
            "unsupported_response_type",
            "Only the authorization code flow",
        ));
    }
    if a.code_challenge_method != "S256"
        || a.code_challenge.len() != 43
        || !a
            .code_challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(("invalid_request", "PKCE with S256 is required"));
    }
    if a.state.as_ref().is_some_and(|s| s.len() > 1024)
        || a.scope.as_ref().is_some_and(|s| s.len() > 256)
        || a.resource.as_ref().is_some_and(|s| s.len() > 512)
    {
        return Err(("invalid_request", "A parameter is too long"));
    }
    Ok(())
}

/// The client's redirect URI with `params` added to its query.
fn back(redirect: &str, params: &[(&str, &str)]) -> String {
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    let q: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    let sep = if redirect.contains('?') { '&' } else { '?' };
    format!("{redirect}{sep}{}", q.join("&"))
}

fn redirect_to(to: &str) -> Response {
    match HeaderValue::from_str(to) {
        Ok(v) => (StatusCode::FOUND, [(header::LOCATION, v)]).into_response(),
        Err(_) => page(
            StatusCode::BAD_REQUEST,
            "Can't sign in",
            "<p>The app's address for the answer isn't one a browser can go to.</p>",
        ),
    }
}

/// Send the client an error (its redirect is registered, so this is safe).
fn back_with_error(c: &Checked, a: &Asked, error: &str, description: &str) -> Response {
    let mut params = vec![("error", error), ("error_description", description)];
    if let Some(s) = &a.state {
        params.push(("state", s));
    }
    redirect_to(&back(&c.redirect, &params))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A page of the approval flow.
fn page(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content=\"width=device-width, initial-scale=1\"><title>{t}</title><style>\
body{{font:15px/1.5 -apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif;max-width:26rem;margin:4rem auto;padding:0 1.25rem;color:#1d1d1f;background:#fff}}\
@media (prefers-color-scheme:dark){{body{{color:#f2f2f2;background:#161616}}input{{background:#222;color:#f2f2f2;border-color:#444}}.dim{{color:#9a9a9a}}}}\
h1{{font-size:1.3rem;margin:0 0 1rem}}.dim{{color:#6e6e73;font-size:.9rem}}.error{{color:#d70015}}\
input{{font:inherit;font-family:ui-monospace,Menlo,monospace;letter-spacing:.08em;width:100%;box-sizing:border-box;padding:.55rem .7rem;border:1px solid #c7c7cc;border-radius:8px;margin:.35rem 0}}\
.buttons{{display:flex;gap:.6rem;margin-top:1rem}}button{{font:inherit;padding:.5rem 1.1rem;border-radius:8px;border:1px solid #c7c7cc;background:transparent;color:inherit;cursor:pointer}}\
button.go{{background:#0071e3;border-color:#0071e3;color:#fff}}code{{font-family:ui-monospace,Menlo,monospace;font-size:.85em}}\
</style></head><body><h1>{t}</h1>{body}</body></html>",
        t = esc(title)
    );
    let mut r = (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response();
    r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'"));
    r
}

/// The approval page: who asks, where access goes, and the code to type.
fn approval(status: StatusCode, c: &Checked, a: &Asked, error: Option<&str>) -> Response {
    let hidden = |name: &str, v: Option<&str>| {
        v.map(|v| format!("<input type=hidden name={name} value=\"{}\">", esc(v)))
            .unwrap_or_default()
    };
    let fields = [
        hidden("response_type", Some(&a.response_type)),
        hidden("client_id", Some(&a.client_id)),
        hidden("redirect_uri", Some(&c.redirect)),
        hidden("state", a.state.as_deref()),
        hidden("code_challenge", Some(&a.code_challenge)),
        hidden("code_challenge_method", Some(&a.code_challenge_method)),
        hidden("scope", a.scope.as_deref()),
        hidden("resource", a.resource.as_deref()),
    ]
    .concat();
    let error = error
        .map(|e| format!("<p class=error>{}</p>", esc(e)))
        .unwrap_or_default();
    let body = format!(
        "<p>It will start, read and follow threads, and answer their agents. It can't delete threads, merge or change settings.</p>\
<p class=dim>Access goes to <strong>{dest}</strong></p>\
<form method=post action=/authorize>{fields}\
<label for=code>Pairing code</label><input id=code name=code autocomplete=one-time-code autocapitalize=characters spellcheck=false placeholder=ABCD-EFGH-JKMN autofocus>\
{error}<p class=dim>Add device in Settings → Devices, or <code>flotilla pair --agent</code> on the server.</p>\
<div class=buttons><button class=go name=action value=allow>Allow</button><button name=action value=deny>Deny</button></div></form>",
        dest = esc(&destination(&c.redirect)),
    );
    page(
        status,
        &format!("Allow {} to use Flotilla?", c.client.name),
        &body,
    )
}

/// `GET /authorize`: the approval page.
async fn authorize_page(
    State(l): State<Listener>,
    ext: Extensions,
    headers: HeaderMap,
    query: axum::extract::RawQuery,
) -> Response {
    if let Err(r) = open_limit(&l, &ext, &headers) {
        return r;
    }
    let a: Asked = serde_urlencoded_from(query.0.as_deref().unwrap_or(""));
    let c = match check_client(&a) {
        Ok(c) => c,
        Err(why) => {
            return page(
                StatusCode::BAD_REQUEST,
                "Can't sign in",
                &format!("<p>{}</p>", esc(&why)),
            );
        }
    };
    if let Err((error, description)) = check_request(&a) {
        return back_with_error(&c, &a, error, description);
    }
    approval(StatusCode::OK, &c, &a, None)
}

/// A form or query, read leniently: what doesn't read is left empty (and then refused).
fn serde_urlencoded_from<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    serde_urlencoded::from_str(s).unwrap_or_default()
}

/// `POST /authorize`: the code typed (or Deny). A right code pairs an agent device named for
/// the client, and the client gets a code for its token.
async fn authorize(
    State(l): State<Listener>,
    ext: Extensions,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let a: Asked = serde_urlencoded_from(std::str::from_utf8(&body).unwrap_or(""));
    let c = match check_client(&a) {
        Ok(c) => c,
        Err(why) => {
            return page(
                StatusCode::BAD_REQUEST,
                "Can't sign in",
                &format!("<p>{}</p>", esc(&why)),
            );
        }
    };
    if let Err((error, description)) = check_request(&a) {
        return back_with_error(&c, &a, error, description);
    }
    if a.action.as_deref() != Some("allow") {
        return back_with_error(&c, &a, "access_denied", "Not allowed");
    }
    let code = a.code.as_deref().unwrap_or("").trim().to_string();
    if code.is_empty() {
        return approval(StatusCode::BAD_REQUEST, &c, &a, Some("Type a pairing code"));
    }
    // Wrong codes count against the same limit as the main API's pairing.
    let ip = ip_of(&ext, &headers);
    let taken = l
        .app
        .tries
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take(&pair_source(ip.as_deref()), Instant::now());
    let me = match taken {
        Ok(me) => me,
        Err(log) => {
            if let Some(log) = log {
                eprintln!("[mcp] {log}");
            }
            return approval(
                StatusCode::TOO_MANY_REQUESTS,
                &c,
                &a,
                Some("Too many wrong codes: wait a minute, then try again"),
            );
        }
    };
    l.expire_grants();
    if l.lock().grants.len() >= GRANTS_MAX {
        return approval(
            StatusCode::SERVICE_UNAVAILABLE,
            &c,
            &a,
            Some("Too many sign-ins at once: try again in a minute"),
        );
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let req = PairRequest {
        code: Secret(code),
        device_name: c.client.name.clone(),
        platform: PLATFORM.into(),
        scope: Some(Scope::Agent),
    };
    // Noted with the device, in the same write, until its client has the token.
    l.app
        .host
        .pair_noting(req, ip.clone(), Some(PENDING), move |r| {
            let _ = tx.send(r);
        });
    let result = rx
        .await
        .unwrap_or_else(|_| Err(PairError::Db("the server is stopping".into())));
    if !matches!(&result, Err(e) if e.guessing()) {
        l.app
            .tries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .at
            .retain(|(n, _, _)| *n != me);
    }
    let from = ip.as_deref().unwrap_or("this machine");
    let paired = match result {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[mcp] didn't sign in {:?} from {from}: {e}", c.client.name);
            let status = if matches!(e, PairError::Db(_)) {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_REQUEST
            };
            return approval(status, &c, &a, Some(&e.to_string()));
        }
    };
    eprintln!(
        "[mcp] signed in {:?} as agent device {} from {from}",
        c.client.name, paired.device
    );
    let grant = random_id(32);
    l.lock().grants.insert(
        sha256(&grant),
        Grant {
            client: a.client_id.clone(),
            redirect: c.redirect.clone(),
            challenge: a.code_challenge.clone(),
            device: paired.device,
            token: Secret(paired.token),
            at: Instant::now(),
        },
    );
    let mut params = vec![("code", grant.as_str())];
    if let Some(s) = &a.state {
        params.push(("state", s));
    }
    redirect_to(&back(&c.redirect, &params))
}

/// A token request's form.
#[derive(Debug, Default, serde::Deserialize)]
struct TokenForm {
    #[serde(default)]
    grant_type: String,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
}

/// The token's answer.
fn tokens(token: &Secret) -> Response {
    Json(json!({ "access_token": token.0, "token_type": "Bearer", "refresh_token": token.0, "scope": SCOPE })).into_response()
}

/// `POST /token`: a code (with its PKCE verifier) for the device's token; or a refresh, which
/// is that token again while its device is paired.
async fn token(
    State(l): State<Listener>,
    ext: Extensions,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ip = ip_of(&ext, &headers);
    let from = pair_source(ip.as_deref());
    if !l.lock().limits.tokens.take(&from, Instant::now()) {
        return too_many();
    }
    let f: TokenForm = serde_urlencoded_from(std::str::from_utf8(&body).unwrap_or(""));
    match f.grant_type.as_str() {
        "authorization_code" => {}
        "refresh_token" => {
            let Some(refresh) = f.refresh_token.filter(|t| !t.is_empty() && t.len() <= 256) else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "Send refresh_token",
                );
            };
            let mut auth = HeaderMap::new();
            if let Ok(v) = HeaderValue::from_str(&format!("Bearer {refresh}")) {
                auth.insert(header::AUTHORIZATION, v);
            }
            return match l.agent(&auth, ip).await {
                Ok((_, token)) => tokens(&token),
                Err(r) if r.status() == StatusCode::TOO_MANY_REQUESTS => r,
                Err(_) => oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "That device was signed out: sign in again",
                ),
            };
        }
        _ => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "authorization_code or refresh_token",
            );
        }
    }
    l.expire_grants();
    let Some(code) = f.code.filter(|c| c.len() <= 128) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "Send code");
    };
    // Used once, right or wrong.
    let Some(grant) = l.lock().grants.remove(&sha256(&code)) else {
        l.lock().limits.wrong.add(&from, Instant::now());
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "That code isn't one this server gave, or it was used or expired",
        );
    };
    let verifier = f.code_verifier.unwrap_or_default();
    let verifier_ok = (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let pkce = verifier_ok
        && bool::from(subtle::ConstantTimeEq::ct_eq(
            challenge.as_bytes(),
            grant.challenge.as_bytes(),
        ));
    let client_ok = f.client_id.as_deref().is_none_or(|c| c == grant.client);
    let redirect_ok = f
        .redirect_uri
        .as_deref()
        .is_none_or(|r| r == grant.redirect);
    if !(pkce && client_ok && redirect_ok) || Instant::now().duration_since(grant.at) > GRANT_TTL {
        eprintln!(
            "[mcp] refused a code for device {} from {from}: {}",
            grant.device,
            if !pkce {
                "wrong verifier"
            } else if !client_ok {
                "another client"
            } else if !redirect_ok {
                "another redirect"
            } else {
                "expired"
            }
        );
        l.revoke(grant.device);
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "That code doesn't go with this request",
        );
    }
    // Before the token goes, in one write: a device whose client has its token is never revoked
    // as pending, and one revoked first (its code ran out meanwhile) gives no token.
    let (db, device) = (l.db.clone(), grant.device.clone());
    let settled = tokio::task::spawn_blocking(move || {
        // Not between a rebuild reading the devices and its swap: it would carry the note.
        let _held = flotilla_core::devices::hold(&db)?;
        flotilla_core::store::Store::open(&db).and_then(|s| s.settle_noted(PENDING, &device))
    })
    .await;
    match settled {
        Ok(Ok(true)) => tokens(&grant.token),
        Ok(Ok(false)) => oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "That code expired: sign in again",
        ),
        Ok(Err(e)) => {
            eprintln!(
                "[mcp] couldn't finish signing in device {}: {e}",
                grant.device
            );
            oauth_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "Couldn't finish signing in: sign in again",
            )
        }
        Err(_) => oauth_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "temporarily_unavailable",
            "The server is stopping",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_are_https_loopback_http_or_an_apps_own() {
        for ok in [
            "https://grok.com/connectors-oauth-exchange-code/",
            "https://agent.meta.ai/api/hatch/oauth/callback",
            "cursor://anysphere.cursor-mcp/oauth/callback",
            "http://127.0.0.1:33418/callback",
            "http://localhost:8787/callback",
            "http://[::1]:9000/cb",
            "com.example.app:/oauth",
        ] {
            assert!(redirect_ok(ok), "{ok}");
        }
        for bad in [
            "http://evil.example/cb",
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
            "https://grok.com/cb#frag",
            "https://user@evil.example/cb",
            "https:///nohost",
            "https://grok.com/a b",
            "x:",
            "",
            &format!("https://e.example/{}", "a".repeat(600)),
        ] {
            assert!(!redirect_ok(bad), "{bad}");
        }
    }

    #[test]
    fn a_client_id_carries_its_registration_and_nothing_else_passes() {
        let c = Client {
            name: "Grok".into(),
            redirects: vec![
                "https://grok.com/cb".into(),
                "http://127.0.0.1:8787/callback".into(),
            ],
        };
        let id = c.id();
        assert_eq!(Client::parse(&id), Some(c.clone()));
        assert_eq!(
            c.redirect(Some("https://grok.com/cb")).as_deref(),
            Some("https://grok.com/cb")
        );
        // A loopback redirect on any port.
        assert_eq!(
            c.redirect(Some("http://127.0.0.1:51234/callback"))
                .as_deref(),
            Some("http://127.0.0.1:51234/callback")
        );
        assert_eq!(c.redirect(Some("http://127.0.0.1:51234/other")), None);
        assert_eq!(c.redirect(Some("https://grok.com/cb2")), None);
        // Two registered: say which.
        assert_eq!(c.redirect(None), None);
        // Forged ones that would carry a bad redirect or name don't parse.
        let forge = |c: &Client| {
            format!(
                "{CLIENT_PREFIX}{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::to_vec(c).unwrap())
            )
        };
        assert_eq!(
            Client::parse(&forge(&Client {
                name: "x".into(),
                redirects: vec!["javascript:alert(1)".into()]
            })),
            None
        );
        assert_eq!(
            Client::parse(&forge(&Client {
                name: "x\u{7}".into(),
                redirects: vec!["https://a.example/".into()]
            })),
            None
        );
        assert_eq!(Client::parse("fmc_!!"), None);
        assert_eq!(Client::parse("someone-else"), None);
    }

    #[test]
    fn limits_count_each_and_all() {
        let mut w = Window::new(2, 3);
        let now = Instant::now();
        assert!(w.take("a", now) && w.take("a", now));
        assert!(!w.take("a", now), "a has had its two");
        assert!(w.take("b", now));
        assert!(!w.take("c", now), "three in all");
        assert!(
            w.take("c", now + WINDOW + Duration::from_secs(1)),
            "a minute on"
        );
    }

    #[test]
    fn the_base_address_is_the_one_asked_for() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8787"));
        assert_eq!(base_of(None, &h), "http://127.0.0.1:8787");
        // Behind Funnel, a tunnel or exe.dev's proxy: the address the client asked for.
        h.insert(
            "x-forwarded-host",
            HeaderValue::from_static("box.tail1234.ts.net:8443"),
        );
        assert_eq!(base_of(None, &h), "https://box.tail1234.ts.net:8443");
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        h.insert(header::HOST, HeaderValue::from_static("box.example.com"));
        h.remove("x-forwarded-host");
        assert_eq!(base_of(None, &h), "https://box.example.com");
        assert_eq!(
            base_of(Some("https://mcp.example.com"), &h),
            "https://mcp.example.com"
        );
        // Asked on this machine's own address: that one, whatever's published.
        let mut local = HeaderMap::new();
        local.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8787"));
        assert_eq!(
            base_of(Some("https://mcp.example.com"), &local),
            "http://127.0.0.1:8787"
        );
        // A host that isn't one isn't repeated back.
        h.insert(
            header::HOST,
            HeaderValue::from_static("evil.example/\"\"><script>"),
        );
        assert_eq!(base_of(None, &h), "https://localhost");
        assert!(
            loopback_host("localhost:1")
                && loopback_host("[::1]:2")
                && !loopback_host("box.example.com")
        );
    }

    #[test]
    fn a_redirect_keeps_its_query_and_adds_the_answer() {
        assert_eq!(
            back(
                "https://grok.com/cb",
                &[("code", "abc"), ("state", "x y&z")]
            ),
            "https://grok.com/cb?code=abc&state=x%20y%26z"
        );
        assert_eq!(
            back("https://a.example/cb?k=1", &[("code", "c")]),
            "https://a.example/cb?k=1&code=c"
        );
        assert_eq!(
            destination("https://grok.com/connectors-oauth-exchange-code/"),
            "https://grok.com"
        );
        assert_eq!(
            destination("cursor://anysphere.cursor-mcp/oauth/callback"),
            "cursor://anysphere.cursor-mcp"
        );
        assert_eq!(
            without_port("http://127.0.0.1:5555/cb"),
            "http://127.0.0.1/cb"
        );
    }
}
