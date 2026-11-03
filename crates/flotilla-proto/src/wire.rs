//! The protocol on the wire: one WebSocket per client (`/v1/ws`), a JSON object per text
//! frame, tagged by `t`. The app never sees these: a client (`flotilla-client`, or the app's
//! in-process host) turns them into the `ClientMsg`/`ServerMsg` it speaks.
//!
//! - The client says `hello` first: who it is, its session, and the views it holds (the
//!   revision of each), to resume. The server answers `welcome`, then catches each view up
//!   (nothing if it's current, the deltas it missed if they're still kept, or a snapshot),
//!   then sends `synced`.
//! - A view comes as a `snap` (whole, at a revision) and then `delta`s, each from the
//!   revision the client holds (`base`) to a new one. Revisions only grow within one server
//!   run (`epoch`); a client that sees a gap, or anything it can't apply, asks for the view
//!   again with `resync`.
//! - Answers (`res`, `progress`) go only to the client that asked; notes go to every client;
//!   live text only to the clients that have that thread open, and is never replayed.
//! - Answers, progress and notes carry `n`, counting up within the client's session. A client
//!   that reconnects says the last it got (`hello.got`), and the server sends again what came
//!   after, if it still has it (else `welcome.resumed` is false: a new session).

use crate::{Note, RawEvent, Reply, ReqId, Request, Scope, Secret, ViewKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The WebSocket's path.
pub const WS_PATH: &str = "/v1/ws";
/// Where a device swaps a pairing code for its token (`POST`, [`PairRequest`] → [`Paired`]).
pub const PAIR_PATH: &str = "/v1/pair";
/// The header (or `?proto=` on the URL, for a browser) that says which protocol a client
/// speaks. A server that speaks another answers the upgrade with 426.
pub const PROTOCOL_HEADER: &str = "flotilla-protocol";

/// What a server can do beyond the protocol's version, for clients to check before they use
/// it (never the version number).
pub const CAPABILITIES: &[&str] = &["deltas", "resume", "progress", "blobs"];

/// Client → server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Up {
    /// First, once per connection.
    Hello(Hello),
    Req {
        id: ReqId,
        #[serde(flatten)]
        req: Request,
    },
    /// Send this view now and as it changes. A thread subscribed to is on screen: polled
    /// every second, and read as things arrive.
    Sub {
        #[serde(flatten)]
        view: ViewKey,
    },
    Unsub {
        #[serde(flatten)]
        view: ViewKey,
    },
    /// A view subscribed to that the client lost track of (a gap, or a delta it couldn't
    /// apply): send it whole. Unlike `sub`, it doesn't count as opening the thread again.
    Resync {
        #[serde(flatten)]
        view: ViewKey,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub proto: u32,
    /// `flotilla-desktop`, `flotilla-cli`…
    pub app: String,
    pub version: String,
    pub platform: String,
    /// This run of the client, made at random: reconnecting with it soon after a drop brings
    /// the answers and notes sent meanwhile.
    pub session: String,
    /// The server run the revisions below are from.
    #[serde(default)]
    pub epoch: Option<String>,
    /// The views it subscribes to, with the revision it holds of each.
    #[serde(default)]
    pub subs: Vec<Held>,
    /// The last answer, progress or note (`n`) it got in this session.
    #[serde(default)]
    pub got: Option<u64>,
    /// Hold this connection to less than its device may do (`flotilla mcp` asks for
    /// [`Scope::Agent`] on this machine's socket too). It can only narrow: a device paired as an
    /// agent stays one whatever this says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
}

/// A view a client subscribes to, and the revision of it it holds (none: send it whole).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Held {
    #[serde(flatten)]
    pub view: ViewKey,
    #[serde(default)]
    pub rev: Option<u64>,
}

/// Server → client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Down {
    Welcome(Welcome),
    /// A request's answer, after the views it changed.
    Res {
        id: ReqId,
        result: Result<Reply, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        n: Option<u64>,
    },
    /// A view, whole, at revision `rev`. It replaces whatever the client held.
    Snap {
        #[serde(flatten)]
        view: ViewKey,
        rev: u64,
        data: Value,
    },
    /// A view's change from revision `base`, which the client must hold, to `rev`.
    Delta {
        #[serde(flatten)]
        view: ViewKey,
        base: u64,
        rev: u64,
        ops: Vec<Op>,
    },
    /// An open thread's streaming text, whole each time. Never stored or replayed.
    Live {
        thread: String,
        partials: Vec<RawEvent>,
    },
    Note {
        note: Note,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        n: Option<u64>,
    },
    /// Part of a request's answer: repos a page at a time, scripts suggested, branches.
    Progress {
        id: ReqId,
        note: Note,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        n: Option<u64>,
    },
    /// Everything the hello asked for has been sent: what the client shows is live.
    Synced,
    /// The server won't go on with this client (a hello with another protocol or another
    /// device's session, or this device was revoked): why. The server closes.
    Error {
        msg: String,
    },
}

impl Down {
    /// The number answers, progress and notes carry within a session.
    pub fn n(&self) -> Option<u64> {
        match self {
            Down::Res { n, .. } | Down::Note { n, .. } | Down::Progress { n, .. } => *n,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub proto: u32,
    pub server: ServerInfo,
    /// This server run, made at random: revisions from another one mean nothing here.
    pub epoch: String,
    /// The session was known, with everything after `hello.got`: that follows.
    pub resumed: bool,
    /// The last request (by id: a client's ids only grow) the server took from this session
    /// before it dropped: one after it never arrived, and should be sent again.
    #[serde(default)]
    pub seen: Option<ReqId>,
    pub caps: Vec<String>,
    /// The paired device this connection is, by its token; none for one this machine trusts
    /// (the server's Unix socket, or the app's own host).
    #[serde(default)]
    pub device: Option<String>,
    /// The Flotillas whose databases were imported into this one (`flotilla import`): a
    /// client whose own database has one of these ids knows it moved here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adopted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Kept with the database, so it's the same across restarts.
    pub id: String,
    pub version: String,
}

/// `GET /.well-known/flotilla`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descriptor {
    pub server: ServerInfo,
    pub protocol: u32,
    pub capabilities: Vec<String>,
}

/// `POST /v1/pair`: a pairing code, and what to call the device it pairs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    pub code: Secret,
    pub device_name: String,
    pub platform: String,
    /// Pair for less than the code allows (`flotilla connect` asks for [`Scope::Agent`]). The
    /// device gets the narrower of this and the code's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
}

/// `POST /v1/pair`'s answer: the new device and its token (sent this once; the server keeps
/// only its hash), and which server it's for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paired {
    pub device: String,
    pub token: Secret,
    pub server: ServerInfo,
}

/// `PUT /v1/blobs`' answer: the stored file's id (from its contents) and size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    pub id: String,
    pub size: u64,
}

/// One change in a delta, on the view's JSON (see `crate::delta`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// A field, replaced whole.
    Set { field: String, v: Value },
    /// A field gone.
    Unset { field: String },
    /// An entry of a keyed list (`crate::delta::keyed`), new (appended) or changed (in place).
    Put { field: String, v: Value },
    /// Entries of a keyed list gone, by key.
    Del { field: String, keys: Vec<Value> },
    /// A keyed list's keys in their new order, when the puts and dels don't leave it so.
    Order { field: String, keys: Vec<Value> },
    /// One from a newer server: skipped (the revision still moves on).
    #[serde(other)]
    Unknown,
}
