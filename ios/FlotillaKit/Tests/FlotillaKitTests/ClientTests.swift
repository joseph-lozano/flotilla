import Foundation
import Synchronization
import Testing
@testable import FlotillaKit

/// A queue a test can wait on, with a time limit (unlike `AsyncStream`, a wait that gives up
/// doesn't end it).
actor Mailbox<T: Sendable> {
    private var items: [T] = []
    private var waiters: [(UUID, CheckedContinuation<T?, Never>)] = []

    func put(_ x: T) {
        if waiters.isEmpty { items.append(x) } else { waiters.removeFirst().1.resume(returning: x) }
    }

    func take(within: Duration = .seconds(5)) async -> T? {
        if !items.isEmpty { return items.removeFirst() }
        let id = UUID()
        return await withCheckedContinuation { c in
            waiters.append((id, c))
            Task { try? await Task.sleep(for: within); self.expire(id) }
        }
    }

    /// The first that fits, dropping the others before it.
    func first(within: Duration = .seconds(5), where ok: @Sendable (T) -> Bool) async -> T? {
        let until = ContinuousClock.now + within
        while true {
            let left = until - ContinuousClock.now
            guard left > .zero, let x = await take(within: left) else { return nil }
            if ok(x) { return x }
        }
    }

    private func expire(_ id: UUID) {
        if let i = waiters.firstIndex(where: { $0.0 == id }) { waiters.remove(at: i).1.resume(returning: nil) }
    }
}

/// A stand-in server's end of one WebSocket.
final class FakeSocket: WebSocket {
    let request: URLRequest
    private let inbox = Mailbox<Result<String, TransportError>>()
    /// What the client wrote, as it wrote it.
    let written = Mailbox<JSON>()
    let closed = Mutex(false)

    init(request: URLRequest) { self.request = request }

    func send(_ text: String) async throws(TransportError) {
        if closed.withLock({ $0 }) { throw .failed("closed") }
        await written.put(try! JSON(parsing: text))
    }

    func receive() async throws(TransportError) -> String {
        guard let next = await inbox.take(within: .seconds(3600)) else { throw .failed("closed") }
        return try next.get()
    }

    let failPing = Mutex(false)

    func ping() async throws(TransportError) {
        if failPing.withLock({ $0 }) { throw .failed("ping failed") }
    }

    func close() {
        closed.withLock { $0 = true }
        Task { await inbox.put(.failure(.failed("closed"))) }
    }

    /// The server says this.
    func say(_ j: JSON) async { await inbox.put(.success(j.text)) }
    /// The connection breaks.
    func drop() async { await inbox.put(.failure(.failed("The network connection was lost."))) }
    func next() async -> JSON? { await written.take() }
}

/// A stand-in server: hands each connection's socket to the test.
final class FakeServer: Transport {
    let sockets = Mailbox<FakeSocket>()
    let refuse = Mutex<TransportError?>(nil)

    func open(_ request: URLRequest) async throws(TransportError) -> any WebSocket {
        if let e = refuse.withLock({ $0 }) { throw e }
        let s = FakeSocket(request: request)
        await sockets.put(s)
        return s
    }

    func next(within: Duration = .seconds(5)) async -> FakeSocket? { await sockets.take(within: within) }
}

/// What a client hands the app, gathered for a test to wait on.
final class Heard: Sendable {
    let box = Mailbox<ClientEvent>()
    init(_ c: FlotillaClient) {
        let box = self.box
        Task { for await e in c.events { await box.put(e) } }
    }

    func first(_ ok: @Sendable @escaping (ClientEvent) -> Bool) async -> ClientEvent? { await box.first(where: ok) }
}

func isOnline(_ e: ClientEvent) -> Bool { if case .connection(.online) = e { true } else { false } }

func welcome(resumed: Bool, seen: Int64? = nil, epoch: String = "e1") -> JSON {
    ["t": "welcome", "proto": 1, "server": ["id": "srv", "version": "0"], "epoch": .string(epoch), "resumed": .bool(resumed), "seen": seen.map(JSON.int) ?? nil, "caps": [], "device": "d"]
}

@Suite(.timeLimit(.minutes(1))) struct ClientBehaviour {
    func client(_ server: FakeServer) -> FlotillaClient {
        var t = FlotillaClient.Timing()
        t.backoffFirst = .milliseconds(20)
        t.backoffMax = .milliseconds(50)
        return FlotillaClient(server: URL(string: "http://127.0.0.1:7777")!, token: "fdt_test", info: ClientInfo(app: "flotilla-ios", version: "0", platform: "ios"), transport: server, timing: t, session: "sess-1")
    }

    @Test func `it says hello with its token and protocol, and keeps a view`() async throws {
        let server = FakeServer()
        let c = client(server)
        let heard = Heard(c)
        await c.start()
        let s = try #require(await server.next())
        #expect(s.request.url?.absoluteString == "ws://127.0.0.1:7777/v1/ws")
        #expect(s.request.value(forHTTPHeaderField: "Authorization") == "Bearer fdt_test")
        #expect(s.request.value(forHTTPHeaderField: "flotilla-protocol") == "1")
        let hello = try #require(await s.next())
        #expect(hello["t"] == "hello" && hello["session"] == "sess-1" && hello["platform"] == "ios")
        await s.say(welcome(resumed: false))
        await s.say(["t": "synced"])
        #expect(await heard.first { if case .connection(.online) = $0 { true } else { false } } != nil)
        await c.subscribe(.thread("t1"))
        #expect(await s.next() == ["t": "sub", "view": "thread", "key": "t1"])
        await s.say(["t": "snap", "view": "thread", "key": "t1", "rev": 3, "data": ["id": "t1", "messages": [["seq": 0, "kind": "text", "text": "hi"]]]])
        let e = await heard.first { if case .view = $0 { true } else { false } }
        guard case .view(.thread(let t))? = e else { Issue.record("no thread"); return }
        #expect(t.messages.first?.text == "hi")
        await c.stop()
    }

    @Test func `a request waits for its answer`() async throws {
        let server = FakeServer()
        let c = client(server)
        let heard = Heard(c)
        await c.start()
        let s = try #require(await server.next())
        _ = await s.next()
        await s.say(welcome(resumed: false))
        async let answer = c.request(.markViewed("t1"))
        let req = try #require(await s.next())
        #expect(req == ["t": "req", "id": 1, "m": "thread.mark_viewed", "p": "t1"])
        await s.say(["t": "res", "id": 1, "result": ["Ok": "done"], "n": 1])
        #expect(try await answer.get() == .done)
        await c.stop()
    }

    @Test func `after a drop it resumes with what it holds, and sends again what wasn't seen`() async throws {
        let server = FakeServer()
        let c = client(server)
        let heard = Heard(c)
        await c.start()
        let s1 = try #require(await server.next())
        _ = await s1.next()
        await s1.say(welcome(resumed: false))
        await c.subscribe(.sidebar)
        _ = await s1.next()
        await s1.say(["t": "snap", "view": "sidebar", "rev": 5, "data": ["threads": []]])
        await s1.say(["t": "note", "note": ["kind": "turn_finished", "data": "t1"], "n": 4])
        _ = await heard.first { if case .note = $0 { true } else { false } }
        await c.send(.archive("t1"))
        await c.send(.unarchive("t1"))
        _ = await s1.next()
        _ = await s1.next()
        await s1.drop()
        let offline = await heard.first { if case .connection(.offline) = $0 { true } else { false } }
        #expect(offline != nil)
        // It comes back by itself, after its backoff.
        let s2 = try #require(await server.next())
        let hello = try #require(await s2.next())
        #expect(hello["epoch"] == "e1")
        #expect(hello["got"] == 4)
        #expect(hello["subs"] == [["view": "sidebar", "rev": 5]])
        // The server saw the first request, not the second.
        await s2.say(welcome(resumed: true, seen: 1))
        #expect(await s2.next() == ["t": "req", "id": 2, "m": "thread.unarchive", "p": "t1"])
        // The note sent again is skipped; the next one isn't.
        await s2.say(["t": "note", "note": ["kind": "turn_finished", "data": "t1"], "n": 4])
        await s2.say(["t": "note", "note": ["kind": "turn_finished", "data": "t2"], "n": 5])
        let note = await heard.first { if case .note = $0 { true } else { false } }
        guard case .note(let n)? = note else { Issue.record("no note"); return }
        #expect(n.turnFinished == "t2")
        #expect(await c.stats.resumed == 1)
        await c.stop()
    }

    @Test func `a token the server doesn't know is final`() async throws {
        let server = FakeServer()
        server.refuse.withLock { $0 = .unauthorized }
        let c = client(server)
        let heard = Heard(c)
        await c.start()
        let e = await heard.first { if case .connection(.refused) = $0 { true } else { false } }
        guard case .connection(.refused(let why))? = e else { Issue.record("not refused"); return }
        #expect(why.contains("isn't paired"))
        // No more tries, and a request fails at once rather than waiting for ever.
        server.refuse.withLock { $0 = nil }
        #expect(await server.next(within: .milliseconds(300)) == nil)
        guard case .failure(let e) = await c.request(.markViewed("t1")) else { Issue.record("answered"); return }
        #expect(e.message.contains("isn't paired"))
        await c.stop()
    }

    @Test func `in the background it lets go, and comes back at once`() async throws {
        let server = FakeServer()
        let c = client(server)
        let heard = Heard(c)
        await c.start()
        let s1 = try #require(await server.next())
        _ = await s1.next()
        await s1.say(welcome(resumed: false))
        await s1.say(["t": "synced"])
        _ = await heard.first { if case .connection(.online) = $0 { true } else { false } }
        await c.pause()
        #expect(s1.closed.withLock { $0 })
        #expect(await server.next(within: .milliseconds(200)) == nil)
        await c.resume()
        let s2 = try #require(await server.next())
        let hello = try #require(await s2.next())
        #expect(hello["session"] == "sess-1")
        await c.stop()
    }

    @Test func `a ping that fails ends the connection`() async throws {
        let server = FakeServer()
        var t = FlotillaClient.Timing()
        t.pingEvery = .milliseconds(50)
        t.backoffFirst = .milliseconds(20)
        let c = FlotillaClient(server: URL(string: "http://127.0.0.1:7777")!, token: "fdt_test", info: ClientInfo(app: "a", version: "0", platform: "ios"), transport: server, timing: t, session: "s")
        await c.start()
        let s1 = try #require(await server.next())
        _ = await s1.next()
        await s1.say(welcome(resumed: false))
        s1.failPing.withLock { $0 = true }
        // It reconnects: the failed ping didn't count as hearing from the server.
        #expect(await server.next(within: .seconds(2)) != nil)
        await c.stop()
    }

    @Test func `an error from the server is said once, and it keeps trying`() async throws {
        let server = FakeServer()
        let c = client(server)
        let heard = Heard(c)
        await c.start()
        let s1 = try #require(await server.next())
        _ = await s1.next()
        await s1.say(["t": "error", "msg": "That session belongs to another device"])
        let e = await heard.first { if case .connection(.refused) = $0 { true } else { false } }
        #expect(e != nil)
        let s2 = try #require(await server.next())
        _ = await s2.next()
        await s2.say(["t": "error", "msg": "That session belongs to another device"])
        #expect(await server.next() != nil)
        await c.stop()
    }

    @Test func `an error after the welcome is said in the server's words, and it keeps trying`() async throws {
        let server = FakeServer()
        let c = client(server)
        let heard = Heard(c)
        await c.start()
        let s1 = try #require(await server.next())
        _ = await s1.next()
        await s1.say(welcome(resumed: false))
        await s1.say(["t": "synced"])
        #expect(await heard.first(isOnline) != nil)
        // The server revokes the device on a connection it welcomed.
        await s1.say(["t": "error", "msg": "This device was removed"])
        let refused = await heard.first { if case .connection = $0 { true } else { false } }
        guard case .connection(.refused(let why))? = refused else { Issue.record("not refused: \(String(describing: refused))"); return }
        #expect(why == "This device was removed")
        // It tries again; nothing about the connection is said until it's back (not offline).
        let s2 = try #require(await server.next())
        _ = await s2.next()
        await s2.say(welcome(resumed: true))
        await s2.say(["t": "synced"])
        let next = await heard.first { if case .connection = $0 { true } else { false } }
        guard case .connection(.online)? = next else { Issue.record("not online: \(String(describing: next))"); return }
        // Welcomed again, the same error is said again.
        await s2.say(["t": "error", "msg": "This device was removed"])
        let again = await heard.first { if case .connection = $0 { true } else { false } }
        guard case .connection(.refused("This device was removed"))? = again else { Issue.record("not said again: \(String(describing: again))"); return }
        #expect(await server.next() != nil)
        await c.stop()
    }
}
