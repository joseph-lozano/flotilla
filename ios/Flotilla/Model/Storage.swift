import Foundation
import Security

/// The device's token for a server, in the Keychain: this device only, readable after the
/// first unlock (so a reconnect in the background works), never in a file or the defaults.
enum Keychain {
    private static let service = "com.useflotilla.flotilla.device-token"

    static func token(server: String) -> String? {
        if case .found(let t) = lookup(server: server) { t } else { nil }
    }

    /// What the Keychain has for `server`.
    enum Lookup: Equatable {
        case found(String)
        /// None kept (forgotten, or a restore onto another phone: tokens stay on their phone).
        case missing
        /// It didn't answer (`OSStatus`); it may next time.
        case failed(OSStatus)
    }

    /// `server`'s token, asked a few times if the Keychain fails (it can, for a moment).
    static func lookup(server: String) -> Lookup {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: server,
            kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var status = errSecSuccess
        for attempt in 0..<3 {
            if attempt > 0 { Thread.sleep(forTimeInterval: 0.2) }
            var item: CFTypeRef?
            status = SecItemCopyMatching(query as CFDictionary, &item)
            #if DEBUG
            // UI tests: a launch whose Keychain doesn't answer.
            if ProcessInfo.processInfo.arguments.contains("-FlotillaKeychainFails") { status = errSecInteractionNotAllowed }
            #endif
            if status == errSecItemNotFound { return .missing }
            if status == errSecSuccess, let data = item as? Data, let token = String(data: data, encoding: .utf8) { return .found(token) }
        }
        return .failed(status)
    }

    /// Keep `token` for `server`, in place of any before. False if the Keychain refused.
    @discardableResult
    static func set(_ token: String, server: String) -> Bool {
        delete(server: server)
        let item: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: server,
            kSecValueData as String: Data(token.utf8), kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecAttrSynchronizable as String: false,
        ]
        return SecItemAdd(item as CFDictionary, nil) == errSecSuccess
    }

    static func delete(server: String) {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: server]
        SecItemDelete(query as CFDictionary)
    }

    #if DEBUG
    /// The servers there's a token for, by id (tests only).
    static func servers() -> [String] {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
            kSecReturnAttributes as String: true, kSecMatchLimit as String: kSecMatchLimitAll,
        ]
        var items: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &items) == errSecSuccess, let list = items as? [[String: Any]] else { return [] }
        return list.compactMap { $0[kSecAttrAccount as String] as? String }.sorted()
    }
    #endif
}

/// The server this phone is paired with: where it is and who it is there. No secrets: its
/// token is in the Keychain under its id.
struct PairedServer: Codable, Hashable, Identifiable {
    /// `https://host` or `http://host:port`.
    var url: String
    /// The server's id (kept with its database, the same across restarts).
    var id: String
    /// This phone's device id there.
    var device: String

    /// What Settings calls it: the host's first label ("box" for
    /// `box.tail1234.ts.net`), or the address (with its port) for an IP.
    var name: String {
        let u = URL(string: url)
        let host = u?.host() ?? url
        if host.allSatisfy({ $0.isNumber || $0 == "." || $0 == ":" }) || host == "localhost" {
            return u?.port.map { "\(host):\($0)" } ?? host
        }
        return host.split(separator: ".").first.map(String.init) ?? host
    }

    private static let key = "pairedServer"
    /// Before one server per phone: every server paired with, and the one in use.
    private static let listKey = "pairedServers"

    /// The server this phone is paired with, in the defaults. A phone that kept several keeps
    /// the one it was using: the others' tokens leave the Keychain, and they're returned with
    /// them, to be told to revoke this phone.
    static func load(_ defaults: UserDefaults = .standard) -> (server: PairedServer?, dropped: [(server: PairedServer, token: String?)]) {
        guard let data = defaults.data(forKey: listKey) else {
            return (defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(PairedServer.self, from: $0) }, [])
        }
        struct List: Decodable {
            var all: [PairedServer] = []
            var current: String?
        }
        let list = (try? JSONDecoder().decode(List.self, from: data)) ?? List()
        let kept = list.all.first { $0.id == list.current }
        let dropped = list.all.filter { $0.id != kept?.id }.map { s in
            let token = Keychain.token(server: s.id)
            Keychain.delete(server: s.id)
            return (server: s, token: token)
        }
        save(kept, defaults)
        return (kept, dropped)
    }

    /// Keep `server` as the one this phone is paired with (nil: none).
    static func save(_ server: PairedServer?, _ defaults: UserDefaults = .standard) {
        if let server, let data = try? JSONEncoder().encode(server) {
            defaults.set(data, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
        defaults.removeObject(forKey: listKey)
    }
}
