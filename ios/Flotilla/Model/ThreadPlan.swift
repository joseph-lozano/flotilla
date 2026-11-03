import FlotillaKit
import Foundation

/// A subagent or a hand-off, as its card shows it.
struct Card: Identifiable, Hashable {
    /// Stable across updates: `child:<id>` for a hand-off, `agent:<tool id>` for a Claude Code
    /// subagent, `sub:<id>` for one only its harness's records show.
    var id: String
    var title: String
    /// The model and how it went: "Haiku 5.5 · 3 calls · 41s".
    var meta: [String]
    var running: Bool
    /// A problem in words (coral): "failed", "gone", "interrupted"…
    var problem: String?
    /// Its latest call while it works: (name, summary).
    var latestName: String?
    var latestSummary: String?
    var report: String?
    var task: String?
    var startedMs: Int64?
    var provider: String?
    /// A delegate's own thread, to open.
    var thread: String?
}

/// A message the phone sent that the thread's view doesn't list yet.
struct LocalSend: Hashable {
    let local: String
    let text: String
    let steer: Bool
    let at: Int64
    /// Its files' names.
    var files: [String] = []
}

/// One row of a thread's page.
enum Row: Identifiable, Hashable {
    case prompt(id: String, from: String?, text: String, status: String?, attachments: [String], notKept: Bool)
    case steer(id: String, text: String, status: String?)
    case text(id: String, text: String)
    case tool(id: String, Message)
    /// Calls one after another, folded into one line ("Called Bash 9 times") that opens to
    /// show them; `id` is `calls:` and the first one's.
    case calls(id: String, [Message])
    case notice(id: String, text: String, isError: Bool)
    /// A question the agent asked you (`AskCard`). `answer`: once answered, how the message
    /// that took the answer is going, where it isn't done (that message has no row of its own).
    case ask(id: String, Message, answer: String?)
    case card(Card)
    /// "2 agents answered", standing in for the prompt that brought their answers; `target`:
    /// the card to show.
    case answered(id: String, text: String, target: String?)
    /// The running turn's streaming text.
    case partial(id: String, text: String)

    var id: String {
        switch self {
        case .prompt(let id, _, _, _, _, _), .steer(let id, _, _), .text(let id, _), .tool(let id, _), .calls(let id, _), .notice(let id, _, _), .ask(let id, _, _), .answered(let id, _, _), .partial(let id, _): id
        case .card(let c): c.id
        }
    }

    var isTool: Bool {
        switch self {
        case .tool, .calls: true
        default: false
        }
    }
}

/// Turns a thread's view into the rows its page shows, as the desktop's thread view does
/// (`thread_view.rs`, `subagents.rs`).
enum ThreadPlan {
    /// What a child's first prompt ends with, after the task (`orchestrate::CHILD_NOTE`).
    static let childNote = "\n\n(Another agent handed you this task; your final message goes back to it.)"
    static let promptNotKept = "(original prompt not kept by boat)"

    struct Plan {
        var rows: [Row]
        /// Running subagents and hand-offs, pinned under the header.
        var pinned: [Card]
        /// Which row shows each message, by seq: its own, or the card it's folded into.
        var at: [Int64: String] = [:]

        /// The row to go to for the message `seq` (a search hit): its own, the card it's in, or
        /// (a reply said in its subagents' cards) the nearest row before it.
        func row(for seq: Int64) -> String? {
            if let id = at[seq] { return id }
            return at.filter { $0.key < seq }.max { $0.key < $1.key }?.value
        }
    }

    static func plan(_ d: ThreadDetail, summary: ThreadSummary?, partials: [RawEvent], local: [LocalSend], catalog: Catalog?, schedules: [Schedule] = []) -> Plan {
        // Prompts a schedule sent: by their id, what says so ("Scheduled · Daily at 09:00").
        let scheduled: [String: String] = Dictionary(d.sent.compactMap { p in
            p.schedule.map { id in (p.id, schedules.first { $0.id == id }.map { "Scheduled · \(Schedules.describe($0.when, locale: .current))" } ?? "Scheduled") }
        }, uniquingKeysWith: { a, _ in a })
        let working = summary?.activity == .working
        var messages = d.messages
        // What this phone sent that the view doesn't list yet shows as sending.
        let listed = Set(d.sent.map(\.id))
        var seq = (messages.last?.seq ?? 0) + 1
        for l in local where !listed.contains(l.local) && !messages.contains(where: { $0.taskID == l.local }) {
            var m = Message(seq: seq, kind: l.steer ? "steer" : "prompt", text: l.text, status: "sending", taskID: l.local, eventKey: l.local, timestamp: l.at)
            m.attachments = l.files
            messages.append(m)
            seq += 1
        }
        if summary?.startFailed == true {
            for i in messages.indices where messages[i].status == "sending" { messages[i].status = "not sent" }
        }

        // Subagents its harness recorded (by their place in `d.subagents`): by the call that
        // started each, and the calls they own.
        let subs = d.subagents
        var byHost: [String: Int] = [:]
        var owned: [String: String] = [:]
        var listing: [String: [Int]] = [:]
        for (i, s) in subs.enumerated() {
            if let host = s.parentCall { byHost[host] = i }
            for call in s.calls {
                owned[call] = s.id
                if listing[call]?.last != i { listing[call, default: []].append(i) }
            }
        }
        // Each subagent's own calls, in order: one pass, not one per card.
        var own = Array(repeating: [Message](), count: subs.count)
        for m in messages {
            guard let call = m.toolID, let mine = listing[call] else { continue }
            for i in mine { own[i].append(m) }
        }
        let tools = Dictionary(messages.compactMap { m in m.toolID.map { ($0, m) } }, uniquingKeysWith: { a, _ in a })
        let children = Dictionary(d.children.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        // What subagents said, which a harness (Codex) also puts in the parent's reply in the
        // same turn: by turn.
        let turn = turns(messages)
        let subTurn = turnsOf(subs, own: own, messages, turn)
        let said = saidByTurn(subs, subTurn)
        let provider = d.thread?.provider

        var rows: [Row] = []
        /// Each row's time, and whether it starts a turn, for placing recorded subagents.
        var stamps: [(at: Int64?, prompt: Bool)] = []
        var at: [Int64: String] = [:]
        func add(_ row: Row, _ m: Message) {
            rows.append(row)
            stamps.append((m.timestamp, m.kind == "prompt"))
            if at[m.seq] == nil { at[m.seq] = row.id }
        }
        // The messages that took your answers to the questions whose cards are here: each card
        // says how its answer's going, in its place. (One whose card is on a page not loaded yet
        // still shows.)
        let cards = Set(messages.filter { $0.kind == "ask" }.compactMap(\.toolID))
        var answers: [String: String] = [:]
        for m in messages where tookAnswer(m, cards: cards) {
            if let ask = m.answerTo, let s = answerStatus(m) { answers[ask] = s }
        }
        // The card each subagent's calls fold into: its `Agent` call's, or its own.
        let cardOf = Dictionary(d.subagents.map { s in (s.id, s.parentCall.flatMap { tools[$0] != nil ? "agent:\($0)" : nil } ?? "sub:\(s.id)") }, uniquingKeysWith: { a, _ in a })
        var placed = Set<String>()
        for m in messages {
            let id = "m\(m.seq)"
            // Your answer to a question: its card shows it, and a search for it goes there. (The
            // message still starts its turn: `turns` counts it.)
            if tookAnswer(m, cards: cards), let ask = m.answerTo {
                at[m.seq] = "ask:\(ask)"
                continue
            }
            // Flotilla's own, as a prompt or, into a running turn, a steer: the same either way.
            // (A server from before it marked them marks none: its prompts are known by their
            // text, as they were.)
            if m.fromFlotilla || m.kind == "prompt", let row = flotillas(m.text ?? "", id: id) {
                add(row, m)
                continue
            }
            switch m.kind {
            case "prompt":
                let text = m.text ?? ""
                if let parent = d.parent, let task = taskOf(text) {
                    add(.prompt(id: id, from: "From \(parent.1)", text: task, status: m.status, attachments: m.attachments, notKept: false), m)
                } else {
                    let from = m.taskID.flatMap { scheduled[$0] }
                    add(.prompt(id: id, from: from, text: text, status: m.status, attachments: m.attachments, notKept: text == promptNotKept), m)
                }
            case "steer":
                add(.steer(id: id, text: m.text ?? "", status: m.status), m)
            case "ask":
                // By its question, not its place: a reply arriving moves it, and its card keeps
                // what you picked and typed.
                add(.ask(id: "ask:\(m.toolID ?? id)", m, answer: m.toolID.flatMap { answers[$0] }), m)
            case "text":
                guard let text = m.text.map({ Self.without(said[turn[m.seq] ?? 0] ?? [], in: $0) }), !text.isEmpty else {
                    // All of it is what a subagent said: its card shows it.
                    let raw = m.text ?? ""
                    let by = subs.indices.first { i in
                        subTurn[i] == turn[m.seq] && subs[i].texts.contains { t in let t = t.trimmingCharacters(in: .whitespacesAndNewlines); return !t.isEmpty && raw.contains(t) }
                    }
                    if let by { at[m.seq] = cardOf[subs[by].id] }
                    continue
                }
                add(.text(id: id, text: text), m)
            case "tool":
                // A subagent's own calls count on its card.
                if let call = m.toolID, let sub = owned[call] {
                    at[m.seq] = cardOf[sub]
                    continue
                }
                // Claude Code's `Agent`, and OpenCode's `task`: a card in place of the call.
                if m.toolName == "Agent" || m.toolName == "Task" || (m.toolName == "task" && provider == "opencode") {
                    let host = m.toolID.flatMap { byHost[$0] }
                    let card = agentCard(m, sub: host.map { subs[$0] }, own: host.map { own[$0] } ?? [], working: working, catalog: catalog, provider: provider ?? "claude-code")
                    add(.card(card), m)
                    continue
                }
                // Only agents Flotilla started get a card: a line that only looks like a request
                // (a file or a comment the agent read) started nothing, and stays a call.
                let handed = (m.pending ? [] : handOffs(m)).filter { children[$0.id] != nil }
                if !handed.isEmpty {
                    for h in handed where !placed.contains(h.id) {
                        placed.insert(h.id)
                        add(.card(handOffCard(h, child: children[h.id], catalog: catalog)), m)
                    }
                    continue
                }
                add(.tool(id: id, m), m)
            default:
                add(.notice(id: id, text: m.text ?? "", isError: m.isError), m)
            }
        }

        // Subagents whose start no call shows (Codex): a card where they started, before the
        // first reply or call after it.
        let recorded = subs.indices.filter { subs[$0].parentCall == nil || tools[subs[$0].parentCall!] == nil }.sorted { (subs[$0].startedMs ?? 0) < (subs[$1].startedMs ?? 0) }
        for i in recorded {
            let s = subs[i]
            let card = recordedCard(s, provider: provider, own: own[i], working: working, catalog: catalog)
            let at = s.startedMs.flatMap { start in stamps.firstIndex { !$0.prompt && ($0.at ?? 0) >= start } } ?? rows.count
            rows.insert(.card(card), at: at)
            stamps.insert((s.startedMs, false), at: at)
        }

        // Calls one after another fold into one line; a search hit among them goes to it.
        let (folded, into) = foldCalls(rows)
        rows = folded
        at = at.mapValues { into[$0] ?? $0 }

        // The running turn's text as it streams.
        if summary?.turnStartedMs != nil {
            for p in partials.sorted(by: { $0.timestamp < $1.timestamp }) {
                if let text = p.streamingText { rows.append(.partial(id: "live:\(p.id)", text: text)) }
            }
        }

        let pinned = rows.compactMap { row -> Card? in
            if case .card(let c) = row, c.running { return c }
            return nil
        }
        return Plan(rows: rows, pinned: pinned, at: at)
    }

    /// Calls in a run this long or longer fold into one line (`FOLD_CALLS`).
    static let foldAt = 2

    /// Each run of `foldAt` or more calls in a row as one `.calls` line (`fold_calls`), and
    /// the line each folded call's row went into, by its id.
    static func foldCalls(_ rows: [Row]) -> ([Row], [String: String]) {
        var out: [Row] = []
        var into: [String: String] = [:]
        var run: [(id: String, m: Message)] = []
        func flush() {
            if run.count >= foldAt {
                let id = "calls:\(run[0].id)"
                for r in run { into[r.id] = id }
                out.append(.calls(id: id, run.map(\.m)))
            } else {
                out += run.map { Row.tool(id: $0.id, $0.m) }
            }
            run = []
        }
        for row in rows {
            if case .tool(let id, let m) = row {
                run.append((id, m))
            } else {
                flush()
                out.append(row)
            }
        }
        flush()
        return (out, into)
    }

    /// "Called Bash 9 times", or for a run of several tools each with its count, in the order
    /// they were first called: "Called Bash 6 times, Read twice, Grep once" (`called_label`).
    static func calledLabel(_ names: [String]) -> String {
        var counts: [(name: String, n: Int)] = []
        for name in names {
            if let i = counts.firstIndex(where: { $0.name == name }) { counts[i].n += 1 } else { counts.append((name, 1)) }
        }
        let said = counts.map { c -> String in
            switch c.n {
            case 1: "\(c.name) once"
            case 2: "\(c.name) twice"
            default: "\(c.name) \(c.n) times"
            }
        }
        return "Called " + said.joined(separator: ", ")
    }

    /// A prompt or steer that took your answer to a question whose card is among `cards`: the
    /// card stands for it.
    static func tookAnswer(_ m: Message, cards: Set<String>) -> Bool {
        (m.kind == "prompt" || m.kind == "steer") && m.answerTo.map(cards.contains) == true
    }

    /// What an answered question's card says of the message that took the answer: a prompt's
    /// turn as its bubble would say it, until it's finished; a steer only until it's delivered.
    static func answerStatus(_ m: Message) -> String? {
        guard let s = m.status, s != "finished" else { return nil }
        if m.kind == "steer" && s != "sending" && s != "not sent" { return nil }
        return s
    }

    // MARK: Cards

    /// A Claude Code `Agent` call (or OpenCode's `task`): its description, its model, how it went.
    /// `own`: its subagent's own calls, in order.
    static func agentCard(_ m: Message, sub: Subagent?, own: [Message], working: Bool, catalog: Catalog?, provider: String = "claude-code") -> Card {
        let input = m.toolInputJSON.flatMap { try? JSON(parsing: $0) } ?? .null
        let openCode = m.toolName == "task"
        if openCode, let sub {
            // OpenCode's call says little (a placeholder boat never shows ending): its records
            // say the rest, on the card in the call's place (as `subagents.rs` has it).
            var card = recordedCard(sub, provider: provider, own: own, working: working, catalog: catalog)
            card.id = "agent:\(m.toolID ?? "\(m.seq)")"
            if sub.name.isEmpty, let d = input["description"]?.string?.nilIfBlank ?? input["title"]?.string?.nilIfBlank { card.title = d }
            card.task = card.task ?? input["prompt"]?.string?.nilIfBlank
            card.provider = provider
            return card
        }
        let background = input["run_in_background"]?.bool == true || sub?.background == true
        // OpenCode's call without its records: done once the turn is.
        let running = background ? (sub?.running == true && working) : m.pending && (!openCode || working)
        var meta: [String] = []
        if let model = sub?.model { meta.append(Format.modelName(provider: provider, model: model, catalog: catalog)) }
        else if let kind = input["subagent_type"]?.string, !kind.isEmpty { meta.append(kind) }
        if background { meta.append("in the background") }
        if let c = Format.calls(Int64(own.count)) { meta.append(c) }
        let result = m.toolResultJSON.flatMap { try? JSON(parsing: $0) }
        if !running {
            if let s = sub?.startedMs, let f = sub?.finishedMs { meta.append(Format.duration(f - s)) }
            else if let ms = result?["duration_ms"]?.int ?? result?["totalDurationMs"]?.int { meta.append(Format.duration(ms)) }
        }
        var report: String?
        if !running {
            report = (background ? nil : result.flatMap(resultText).map(cleanReport)) ?? sub?.texts.last
        }
        let problem = running ? nil : (m.status ?? (m.isError ? "error" : nil))
        return Card(
            id: "agent:\(m.toolID ?? "\(m.seq)")", title: input["description"]?.string?.nilIfBlank ?? input["title"]?.string?.nilIfBlank ?? "Subagent", meta: meta, running: running, problem: problem,
            latestName: own.last?.toolName, latestSummary: own.last?.toolSummary, report: report?.nilIfBlank, task: input["prompt"]?.string?.nilIfBlank,
            startedMs: sub?.startedMs ?? m.timestamp, provider: provider
        )
    }

    /// A subagent its harness's records show: Codex's spawns, and OpenCode's (on its call).
    /// `own`: its own calls, in order.
    static func recordedCard(_ s: Subagent, provider: String?, own: [Message], working: Bool, catalog: Catalog?) -> Card {
        let running = s.running && working
        var meta: [String] = []
        if let model = s.model { meta.append(provider.map { Format.modelName(provider: $0, model: model, catalog: catalog) } ?? Catalog.readable(model)) }
        else if let kind = s.kind { meta.append(kind) }
        if let c = Format.calls(Int64(max(own.count, s.calls.count))) { meta.append(c) }
        if !running, let a = s.startedMs, let b = s.finishedMs { meta.append(Format.duration(b - a)) }
        return Card(
            id: "sub:\(s.id)", title: s.name.isEmpty ? "Subagent" : s.name.prefix(1).uppercased() + s.name.dropFirst(), meta: meta, running: running, problem: nil,
            latestName: own.last?.toolName ?? s.latestName, latestSummary: own.last?.toolSummary ?? s.latestSummary,
            report: running ? nil : s.texts.last?.nilIfBlank, task: s.prompt?.nilIfBlank, startedMs: s.startedMs, provider: nil
        )
    }

    struct HandOff: Hashable {
        let id: String
        var title: String?
        var agent: String?
        var model: String?
        var task: String?
    }

    /// The `flotilla delegate` and `flotilla agent` requests a finished call printed
    /// (`FLOTILLA <id> <signature> {json}` lines, or without the signature as they were before
    /// requests were signed; JSON inside strings included), in order. Nothing here says the line
    /// is genuine: the server carries out only those its VM signed.
    static func handOffs(_ m: Message) -> [HandOff] {
        guard let result = m.toolResultJSON else { return [] }
        // No line can start `FLOTILLA ` unless the result has it, or a `\u` escape that might
        // spell it (as JSON inside JSON too): most results aren't parsed at all.
        guard has(result, "FLOTILLA ") || has(result, "\\u") else { return [] }
        var texts: [String] = []
        if let v = try? JSON(parsing: result) { strings(v, into: &texts) } else { texts = [result] }
        var out: [HandOff] = []
        for text in texts {
            for line in text.split(whereSeparator: \.isNewline) {
                let l = line.drop { $0 == " " || $0 == "\t" }
                guard l.hasPrefix("FLOTILLA ") else { continue }
                let rest = l.dropFirst(9)
                guard let space = rest.firstIndex(of: " ") else { continue }
                let id = String(rest[..<space])
                guard id.count == 36, id.allSatisfy({ $0.isHexDigit || $0 == "-" }), !out.contains(where: { $0.id == id }) else { continue }
                var body = rest[rest.index(after: space)...]
                // The signature (64 hex digits) before the JSON.
                if let gap = body.firstIndex(of: " "), body.distance(from: body.startIndex, to: gap) == 64, body[..<gap].allSatisfy(\.isHexDigit) {
                    body = body[body.index(after: gap)...]
                }
                let req = try? JSON(parsing: String(body))
                if let req, req["id"]?.string != id { continue }
                out.append(HandOff(id: id, title: req?["title"]?.string, agent: req?["agent"]?.string, model: req?["model"]?.string, task: req?["task"]?.string))
            }
        }
        return out
    }

    /// Whether `text` has `bytes` in it, byte for byte: `contains` compares characters, slower
    /// over a long result than parsing it.
    private static func has(_ text: String, _ bytes: StaticString) -> Bool {
        var text = text
        return text.withUTF8 { t in
            bytes.withUTF8Buffer { b in t.count >= b.count && memmem(t.baseAddress, t.count, b.baseAddress, b.count) != nil }
        }
    }

    private static func strings(_ v: JSON, into out: inout [String]) {
        switch v {
        case .string(let s):
            // A tool's output is sometimes JSON inside a string.
            if let inner = try? JSON(parsing: s), inner.array != nil || inner.object != nil { strings(inner, into: &out) } else { out.append(s) }
        case .array(let a): a.forEach { strings($0, into: &out) }
        case .object(let o): o.values.forEach { strings($0, into: &out) }
        default: break
        }
    }

    static func handOffCard(_ h: HandOff, child: ChildCard?, catalog: Catalog?) -> Card {
        guard let c = child else {
            // Before the server lists it: what the request said.
            let model = [h.agent, h.model].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
            let title = h.title?.nilIfBlank.map { Format.ellipsize($0, 80) }
                ?? h.task?.split(whereSeparator: \.isNewline).first.map { Format.ellipsize(String($0), 60) } ?? "Delegated task"
            return Card(id: "child:\(h.id)", title: title, meta: model.isEmpty ? [] : [model], running: true, task: h.task, provider: h.agent)
        }
        var meta = [Format.modelName(provider: c.provider, model: c.model, catalog: catalog)]
        if let calls = Format.calls(c.calls) { meta.append(calls) }
        if !c.state.running, let ms = c.durationMs { meta.append(Format.duration(ms)) }
        let problem: String? = switch c.state {
        case .failed: "failed"
        case .error: "error"
        case .gone: "VM gone"
        case .didntStart: "didn't start"
        case .stopped: "stopped"
        default: nil
        }
        return Card(
            id: "child:\(c.id)", title: c.title.nilIfBlank ?? "Delegated task", meta: meta, running: c.state.running, problem: problem,
            latestName: c.latest?.0, latestSummary: c.latest?.1, report: c.answer?.nilIfBlank, task: c.task.nilIfBlank, startedMs: c.startedMs, provider: c.provider,
            thread: c.thread ? c.id : nil
        )
    }

    /// Each message's turn: how many prompts there are up to and including it.
    static func turns(_ messages: [Message]) -> [Int64: Int] {
        var n = 0
        var out: [Int64: Int] = [:]
        for m in messages {
            if m.kind == "prompt" { n += 1 }
            out[m.seq] = n
        }
        return out
    }

    /// What subagents said, by the turn each ran in (`subTurn`, in the same order), as
    /// `subagents::plan` has it.
    static func saidByTurn(_ subs: [Subagent], _ subTurn: [Int]) -> [Int: [String]] {
        var out: [Int: [String]] = [:]
        for (s, t) in zip(subs, subTurn) {
            out[t, default: []] += s.texts.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
        }
        return out
    }

    /// The turn each subagent ran in: that of its first call (`own`, each one's calls in order),
    /// else of the last prompt before it started, else the last turn.
    static func turnsOf(_ subs: [Subagent], own: [[Message]], _ messages: [Message], _ turn: [Int64: Int]) -> [Int] {
        let last = turn.values.max() ?? 0
        let prompts = messages.filter { $0.kind == "prompt" }.map { $0.timestamp ?? 0 }
        return zip(subs, own).map { s, own in
            own.first.flatMap { turn[$0.seq] }
                ?? s.startedMs.map { start in min(max(prompts.filter { $0 <= start }.count, 1), max(last, 1)) }
                ?? last
        }
    }

    /// `text` without what a subagent said, where it stands as whole lines: its card has it.
    /// Empty when that's all the text was.
    static func without(_ said: [String], in text: String) -> String {
        var rest = text.trimmingCharacters(in: .whitespacesAndNewlines)
        for s in said {
            if rest == s { return "" }
            var from = rest.startIndex
            while let r = rest.range(of: s, range: from..<rest.endIndex) {
                let startsLine = r.lowerBound == rest.startIndex || rest[rest.index(before: r.lowerBound)] == "\n"
                let endsLine = r.upperBound == rest.endIndex || rest[r.upperBound] == "\n"
                if startsLine && endsLine {
                    rest.removeSubrange(r)
                    from = rest.startIndex
                } else {
                    from = r.upperBound
                }
            }
        }
        return rest.replacingOccurrences(of: "\n\n\n", with: "\n\n").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// A tool result's text: its `content` or `output`.
    private static func resultText(_ r: JSON) -> String? {
        if let s = r["content"]?.string { return s }
        if let s = r["output"]?.string { return s }
        if let parts = r["content"]?.array {
            let t = parts.compactMap { $0["text"]?.string }.joined(separator: "\n")
            return t.isEmpty ? nil : t
        }
        return nil
    }

    /// A subagent's report without the harness's bookkeeping lines.
    private static func cleanReport(_ s: String) -> String {
        var t = s
        // OpenCode's: `<task id=… state=…><task_result>…</task_result></task>`.
        if let a = t.range(of: "<task_result>") {
            t = String(t[a.upperBound...])
            if let b = t.range(of: "</task_result>") { t = String(t[..<b.lowerBound]) }
        }
        if let r = t.range(of: "<usage>") { t = String(t[..<r.lowerBound]) }
        let lines = t.split(separator: "\n", omittingEmptySubsequences: false).filter { line in
            let l = line.trimmingCharacters(in: .whitespaces)
            return !l.hasPrefix("agentId:") && l != "The report follows:" && l != "</tools-used-before-summary>"
        }
        return lines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: Prompts that aren't the user's

    /// What a request of the agent's that came through cut short shows as.
    static let unreadableLine = "Couldn't read a request from the agent: it was cut short"

    /// Flotilla's own message, as the line marking it: who answered (their card to show), or
    /// what couldn't start or be read. `nil`: none it knows.
    static func flotillas(_ text: String, id: String) -> Row? {
        if let members = parseReport(text) {
            let line = members.count == 1 ? "\(members[0].title) answered" : "\(members.count) agents answered"
            return .answered(id: id, text: line, target: members.first.map { "child:\($0.id)" })
        }
        if let a = parseAnswer(text) { return .answered(id: id, text: "\(a.title) \(a.how)", target: "child:\(a.id)") }
        if let c = parseCouldntStart(text) { return .answered(id: id, text: "Couldn't start \(c.title): \(c.why)", target: nil) }
        if isUnreadable(text) { return .answered(id: id, text: unreadableLine, target: nil) }
        return nil
    }

    /// `[flotilla] Couldn't read your request id: it was cut short…` (`orchestrate::unreadable_text`).
    static func isUnreadable(_ text: String) -> Bool {
        text.hasPrefix("[flotilla] Couldn't read your request ") && text.contains(": it was cut short")
    }

    /// `[flotilla] Agents answered (N): … ### "Title" (id) …`: the agents, in order.
    static func parseReport(_ text: String) -> [(title: String, id: String)]? {
        guard text.hasPrefix("[flotilla] Agents answered (") else { return nil }
        var out: [(String, String)] = []
        for part in text.components(separatedBy: "### \"").dropFirst() {
            guard let a = part.range(of: "\" (") else { continue }
            let after = part[a.upperBound...]
            guard let close = after.firstIndex(of: ")") else { continue }
            out.append((String(part[..<a.lowerBound]), String(after[..<close])))
        }
        return out
    }

    /// `[flotilla] Couldn't start "Title" (id): why` (`orchestrate::couldnt_start_text`).
    static func parseCouldntStart(_ text: String) -> (title: String, why: String)? {
        guard text.hasPrefix("[flotilla] Couldn't start \"") else { return nil }
        let rest = text.dropFirst("[flotilla] Couldn't start \"".count)
        guard let a = rest.range(of: "\" ("), let b = rest[a.upperBound...].range(of: "): ") else { return nil }
        return (String(rest[..<a.lowerBound]), rest[b.upperBound...].trimmingCharacters(in: .whitespacesAndNewlines))
    }

    /// `[flotilla] "Title" (id[, branch b]) how:\n\nanswer`, or `… how without an answer.`
    static func parseAnswer(_ text: String) -> (title: String, id: String, how: String, answer: String)? {
        guard text.hasPrefix("[flotilla] \"") else { return nil }
        let rest = text.dropFirst("[flotilla] \"".count)
        guard let a = rest.range(of: "\" (") else { return nil }
        let title = String(rest[..<a.lowerBound])
        let after = rest[a.upperBound...]
        guard let b = after.range(of: ") ") else { return nil }
        let id = after[..<b.lowerBound].split(separator: ",").first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
        let tail = after[b.upperBound...]
        if let colon = tail.firstIndex(of: ":") {
            return (title, id, tail[..<colon].trimmingCharacters(in: .whitespaces), tail[tail.index(after: colon)...].trimmingCharacters(in: .whitespacesAndNewlines))
        }
        var how = String(tail)
        while how.hasSuffix(".") { how.removeLast() }
        return (title, id, how.trimmingCharacters(in: .whitespaces), "")
    }

    /// A child's task as its parent handed it, without the note for the agent.
    static func taskOf(_ text: String) -> String? {
        if text.hasSuffix(childNote) { return String(text.dropLast(childNote.count)) }
        let note = childNote.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.hasSuffix(note) { return String(text.dropLast(note.count)).trimmingCharacters(in: .whitespacesAndNewlines) }
        return nil
    }

    // MARK: Tool detail

    /// What a tool row shows when opened (`tool_detail`).
    /// What a tool call's Copy takes, whole (its line is cut to 120 characters): the command,
    /// else the path, pattern, query or address, as the server picks its summary, and its menu
    /// item's words.
    static func toolCopy(_ m: Message) -> (label: String, text: String) {
        let input = m.toolInputJSON.flatMap { try? JSON(parsing: $0) }
        for key in ["command", "file_path", "path", "pattern", "query", "url"] {
            guard let v = input?[key]?.string?.nilIfBlank else { continue }
            switch key {
            case "command": return ("Copy command", v)
            case "file_path", "path": return ("Copy path", v)
            default: return ("Copy", v)
            }
        }
        return ("Copy", m.toolSummary?.nilIfBlank ?? m.toolName ?? "")
    }

    /// `whole`: for Copy output, nothing cut and a file read as the file itself.
    static func toolDetail(_ m: Message, whole: Bool = false) -> String {
        let r = m.toolResultJSON.flatMap { try? JSON(parsing: $0) }
        let text: String
        switch m.toolResultType ?? r?["type"]?.string {
        case "bash_output":
            text = (r?["content"]?.string ?? r?["output"]?.string).flatMap(\.nilIfBlank) ?? (whole ? "" : "(no output)")
        case "file_write":
            text = "wrote \(r?["path"]?.string ?? "")\n\n\(r?["content"]?.string ?? "")"
        case "file_content" where whole:
            text = r?["content"]?.string ?? ""
        case "file_content":
            let lines = r?["content"]?.string?.split(separator: "\n", omittingEmptySubsequences: false).count ?? 0
            text = "read \(r?["path"]?.string ?? "") (\(lines) lines)"
        case "file_edit":
            let old = (r?["old_string"]?.string ?? r?["oldString"]?.string ?? "").split(separator: "\n", omittingEmptySubsequences: false).map { "- \($0)" }
            let new = (r?["new_string"]?.string ?? r?["newString"]?.string ?? "").split(separator: "\n", omittingEmptySubsequences: false).map { "+ \($0)" }
            text = ([r?["path"]?.string ?? r?["file_path"]?.string ?? ""] + old + new).joined(separator: "\n")
        default:
            // Plain text (kept as a JSON string), or an `output`.
            if let s = r?.string { text = s }
            else if let out = r?["output"] { text = out.string ?? out.text }
            else { text = m.toolInputJSON ?? "" }
        }
        return whole ? text : Format.ellipsize(text, 6000)
    }

    /// What a tool's result says, whole (Show all, Copy all).
    static func toolText(_ m: Message) -> String {
        toolDetail(m, whole: true)
    }
}

extension String {
    var nilIfBlank: String? { trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : self }
}
