import Foundation

/// What the agents' own records say about their use (`flotilla_proto::limits`), and how the
/// phone shows it (`flotilla_model::limits`, line for line): a thread's context, and the agent
/// accounts' usage limits. Each number is the harness's or its service's own; one that isn't
/// known is nil, and nothing shows for it.

/// How much of its model's context window a thread fills, after its latest reply.
public struct ContextUse: Sendable, Hashable {
    public var used: Int64
    /// The model's window as the harness counts it, once known.
    public var window: Int64?
    public var model: String?
    /// The account whose limits the thread spends ("claude", "chatgpt"), if known.
    public var account: String?
    public var atMs: Int64?

    public init(used: Int64, window: Int64? = nil, model: String? = nil, account: String? = nil, atMs: Int64? = nil) {
        self.used = used
        self.window = window
        self.model = model
        self.account = account
        self.atMs = atMs
    }

    init?(json j: JSON) {
        guard let used = j.optInt("used") else { return nil }
        self.used = used
        window = j.optInt("window")
        model = j.optStr("model")
        account = j.optStr("account")
        atMs = j.optInt("at_ms")
    }

    /// The share of the window used, 0 to 100, once the window is known.
    public var percent: Double? {
        guard let w = window, w > 0 else { return nil }
        return min(Double(used) / Double(w) * 100, 100)
    }
}

/// One limit: a share of the account's allowance used in a window that resets.
public struct LimitWindow: Sendable, Hashable {
    public var id: String
    /// "5-hour", "Weekly", "Weekly · Fable".
    public var label: String
    /// Percent used, 0 to 100.
    public var used: Int
    /// When it resets (RFC 3339), if told.
    public var resetsAt: String?
    public var minutes: Int?
    /// The model it's for alone ("Fable"), when it isn't the whole account's.
    public var scope: String?

    public init(id: String, label: String, used: Int, resetsAt: String? = nil, minutes: Int? = nil, scope: String? = nil) {
        self.id = id
        self.label = label
        self.used = used
        self.resetsAt = resetsAt
        self.minutes = minutes
        self.scope = scope
    }

    init(json j: JSON) {
        id = j.optStr("id") ?? ""
        label = j.optStr("label") ?? ""
        used = Int(j.optInt("used") ?? 0)
        resetsAt = j.optStr("resets_at")
        minutes = j.optInt("minutes").map(Int.init)
        scope = j.optStr("scope")
    }

    /// All used up until it resets.
    public var reached: Bool { used >= 100 }

    /// It limits a thread on `model` (the harness's id for it): the whole account's do, and
    /// one for a model alone only threads on that model.
    public func applies(to model: String?) -> Bool {
        guard let scope else { return true }
        return model?.lowercased().contains(scope.lowercased()) ?? false
    }

    /// When it resets, in ms since the epoch.
    public var resetsMs: Int64? {
        // The server writes them to the second, in UTC.
        guard let s = resetsAt, let d = try? Date(s, strategy: .iso8601) else { return nil }
        return Int64(d.timeIntervalSince1970 * 1000)
    }

    /// Still running at `now`: it hasn't reset since it was read. One whose reset has passed
    /// says nothing about now, so it isn't shown.
    public func current(now: Int64) -> Bool { resetsMs.map { $0 > now } ?? true }

    /// A weekly window (a week or longer): the only kind shown (`limits::weekly`). A 5-hour one
    /// comes and goes too fast to plan by, so it's left out everywhere.
    public var weekly: Bool { minutes.map { $0 >= 7 * 24 * 60 } ?? id.hasPrefix("seven_day") }
}

/// An agent account's limits, as its service last reported them.
public struct AccountLimits: Sendable, Hashable {
    /// "claude" or "chatgpt".
    public var account: String
    /// The plan, when the service names it ("pro").
    public var plan: String?
    public var windows: [LimitWindow]
    public var readMs: Int64

    public init(account: String, plan: String? = nil, windows: [LimitWindow], readMs: Int64) {
        self.account = account
        self.plan = plan
        self.windows = windows
        self.readMs = readMs
    }

    init(json j: JSON) throws(ReadError) {
        account = try j.str("account")
        plan = j.optStr("plan")
        windows = try j.list("windows").map(LimitWindow.init(json:))
        readMs = j.optInt("read_ms") ?? 0
    }

    /// The account's name as people know it.
    public var name: String { Limits.accountName(account) }

    /// Its weekly windows still running at `now`: what the readout, its details and "limit
    /// reached" go by.
    public func windows(now: Int64) -> [LimitWindow] { windows.filter { $0.weekly && $0.current(now: now) } }
}

public enum Limits {
    public enum Tone: Sendable { case plain, warn, bad }

    /// The harness whose logo stands for an account: Claude Code's for Claude, Codex's for
    /// ChatGPT.
    public static func logo(_ account: String) -> String? {
        switch account {
        case "claude": "claude-code"
        case "chatgpt": "codex"
        default: nil
        }
    }

    public static func accountName(_ account: String) -> String {
        switch account {
        case "claude": "Claude"
        case "chatgpt": "ChatGPT"
        default: account
        }
    }

    /// A count of tokens, short: 950, 9.5k, 22k, 1.2m (as T3 writes them).
    public static func tokens(_ n: Int64) -> String {
        func short(_ v: Double, _ unit: String) -> String {
            let s = String(format: "%.1f", v)
            return (s.hasSuffix(".0") ? String(s.dropLast(2)) : s) + unit
        }
        switch n {
        case ..<1_000: return "\(n)"
        case ..<10_000: return short(Double(n) / 1_000, "k")
        case ..<999_500: return "\(Int64((Double(n) / 1_000).rounded()))k"
        default: return short(Double(n) / 1_000_000, "m")
        }
    }

    /// A count in full: 21,909.
    public static func exact(_ n: Int64) -> String {
        let digits = Array(String(n))
        var out = ""
        for (i, c) in digits.enumerated() {
            if i > 0 && (digits.count - i) % 3 == 0 { out.append(",") }
            out.append(c)
        }
        return out
    }

    /// A share as T3 writes it: one decimal under 10% (2.4%), whole above (42%).
    public static func percent(_ p: Double) -> String {
        if p < 10 {
            let s = String(format: "%.1f", max(p, 0))
            return (s.hasSuffix(".0") ? String(s.dropLast(2)) : s) + "%"
        }
        return "\(Int64(p.rounded()))%"
    }

    /// A context's tone: amber from 80% of the window, red from 90%.
    public static func contextTone(_ percent: Double) -> Tone { percent >= 90 ? .bad : percent >= 80 ? .warn : .plain }

    /// A limit's tone: amber from 80% used, red when used up.
    public static func limitTone(_ used: Int) -> Tone { used >= 100 ? .bad : used >= 80 ? .warn : .plain }

    /// How many of `cells` a share lights: any use lights one, and all only when it's all used.
    public static func lit(_ percent: Double, cells: Int) -> Int {
        if percent <= 0 { return 0 }
        if percent >= 100 { return cells }
        return min(max(Int((percent / 100 * Double(cells)).rounded(.up)), 1), cells - 1)
    }

    /// The window closest to its limit across the accounts, with its account.
    public static func tightest(_ all: [AccountLimits], now: Int64) -> (AccountLimits, LimitWindow)? {
        var best: (AccountLimits, LimitWindow)?
        for l in all {
            for w in l.windows(now: now) where best.map({ w.used > $0.1.used }) ?? true { best = (l, w) }
        }
        return best
    }

    /// The window that has used up the thread's account, if one has: no turn runs until it
    /// resets (the latest reset of those used up). One for another model alone doesn't count.
    public static func reached(_ all: [AccountLimits], context c: ContextUse?, now: Int64) -> (AccountLimits, LimitWindow)? {
        guard let account = c?.account, let l = all.first(where: { $0.account == account }) else { return nil }
        let full = l.windows(now: now).filter { $0.reached && $0.applies(to: c?.model) }
        return full.max { ($0.resetsMs ?? .max) < ($1.resetsMs ?? .max) }.map { (l, $0) }
    }

    /// How long until `at`: "4d 6h", "2h 13m", "12m", "under a minute".
    public static func until(_ at: Int64, now: Int64) -> String {
        let mins = max(at - now, 0) / 60_000
        let (d, h, m) = (mins / 1440, mins % 1440 / 60, mins % 60)
        switch (d, h, m) {
        case (0, 0, 0): return "under a minute"
        case (0, 0, _): return "\(m)m"
        case (0, _, _): return "\(h)h \(m)m"
        default: return "\(d)d \(h)h"
        }
    }
}
