/// The requests the phone makes beyond milestone 1's, as `flotilla_proto::cmd` writes them,
/// and the notes that answer them in pieces.

/// A file sent with a prompt, uploaded already (`PUT /v1/blobs`): what `flotilla_proto::Attached`
/// carries. The phone has no path the server could open, so it sends none.
public struct Attached: Sendable, Hashable {
    public var name: String
    public var size: Int64
    public var kind: FileKind
    /// `b_<sha256 hex>`.
    public var blob: String

    public init(name: String, size: Int64, kind: FileKind, blob: String) {
        self.name = name
        self.size = size
        self.kind = kind
        self.blob = blob
    }

    var json: JSON {
        ["path": "", "name": .string(name), "size": .int(size), "kind": .string(kind.rawValue), "blob": .string(blob)]
    }
}

/// An existing branch to start a project thread on, and what it merges into.
public struct OnBranch: Sendable, Hashable {
    public var name: String
    public var base: String

    public init(name: String, base: String) {
        self.name = name
        self.base = base
    }

    init?(json: JSON?) {
        guard let name = json?["name"]?.string else { return nil }
        self.init(name: name, base: json?["base"]?.string ?? "")
    }

    var json: JSON { ["name": .string(name), "base": .string(base)] }
}

/// A new thread, as `thread.new` takes it. `id` is the client's own UUID, which the thread
/// takes, so a request sent again makes one thread.
public struct NewThread: Sendable, Hashable {
    public var id: String
    /// boat environment (a project's own, for a project thread).
    public var environment: String
    public var projectID: String?
    public var provider: String
    public var model: String
    public var effort: String?
    public var fast: Bool
    public var prompt: String
    public var attachments: [Attached]
    /// None: a new branch from the default branch, named from the prompt.
    public var onBranch: OnBranch?
    /// One of several threads started from one prompt, one per model: a word for its new
    /// branch's name (its model's), so theirs differ. Left out of the request when none.
    public var branchTag: String?

    public init(id: String, environment: String, projectID: String?, provider: String, model: String, effort: String?, fast: Bool, prompt: String, attachments: [Attached] = [], onBranch: OnBranch? = nil, branchTag: String? = nil) {
        self.id = id
        self.environment = environment
        self.projectID = projectID
        self.provider = provider
        self.model = model
        self.effort = effort
        self.fast = fast
        self.prompt = prompt
        self.attachments = attachments
        self.onBranch = onBranch
        self.branchTag = branchTag
    }

    var json: JSON {
        var f: [String: JSON] = [
            "environment": .string(environment), "project_id": JSON(projectID), "provider": .string(provider), "model": .string(model), "effort": JSON(effort),
            "fast": .bool(fast), "title": .null, "machine_type": .null, "prompt": .string(prompt), "attachments": .array(attachments.map(\.json)),
            "wait_ready": false, "on_branch": onBranch?.json ?? .null, "id": .string(id), "parent": .null,
        ]
        if let branchTag { f["branch_tag"] = .string(branchTag) }
        return .object(f)
    }
}

/// One prompt started on several models from New thread: one thread each, as the desktop's
/// `new_thread.rs` starts them.
public enum FanOut {
    /// Why `count` threads can't start now, if they can't: several on one existing branch
    /// would share it, and boat starts only so many VMs (`room`, when known).
    public static func cantStart(_ count: Int, onBranch: OnBranch?, room: Int64?) -> String? {
        if count > 1 && onBranch != nil {
            return "Several models each need a new branch: pick New branch, or one model."
        }
        guard let room, Int64(count) > room else { return nil }
        if room <= 0 { return "boat won't start another VM right now. Try again in a minute." }
        let s = room == 1 ? "" : "s"
        return "boat will start \(room) more VM\(s) right now: pick \(room) model\(s) or fewer."
    }

    /// The word a thread's new branch carries (its model's name, `label`): only when several
    /// start together, in a project, each on a new branch.
    public static func branchTag(label: String, count: Int, projectID: String?, onBranch: OnBranch?) -> String? {
        count > 1 && projectID != nil && onBranch == nil ? label : nil
    }

    /// What to say when starts were refused (`failed`: each model's name and why) out of
    /// `total` started together: one model's as before; several, each named.
    public static func problem(_ failed: [(model: String, message: String)], of total: Int) -> String {
        if total == 1, let only = failed.first { return "Couldn't start the thread: \(only.message)" }
        if failed.count == total, let first = failed.first, failed.allSatisfy({ $0.message == first.message }) {
            return "Couldn't start the threads: \(first.message)"
        }
        return failed.map { "Couldn't start on \($0.model): \($0.message)" }.joined(separator: "\n")
    }

    /// The requests: one per model, each with its own id (`ids`, in the same order), the same
    /// prompt, files, project and branch.
    public static func threads(
        ids: [String], models: [ThreadSettings], catalog: Catalog?, environment: String, projectID: String?, onBranch: OnBranch?, prompt: String, attachments: [Attached]
    ) -> [NewThread] {
        zip(ids, models).map { id, s in
            let label = catalog?.modelLabel(provider: s.provider, model: s.model) ?? s.model
            return NewThread(
                id: id, environment: environment, projectID: projectID, provider: s.provider, model: s.model, effort: s.effort, fast: s.fast, prompt: prompt,
                attachments: attachments, onBranch: projectID == nil ? nil : onBranch,
                branchTag: branchTag(label: label, count: models.count, projectID: projectID, onBranch: projectID == nil ? nil : onBranch)
            )
        }
    }
}

/// A setting `settings.set` changes, as `flotilla_proto::Setting` writes it (`{"key", "value"}`).
public enum Setting: Sendable, Hashable {
    /// Archive a thread when its pull request merges.
    case archiveOnMerge(Bool)
    /// Archive threads idle for this many days (0: never); only `idleDayChoices`.
    case archiveIdleDays(Int)

    /// What Settings offers for idle days (0: Never), as `flotilla_core::tidy::IDLE_DAY_CHOICES`.
    public static let idleDayChoices = [0, 3, 7, 14, 30]

    var json: JSON {
        switch self {
        case .archiveOnMerge(let on): ["key": "archive_on_merge", "value": .bool(on)]
        case .archiveIdleDays(let d): ["key": "archive_idle_days", "value": .int(Int64(d))]
        }
    }
}

/// A one-time pairing code, for another device to type or scan.
public struct PairingCode: Sendable, Hashable, CustomStringConvertible, CustomDebugStringConvertible {
    /// The 12 characters (`Pairing.display` groups them).
    public let code: String
    public let expiresMs: Int64
    /// `flotilla://pair?u=…#<code>`.
    public let link: String

    public init(code: String, expiresMs: Int64, link: String) {
        self.code = code
        self.expiresMs = expiresMs
        self.link = link
    }

    // Never in a log.
    public var description: String { "PairingCode(expires: \(expiresMs), …)" }
    public var debugDescription: String { description }
}

extension Request {
    /// Start a thread.
    public static func newThread(_ t: NewThread) -> Request { Request(method: "thread.new", params: t.json) }

    /// A project's open pull requests and branches: `branches` comes as progress.
    public static func listBranches(_ project: String) -> Request { Request(method: "github.branches", params: .string(project)) }

    /// The prompt that asks the agent to address its PR's review: answers `.prompt`.
    public static func reviewPrompt(_ thread: String) -> Request { Request(method: "pr.review_prompt", params: .string(thread)) }

    /// New thread on this thread's branch (its VM is gone), with `prompt` filled in:
    /// `branch_checked` comes as progress.
    public static func onBranch(_ thread: String, prompt: String?) -> Request {
        Request(method: "thread.on_branch", params: ["thread": .string(thread), "prompt": JSON(prompt)])
    }

    /// A code (and link) that pairs another device: answers `.code`.
    public static let pairCode = Request(method: "devices.pair", params: ["ttl_secs": .null])
    public static func revokeDevice(_ id: String) -> Request { Request(method: "devices.revoke", params: .string(id)) }
    /// Name a paired device: what Devices lists it as.
    public static func renameDevice(_ id: String, name: String) -> Request { Request(method: "devices.rename", params: ["device": .string(id), "name": .string(name)]) }
    /// Check the server's setup again.
    public static let runDoctor = Request(method: "doctor.run")
    /// Read the account's usage from boat again: answers once it's in the sidebar.
    public static let readUsage = Request(method: "usage.read")
    /// Check every VM that's up for what its agents spent, their limits and the prices, now:
    /// answers when it's done (the `agent_usage` view has it).
    public static let readAgentUsage = Request(method: "agent_usage.read")

    /// Send this device notifications: its APNs token (hex), which APNs (`sandbox` or
    /// `production`), and the app's bundle id; and what it registered with the push relay, for
    /// a server without the app's APNs key to send through.
    public static func registerPush(token: String, env: String, topic: String, relay: RelayRegistration? = nil) -> Request {
        var p: [String: JSON] = ["token": .string(token), "env": .string(env), "topic": .string(topic)]
        if let relay {
            p["relay"] = .object(["url": .string(relay.relay), "handle": .string(relay.handle), "secret": .string(relay.secret), "key": .string(relay.key)])
        }
        return Request(method: "push.register", params: .object(p))
    }

    public static let unregisterPush = Request(method: "push.unregister")

    /// Change one of the server's settings.
    public static func set(_ s: Setting) -> Request { Request(method: "settings.set", params: s.json) }
}

/// An open pull request New thread can start on.
public struct OpenPR: Sendable, Hashable {
    public var number: Int64
    public var title: String
    public var url: String
    public var head: String
    public var base: String
    public var draft: Bool
    /// From someone's fork, which boat can't push to.
    public var fromFork: Bool

    public init(number: Int64, title: String, url: String = "", head: String, base: String, draft: Bool = false, fromFork: Bool = false) {
        self.number = number
        self.title = title
        self.url = url
        self.head = head
        self.base = base
        self.draft = draft
        self.fromFork = fromFork
    }
}

/// A project's open pull requests and branches.
public struct BranchChoices: Sendable, Hashable {
    public var prs: [OpenPR]
    public var branches: [String]

    public init(prs: [OpenPR], branches: [String]) {
        self.prs = prs
        self.branches = branches
    }

    init(json j: JSON) {
        prs = (j["prs"]?.array ?? []).compactMap { p in
            guard let n = p["number"]?.int, let head = p["head"]?.string else { return nil }
            return OpenPR(number: n, title: p.optStr("title") ?? "", url: p.optStr("url") ?? "", head: head, base: p.optStr("base") ?? "", draft: p.flag("draft"), fromFork: p.flag("from_fork"))
        }
        branches = (j["branches"]?.array ?? []).compactMap(\.string)
    }
}

extension Note {
    /// `branches`: a project's pull requests and branches, or why they couldn't be read.
    public var branches: (project: String, result: Result<BranchChoices, ServerError>)? {
        guard kind == "branches", let project = data["project"]?.string, let r = data["result"] else { return nil }
        if let ok = r["Ok"] { return (project, .success(BranchChoices(json: ok))) }
        return (project, .failure(ServerError(r["Err"]?.string ?? "GitHub didn't answer")))
    }

    /// `branch_checked`: open New thread on the branch (with the prompt), or why not.
    public var branchChecked: (project: String, branch: OnBranch, prompt: String?, result: Result<Void, ServerError>)? {
        guard kind == "branch_checked", let project = data["project"]?.string, let branch = OnBranch(json: data["branch"]), let r = data["result"] else { return nil }
        let result: Result<Void, ServerError> = r["Err"].map { .failure(ServerError($0.string ?? "")) } ?? .success(())
        return (project, branch, data["prompt"]?.string, result)
    }
}
