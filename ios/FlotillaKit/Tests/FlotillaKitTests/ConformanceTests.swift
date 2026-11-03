import Foundation
import Testing
@testable import FlotillaKit

/// The repo's `mobile/fixtures`, from this file's place in the tree.
func fixture(_ path: String) throws -> JSON {
    let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    let data = try Data(contentsOf: root.appending(path: "mobile/fixtures/\(path)"))
    return try JSON(parsing: data)
}

extension ViewKey {
    init(recorded j: JSON) throws {
        self = try #require(ViewKey(fields: j), "a view key: \(j.text)")
    }
}

/// The Rust side (`crates/flotilla-server/tests/conformance.rs`) recorded these; the Swift
/// side must make exactly the same of them.
@Suite struct RustConformance {
    @Test func `deltas apply exactly as Rust's diff meant`() throws {
        let cases = try #require(try fixture("conformance/deltas.json")["cases"]?.array)
        #expect(cases.count > 150)
        var orders = 0
        for (n, c) in cases.enumerated() {
            let view = try ViewKey(recorded: try #require(c["view"]))
            let ops = try #require(c["ops"]?.array).map(Op.init(json:))
            orders += ops.count { if case .order = $0 { true } else { false } }
            var data = try #require(c["old"])
            let want = try #require(c["new"])
            if want.isNull {
                #expect(throws: Delta.Failure.self, "case \(n) should fail") { try Delta.apply(view, to: &data, ops) }
            } else {
                try Delta.apply(view, to: &data, ops)
                #expect(data == want, "case \(n)")
            }
        }
        #expect(orders > 50)
    }

    /// A real session: snapshots, deltas, a lost delta and its resync, a drop that resumes
    /// (with answers sent again and skipped), drops with requests in flight (a resumed welcome
    /// that resends by `seen`, and one in the same server run that isn't resumed), snapshots
    /// with values this version doesn't know or that don't read, a server restart. Each frame
    /// must come to what it came to in Rust, each message the client writes must be the JSON
    /// Rust wrote, what a welcome sends again or says is lost must be what Rust's did, and the
    /// views must end the same.
    @Test func `a recorded session replays to the same views`() throws {
        let rec = try fixture("conformance/session.json")
        let steps = try #require(rec["steps"]?.array)
        let session = try #require(steps.lazy.compactMap { $0["up"] }.first { $0["t"] == "hello" }?["session"]?.string)
        var core = SessionCore(id: session)
        /// What the core said to write that the recording should show next.
        var expectUp: [Up] = []
        /// What the last welcome sent again and said was lost, for the recording's `welcomed`.
        var welcomed: (again: [Int64], lost: [Int64])?
        var (connects, injected, unreadable) = (0, 0, 0)
        for (i, step) in steps.enumerated() {
            if step["connect"] != nil {
                connects += 1
            } else if step["disconnect"] != nil {
                core.lost(unsent: [])
            } else if step["lost"] != nil {
                continue
            } else if let w = step["welcomed"] {
                let want = (again: (w["again"]?.array ?? []).compactMap(\.int), lost: (w["lost"]?.array ?? []).compactMap(\.int))
                #expect(welcomed?.again == want.again && welcomed?.lost == want.lost, "step \(i): Swift \(String(describing: welcomed)), Rust \(w.text)")
                welcomed = nil
            } else if let up = step["up"] {
                // What the core said to write (sent again after a welcome, a resync): next.
                if let next = expectUp.first, next.json == up {
                    expectUp.removeFirst()
                    continue
                }
                switch up["t"]?.string {
                case "hello":
                    let mine = core.hello(app: "flotilla-ios", version: "0", platform: "ios").json
                    // The recorder said one less than it got on the resumed hello, on purpose.
                    var theirs = up
                    if case .object(var f) = theirs {
                        f["got"] = mine["got"]
                        theirs = .object(f)
                        if let g = up["got"]?.int, let m = mine["got"]?.int { #expect(m >= g, "step \(i)") }
                    }
                    #expect(mine == theirs, "step \(i): \(mine.text) vs \(up.text)")
                case "sub":
                    let v = try ViewKey(recorded: up)
                    #expect(core.app(.sub(v)).map(\.json) == [up], "step \(i)")
                case "req":
                    let id = UInt64(try #require(up["id"]?.int))
                    let r = Request(method: try #require(up["m"]?.string), params: up["p"])
                    #expect(core.app(.req(id: id, r)).map(\.json) == [up], "step \(i)")
                case "resync":
                    let next = expectUp.isEmpty ? nil : expectUp.removeFirst()
                    #expect(next?.json == up, "step \(i): the core didn't ask for this resync")
                default:
                    Issue.record("step \(i): an unknown up \(up.text)")
                }
            } else if let raw = step["down"] {
                let taken = try #require(step["taken"])
                let down = try #require(Down(json: raw), "step \(i) doesn't read: \(raw.text.prefix(200))")
                if step["injected"] != nil { injected += 1 }
                let got = core.received(down)
                switch got {
                case .welcomed(_, let ups, let lost):
                    #expect(taken == "welcome", "step \(i)")
                    // What it sends again comes next; what's lost is checked at `welcomed`.
                    expectUp.append(contentsOf: ups)
                    welcomed = (ups.compactMap { if case .req(let id, _) = $0 { Int64(id) } else { nil } }, lost.map(Int64.init))
                case .fail(let why):
                    Issue.record("step \(i): failed \(why)")
                case .skipped: #expect(taken == "skipped", "step \(i)")
                case .unreadable(let key, _):
                    let want = try #require(taken["unreadable"], "step \(i): Rust took \(taken.text), Swift couldn't read \(key)")
                    #expect(key == (try ViewKey(recorded: want)), "step \(i)")
                    // Kept, with its revision, and not asked for again.
                    #expect(core.replica.rev(key) != nil, "step \(i): \(key) dropped")
                    unreadable += 1
                case .nothing: #expect(taken == "nothing", "step \(i)")
                case .resync(let view, _):
                    let want = try #require(taken["resync"], "step \(i): Rust took \(taken.text), Swift asked for \(view)")
                    #expect(view == (try ViewKey(recorded: want)), "step \(i)")
                    expectUp.append(.resync(view))
                case .refuse(let why, _):
                    Issue.record("step \(i): refused \(why)")
                case .deliver(let events):
                    #expect(events.count == 1, "step \(i)")
                    switch events.first {
                    case .view(let v)?:
                        let want = try #require(taken["show"], "step \(i): Rust took \(taken.text), Swift showed \(v.key)")
                        #expect(v.key == (try ViewKey(recorded: want)), "step \(i)")
                    case .answer(let id, _)?: #expect(Int64(id) == taken["res"]?.int, "step \(i)")
                    case .live?: #expect(taken == "live", "step \(i)")
                    case .note?: #expect(taken == "note", "step \(i)")
                    case .progress?: #expect(taken == "progress", "step \(i)")
                    case .connection(.online)?: #expect(taken == "synced", "step \(i)")
                    case let other: Issue.record("step \(i): Rust took \(taken.text), Swift \(String(describing: other))")
                    }
                }
            }
        }
        #expect(connects == 5)
        #expect(injected == 2 && unreadable == 1)
        #expect(expectUp.isEmpty)
        let views = try #require(rec["views"]?.array)
        #expect(Set(core.replica.subscribed) == Set(try views.map { try ViewKey(recorded: $0) }))
        for v in views {
            let key = try ViewKey(recorded: v)
            #expect(core.replica.rev(key).map(Int64.init) == v["rev"]?.int, "\(key)'s revision")
            #expect(core.replica.data(key) == v["data"], "\(key)'s data")
            // And it reads as the view.
            _ = try ServerView(key: key, data: try #require(core.replica.data(key)))
        }
    }
}

extension RustConformance {
    /// The usage the recorded session ends with, as Rust wrote it: every count, extra starts,
    /// when a full window frees up, when it was read.
    @Test func `the account's usage reads as Rust wrote it`() throws {
        let rec = try fixture("conformance/session.json")
        let view = try #require(rec["views"]?.array?.first { $0["view"] == "sidebar" })
        guard case .sidebar(let s) = try ServerView(key: .sidebar, data: try #require(view["data"])) else { Issue.record("not a sidebar"); return }
        let u = try #require(s.usage)
        #expect(u.vms == Count(3, of: 100) && u.minute == Count(12, of: 12) && u.hour == Count(22, of: 60) && u.day == Count(187, of: 200))
        #expect(u.vmHours == Count(48, of: 556) && u.renews == "2026-11-30T01:01:38.000Z" && u.blocked == nil)
        #expect(u.startsFree == "2026-10-31T17:04:00.000Z" && u.readMs == 1_793_466_000_000)
        #expect(u.extraStarts == ExtraStarts(on: true, used: 7, spendCents: Count(29, of: 8000)))
        #expect(s.engineRunning)
    }
}

/// A long thread, recorded by Rust (`history.json`): its view's newest pages; older pages
/// fetched as Rust's `History` wanted them; the view moving on as the thread grows; a message
/// changing outside it; a lost delta and its resync; tool output cut (multibyte across the
/// cut, a list too long, text that isn't JSON) and fetched whole. The Swift replica must end
/// with the same views, and the Swift `History` must want and show exactly what Rust's did.
@Suite struct RustHistoryConformance {
    @Test func `a long thread pages and cuts as Rust's did`() throws {
        let rec = try fixture("conformance/history.json")
        let steps = try #require(rec["steps"]?.array)
        let session = try #require(steps.lazy.compactMap { $0["up"] }.first { $0["t"] == "hello" }?["session"]?.string)
        var core = SessionCore(id: session)
        var history = History()
        var (checks, pages, wholes, cut) = (0, 0, 0, 0)
        var reached: Int64?
        for (i, step) in steps.enumerated() {
            if step["connect"] != nil || step["lost"] != nil {
                continue
            } else if step["disconnect"] != nil {
                core.lost(unsent: [])
            } else if let up = step["up"] {
                switch up["t"]?.string {
                case "hello": _ = core.hello(app: "flotilla-ios", version: "0", platform: "ios")
                case "sub": #expect(core.app(.sub(try ViewKey(recorded: up))).map(\.json) == [up], "step \(i)")
                case "req":
                    let id = UInt64(try #require(up["id"]?.int))
                    #expect(core.app(.req(id: id, Request(method: try #require(up["m"]?.string), params: up["p"]))).map(\.json) == [up], "step \(i)")
                case "resync": continue
                default: Issue.record("step \(i): an unknown up \(up.text)")
                }
            } else if let raw = step["down"] {
                let down = try #require(Down(json: raw), "step \(i) doesn't read")
                switch core.received(down) {
                case .deliver(let events):
                    for case .view(.thread(let d)) in events {
                        history.view(d)
                    }
                case .resync(let view, _):
                    #expect(step["taken"]?["resync"] != nil, "step \(i): Swift asked for \(view), Rust didn't")
                default: break
                }
            } else if let more = step["more"] {
                let got = history.more()
                #expect(got == more.bool, "step \(i): more")
            } else if let wanted = step["wanted"] {
                let got = step["room"]?.int.map { history.wanted(upTo: Int($0)) } ?? history.wanted()
                #expect(got == (wanted.array ?? []).compactMap(\.int), "step \(i): wanted")
            } else if let reach = step["reach"] {
                let seq = try #require(reach["seq"]?.int)
                #expect(!history.shows(seq), "step \(i): shown before it's reached")
                #expect(history.reach(seq) == reach["wanted"]?.bool, "step \(i): reach")
                reached = seq
            } else if let hits = step["hits"] {
                // The answer as this phone reads it: the same hits, marks on its characters.
                let res = try #require(steps[..<i].last { $0["down"]?["t"] == "res" }?["down"]?["result"])
                guard case .success(.hits(let got))? = Reply.result(res) else { Issue.record("step \(i): not hits"); continue }
                #expect(got.map(\.seq) == (hits["seqs"]?.array ?? []).map(\.int), "step \(i): hits")
                #expect(got.map(\.snippet) == (hits["snippets"]?.array ?? []).map(\.string), "step \(i): snippets")
                for h in got {
                    #expect(!h.marks.isEmpty && h.markedRanges.count == h.marks.count, "step \(i): marks")
                    #expect(h.markedRanges.allSatisfy { h.snippet[$0].compare("reply", options: [.caseInsensitive]) == .orderedSame || h.snippet[$0].contains("12:") || h.snippet[$0] == "done" }, "step \(i): \(h.markedRanges.map { h.snippet[$0] })")
                }
            } else if let page = step["page"] {
                let p = try HistoryPage(json: try #require(page["body"]))
                #expect(Paging.before(p.page) == page["before"]?.int)
                history.page(p)
                pages += 1
            } else if let h = step["history"] {
                checks += 1
                let name = h["name"]?.string ?? "?"
                #expect(history.moreAbove == h["more_above"]?.bool, "\(name): more above")
                #expect(history.loading == h["loading"]?.bool, "\(name): loading")
                let want = h["shown"]?.array ?? []
                let got = history.messages
                #expect(got.count == want.count, "\(name): \(got.count) shown, Rust \(want.count)")
                for (m, w) in zip(got, want) {
                    #expect(m.seq == w["seq"]?.int && m.kind == w["kind"]?.string && m.text == w["text"]?.string, "\(name): seq \(m.seq)")
                    #expect(m.cut.map(\.field) == (w["cut"]?.array ?? []).compactMap(\.string), "\(name): seq \(m.seq)'s cuts")
                    #expect(m.toolInputJSON.map { Int64($0.utf8.count) } == w["input_bytes"]?.int, "\(name): seq \(m.seq)'s input")
                    #expect(m.toolResultJSON.map { Int64($0.utf8.count) } == w["result_bytes"]?.int, "\(name): seq \(m.seq)'s result")
                    if !m.cut.isEmpty {
                        cut += 1
                        #expect(m.cut.allSatisfy { $0.bytes > Int64(Paging.cap) })
                        #expect((m.toolInputJSON?.utf8.count ?? 0) <= Paging.cap && (m.toolResultJSON?.utf8.count ?? 0) <= Paging.cap)
                    }
                }
            } else if let f = step["full"] {
                // The whole string is as long as the cut said, and starts as the cut one does.
                let body = try #require(f["body"]?.string)
                #expect(Int64(body.utf8.count) == f["bytes"]?.int)
                let seq = try #require(f["seq"]?.int)
                let shown = try #require(history.messages.first { $0.seq == seq })
                let field = f["field"]?.string == "tool_input_json" ? shown.toolInputJSON : shown.toolResultJSON
                #expect(shown.cut.contains { $0.mark == f["mark"]?.string })
                if let field, (try? JSON(parsing: Data(body.utf8))) != nil, (try? JSON(parsing: Data(field.utf8))) != nil {
                    #expect(true)
                } else {
                    #expect(body.hasPrefix(String(try #require(field).dropLast())), "seq \(seq): cut as text, it starts the whole")
                }
                wholes += 1
            }
        }
        #expect(checks >= 8 && pages >= 5 && wholes >= 4 && cut > 0, "\(checks) checks, \(pages) pages, \(wholes) wholes, \(cut) cut")
        // Opened at a search hit on the first page, and it showed.
        let hit = try #require(reached)
        #expect(history.shows(hit))
        // All of it, in seq order, at the end.
        #expect(history.messages.map(\.seq) == Array(0..<Int64(history.messages.count)))
        #expect(!history.moreAbove)
        let views = try #require(rec["views"]?.array)
        for v in views {
            let key = try ViewKey(recorded: v)
            #expect(core.replica.rev(key).map(Int64.init) == v["rev"]?.int, "\(key)'s revision")
            #expect(core.replica.data(key) == v["data"], "\(key)'s data")
            _ = try ServerView(key: key, data: try #require(core.replica.data(key)))
        }
    }

    @Test func `pages one at a time, kept as the view moves on, fetched again when changed`() {
        func detail(_ n: Int64, _ text: (Int64) -> String) -> (ThreadDetail, [Message]) {
            let all = (0..<n).map { Message(seq: $0, kind: "text", text: text($0)) }
            let older = max(0, (n - Paging.window)) / Paging.page * Paging.page
            // Stand-in marks: the page's texts, as a mark is a fingerprint of them.
            let pages = Dictionary(grouping: all, by: { Paging.pageOf($0.seq) }).map { PageMark(page: $0.key, mark: $0.value.compactMap(\.text).joined(separator: "|")) }.sorted { $0.page < $1.page }
            var j: [String: JSON] = ["id": .string("t"), "older": .int(older), "messages": .array([]), "pages": .array([])]
            j["pages"] = .array(pages.map { .object(["page": .int($0.page), "mark": .string($0.mark)]) })
            var d = try! ThreadDetail(json: .object(j))
            d.messages = Array(all[Int(older)...])
            return (d, all)
        }
        func page(_ all: [Message], _ p: Int64) -> HistoryPage {
            let messages = all.filter { Paging.pageOf($0.seq) == p }
            let body: JSON = .object(["thread": .string("t"), "page": .int(p), "mark": .string(messages.compactMap(\.text).joined(separator: "|")), "messages": .array([])])
            var hp = try! HistoryPage(json: body)
            hp.messages = messages
            return hp
        }
        var h = History()
        let (d, all) = detail(260) { "m\($0)" }
        h.view(d)
        #expect(h.messages.map(\.seq) == Array(150..<260))
        var w = h.wanted()
        #expect(w.isEmpty)
        var m = h.more()
        #expect(m)
        m = h.more()
        #expect(!m)
        w = h.wanted()
        #expect(w == [2])
        w = h.wanted()
        #expect(w.isEmpty)
        h.page(page(all, 2))
        #expect(h.messages.first?.seq == 100)
        // The view moves on: page 3 left it unchanged and is kept.
        let (d2, _) = detail(300) { "m\($0)" }
        h.view(d2)
        #expect(h.messages.first?.seq == 100)
        w = h.wanted()
        #expect(w.isEmpty)
        // One changes outside the view: shown as it was, fetched again.
        let (d3, all3) = detail(300) { $0 == 120 ? "changed" : "m\($0)" }
        h.view(d3)
        #expect(h.messages.first { $0.seq == 120 }?.text == "m120")
        w = h.wanted()
        #expect(w == [2])
        h.page(page(all3, 2))
        #expect(h.messages.first { $0.seq == 120 }?.text == "changed")
        // The fetch from before the change answers late: it doesn't replace the current page.
        h.page(page(all, 2))
        #expect(h.messages.first { $0.seq == 120 }?.text == "changed")
        w = h.wanted()
        #expect(w.isEmpty)
        h.failed(2)
        #expect(Paging.pageOf(-1) == -1 && Paging.pageOf(49) == 0 && Paging.pageOf(50) == 1)
    }
}

/// The views a real server sent (`mobile/fixtures/side-live4`) read as the phone needs them.
@Suite struct CapturedViews {
    @Test func `the sidebar reads`() throws {
        guard case .sidebar(let s) = try ServerView(key: .sidebar, data: try fixture("side-live4/sidebar.json")) else { Issue.record("not a sidebar"); return }
        let row = try #require(s.threads.first)
        #expect(row.thread.title == "Planting bugs and getting reviews")
        #expect(row.thread.provider == "claude-code")
        #expect(row.activity == .idle)
        #expect(row.lastReply?.isEmpty == false)
    }

    @Test func `the thread reads, with its hand-off cards`() throws {
        let id = "31360c1e-cbab-4b44-993c-7dec50d661f5"
        guard case .thread(let t) = try ServerView(key: .thread(id), data: try fixture("side-live4/thread-\(id).json")) else { Issue.record("not a thread"); return }
        #expect(t.messages.count == 8)
        #expect(t.messages.map(\.kind) == ["prompt", "tool", "text", "tool", "text", "prompt", "tool", "text"])
        #expect(t.children.count == 2)
        #expect(t.children.allSatisfy { $0.state == .done && $0.answer != nil })
        #expect(t.thread?.state == .idle)
        // Under another thread's key it's a doubt.
        #expect(throws: ReadError.self) { try ServerView(key: .thread("other"), data: try fixture("side-live4/thread-\(id).json")) }
    }

    @Test func `the catalog reads, with model labels`() throws {
        guard case .catalog(let c?) = try ServerView(key: .catalog, data: try fixture("side-live4/catalog.json")) else { Issue.record("no catalog"); return }
        #expect(c.providers.contains { $0.key == "claude-code" })
        #expect(c.modelLabel(provider: "claude-code", model: "opus-5-5") != "opus-5-5")
        #expect(c.label(provider: "claude-code", model: "claude-sonnet-5") == nil)
    }

    @Test func `a model the catalog doesn't name reads as words`() {
        #expect(Catalog.readable("claude-sonnet-5") == "Sonnet 5")
        #expect(Catalog.readable("claude-opus-5-5") == "Opus 5.5")
        #expect(Catalog.readable("claude-haiku-4-5-20251001") == "Haiku 4.5")
        #expect(Catalog.readable("gpt-6.1-sol") == "GPT-6.1 Sol")
        #expect(Catalog.readable("gpt-5.4-mini") == "GPT-5.4 Mini")
        #expect(Catalog.readable("anthropic/claude-opus-4-1") == "Opus 4.1")
        #expect(Catalog.readable("gemini-3-pro") == "Gemini 3 Pro")
        #expect(Catalog.readable("o3") == "o3")
        #expect(Catalog.readable("") == "")
    }
}
