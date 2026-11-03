import FlotillaKit
import Foundation

/// The desktop's words and times (`crates/flotilla-app/src/fmt.rs`), for the phone.
enum Format {
    static func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

    /// "now", "4m", "2h", "yday", "3d", else the date ("Oct 3").
    static func ago(_ ms: Int64, now: Int64 = nowMs()) -> String {
        let s = max(now - ms, 0) / 1000
        switch s {
        case ..<60: return "now"
        case ..<3600: return "\(s / 60)m"
        case ..<86400: return "\(s / 3600)h"
        case ..<172_800: return "yday"
        case ..<2_592_000: return "\(s / 86400)d"
        default:
            return Date(timeIntervalSince1970: Double(ms) / 1000).formatted(.dateTime.month(.abbreviated).day())
        }
    }

    /// For VoiceOver: "4 minutes ago".
    static func agoSpoken(_ ms: Int64, now: Int64 = nowMs()) -> String {
        let s = max(now - ms, 0) / 1000
        if s < 60 { return "just now" }
        return Date(timeIntervalSince1970: Double(ms) / 1000).formatted(.relative(presentation: .named))
    }

    /// A running turn's time: "0:41", "12:03", "1:02:03".
    static func elapsed(since ms: Int64, now: Int64 = nowMs()) -> String {
        let s = max(now - ms, 0) / 1000
        if s >= 3600 { return String(format: "%d:%02d:%02d", s / 3600, s / 60 % 60, s % 60) }
        return String(format: "%d:%02d", s / 60, s % 60)
    }

    /// How long until an idle VM stops: "stops soon", "8 min left", "2 h left", "1 h 5 min left".
    static func timeLeft(_ ms: Int64) -> String {
        let minutes = (max(ms, 0) + 59_999) / 60_000
        if minutes == 0 { return "stops soon" }
        if minutes < 60 { return "\(minutes) min left" }
        if minutes % 60 == 0 { return "\(minutes / 60) h left" }
        return "\(minutes / 60) h \(minutes % 60) min left"
    }

    /// A subagent's time: "41s", "2m 05s".
    static func duration(_ ms: Int64) -> String {
        let s = max(ms / 1000, 1)
        return s >= 60 ? String(format: "%dm %02ds", s / 60, s % 60) : "\(s)s"
    }

    static func calls(_ n: Int64) -> String? {
        n <= 0 ? nil : n == 1 ? "1 call" : "\(n) calls"
    }

    static func providerName(_ key: String) -> String {
        switch key {
        case "claude-code": "Claude Code"
        case "codex": "Codex"
        case "pi": "Pi"
        case "prime-agent": "Prime"
        case "opencode": "OpenCode"
        case "kimi": "Kimi"
        case "mistral": "Mistral"
        default: key
        }
    }

    /// "Opus 5.5": the catalog's label, else the id as words (before the catalog comes, too).
    static func modelName(provider: String, model: String, catalog: Catalog?) -> String {
        catalog?.label(provider: provider, model: model) ?? Catalog.readable(model)
    }

    /// "Opus 5.5 · high · fast": the model's label, its effort (none if "none") and fast.
    static func model(_ t: FlotillaKit.Thread, catalog: Catalog?) -> String {
        model(provider: t.provider, model: t.model, effort: t.effort, fast: t.fast, catalog: catalog)
    }

    static func model(provider: String, model: String, effort: String?, fast: Bool, catalog: Catalog?) -> String {
        var parts = [modelName(provider: provider, model: model, catalog: catalog)]
        if let e = effort, !e.isEmpty, e != "none" { parts.append(e) }
        if fast { parts.append("fast") }
        return parts.joined(separator: " · ")
    }

    private static let iso: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private static let isoPlain = ISO8601DateFormatter()

    /// An RFC 3339 time as ms since the epoch.
    static func ms(_ rfc3339: String?) -> Int64? {
        guard let s = rfc3339 else { return nil }
        guard let d = iso.date(from: s) ?? isoPlain.date(from: s) else { return nil }
        return Int64(d.timeIntervalSince1970 * 1000)
    }

    /// Days left before a stopped VM's disk goes (14 days after it stopped).
    static func daysLeft(_ t: FlotillaKit.Thread, now: Int64 = nowMs()) -> Int64? {
        guard t.state == .stopped, let stopped = ms(t.lastStoppedAt) else { return nil }
        return 14 - (now - stopped) / 86_400_000
    }

    /// The first line worth showing of a reply, markdown markers dropped (`util::preview_line`).
    static func previewLine(_ text: String) -> String? {
        guard let line = text.split(whereSeparator: \.isNewline).map({ $0.trimmingCharacters(in: .whitespaces) }).first(where: { !$0.isEmpty }) else { return nil }
        var l = Substring(line)
        while let f = l.first, "#>-* ".contains(f) { l = l.dropFirst() }
        let cleaned = l.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "**", with: "").replacingOccurrences(of: "__", with: "").replacingOccurrences(of: "`", with: "")
        return cleaned.isEmpty ? nil : ellipsize(cleaned, 160)
    }

    /// A report's line for its closed card: the first that says something, not a heading or a
    /// label ("1. `uname -a`:") over what follows; its prose first, else its code. The Mac's
    /// `thread_view::report_preview`.
    static func reportPreview(_ report: String) -> String? {
        var code = false
        var prose: String?
        var firstCode: String?
        for line in report.split(whereSeparator: \.isNewline).map({ $0.trimmingCharacters(in: .whitespaces) }) {
            if line.hasPrefix("```") {
                code.toggle()
                continue
            }
            if line.isEmpty { continue }
            if code {
                firstCode = firstCode ?? line
            } else if !line.hasPrefix("#"), !line.hasSuffix(":") {
                prose = line
                break
            }
        }
        return (prose ?? firstCode).flatMap(previewLine) ?? previewLine(report)
    }

    static func ellipsize(_ s: String, _ n: Int) -> String {
        s.count <= n ? s : String(s.prefix(n)).trimmingCharacters(in: .whitespaces) + "…"
    }

    /// A file's size: "512 B", "48 KB", "3.4 MB", "25 MB" (`fmt::bytes`).
    static func bytes(_ n: Int64) -> String {
        let kb: Int64 = 1024, mb = AttachLimits.mb
        switch n {
        case ..<kb: return "\(n) B"
        case ..<mb: return "\(n / kb) KB"
        case ..<(10 * mb): return String(format: "%.1f MB", Double(n) / Double(mb))
        default: return "\(n / mb) MB"
        }
    }

    static func oneLine(_ s: String) -> String {
        s.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
    }

    /// The plan's VM hours used, as the sidebar's footer shows them: "7/556 VM hours".
    static func usage(_ u: Usage?) -> (used: String, rest: String, tone: Tone)? {
        guard let u else { return nil }
        if u.blocked != nil { return ("Can't start VMs", "", .bad) }
        guard let used = vmHoursUsed(u) else { return nil }
        return ("\(used.used)", "/\(used.limit) VM hours", tone(used))
    }

    /// The plan's VM hours used of those it includes (boat reports those left).
    static func vmHoursUsed(_ u: Usage) -> Count? {
        u.vmHours.map { Count($0.limit - $0.used, of: $0.limit) }
    }

    /// A count against its limit: amber from 80% of it, red at it (`sidebar::tone`).
    static func tone(_ c: Count) -> Tone {
        guard c.limit > 0 else { return .plain }
        return c.used >= c.limit ? .bad : c.used * 5 >= c.limit * 4 ? .warn : .plain
    }

    /// A meter's lit cells of ten: any use lights one, and all ten only at the limit.
    static func litCells(_ c: Count) -> Int {
        guard c.limit > 0, c.used > 0 else { return 0 }
        if c.used >= c.limit { return 10 }
        return Int(min(max((c.used * 10 + c.limit - 1) / c.limit, 1), 9))
    }

    /// When the usage was read: "just now", "4m ago", "yesterday", "Oct 3".
    static func agoPhrase(_ ms: Int64, now: Int64 = nowMs()) -> String {
        switch ago(ms, now: now) {
        case "now": "just now"
        case "yday": "yesterday"
        case let s where s.first?.isNumber == true: "\(s) ago"
        case let s: s
        }
    }

    /// An RFC 3339 time as a local date ("Nov 30") or clock time ("4:52 PM").
    static func date(_ rfc3339: String?) -> String? {
        ms(rfc3339).map { Date(timeIntervalSince1970: Double($0) / 1000).formatted(.dateTime.month(.abbreviated).day()) }
    }

    static func clock(_ rfc3339: String?) -> String? {
        ms(rfc3339).map { Date(timeIntervalSince1970: Double($0) / 1000).formatted(date: .omitted, time: .shortened) }
    }

    /// Cents as dollars: "$80", "$0.29".
    static func dollars(_ cents: Int64) -> String {
        cents % 100 == 0 ? "$\(cents / 100)" : String(format: "$%lld.%02lld", cents / 100, cents % 100)
    }

    enum Tone { case plain, ok, warn, bad, dim }
}
