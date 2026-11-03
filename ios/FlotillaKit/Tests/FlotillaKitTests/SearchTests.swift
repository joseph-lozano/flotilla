import Foundation
import Testing
@testable import FlotillaKit

@Suite struct SearchReading {
    @Test func `hits read from the server's answer, marks on their characters`() throws {
        let answer = try JSON(parsing: #"""
        {"Ok": {"hits": [
          {"thread": "t1", "title": "Fix the retry loop", "kind": "title", "seq": null, "snippet": "Fix the retry loop", "marks": [[8, 13]], "at": 1793300000000},
          {"thread": "t1", "title": "Fix the retry loop", "kind": "text", "seq": 157, "snippet": "…é ✓ the retry loop", "marks": [[14, 19], [0, 99], [1, 2]], "at": null, "hits": 4},
          {"thread": "t2", "title": "Later", "kind": "something new", "seq": 3, "snippet": "x"}
        ]}}
        """#)
        guard case .success(.hits(let hits))? = Reply.result(answer) else { Issue.record("not hits"); return }
        #expect(hits.count == 2, "a kind this phone doesn't know is left out")
        #expect(hits[0].kind == .title && hits[0].seq == nil && hits[0].markedRanges.map { String(hits[0].snippet[$0]) } == ["retry"])
        // "…é ✓ the " is 14 bytes: the mark at 14..<19 is "retry"; one past the end, and one
        // inside the ellipsis's bytes, are dropped.
        #expect(hits[1].seq == 157 && hits[1].at == nil && hits[1].hits == 4)
        #expect(hits[1].markedRanges.map { String(hits[1].snippet[$0]) } == ["retry"])
        #expect(hits[1].id == "t1:157" && hits[0].id == "t1:title")
    }

    @Test func `search is asked with the query and a limit`() {
        let r = Request.search("retry loop")
        #expect(r.method == "search")
        #expect(r.params == ["query": "retry loop", "limit": .int(Int64(Search.limit))])
    }

    /// T3's rules through Rust's own answers (`search_cases_the_swift_search_follows`): the same
    /// folds and words, the same match for each thread (quality, strength, what's marked, the
    /// field that says why, its pick, its score), the same order for each list.
    @Test func `search follows Rust's rules exactly`() throws {
        let f = try fixture("conformance/search.json")
        let now = try #require(f["now"]?.int)
        for c in try #require(f["folds"]?.array) {
            let q = try #require(c["query"]?.string)
            #expect(Search.foldQuery(q) == c["folded"]?.string, "\(q)")
        }
        for c in try #require(f["tokens"]?.array) {
            let q = try #require(c["query"]?.string)
            let t = Search.messageTokens(q)
            #expect(t.long == c["long"]?.array?.compactMap(\.string), "\(q)")
            #expect(t.short == c["short"]?.array?.compactMap(\.string), "\(q)")
            #expect(t.words == c["words"]?.array?.compactMap(\.string), "\(q)")
            #expect(Search.findsMessages(q) == c["messages"]?.bool, "\(q)")
        }
        let cases = try #require(f["threads"]?.array)
        #expect(cases.count > 100)
        for c in cases {
            let q = try #require(c["query"]?.string)
            let typos = c["typos"]?.bool ?? false
            let thread = try #require(c["thread"])
            let s = subject(thread)
            let m = Search.scoreThread(Search.foldQuery(q), s, now: now, typos: typos)
            let want = c["match"]
            let label = "\(q) in \(s.title) (typos \(typos))"
            guard let want, !want.isNull else {
                #expect(m == nil, "\(label): \(String(describing: m))")
                continue
            }
            let got = try #require(m, "\(label)")
            #expect(abs(got.quality - (want["quality"]?.double ?? -1)) < 1e-9, "\(label): quality \(got.quality)")
            #expect(abs(got.score - (want["score"]?.double ?? -1)) < 1e-9, "\(label): score \(got.score)")
            #expect(got.strong == want["strong"]?.bool, "\(label)")
            #expect(Search.byteRanges(s.title, got.ranges) == ranges(want["ranges"]), "\(label): ranges")
            #expect(got.pickedAt == want["picked_at"]?.int, "\(label)")
            #expect(got.pickedExactly == want["picked_exactly"]?.bool, "\(label)")
            let hint = want["hint"].flatMap { $0.isNull ? nil : $0 }
            #expect(got.hint?.label == hint?["label"]?.string && got.hint?.text == hint?["text"]?.string, "\(label): hint \(String(describing: got.hint))")
            if let h = got.hint { #expect(Search.byteRanges(h.text, h.ranges) == ranges(hint?["ranges"]), "\(label): hint ranges") }
        }
        for c in try #require(f["ranks"]?.array) {
            let q = try #require(c["query"]?.string)
            let threads = try #require(c["threads"]?.array).map(subject)
            let order = Search.rank(threads, q, now: now) { $0 }.map(\.item.title)
            #expect(order == c["order"]?.array?.compactMap(\.string), "\(q)")
        }
        for c in try #require(f["picked"]?.array) {
            #expect(Search.pickedQuery(c["query"]?.string ?? "") == c["picked"]?.string)
        }
        for c in try #require(f["boosts"]?.array) {
            let b = Search.usageBoost(activity: c["activity"]?.int, opens: Opens(json: c["opens"]), now: now)
            #expect(abs(b - (c["boost"]?.double ?? -1)) < 1e-9)
        }
    }

    func subject(_ v: JSON) -> Search.Subject {
        Search.Subject(
            id: v["id"]?.string,
            title: v["title"]?.string ?? "",
            branch: v["branch"]?.string,
            project: v["project"]?.string,
            prs: (v["prs"]?.array ?? []).map { p in Search.PrRef(number: p["number"]?.int ?? 0, title: p["title"]?.string, references: (p["references"]?.array ?? []).compactMap(\.string)) },
            activity: v["activity"]?.int,
            opens: Opens(json: v["opens"])
        )
    }

    func ranges(_ v: JSON?) -> [Range<Int>] {
        (v?.array ?? []).compactMap { m in
            guard let p = m.array, p.count == 2, let a = p[0].int, let b = p[1].int else { return nil }
            return Int(a)..<Int(b)
        }
    }

    @Test func `a hit Flotilla brought says who from`() throws {
        let answer = try JSON(parsing: #"""
        {"Ok": {"hits": [
          {"thread": "t1", "title": "Palindrome", "kind": "prompt", "seq": 4, "snippet": "Correctness: no bug.", "from": "From Review palindrome.py", "hits": 3},
          {"thread": "t2", "title": "Other", "kind": "prompt", "seq": 2, "snippet": "Add tests."}
        ]}}
        """#)
        guard case .success(.hits(let hits))? = Reply.result(answer) else { Issue.record("not hits"); return }
        #expect(hits.map(\.from) == ["From Review palindrome.py", nil])
        #expect(hits.map(\.hits) == [3, 0])
    }

    @Test func `messages are searched for words of three characters or two Chinese, Japanese or Korean`() {
        #expect(Search.findsMessages("retry"))
        #expect(Search.findsMessages("部署"))
        #expect(!Search.findsMessages("df -h"), "short words only narrow")
        #expect(!Search.findsMessages("ls"))
        #expect(!Search.findsMessages(" "))
        // A control character (a NUL) only separates words, as on the server.
        #expect(Search.messageTokens("retry\u{0}loop\u{1}x").words == ["retry", "loop", "x"])
        #expect(Search.foldQuery("Retry\u{0}Loop") == "retry loop")
        // Quotes inside control characters still come off, as on the server.
        #expect(Search.foldQuery("\u{0}\"retry\"\u{0}") == "retry")
        #expect(Search.messageTokens("\u{0}\"retry\"\u{0}").words == ["retry"])
    }

    @Test func `an open is sent with the query it was picked for`() {
        #expect(Request.opened("t1", query: "retry").params == ["thread": "t1", "query": "retry"])
        #expect(Request.opened("t1").params == ["thread": "t1", "query": .null])
        #expect(Request.opened("t1").method == "thread.opened")
    }
}

@Suite struct Reaching {
    func detail(_ n: Int64) -> (ThreadDetail, [Message]) {
        let all = (0..<n).map { Message(seq: $0, kind: "text", text: "m\($0)") }
        let older = max(0, (n - Paging.window)) / Paging.page * Paging.page
        let pages = Dictionary(grouping: all, by: { Paging.pageOf($0.seq) }).map { PageMark(page: $0.key, mark: $0.value.compactMap(\.text).joined(separator: "|")) }.sorted { $0.page < $1.page }
        var j: [String: JSON] = ["id": .string("t"), "older": .int(older), "messages": .array([])]
        j["pages"] = .array(pages.map { .object(["page": .int($0.page), "mark": .string($0.mark)]) })
        var d = try! ThreadDetail(json: .object(j))
        d.messages = Array(all[Int(older)...])
        return (d, all)
    }

    func page(_ all: [Message], _ p: Int64) -> HistoryPage {
        let messages = all.filter { Paging.pageOf($0.seq) == p }
        var hp = try! HistoryPage(json: .object(["thread": .string("t"), "page": .int(p), "mark": .string(messages.compactMap(\.text).joined(separator: "|")), "messages": .array([])]))
        hp.messages = messages
        return hp
    }

    /// As Rust's `reaching_an_old_message_fetches_down_to_its_page_a_few_at_a_time`.
    @Test func `reaching an old message fetches down to its page a few at a time`() {
        let (d, all) = detail(3146)
        var h = History()
        var r = h.reach(170)
        #expect(!r, "nothing to reach before the view")
        h.view(d)
        #expect(h.shows(3100) && !h.shows(170) && !h.shows(3146))
        r = h.reach(3050)
        #expect(!r, "in the view already")
        r = h.reach(4000) || h.reach(-1)
        #expect(!r)
        r = h.reach(170)
        #expect(r)
        r = h.reach(170) || h.reach(200)
        #expect(!r, "wanted already")
        #expect(h.loading)
        var w = h.wanted(upTo: 4)
        #expect(w == [59, 58, 57, 56])
        w = h.wanted(upTo: 0)
        #expect(w.isEmpty)
        for p in [59, 58, 57, 56] as [Int64] { h.page(page(all, p)) }
        #expect(h.messages.first?.seq == 2800 && !h.shows(170))
        var fetched = 4
        while true {
            let pages = h.wanted(upTo: 4)
            if pages.isEmpty { break }
            fetched += pages.count
            for p in pages.reversed() { h.page(page(all, p)) }
        }
        #expect(fetched == 57, "pages 59 down to 3")
        #expect(h.shows(170) && !h.loading)
        #expect(h.messages.first?.seq == 150)
        let m = h.more()
        #expect(m)
        w = h.wanted()
        #expect(w == [2])
        r = h.reach(10)
        #expect(r)
        w = h.wanted()
        #expect(w == [1, 0])
    }
}
