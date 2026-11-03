/// A client's copy of the views it subscribes to, kept up to date from the server's
/// snapshots and deltas exactly as `flotilla_proto::Replica` keeps it. It checks every
/// revision: a delta that doesn't start from what it holds, or that it can't apply, is a
/// doubt, and the view is asked for whole (`resync`). Data that doesn't read as the view (a
/// newer server's) is kept, and later deltas applied to it, but it isn't asked for again: the
/// server would only send the same. It hands the client a view only when it changed (or first
/// arrives after a subscribe).
public struct Replica: Sendable {
    /// The server run the revisions are from.
    public private(set) var epoch: String?
    private var views: [ViewKey: Copy] = [:]

    struct Copy: Sendable {
        /// The revision held and the view's data at it, or nil while a snapshot is awaited.
        var held: (rev: UInt64, data: JSON)?
        /// The view's data as last handed on; nil until the first since subscribing.
        var shown: JSON?
        /// The data held didn't read as the view (a newer server's snapshot).
        var unreadable = false
    }

    /// What a message from the server came to.
    public enum Applied: Sendable {
        /// The view changed (or arrived): show it.
        case changed(ServerView)
        /// Nothing new to show.
        case same
        /// It couldn't be applied: ask for the view whole.
        case doubt(String)
        /// A snapshot (or a delta on one) that doesn't read as the view: kept, and not asked
        /// for again.
        case unreadable(String)
    }

    /// What a message from the server means for the client.
    public enum Taken: Sendable {
        /// A view to show.
        case view(ServerView)
        /// A view the client lost track of: ask for it whole.
        case resync(ViewKey, String)
        /// A view whose data doesn't read (a newer server's): nothing to show, nothing to ask.
        case unreadable(ViewKey, String)
        case welcome(Welcome)
        case synced
        /// The server refused what was sent, and closes.
        case error(String)
        /// An answer, note, progress or live text: hand it on.
        case pass(Down)
        case nothing
    }

    public init() {}

    /// A fresh subscribe: whatever was held of it goes, and its first snapshot is handed on.
    public mutating func subscribe(_ view: ViewKey) {
        views[view] = Copy()
    }

    public mutating func unsubscribe(_ view: ViewKey) {
        views[view] = nil
    }

    public var subscribed: [ViewKey] { views.keys.sorted() }

    public func isSubscribed(_ view: ViewKey) -> Bool { views[view] != nil }

    /// What to say in a hello: every view subscribed to, with the revision held.
    public func held() -> [Held] {
        views.keys.sorted().map { Held(view: $0, rev: views[$0]!.held?.rev) }
    }

    /// The server's run: from another one, no revision held means anything.
    public mutating func welcome(epoch: String) {
        if self.epoch != epoch {
            for k in views.keys { views[k]!.held = nil }
            self.epoch = epoch
        }
    }

    /// Asked for whole again: wait for its snapshot.
    public mutating func resyncing(_ view: ViewKey) {
        views[view]?.held = nil
    }

    public mutating func snap(_ view: ViewKey, rev: UInt64, data: JSON) -> Applied {
        guard var c = views[view] else { return .same }
        views[view] = nil
        defer { views[view] = c }
        c.held = (rev, data)
        // The whole view: if it doesn't read, asking again gets the same.
        switch Self.show(view, data, &c) {
        case .failure(let u): return .unreadable(u.why)
        case .success(let a): return a
        }
    }

    public mutating func delta(_ view: ViewKey, base: UInt64, rev: UInt64, ops: [Op]) -> Applied {
        guard var c = views[view] else { return .same }
        // Waiting for a snapshot: nothing to do.
        guard case (let held, var data)? = c.held else { return .same }
        // This one is already applied.
        if rev <= held { return .same }
        if held != base {
            views[view]!.held = nil
            return .doubt("a delta from \(base) to \(rev), holding \(held)")
        }
        // Take the copy out, so its data is changed in place.
        views[view] = nil
        c.held = nil
        defer { views[view] = c }
        do {
            try Delta.apply(view, to: &data, ops)
        } catch {
            return .doubt(error.description)
        }
        c.held = (rev, data)
        // On data that read, a delta that leaves it not reading may be a mistake: asked for
        // whole. On data that didn't read already, it's the same as the snapshot.
        let was = c.unreadable
        switch Self.show(view, data, &c) {
        case .failure(let u) where was: return .unreadable(u.why)
        case .failure(let u):
            c.held = nil
            return .doubt(u.why)
        case .success(let a): return a
        }
    }

    /// `data` (what `c` now holds) as the view; a failure if it doesn't read as one.
    private static func show(_ view: ViewKey, _ data: JSON, _ c: inout Copy) -> Result<Applied, Unread> {
        let shown: ServerView
        do {
            shown = try ServerView(key: view, data: data)
        } catch {
            c.unreadable = true
            return .failure(Unread(error.description))
        }
        c.unreadable = false
        if c.shown == data { return .success(.same) }
        c.shown = data
        return .success(.changed(shown))
    }

    struct Unread: Error {
        let why: String
        init(_ why: String) { self.why = why }
    }

    /// Take a message from the server: keep the views up to date, and say what to do.
    public mutating func take(_ down: Down) -> Taken {
        let key: ViewKey
        let applied: Applied
        switch down {
        case .snap(let v, let rev, let data):
            key = v
            applied = snap(v, rev: rev, data: data)
        case .delta(let v, let base, let rev, let ops):
            key = v
            applied = delta(v, base: base, rev: rev, ops: ops)
        case .welcome(let w):
            welcome(epoch: w.epoch)
            return .welcome(w)
        case .synced: return .synced
        case .error(let msg): return .error(msg)
        case .res, .live, .note, .progress: return .pass(down)
        }
        switch applied {
        case .changed(let view): return .view(view)
        case .same: return .nothing
        case .doubt(let why): return .resync(key, why)
        case .unreadable(let why): return .unreadable(key, why)
        }
    }

    /// The view's data as held.
    public func data(_ view: ViewKey) -> JSON? { views[view]?.held?.data }

    public func rev(_ view: ViewKey) -> UInt64? { views[view]?.held?.rev }
}
