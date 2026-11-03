import CryptoKit
import Foundation
import Security

// The phone's side of Flotilla's push relay (relay/README.md). A server without the app's APNs
// key sends through the relay; the phone registers with the relay, gives its server what it
// got and a key of its own (`push.register`), and its Notification Service Extension opens what
// arrives (`PushSeal.open`) into the notification the server would have sent Apple.

/// Flotilla's push encryption: ChaCha20-Poly1305 with the phone's 32-byte key, a 12-byte
/// nonce, no associated data; base64 of nonce ‖ ciphertext ‖ tag (`ChaChaPoly.SealedBox`'s
/// `combined`). The same as `flotilla_core::push::seal`, checked against `relay/test/vectors.json`.
public enum PushSeal {
    /// A new key, as base64.
    public static func newKey() -> String {
        SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }.base64EncodedString()
    }

    /// What a server sealed with `key` (base64), if it did.
    public static func open(_ sealed: String, key: String) -> Data? {
        guard let key = Data(base64Encoded: key), key.count == 32,
              let combined = Data(base64Encoded: sealed), combined.count >= 28,
              let box = try? ChaChaPoly.SealedBox(combined: combined)
        else { return nil }
        return try? ChaChaPoly.open(box, using: SymmetricKey(data: key))
    }

    /// `plaintext` sealed with `key` (base64), with `nonce` if given (tests), else a random one.
    public static func seal(_ plaintext: Data, key: String, nonce: Data? = nil) -> String? {
        guard let key = Data(base64Encoded: key), key.count == 32 else { return nil }
        let n = try? nonce.map { try ChaChaPoly.Nonce(data: $0) }
        guard let box = try? ChaChaPoly.seal(plaintext, using: SymmetricKey(data: key), nonce: n ?? ChaChaPoly.Nonce()) else { return nil }
        return box.combined.base64EncodedString()
    }
}

/// A notification as its server wrote it (`flotilla_core::push::payload`), from what the
/// extension opened.
public struct PushNote: Sendable, Equatable {
    public var title: String
    public var body: String
    /// Notification Center groups by it.
    public var threadID: String?
    /// Where a tap goes: `server`, `kind`, and `thread` if it's about one.
    public var flotilla: [String: String]
    /// When its server sealed it (Unix seconds), if it said.
    public var sentAt: Int64?

    /// How old a note may be and still show its words: APNs keeps one up to a day, and the
    /// server tries a refused one again for up to an hour.
    public static let maxAge: TimeInterval = 25 * 3600

    /// Sealed recently enough to show, at `now`: one replayed later, or with no time, shows as
    /// the relay sent it (the neutral alert).
    public func fresh(at now: Date) -> Bool {
        guard let sentAt else { return false }
        return now.timeIntervalSince1970 - TimeInterval(sentAt) <= Self.maxAge
    }

    public init?(json data: Data) {
        guard let o = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let aps = o["aps"] as? [String: Any],
              let alert = aps["alert"] as? [String: Any],
              let title = alert["title"] as? String,
              let body = alert["body"] as? String
        else { return nil }
        self.title = title
        self.body = body
        threadID = aps["thread-id"] as? String
        let ours = o["flotilla"] as? [String: Any] ?? [:]
        flotilla = ours.compactMapValues { $0 as? String }
        sentAt = (ours["sent_at"] as? NSNumber)?.int64Value
    }
}

/// What this phone registered with the relay for its server, kept in the Keychain the app and
/// its extension share: what the server is given, and the key the extension opens with.
public struct RelayRegistration: Codable, Sendable, Equatable {
    /// The relay, `https://push.useflotilla.com`.
    public var relay: String
    /// The server it's for (its id).
    public var server: String
    public var handle: String
    public var secret: String
    /// The phone's own key (base64): only the phone and its server have it.
    public var key: String
    /// The APNs token and environment the relay has.
    public var token: String
    public var env: String

    public init(relay: String, server: String, handle: String, secret: String, key: String, token: String, env: String) {
        self.relay = relay
        self.server = server
        self.handle = handle
        self.secret = secret
        self.key = key
        self.token = token
        self.env = env
    }
}

/// A handle the phone has left (Forget, pairing with another server) that the relay may still
/// have: it's asked to forget it until it does, or for a week.
public struct RelayLeftover: Codable, Sendable, Equatable {
    public var relay: String
    public var handle: String
    public var secret: String
    /// When the phone left it.
    public var since: Date

    public init(relay: String, handle: String, secret: String, since: Date = Date()) {
        self.relay = relay
        self.handle = handle
        self.secret = secret
        self.since = since
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        relay = try c.decode(String.self, forKey: .relay)
        handle = try c.decode(String.self, forKey: .handle)
        secret = try c.decode(String.self, forKey: .secret)
        since = try c.decodeIfPresent(Date.self, forKey: .since) ?? Date()
    }
}

/// Everything about the relay this phone keeps.
public struct RelayState: Codable, Sendable, Equatable {
    public var current: RelayRegistration?
    public var leftovers: [RelayLeftover] = []

    public init(current: RelayRegistration? = nil, leftovers: [RelayLeftover] = []) {
        self.current = current
        self.leftovers = leftovers
    }

    /// Leave `current` (if it's `server`'s, or any with nil): it becomes a leftover, and its key
    /// goes with it.
    public mutating func leave(server: String? = nil, at now: Date = Date()) {
        guard let c = current, server == nil || c.server == server else { return }
        leftovers.append(RelayLeftover(relay: c.relay, handle: c.handle, secret: c.secret, since: now))
        current = nil
    }

    /// What the extension tries: the current key.
    public var keys: [String] { current.map { [$0.key] } ?? [] }
}

/// A relay's address as phones and servers compare it: the scheme and host in lower case, no
/// default port, no trailing `/` (as `flotilla_core::push::relay_url` keeps it).
public enum RelayAddress {
    public static func normalized(_ address: String) -> String? {
        guard let c = URLComponents(string: address.trimmingCharacters(in: .whitespacesAndNewlines)),
              let scheme = c.scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = c.host?.lowercased(), !host.isEmpty
        else { return nil }
        let port = c.port.flatMap { (scheme == "https" && $0 == 443) || (scheme == "http" && $0 == 80) ? nil : $0 }
        var path = c.path
        while path.hasSuffix("/") { path.removeLast() }
        let at = host.contains(":") && !host.hasPrefix("[") ? "[\(host)]" : host
        return "\(scheme)://\(at)\(port.map { ":\($0)" } ?? "")\(path)"
    }
}

/// The relay's state in the Keychain, in the access group the app and its extension share:
/// this device only, readable after the first unlock (a notification can come while locked).
public enum RelayKeychain {
    private static let service = "com.useflotilla.flotilla.push-relay"
    private static let account = "state"

    /// What the Keychain has.
    public enum Loaded: Sendable, Equatable {
        case found(RelayState)
        /// Nothing kept.
        case missing
        /// It didn't answer, or what it had isn't readable: it may next time.
        case failed(OSStatus)

        /// The state, empty if there's none; nil if the Keychain didn't answer.
        public var state: RelayState? {
            switch self {
            case .found(let s): s
            case .missing: RelayState()
            case .failed: nil
            }
        }
    }

    #if DEBUG
    /// Tests: what the Keychain answers instead, to a read and to a write.
    nonisolated(unsafe) public static var failing: (load: OSStatus?, save: OSStatus?) = (nil, nil)
    #endif

    private static func query(_ group: String?) -> [String: Any] {
        var q: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: account]
        if let group { q[kSecAttrAccessGroup as String] = group }
        return q
    }

    /// What's kept, asked a few times if the Keychain fails (it can, for a moment).
    public static func load(group: String?) -> Loaded {
        var q = query(group)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var status = errSecSuccess
        for attempt in 0..<3 {
            if attempt > 0 { Foundation.Thread.sleep(forTimeInterval: 0.1) }
            var item: CFTypeRef?
            status = SecItemCopyMatching(q as CFDictionary, &item)
            #if DEBUG
            if let f = failing.load { status = f }
            #endif
            if status == errSecItemNotFound { return .missing }
            if status == errSecSuccess, let data = item as? Data {
                guard let state = try? JSONDecoder().decode(RelayState.self, from: data) else { return .failed(errSecDecode) }
                return .found(state)
            }
        }
        return .failed(status)
    }

    /// Keep `state` in place of what's there: updated in place, so the extension never reads a
    /// gap, and added only when there's none. False if the Keychain refused; what was there stays.
    @discardableResult
    public static func save(_ state: RelayState, group: String?) -> Bool {
        #if DEBUG
        if let f = failing.save, f != errSecSuccess { return false }
        #endif
        if state == RelayState() {
            let status = SecItemDelete(query(group) as CFDictionary)
            return status == errSecSuccess || status == errSecItemNotFound
        }
        guard let data = try? JSONEncoder().encode(state) else { return false }
        let status = SecItemUpdate(query(group) as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecSuccess { return true }
        guard status == errSecItemNotFound else { return false }
        var item = query(group)
        item[kSecValueData as String] = data
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        item[kSecAttrSynchronizable as String] = false
        return SecItemAdd(item as CFDictionary, nil) == errSecSuccess
    }
}

/// The relay's requests a phone makes (relay/README.md).
public struct RelayClient: Sendable {
    public let url: URL
    let session: URLSession

    public init(url: URL, session: URLSession = .shared) {
        self.url = url
        self.session = session
    }

    public enum Answer: Equatable, Sendable {
        case done
        /// The relay doesn't know the handle (or the secret): register again.
        case gone
        /// It didn't answer, or refused for now: try again later.
        case failed(String)
    }

    private func post(_ path: String, _ body: [String: String]) async -> (Int, Data)? {
        var r = URLRequest(url: url.appending(path: path))
        r.httpMethod = "POST"
        r.timeoutInterval = 10
        r.setValue("application/json", forHTTPHeaderField: "content-type")
        r.httpBody = try? JSONSerialization.data(withJSONObject: body)
        guard let (data, response) = try? await session.data(for: r), let http = response as? HTTPURLResponse else { return nil }
        return (http.statusCode, data)
    }

    /// Register this phone's APNs token: a handle and secret, or why not.
    public func register(token: String, topic: String, env: String) async -> Result<(handle: String, secret: String), Problem> {
        guard let (status, data) = await post("v1/register", ["token": token, "topic": topic, "env": env]) else { return .failure(Problem("The push relay didn't answer")) }
        guard status == 201, let o = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let handle = o["handle"] as? String, let secret = o["secret"] as? String
        else { return .failure(Problem("The push relay refused: \(status)")) }
        return .success((handle, secret))
    }

    /// The relay has `r`'s handle still, now for `token`.
    public func refresh(_ r: RelayRegistration, token: String, env: String) async -> Answer {
        guard let (status, _) = await post("v1/refresh", ["handle": r.handle, "secret": r.secret, "token": token, "env": env]) else { return .failed("no answer") }
        return Self.answer(status)
    }

    /// The relay forgets `handle`.
    public func unregister(handle: String, secret: String) async -> Answer {
        guard let (status, _) = await post("v1/unregister", ["handle": handle, "secret": secret]) else { return .failed("no answer") }
        return Self.answer(status)
    }

    static func answer(_ status: Int) -> Answer {
        switch status {
        case 200, 204: .done
        case 404: .gone
        default: .failed("\(status)")
        }
    }

    public struct Problem: Error, Equatable, Sendable {
        public let message: String
        public init(_ message: String) { self.message = message }
    }
}
