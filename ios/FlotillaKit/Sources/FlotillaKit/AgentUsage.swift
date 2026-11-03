import Foundation

/// What the agents spent (`flotilla_proto::spend`): their tokens hour by hour, as their
/// harnesses recorded each request in the thread's VM, and what they'd cost at the API's list
/// prices (T3 Code's estimate, not the subscription's bill). `Spend` adds them up as
/// `flotilla_model::spend` does: the last 30 days in this phone's own days. A model with no known
/// price has tokens and no cost.

/// The speed a request ran at: Fast and Ultrafast cost more than Standard.
public enum Speed: String, Sendable, Hashable {
    case standard, fast, ultrafast
}

/// Tokens by kind, as T3 counts them: input is only what wasn't read from the cache; output
/// includes reasoning.
public struct TokenTotals: Sendable, Hashable {
    public var input: Int64 = 0
    public var cacheRead: Int64 = 0
    public var cacheWrite: Int64 = 0
    public var output: Int64 = 0
    public var reasoning: Int64 = 0

    public init(input: Int64 = 0, cacheRead: Int64 = 0, cacheWrite: Int64 = 0, output: Int64 = 0, reasoning: Int64 = 0) {
        self.input = input
        self.cacheRead = cacheRead
        self.cacheWrite = cacheWrite
        self.output = output
        self.reasoning = reasoning
    }

    init(json j: JSON) {
        self.init(input: j.optInt("input") ?? 0, cacheRead: j.optInt("cache_read") ?? 0, cacheWrite: j.optInt("cache_write") ?? 0,
                  output: j.optInt("output") ?? 0, reasoning: j.optInt("reasoning") ?? 0)
    }

    /// Every token processed (reasoning is in output).
    public var total: Int64 { input + cacheRead + cacheWrite + output }

    mutating func add(_ o: TokenTotals) {
        input += o.input
        cacheRead += o.cacheRead
        cacheWrite += o.cacheWrite
        output += o.output
        reasoning += o.reasoning
    }
}

/// An estimated cost in dollars, by kind of token. `other`: a cost the harness gave for a model
/// with no public price, which can't be split. `premium`: what Fast and Ultrafast added over
/// Standard, nil when that can't be known; `saved`: what reading from the cache saved. Neither
/// is in the total again.
public struct UsageCost: Sendable, Hashable {
    public var input: Double = 0
    public var cacheRead: Double = 0
    public var cacheWrite: Double = 0
    public var output: Double = 0
    public var other: Double = 0
    public var premium: Double? = 0
    public var saved: Double = 0

    public init(input: Double = 0, cacheRead: Double = 0, cacheWrite: Double = 0, output: Double = 0, other: Double = 0, premium: Double? = 0, saved: Double = 0) {
        self.input = input
        self.cacheRead = cacheRead
        self.cacheWrite = cacheWrite
        self.output = output
        self.other = other
        self.premium = premium
        self.saved = saved
    }

    init(json j: JSON) {
        let n = { (k: String) in j[k]?.double ?? 0 }
        self.init(input: n("input"), cacheRead: n("cache_read"), cacheWrite: n("cache_write"), output: n("output"), other: n("other"), premium: j["premium"]?.double, saved: n("saved"))
    }

    public var total: Double { input + cacheRead + cacheWrite + output + other }

    mutating func add(_ o: UsageCost) {
        input += o.input
        cacheRead += o.cacheRead
        cacheWrite += o.cacheWrite
        output += o.output
        other += o.other
        // Unknown in a part is unknown in the whole.
        premium = premium.flatMap { a in o.premium.map { a + $0 } }
        saved += o.saved
    }

    /// Each kind with its share of the ink, as T3 mixes it: Input, Cache read, Cache write,
    /// Output, Other.
    public var kinds: [(label: String, value: Double, ink: Double)] {
        [("Input", input, 0.6), ("Cache read", cacheRead, 0.3), ("Cache write", cacheWrite, 0.72), ("Output", output, 1), ("Other", other, 0.44)]
    }
}

/// A quarter hour's use of one model at one speed.
public struct UsageHour: Sendable, Hashable {
    /// The quarter hour's start (ms since the epoch, UTC).
    public var hourMs: Int64
    /// "claude-code", "codex" or "opencode".
    public var provider: String
    public var model: String
    public var speed: Speed
    public var tokens: TokenTotals
    public var requests: Int64
    /// None for a model with no known price.
    public var cost: UsageCost?

    public init(hourMs: Int64, provider: String, model: String, speed: Speed = .standard, tokens: TokenTotals, requests: Int64 = 1, cost: UsageCost? = nil) {
        self.hourMs = hourMs
        self.provider = provider
        self.model = model
        self.speed = speed
        self.tokens = tokens
        self.requests = requests
        self.cost = cost
    }

    init(json j: JSON) throws(ReadError) {
        hourMs = j.optInt("hour_ms") ?? 0
        provider = try j.str("provider")
        model = try j.str("model")
        speed = j.optStr("speed").flatMap(Speed.init(rawValue:)) ?? .standard
        tokens = TokenTotals(json: j["tokens"] ?? .null)
        requests = j.optInt("requests") ?? 0
        cost = j["cost"].flatMap { $0.object != nil ? UsageCost(json: $0) : nil }
    }
}

/// The view: the last 31 days' hours, oldest first.
public struct AgentUsage: Sendable, Hashable {
    public var hours: [UsageHour]
    /// When the VMs were last read.
    public var readMs: Int64?
    /// When the model prices were fetched, if they have been.
    public var pricesMs: Int64?

    public init(hours: [UsageHour], readMs: Int64? = nil, pricesMs: Int64? = nil) {
        self.hours = hours
        self.readMs = readMs
        self.pricesMs = pricesMs
    }

    public init(json j: JSON) throws(ReadError) {
        guard j.object != nil else { throw ReadError("not an agent usage view") }
        hours = try j.list("hours").map(UsageHour.init(json:))
        readMs = j.optInt("read_ms")
        pricesMs = j.optInt("prices_ms")
    }
}

/// How the phone adds up what the agents spent, as `flotilla_model::spend`.
public enum Spend {
    /// T3's default period.
    public static let days = 30

    public struct ModelSum: Sendable, Hashable {
        public var provider: String
        public var model: String
        public var tokens = TokenTotals()
        /// None when none of its requests is priced.
        public var cost: UsageCost?
        public var requests: Int64 = 0

        /// What a million of its tokens cost, when it's priced.
        public var perMillion: Double? {
            guard let cost, tokens.total > 0 else { return nil }
            return cost.total / Double(tokens.total) * 1e6
        }
    }

    public struct DaySum: Sendable, Hashable {
        /// The day, as `summarize`'s `dayOf` counts them.
        public var day: Int64
        public var cost: Double = 0
        public var tokens: Int64 = 0
    }

    public struct Summary: Sendable, Hashable {
        public var tokens = TokenTotals()
        /// What the priced requests cost; nil when none is.
        public var cost: UsageCost?
        /// All that the Fast and Ultrafast requests cost.
        public var fast: Double = 0
        public var requests: Int64 = 0
        /// Tokens of models with no known price.
        public var unpriced: Int64 = 0
        /// The most costly first, then the most tokens.
        public var models: [ModelSum] = []
        /// Oldest first.
        public var days: [DaySum] = []

        public init() {}

        /// Some use in the period.
        public var any: Bool { tokens.total > 0 }
    }

    /// The last 30 days up to `today` (a day number `dayOf` would give), for every model or
    /// `only` one (its harness and name).
    public static func summarize(_ u: AgentUsage, today: Int64, dayOf: (Int64) -> Int64, only: (provider: String, model: String)? = nil) -> Summary {
        let first = today - Int64(days) + 1
        var s = Summary()
        s.days = (first...today).map { DaySum(day: $0) }
        for h in u.hours where only.map({ h.provider == $0.provider && h.model == $0.model }) ?? true {
            let day = dayOf(h.hourMs)
            guard day >= first, day <= today else { continue }
            s.tokens.add(h.tokens)
            s.requests += h.requests
            if let c = h.cost {
                s.cost = (s.cost ?? UsageCost()).adding(c)
                if h.speed != .standard { s.fast += c.total }
            } else {
                s.unpriced += h.tokens.total
            }
            let i = Int(day - first)
            s.days[i].tokens += h.tokens.total
            s.days[i].cost += h.cost?.total ?? 0
            let m = s.models.firstIndex { $0.provider == h.provider && $0.model == h.model } ?? {
                s.models.append(ModelSum(provider: h.provider, model: h.model))
                return s.models.count - 1
            }()
            s.models[m].tokens.add(h.tokens)
            s.models[m].requests += h.requests
            if let c = h.cost { s.models[m].cost = (s.models[m].cost ?? UsageCost()).adding(c) }
        }
        s.models.sort { a, b in
            let (ca, cb) = (a.cost?.total ?? 0, b.cost?.total ?? 0)
            if ca != cb { return ca > cb }
            if a.tokens.total != b.tokens.total { return a.tokens.total > b.tokens.total }
            return a.model < b.model
        }
        return s
    }

    /// Of the input, the share read from the cache (a cache write is a miss).
    public static func cacheHit(_ t: TokenTotals) -> Double? {
        let input = t.input + t.cacheRead + t.cacheWrite
        return input > 0 ? Double(t.cacheRead) / Double(input) : nil
    }

    /// Dollars: "$1,234.57", "$0.42"; under half a cent, "<$0.01".
    public static func usd(_ v: Double) -> String {
        if v > 0 && v < 0.005 { return "<$0.01" }
        let cents = Int64((v * 100).rounded())
        return "$\(Limits.exact(cents / 100)).\(String(format: "%02lld", cents % 100))"
    }

    /// Tokens as T3's Usage writes them: three figures at most ("568m", "45.8m", "9.53k").
    public static func tokens(_ n: Int64) -> String {
        let v = Double(n)
        let (scaled, unit): (Double, String) = switch n {
        case 1_000_000_000_000...: (v / 1e12, "t")
        case 1_000_000_000...: (v / 1e9, "b")
        case 1_000_000...: (v / 1e6, "m")
        case 1_000...: (v / 1e3, "k")
        default: (v, "")
        }
        if unit.isEmpty { return "\(n)" }
        let digits = scaled >= 100 ? 0 : scaled >= 10 ? 1 : 2
        var s = String(format: "%.\(digits)f", scaled)
        if s.contains(".") {
            while s.hasSuffix("0") { s.removeLast() }
            if s.hasSuffix(".") { s.removeLast() }
        }
        return s + unit
    }

    /// The account whose usage page shows a harness's use.
    public static func accountOf(_ provider: String) -> String? {
        switch provider {
        case "claude-code": "claude"
        case "codex": "chatgpt"
        default: nil
        }
    }

    /// Where each provider shows its account's own usage, and its site's name.
    public static func usagePage(_ account: String) -> (url: URL, site: String)? {
        switch account {
        case "claude": (URL(string: "https://claude.ai/settings/usage")!, "claude.ai")
        case "chatgpt": (URL(string: "https://chatgpt.com/codex/settings/usage")!, "chatgpt.com")
        default: nil
        }
    }
}

extension UsageCost {
    func adding(_ o: UsageCost) -> UsageCost {
        var c = self
        c.add(o)
        return c
    }
}
