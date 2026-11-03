//! One connection to a Flotilla server for every tool call at once: the views it holds, the
//! answers that come, and a way to wait for either. Tool calls run on threads of their own
//! (an assistant may call several at once), so this is shared and each call waits on it.
//!
//! The connection always asks the server for an agent's scope (`flotilla_proto::Scope::Agent`):
//! a device paired as an agent is one anyway, and on this machine's socket the server holds it
//! to that too. So what the tools can do is what the server lets an agent do, whatever this
//! crate asks.

use flotilla_client::{Client, Info, Target};
use flotilla_proto::{
    ClientMsg, Connection, Cut, HistoryPage, Reply, ReqId, Request, Scope, Secret, ServerMsg,
    Sidebar, ThreadDetail, View, ViewKey,
};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long to wait for a view the server should send at once.
const VIEW_PATIENCE: Duration = Duration::from_secs(30);

/// Where the server is, and how to get in.
#[derive(Clone)]
pub struct Door {
    pub target: Target,
    /// A paired device's token, over the network. Never shown.
    pub token: Option<Secret>,
}

impl std::fmt::Debug for Door {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Door")
            .field("target", &self.target)
            .field("token", &self.token.as_ref().map(|_| "…"))
            .finish()
    }
}

#[derive(Default)]
pub(crate) struct State {
    views: HashMap<ViewKey, View>,
    answers: HashMap<ReqId, Result<Reply, String>>,
    /// The server won't have this client: why.
    refused: Option<String>,
    /// Why the last connection failed, while it's down.
    offline: Option<String>,
    /// How many tool calls hold each thread's view.
    held: HashMap<ViewKey, usize>,
    /// Counts every message, so a waiter can tell something came.
    seen: u64,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// A connection to a server, shared by every tool call.
pub struct Link {
    client: Client,
    door: Door,
    shared: Arc<Shared>,
    /// The next request's id. Held while it's sent, so ids go out in order whatever thread
    /// asks: a reconnect resends what's after the last one the server saw.
    next: Mutex<ReqId>,
}

impl Link {
    /// Connect (and keep connected) as `app`, holding the threads and models.
    pub fn connect(door: Door, app: &str) -> Link {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        let into = shared.clone();
        let info = Info {
            app: app.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            token: door.token.clone(),
            scope: Some(Scope::Agent),
        };
        let client = Client::connect(door.target.clone(), info, move |m| {
            let mut s = into.state.lock().unwrap_or_else(|e| e.into_inner());
            s.seen += 1;
            match m {
                ServerMsg::Snap { view } => {
                    let key = view.key();
                    // A thread no tool call holds any more isn't kept.
                    if !matches!(key, ViewKey::Thread(_)) || s.held.contains_key(&key) {
                        s.views.insert(key, *view);
                    }
                }
                ServerMsg::Res { id, result } => {
                    s.answers.insert(id, result);
                }
                ServerMsg::Connection(Connection::Refused { reason }) => s.refused = Some(reason),
                ServerMsg::Connection(Connection::Offline { error }) => s.offline = Some(error),
                ServerMsg::Connection(Connection::Online) => s.offline = None,
                ServerMsg::Note(_) | ServerMsg::Progress { .. } | ServerMsg::Live { .. } => {}
            }
            drop(s);
            into.changed.notify_all();
            true
        });
        client.send(ClientMsg::Sub {
            view: ViewKey::Sidebar,
        });
        client.send(ClientMsg::Sub {
            view: ViewKey::Catalog,
        });
        Link {
            client,
            door,
            shared,
            next: Mutex::new(1),
        }
    }

    /// Wait up to `patience` for `f` to find what it wants in what the server sent. `what`
    /// names it, for the error when it doesn't come.
    pub(crate) fn wait<T>(
        &self,
        what: &str,
        patience: Duration,
        mut f: impl FnMut(&State) -> Option<T>,
    ) -> Result<T, String> {
        let until = Instant::now() + patience;
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(why) = &s.refused {
                return Err(why.clone());
            }
            if let Some(t) = f(&s) {
                return Ok(t);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(match &s.offline {
                    Some(e) => format!("Can't reach the Flotilla server: {e}"),
                    None => format!("The Flotilla server didn't send {what} in time"),
                });
            }
            s = self
                .shared
                .changed
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Until anything more comes from the server, or `patience` is up.
    pub fn settle(&self, patience: Duration) {
        let s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let seen = s.seen;
        let _ = self
            .shared
            .changed
            .wait_timeout_while(s, patience, |s| s.seen == seen && s.refused.is_none());
    }

    pub fn sidebar(&self) -> Result<Sidebar, String> {
        self.wait("the threads", VIEW_PATIENCE, |s| {
            match s.views.get(&ViewKey::Sidebar) {
                Some(View::Sidebar(b)) => Some(b.clone()),
                _ => None,
            }
        })
    }

    /// The model catalog, once the server has read it (None until then).
    pub fn catalog(&self) -> Result<Option<flotilla_proto::Catalog>, String> {
        self.wait("the models", VIEW_PATIENCE, |s| {
            match s.views.get(&ViewKey::Catalog) {
                Some(View::Catalog(c)) => Some(c.clone()),
                _ => None,
            }
        })
    }

    /// A thread's view, held while the [`Held`] lives (the server keeps it current meanwhile).
    pub fn hold(&self, thread: &str) -> Held<'_> {
        let key = ViewKey::Thread(thread.to_string());
        let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let n = s.held.entry(key.clone()).or_insert(0);
        *n += 1;
        if *n == 1 {
            self.client.send(ClientMsg::Sub { view: key.clone() });
        }
        Held { link: self, key }
    }

    /// Send a request; its answer within `patience`.
    pub fn ask(&self, req: Request, patience: Duration) -> Result<Reply, String> {
        let id = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            let id = *next;
            *next += 1;
            self.client.send(ClientMsg::Req { id, req });
            id
        };
        let take = || {
            let mut s = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            s.answers.remove(&id)
        };
        // Taken out under the lock as it's found, so two waiters can't both have it.
        loop {
            self.wait("an answer", patience, |s| {
                s.answers.contains_key(&id).then_some(())
            })?;
            if let Some(answer) = take() {
                return answer;
            }
        }
    }

    /// A page of a thread's older messages, over the server's HTTP.
    pub fn history(&self, thread: &str, page: i64) -> Result<HistoryPage, String> {
        flotilla_client::history(
            &self.door.target,
            self.door.token.as_ref(),
            thread,
            flotilla_proto::History::before(page),
        )
    }

    /// A string a message carries cut short, whole.
    pub fn full(&self, thread: &str, seq: i64, cut: &Cut) -> Result<String, String> {
        flotilla_client::full(
            &self.door.target,
            self.door.token.as_ref(),
            thread,
            seq,
            cut,
        )
    }
}

/// A thread's view, held for a tool call: let go when it's dropped.
pub struct Held<'a> {
    link: &'a Link,
    key: ViewKey,
}

impl Held<'_> {
    /// The thread's view as the server last sent it, waiting for the first.
    pub fn detail(&self) -> Result<ThreadDetail, String> {
        self.link.wait("the thread", VIEW_PATIENCE, |s| {
            match s.views.get(&self.key) {
                Some(View::Thread(d)) => Some((**d).clone()),
                _ => None,
            }
        })
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        let mut s = self
            .link
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let last = match s.held.get_mut(&self.key) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            _ => true,
        };
        if last {
            s.held.remove(&self.key);
            s.views.remove(&self.key);
            self.link.client.send(ClientMsg::Unsub {
                view: self.key.clone(),
            });
        }
    }
}
