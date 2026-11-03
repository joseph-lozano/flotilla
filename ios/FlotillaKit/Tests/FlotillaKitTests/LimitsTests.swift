import Foundation
import Testing
@testable import FlotillaKit

@Suite struct LimitsReading {
    /// The views as the live run's server sent them on 2026-11-01 (Claude Code on Haiku 5.5,
    /// and the Claude and ChatGPT accounts' limits), trimmed to what these read.
    @Test func `a thread's context and the accounts' limits read from the views`() throws {
        let thread = try JSON(parsing: #"""
        {"id": "98366a89", "thread": null, "messages": [], "sent": [],
         "context": {"used": 23767, "window": 1000000, "model": "claude-haiku-5-5", "account": "claude", "at_ms": 1793539520284}}
        """#)
        let d = try ThreadDetail(json: thread)
        #expect(d.context == ContextUse(used: 23_767, window: 1_000_000, model: "claude-haiku-5-5", account: "claude", atMs: 1_793_539_520_284))
        #expect(d.context?.percent.map(Limits.percent) == "2.4%")
        // An older server's view: no context, nothing shown.
        #expect(try ThreadDetail(json: JSON(parsing: #"{"id": "t", "messages": []}"#)).context == nil)
        // Before the window is known: the count alone.
        #expect(try ThreadDetail(json: JSON(parsing: #"{"id": "t", "context": {"used": 900, "window": null}}"#)).context?.percent == nil)

        let sidebar = try JSON(parsing: #"""
        {"threads": [], "limits": [
          {"account": "chatgpt", "plan": "pro", "windows": [{"id": "seven_day", "label": "Weekly", "used": 10, "resets_at": "2026-11-04T19:09:31Z", "minutes": 10080}], "read_ms": 1793539513422},
          {"account": "claude", "plan": null, "windows": [
            {"id": "seven_day", "label": "Weekly", "used": 92, "resets_at": "2026-11-01T15:00:00Z", "minutes": 10080},
            {"id": "seven_day_fable", "label": "Weekly · Fable", "used": 25, "resets_at": "2026-11-01T15:00:00Z", "minutes": 10080, "scope": "Fable"}], "read_ms": 1793539511262}
        ]}
        """#)
        let s = try Sidebar(json: sidebar)
        #expect(s.limits.map(\.name) == ["ChatGPT", "Claude"])
        #expect(s.limits[1].windows.map(\.label) == ["Weekly", "Weekly · Fable"])
        #expect(s.limits[1].windows.map(\.scope) == [nil, "Fable"])
        #expect(s.limits[0].windows[0].resetsMs == 1_793_819_371_000)
        #expect(try Sidebar(json: JSON(parsing: #"{"threads": []}"#)).limits.isEmpty)
    }

    @Test func `counts are written as T3 writes them`() {
        #expect(Limits.tokens(950) == "950")
        #expect(Limits.tokens(9_500) == "9.5k")
        #expect(Limits.tokens(9_000) == "9k")
        #expect(Limits.tokens(21_909) == "22k")
        #expect(Limits.tokens(828_400) == "828k")
        #expect(Limits.tokens(999_600) == "1m")
        #expect(Limits.tokens(1_260_000) == "1.3m")
        #expect(Limits.exact(21_909) == "21,909")
        #expect(Limits.exact(1_000_000) == "1,000,000")
        #expect(Limits.exact(7) == "7")
        #expect(Limits.percent(2.38) == "2.4%")
        #expect(Limits.percent(2.0) == "2%")
        #expect(Limits.percent(42.6) == "43%")
        #expect(Limits.lit(0, cells: 10) == 0 && Limits.lit(0.1, cells: 10) == 1 && Limits.lit(99.9, cells: 10) == 9 && Limits.lit(100, cells: 10) == 10)
        #expect(Limits.contextTone(80) == .warn && Limits.contextTone(90) == .bad && Limits.limitTone(92) == .warn && Limits.limitTone(100) == .bad)
    }

    @Test func `a window whose reset passed isn't shown, and a used-up one stops the thread's account`() {
        let now: Int64 = 1_793_541_600_000 // 2026-11-01T14:00:00Z
        let claude = AccountLimits(account: "claude", windows: [
            LimitWindow(id: "seven_day", label: "Weekly", used: 92, resetsAt: "2026-11-01T15:00:00Z"),
            LimitWindow(id: "five_hour", label: "5-hour", used: 100, resetsAt: "2026-11-01T13:00:00Z"),
        ], readMs: now)
        let chatgpt = AccountLimits(account: "chatgpt", windows: [LimitWindow(id: "seven_day", label: "Weekly", used: 10, resetsAt: "2026-11-04T19:09:31Z")], readMs: now)
        #expect(claude.windows(now: now).map(\.id) == ["seven_day"])
        #expect(Limits.tightest([chatgpt, claude], now: now).map { "\($0.0.account) \($0.1.used)" } == "claude 92")
        // A tie keeps the first, as the Mac picks.
        let tied = AccountLimits(account: "chatgpt", windows: [LimitWindow(id: "seven_day", label: "Weekly", used: 92)], readMs: now)
        #expect(Limits.tightest([tied, claude], now: now)?.0.account == "chatgpt")
        let thread = ContextUse(used: 1, account: "claude")
        #expect(Limits.reached([claude], context: thread, now: now) == nil)
        var full = claude
        full.windows[0].used = 100
        #expect(Limits.reached([full], context: thread, now: now)?.1.id == "seven_day")
        #expect(Limits.reached([full], context: ContextUse(used: 1, account: "chatgpt"), now: now) == nil)
        // A model's own window used up stops only threads on that model.
        let fable = AccountLimits(account: "claude", windows: [LimitWindow(id: "seven_day_fable", label: "Weekly · Fable", used: 100, resetsAt: "2026-11-01T15:00:00Z", scope: "Fable")], readMs: now)
        #expect(Limits.reached([fable], context: ContextUse(used: 1, model: "claude-haiku-5-5", account: "claude"), now: now) == nil)
        #expect(Limits.reached([fable], context: ContextUse(used: 1, model: "claude-fable-5-1[1m]", account: "claude"), now: now) != nil)
        #expect(Limits.until(now + 2 * 3_600_000 + 13 * 60_000, now: now) == "2h 13m")
        #expect(Limits.until(now + 4 * 86_400_000 + 6 * 3_600_000, now: now) == "4d 6h")
        #expect(Limits.until(now + 30_000, now: now) == "under a minute")
    }

    /// As Rust's `only_weekly_windows_are_shown`: a 5-hour window, however full, is left out of
    /// the readout, its details and "limit reached", whether its length is told or only its id.
    @Test func `only weekly windows are shown`() {
        let now: Int64 = 1_793_541_600_000 // 2026-11-01T14:00:00Z
        let claude = AccountLimits(account: "claude", windows: [
            LimitWindow(id: "five_hour", label: "5-hour", used: 100, resetsAt: "2026-11-01T15:00:00Z", minutes: 300),
            LimitWindow(id: "primary", label: "5-hour", used: 97, resetsAt: "2026-11-01T15:00:00Z", minutes: 300),
            LimitWindow(id: "five_hour", label: "5-hour", used: 99, resetsAt: "2026-11-01T15:00:00Z"),
            LimitWindow(id: "seven_day", label: "Weekly", used: 8, resetsAt: "2026-11-05T15:00:00Z", minutes: 10080),
        ], readMs: now)
        #expect(claude.windows(now: now).map(\.used) == [8])
        #expect(Limits.tightest([claude], now: now)?.1.used == 8)
        #expect(Limits.reached([claude], context: ContextUse(used: 1, account: "claude"), now: now) == nil)
    }
}
