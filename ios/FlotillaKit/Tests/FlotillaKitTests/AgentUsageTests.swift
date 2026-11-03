import Foundation
import Testing
@testable import FlotillaKit

@Suite struct AgentUsageReading {
    /// The view as the server writes it (`flotilla_proto::spend`): one priced hour at Fast, one
    /// with no known price.
    @Test func `the agents' usage reads from its view`() throws {
        let data = try JSON(parsing: #"""
        {"hours": [
          {"hour_ms": 1793538000000, "provider": "claude-code", "model": "claude-haiku-5-5", "speed": "fast",
           "tokens": {"input": 2, "cache_read": 15615, "cache_write": 4792, "output": 91, "reasoning": 0}, "requests": 1,
           "cost": {"input": 0.25, "cache_read": 0.125, "cache_write": 0.5, "output": 1.5, "other": 0.0, "premium": 1.25, "saved": 0.0625}},
          {"hour_ms": 1793541600000, "provider": "codex", "model": "gpt-x", "speed": "standard",
           "tokens": {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0, "reasoning": 0}, "requests": 2, "cost": null}],
         "read_ms": 1793540000000, "prices_ms": null}
        """#)
        guard case .agentUsage(let u) = try ServerView(key: .agentUsage, data: data) else { Issue.record("not the agents' usage"); return }
        #expect(u.hours.count == 2 && u.readMs == 1_793_540_000_000 && u.pricesMs == nil)
        #expect(u.hours[0].speed == .fast && u.hours[0].tokens.total == 20_500 && u.hours[0].cost?.total == 2.375)
        #expect(u.hours[1].cost == nil && u.hours[1].requests == 2)
        // An older server's view that doesn't say: empty, not a failure.
        #expect(try AgentUsage(json: JSON(parsing: "{}")).hours.isEmpty)
        #expect(ViewKey(fields: ["view": "agent_usage"]) == .agentUsage)
        #expect(ViewKey.devices < .agentUsage)
    }

    /// As `flotilla_model::spend`'s test: the last 30 days, by model, the 31st day out.
    @Test func `it adds up the last thirty days by model`() {
        let day: Int64 = 86_400_000
        let today: Int64 = 100
        func hour(_ d: Int64, _ provider: String, _ model: String, _ speed: Speed, _ tokens: Int64, _ cost: Double?) -> UsageHour {
            UsageHour(hourMs: d * day + 3_600_000, provider: provider, model: model, speed: speed, tokens: TokenTotals(input: tokens), requests: 1,
                      cost: cost.map { UsageCost(input: $0, premium: speed == .standard ? 0 : $0 / 2, saved: $0 / 10) })
        }
        let u = AgentUsage(hours: [
            hour(today - 30, "codex", "gpt-6-luna", .standard, 999, 9),
            hour(today - 29, "codex", "gpt-6-luna", .standard, 100, 1),
            hour(today, "claude-code", "claude-opus-5-5", .fast, 200, 4),
            hour(today, "claude-code", "claude-opus-5-5", .standard, 300, 2),
            hour(today - 3, "opencode", "big-pickle", .standard, 5_000, nil),
        ])
        let s = Spend.summarize(u, today: today, dayOf: { $0 / day })
        #expect(s.tokens.total == 5_600 && s.requests == 4 && s.unpriced == 5_000 && s.days.count == 30)
        #expect(s.cost?.total == 7 && s.cost?.premium == 2 && s.fast == 4)
        var unknown = u
        unknown.hours[2].cost?.premium = nil
        #expect(Spend.summarize(unknown, today: today, dayOf: { $0 / day }).cost?.premium == .some(nil))
        #expect(s.models.map(\.model) == ["claude-opus-5-5", "gpt-6-luna", "big-pickle"])
        #expect(s.models.map { $0.cost?.total } == [6, 1, nil])
        #expect(s.days[0].day == today - 29 && s.days[0].cost == 1 && s.days[29].cost == 6 && s.days[29].tokens == 500)
        let one = Spend.summarize(u, today: today, dayOf: { $0 / day }, only: ("opencode", "big-pickle"))
        #expect(one.cost == nil && one.unpriced == 5_000 && one.days[26].tokens == 5_000 && one.models.count == 1)
        #expect(one.models[0].perMillion == nil)
        #expect(s.models[0].perMillion == 6.0 / 500 * 1e6)
        #expect(!Spend.summarize(AgentUsage(hours: []), today: today, dayOf: { $0 / day }).any)
    }

    @Test func `dollars, tokens and cache hits are written as Rust writes them`() {
        #expect(Spend.usd(1234.567) == "$1,234.57")
        #expect(Spend.usd(0.42) == "$0.42")
        #expect(Spend.usd(0.004) == "<$0.01")
        #expect(Spend.usd(0) == "$0.00")
        #expect(Spend.tokens(568_300_000) == "568m")
        #expect(Spend.tokens(141_900_000) == "142m")
        #expect(Spend.tokens(45_800_000) == "45.8m")
        #expect(Spend.tokens(9_530) == "9.53k")
        #expect(Spend.tokens(2_000_000) == "2m")
        #expect(Spend.tokens(950) == "950")
        #expect(Spend.cacheHit(TokenTotals(input: 10, cacheRead: 80, cacheWrite: 10)) == 0.8)
        #expect(Spend.cacheHit(TokenTotals(output: 5)) == nil)
        #expect(Spend.usagePage("claude")?.site == "claude.ai" && Spend.usagePage("opencode") == nil)
    }
}
