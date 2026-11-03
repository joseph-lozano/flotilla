import Foundation
import Testing
@testable import FlotillaKit

/// The push relay's encryption against `relay/test/vectors.json`, which Node made and the
/// Rust side (`push.rs`: `the_encryption_is_the_vectors_node_made`) checks too: what a server
/// seals, CryptoKit opens, byte for byte.
@Suite struct PushRelay {
    struct Vectors: Decodable {
        struct Seal: Decodable {
            let nonce: String
            let plaintext: String
            let ciphertext: String
        }

        let key: String
        let other_key: String
        let seals: [Seal]
    }

    static let vectors: Vectors = {
        let root = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let data = try! Data(contentsOf: root.appending(path: "relay/test/vectors.json"))
        return try! JSONDecoder().decode(Vectors.self, from: data)
    }()

    static func bytes(hex: String) -> Data {
        Data(stride(from: 0, to: hex.count, by: 2).map { i in
            let s = hex.index(hex.startIndex, offsetBy: i)
            return UInt8(hex[s..<hex.index(s, offsetBy: 2)], radix: 16)!
        })
    }

    @Test func `the server's ciphertext opens, and seals the same`() {
        let v = Self.vectors
        #expect(v.seals.count == 3)
        for s in v.seals {
            #expect(PushSeal.open(s.ciphertext, key: v.key) == Data(s.plaintext.utf8))
            #expect(PushSeal.seal(Data(s.plaintext.utf8), key: v.key, nonce: Self.bytes(hex: s.nonce)) == s.ciphertext)
            // Another key, or a byte changed, opens nothing.
            #expect(PushSeal.open(s.ciphertext, key: v.other_key) == nil)
            var changed = Data(base64Encoded: s.ciphertext)!
            changed[changed.count - 1] ^= 1
            #expect(PushSeal.open(changed.base64EncodedString(), key: v.key) == nil)
        }
        #expect(PushSeal.open("not base64", key: v.key) == nil)
        #expect(PushSeal.open("AAAA", key: v.key) == nil)
        #expect(PushSeal.open(v.seals[0].ciphertext, key: "AAAA") == nil)
        // A new key each time, and a fresh nonce each seal.
        let key = PushSeal.newKey()
        #expect(Data(base64Encoded: key)?.count == 32 && key != PushSeal.newKey())
        let a = PushSeal.seal(Data("hello".utf8), key: key)!
        #expect(a != PushSeal.seal(Data("hello".utf8), key: key)!)
        #expect(PushSeal.open(a, key: key) == Data("hello".utf8))
    }

    @Test func `what opens is the notification its server wrote`() throws {
        let v = Self.vectors
        let first = try #require(PushNote(json: PushSeal.open(v.seals[0].ciphertext, key: v.key)!))
        #expect(first.title == "Fix flaky retry test")
        #expect(first.body == "Finished. PR #42 is open and its 12 checks pass.")
        #expect(first.threadID == "t1")
        #expect(first.flotilla == ["server": "srv_3f5c1a7e92d4", "kind": "turn_finished", "thread": "t1"])
        // When it was sealed: fresh for a day and an hour, then not (replayed).
        #expect(first.sentAt == 1793664000)
        let sealed = Date(timeIntervalSince1970: 1793664000)
        #expect(first.fresh(at: sealed.addingTimeInterval(60)))
        #expect(first.fresh(at: sealed.addingTimeInterval(PushNote.maxAge)))
        #expect(!first.fresh(at: sealed.addingTimeInterval(PushNote.maxAge + 1)))
        var unstamped = first
        unstamped.sentAt = nil
        #expect(!unstamped.fresh(at: sealed))
        let second = try #require(PushNote(json: PushSeal.open(v.seals[1].ciphertext, key: v.key)!))
        #expect(second.title == "Café ☕ «naïve» 日本語 🚢")
        // A project's setup: no thread.
        let third = try #require(PushNote(json: PushSeal.open(v.seals[2].ciphertext, key: v.key)!))
        #expect(third.flotilla["thread"] == nil && third.flotilla["kind"] == "setup_failed")
        #expect(PushNote(json: Data("{}".utf8)) == nil)
    }

    @Test func `push register carries what the relay gave`() {
        let r = RelayRegistration(relay: "https://push.useflotilla.com", server: "srv_3f5c1a7e92d4", handle: "giLjuhbbFalUBROEtNqwBg", secret: "c2VjcmV0", key: "a2V5", token: "00ff", env: "sandbox")
        // flotilla-proto's `push_requests_on_the_wire` reads the same.
        #expect(Up.req(id: 5, .registerPush(token: "00ff", env: "sandbox", topic: "com.useflotilla.flotilla", relay: r)).json.text
            == #"{"id":5,"m":"push.register","p":{"env":"sandbox","relay":{"handle":"giLjuhbbFalUBROEtNqwBg","key":"a2V5","secret":"c2VjcmV0","url":"https://push.useflotilla.com"},"token":"00ff","topic":"com.useflotilla.flotilla"},"t":"req"}"#)
    }

    @Test func `leaving a server keeps its handle until the relay forgets it`() {
        let r = RelayRegistration(relay: "https://push.useflotilla.com", server: "a", handle: "h", secret: "s", key: "k", token: "t", env: "sandbox")
        var state = RelayState(current: r)
        #expect(state.keys == ["k"])
        state.leave(server: "b")
        #expect(state.current == r)
        let then = Date(timeIntervalSince1970: 1793664000)
        state.leave(server: "a", at: then)
        #expect(state.current == nil && state.keys.isEmpty)
        #expect(state.leftovers == [RelayLeftover(relay: r.relay, handle: "h", secret: "s", since: then)])
        #expect(RelayClient.answer(204) == .done && RelayClient.answer(404) == .gone && RelayClient.answer(429) == .failed("429"))
        // Kept before leftovers had a date: read as left now.
        let old = try! JSONDecoder().decode(RelayLeftover.self, from: Data(#"{"relay":"https://push.useflotilla.com","handle":"h","secret":"s"}"#.utf8))
        #expect(old.handle == "h" && Date().timeIntervalSince(old.since) < 60)
    }

    @Test func `a relay written two ways is one, as the server compares it`() {
        // flotilla_core::push's `a_relay_written_two_ways_is_one`.
        for same in ["https://push.useflotilla.com", "HTTPS://PUSH.useflotilla.com/", "https://push.useflotilla.com:443", " https://push.useflotilla.com\t"] {
            #expect(RelayAddress.normalized(same) == "https://push.useflotilla.com", "\(same)")
        }
        #expect(RelayAddress.normalized("http://LOCALHOST:8796/") == "http://localhost:8796")
        #expect(RelayAddress.normalized("https://Example.com/Relay/") == "https://example.com/Relay")
        #expect(RelayAddress.normalized("ftp://example.com") == nil)
        #expect(RelayAddress.normalized("not a url") == nil)
    }

    @Test func `push register's answer says which relay the server uses`() {
        // flotilla-proto's `push_requests_on_the_wire`.
        #expect(Reply(json: try! JSON(parsing: #"{"push":{"relay":"https://push.useflotilla.com"}}"#)) == .push(relay: "https://push.useflotilla.com"))
        #expect(Reply(json: try! JSON(parsing: #"{"push":{}}"#)) == .push(relay: nil))
        // An older server's.
        #expect(Reply(json: .string("done")) == .done)
    }
}
