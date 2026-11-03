/// Preview: a private URL for any port in a thread's VM (`thread.preview`, which wakes a
/// stopped VM first), and the services a repo declares (`.agents/services.yml`, kept as
/// `flotilla_proto::ThreadServices` in the thread's view) with what can be done to each
/// (`services`). The rules are the desktop's `thread_view.rs`: which services Preview's menu
/// shows, a service's state in words, which link Open opens, and the end of its output.

/// A link on a service's preview (`flotilla_proto::Link`): its title and full URL.
public struct ServiceLink: Sendable, Hashable {
    public var title: String
    public var url: String

    public init(title: String, url: String) {
        self.title = title
        self.url = url
    }
}

/// One service in a thread's VM (`flotilla_proto::ServiceRun`).
public struct ServiceRun: Sendable, Hashable, Identifiable {
    public var name: String
    public var port: Int64
    /// `starting`, `ready`, `not_responding` (running, never answered), `exited`
    /// (`exitCode`), `stopped` (by you) or `failed` (couldn't start: `error`).
    public var state: String
    public var exitCode: Int64?
    public var error: String?
    /// The end of what it printed.
    public var output: String
    /// Its preview URL, when it has a portal. It carries boat's sign-in token: never logged.
    public var preview: String?
    /// Its preview links as full URLs, the primary one first.
    public var links: [ServiceLink]

    public var id: String { name }

    public init(name: String, port: Int64, state: String, exitCode: Int64? = nil, error: String? = nil, output: String = "", preview: String? = nil, links: [ServiceLink] = []) {
        self.name = name
        self.port = port
        self.state = state
        self.exitCode = exitCode
        self.error = error
        self.output = output
        self.preview = preview
        self.links = links
    }

    /// One read from the view: nil without a name (nothing to act on). Anything but an object
    /// is a doubt.
    init?(json j: JSON) throws(ReadError) {
        guard j.object != nil else { throw ReadError("a service isn't an object") }
        guard let name = j.optStr("name") else { return nil }
        self.name = name
        port = j.optInt("port") ?? 0
        state = j.optStr("state") ?? ""
        exitCode = j.optInt("exit_code")
        error = j.optStr("error")
        output = j.optStr("output") ?? ""
        preview = j.optStr("preview")
        links = try j.list("links").compactMap { l in l.optStr("url").map { ServiceLink(title: l.optStr("title") ?? "", url: $0) } }
    }

    /// Starting, ready or running without answering: Open and Stop work.
    public var live: Bool { ["starting", "ready", "not_responding"].contains(state) }

    /// It needs you: exited, failed, or running but never answering.
    public var problem: Bool { ["exited", "failed", "not_responding"].contains(state) }

    /// Its state in words: "ready · :4100", "starting…", "exited 1".
    public var words: String {
        switch state {
        case "ready": "ready · :\(port)"
        case "starting": "starting…"
        case "not_responding": "not responding on :\(port)"
        case "exited": "exited \(exitCode.map(String.init) ?? "early")"
        case "stopped": "stopped"
        default: "failed\(error.map { ": \($0)" } ?? "")"
        }
    }

    /// What Open opens first, then the rest by their titles: its links, or its preview under
    /// its own name when it has no links.
    public var openLinks: [ServiceLink] {
        if !links.isEmpty { return links }
        return preview.map { [ServiceLink(title: name, url: $0)] } ?? []
    }

    /// The last `n` lines of its output that aren't blank (`flotilla_model::setup::tail`).
    public func tail(_ n: Int = 80) -> [String] {
        let lines = output.split(whereSeparator: \.isNewline).compactMap { line -> String? in
            var kept = line
            while let last = kept.last, last.isWhitespace { kept = kept.dropLast() }
            return kept.isEmpty ? nil : String(kept)
        }
        return Array(lines.suffix(n))
    }
}

/// A thread's services as last seen (`flotilla_proto::ThreadServices`).
public struct ThreadServices: Sendable, Hashable {
    /// The VM they were started on: after a fork or a rebuild, none of them are running.
    public var sandbox: String
    /// Which file declared them.
    public var file: String?
    /// What's wrong with the file, if it doesn't check out (the services keep their last state).
    public var error: String?
    public var services: [ServiceRun]

    public init(sandbox: String, file: String? = nil, error: String? = nil, services: [ServiceRun]) {
        self.sandbox = sandbox
        self.file = file
        self.error = error
        self.services = services
    }

    init(json j: JSON) throws(ReadError) {
        guard j.object != nil else { throw ReadError("services isn't an object") }
        sandbox = j.optStr("sandbox") ?? ""
        file = j.optStr("file")
        error = j.optStr("error")
        var runs: [ServiceRun] = []
        for s in try j.list("services") {
            if let run = try ServiceRun(json: s) { runs.append(run) }
        }
        services = runs
    }

    /// What Preview's menu shows for `thread`: its services on the VM it has now, while that's
    /// up, if there are any or the file has a problem. A stopped VM gets plain Preview (which
    /// wakes it); its services carry on when it wakes.
    public static func current(_ services: ThreadServices?, thread: Thread?) -> ThreadServices? {
        guard let services, let thread, thread.state == .idle || thread.state == .running, thread.sandboxID == services.sandbox,
              !services.services.isEmpty || services.error != nil else { return nil }
        return services
    }
}

/// A port as Preview's field takes it: a whole number from 1 to 65535, spaces around it allowed.
public func previewPort(_ text: String) -> UInt16? {
    UInt16(text.trimmingCharacters(in: .whitespaces)).flatMap { $0 == 0 ? nil : $0 }
}

extension Request {
    /// A private URL for `port` in the thread's VM, woken first if it's stopped. Answers `url`.
    public static func preview(_ thread: String, port: UInt16) -> Request {
        Request(method: "thread.preview", params: ["thread": .string(thread), "port": .int(Int64(port))])
    }

    /// Start what isn't running (services you stopped too).
    public static func ensureServices(_ thread: String) -> Request {
        Request(method: "services", params: ["ensure": .string(thread)])
    }

    /// Restart a service (Start, for one you stopped).
    public static func restartService(_ thread: String, name: String) -> Request {
        Request(method: "services", params: ["restart": ["thread": .string(thread), "name": .string(name)]])
    }

    public static func stopService(_ thread: String, name: String) -> Request {
        Request(method: "services", params: ["stop": ["thread": .string(thread), "name": .string(name)]])
    }
}
