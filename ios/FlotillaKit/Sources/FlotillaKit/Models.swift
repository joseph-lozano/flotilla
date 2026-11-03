/// The data the views carry, as `flotilla_proto` defines it: only what the phone shows.
/// Read leniently: a field the server leaves out (or a newer server adds) is fine. What makes
/// a view unreadable, and so a doubt to resync, is its shape: an object that isn't one, a list
/// that isn't, or a thread without its id.

public struct ReadError: Error, CustomStringConvertible, Sendable {
    public let description: String
    init(_ description: String) { self.description = description }
}

extension JSON {
    func req(_ key: String) throws(ReadError) -> JSON {
        guard let v = self[key], !v.isNull else { throw ReadError("\(key) is missing") }
        return v
    }

    func str(_ key: String) throws(ReadError) -> String {
        guard let s = self[key]?.string else { throw ReadError("\(key) isn't text") }
        return s
    }

    func optStr(_ key: String) -> String? { self[key]?.string }
    func optInt(_ key: String) -> Int64? { self[key]?.int }
    func flag(_ key: String) -> Bool { self[key]?.bool ?? false }

    /// A list (missing or null reads as empty; anything else isn't one).
    func list(_ key: String) throws(ReadError) -> [JSON] {
        switch self[key] {
        case nil, .null?: return []
        case .array(let a)?: return a
        default: throw ReadError("\(key) isn't a list")
        }
    }

    /// A `(String, String)` tuple, as serde writes it: a two-item list.
    func pair(_ key: String) -> (String, String)? {
        guard let a = self[key]?.array, a.count == 2, let x = a[0].string, let y = a[1].string else { return nil }
        return (x, y)
    }

    /// `(i64, i64)`.
    func intPair(_ key: String) -> (Int64, Int64)? {
        guard let a = self[key]?.array, a.count == 2, let x = a[0].int, let y = a[1].int else { return nil }
        return (x, y)
    }
}

public enum ThreadState: String, Sendable, Hashable {
    case creating, starting, idle, running, stopping, stopped, expired, error
}

public struct Thread: Sendable, Hashable {
    public var id: String
    public var title: String
    public var sandboxID: String?
    public var conversationID: String?
    public var provider: String
    public var model: String
    public var effort: String?
    public var fast: Bool
    public var state: ThreadState
    public var keepRunning: Bool
    /// When an idle VM stops (RFC 3339).
    public var archiveAfter: String?
    public var lastStoppedAt: String?
    public var createdAt: String
    public var repoProjectID: String?
    public var branch: String?

    init(json j: JSON) throws(ReadError) {
        id = try j.str("id")
        title = j.optStr("title") ?? ""
        sandboxID = j.optStr("sandbox_id")
        conversationID = j.optStr("conversation_id")
        provider = j.optStr("provider") ?? ""
        model = j.optStr("model") ?? ""
        effort = j.optStr("effort")
        fast = j.flag("fast")
        state = j.optStr("state").flatMap(ThreadState.init(rawValue:)) ?? .error
        keepRunning = j.flag("keep_running")
        archiveAfter = j.optStr("archive_after")
        lastStoppedAt = j.optStr("last_stopped_at")
        createdAt = j.optStr("created_at") ?? ""
        repoProjectID = j.optStr("repo_project_id")
        branch = j.optStr("branch")
    }

    /// No VM to prompt: it expired, or the start failed before one was made.
    public var vmGone: Bool { state == .expired || (state == .error && sandboxID == nil) }
}

/// Sidebar grouping: what the thread needs from the user right now.
public enum Activity: String, Sendable, Hashable {
    case unread, idle, working, archived, expired
}

public struct Check: Sendable, Hashable {
    public var name: String
    /// `pass`, `fail` or `pending`.
    public var state: String
    public var url: String?
    /// The check run's title or the status's description ("2 tests failed").
    public var description: String?

    public init(name: String, state: String, url: String? = nil, description: String? = nil) {
        self.name = name
        self.state = state
        self.url = url
        self.description = description
    }
}

public struct PullRequest: Sendable, Hashable {
    public var number: Int64
    public var url: String
    public var title: String
    public var head: String
    public var base: String
    /// `OPEN`, `CLOSED` or `MERGED`.
    public var state: String
    public var isDraft: Bool
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    public var mergeable: String?
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or none.
    public var review: String?
    public var additions: Int64
    public var deletions: Int64
    public var checks: [Check]

    public init(number: Int64, url: String, title: String, head: String, base: String, state: String, isDraft: Bool, mergeable: String?, review: String?, additions: Int64, deletions: Int64, checks: [Check]) {
        self.number = number
        self.url = url
        self.title = title
        self.head = head
        self.base = base
        self.state = state
        self.isDraft = isDraft
        self.mergeable = mergeable
        self.review = review
        self.additions = additions
        self.deletions = deletions
        self.checks = checks
    }

    init(json j: JSON) throws(ReadError) {
        number = j.optInt("number") ?? 0
        url = j.optStr("url") ?? ""
        title = j.optStr("title") ?? ""
        head = j.optStr("head") ?? ""
        base = j.optStr("base") ?? ""
        state = j.optStr("state") ?? ""
        isDraft = j.flag("is_draft")
        mergeable = j.optStr("mergeable")
        review = j.optStr("review")
        additions = j.optInt("additions") ?? 0
        deletions = j.optInt("deletions") ?? 0
        checks = try j.list("checks").map { Check(name: $0.optStr("name") ?? "", state: $0.optStr("state") ?? "", url: $0.optStr("url"), description: $0.optStr("description")) }
    }

    public var failing: Int { checks.count { $0.state == "fail" } }
    public var pending: Int { checks.count { $0.state == "pending" } }
    public var passing: Int { checks.count { $0.state == "pass" } }
    public var open: Bool { state == "OPEN" }
    public var conflicting: Bool { mergeable == "CONFLICTING" }

    /// Open, not a draft, no conflicts, no failing or pending checks, and no review owed.
    public var canMerge: Bool {
        open && !isDraft && mergeable == "MERGEABLE" && failing == 0 && pending == 0 && review != "REVIEW_REQUIRED" && review != "CHANGES_REQUESTED"
    }
}

/// A thread's row in the sidebar.
public struct ThreadSummary: Sendable, Hashable, Identifiable {
    public var thread: Thread
    public var activity: Activity
    /// Newest event (ms), else when the thread was created.
    public var lastActivityMs: Int64
    /// When the running turn started (ms), while one runs.
    public var turnStartedMs: Int64?
    /// The model-written summary of the agent's latest reply, else its first line.
    public var lastReply: String?
    /// The latest prompt or steer, on one line.
    public var lastPrompt: String?
    public var pr: PullRequest?
    public var startFailed: Bool
    /// When you last wrote in it (ms): a prompt or steer of yours, not one Flotilla brought.
    public var promptedMs: Int64?
    /// How often and how recently it was opened, and what for from a search.
    public var opens: Opens?

    public var id: String { thread.id }

    init(json j: JSON) throws(ReadError) {
        thread = try Thread(json: j.req("thread"))
        activity = j.optStr("activity").flatMap(Activity.init(rawValue:)) ?? .idle
        lastActivityMs = j.optInt("last_activity_ms") ?? 0
        turnStartedMs = j.optInt("turn_started_ms")
        lastReply = j.optStr("last_reply")
        lastPrompt = j.optStr("last_prompt")
        if let pr = j["pr"]?.array, let first = pr.first { self.pr = try PullRequest(json: first) }
        startFailed = j.flag("start_failed")
        promptedMs = j.optInt("prompted_ms")
        opens = Opens(json: j["opens"])
    }
}

/// A count against its limit.
public struct Count: Sendable, Hashable {
    public var used: Int64
    public var limit: Int64

    public init(_ used: Int64, of limit: Int64) {
        self.used = used
        self.limit = limit
    }
}

/// The account's usage against its plan's limits, as boat last gave it.
public struct Usage: Sendable, Hashable {
    /// VMs running, of how many may run at once.
    public var vms: Count?
    /// VM starts in the last minute, hour and 24 hours. None when starts are unlimited.
    public var minute: Count?
    public var hour: Count?
    public var day: Count?
    /// The plan's VM hours left this period, of how many it includes: `used` holds what's
    /// left, as Rust's `(left, of)` pair does.
    public var vmHours: Count?
    /// When the plan's VM hours renew (RFC 3339).
    public var renews: String?
    /// Why boat won't start a VM right now.
    public var blocked: String?
    /// While a start window is full: when a VM can start again (RFC 3339), if boat says.
    public var startsFree: String?
    public var extraStarts: ExtraStarts?
    /// When boat gave these numbers (ms since the epoch).
    public var readMs: Int64?

    public init(
        vms: Count? = nil, minute: Count? = nil, hour: Count? = nil, day: Count? = nil, vmHours: Count? = nil, renews: String? = nil,
        blocked: String? = nil, startsFree: String? = nil, extraStarts: ExtraStarts? = nil, readMs: Int64? = nil
    ) {
        self.vms = vms
        self.minute = minute
        self.hour = hour
        self.day = day
        self.vmHours = vmHours
        self.renews = renews
        self.blocked = blocked
        self.startsFree = startsFree
        self.extraStarts = extraStarts
        self.readMs = readMs
    }

    init(json j: JSON) {
        func count(_ key: String) -> Count? { j.intPair(key).map { Count($0.0, of: $0.1) } }
        vms = count("vms")
        minute = count("minute")
        hour = count("hour")
        day = count("day")
        vmHours = count("vm_hours")
        renews = j.optStr("renews")
        blocked = j.optStr("blocked")
        startsFree = j.optStr("starts_free")
        extraStarts = j["extra_starts"].flatMap { $0.isNull ? nil : ExtraStarts(json: $0) }
        readMs = j.optInt("read_ms")
    }

    /// How many more VMs boat will start now (`new_thread::start_room`): the least room left
    /// in the VMs it lets run at once and, unless extra starts carry on past them, its starts
    /// per minute, hour and day. None when it hasn't said.
    public var startRoom: Int64? {
        let past = extraStarts.map { $0.on && !$0.capReached } ?? false
        return ((past ? [] : [minute, hour, day]) + [vms]).compactMap { $0.map { max($0.limit - $0.used, 0) } }.min()
    }
}

/// boat's extra starts (past the plan's limits, billed by boat): on or off, and this billing
/// month's so far.
public struct ExtraStarts: Sendable, Hashable {
    public var on: Bool
    public var used: Int64
    /// What they've cost this month, of the monthly cap, in cents.
    public var spendCents: Count?
    /// The cap is reached: no more extra starts this month.
    public var capReached: Bool

    public init(on: Bool, used: Int64 = 0, spendCents: Count? = nil, capReached: Bool = false) {
        self.on = on
        self.used = used
        self.spendCents = spendCents
        self.capReached = capReached
    }

    init(json j: JSON) {
        on = j.flag("on")
        used = j.optInt("used") ?? 0
        spendCents = j.intPair("spend_cents").map { Count($0.0, of: $0.1) }
        capReached = j.flag("cap_reached")
    }
}

public struct Project: Sendable, Hashable {
    public var id: String
    public var owner: String
    public var repo: String
    public var defaultBranch: String
    /// The project's own boat environment, which its threads start in.
    public var environmentName: String

    public init(id: String, owner: String, repo: String, defaultBranch: String = "main", environmentName: String = "") {
        self.id = id
        self.owner = owner
        self.repo = repo
        self.defaultBranch = defaultBranch
        self.environmentName = environmentName
    }

    init(json j: JSON) throws(ReadError) {
        id = try j.str("id")
        owner = j.optStr("owner") ?? ""
        repo = j.optStr("repo") ?? ""
        defaultBranch = j.optStr("default_branch") ?? "main"
        environmentName = j.optStr("environment_name") ?? ""
    }
}

/// A boat environment a thread with No project can start in.
public struct BoatEnvironment: Sendable, Hashable {
    public var id: String
    public var name: String
    public var isDefault: Bool

    init(json j: JSON) {
        id = j.optStr("id") ?? ""
        name = j.optStr("name") ?? ""
        isDefault = j.flag("is_default")
    }
}

/// The sidebar view.
public struct Sidebar: Sendable, Hashable {
    /// Every thread, newest activity first.
    public var threads: [ThreadSummary]
    /// Threads handed their task by another: child id → parent id.
    public var parents: [String: String]
    /// Threads waiting on an answer from a thread they handed work to.
    public var waiting: [String]
    public var projects: [Project]
    public var environments: [BoatEnvironment]
    public var usage: Usage?
    /// Why syncing with boat failed, if it did.
    public var syncError: String?
    /// `running`, `offline`, or why the engine stopped.
    public var engineStopped: String?
    /// The server's engine runs (it can read boat).
    public var engineRunning: Bool
    /// The agent accounts' usage limits (Claude's, ChatGPT's), as last read in a VM.
    public var limits: [AccountLimits]

    init(json j: JSON) throws(ReadError) {
        guard j.object != nil else { throw ReadError("the sidebar isn't an object") }
        threads = try j.list("threads").map { (row: JSON) throws(ReadError) in try ThreadSummary(json: row) }
        parents = (j["parents"]?.object ?? [:]).compactMapValues(\.string)
        waiting = try j.list("waiting").compactMap(\.string)
        projects = try j.list("projects").map { (p: JSON) throws(ReadError) in try Project(json: p) }
        environments = try j.list("environments").map(BoatEnvironment.init(json:))
        usage = j["usage"].flatMap { $0.isNull ? nil : Usage(json: $0) }
        syncError = j["status"]?.optStr("error")
        engineStopped = j["engine"]?["stopped"]?.string
        engineRunning = j["engine"]?.string == "running"
        limits = try j.list("limits").map { (l: JSON) throws(ReadError) in try AccountLimits(json: l) }
    }
}

/// One row of a thread: a prompt, steer, reply, tool call or notice. Ordered by `seq`.
public struct Message: Sendable, Hashable, Identifiable {
    public var seq: Int64
    public var taskID: String?
    /// `prompt`, `steer`, `text`, `tool` or `notice`.
    public var kind: String
    public var text: String?
    /// Prompt status, steer mode, the notice's event type, or how a tool's turn ended.
    public var status: String?
    public var model: String?
    public var toolName: String?
    public var toolID: String?
    public var toolSummary: String?
    public var toolInputJSON: String?
    public var toolResultJSON: String?
    public var toolResultType: String?
    public var isError: Bool
    public var pending: Bool
    public var eventKey: String?
    public var timestamp: Int64?
    public var attachments: [String]
    /// The strings that went out cut short (over `History.cap`), to fetch whole when wanted.
    public var cut: [Cut]
    /// A prompt or steer that took your answer to a question: that question's id (its ask
    /// message's `toolID`). Its card shows the answer; the prompt still starts its turn.
    public var answerTo: String?
    /// Flotilla's own message to the agent, not yours (`[flotilla] …`: a hand-off's answer, the
    /// agents' report, a start or a request that failed), as a prompt or, into a running turn,
    /// a steer. Shown as Flotilla's whichever way it went.
    public var fromFlotilla: Bool

    public var id: Int64 { seq }

    public init(seq: Int64, kind: String, text: String? = nil, status: String? = nil, taskID: String? = nil, eventKey: String? = nil, timestamp: Int64? = nil) {
        self.seq = seq
        self.kind = kind
        self.text = text
        self.status = status
        self.taskID = taskID
        self.eventKey = eventKey
        self.timestamp = timestamp
        isError = false
        pending = false
        attachments = []
        cut = []
        fromFlotilla = false
    }

    init(json j: JSON) throws(ReadError) {
        guard let seq = j["seq"]?.int else { throw ReadError("a message without its seq") }
        self.seq = seq
        taskID = j.optStr("task_id")
        kind = j.optStr("kind") ?? "notice"
        text = j.optStr("text")
        status = j.optStr("status")
        model = j.optStr("model")
        toolName = j.optStr("tool_name")
        toolID = j.optStr("tool_id")
        toolSummary = j.optStr("tool_summary")
        toolInputJSON = j.optStr("tool_input_json")
        toolResultJSON = j.optStr("tool_result_json")
        toolResultType = j.optStr("tool_result_type")
        isError = j.flag("is_error")
        pending = j.flag("pending")
        eventKey = j.optStr("event_key")
        timestamp = j.optInt("timestamp")
        attachments = try j.list("attachments").compactMap(\.string)
        cut = try j.list("cut").map { (c: JSON) throws(ReadError) in try Cut(json: c) }
        answerTo = j.optStr("answer_to")
        fromFlotilla = j.flag("from_flotilla")
    }
}

/// A string a message carries cut short: which field, how long it is whole (bytes), and its
/// mark, to ask for it whole.
public struct Cut: Sendable, Hashable {
    /// `tool_input_json` or `tool_result_json`.
    public var field: String
    public var bytes: Int64
    public var mark: String

    public init(field: String, bytes: Int64, mark: String) {
        self.field = field
        self.bytes = bytes
        self.mark = mark
    }

    init(json j: JSON) throws(ReadError) {
        field = try j.str("field")
        bytes = j.optInt("bytes") ?? 0
        mark = try j.str("mark")
    }
}

/// A page's mark, in a thread's view: a copy of the page is good while it matches.
public struct PageMark: Sendable, Hashable {
    public var page: Int64
    public var mark: String

    public init(page: Int64, mark: String) {
        self.page = page
        self.mark = mark
    }

    init(json j: JSON) throws(ReadError) {
        guard let page = j["page"]?.int else { throw ReadError("a page mark without its page") }
        self.page = page
        mark = try j.str("mark")
    }
}

/// One page of a thread's older messages (`GET /v1/threads/:id/history`), and its mark as read.
public struct HistoryPage: Sendable, Hashable {
    public var thread: String
    public var page: Int64
    public var mark: String
    public var messages: [Message]

    public init(json j: JSON) throws(ReadError) {
        thread = try j.str("thread")
        guard let page = j["page"]?.int else { throw ReadError("a history page without its page") }
        self.page = page
        mark = try j.str("mark")
        messages = try j.list("messages").map { (m: JSON) throws(ReadError) in try Message(json: m) }
    }
}

/// A prompt or steer as it was typed and sent.
public struct SentPrompt: Sendable, Hashable {
    public var id: String
    public var kind: String
    public var text: String
    /// The scheduled prompt that sent it, if one did.
    public var schedule: String?

    init(json j: JSON) throws(ReadError) {
        id = try j.str("id")
        kind = j.optStr("kind") ?? "prompt"
        text = j.optStr("text") ?? ""
        schedule = j.optStr("schedule")
    }
}

public enum ChildState: String, Sendable, Hashable {
    case starting, working, done, error, gone, didntStart = "didnt_start", failed, stopped

    /// Starting or at work.
    public var running: Bool { self == .starting || self == .working }
}

/// A thread another handed work to, or an agent it started in its own VM, as its card shows.
public struct ChildCard: Sendable, Hashable, Identifiable {
    public var id: String
    public var title: String
    public var provider: String
    public var model: String
    public var state: ChildState
    public var task: String
    public var calls: Int64
    /// Its latest call (name, summary) while it works.
    public var latest: (String, String)?
    public var answer: String?
    public var startedMs: Int64?
    public var durationMs: Int64?
    /// A thread of its own to open (a delegate).
    public var thread: Bool

    init(json j: JSON) throws(ReadError) {
        id = try j.str("id")
        title = j.optStr("title") ?? ""
        provider = j.optStr("provider") ?? ""
        model = j.optStr("model") ?? ""
        state = j.optStr("state").flatMap(ChildState.init(rawValue:)) ?? .working
        task = j.optStr("task") ?? ""
        calls = j.optInt("calls") ?? 0
        latest = j.pair("latest")
        answer = j.optStr("answer")
        startedMs = j.optInt("started_ms")
        durationMs = j.optInt("duration_ms")
        thread = j.flag("thread")
    }

    public static func == (a: ChildCard, b: ChildCard) -> Bool {
        a.id == b.id && a.title == b.title && a.provider == b.provider && a.model == b.model && a.state == b.state && a.task == b.task && a.calls == b.calls
            && a.latest?.0 == b.latest?.0 && a.latest?.1 == b.latest?.1 && a.answer == b.answer && a.startedMs == b.startedMs && a.durationMs == b.durationMs
            && a.thread == b.thread
    }

    public func hash(into h: inout Hasher) { h.combine(id) }
}

/// One subagent, as its harness recorded it in the VM.
public struct Subagent: Sendable, Hashable {
    public var id: String
    /// The call that started it (Claude Code's Agent call).
    public var parentCall: String?
    public var name: String
    public var kind: String?
    public var model: String?
    public var prompt: String?
    public var running: Bool
    public var background: Bool
    public var startedMs: Int64?
    public var finishedMs: Int64?
    /// Its own tool calls' ids.
    public var calls: [String]
    /// Its latest call (name, summary), for a harness whose calls boat doesn't show (OpenCode).
    public var latestName: String?
    public var latestSummary: String?
    /// What it said, in order; the last is its answer.
    public var texts: [String]

    init(json j: JSON) throws(ReadError) {
        id = try j.str("id")
        parentCall = j.optStr("parent_call")
        name = j.optStr("name") ?? ""
        kind = j.optStr("kind")
        model = j.optStr("model")
        prompt = j.optStr("prompt")
        running = j.flag("running")
        background = j.flag("background")
        startedMs = j.optInt("started_ms")
        finishedMs = j.optInt("finished_ms")
        calls = try j.list("calls").compactMap(\.string)
        if let pair = j["latest"]?.array, pair.count == 2 {
            latestName = pair[0].string
            latestSummary = pair[1].string
        }
        texts = try j.list("texts").compactMap(\.string)
    }
}

/// One thread: everything its page shows.
public struct ThreadDetail: Sendable, Hashable {
    public var id: String
    /// None when there's no such thread (deleted, or not written yet).
    public var thread: Thread?
    /// Its newest messages, from seq `older` on (whole pages); older ones come a page at a time.
    public var messages: [Message]
    public var older: Int64
    /// Every page's mark, oldest first.
    public var pages: [PageMark]
    public var sent: [SentPrompt]
    public var startError: String?
    /// A project thread's repo, and the branch its work merges into.
    public var projectRepo: String?
    public var into: String?
    public var subagents: [Subagent]
    /// The thread that handed it its task: (id, title).
    public var parent: (String, String)?
    public var children: [ChildCard]
    /// How full its context is, as its harness last recorded it.
    public var context: ContextUse?
    /// Its services (`.agents/services.yml`) as last seen.
    public var services: ThreadServices?
    /// Its ports whose links anyone may open now, by port (`public`).
    public var publicLinks: [PublicLink]

    init(json j: JSON) throws(ReadError) {
        guard j.object != nil else { throw ReadError("the thread isn't an object") }
        id = try j.str("id")
        thread = try j["thread"].flatMap { (t: JSON) throws(ReadError) in t.isNull ? nil : try Thread(json: t) }
        messages = try j.list("messages").map { (m: JSON) throws(ReadError) in try Message(json: m) }
        older = j.optInt("older") ?? 0
        pages = try j.list("pages").map { (p: JSON) throws(ReadError) in try PageMark(json: p) }
        sent = try j.list("sent").map { (s: JSON) throws(ReadError) in try SentPrompt(json: s) }
        startError = j.optStr("start_error")
        projectRepo = j["project"]?["project"]?.optStr("repo")
        into = j["project"]?.optStr("into")
        subagents = try (j["lineage"].map { (l: JSON) throws(ReadError) in try l.list("subagents") } ?? []).map { (s: JSON) throws(ReadError) in try Subagent(json: s) }
        parent = j.pair("parent")
        children = try j.list("children").map { (c: JSON) throws(ReadError) in try ChildCard(json: c) }
        context = j["context"].flatMap { $0.isNull ? nil : ContextUse(json: $0) }
        services = try j["services"].flatMap { (s: JSON) throws(ReadError) in s.isNull ? nil : try ThreadServices(json: s) }
        var links: [PublicLink] = []
        for l in try j.list("public") {
            if let link = try PublicLink(json: l) { links.append(link) }
        }
        publicLinks = links
    }

    public static func == (a: ThreadDetail, b: ThreadDetail) -> Bool {
        a.id == b.id && a.thread == b.thread && a.messages == b.messages && a.older == b.older && a.pages == b.pages && a.sent == b.sent && a.startError == b.startError && a.projectRepo == b.projectRepo
            && a.into == b.into && a.subagents == b.subagents && a.parent?.0 == b.parent?.0 && a.parent?.1 == b.parent?.1 && a.children == b.children
            && a.context == b.context && a.services == b.services && a.publicLinks == b.publicLinks
    }

    public func hash(into h: inout Hasher) {
        h.combine(id)
        h.combine(messages.count)
    }
}

/// boat's model catalog.
public struct Catalog: Sendable, Hashable {
    public struct Model: Sendable, Hashable {
        public var id: String
        public var label: String?
        public var fastMode: Bool
        /// The effort levels it takes, and its default; none: it takes none.
        public var effort: Effort?

        public init(id: String, label: String? = nil, fastMode: Bool = false, effort: Effort? = nil) {
            self.id = id
            self.label = label
            self.fastMode = fastMode
            self.effort = effort
        }
    }

    public struct Effort: Sendable, Hashable {
        public var supported: [String]
        public var defaultLevel: String?

        public init(supported: [String], defaultLevel: String?) {
            self.supported = supported
            self.defaultLevel = defaultLevel
        }
    }

    public struct Provider: Sendable, Hashable {
        public var key: String
        public var cliName: String?
        public var aliases: [String]
        public var defaultModel: String?
        public var models: [Model]

        public init(key: String, cliName: String? = nil, aliases: [String] = [], defaultModel: String? = nil, models: [Model]) {
            self.key = key
            self.cliName = cliName
            self.aliases = aliases
            self.defaultModel = defaultModel
            self.models = models
        }
    }

    public var providers: [Provider]

    public init(providers: [Provider]) {
        self.providers = providers
    }

    init(json j: JSON) throws(ReadError) {
        providers = try j.list("providers").map { (p: JSON) throws(ReadError) in
            Provider(
                key: try p.str("key"), cliName: p.optStr("cli_name"), aliases: try p.list("aliases").compactMap(\.string), defaultModel: p.optStr("default_model"),
                models: try p.list("models").compactMap { m in
                    m.optStr("id").map {
                        Model(
                            id: $0, label: m.optStr("label"), fastMode: m.flag("fast_mode"),
                            effort: m["effort"].flatMap { e in e.object == nil ? nil : Effort(supported: e["supported"]?.array?.compactMap(\.string) ?? [], defaultLevel: e.optStr("default")) }
                        )
                    }
                }
            )
        }
    }

    /// Check settings against the catalog and give them in canonical form, as Rust's
    /// `Catalog::validate`: a model with effort levels gets its default when none is given.
    public func validate(provider: String, model: String, effort: String?, fast: Bool) -> ThreadSettings? {
        guard let p = self.provider(provider), let m = p.models.first(where: { $0.id == model }) else { return nil }
        let level: String?
        switch (m.effort, effort) {
        case (let opts?, let e?): guard opts.supported.contains(e) else { return nil }; level = e
        case (let opts?, nil): level = opts.defaultLevel ?? opts.supported.first
        case (nil, _?): return nil
        case (nil, nil): level = nil
        }
        if fast && !m.fastMode { return nil }
        return ThreadSettings(provider: p.key, model: m.id, effort: level, fast: fast)
    }

    /// `model` with an effort and fast that suit it, as near `effort` and `fast` as it allows
    /// (Rust's `Catalog::fit`): the same level if it has it, else the nearest it has (the lower
    /// on a tie), else its default; fast only if it has fast mode. For one prompt started on
    /// several models. None for a model the catalog doesn't have.
    public func fit(provider: String, model: String, effort: String?, fast: Bool) -> ThreadSettings? {
        let m = self.provider(provider)?.models.first { $0.id == model }
        let level: String? = switch (m?.effort, effort) {
        case (let opts?, let e?): Self.nearestEffort(e, supported: opts.supported)
        default: nil
        }
        return validate(provider: provider, model: model, effort: level, fast: fast && m?.fastMode == true)
    }

    /// Effort levels from least to most, as boat's catalog names them.
    static let effortOrder = ["none", "minimal", "low", "medium", "high", "xhigh", "max"]

    /// The level of `supported` nearest `wanted` (itself if there; the lower on a tie). None
    /// when `wanted` isn't a level Flotilla knows the order of (or none of `supported` is): the
    /// model's default then applies.
    static func nearestEffort(_ wanted: String, supported: [String]) -> String? {
        if supported.contains(wanted) { return wanted }
        guard let want = effortOrder.firstIndex(of: wanted) else { return nil }
        return supported
            .compactMap { s in effortOrder.firstIndex(of: s).map { (rank: $0, level: s) } }
            .min { (abs($0.rank - want), $0.rank) < (abs($1.rank - want), $1.rank) }?
            .level
    }

    /// The agents offered for new threads: those whose subagents Flotilla can show.
    public static func offered(_ provider: String) -> Bool { ["codex", "claude-code", "opencode"].contains(provider) }

    /// Claude Code's default model, else the first offered agent's (`picker::default_settings`).
    public var defaultSettings: ThreadSettings? {
        guard let p = provider("claude-code") ?? providers.first(where: { Self.offered($0.key) }),
              let model = p.defaultModel ?? p.models.first?.id
        else { return nil }
        return validate(provider: p.key, model: model, effort: nil, fast: false)
    }

    /// A provider by key, CLI name or alias.
    public func provider(_ name: String) -> Provider? {
        providers.first { $0.key == name } ?? providers.first { $0.cliName == name || $0.aliases.contains(name) }
    }

    /// The model's label ("Opus 5.5"), else its id (a branch tag names it so, as on the Mac).
    public func modelLabel(provider: String, model: String) -> String {
        label(provider: provider, model: model) ?? model
    }

    /// The model's label, if the catalog has one.
    public func label(provider: String, model: String) -> String? {
        self.provider(provider)?.models.first { $0.id == model }?.label
    }

    /// A model's id as words, for one the catalog doesn't name: "claude-sonnet-5" is
    /// "Sonnet 5", "claude-haiku-4-5-20251001" "Haiku 4.5", "gpt-5.4-mini" "GPT-5.4 Mini",
    /// "anthropic/claude-opus-4-1" "Opus 4.1".
    public static func readable(_ id: String) -> String {
        var parts = (id.split(separator: "/").last ?? "").split(separator: "-").map(String.init)
        if parts.count > 1, parts[0].lowercased() == "claude" { parts.removeFirst() }
        // A dated snapshot.
        if parts.count > 1, let last = parts.last, last.count == 8, last.allSatisfy(\.isNumber) { parts.removeLast() }
        var words: [String] = []
        for p in parts where !p.isEmpty {
            if let last = words.last, p.allSatisfy(\.isNumber), last.last?.isNumber == true, !last.contains(".") {
                // "5", "5": 5.5.
                words[words.count - 1] = "\(last).\(p)"
            } else if words.last == "GPT", p.first?.isNumber == true {
                words[words.count - 1] = "GPT-\(p)"
            } else if p.lowercased() == "gpt" {
                words.append("GPT")
            } else if p.allSatisfy(\.isLetter) {
                words.append(p.prefix(1).uppercased() + p.dropFirst())
            } else {
                words.append(p)
            }
        }
        return words.isEmpty ? id : words.joined(separator: " ")
    }
}

/// The four settings a thread locks at its start.
public struct ThreadSettings: Sendable, Hashable {
    public var provider: String
    public var model: String
    public var effort: String?
    public var fast: Bool

    public init(provider: String, model: String, effort: String?, fast: Bool) {
        self.provider = provider
        self.model = model
        self.effort = effort
        self.fast = fast
    }

    init?(json j: JSON?) {
        guard let j, let p = j.optStr("provider"), let m = j.optStr("model") else { return nil }
        self.init(provider: p, model: m, effort: j.optStr("effort"), fast: j.flag("fast"))
    }
}

/// One of the server's setup checks (`flotilla doctor`), and the commands that fix it there.
public struct Diagnosis: Sendable, Hashable {
    /// `boat_key`, `git_hub`, `claude`, `tailscale`, `database`, `others`, `push`, `mcp`…
    public var check: String
    /// `ok`, `warn`, `fail` or `off`.
    public var health: String
    public var says: String
    public var fix: [String]

    public init(check: String, health: String, says: String, fix: [String]) {
        self.check = check
        self.health = health
        self.says = says
        self.fix = fix
    }

    /// Threads can't run while it fails, as `CheckOf::threads_need` says: their VMs need the
    /// boat key, and everything they do is written to the database.
    public var threadsNeed: Bool { check == "boat_key" || check == "database" }

    /// What it's called, as `CheckOf::label` says.
    public var label: String {
        switch check {
        case "boat_key": "boat API key"
        case "git_hub": "GitHub"
        case "claude": "Titles and previews"
        case "tailscale": "Tailscale"
        case "database": "Database"
        case "others": "Other Flotillas"
        case "push": "Push"
        case "mcp": "Remote MCP"
        default: check.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }
}

/// The settings view: what the phone shows of it.
public struct ServerSettings: Sendable, Hashable {
    /// Defaults for new threads, once chosen.
    public var defaults: ThreadSettings?
    /// The server's setup checks; none until they've run.
    public var checks: [Diagnosis]?
    public var checkedMs: Int64?
    /// Archive a thread when its pull request merges (on unless turned off).
    public var archiveOnMerge: Bool
    /// Archive threads idle for this many days (0: never).
    public var archiveIdleDays: Int

    /// A check threads can't run without failed (`Doctor::stops_threads`): the setup takes New
    /// thread's place.
    public var stopsThreads: Bool { checks?.contains { $0.health == "fail" && $0.threadsNeed } ?? false }

    /// How many checks passed, of those that are on, as `Doctor::tally` says: an optional part
    /// that's off (push, remote MCP) isn't counted, so it doesn't make the setup look unfinished.
    public var tally: (ok: Int, of: Int) {
        let on = checks?.filter { $0.health != "off" } ?? []
        return (on.filter { $0.health == "ok" }.count, on.count)
    }

    init(json j: JSON) throws(ReadError) {
        guard j.object != nil else { throw ReadError("the settings aren't an object") }
        defaults = ThreadSettings(json: j["defaults"])
        archiveOnMerge = j["archive_on_merge"]?.bool ?? true
        archiveIdleDays = j.optInt("archive_idle_days").map { Int($0) } ?? 0
        // (JSON takes `nil` as `.null`: an Optional's none is spelled out.)
        let doctor: JSON? = if let d = j["doctor"], !d.isNull { d } else { .none }
        checks = try doctor.map { (d: JSON) throws(ReadError) in
            try d.list("checks").map { c in Diagnosis(check: c.optStr("check") ?? "", health: c.optStr("health") ?? "ok", says: c.optStr("says") ?? "", fix: c["fix"]?.array?.compactMap(\.string) ?? []) }
        }
        checkedMs = doctor?.optInt("at_ms")
    }
}

/// A device paired with the server.
public struct DeviceRow: Sendable, Hashable, Identifiable {
    public var id: String
    public var name: String
    /// `macos`, `linux`, `ios`…
    public var platform: String
    public var pairedMs: Int64
    public var lastSeenMs: Int64
    public var connected: Bool
    /// An assistant through `flotilla mcp`, which the server holds to what an agent may do.
    public var agent: Bool

    init(json j: JSON) throws(ReadError) {
        id = try j.str("id")
        name = j.optStr("name") ?? ""
        platform = j.optStr("platform") ?? ""
        pairedMs = j.optInt("paired_ms") ?? 0
        lastSeenMs = j.optInt("last_seen_ms") ?? 0
        connected = j.flag("connected")
        agent = j.optStr("scope") == "agent"
    }
}

/// One event from boat, kept verbatim: streaming partials reach the client as these.
public struct RawEvent: Sendable, Hashable {
    public var id: String
    public var kind: String
    public var timestamp: Int64
    public var taskID: String?
    public var data: JSON

    public init(id: String, kind: String, timestamp: Int64, taskID: String? = nil, data: JSON) {
        self.id = id
        self.kind = kind
        self.timestamp = timestamp
        self.taskID = taskID
        self.data = data
    }

    init(json j: JSON) throws(ReadError) {
        id = try j.str("id")
        kind = j.optStr("kind") ?? ""
        timestamp = j.optInt("timestamp") ?? 0
        taskID = j.optStr("task_id")
        data = j["data"] ?? .null
    }

    /// The text it streams so far, if any.
    public var streamingText: String? {
        guard let s = data["content"]?.string, !s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return s
    }
}

/// A view, read from its key and data. Views the phone doesn't show are kept as they came.
public enum ServerView: Sendable, Hashable {
    case sidebar(Sidebar)
    /// boat's model catalog, once read.
    case catalog(Catalog?)
    case thread(ThreadDetail)
    case settings(ServerSettings)
    /// The devices paired with the server, most recently seen first.
    case devices([DeviceRow])
    /// What the agents spent.
    case agentUsage(AgentUsage)
    /// Every scheduled prompt, due soonest first.
    case schedules([Schedule])
    case other(ViewKey, JSON)

    public var key: ViewKey {
        switch self {
        case .sidebar: .sidebar
        case .catalog: .catalog
        case .thread(let t): .thread(t.id)
        case .settings: .settings
        case .devices: .devices
        case .agentUsage: .agentUsage
        case .schedules: .schedules
        case .other(let k, _): k
        }
    }

    /// As Rust's `View::from_data`: the data must read as the view, and be the view of its key.
    public init(key: ViewKey, data: JSON) throws(ReadError) {
        switch key {
        case .sidebar:
            self = .sidebar(try Sidebar(json: data))
        case .catalog:
            self = .catalog(data.isNull ? nil : try Catalog(json: data))
        case .thread(let id):
            let t = try ThreadDetail(json: data)
            guard t.id == id else { throw ReadError("a view of thread \(t.id) sent as thread \(id)") }
            self = .thread(t)
        case .mentions(let id):
            guard data["thread"]?.string == id else { throw ReadError("a mentions view of another thread sent as \(id)") }
            self = .other(key, data)
        case .settings:
            self = .settings(try ServerSettings(json: data))
        case .devices:
            guard data.object != nil else { throw ReadError("not a devices view") }
            self = .devices(try data.list("devices").map { (d: JSON) throws(ReadError) in try DeviceRow(json: d) })
        case .schedules:
            guard data.object != nil else { throw ReadError("not a schedules view") }
            // One this version can't read is left out, not the whole view.
            self = .schedules(try data.list("schedules").compactMap { Schedule(json: $0) })
        case .project:
            guard data.object != nil else { throw ReadError("not a \(key.name) view") }
            self = .other(key, data)
        case .agentUsage:
            self = .agentUsage(try AgentUsage(json: data))
        }
    }
}

import Foundation
