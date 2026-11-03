import Foundation
import Testing
@testable import FlotillaKit

@Suite struct JSONSemantics {
    @Test func `numbers keep serde_json's meaning`() throws {
        #expect(try JSON(parsing: "2") == .int(2))
        #expect(try JSON(parsing: "2.0") == .double(2))
        #expect(try JSON(parsing: "2") != JSON(parsing: "2.0"))
        #expect(try JSON(parsing: "-7") == .int(-7))
        #expect(try JSON(parsing: "1e3") == .double(1000))
        #expect(try JSON(parsing: "1793452678124") == .int(1_793_452_678_124))
    }

    @Test func `objects are unordered and text round-trips`() throws {
        let a = try JSON(parsing: #"{"b": [1, 2.5, "x"], "a": {"z": null, "y": true}}"#)
        let b = try JSON(parsing: #"{"a":{"y":true,"z":null},"b":[1,2.5,"x"]}"#)
        #expect(a == b)
        #expect(a.text == #"{"a":{"y":true,"z":null},"b":[1,2.5,"x"]}"#)
        #expect(try JSON(parsing: a.text) == a)
    }

    @Test func `strings unescape, surrogate pairs included`() throws {
        #expect(try JSON(parsing: #""a\"b\\c\n\u00e9\u65e5\ud83d\ude00""#) == .string("a\"b\\c\né日😀"))
        #expect(try JSON(parsing: #""é ✓""#) == .string("é ✓"))
        let s = JSON.string("tab\there \"q\" \u{1}")
        #expect(try JSON(parsing: s.text) == s)
    }

    @Test(arguments: ["", "{", "[1,]", "{\"a\" 1}", "tru", "\"\\ud83d\"", "1 2", "\"\\x\""])
    func `bad JSON is refused`(_ text: String) {
        #expect(throws: JSONError.self) { try JSON(parsing: text) }
    }
}

/// `flotilla_proto::replica`'s own tests, in Swift.
@Suite struct ReplicaRules {
    @Test func `checks revisions and hands on only changes`() {
        var r = Replica()
        r.welcome(epoch: "e1")
        r.subscribe(.catalog)
        #expect(r.held() == [Held(view: .catalog, rev: nil)])
        // The first snapshot is handed on even if it's empty; the same again isn't.
        guard case .changed(.catalog(nil)) = r.snap(.catalog, rev: 3, data: .null) else { Issue.record("first"); return }
        guard case .same = r.snap(.catalog, rev: 4, data: .null) else { Issue.record("same"); return }
        #expect(r.rev(.catalog) == 4)
        // A stale or repeated delta is skipped; one from a revision not held is a doubt.
        guard case .same = r.delta(.catalog, base: 2, rev: 4, ops: []) else { Issue.record("stale"); return }
        guard case .doubt = r.delta(.catalog, base: 5, rev: 6, ops: []) else { Issue.record("gap"); return }
        #expect(r.rev(.catalog) == nil)
        // While a snapshot is awaited, deltas wait too.
        guard case .same = r.delta(.catalog, base: 4, rev: 7, ops: []) else { Issue.record("waiting"); return }
        guard case .changed(.catalog(let c?)) = r.snap(.catalog, rev: 8, data: ["providers": []]) else { Issue.record("catalog"); return }
        #expect(c.providers.isEmpty)
        // Another server run: nothing held counts.
        r.welcome(epoch: "e2")
        #expect(r.held()[0].rev == nil)
        // Not subscribed: ignored.
        guard case .same = r.snap(.settings, rev: 1, data: [:]) else { Issue.record("unsubscribed"); return }
    }

    @Test func `a delta that doesn't fit or doesn't read is a doubt`() {
        var r = Replica()
        let t = ViewKey.mentions("t1")
        r.subscribe(t)
        guard case .changed = r.snap(t, rev: 1, data: ["thread": "t1", "mentions": nil]) else { Issue.record("first"); return }
        // A thread view whose messages aren't a list.
        let th = ViewKey.thread("t1")
        r.subscribe(th)
        guard case .changed = r.snap(th, rev: 1, data: ["id": "t1", "messages": []]) else { Issue.record("thread"); return }
        guard case .doubt = r.delta(th, base: 1, rev: 2, ops: [.set(field: "messages", v: 5)]) else { Issue.record("unreadable"); return }
        #expect(r.rev(th) == nil)
        // A snapshot of another thread under this key doesn't read: kept, not asked for again.
        guard case .unreadable = r.snap(t, rev: 3, data: ["thread": "t2", "mentions": nil]) else { Issue.record("other key"); return }
    }

    @Test func `a doubt lets go of the data and keeps what was shown`() {
        var r = Replica()
        let t = ViewKey.mentions("t1")
        r.subscribe(t)
        let first: JSON = ["thread": "t1", "mentions": nil]
        guard case .changed = r.snap(t, rev: 1, data: first) else { Issue.record("first"); return }
        // A delta that changes a field and then fails: none of it is held.
        guard case .doubt = r.delta(t, base: 1, rev: 2, ops: [.set(field: "mentions", v: []), .del(field: "mentions", keys: ["x"])]) else { Issue.record("half"); return }
        #expect(r.rev(t) == nil && r.data(t) == nil)
        // Its snapshot, the same as was shown, isn't handed on again.
        guard case .same = r.snap(t, rev: 3, data: first) else { Issue.record("shown"); return }
        #expect(r.rev(t) == 3 && r.data(t) == first)
        // A gap, and data that doesn't read as the view: the same.
        guard case .doubt = r.delta(t, base: 4, rev: 5, ops: []) else { Issue.record("gap"); return }
        #expect(r.rev(t) == nil && r.data(t) == nil)
        // A snapshot that doesn't read as the view: kept with its revision, and not asked for
        // again (asking gets the same); what was shown stays.
        let odd: JSON = ["thread": "t2", "mentions": nil]
        guard case .unreadable = r.snap(t, rev: 6, data: odd) else { Issue.record("other thread"); return }
        #expect(r.rev(t) == 6 && r.data(t) == odd)
        guard case .same = r.snap(t, rev: 7, data: first) else { Issue.record("still shown"); return }
        // Another server run lets go of it too.
        r.welcome(epoch: "e2")
        #expect(r.rev(t) == nil && r.data(t) == nil)
    }

    /// As Rust's: a snapshot that doesn't read (a newer server's) isn't asked for again, which
    /// would only get the same: it's kept with its revision, deltas go on applying to it, and
    /// once it reads it's shown.
    @Test func `a snapshot that doesn't read is kept, not asked for again`() {
        var r = Replica()
        let th = ViewKey.thread("t1")
        r.subscribe(th)
        guard case .unreadable(th, _) = r.take(.snap(th, rev: 1, data: ["id": "t1", "messages": 5])) else { Issue.record("snapshot"); return }
        #expect(r.rev(th) == 1)
        guard case .unreadable = r.delta(th, base: 1, rev: 2, ops: [.set(field: "extra", v: 1)]) else { Issue.record("still"); return }
        #expect(r.rev(th) == 2)
        guard case .changed = r.delta(th, base: 2, rev: 3, ops: [.set(field: "messages", v: [])]) else { Issue.record("reads"); return }
        #expect(r.rev(th) == 3)
    }

    @Test func `a device paired as an agent reads as one`() throws {
        let j = try JSON(parsing: #"{"devices":[{"id":"a","name":"Claude Code","platform":"macos","paired_ms":1,"last_seen_ms":2,"connected":true,"scope":"agent"},{"id":"b","name":"iPhone","platform":"ios","paired_ms":1,"last_seen_ms":2,"connected":false,"scope":"full"}]}"#)
        guard case .devices(let list) = try ServerView(key: .devices, data: j) else { Issue.record("not devices"); return }
        #expect(list.map { $0.agent } == [true, false])
    }

    @Test func `views come in Rust's order`() {
        var r = Replica()
        for v in [ViewKey.thread("b"), .devices, .thread("a"), .sidebar, .catalog] { r.subscribe(v) }
        #expect(r.subscribed == [.sidebar, .catalog, .thread("a"), .thread("b"), .devices])
    }
}

/// `flotilla_client`'s session rules: what's sent again after a drop, and what's skipped.
@Suite struct SessionRules {
    func welcome(resumed: Bool, seen: ReqID? = nil, epoch: String = "e1") -> Welcome {
        Welcome(proto: 1, server: ServerInfo(id: "s", version: "0"), epoch: epoch, resumed: resumed, seen: seen, caps: [], device: "d")
    }

    /// The welcome, taken as every frame is.
    func welcomed(_ core: inout SessionCore, _ w: Welcome) -> (send: [Up], lost: [ReqID]) {
        guard case .welcomed(_, let send, let lost) = core.received(.welcome(w)) else {
            Issue.record("not welcomed")
            return ([], [])
        }
        return (send, lost)
    }

    func live(_ core: inout SessionCore) {
        _ = core.hello(app: "a", version: "0", platform: "ios")
        _ = welcomed(&core, welcome(resumed: false))
    }

    @Test func `a resumed session sends again only what the server didn't see`() {
        var core = SessionCore(id: "s1")
        live(&core)
        _ = core.app(.req(id: 1, .markViewed("t")))
        _ = core.app(.req(id: 2, .archive("t")))
        _ = core.app(.req(id: 3, .markUnread("t")))
        core.lost(unsent: [])
        _ = core.app(.req(id: 4, .unarchive("t")))
        _ = core.hello(app: "a", version: "0", platform: "ios")
        let (ups, lost) = welcomed(&core, welcome(resumed: true, seen: 2))
        #expect(lost.isEmpty)
        #expect(ups == [.req(id: 3, .markUnread("t")), .req(id: 4, .unarchive("t"))])
        #expect(Set(core.inflight.keys) == [1, 2, 3, 4])
    }

    @Test func `a fresh session sends again what's safe and fails the rest`() {
        var core = SessionCore(id: "s1")
        live(&core)
        _ = core.app(.req(id: 1, .send(thread: "t", text: "hi", local: "local-1")))
        _ = core.app(.req(id: 2, .archive("t")))
        guard case .deliver = core.received(.res(id: 9, result: .success(.done), n: 5)) else { Issue.record("res"); return }
        #expect(core.got == 5)
        core.lost(unsent: [])
        _ = core.hello(app: "a", version: "0", platform: "ios")
        let (ups, lost) = welcomed(&core, welcome(resumed: false, epoch: "e2"))
        #expect(ups == [.req(id: 1, .send(thread: "t", text: "hi", local: "local-1"))])
        #expect(lost == [2])
        // Its numbering starts again.
        #expect(core.got == nil)
        guard case .deliver = core.received(.note(Note(json: ["kind": "turn_finished", "data": "t"])!, n: 1)) else { Issue.record("note"); return }
    }

    @Test func `numbered messages already had are skipped`() {
        var core = SessionCore(id: "s1")
        live(&core)
        guard case .deliver = core.received(.res(id: 1, result: .success(.done), n: 3)) else { Issue.record("first"); return }
        guard case .skipped = core.received(.res(id: 1, result: .success(.done), n: 3)) else { Issue.record("again"); return }
        guard case .skipped = core.received(.note(Note(json: ["kind": "x"])!, n: 2)) else { Issue.record("older"); return }
    }

    @Test func `views opened while away are subscribed after the hello, not held in it`() {
        var core = SessionCore(id: "s1")
        live(&core)
        _ = core.app(.sub(.sidebar))
        _ = core.received(.snap(.sidebar, rev: 4, data: ["threads": []]))
        core.lost(unsent: [])
        _ = core.app(.sub(.thread("t1")))
        _ = core.app(.sub(.thread("t2")))
        _ = core.app(.unsub(.thread("t2")))
        guard case .hello(let h) = core.hello(app: "a", version: "0", platform: "ios") else { Issue.record("hello"); return }
        #expect(h.subs == [Held(view: .sidebar, rev: 4)])
        #expect(h.epoch == "e1")
        let (ups, _) = welcomed(&core, welcome(resumed: true))
        #expect(ups == [.sub(.thread("t1"))])
    }

    @Test func `what the app sends before the welcome goes after it, unsubscribes too`() {
        var core = SessionCore(id: "s1")
        live(&core)
        _ = core.app(.sub(.thread("t1")))
        core.lost(unsent: [])
        // The hello holds t1 (its snapshot hadn't come), so the server subscribes it again...
        guard case .hello(let h) = core.hello(app: "a", version: "0", platform: "ios") else { Issue.record("hello"); return }
        #expect(h.subs == [Held(view: .thread("t1"), rev: nil)])
        // ...and closing it before the welcome must still reach the server, after the welcome.
        #expect(core.app(.unsub(.thread("t1"))).isEmpty)
        #expect(core.app(.req(id: 7, .cachedRepos)).isEmpty)
        let (ups, _) = welcomed(&core, welcome(resumed: true))
        #expect(ups == [.unsub(.thread("t1")), .req(id: 7, .cachedRepos)])
        #expect(!core.replica.isSubscribed(.thread("t1")))
        #expect(core.inflight.keys.contains(7))
    }

    @Test func `what the app sent before a welcome that never came waits as if sent while away`() {
        var core = SessionCore(id: "s1")
        _ = core.hello(app: "a", version: "0", platform: "ios")
        _ = core.app(.sub(.sidebar))
        _ = core.app(.req(id: 1, .cachedRepos))
        core.lost(unsent: [])
        #expect(core.waiting == [.sub(.sidebar), .req(id: 1, .cachedRepos)])
        #expect(core.replica.isSubscribed(.sidebar))
    }

    @Test func `what wasn't written waits, in order; an unsubscribe and a resync don't`() {
        var core = SessionCore(id: "s1")
        live(&core)
        _ = core.app(.req(id: 1, .archive("t")))
        _ = core.app(.sub(.thread("x")))
        core.lost(unsent: [.req(id: 1, .archive("t")), .unsub(.sidebar), .resync(.catalog), .sub(.thread("x"))])
        #expect(core.inflight.isEmpty)
        #expect(core.waiting == [.req(id: 1, .archive("t")), .sub(.thread("x"))])
    }

    @Test func `a view opened and closed while the socket died isn't opened again`() {
        var core = SessionCore(id: "s1")
        live(&core)
        let ups = core.app(.sub(.thread("t1"))) + core.app(.unsub(.thread("t1")))
        // Neither was written before the socket went.
        core.lost(unsent: ups)
        #expect(core.waiting.isEmpty)
        guard case .hello(let h) = core.hello(app: "a", version: "0", platform: "ios") else { Issue.record("hello"); return }
        #expect(h.subs.isEmpty)
    }

    @Test func `the request being written when the socket died is left to seen`() {
        var core = SessionCore(id: "s1")
        live(&core)
        _ = core.app(.req(id: 1, .archive("t")))
        let queued = core.app(.req(id: 2, .markUnread("t")))
        // 1 was being written (it may have arrived); 2, queued behind it, never was.
        core.lost(unsent: queued)
        #expect(Array(core.inflight.keys) == [1])
        #expect(core.waiting == [.req(id: 2, .markUnread("t"))])
        _ = core.hello(app: "a", version: "0", platform: "ios")
        // It arrived: it isn't sent again.
        let (ups, lost) = welcomed(&core, welcome(resumed: true, seen: 1))
        #expect(ups == [.req(id: 2, .markUnread("t"))] && lost.isEmpty)
        _ = core.received(.res(id: 1, result: .success(.done), n: 1))
        _ = core.received(.res(id: 2, result: .success(.done), n: 2))
        #expect(core.inflight.isEmpty)
        // A new session: whether it arrived can't be known, so it isn't run twice; it's lost.
        _ = core.app(.req(id: 3, .archive("t")))
        core.lost(unsent: [])
        _ = core.hello(app: "a", version: "0", platform: "ios")
        let (again, gone) = welcomed(&core, welcome(resumed: false, epoch: "e2"))
        #expect(again.isEmpty && gone == [3])
    }

    @Test func `a doubt asks for the view whole, and data that doesn't read doesn't`() {
        var core = SessionCore(id: "s1")
        live(&core)
        _ = core.app(.sub(.catalog))
        guard case .deliver = core.received(.snap(.catalog, rev: 4, data: ["providers": []])) else { Issue.record("snap"); return }
        guard case .resync(.catalog, _) = core.received(.delta(.catalog, base: 5, rev: 6, ops: [])) else { Issue.record("doubt"); return }
        #expect(core.replica.rev(.catalog) == nil)
        guard case .unreadable(.catalog, _) = core.received(.snap(.catalog, rev: 7, data: ["providers": "not a list"])) else { Issue.record("unreadable"); return }
        // Kept, with its revision, and not asked for again.
        #expect(core.replica.rev(.catalog) == 7)
    }

    @Test func `a refusal is said once`() {
        var core = SessionCore(id: "s1")
        guard case .refuse(_, .some) = core.refuse("revoked") else { Issue.record("first"); return }
        guard case .refuse(_, nil) = core.refuse("revoked") else { Issue.record("again"); return }
    }

    @Test func `before the welcome only a welcome or an error will do`() {
        var core = SessionCore(id: "s1")
        guard case .nothing = core.received(.synced) else { Issue.record("no connection"); return }
        _ = core.hello(app: "a", version: "0", platform: "ios")
        guard case .fail = core.received(.res(id: 1, result: .success(.done), n: 1)) else { Issue.record("an answer"); return }
        // Not taken as had.
        #expect(core.got == nil)
        guard case .fail = core.received(.synced) else { Issue.record("synced"); return }
        #expect(core.phase == .hello)
        guard case .welcomed = core.received(.welcome(welcome(resumed: false))) else { Issue.record("welcome"); return }
        #expect(core.phase == .live)
        // A welcome once welcomed is nothing to the session.
        guard case .nothing = core.received(.welcome(welcome(resumed: true))) else { Issue.record("again"); return }
    }

    @Test func `an error after the welcome is said once, and again after the next welcome`() {
        var core = SessionCore(id: "s1")
        live(&core)
        guard case .refuse("revoked", .connection(.refused("revoked"))?) = core.received(.error("revoked")) else { Issue.record("live"); return }
        core.lost(unsent: [])
        _ = core.hello(app: "a", version: "0", platform: "ios")
        guard case .refuse("revoked", nil) = core.received(.error("revoked")) else { Issue.record("before the welcome"); return }
        core.lost(unsent: [])
        live(&core)
        #expect(core.refused == nil)
        guard case .refuse("revoked", .connection(.refused("revoked"))?) = core.received(.error("revoked")) else { Issue.record("after the next welcome"); return }
    }
}

/// `flotilla_proto::pair`'s tests, in Swift.
@Suite struct PairingRules {
    @Test func `links carry the server in the query and the code in the fragment`() throws {
        let l = "flotilla://pair?u=https%3A%2F%2Fflotilla.tail1234.ts.net#ABCDEFGHJKMN"
        #expect(try Pairing.read(l) == Pairing.Target(server: "https://flotilla.tail1234.ts.net", code: "ABCDEFGHJKMN"))
        #expect(try Pairing.read("flotilla-dev://pair?u=http%3A%2F%2F127.0.0.1%3A7777#ABCDEFGHJKMN").server == "http://127.0.0.1:7777")
        #expect(try Pairing.read("  flotilla://pair?u=http%3a%2f%2f127.0.0.1%3a7777#abcd-efgh-jkmn ").code == "ABCDEFGHJKMN")
        #expect(try Pairing.read("https://box.tail1.ts.net/pair#ABCDEFGHJKMN") == Pairing.Target(server: "https://box.tail1.ts.net", code: "ABCDEFGHJKMN"))
        #expect(try Pairing.read("box.tail1.ts.net") == Pairing.Target(server: "https://box.tail1.ts.net", code: nil))
        for bad in ["flotilla://pair#ABCDEFGHJKMN", "flotilla://other?u=x", "flotilla://pair?u=https%3A%2F%2Fx#WRONG", "ftp://x", "https://x/some/path", ""] {
            #expect(throws: Pairing.Problem.self, "\(bad)") { try Pairing.read(bad) }
        }
    }

    @Test func `server addresses read as URLs`() throws {
        for (typed, url) in [
            ("https://a.ts.net/", "https://a.ts.net"), ("wss://a.ts.net", "https://a.ts.net"), ("ws://127.0.0.1:7777", "http://127.0.0.1:7777"),
            ("127.0.0.1:7777", "http://127.0.0.1:7777"), ("[::1]:7777", "http://[::1]:7777"), ("a.ts.net", "https://a.ts.net"), ("HTTP://Box:80", "http://Box:80"),
        ] {
            #expect(try Pairing.serverURL(typed) == url, "\(typed)")
        }
        for bad in ["", "https://", "https:", "https://user@host", "https://host/path", "https://host?x", "file:///etc", "host:port"] {
            #expect(throws: Pairing.Problem.self, "\(bad)") { try Pairing.serverURL(bad) }
        }
    }

    @Test func `codes read however typed`() {
        #expect(Pairing.normalize(code: " abcd efgh-jkmn ") == "ABCDEFGHJKMN")
        #expect(Pairing.display(code: "ABCDEFGHJKMN") == "ABCD-EFGH-JKMN")
        for bad in ["", "ABCDEFGHJKM", "ABCDEFGHJKMNP", "ABCDEFGHJKM0", "ABCDEFGHJKMI"] {
            #expect(Pairing.normalize(code: bad) == nil, "\(bad)")
        }
    }

    @Test func `a token never shows in a description`() {
        let p = Pairing.Paired(device: "d1", token: "fdt_0123456789abcdef_secret", server: ServerInfo(id: "s", version: "1"))
        #expect(!String(describing: p).contains("secret"))
        #expect(!String(reflecting: p).contains("secret"))
        #expect(!String(describing: Pairing.Target(server: "https://x", code: "ABCDEFGHJKMN")).contains("ABCD"))
    }
}
