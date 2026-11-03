//! Flotilla for other agents: an MCP server that lets an assistant (Claude Code, OpenClaw,
//! Hermes Agent) start threads on a Flotilla server, read them, send follow-ups, wait for a
//! turn to end and answer an agent's questions. It's a client of the server like the apps are
//! ([`link::Link`]), as an agent device: the server, not this crate, decides what that may do
//! (`flotilla_proto::Scope::Agent`). Design and setup: `design/agent-api.md`.
//!
//! [`Mcp::handle`] takes one JSON-RPC message and gives the reply to write, whatever carries
//! them; [`serve_stdio`] carries them over stdin and stdout, a line each (`flotilla mcp`), and
//! the server's MCP listener over streamable HTTP (`flotilla serve --mcp-port`, in
//! `flotilla-server`). Each request runs on a thread of its own, so a long `wait_for_turn`
//! doesn't hold up the rest.

pub mod link;
mod tools;

pub use link::{Door, Link};

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// The MCP versions this speaks, newest first. A client asking for another is answered with
/// the newest.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// What an assistant is told about this server as it connects (`instructions`).
const INSTRUCTIONS: &str = "Flotilla runs coding agents (Claude Code, Codex, OpenCode), each in a VM of its own, on the user's Flotilla server. Start one with start_thread (list_models and list_projects say what to pick), then wait_for_turn for its answer; send_message follows up (or steers a turn that's running). A turn takes minutes: wait_for_turn returns after timeout_seconds, so call it again while the thread works. read_thread reads a thread compactly; pass seq for one message whole. Thread ids may be shortened to their first 8 characters.";

/// JSON-RPC's error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// An MCP server over a connection to Flotilla.
pub struct Mcp {
    link: Arc<Link>,
    /// When this server last started or sent to each thread (and the time then, in ms): a
    /// turn that hasn't shown as working, nor had anything happen since, is waited for a
    /// moment before it's called over.
    sent: Mutex<HashMap<String, (Instant, i64)>>,
    /// Requests running now, by id as JSON: only these can be cancelled.
    running: Mutex<HashSet<String>>,
    /// Requests the client gave up on (`notifications/cancelled`), by id as JSON.
    cancelled: Mutex<HashSet<String>>,
    /// The client is going (stdin closed): waits end now, as if each were cancelled.
    closing: AtomicBool,
    /// The longest `wait_for_turn` for a call that takes no progress: a client that hears
    /// nothing while a tool runs gives up after its own timeout (two minutes, for some over
    /// HTTP). None: as long as asked.
    quiet_wait: Option<u64>,
}

/// Where notifications go while a request runs (progress, to keep a long wait alive).
pub type Notify<'a> = &'a (dyn Fn(Value) + Sync);

impl Mcp {
    pub fn new(link: Link) -> Mcp {
        Mcp::shared(Arc::new(link), None)
    }

    /// One of several sessions on one connection (one device's, over HTTP), each waiting
    /// quietly at most `quiet_wait` seconds.
    pub fn shared(link: Arc<Link>, quiet_wait: Option<u64>) -> Mcp {
        Mcp {
            link,
            sent: Mutex::default(),
            running: Mutex::default(),
            cancelled: Mutex::default(),
            closing: AtomicBool::new(false),
            quiet_wait,
        }
    }

    /// The client gave up on request `id` (it went away): as `notifications/cancelled` says.
    pub fn cancel(&self, id: &Value) {
        self.cancel_key(id.to_string());
    }

    /// Cancel a request by its id as JSON, if it's running (nothing is kept for any other).
    fn cancel_key(&self, key: String) {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains(&key) {
            self.cancelled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key);
        }
    }

    /// The client is going: every wait under way ends, and says the thread's still working.
    pub fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    /// One message as it came (a line of JSON): the reply to write, if one is due.
    pub fn handle(&self, line: &str, notify: Notify) -> Option<Value> {
        match serde_json::from_str::<Value>(line) {
            Ok(msg) => self.handle_value(msg, notify),
            Err(e) => Some(error(
                Value::Null,
                PARSE_ERROR,
                &format!("That isn't JSON: {e}"),
            )),
        }
    }

    pub fn handle_value(&self, msg: Value, notify: Notify) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "Send one JSON-RPC message at a time, as an object",
            ));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response to a request of ours: there are none.
            if obj.contains_key("result") || obj.contains_key("error") {
                return None;
            }
            return Some(error(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "A request needs a method",
            ));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id.filter(|i| !i.is_null()) else {
            self.notified(method, &params);
            return None;
        };
        let key = id.to_string();
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone());
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools::definitions() })),
            "tools/call" => self.call(&id, &params, notify),
            other => Err((
                METHOD_NOT_FOUND,
                format!("Flotilla's MCP server doesn't do {other}"),
            )),
        };
        {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.remove(&key);
            self.cancelled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        }
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, msg)) => error(id, code, &msg),
        })
    }

    fn notified(&self, method: &str, params: &Value) {
        if method == "notifications/cancelled"
            && let Some(id) = params.get("requestId")
        {
            self.cancel_key(id.to_string());
        }
    }

    fn call(&self, id: &Value, params: &Value, notify: Notify) -> Result<Value, (i64, String)> {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return Err((INVALID_PARAMS, "Say which tool: params.name".into()));
        };
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !args.is_object() {
            return Err((INVALID_PARAMS, "A tool's arguments are an object".into()));
        }
        let token = params.pointer("/_meta/progressToken").cloned();
        let key = id.to_string();
        let progress = |n: u64, message: &str| {
            if let Some(token) = &token {
                notify(
                    json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": { "progressToken": token, "progress": n, "message": message } }),
                );
            }
        };
        let cancelled = || {
            self.closing.load(Ordering::SeqCst)
                || self
                    .cancelled
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&key)
        };
        let wait_cap = if token.is_none() {
            self.quiet_wait
        } else {
            None
        };
        let call = tools::Call {
            progress: &progress,
            cancelled: &cancelled,
            wait_cap,
        };
        match tools::call(self, name, &args, &call) {
            Ok(text) => {
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false }))
            }
            // What went wrong, for the model to read and act on.
            Err(tools::Failed::Tool(why)) => {
                Ok(json!({ "content": [{ "type": "text", "text": why }], "isError": true }))
            }
            Err(tools::Failed::Unknown) => Err((INVALID_PARAMS, format!("Unknown tool: {name}"))),
        }
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "flotilla", "title": "Flotilla", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Serve MCP over stdin and stdout, a JSON message a line, until stdin closes. Each request
/// runs on its own thread; what's written goes out a whole line at a time. Once stdin closes,
/// the requests under way finish (a wait ends at once) and are answered before it returns.
pub fn serve_stdio(mcp: Arc<Mcp>) -> std::io::Result<()> {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let write = {
        let out = out.clone();
        move |v: Value| {
            let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(o, "{v}");
            let _ = o.flush();
        }
    };
    let write: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(write);
    let stdin = std::io::stdin();
    let mut running: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut read = Ok(());
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                read = Err(e);
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        running.retain(|t| !t.is_finished());
        let (mcp, write) = (mcp.clone(), write.clone());
        running.push(
            std::thread::Builder::new()
                .name("flotilla-mcp-call".into())
                .spawn(move || {
                    let notify = |v: Value| write(v);
                    if let Some(reply) = mcp.handle(&line, &notify) {
                        write(reply);
                    }
                })?,
        );
    }
    mcp.close();
    for t in running {
        let _ = t.join();
    }
    read
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cancellation is kept only while its request runs: one for any other id is dropped, so a
    /// client can't make the server keep ids it never sent.
    #[test]
    fn only_a_running_request_can_be_cancelled() {
        let door = Door {
            target: flotilla_client::Target::Unix("/nonexistent/flotilla.sock".into()),
            token: None,
        };
        let mcp = Mcp::new(Link::connect(door, "test"));
        for n in 0..100 {
            let note = json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": format!("never-sent-{n}")}});
            assert_eq!(mcp.handle_value(note, &|_| {}), None);
            mcp.cancel(&json!(n));
        }
        assert!(mcp.cancelled.lock().unwrap().is_empty());
        mcp.running.lock().unwrap().insert(json!(7).to_string());
        mcp.cancel(&json!(7));
        assert!(mcp.cancelled.lock().unwrap().contains("7"));
        // A request that's answered leaves nothing behind.
        assert!(
            mcp.handle_value(
                json!({"jsonrpc": "2.0", "id": 8, "method": "ping"}),
                &|_| {}
            )
            .is_some()
        );
        assert!(!mcp.running.lock().unwrap().contains("8"));
    }
}
