import Foundation

/// Scheduled prompts, as `flotilla_proto::schedule` writes them: a prompt the server sends
/// later, once or again and again, to one thread or a new thread each run. The words for a
/// row follow `flotilla_model::schedule` (tested against the same cases).

/// When it runs, in its zone, at most once a day (a one-off aside). Times of day are minutes
/// after midnight; days are ISO weekdays, 1 (Monday) to 7 (Sunday).
public enum ScheduleTiming: Sendable, Hashable {
    case once(atMs: Int64)
    /// Every `every` hours from `fromMs`, its first run: only as set before runs had to be a
    /// day apart (the server pauses one under 24), so it still reads.
    case hours(every: Int, fromMs: Int64)
    case daily(at: Int)
    case weekdays(at: Int)
    case weekly(days: [Int], at: Int)
    case cron(String)

    init?(json: JSON?) {
        guard let first = json?.object?.first else { return nil }
        let (kind, v) = first
        let at = Int(v["at"]?.int ?? 0)
        switch kind {
        case "once": self = .once(atMs: v["at_ms"]?.int ?? 0)
        case "hours": self = .hours(every: Int(v["every"]?.int ?? 1), fromMs: v["from_ms"]?.int ?? 0)
        case "daily": self = .daily(at: at)
        case "weekdays": self = .weekdays(at: at)
        case "weekly": self = .weekly(days: v["days"]?.array?.compactMap { $0.int.map(Int.init) } ?? [], at: at)
        case "cron": self = .cron(v["expr"]?.string ?? "")
        default: return nil
        }
    }

    var json: JSON {
        switch self {
        case .once(let at): ["once": ["at_ms": .int(at)]]
        case .hours(let every, let from): ["hours": ["every": .int(Int64(every)), "from_ms": .int(from)]]
        case .daily(let at): ["daily": ["at": .int(Int64(at))]]
        case .weekdays(let at): ["weekdays": ["at": .int(Int64(at))]]
        case .weekly(let days, let at): ["weekly": ["days": .array(days.map { .int(Int64($0)) }), "at": .int(Int64(at))]]
        case .cron(let expr): ["cron": ["expr": .string(expr)]]
        }
    }

    public var isOnce: Bool { if case .once = self { true } else { false } }
}

/// What a new thread starts with: what New thread takes, less the prompt.
public struct FreshThread: Sendable, Hashable {
    public var environment: String
    public var projectID: String?
    public var provider: String
    public var model: String
    public var effort: String?
    public var fast: Bool
    /// boat's machine type (`small`), kept as it was set.
    public var machineType: String?
    public var onBranch: OnBranch?

    public init(environment: String, projectID: String?, provider: String, model: String, effort: String?, fast: Bool, machineType: String? = nil, onBranch: OnBranch? = nil) {
        self.environment = environment
        self.projectID = projectID
        self.provider = provider
        self.model = model
        self.effort = effort
        self.fast = fast
        self.machineType = machineType
        self.onBranch = onBranch
    }

    init?(json j: JSON?) {
        guard let j, let provider = j.optStr("provider"), let model = j.optStr("model") else { return nil }
        self.init(
            environment: j.optStr("environment") ?? "", projectID: j.optStr("project_id"), provider: provider, model: model, effort: j.optStr("effort"), fast: j.flag("fast"),
            machineType: j.optStr("machine_type"), onBranch: OnBranch(json: j["on_branch"]))
    }

    var json: JSON {
        [
            "environment": .string(environment), "project_id": JSON(projectID), "provider": .string(provider), "model": .string(model), "effort": JSON(effort), "fast": .bool(fast),
            "machine_type": JSON(machineType), "on_branch": onBranch?.json ?? .null,
        ]
    }
}

/// Where each run's prompt goes.
public enum ScheduleTarget: Sendable, Hashable {
    /// A new thread each run.
    case new(FreshThread)
    /// This thread, as a follow-up.
    case thread(String)

    init?(json: JSON?) {
        if let t = json?["thread"]?.string {
            self = .thread(t)
        } else if let f = FreshThread(json: json?["new"]) {
            self = .new(f)
        } else {
            return nil
        }
    }

    var json: JSON {
        switch self {
        case .new(let f): ["new": f.json]
        case .thread(let t): ["thread": .string(t)]
        }
    }

    public var thread: String? { if case .thread(let t) = self { t } else { nil } }
}

/// One run, or one that didn't happen.
public struct ScheduleRun: Sendable, Hashable {
    public enum Outcome: String, Sendable, Hashable { case sent, missed, failed }
    public var atMs: Int64
    public var outcome: Outcome
    /// The thread it went to, or started.
    public var thread: String?
    /// Why it didn't run.
    public var why: String?

    init?(json j: JSON?) {
        guard let j, j.object != nil else { return nil }
        atMs = j.optInt("at_ms") ?? 0
        // One this version doesn't know reads as failed, as Rust's does.
        outcome = Outcome(rawValue: j.optStr("outcome") ?? "") ?? .failed
        thread = j.optStr("thread")
        why = j.optStr("why")
    }

    public init(atMs: Int64, outcome: Outcome, thread: String? = nil, why: String? = nil) {
        self.atMs = atMs
        self.outcome = outcome
        self.thread = thread
        self.why = why
    }
}

/// A schedule as the server keeps it.
public struct Schedule: Sendable, Hashable, Identifiable {
    public var id: String
    public var prompt: String
    public var target: ScheduleTarget
    public var when: ScheduleTiming
    public var zone: String
    public var paused: Bool
    public var nextMs: Int64?
    /// A run due then waits for the thread's turn to end.
    public var waitingMs: Int64?
    public var last: ScheduleRun?
    public var runs: Int64
    /// The device that set it up.
    public var from: String

    init?(json j: JSON) {
        // A target or timing this version doesn't know: left out, as Rust's store leaves it.
        guard let id = j.optStr("id"), let target = ScheduleTarget(json: j["target"]), let when = ScheduleTiming(json: j["when"]) else { return nil }
        self.id = id
        prompt = j.optStr("prompt") ?? ""
        self.target = target
        self.when = when
        zone = j.optStr("zone") ?? "UTC"
        paused = j.flag("paused")
        nextMs = j.optInt("next_ms")
        waitingMs = j.optInt("waiting_ms")
        last = ScheduleRun(json: j["last"])
        runs = j.optInt("runs") ?? 0
        from = j.optStr("from") ?? ""
    }

    public init(id: String, prompt: String, target: ScheduleTarget, when: ScheduleTiming, zone: String, paused: Bool = false, nextMs: Int64?, waitingMs: Int64? = nil, last: ScheduleRun? = nil, runs: Int64 = 0, from: String = "") {
        self.id = id
        self.prompt = prompt
        self.target = target
        self.when = when
        self.zone = zone
        self.paused = paused
        self.nextMs = nextMs
        self.waitingMs = waitingMs
        self.last = last
        self.runs = runs
        self.from = from
    }

    /// A one-off that has run (or didn't, and won't on its own).
    public var done: Bool { when.isOnce && nextMs == nil && !paused && waitingMs == nil }
}

/// A schedule as a client sets it (`schedule.put`).
public struct ScheduleSpec: Sendable, Hashable {
    /// A UUID the client made: put again, it replaces the same one.
    public var id: String
    public var prompt: String
    public var target: ScheduleTarget
    public var when: ScheduleTiming
    public var zone: String

    public init(id: String, prompt: String, target: ScheduleTarget, when: ScheduleTiming, zone: String) {
        self.id = id
        self.prompt = prompt
        self.target = target
        self.when = when
        self.zone = zone
    }

    var json: JSON {
        ["id": .string(id), "prompt": .string(prompt), "target": target.json, "when": when.json, "zone": .string(zone)]
    }
}

extension Request {
    /// Make a scheduled prompt, or change the one with its id.
    public static func putSchedule(_ spec: ScheduleSpec) -> Request { Request(method: "schedule.put", params: spec.json) }
    /// Pause it, or let it run again from now.
    public static func pauseSchedule(_ id: String, paused: Bool) -> Request { Request(method: "schedule.pause", params: ["schedule": .string(id), "paused": .bool(paused)]) }
    public static func deleteSchedule(_ id: String) -> Request { Request(method: "schedule.delete", params: .string(id)) }
    /// Run it now, leaving its next run as it is.
    public static func runSchedule(_ id: String) -> Request { Request(method: "schedule.run", params: .string(id)) }
}

extension Note {
    /// `schedule_failed`: a scheduled prompt didn't run, and why (said once for a run of
    /// failures alike).
    public var scheduleFailed: (schedule: String, prompt: String, thread: String?, why: String)? {
        guard kind == "schedule_failed", let s = data["schedule"]?.string else { return nil }
        return (s, data.optStr("prompt") ?? "", data.optStr("thread"), data.optStr("why") ?? "")
    }
}

/// The words and times rows use, as `flotilla_model::schedule` says them.
public enum Schedules {
    static let dayNames = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]

    public static let posix = Locale(identifier: "en_US_POSIX")

    /// Minutes after midnight as `09:30` (Rust's words), or as `locale` writes a time ("9:30 AM").
    public static func clock(_ at: Int, locale: Locale = posix) -> String {
        if locale == posix { return String(format: "%02d:%02d", (at / 60) % 24, at % 60) }
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        guard let d = c.date(from: DateComponents(year: 2000, month: 1, day: 1, hour: (at / 60) % 24, minute: at % 60)) else { return "" }
        return format(Int64(d.timeIntervalSince1970 * 1000), DateFormatter.dateFormat(fromTemplate: "jmm", options: 0, locale: locale) ?? "HH:mm", "UTC", locale: locale)
    }

    /// Its timing in words: "Daily at 09:00", "Mon, Wed and Fri at 18:30", "Every 3 hours",
    /// "0 9 * * 1-5 (cron)"; a one-off is "Once".
    public static func describe(_ when: ScheduleTiming, locale: Locale = posix) -> String {
        let clock = { (at: Int) in Self.clock(at, locale: locale) }
        switch when {
        case .once: return "Once"
        case .hours(1, _): return "Every hour"
        case .hours(let every, _): return "Every \(every) hours"
        case .daily(let at): return "Daily at \(clock(at))"
        case .weekdays(let at): return "Weekdays at \(clock(at))"
        case .weekly(let days, let at):
            let days = Array(Set(days.filter { (1...7).contains($0) })).sorted()
            switch days {
            case [1, 2, 3, 4, 5, 6, 7]: return "Daily at \(clock(at))"
            case [1, 2, 3, 4, 5]: return "Weekdays at \(clock(at))"
            case let one where one.count == 1: return "\(dayNames[one[0] - 1])s at \(clock(at))"
            default:
                let names = days.map { String(dayNames[$0 - 1].prefix(3)) }
                return "\(names.dropLast().joined(separator: ", ")) and \(names.last ?? "") at \(clock(at))"
            }
        case .cron(let expr): return "\(expr.split(separator: " ", omittingEmptySubsequences: true).joined(separator: " ")) (cron)"
        }
    }

    static func calendar(_ zone: String) -> Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: zone) ?? .current
        c.locale = Locale(identifier: "en_US_POSIX")
        return c
    }

    static func format(_ ms: Int64, _ pattern: String, _ zone: String, locale: Locale = posix) -> String {
        let f = DateFormatter()
        f.locale = locale
        f.timeZone = TimeZone(identifier: zone) ?? .current
        f.dateFormat = pattern
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }

    /// A moment in words beside `nowMs`, in `zone`: "today 09:00", "tomorrow 09:00",
    /// "yesterday 18:30", "Fri 09:00" (this week), else "Mon 9 Nov 09:00" (with the year
    /// when it isn't this one's).
    public static func whenWords(_ ms: Int64, now nowMs: Int64, zone: String, locale: Locale = posix) -> String {
        let cal = calendar(zone)
        let t = Date(timeIntervalSince1970: Double(ms) / 1000), now = Date(timeIntervalSince1970: Double(nowMs) / 1000)
        let days = cal.dateComponents([.day], from: cal.startOfDay(for: now), to: cal.startOfDay(for: t)).day ?? 0
        let time = locale == posix ? format(ms, "HH:mm", zone) : format(ms, DateFormatter.dateFormat(fromTemplate: "jmm", options: 0, locale: locale) ?? "HH:mm", zone, locale: locale)
        let day = { (template: String) in format(ms, locale == posix ? template : (DateFormatter.dateFormat(fromTemplate: template.replacingOccurrences(of: " ", with: ""), options: 0, locale: locale) ?? template), zone, locale: locale) }
        switch days {
        case 0: return "today \(time)"
        case 1: return "tomorrow \(time)"
        case -1: return "yesterday \(time)"
        case 2...6: return "\(day("EEE")) \(time)"
        default:
            let sameYear = cal.component(.year, from: t) == cal.component(.year, from: now)
            return "\(day(sameYear ? "EEE d MMM" : "EEE d MMM yyyy")) \(time)"
        }
    }

    /// Its timing and next run in words, the times in `here` (this device's zone): "Daily at
    /// 09:00 · next tomorrow 09:00", "Once · tomorrow 09:00", "Weekdays at 09:00 · paused".
    /// A recurring time names its zone when it isn't `here`.
    public static func summary(_ s: Schedule, now: Int64, here: String, locale: Locale = posix) -> String {
        let timing: String
        let whenWords = { (ms: Int64, now: Int64, zone: String) in Self.whenWords(ms, now: now, zone: zone, locale: locale) }
        switch s.when {
        case .once, .hours: timing = describe(s.when, locale: locale)
        default: timing = s.zone == here ? describe(s.when, locale: locale) : "\(describe(s.when, locale: locale)) (\(s.zone))"
        }
        let state: String
        if s.paused {
            state = "paused"
        } else if s.waitingMs != nil {
            state = "waiting for the turn to end"
        } else if let next = s.nextMs {
            state = s.when.isOnce ? whenWords(next, now, here) : "next \(whenWords(next, now, here))"
        } else if let l = s.last {
            state = l.outcome == .sent ? "sent \(whenWords(l.atMs, now, here))" : "didn't run \(whenWords(l.atMs, now, here))"
        } else {
            state = "done"
        }
        return "\(timing) · \(state)"
    }

    /// Its last run in words, if one is worth saying.
    public static func lastWords(_ s: Schedule, now: Int64, here: String, locale: Locale = posix) -> String? {
        guard let l = s.last else { return nil }
        let at = whenWords(l.atMs, now: now, zone: here, locale: locale)
        let why = l.why.map { w in w.prefix(1).lowercased() + w.dropFirst() }
        switch (l.outcome, why) {
        case (.sent, _) where s.when.isOnce && s.nextMs == nil: return nil
        case (.sent, _): return "Last sent \(at)"
        case (.missed, let w?): return "Missed \(at): \(w)"
        case (.missed, nil): return "Missed \(at)"
        case (.failed, let w?): return "Didn't run \(at): \(w)"
        case (.failed, nil): return "Didn't run \(at)"
        }
    }

    /// A wall-clock time in `zone` as ms (a skipped time shifted past the gap, as the server
    /// does).
    public static func localMs(_ zone: String, year: Int, month: Int, day: Int, minutes: Int) -> Int64? {
        let cal = calendar(zone)
        guard let d = cal.date(from: DateComponents(year: year, month: month, day: day, hour: minutes / 60, minute: minutes % 60)) else { return nil }
        return Int64((d.timeIntervalSince1970 * 1000).rounded())
    }

    /// Its first run after `afterMs`, worked out here for the form (the server counts it
    /// again). None for a cron: the server reads those.
    public static func nextAfter(_ when: ScheduleTiming, zone: String, after afterMs: Int64) -> Int64? {
        switch when {
        case .once(let at): return at > afterMs ? at : nil
        case .hours(let every, let from):
            let step = Int64(max(every, 1)) * 3_600_000
            if from > afterMs { return from }
            return from + ((afterMs - from) / step + 1) * step
        case .cron: return nil
        case .daily(let at), .weekdays(let at), .weekly(_, let at):
            let cal = calendar(zone)
            let days: Set<Int> = switch when {
            case .weekdays: [1, 2, 3, 4, 5]
            case .weekly(let d, _): Set(d)
            default: [1, 2, 3, 4, 5, 6, 7]
            }
            var day = cal.startOfDay(for: Date(timeIntervalSince1970: Double(afterMs) / 1000))
            for _ in 0..<15 {
                // ISO: Monday 1 … Sunday 7 (Calendar's weekday is Sunday 1 … Saturday 7).
                let iso = (cal.component(.weekday, from: day) + 5) % 7 + 1
                let c = cal.dateComponents([.year, .month, .day], from: day)
                if days.contains(iso), let ms = localMs(zone, year: c.year ?? 0, month: c.month ?? 0, day: c.day ?? 0, minutes: at), ms > afterMs {
                    return ms
                }
                guard let next = cal.date(byAdding: .day, value: 1, to: day) else { return nil }
                day = next
            }
            return nil
        }
    }
}
