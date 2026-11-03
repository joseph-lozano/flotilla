import FlotillaKit
import Foundation
import Testing
import UserNotifications

/// The Notification Service Extension's own code (FlotillaNotify), on what the push relay
/// delivers: a ciphertext the server sealed (`relay/test/vectors.json`, which the Rust and Kit
/// tests check too) opens with the key in the shared Keychain group into the server's words and
/// a tap's `flotilla` fields; one it can't open stays the neutral alert. The simulator never
/// runs the extension for `simctl push` (RelayPushUITests covers real delivery).
extension RelayKeychainUsers {
@MainActor
@Suite struct RelayExtension {
    struct Vectors: Decodable {
        struct Seal: Decodable {
            let plaintext: String
            let ciphertext: String
        }

        let key: String
        let other_key: String
        let seals: [Seal]
    }

    static let vectors: Vectors = {
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        return try! JSONDecoder().decode(Vectors.self, from: Data(contentsOf: root.appending(path: "relay/test/vectors.json")))
    }()

    /// What a delivered notification shows, and what a tap reads.
    struct Shown: Sendable {
        let title: String
        let body: String
        let threadIdentifier: String
        let flotilla: [String: String]?
        let sealed: Bool

        init(_ c: UNNotificationContent) {
            title = c.title
            body = c.body
            threadIdentifier = c.threadIdentifier
            flotilla = c.userInfo["flotilla"] as? [String: String]
            sealed = c.userInfo["e"] != nil
        }
    }

    /// What the extension delivers for a relay notification carrying `sealed`, with `key` as
    /// this phone's (the Keychain's state put back after).
    func deliver(_ sealed: String, key: String?) async -> Shown {
        let group = NotificationService.group
        let before = RelayKeychain.load(group: group).state ?? RelayState()
        defer { RelayKeychain.save(before, group: group) }
        let current = key.map { RelayRegistration(relay: "https://push.useflotilla.com", server: "srv_3f5c1a7e92d4", handle: "giLjuhbbFalUBROEtNqwBg", secret: "s", key: $0, token: "t", env: "sandbox") }
        #expect(RelayKeychain.save(RelayState(current: current), group: group), "the shared Keychain group keeps it")
        let content = UNMutableNotificationContent()
        content.title = "Flotilla"
        content.body = "New activity"
        content.userInfo = ["aps": ["alert": ["title": "Flotilla", "body": "New activity"], "mutable-content": 1], "e": sealed]
        let request = UNNotificationRequest(identifier: "relay", content: content, trigger: nil)
        return await withCheckedContinuation { done in
            NotificationService().didReceive(request) { done.resume(returning: Shown($0)) }
        }
    }

    @Test func `the shared group is the team's, whichever team builds it`() throws {
        let group = try #require(NotificationService.group)
        let (team, rest) = (group.prefix(10), group.dropFirst(10))
        #expect(team.allSatisfy { $0.isUppercase || $0.isNumber })
        #expect(rest == ".com.useflotilla.flotilla.shared")
    }

    /// The vectors' first note, sealed `age` seconds ago with the vectors' key.
    func note(sealed age: TimeInterval) -> String {
        let v = Self.vectors
        let at = Int64(Date().timeIntervalSince1970 - age)
        let plaintext = v.seals[0].plaintext.replacingOccurrences(of: #""sent_at":1793664000"#, with: #""sent_at":\#(at)"#)
        #expect(plaintext != v.seals[0].plaintext)
        return PushSeal.seal(Data(plaintext.utf8), key: v.key)!
    }

    @Test func `a relayed notification opens into its server's words`() async {
        let v = Self.vectors
        let shown = await deliver(note(sealed: 60), key: v.key)
        #expect(shown.title == "Fix flaky retry test")
        #expect(shown.body == "Finished. PR #42 is open and its 12 checks pass.")
        #expect(shown.threadIdentifier == "t1")
        #expect(!shown.sealed)
        // What a tap reads (`AppModel.about`).
        #expect(shown.flotilla == ["server": "srv_3f5c1a7e92d4", "kind": "turn_finished", "thread": "t1"])
    }

    @Test func `one it can't open stays the neutral alert`() async {
        let v = Self.vectors
        for key in [v.other_key, nil] as [String?] {
            let shown = await deliver(note(sealed: 60), key: key)
            #expect(shown.title == "Flotilla" && shown.body == "New activity")
            #expect(shown.flotilla == nil && shown.sealed)
        }
        // Sealed more than a day ago (a replay): neutral too.
        let replayed = await deliver(note(sealed: PushNote.maxAge + 60), key: v.key)
        #expect(replayed.title == "Flotilla" && replayed.body == "New activity" && replayed.flotilla == nil)
        let garbled = await deliver("not a ciphertext", key: v.key)
        #expect(garbled.title == "Flotilla" && garbled.body == "New activity")
    }
}
}
