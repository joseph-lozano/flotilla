import Foundation

/// What a file holds, for its icon, as `flotilla_proto::Kind` names it from the file's name.
public enum FileKind: String, Sendable, Hashable {
    case image, text, code, data, sheet, archive, video, audio, binary, other

    /// By the file's extension (`Kind::of_name`).
    public init(name: String) {
        let ext = (name as NSString).pathExtension.lowercased()
        switch ext {
        case "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "ico", "heic", "heif", "avif": self = .image
        case "txt", "log", "md", "markdown", "rst", "rtf", "pdf", "doc", "docx", "odt", "pages": self = .text
        case "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "go", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "m", "mm", "cs", "rb", "php", "sh",
             "bash", "zsh", "fish", "html", "htm", "css", "scss", "sass", "less", "sql", "lua", "ex", "exs", "erl", "hs", "ml", "scala", "dart", "r", "jl", "vue",
             "svelte", "zig", "pl", "diff", "patch": self = .code
        case "json", "jsonl", "ndjson", "yaml", "yml", "toml", "xml", "ini", "plist": self = .data
        case "csv", "tsv", "xls", "xlsx", "numbers", "ods": self = .sheet
        case "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar": self = .archive
        case "mp4", "mov", "webm", "ogv", "mkv", "avi", "m4v": self = .video
        case "mp3", "wav", "ogg", "flac", "m4a", "aac", "opus", "aiff": self = .audio
        case "exe", "bin", "dmg", "so", "dylib", "dll", "o", "a", "wasm", "class", "jar", "iso", "sqlite", "db": self = .binary
        default: self = .other
        }
    }
}

/// boat's limits on one prompt's files, on their bytes (`flotilla_core::attach`): checked here
/// before anything goes up, as the desktop does.
public enum AttachLimits {
    public static let maxFiles = 12
    /// boat's megabytes are binary.
    public static let mb: Int64 = 1024 * 1024
    public static let maxFileMB: Int64 = 25
    public static let maxTotalMB: Int64 = 64

    /// Why files of these names and sizes can't go with one prompt, if they can't.
    public static func overLimit(_ files: [(name: String, size: Int64)]) -> String? {
        if files.count > maxFiles { return "\(maxFiles) files at most" }
        if let f = files.first(where: { $0.size > maxFileMB * mb }) { return "\(f.name) is over \(maxFileMB) MB" }
        if files.reduce(0, { $0 + $1.size }) > maxTotalMB * mb { return "Over \(maxTotalMB) MB in total" }
        return nil
    }
}

/// `PUT /v1/blobs`: a file kept on the server by its contents, for a prompt to name.
public enum Uploads {
    public static let path = "/v1/blobs"

    public struct Problem: Error, Sendable, Hashable, CustomStringConvertible {
        public let description: String
        /// The server doesn't know this device's token any more.
        public let unpaired: Bool
        public init(_ description: String, unpaired: Bool = false) {
            self.description = description
            self.unpaired = unpaired
        }
    }

    /// The stored file's id (`b_<sha256 hex>`) and size.
    public struct Blob: Sendable, Hashable {
        public let id: String
        public let size: Int64
    }

    /// Upload `data` to `server` (`http(s)://host[:port]`) with the device's token.
    public static func put(_ data: Data, server: String, token: String, session: URLSession = Uploads.session) async throws(Problem) -> Blob {
        guard !data.isEmpty else { throw Problem("An empty file") }
        guard Int64(data.count) <= AttachLimits.maxTotalMB * AttachLimits.mb else { throw Problem("Over \(AttachLimits.maxTotalMB) MB") }
        guard let base = try? Pairing.serverURL(server), let url = URL(string: base + path) else { throw Problem("\(server) isn't a server's address") }
        var req = URLRequest(url: url)
        req.httpMethod = "PUT"
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        req.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
        req.cachePolicy = .reloadIgnoringLocalCacheData
        req.timeoutInterval = 120
        let answer: Data
        let response: URLResponse
        do {
            (answer, response) = try await session.upload(for: req, from: data)
        } catch {
            throw Problem("Can't reach the server: \(error.localizedDescription)")
        }
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        let text = String(decoding: answer, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        switch status {
        case 200: break
        case 401: throw Problem(TransportError.unauthorized.description, unpaired: true)
        case 413: throw Problem("The server takes files up to \(AttachLimits.maxTotalMB) MB")
        default: throw Problem(text.isEmpty ? "The server said \(status)" : text)
        }
        guard let json = try? JSON(parsing: answer), let id = json["id"]?.string, id.hasPrefix("b_") else { throw Problem("The server didn't say where it kept the file") }
        return Blob(id: id, size: json["size"]?.int ?? Int64(data.count))
    }

    /// Nothing kept: no cache, no cookies (the token goes in a header each time).
    public static let session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.urlCache = nil
        config.httpCookieStorage = nil
        return URLSession(configuration: config)
    }()
}
