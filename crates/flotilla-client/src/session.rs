//! The client's session with a server, without the socket: what to say in a hello, what to
//! send again after a drop, which numbered messages it already has, and the replica of its
//! views. The connection (the crate's `Client`) feeds it every frame and writes what it says
//! to; the conformance recorder (flotilla-server's tests) drives it as a phone would.
//! FlotillaKit's `SessionCore` (Session.swift) is this, line for line.

use flotilla_proto::wire::{Down, Hello, Up, Welcome};
use flotilla_proto::{
    ClientMsg, Connection, PROTOCOL, Replica, ReqId, Request, ServerMsg, Taken, ViewKey,
};
use std::collections::BTreeMap;

/// The client's session with a server, as the connection keeps it.
#[derive(Debug)]
pub struct SessionCore {
    /// This run of the client, made at random.
    id: String,
    replica: Replica,
    /// Requests sent and not answered yet.
    inflight: BTreeMap<ReqId, Request>,
    /// What the app sent while there was no connection: requests, and views it opened then
    /// (subscribed to as opened, after the hello, not resumed).
    waiting: Vec<Up>,
    /// What the app sent after the hello went, before the welcome came: taken as if it came
    /// once the connection was up.
    early: Vec<ClientMsg>,
    /// The last answer, progress or note (by its `n`) this session got.
    got: Option<u64>,
    /// The server refused this client: said once per reason.
    refused: Option<String>,
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No connection.
    Down,
    /// The hello has gone; waiting for the welcome.
    Hello,
    /// Welcomed: messages go straight out.
    Live,
}

/// What a frame from the server came to.
#[derive(Debug)]
pub enum Outcome {
    /// The welcome: fail the requests whose answers won't come (`lost`), then write `send`,
    /// in order (requests to send again, then what waited).
    Welcomed {
        welcome: Welcome,
        send: Vec<Up>,
        lost: Vec<ReqId>,
    },
    /// Hand these to the app.
    Deliver(Vec<ServerMsg>),
    /// A view it lost track of: ask for it whole (`Up::Resync`). Why.
    Resync(ViewKey, String),
    /// A view whose data doesn't read (a newer server's): kept, and not asked for again. Why.
    Unreadable(ViewKey, String),
    /// Already had (its `n`): nothing.
    Skipped,
    /// The server refused this client and closes: why (deliver the refusal if any).
    Refuse(String, Option<ServerMsg>),
    /// Not what the server should say now: end the connection (why).
    Fail(String),
    Nothing,
}

impl SessionCore {
    pub fn new(id: String) -> SessionCore {
        SessionCore {
            id,
            replica: Replica::new(),
            inflight: BTreeMap::new(),
            waiting: vec![],
            early: vec![],
            got: None,
            refused: None,
            phase: Phase::Down,
        }
    }

    pub fn replica(&self) -> &Replica {
        &self.replica
    }

    pub fn inflight(&self) -> &BTreeMap<ReqId, Request> {
        &self.inflight
    }

    pub fn got(&self) -> Option<u64> {
        self.got
    }

    pub fn refused(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The app's message: what to write now (nothing while there's no connection; it's kept).
    pub fn app(&mut self, msg: ClientMsg) -> Vec<Up> {
        match self.phase {
            Phase::Down => {
                self.fold(msg);
                vec![]
            }
            Phase::Hello => {
                self.early.push(msg);
                vec![]
            }
            Phase::Live => vec![self.live(msg)],
        }
    }

    fn fold(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::Req { id, req } => self.waiting.push(Up::Req { id, req }),
            ClientMsg::Sub { view } => {
                self.replica.subscribe(view.clone());
                self.waiting.push(Up::Sub { view });
            }
            ClientMsg::Unsub { view } => {
                self.replica.unsubscribe(&view);
                self.waiting
                    .retain(|u| !matches!(u, Up::Sub { view: v } if *v == view));
            }
        }
    }

    fn live(&mut self, msg: ClientMsg) -> Up {
        match msg {
            ClientMsg::Req { id, req } => {
                self.inflight.insert(id, req.clone());
                Up::Req { id, req }
            }
            ClientMsg::Sub { view } => {
                self.replica.subscribe(view.clone());
                Up::Sub { view }
            }
            ClientMsg::Unsub { view } => {
                self.replica.unsubscribe(&view);
                Up::Unsub { view }
            }
        }
    }

    /// A new connection: the hello to send first.
    pub fn hello(&mut self, app: &str, version: &str, platform: &str) -> Up {
        self.phase = Phase::Hello;
        self.early.clear();
        let opened: Vec<&ViewKey> = self
            .waiting
            .iter()
            .filter_map(|u| {
                if let Up::Sub { view } = u {
                    Some(view)
                } else {
                    None
                }
            })
            .collect();
        Up::Hello(Hello {
            proto: PROTOCOL,
            app: app.to_string(),
            version: version.to_string(),
            platform: platform.to_string(),
            session: self.id.clone(),
            epoch: self.replica.epoch().map(str::to_string),
            subs: self
                .replica
                .held()
                .into_iter()
                .filter(|h| !opened.contains(&&h.view))
                .collect(),
            got: self.got,
            scope: None,
        })
    }

    /// The server's welcome: what to write, in order (requests to send again, then what
    /// waited), and the requests whose answers won't come.
    fn welcomed(&mut self, w: &Welcome) -> (Vec<Up>, Vec<ReqId>) {
        self.replica.welcome(&w.epoch);
        self.refused = None;
        let mut lost = vec![];
        let mut again = vec![];
        if w.resumed {
            // Answers to what it took come with what it kept; what it never got goes again.
            for id in self
                .inflight
                .keys()
                .copied()
                .filter(|id| w.seen.is_none_or(|seen| *id > seen))
                .collect::<Vec<_>>()
            {
                again.push(Up::Req {
                    id,
                    req: self.inflight.remove(&id).expect("in flight"),
                });
            }
        } else {
            // A new session: its numbering starts again, and answers to what was in flight
            // won't come. What has a client-made id is safe to send again; the rest fails.
            self.got = None;
            for (id, req) in std::mem::take(&mut self.inflight) {
                if req.resendable() {
                    again.push(Up::Req { id, req })
                } else {
                    lost.push(id)
                }
            }
        }
        let mut out = again;
        out.append(&mut self.waiting);
        for up in &out {
            if let Up::Req { id, req } = up {
                self.inflight.insert(*id, req.clone());
            }
        }
        self.phase = Phase::Live;
        for msg in std::mem::take(&mut self.early) {
            let up = self.live(msg);
            out.push(up);
        }
        (out, lost)
    }

    /// A frame from the server.
    pub fn received(&mut self, down: Down) -> Outcome {
        match self.phase {
            Phase::Down => return Outcome::Nothing,
            // The welcome first: whether the server still had this session.
            Phase::Hello => {
                return match down {
                    Down::Welcome(w) => {
                        let (send, lost) = self.welcomed(&w);
                        Outcome::Welcomed {
                            welcome: w,
                            send,
                            lost,
                        }
                    }
                    Down::Error { msg } => self.refuse(msg),
                    _ => Outcome::Fail("The server didn't say hello".into()),
                };
            }
            Phase::Live => {}
        }
        // Answers, progress and notes are numbered: one got already (sent again after a
        // drop) is skipped.
        if let Some(n) = down.n() {
            if self.got.is_some_and(|got| n <= got) {
                return Outcome::Skipped;
            }
            self.got = Some(n);
        }
        match self.replica.take(down) {
            Taken::View(view) => Outcome::Deliver(vec![ServerMsg::Snap { view }]),
            Taken::Pass(d) => match d {
                Down::Res { id, result, .. } => {
                    self.inflight.remove(&id);
                    Outcome::Deliver(vec![ServerMsg::Res { id, result }])
                }
                Down::Live { thread, partials } => {
                    Outcome::Deliver(vec![ServerMsg::Live { thread, partials }])
                }
                Down::Note { note, .. } => Outcome::Deliver(vec![ServerMsg::Note(note)]),
                Down::Progress { id, note, .. } => {
                    Outcome::Deliver(vec![ServerMsg::Progress { id, note }])
                }
                _ => Outcome::Nothing,
            },
            Taken::Resync(view, why) => {
                self.replica.resyncing(&view);
                Outcome::Resync(view, why)
            }
            Taken::Unreadable(view, why) => Outcome::Unreadable(view, why),
            Taken::Synced => Outcome::Deliver(vec![ServerMsg::Connection(Connection::Online)]),
            Taken::Error(msg) => self.refuse(msg),
            Taken::Welcome(_) | Taken::Nothing => Outcome::Nothing,
        }
    }

    /// The server won't take this client: say why once.
    pub fn refuse(&mut self, reason: String) -> Outcome {
        let first = self.refused.as_deref() != Some(reason.as_str());
        self.refused = Some(reason.clone());
        let event = first.then(|| {
            ServerMsg::Connection(Connection::Refused {
                reason: reason.clone(),
            })
        });
        Outcome::Refuse(reason, event)
    }

    /// The connection ended. `unsent`: what was queued and not written, in order: it waits for
    /// the next connection (a request no longer counts as in flight; an unsubscribe or a
    /// resync needs no resending, since the next hello says what's held, and a subscribe goes
    /// again only if the view is still open). The one being written when it ended isn't in
    /// `unsent`: it may have gone, so a request stays in flight (the next welcome's `seen`
    /// says whether it arrived), and a subscribe is in the next hello.
    pub fn lost(&mut self, unsent: Vec<Up>) {
        let early = std::mem::take(&mut self.early);
        self.phase = Phase::Down;
        let mut back = vec![];
        for up in unsent {
            match up {
                Up::Req { id, req } => {
                    self.inflight.remove(&id);
                    back.push(Up::Req { id, req });
                }
                Up::Sub { view } if self.replica.is_subscribed(&view) => {
                    back.push(Up::Sub { view })
                }
                Up::Sub { .. } | Up::Unsub { .. } | Up::Resync { .. } | Up::Hello(_) => {}
            }
        }
        back.append(&mut self.waiting);
        self.waiting = back;
        // What the app sent while the welcome was awaited folds as if sent while away.
        for msg in early {
            self.fold(msg);
        }
    }
}

/// FlotillaKit's `SessionRules` (UnitTests.swift), one for one.
#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_proto::wire::{Held, ServerInfo};
    use flotilla_proto::{Note, Reply};
    use serde_json::json;

    fn welcome(resumed: bool, seen: Option<ReqId>, epoch: &str) -> Welcome {
        Welcome {
            proto: 1,
            server: ServerInfo {
                id: "s".into(),
                version: "0".into(),
            },
            epoch: epoch.into(),
            resumed,
            seen,
            caps: vec![],
            device: Some("d".into()),
            adopted: vec![],
        }
    }

    /// The welcome, taken as every frame is.
    fn welcomed(core: &mut SessionCore, w: Welcome) -> (Vec<Up>, Vec<ReqId>) {
        match core.received(Down::Welcome(w)) {
            Outcome::Welcomed { send, lost, .. } => (send, lost),
            other => panic!("not welcomed: {other:?}"),
        }
    }

    fn live(core: &mut SessionCore) {
        core.hello("a", "0", "ios");
        welcomed(core, welcome(false, None, "e1"));
    }

    fn hello(core: &mut SessionCore) -> Hello {
        match core.hello("a", "0", "ios") {
            Up::Hello(h) => h,
            other => panic!("not a hello: {other:?}"),
        }
    }

    fn req(id: ReqId, req: Request) -> ClientMsg {
        ClientMsg::Req { id, req }
    }

    fn res(id: ReqId, n: u64) -> Down {
        Down::Res {
            id,
            result: Ok(Reply::Done),
            n: Some(n),
        }
    }

    fn note(n: u64) -> Down {
        Down::Note {
            note: serde_json::from_value::<Note>(json!({"kind": "turn_finished", "data": "t"}))
                .unwrap(),
            n: Some(n),
        }
    }

    fn sub(v: ViewKey) -> ClientMsg {
        ClientMsg::Sub { view: v }
    }

    fn unsub(v: ViewKey) -> ClientMsg {
        ClientMsg::Unsub { view: v }
    }

    fn t(id: &str) -> ViewKey {
        ViewKey::Thread(id.into())
    }

    fn send() -> Request {
        Request::Send {
            thread: "t".into(),
            text: "hi".into(),
            files: vec![],
            local: "local-1".into(),
        }
    }

    #[test]
    fn a_resumed_session_sends_again_only_what_the_server_didnt_see() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        core.app(req(1, Request::MarkViewed("t".into())));
        core.app(req(2, Request::Archive("t".into())));
        core.app(req(3, Request::MarkUnread("t".into())));
        core.lost(vec![]);
        core.app(req(4, Request::Unarchive("t".into())));
        core.hello("a", "0", "ios");
        let (ups, lost) = welcomed(&mut core, welcome(true, Some(2), "e1"));
        assert!(lost.is_empty());
        assert_eq!(
            ups,
            vec![
                Up::Req {
                    id: 3,
                    req: Request::MarkUnread("t".into())
                },
                Up::Req {
                    id: 4,
                    req: Request::Unarchive("t".into())
                }
            ]
        );
        assert_eq!(
            core.inflight.keys().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn a_fresh_session_sends_again_whats_safe_and_fails_the_rest() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        core.app(req(1, send()));
        core.app(req(2, Request::Archive("t".into())));
        assert!(matches!(core.received(res(9, 5)), Outcome::Deliver(_)));
        assert_eq!(core.got, Some(5));
        core.lost(vec![]);
        core.hello("a", "0", "ios");
        let (ups, lost) = welcomed(&mut core, welcome(false, None, "e2"));
        assert_eq!(ups, vec![Up::Req { id: 1, req: send() }]);
        assert_eq!(lost, vec![2]);
        // Its numbering starts again.
        assert_eq!(core.got, None);
        assert!(matches!(core.received(note(1)), Outcome::Deliver(_)));
    }

    #[test]
    fn numbered_messages_already_had_are_skipped() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        assert!(matches!(core.received(res(1, 3)), Outcome::Deliver(_)));
        assert!(matches!(core.received(res(1, 3)), Outcome::Skipped));
        assert!(matches!(core.received(note(2)), Outcome::Skipped));
    }

    #[test]
    fn views_opened_while_away_are_subscribed_after_the_hello_not_held_in_it() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        core.app(sub(ViewKey::Sidebar));
        core.received(Down::Snap {
            view: ViewKey::Sidebar,
            rev: 4,
            data: json!({"threads": []}),
        });
        core.lost(vec![]);
        core.app(sub(t("t1")));
        core.app(sub(t("t2")));
        core.app(unsub(t("t2")));
        let h = hello(&mut core);
        assert_eq!(
            h.subs,
            vec![Held {
                view: ViewKey::Sidebar,
                rev: Some(4)
            }]
        );
        assert_eq!(h.epoch.as_deref(), Some("e1"));
        let (ups, _) = welcomed(&mut core, welcome(true, None, "e1"));
        assert_eq!(ups, vec![Up::Sub { view: t("t1") }]);
    }

    #[test]
    fn what_the_app_sends_before_the_welcome_goes_after_it_unsubscribes_too() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        core.app(sub(t("t1")));
        core.lost(vec![]);
        // The hello holds t1 (its snapshot hadn't come), so the server subscribes it again...
        let h = hello(&mut core);
        assert_eq!(
            h.subs,
            vec![Held {
                view: t("t1"),
                rev: None
            }]
        );
        // ...and closing it before the welcome must still reach the server, after the welcome.
        assert!(core.app(unsub(t("t1"))).is_empty());
        assert!(core.app(req(7, Request::CachedRepos)).is_empty());
        let (ups, _) = welcomed(&mut core, welcome(true, None, "e1"));
        assert_eq!(
            ups,
            vec![
                Up::Unsub { view: t("t1") },
                Up::Req {
                    id: 7,
                    req: Request::CachedRepos
                }
            ]
        );
        assert!(!core.replica.is_subscribed(&t("t1")));
        assert!(core.inflight.contains_key(&7));
    }

    #[test]
    fn what_the_app_sent_before_a_welcome_that_never_came_waits_as_if_sent_while_away() {
        let mut core = SessionCore::new("s1".into());
        core.hello("a", "0", "ios");
        core.app(sub(ViewKey::Sidebar));
        core.app(req(1, Request::CachedRepos));
        core.lost(vec![]);
        assert_eq!(
            core.waiting,
            vec![
                Up::Sub {
                    view: ViewKey::Sidebar
                },
                Up::Req {
                    id: 1,
                    req: Request::CachedRepos
                }
            ]
        );
        assert!(core.replica.is_subscribed(&ViewKey::Sidebar));
    }

    #[test]
    fn what_wasnt_written_waits_in_order_an_unsubscribe_and_a_resync_dont() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        core.app(req(1, Request::Archive("t".into())));
        core.app(sub(t("x")));
        core.lost(vec![
            Up::Req {
                id: 1,
                req: Request::Archive("t".into()),
            },
            Up::Unsub {
                view: ViewKey::Sidebar,
            },
            Up::Resync {
                view: ViewKey::Catalog,
            },
            Up::Sub { view: t("x") },
        ]);
        assert!(core.inflight.is_empty());
        assert_eq!(
            core.waiting,
            vec![
                Up::Req {
                    id: 1,
                    req: Request::Archive("t".into())
                },
                Up::Sub { view: t("x") }
            ]
        );
    }

    #[test]
    fn a_view_opened_and_closed_while_the_socket_died_isnt_opened_again() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        let mut ups = core.app(sub(t("t1")));
        ups.extend(core.app(unsub(t("t1"))));
        // Neither was written before the socket went.
        core.lost(ups);
        assert!(core.waiting.is_empty());
        assert!(hello(&mut core).subs.is_empty());
    }

    #[test]
    fn the_request_being_written_when_the_socket_died_is_left_to_seen() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        core.app(req(1, Request::Archive("t".into())));
        let queued = core.app(req(2, Request::MarkUnread("t".into())));
        // 1 was being written (it may have arrived); 2, queued behind it, never was.
        core.lost(queued);
        assert_eq!(core.inflight.keys().copied().collect::<Vec<_>>(), vec![1]);
        assert_eq!(
            core.waiting,
            vec![Up::Req {
                id: 2,
                req: Request::MarkUnread("t".into())
            }]
        );
        core.hello("a", "0", "ios");
        // It arrived: it isn't sent again.
        let (ups, lost) = welcomed(&mut core, welcome(true, Some(1), "e1"));
        assert_eq!(
            (ups, lost),
            (
                vec![Up::Req {
                    id: 2,
                    req: Request::MarkUnread("t".into())
                }],
                vec![]
            )
        );
        core.received(res(1, 1));
        core.received(res(2, 2));
        assert!(core.inflight.is_empty());
        // A new session: whether it arrived can't be known, so it isn't run twice; it's lost.
        core.app(req(3, Request::Archive("t".into())));
        core.lost(vec![]);
        core.hello("a", "0", "ios");
        assert_eq!(
            welcomed(&mut core, welcome(false, None, "e2")),
            (vec![], vec![3])
        );
    }

    #[test]
    fn a_doubt_asks_for_the_view_whole_and_data_that_doesnt_read_doesnt() {
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        core.app(sub(ViewKey::Catalog));
        assert!(matches!(
            core.received(Down::Snap {
                view: ViewKey::Catalog,
                rev: 4,
                data: json!({"providers": []})
            }),
            Outcome::Deliver(_)
        ));
        assert!(matches!(
            core.received(Down::Delta {
                view: ViewKey::Catalog,
                base: 5,
                rev: 6,
                ops: vec![]
            }),
            Outcome::Resync(ViewKey::Catalog, _)
        ));
        assert_eq!(core.replica.rev(&ViewKey::Catalog), None);
        assert!(matches!(
            core.received(Down::Snap {
                view: ViewKey::Catalog,
                rev: 7,
                data: json!({"providers": "not a list"})
            }),
            Outcome::Unreadable(ViewKey::Catalog, _)
        ));
        // Kept, with its revision, and not asked for again.
        assert_eq!(core.replica.rev(&ViewKey::Catalog), Some(7));
    }

    #[test]
    fn a_refusal_is_said_once() {
        let mut core = SessionCore::new("s1".into());
        assert!(matches!(
            core.refuse("revoked".into()),
            Outcome::Refuse(_, Some(_))
        ));
        assert!(matches!(
            core.refuse("revoked".into()),
            Outcome::Refuse(_, None)
        ));
    }

    #[test]
    fn before_the_welcome_only_a_welcome_or_an_error_will_do() {
        let mut core = SessionCore::new("s1".into());
        assert!(matches!(core.received(Down::Synced), Outcome::Nothing));
        core.hello("a", "0", "ios");
        assert!(matches!(core.received(res(1, 1)), Outcome::Fail(_)));
        // Not taken as had.
        assert_eq!(core.got, None);
        assert!(matches!(core.received(Down::Synced), Outcome::Fail(_)));
        assert_eq!(core.phase, Phase::Hello);
        assert!(matches!(
            core.received(Down::Welcome(welcome(false, None, "e1"))),
            Outcome::Welcomed { .. }
        ));
        assert_eq!(core.phase, Phase::Live);
        // A welcome once welcomed is nothing to the session.
        assert!(matches!(
            core.received(Down::Welcome(welcome(true, None, "e1"))),
            Outcome::Nothing
        ));
    }

    #[test]
    fn an_error_after_the_welcome_is_said_once_and_again_after_the_next_welcome() {
        let refused = |reason: &str| {
            Some(ServerMsg::Connection(Connection::Refused {
                reason: reason.into(),
            }))
        };
        let mut core = SessionCore::new("s1".into());
        live(&mut core);
        assert!(
            matches!(core.received(Down::Error { msg: "revoked".into() }), Outcome::Refuse(why, event) if why == "revoked" && event == refused("revoked"))
        );
        core.lost(vec![]);
        core.hello("a", "0", "ios");
        assert!(
            matches!(core.received(Down::Error { msg: "revoked".into() }), Outcome::Refuse(why, None) if why == "revoked")
        );
        core.lost(vec![]);
        live(&mut core);
        assert_eq!(core.refused, None);
        assert!(
            matches!(core.received(Down::Error { msg: "revoked".into() }), Outcome::Refuse(why, event) if why == "revoked" && event == refused("revoked"))
        );
    }
}
