/// What the app sends its client: requests and subscriptions.
public enum ClientMsg: Sendable, Hashable {
    case req(id: ReqID, Request)
    case sub(ViewKey)
    case unsub(ViewKey)
}

/// How the client's connection to the server is.
public enum ConnectionState: Sendable, Hashable {
    /// Not connected yet, or trying again.
    case connecting
    /// Connected, and what's shown is caught up.
    case online
    /// Not connected (why the last try failed); trying again.
    case offline(String)
    /// It won't work until something changes elsewhere (the device was revoked): why.
    case refused(String)
}

/// What the client hands the app.
public enum ClientEvent: Sendable {
    case view(ServerView)
    case answer(id: ReqID, Result<Reply, ServerError>)
    case live(thread: String, partials: [RawEvent])
    case note(Note)
    case progress(id: ReqID, Note)
    case connection(ConnectionState)
}

/// The client's session with a server, without the socket: what to say in a hello, what to
/// send again after a drop, which numbered messages it already has, and the replica of its
/// views. The connection (`FlotillaClient`) feeds it every frame and writes what it says to.
/// `flotilla_client::session::SessionCore` is this, line for line.
public struct SessionCore: Sendable {
    /// This run of the client, made at random.
    public let id: String
    public private(set) var replica = Replica()
    /// Requests sent and not answered yet.
    public private(set) var inflight: [ReqID: Request] = [:]
    /// What the app sent while there was no connection: requests, and views it opened then
    /// (subscribed to as opened, after the hello, not resumed).
    public private(set) var waiting: [Up] = []
    /// What the app sent after the hello went, before the welcome came: taken as if it came
    /// once the connection was up (as Rust's client leaves it in its channel meanwhile).
    private var early: [ClientMsg] = []
    /// The last answer, progress or note (by its `n`) this session got.
    public private(set) var got: UInt64?
    /// The server refused this client: said once per reason.
    public private(set) var refused: String?
    public private(set) var phase: Phase = .down

    public enum Phase: Sendable, Hashable {
        /// No connection.
        case down
        /// The hello has gone; waiting for the welcome.
        case hello
        /// Welcomed: messages go straight out.
        case live
    }

    /// What a frame from the server came to.
    public enum Outcome: Sendable {
        /// The welcome: fail the requests whose answers won't come, then write these, in
        /// order (requests to send again, then what waited).
        case welcomed(Welcome, send: [Up], lost: [ReqID])
        /// Hand these to the app.
        case deliver([ClientEvent])
        /// A view it lost track of: ask for it whole (`resync`). Why.
        case resync(ViewKey, String)
        /// A view whose data doesn't read (a newer server's): kept, and not asked for again.
        /// Why.
        case unreadable(ViewKey, String)
        /// Already had (its `n`): nothing.
        case skipped
        /// The server refused this client and closes: why (deliver the refusal if any).
        case refuse(String, ClientEvent?)
        /// Not what the server should say now: end the connection (why).
        case fail(String)
        case nothing
    }

    public init(id: String) {
        self.id = id
    }

    /// The app's message: what to write now (nothing while there's no connection; it's kept).
    public mutating func app(_ msg: ClientMsg) -> [Up] {
        switch phase {
        case .down:
            fold(msg)
            return []
        case .hello:
            early.append(msg)
            return []
        case .live:
            return [live(msg)]
        }
    }

    private mutating func fold(_ msg: ClientMsg) {
        switch msg {
        case .req(let id, let r): waiting.append(.req(id: id, r))
        case .sub(let v):
            replica.subscribe(v)
            waiting.append(.sub(v))
        case .unsub(let v):
            replica.unsubscribe(v)
            waiting.removeAll { $0 == .sub(v) }
        }
    }

    private mutating func live(_ msg: ClientMsg) -> Up {
        switch msg {
        case .req(let id, let r):
            inflight[id] = r
            return .req(id: id, r)
        case .sub(let v):
            replica.subscribe(v)
            return .sub(v)
        case .unsub(let v):
            replica.unsubscribe(v)
            return .unsub(v)
        }
    }

    /// A new connection: the hello to send first.
    public mutating func hello(app: String, version: String, platform: String) -> Up {
        phase = .hello
        early = []
        let opened = Set(waiting.compactMap { if case .sub(let v) = $0 { v } else { nil } })
        return .hello(Hello(
            app: app, version: version, platform: platform, session: id, epoch: replica.epoch,
            subs: replica.held().filter { !opened.contains($0.view) }, got: got
        ))
    }

    /// The server's welcome: what to write, in order (requests to send again, then what
    /// waited), and the requests whose answers won't come.
    private mutating func welcomed(_ w: Welcome) -> (send: [Up], lost: [ReqID]) {
        replica.welcome(epoch: w.epoch)
        refused = nil
        var lost: [ReqID] = []
        var again: [Up] = []
        if w.resumed {
            // Answers to what it took come with what it kept; what it never got goes again.
            for id in inflight.keys.sorted() where w.seen.map({ id > $0 }) ?? true {
                again.append(.req(id: id, inflight.removeValue(forKey: id)!))
            }
        } else {
            // A new session: its numbering starts again, and answers to what was in flight
            // won't come. What has a client-made id is safe to send again; the rest fails.
            got = nil
            for id in inflight.keys.sorted() {
                let r = inflight.removeValue(forKey: id)!
                if r.resendable { again.append(.req(id: id, r)) } else { lost.append(id) }
            }
        }
        var out = again + waiting
        waiting = []
        for up in out {
            if case .req(let id, let r) = up { inflight[id] = r }
        }
        phase = .live
        for msg in early { out.append(live(msg)) }
        early = []
        return (out, lost)
    }

    /// A frame from the server.
    public mutating func received(_ down: Down) -> Outcome {
        switch phase {
        case .down:
            return .nothing
        case .hello:
            // The welcome first: whether the server still had this session.
            switch down {
            case .welcome(let w):
                let (send, lost) = welcomed(w)
                return .welcomed(w, send: send, lost: lost)
            case .error(let msg):
                return refuse(msg)
            default:
                return .fail("The server didn't say hello")
            }
        case .live:
            break
        }
        // Answers, progress and notes are numbered: one got already (sent again after a
        // drop) is skipped.
        if let n = down.n {
            if let got, n <= got { return .skipped }
            got = n
        }
        switch replica.take(down) {
        case .view(let v): return .deliver([.view(v)])
        case .pass(let d):
            switch d {
            case .res(let id, let result, _):
                inflight[id] = nil
                return .deliver([.answer(id: id, result)])
            case .live(let t, let p): return .deliver([.live(thread: t, partials: p)])
            case .note(let note, _): return .deliver([.note(note)])
            case .progress(let id, let note, _): return .deliver([.progress(id: id, note)])
            default: return .nothing
            }
        case .resync(let view, let why):
            replica.resyncing(view)
            return .resync(view, why)
        case .unreadable(let view, let why):
            return .unreadable(view, why)
        case .synced:
            return .deliver([.connection(.online)])
        case .error(let msg):
            return refuse(msg)
        case .welcome, .nothing:
            return .nothing
        }
    }

    /// The server won't take this client: say why once.
    public mutating func refuse(_ reason: String) -> Outcome {
        let first = refused != reason
        refused = reason
        return .refuse(reason, first ? .connection(.refused(reason)) : nil)
    }

    /// The connection ended. `unsent`: what was queued and not written, in order: it waits for
    /// the next connection (a request no longer counts as in flight; an unsubscribe or a
    /// resync needs no resending, since the next hello says what's held, and a subscribe goes
    /// again only if the view is still open). The one being written when it ended isn't in
    /// `unsent`: it may have gone, so a request stays in flight (the next welcome's `seen`
    /// says whether it arrived), and a subscribe is in the next hello.
    public mutating func lost(unsent: [Up]) {
        let early = self.early
        self.early = []
        phase = .down
        var back: [Up] = []
        for up in unsent {
            switch up {
            case .req(let id, let r):
                inflight[id] = nil
                back.append(.req(id: id, r))
            case .sub(let v) where replica.isSubscribed(v): back.append(up)
            case .sub, .unsub, .resync, .hello: break
            }
        }
        waiting = back + waiting
        // What the app sent while the welcome was awaited folds as if sent while away.
        for msg in early { fold(msg) }
    }
}
