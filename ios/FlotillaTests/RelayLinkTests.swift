import FlotillaKit
import Foundation
import Security
import Testing
@testable import Flotilla

/// A stand-in push relay: each request's path, and what `answer` says to it: a status, a body,
/// and seconds to hold it back (nil: no answer).
nonisolated final class StubRelay: URLProtocol, @unchecked Sendable {
    nonisolated(unsafe) static var answer: (String) -> (Int, String, Double)? = { _ in nil }
    nonisolated(unsafe) static var seen: [String] = []
    nonisolated(unsafe) static var made = 0

    /// A register's answer: a handle of its own each time.
    static func registered() -> (Int, String, Double) {
        made += 1
        let handle = "h\(made)".padding(toLength: 22, withPad: "x", startingAt: 0)
        return (201, #"{"handle":"\#(handle)","secret":"ssss"}"#, 0)
    }

    override class func canInit(with request: URLRequest) -> Bool { request.url?.host() == "relay.test" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        let path = request.url?.path() ?? ""
        Self.seen.append(path)
        guard let (status, body, after) = Self.answer(path) else {
            client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost))
            return
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + after) { [self] in
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: ["content-type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }
    }
    override func stopLoading() {}
}

/// The suites that use the relay's Keychain item, one at a time.
@Suite(.serialized) struct RelayKeychainUsers {}

/// `RelayLink` against a stand-in relay, with the Keychain's state put back after.
extension RelayKeychainUsers {
@MainActor
@Suite struct RelayLinkStates {
    let topic = "com.useflotilla.flotilla"

    func with(_ body: () async throws -> Void) async rethrows {
        let before = RelayKeychain.load(group: RelayLink.group).state ?? RelayState()
        URLProtocol.registerClass(StubRelay.self)
        UserDefaults.standard.set("https://relay.test", forKey: "FlotillaPushRelay")
        defer {
            URLProtocol.unregisterClass(StubRelay.self)
            UserDefaults.standard.removeObject(forKey: "FlotillaPushRelay")
            RelayKeychain.failing = (nil, nil)
            RelayKeychain.save(before, group: RelayLink.group)
            StubRelay.answer = { _ in nil }
            StubRelay.seen = []
        }
        RelayKeychain.save(RelayState(), group: RelayLink.group)
        try await body()
    }

    var stored: RelayState { RelayKeychain.load(group: RelayLink.group).state ?? RelayState() }

    /// The relay answering register with a new handle and everything else with `other`.
    func up(_ other: Int = 204, after: Double = 0) {
        StubRelay.answer = { path in path == "/v1/register" ? StubRelay.registered() : (other, other == 404 ? #"{"error":"No such handle","code":"no_such_handle"}"# : "", after) }
    }

    @Test func `a relay that doesn't answer is tried again, and keeps what the server has`() async {
        await with {
            let server = "srv_\(UUID().uuidString.prefix(8))"
            StubRelay.answer = { _ in nil }
            let down = await RelayLink.ensure(server: server, token: "aa11", env: "sandbox", topic: topic)
            #expect(down == .init(registration: nil, settled: false))
            up()
            let made = await RelayLink.ensure(server: server, token: "aa11", env: "sandbox", topic: topic)
            #expect(made.settled && made.registration != nil)
            #expect(stored.current == made.registration)
            // A new token while it's down again: the last registration stays, not settled.
            StubRelay.answer = { _ in nil }
            let moved = await RelayLink.ensure(server: server, token: "bb22", env: "sandbox", topic: topic)
            #expect(!moved.settled && moved.registration == made.registration)
        }
    }

    @Test func `a refresh moves the handle to a new token, and one the relay forgot is made again with a new key`() async {
        await with {
            let server = "srv_\(UUID().uuidString.prefix(8))"
            up()
            let first = await RelayLink.ensure(server: server, token: "aa11", env: "sandbox", topic: topic).registration!
            let moved = await RelayLink.ensure(server: server, token: "cc33", env: "production", topic: topic)
            #expect(moved.settled && moved.registration?.handle == first.handle && moved.registration?.token == "cc33" && moved.registration?.env == "production")
            #expect(stored.current == moved.registration)
            // The relay lost it.
            up(404)
            let again = await RelayLink.ensure(server: server, token: "dd44", env: "production", topic: topic)
            #expect(again.settled && again.registration?.handle != first.handle && again.registration?.key != first.key)
            #expect(stored.current == again.registration)
        }
    }

    @Test func `a registration for another server is left for this one's`() async throws {
        try await with {
            up()
            let old = await RelayLink.ensure(server: "srv_old", token: "aa11", env: "sandbox", topic: topic).registration!
            let new = await RelayLink.ensure(server: "srv_new", token: "aa11", env: "sandbox", topic: topic).registration!
            #expect(new.handle != old.handle)
            #expect(stored.current == new)
            try await Task.sleep(for: .seconds(1))
            #expect(StubRelay.seen.contains("/v1/unregister"), "the old handle is asked to go")
            #expect(stored.leftovers.isEmpty)
        }
    }

    @Test func `Forget is kept at once, and a late answer can't bring the registration back`() async throws {
        try await with {
            let server = "srv_\(UUID().uuidString.prefix(8))"
            up()
            let made = await RelayLink.ensure(server: server, token: "aa11", env: "sandbox", topic: topic).registration!
            // A refresh (a new token) held back by the relay; Forget meanwhile.
            up(204, after: 0.5)
            let pending = Task { await RelayLink.ensure(server: server, token: "ee55", env: "sandbox", topic: topic) }
            try await Task.sleep(for: .milliseconds(100))
            StubRelay.answer = { _ in nil }
            RelayLink.leave(server: server)
            // Before anything else runs: no key, and the handle is a leftover.
            #expect(stored.current == nil && stored.keys.isEmpty)
            #expect(stored.leftovers.map(\.handle) == [made.handle])
            let late = await pending.value
            #expect(late.registration == nil && !late.settled)
            #expect(stored.current == nil, "the late refresh didn't restore it")
            // A register under way when Forget came: its handle goes too.
            StubRelay.answer = { path in path == "/v1/register" ? { var r = StubRelay.registered(); r.2 = 0.5; return r }() : nil }
            let registering = Task { await RelayLink.ensure(server: server, token: "ff66", env: "sandbox", topic: topic) }
            try await Task.sleep(for: .milliseconds(100))
            RelayLink.leave(server: server)
            #expect(await registering.value.registration == nil)
            #expect(stored.current == nil && stored.leftovers.count == 2)
        }
    }

    @Test func `a launch leaves a registration for a server it isn't paired with`() async {
        await with {
            up()
            let made = await RelayLink.ensure(server: "srv_kept", token: "aa11", env: "sandbox", topic: topic).registration!
            StubRelay.answer = { _ in nil }
            RelayLink.reconcile(paired: "srv_kept")
            #expect(stored.current == made)
            RelayLink.reconcile(paired: nil)
            #expect(stored.current == nil && stored.leftovers.map(\.handle) == [made.handle])
        }
    }

    @Test func `a Keychain that refuses or doesn't answer changes nothing`() async {
        await with {
            let server = "srv_\(UUID().uuidString.prefix(8))"
            up()
            let made = await RelayLink.ensure(server: server, token: "aa11", env: "sandbox", topic: topic).registration!
            // The refresh's write refused: what was kept stays, and it's not settled.
            RelayKeychain.failing = (nil, errSecIO)
            let refused = await RelayLink.ensure(server: server, token: "bb22", env: "sandbox", topic: topic)
            #expect(refused == .init(registration: made, settled: false))
            RelayKeychain.failing = (nil, nil)
            #expect(stored.current == made)
            // Reads fail: what's kept is unknown, not none (push.register waits), and Forget,
            // ensure and sweep change nothing (the next launch reconciles).
            #expect(RelayLink.kept(for: server) == (made, true))
            RelayKeychain.failing = (errSecInteractionNotAllowed, nil)
            #expect(RelayLink.kept(for: server) == (nil, false))
            RelayLink.leave(server: server)
            #expect(await RelayLink.ensure(server: server, token: "aa11", env: "sandbox", topic: topic) == .init(registration: nil, settled: false))
            RelayLink.sweep()
            RelayKeychain.failing = (nil, nil)
            #expect(stored.current == made && stored.leftovers.isEmpty)
        }
    }

    @Test func `handles left are asked to go until the relay forgets them, or for a week`() async throws {
        try await with {
            let fresh = RelayLeftover(relay: "https://relay.test", handle: "gggggggggggggggggggggg", secret: "s")
            let stale = RelayLeftover(relay: "https://relay.test", handle: "kkkkkkkkkkkkkkkkkkkkkk", secret: "s", since: Date().addingTimeInterval(-RelayLink.leftoverFor - 60))
            RelayKeychain.save(RelayState(leftovers: [fresh, stale]), group: RelayLink.group)
            StubRelay.answer = { _ in nil }
            RelayLink.sweep()
            try await Task.sleep(for: .seconds(1))
            #expect(stored.leftovers == [fresh], "kept while the relay doesn't answer; dropped after a week")
            StubRelay.answer = { _ in (204, "", 0) }
            RelayLink.sweep()
            try await Task.sleep(for: .seconds(1))
            #expect(stored.leftovers.isEmpty)
            #expect(StubRelay.seen.last == "/v1/unregister")
        }
    }

    @Test func `the relay a server says it uses, however it's written`() async {
        await with {
            #expect(RelayLink.uses("https://relay.test"))
            #expect(RelayLink.uses("HTTPS://Relay.test:443/"))
            #expect(!RelayLink.uses("https://push.example.com"))
            #expect(!RelayLink.uses(nil))
        }
    }
}
}
