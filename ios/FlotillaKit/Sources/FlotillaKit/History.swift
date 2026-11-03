import Foundation

/// Long threads, as `flotilla_proto::history` pages them. A thread's view holds only its newest
/// whole pages of `page` seqs (at least `window` messages), how many come before them (`older`),
/// and every page's mark. Older pages come one at a time over HTTP
/// (`GET /v1/threads/:id/history?before=<seq>`), and a tool string over `cap` bytes comes cut,
/// with the whole of it at `GET /v1/threads/:id/full`.
public enum Paging {
    public static let page: Int64 = 50
    public static let window: Int64 = 100
    /// The longest tool input or result sent whole, in bytes.
    public static let cap = 32 * 1024

    /// The page a seq is on.
    public static func pageOf(_ seq: Int64) -> Int64 {
        seq >= 0 ? seq / page : -((-seq + page - 1) / page)
    }

    /// The `before` that fetches a page.
    public static func before(_ page: Int64) -> Int64 {
        (page + 1) * Paging.page
    }
}

/// A thread's older pages beside its view: which are held and still good (by their marks),
/// which to fetch, and the messages to show (the pages held, then the view's).
/// `flotilla_proto::History`, line for line; the conformance tests replay Rust's recording.
///
/// - A page leaving the view as it moves on is kept from the view's copy if its mark didn't
///   change meanwhile, so nothing on screen goes.
/// - A page whose mark changed is shown as it was until its fetch comes back.
/// - Pages the user hasn't scrolled to aren't fetched.
public struct History: Sendable {
    struct Held: Sendable {
        var mark: String
        var messages: [Message]
    }

    /// The view as last seen: where it starts, its marks, its messages.
    private var start: Int64 = 0
    private var marks: [Int64: String] = [:]
    private var window: [Message] = []
    private var seen = false
    /// Older pages held: page → its mark when fetched or kept, and its messages.
    private var held: [Int64: Held] = [:]
    /// The oldest page wanted (the user scrolled up to it); nil: just the view.
    private var want: Int64?
    /// Pages asked for, with the view's mark for each when asked: not asked again until that
    /// changes (or the ask fails).
    private var asked: [Int64: String] = [:]

    public init() {}

    /// The first page in the view.
    private var first: Int64 { Paging.pageOf(start) }

    /// A new view of the thread.
    public mutating func view(_ d: ThreadDetail) {
        let marks = Dictionary(d.pages.map { ($0.page, $0.mark) }, uniquingKeysWith: { _, b in b })
        let first = Paging.pageOf(d.older)
        if seen, self.first < first {
            // Pages that left the view, kept from its copy when they didn't change meanwhile.
            for page in self.first..<first {
                guard let was = self.marks[page], let now = marks[page] else { continue }
                let messages = window.filter { Paging.pageOf($0.seq) == page }
                if was == now, Int64(messages.count) == Paging.page {
                    held[page] = Held(mark: now, messages: messages)
                    want = min(want ?? page, page)
                }
            }
        }
        // Nothing held that's in the view now, or that's gone.
        held = held.filter { $0.key < first && marks[$0.key] != nil }
        asked = asked.filter { $0.key < first && marks[$0.key] != nil }
        if let w = want, w >= first { want = nil }
        start = d.older
        self.marks = marks
        window = d.messages
        seen = true
    }

    /// A fetched page. One fetched at an older mark that answers late never replaces the
    /// page held at the mark the view has now.
    public mutating func page(_ p: HistoryPage) {
        guard let now = marks[p.page] else { return }
        let current = held[p.page]?.mark == now
        if p.page < first, p.mark == now || !current {
            held[p.page] = Held(mark: p.mark, messages: p.messages)
        }
    }

    /// A fetch failed: it can be asked for again.
    public mutating func failed(_ page: Int64) {
        asked[page] = nil
    }

    /// The pages shown above the view: held ones, from the view down, while unbroken.
    private var shown: [Int64] {
        let want = want ?? first
        var out: [Int64] = []
        var p = first - 1
        while p >= want, held[p] != nil {
            out.append(p)
            p -= 1
        }
        return out
    }

    /// The messages to show, in seq order: the older pages held, then the view's.
    public var messages: [Message] {
        shown.reversed().flatMap { held[$0]?.messages ?? [] } + window
    }

    /// The oldest seq shown.
    private var lowest: Int64 {
        shown.last.map { $0 * Paging.page } ?? start
    }

    /// There are older messages than those shown.
    public var moreAbove: Bool { seen && lowest > 0 }

    /// Some page wanted isn't here yet.
    public var loading: Bool {
        let want = want ?? first
        return want < first && (want..<first).contains { held[$0] == nil }
    }

    /// The user scrolled near the top: want the page above what's shown. False if there's
    /// none, or the one wanted last hasn't come yet.
    public mutating func more() -> Bool {
        guard moreAbove, !loading else { return false }
        want = Paging.pageOf(lowest) - 1
        return true
    }

    /// Open at the message `seq` (a search hit): want every page from the view down to the one
    /// that holds it, so it shows once they're in (only an unbroken run shows). False when
    /// nothing more is wanted: it's in the view or wanted already, or there's no such page.
    public mutating func reach(_ seq: Int64) -> Bool {
        guard seen, seq >= 0, seq < start else { return false }
        let page = Paging.pageOf(seq)
        if marks[page] == nil { return false }
        if let w = want, w <= page { return false }
        want = page
        return true
    }

    /// Whether the message `seq` is among those shown.
    public func shows(_ seq: Int64) -> Bool {
        seen && (seq >= start ? window.contains { $0.seq == seq } : seq >= lowest)
    }

    /// The pages to fetch now, nearest first: wanted, and missing or changed, and not already
    /// asked for at the mark the view has.
    public mutating func wanted() -> [Int64] {
        wanted(upTo: Int.max)
    }

    /// As `wanted()`, at most `room` of them (fetches a client still has room for): the rest
    /// stay wanted, for when these come in.
    public mutating func wanted(upTo room: Int) -> [Int64] {
        guard let want, want < first else { return [] }
        var out: [Int64] = []
        for p in stride(from: first - 1, through: want, by: -1) {
            if out.count >= room { break }
            guard let mark = marks[p] else { continue }
            let good = held[p]?.mark == mark
            if good || asked[p] == mark { continue }
            asked[p] = mark
            out.append(p)
        }
        return out
    }
}

/// Reading what a thread's view leaves out, over the server's HTTP with the device's token.
public enum Reading {
    /// A thread's page of older messages: the one that ends at seq `before`.
    public static func page(server: String, token: String, thread: String, before: Int64, session: URLSession = Uploads.session) async throws(Uploads.Problem) -> HistoryPage {
        let data = try await get(server: server, token: token, path: "/v1/threads/\(escape(thread))/history?before=\(before)", session: session)
        guard let json = try? JSON(parsing: data), let page = try? HistoryPage(json: json) else { throw Uploads.Problem("The server's page of messages didn't read") }
        return page
    }

    /// A string a message carries cut short, whole.
    public static func full(server: String, token: String, thread: String, seq: Int64, cut: Cut, session: URLSession = Uploads.session) async throws(Uploads.Problem) -> String {
        let path = "/v1/threads/\(escape(thread))/full?seq=\(seq)&field=\(escape(cut.field))&mark=\(escape(cut.mark))"
        let data = try await get(server: server, token: token, path: path, session: session)
        return String(decoding: data, as: UTF8.self)
    }

    private static func escape(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? s
    }

    private static func get(server: String, token: String, path: String, session: URLSession) async throws(Uploads.Problem) -> Data {
        guard let base = try? Pairing.serverURL(server), let url = URL(string: base + path) else { throw Uploads.Problem("\(server) isn't a server's address") }
        var req = URLRequest(url: url)
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        req.cachePolicy = .reloadIgnoringLocalCacheData
        req.timeoutInterval = 60
        let answer: Data
        let response: URLResponse
        do {
            (answer, response) = try await session.data(for: req)
        } catch {
            throw Uploads.Problem("Can't reach the server: \(error.localizedDescription)")
        }
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        switch status {
        case 200: return answer
        case 401: throw Uploads.Problem(TransportError.unauthorized.description, unpaired: true)
        default:
            let text = String(decoding: answer, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            throw Uploads.Problem(text.isEmpty ? "The server said \(status)" : text)
        }
    }
}
