import Foundation

/// The protocol's version: a server that speaks another refuses the upgrade (426).
public let protocolVersion: Int64 = 1
public let wsPath = "/v1/ws"
public let pairPath = "/v1/pair"
public let protocolHeader = "flotilla-protocol"

/// A request's id, the client's own: its answer carries it back. Only grows within a session.
public typealias ReqID = UInt64

/// What a view is of. Ordered as Rust's derived `Ord` orders `ViewKey` (by variant, then
/// key), so a hello lists the views in the same order Rust's replica does.
public enum ViewKey: Sendable, Hashable, Comparable, CustomStringConvertible {
    case sidebar
    case catalog
    case settings
    case thread(String)
    case mentions(String)
    case project(String)
    case devices
    /// What the agents spent; subscribing checks the VMs (at most every five minutes).
    case agentUsage
    /// Every scheduled prompt.
    case schedules

    /// The view's kind, as the wire names it.
    public var name: String {
        switch self {
        case .sidebar: "sidebar"
        case .catalog: "catalog"
        case .settings: "settings"
        case .thread: "thread"
        case .mentions: "mentions"
        case .project: "project"
        case .devices: "devices"
        case .agentUsage: "agent_usage"
        case .schedules: "schedules"
        }
    }

    var key: String? {
        switch self {
        case .thread(let k), .mentions(let k), .project(let k): k
        default: nil
        }
    }

    private var rank: Int {
        switch self {
        case .sidebar: 0
        case .catalog: 1
        case .settings: 2
        case .thread: 3
        case .mentions: 4
        case .project: 5
        case .devices: 6
        case .agentUsage: 7
        case .schedules: 8
        }
    }

    public static func < (a: ViewKey, b: ViewKey) -> Bool {
        if a.rank != b.rank { return a.rank < b.rank }
        return (a.key ?? "") < (b.key ?? "")
    }

    public var description: String { key.map { "\(name)(\($0))" } ?? name }

    /// The fields it adds to a message (`"view"`, and `"key"` for a view of one thing).
    var fields: [String: JSON] {
        var f: [String: JSON] = ["view": .string(name)]
        if let key { f["key"] = .string(key) }
        return f
    }

    /// From a message's `view` and `key`.
    init?(fields: JSON) {
        guard let name = fields["view"]?.string else { return nil }
        let key = fields["key"]?.string
        switch (name, key) {
        case ("sidebar", _): self = .sidebar
        case ("catalog", _): self = .catalog
        case ("settings", _): self = .settings
        case ("devices", _): self = .devices
        case ("agent_usage", _): self = .agentUsage
        case ("schedules", _): self = .schedules
        case ("thread", let k?): self = .thread(k)
        case ("mentions", let k?): self = .mentions(k)
        case ("project", let k?): self = .project(k)
        default: return nil
        }
    }
}

/// A request: a method and its parameters, as `{"m": …, "p": …}`.
public struct Request: Sendable, Hashable {
    public let method: String
    public let params: JSON?

    public init(method: String, params: JSON? = nil) {
        self.method = method
        self.params = params
    }

    /// One with a client-made id (`thread.new`'s thread id, `thread.send`'s and `thread.answer`'s
    /// `local-` id, a schedule's id): the server does it once however often it's sent, so it's
    /// safe to send again after a drop.
    public var resendable: Bool { method == "thread.new" || method == "thread.send" || method == "schedule.put" || method == "thread.answer" }

    /// Send a message (a steer while a turn runs), with its files uploaded already. `local`:
    /// the `local-<uuid>` it shows as "sending" under until boat takes it.
    public static func send(thread: String, text: String, files: [Attached] = [], local: String) -> Request {
        Request(method: "thread.send", params: ["thread": .string(thread), "text": .string(text), "files": .array(files.map(\.json)), "local": .string(local)])
    }

    /// Answer a question the thread's agent waits on (`ask`: the ask message's `toolID`): one
    /// answer per question, the options picked or what was typed. It goes as a message does,
    /// under `local`.
    public static func answer(thread: String, ask: String, answers: [[String]], local: String) -> Request {
        Request(method: "thread.answer", params: ["thread": .string(thread), "ask": .string(ask), "answers": .array(answers.map { .array($0.map { .string($0) }) }), "local": .string(local)])
    }

    public static func interrupt(_ thread: String) -> Request { Request(method: "thread.interrupt", params: .string(thread)) }
    public static func mergePR(_ thread: String) -> Request { Request(method: "pr.merge", params: .string(thread)) }
    public static func markViewed(_ thread: String) -> Request { Request(method: "thread.mark_viewed", params: .string(thread)) }
    public static func markUnread(_ thread: String) -> Request { Request(method: "thread.mark_unread", params: .string(thread)) }
    public static func archive(_ thread: String) -> Request { Request(method: "thread.archive", params: .string(thread)) }
    public static func unarchive(_ thread: String) -> Request { Request(method: "thread.unarchive", params: .string(thread)) }
    public static func rename(_ thread: String, title: String) -> Request { Request(method: "thread.rename", params: ["thread": .string(thread), "title": .string(title)]) }
    /// Delete its VM (and its disk) on boat, and its history.
    public static func delete(_ thread: String) -> Request { Request(method: "thread.delete", params: .string(thread)) }
    public static let cachedRepos = Request(method: "github.cached_repos")
}

/// A view a client subscribes to, and the revision of it it holds (none: send it whole).
public struct Held: Sendable, Hashable {
    public let view: ViewKey
    public let rev: UInt64?
}

/// Client → server.
public enum Up: Sendable, Hashable {
    case hello(Hello)
    case req(id: ReqID, Request)
    case sub(ViewKey)
    case unsub(ViewKey)
    /// A view subscribed to that the client lost track of: send it whole (without it counting
    /// as opening the thread again).
    case resync(ViewKey)

    public var json: JSON {
        switch self {
        case .hello(let h):
            var f: [String: JSON] = [
                "t": "hello", "proto": .int(h.proto), "app": .string(h.app), "version": .string(h.version), "platform": .string(h.platform),
                "session": .string(h.session), "epoch": JSON(h.epoch),
                "subs": .array(h.subs.map { held in
                    var v = held.view.fields
                    v["rev"] = held.rev.map { .int(Int64($0)) } ?? .null
                    return .object(v)
                }),
            ]
            f["got"] = h.got.map { .int(Int64($0)) } ?? .null
            return .object(f)
        case .req(let id, let r):
            var f: [String: JSON] = ["t": "req", "id": .int(Int64(id)), "m": .string(r.method)]
            if let p = r.params { f["p"] = p }
            return .object(f)
        case .sub(let v): return .object(v.fields.merging(["t": "sub"]) { a, _ in a })
        case .unsub(let v): return .object(v.fields.merging(["t": "unsub"]) { a, _ in a })
        case .resync(let v): return .object(v.fields.merging(["t": "resync"]) { a, _ in a })
        }
    }
}

public struct Hello: Sendable, Hashable {
    public var proto: Int64 = protocolVersion
    public var app: String
    public var version: String
    public var platform: String
    /// This run of the client: reconnecting with it soon after a drop brings what was sent
    /// meanwhile.
    public var session: String
    /// The server run the revisions below are from.
    public var epoch: String?
    public var subs: [Held]
    /// The last answer, progress or note (`n`) it got in this session.
    public var got: UInt64?
}

public struct ServerInfo: Sendable, Hashable {
    public let id: String
    public let version: String
}

public struct Welcome: Sendable, Hashable {
    public let proto: Int64
    public let server: ServerInfo
    /// This server run: revisions from another mean nothing here.
    public let epoch: String
    /// The session was known, with everything after `hello.got`: that follows.
    public let resumed: Bool
    /// The last request the server took from this session before it dropped.
    public let seen: ReqID?
    public let caps: [String]
    /// The paired device this connection is.
    public let device: String?
}

/// One change in a delta, on the view's JSON.
public enum Op: Sendable, Hashable {
    case set(field: String, v: JSON)
    case unset(field: String)
    /// An entry of a keyed list, new (appended) or changed (in place).
    case put(field: String, v: JSON)
    /// Entries of a keyed list gone, by key.
    case del(field: String, keys: [JSON])
    /// A keyed list's keys in their new order.
    case order(field: String, keys: [JSON])
    /// One from a newer server: skipped (the revision still moves on).
    case unknown

    init(json: JSON) {
        let field = json["field"]?.string
        switch (json["op"]?.string, field) {
        case ("set", let f?): self = .set(field: f, v: json["v"] ?? .null)
        case ("unset", let f?): self = .unset(field: f)
        case ("put", let f?): self = .put(field: f, v: json["v"] ?? .null)
        case ("del", let f?): self = .del(field: f, keys: json["keys"]?.array ?? [])
        case ("order", let f?): self = .order(field: f, keys: json["keys"]?.array ?? [])
        default: self = .unknown
        }
    }
}

/// Server → client.
public enum Down: Sendable, Hashable {
    case welcome(Welcome)
    /// A request's answer, after the views it changed.
    case res(id: ReqID, result: Result<Reply, ServerError>, n: UInt64?)
    /// A view, whole, at revision `rev`.
    case snap(ViewKey, rev: UInt64, data: JSON)
    /// A view's change from revision `base`, which the client must hold, to `rev`.
    case delta(ViewKey, base: UInt64, rev: UInt64, ops: [Op])
    /// An open thread's streaming text, whole each time. Never stored or replayed.
    case live(thread: String, partials: [RawEvent])
    case note(Note, n: UInt64?)
    /// Part of a request's answer.
    case progress(id: ReqID, note: Note, n: UInt64?)
    /// Everything the hello asked for has been sent: what the client shows is live.
    case synced
    /// The server won't go on with this client: why. It closes.
    case error(String)

    /// The number answers, progress and notes carry within a session.
    public var n: UInt64? {
        switch self {
        case .res(_, _, let n), .note(_, let n), .progress(_, _, let n): n
        default: nil
        }
    }

    /// A frame, or nil for one this client doesn't read (as Rust's client skips one that
    /// doesn't parse).
    public init?(text: String) {
        guard let json = try? JSON(parsing: text) else { return nil }
        self.init(json: json)
    }

    public init?(json: JSON) {
        func u64(_ k: String) -> UInt64? { json[k]?.int.flatMap { $0 >= 0 ? UInt64($0) : nil } }
        switch json["t"]?.string {
        case "welcome":
            guard let proto = json["proto"]?.int, let server = json["server"], let id = server["id"]?.string, let epoch = json["epoch"]?.string,
                  let resumed = json["resumed"]?.bool
            else { return nil }
            self = .welcome(Welcome(
                proto: proto, server: ServerInfo(id: id, version: server["version"]?.string ?? ""), epoch: epoch, resumed: resumed,
                seen: u64("seen"), caps: json["caps"]?.array?.compactMap(\.string) ?? [], device: json["device"]?.string
            ))
        case "res":
            guard let id = u64("id"), let result = json["result"].flatMap(Reply.result) else { return nil }
            self = .res(id: id, result: result, n: u64("n"))
        case "snap":
            guard let view = ViewKey(fields: json), let rev = u64("rev"), let data = json["data"] else { return nil }
            self = .snap(view, rev: rev, data: data)
        case "delta":
            guard let view = ViewKey(fields: json), let base = u64("base"), let rev = u64("rev"), let ops = json["ops"]?.array else { return nil }
            self = .delta(view, base: base, rev: rev, ops: ops.map(Op.init(json:)))
        case "live":
            guard let thread = json["thread"]?.string, let partials = json["partials"]?.array else { return nil }
            self = .live(thread: thread, partials: partials.compactMap { try? RawEvent(json: $0) })
        case "note":
            guard let note = json["note"].flatMap(Note.init(json:)) else { return nil }
            self = .note(note, n: u64("n"))
        case "progress":
            guard let id = u64("id"), let note = json["note"].flatMap(Note.init(json:)) else { return nil }
            self = .progress(id: id, note: note, n: u64("n"))
        case "synced":
            self = .synced
        case "error":
            self = .error(json["msg"]?.string ?? "The server refused this client")
        default:
            return nil
        }
    }
}

/// A request that failed: the server's message, for the user.
public struct ServerError: Error, Sendable, Hashable, CustomStringConvertible {
    public let message: String
    public init(_ message: String) { self.message = message }
    public var description: String { message }
}

/// A request's answer when it worked.
public enum Reply: Sendable, Hashable {
    case done
    /// Seen before (its client-made id): nothing more was done.
    case already
    case created(String)
    /// Sent as a new turn (`prompt`) or a steer of the running one, with boat's id.
    case sent(steer: Bool, id: String)
    case url(String)
    case prompt(String)
    case name(String)
    /// A pairing code, made for this device to show (`devices.pair`).
    case code(PairingCode)
    /// What a search found (`search`); hits of a kind this phone doesn't know are left out.
    case hits([SearchHit])
    /// How the server sends notifications (`push.register`): the push relay it sends through,
    /// or none (its own key, or off).
    case push(relay: String?)
    /// Any other, as it came.
    case other(JSON)

    init(json: JSON) {
        switch json {
        case .string("done"): self = .done
        case .string("already"): self = .already
        case .object(let o) where o.count == 1:
            let (k, v) = o.first!
            switch (k, v) {
            case ("created", .string(let id)): self = .created(id)
            case ("url", .string(let u)): self = .url(u)
            case ("prompt", .string(let p)): self = .prompt(p)
            case ("name", .string(let n)): self = .name(n)
            case ("hits", .array(let hits)): self = .hits(hits.compactMap(SearchHit.init(json:)))
            case ("push", .object(let p)): self = .push(relay: p["relay"]?.string)
            case ("code", let c):
                if let code = c["code"]?.string, let link = c["link"]?.string { self = .code(PairingCode(code: code, expiresMs: c["expires_ms"]?.int ?? 0, link: link)) }
                else { self = .other(json) }
            case ("sent", let s):
                if let id = s["Prompt"]?.string { self = .sent(steer: false, id: id) }
                else if let id = s["Steer"]?.string { self = .sent(steer: true, id: id) }
                else { self = .other(json) }
            default: self = .other(json)
            }
        default: self = .other(json)
        }
    }

    /// serde's `Result`: `{"Ok": …}` or `{"Err": "…"}`.
    static func result(_ json: JSON) -> Result<Reply, ServerError>? {
        if let ok = json["Ok"] { return .success(Reply(json: ok)) }
        if let err = json["Err"] { return .failure(ServerError(err.string ?? err.text)) }
        return nil
    }
}

/// What the client is told that isn't a view or an answer: its kind and data, with the
/// kinds the phone acts on read out.
public struct Note: Sendable, Hashable {
    public let kind: String
    public let data: JSON

    init?(json: JSON) {
        guard let kind = json["kind"]?.string else { return nil }
        self.kind = kind
        self.data = json["data"] ?? .null
    }

    /// `turn_finished`: a turn the engine watched running has ended.
    public var turnFinished: String? { kind == "turn_finished" ? data.string : nil }

    /// `pr_merged`: a thread's pull request was merged, and whether the server archived the
    /// thread for it (Settings → Archiving; missing: it didn't).
    public var prMerged: PRMerged? {
        guard kind == "pr_merged", let t = data["thread"]?.string, let n = data["number"]?.int else { return nil }
        return PRMerged(thread: t, number: n, title: data.optStr("title") ?? "", branch: data.optStr("branch") ?? "", archived: data.flag("archived"))
    }

    /// `idle_archived`: threads the server archived for sitting idle as long as Settings says.
    public var idleArchived: [String]? {
        guard kind == "idle_archived", let ids = data.array else { return nil }
        return ids.compactMap(\.string)
    }
}

/// A merged pull request, as `pr_merged` tells of it.
public struct PRMerged: Sendable, Hashable {
    public var thread: String
    public var number: Int64
    public var title: String
    /// What it merged into.
    public var branch: String
    /// The server archived the thread for it.
    public var archived: Bool
}
