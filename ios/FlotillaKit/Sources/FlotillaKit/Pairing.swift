import Foundation

/// Pairing, as `flotilla_proto::pair` reads it: one-time codes, and the links that carry
/// them. A link is `flotilla://pair?u=<the server's URL, percent-encoded>#<code>`
/// (`flotilla-dev://` from a dev build); the code is in the fragment.
public enum Pairing {
    /// What a code is made of: no 0, O, 1 or I to misread.
    public static let alphabet = Set("23456789ABCDEFGHJKLMNPQRSTUVWXYZ")
    public static let codeLength = 12
    public static let schemes = ["flotilla", "flotilla-dev"]

    /// `abcd efgh-jklm` → `ABCDEFGHJKLM`, if it's a code at all.
    public static func normalize(code: String) -> String? {
        let code = String(code.filter { $0 != "-" && $0 != " " && $0 != "\t" }.uppercased())
        return code.count == codeLength && code.allSatisfy(alphabet.contains) ? code : nil
    }

    /// A code in three fours, for reading out: `ABCD-EFGH-JKLM`.
    public static func display(code: String) -> String {
        stride(from: 0, to: code.count, by: 4).map { i in
            let a = code.index(code.startIndex, offsetBy: i)
            return String(code[a..<(code.index(a, offsetBy: 4, limitedBy: code.endIndex) ?? code.endIndex)])
        }.joined(separator: "-")
    }

    /// What a pasted link (or a typed server address) says.
    public struct Target: Sendable, Hashable {
        /// `https://host` or `http://host:port`.
        public var server: String
        public var code: String?
    }

    public struct Problem: Error, Sendable, Hashable, CustomStringConvertible {
        public let description: String
        public init(_ description: String) { self.description = description }
    }

    /// Read a pairing link, a web pairing link (`https://host/pair#CODE`), or a server's address.
    public static func read(_ text: String) throws(Problem) -> Target {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let (before, fragment): (Substring, Substring?) = if let i = text.firstIndex(of: "#") {
            (text[..<i], text[text.index(after: i)...])
        } else {
            (text[...], nil)
        }
        var code: String?
        if let f = fragment?.trimmingCharacters(in: .whitespaces), !f.isEmpty {
            guard let c = normalize(code: f) else { throw Problem("The link's code isn't right") }
            code = c
        }
        let lower = before.lowercased()
        let server: String
        if let scheme = schemes.first(where: { lower.hasPrefix("\($0)://") }) {
            var rest = before.dropFirst(scheme.count + 3)
            guard rest.hasPrefix("pair") else { throw Problem("That isn't a pairing link") }
            rest = rest.dropFirst(4)
            while rest.hasPrefix("/") { rest = rest.dropFirst() }
            guard rest.hasPrefix("?") else { throw Problem("That isn't a pairing link") }
            guard let u = rest.dropFirst().split(separator: "&").first(where: { $0.hasPrefix("u=") }) else {
                throw Problem("The link doesn't say which server")
            }
            guard let decoded = String(u.dropFirst(2)).removingPercentEncoding else { throw Problem("The link's server isn't readable") }
            server = decoded
        } else if before.hasSuffix("/pair/") {
            server = String(before.dropLast(6))
        } else if before.hasSuffix("/pair") {
            server = String(before.dropLast(5))
        } else {
            server = String(before)
        }
        return Target(server: try serverURL(server), code: code)
    }

    /// A server's address as typed, as a URL: `ws(s)://` reads as `http(s)://`, and with no
    /// scheme `host:port` is `http://` and a bare host `https://` (a Tailscale name).
    public static func serverURL(_ text: String) throws(Problem) -> String {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let lower = text.lowercased()
        var scheme: String
        var rest: Substring
        if let p = ["https://", "wss://"].first(where: lower.hasPrefix) {
            scheme = "https"
            rest = text.dropFirst(p.count)
        } else if let p = ["http://", "ws://"].first(where: lower.hasPrefix) {
            scheme = "http"
            rest = text.dropFirst(p.count)
        } else if lower.contains("://") {
            throw Problem("\(text) isn't a server's address (http:// or https://)")
        } else {
            scheme = hasPort(text[...]) ? "http" : "https"
            rest = text[...]
        }
        while rest.hasSuffix("/") { rest = rest.dropLast() }
        guard !rest.isEmpty else { throw Problem("Say the server's address") }
        let colonOK = !rest.contains(":") || hasPort(rest) || (rest.hasPrefix("[") && rest.hasSuffix("]"))
        if rest.contains(where: { "/?#@ ".contains($0) }) || !colonOK {
            throw Problem("\(text) isn't just a server's address: give its host (and port), like https://flotilla.tail1234.ts.net")
        }
        return "\(scheme)://\(rest)"
    }

    /// `host:port` (and `[v6]:port`), not a bare host or `[v6]`.
    private static func hasPort(_ authority: Substring) -> Bool {
        guard let i = authority.lastIndex(of: ":") else { return false }
        let host = authority[..<i]
        let port = authority[authority.index(after: i)...]
        return !port.isEmpty && port.allSatisfy(\.isASCII) && port.allSatisfy(\.isNumber) && (!host.contains(":") || host.hasSuffix("]"))
    }

    /// What pairing gives a device: its id, its token (only this once), and the server's.
    public struct Paired: Sendable {
        public let device: String
        public let token: String
        public let server: ServerInfo
    }

    /// Swap a pairing code for this device's token at `server` (`POST /v1/pair`). The error
    /// says why not, for the user (the server's own words when it gave any).
    public static func pair(server: String, code: String, deviceName: String, platform: String = "ios", session: URLSession = .shared) async throws(Problem) -> Paired {
        let server = try serverURL(server)
        guard let code = normalize(code: code) else { throw Problem("That code isn't right: it's 12 letters and digits, like ABCD-EFGH-JKMN") }
        guard let url = URL(string: server + pairPath) else { throw Problem("\(server) isn't a server's address") }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.cachePolicy = .reloadIgnoringLocalCacheData
        req.timeoutInterval = 30
        let body: JSON = ["code": .string(code), "device_name": .string(deviceName), "platform": .string(platform)]
        req.httpBody = Data(body.text.utf8)
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: req)
        } catch {
            throw Problem("Can't reach \(server): \(error.localizedDescription)")
        }
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard status == 200 else { throw Problem(text.isEmpty ? "The server said \(status)" : text) }
        guard let json = try? JSON(parsing: data), let device = json["device"]?.string, let token = json["token"]?.string,
              let id = json["server"]?["id"]?.string
        else { throw Problem("\(server) didn't answer as a Flotilla server") }
        return Paired(device: device, token: token, server: ServerInfo(id: id, version: json["server"]?["version"]?.string ?? ""))
    }
}

// A token or a code never shows in a log or a debugger's description.
extension Pairing.Paired: CustomStringConvertible, CustomDebugStringConvertible {
    public var description: String { "Paired(device: \(device), server: \(server.id), token: …)" }
    public var debugDescription: String { description }
}

extension Pairing.Target: CustomStringConvertible, CustomDebugStringConvertible {
    public var description: String { "Target(server: \(server), code: \(code == nil ? "none" : "…"))" }
    public var debugDescription: String { description }
}
