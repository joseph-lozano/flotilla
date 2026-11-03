/// Public preview links (`flotilla_proto::public`): a port's link with no sign-in token, so
/// anyone who has it can open it, for an hour, a day, a week or until you make it private
/// (`thread.make_public`, `thread.make_private`). The server keeps each one and makes it private
/// when its time is up.

/// How long a link stays public (`flotilla_proto::PublicFor`).
public enum PublicFor: String, Sendable, Hashable, CaseIterable {
    case hour
    case day
    case week
    /// Until you make it private (or archive or delete the thread).
    case untilStopped = "until_stopped"

    /// As the menus list it.
    public var label: String {
        switch self {
        case .hour: "1 hour"
        case .day: "1 day"
        case .week: "1 week"
        case .untilStopped: "Until I stop it"
        }
    }

    /// After "public": "for 1 hour", or "until you stop it".
    public var words: String {
        switch self {
        case .hour: "for 1 hour"
        case .day: "for 1 day"
        case .week: "for 1 week"
        case .untilStopped: "until you stop it"
        }
    }
}

/// A port's link while anyone may open it (`flotilla_proto::PublicLink`).
public struct PublicLink: Sendable, Hashable, Identifiable {
    public var port: Int64
    /// The link, with no token.
    public var url: String
    public var sinceMs: Int64
    /// When it goes private by itself; nil: when you make it private.
    public var untilMs: Int64?

    public var id: Int64 { port }

    public init(port: Int64, url: String, sinceMs: Int64 = 0, untilMs: Int64? = nil) {
        self.port = port
        self.url = url
        self.sinceMs = sinceMs
        self.untilMs = untilMs
    }

    /// One read from the view: nil without a port and URL (nothing to show). Anything but an
    /// object is a doubt.
    init?(json j: JSON) throws(ReadError) {
        guard j.object != nil else { throw ReadError("a public link isn't an object") }
        guard let port = j.optInt("port"), let url = j.optStr("url") else { return nil }
        self.init(port: port, url: url, sinceMs: j.optInt("since_ms") ?? 0, untilMs: j.optInt("until_ms"))
    }

    /// How long it has left, in words ("58 min left"); nil when it lasts until it's stopped.
    public func left(nowMs: Int64) -> String? {
        untilMs.map { publicTimeLeft($0 - nowMs) }
    }

    /// What its row says: "Public · 58 min left", or "Public until you stop it".
    public func words(nowMs: Int64) -> String {
        left(nowMs: nowMs).map { "Public · \($0)" } ?? "Public until you stop it"
    }
}

/// `ms` left, in words, rounded up, as `flotilla_proto::public::time_left` says it.
public func publicTimeLeft(_ ms: Int64) -> String {
    let minute: Int64 = 60_000, hour = 60 * minute, day = 24 * hour
    let up = { (unit: Int64) in (ms + unit - 1) / unit }
    switch ms {
    case ...0: return "ending"
    case ..<minute: return "less than a minute left"
    case ..<hour: return "\(up(minute)) min left"
    case ...(2 * day): return "\(min(up(hour), 48)) h left"
    default:
        let n = up(day)
        return n == 1 ? "1 day left" : "\(n) days left"
    }
}

extension Request {
    /// Let anyone with the port's link open it, `for` this long (from now; again if it's public
    /// already), waking a stopped VM first. Answers `url`: the link.
    public static func makePublic(_ thread: String, port: UInt16, for duration: PublicFor) -> Request {
        Request(method: "thread.make_public", params: ["thread": .string(thread), "port": .int(Int64(port)), "for": .string(duration.rawValue)])
    }

    /// The port's link needs the sign-in token again, at once (a stopped VM: as it wakes).
    public static func makePrivate(_ thread: String, port: UInt16) -> Request {
        Request(method: "thread.make_private", params: ["thread": .string(thread), "port": .int(Int64(port))])
    }
}
