import Foundation

/// Search, as `flotilla_proto::search` has it: T3 Code's rules, line for line, by Unicode scalar
/// as Rust goes by `char`. The server ranks and cuts message hits; the phone ranks titles by the
/// same rules (`mobile/fixtures/conformance/search.json`, recorded from Rust's, keeps the two the
/// same): a whole word beats a word's start beats (four letters or more, titles only) the inside
/// of a word; words typed together count (`vscode`); one typo is forgiven when nothing matches at
/// a word start; picks and use reorder matches of about the same quality.

/// What a hit is: a thread's title, or one of its messages (as `Message.kind` says it).
public enum HitKind: String, Sendable, Hashable {
    case title, prompt, steer
    /// A reply.
    case text
    /// A tool call: its command, or its path, pattern or URL.
    case tool
}

/// One thing a search found: a thread by its title, or a thread by its messages, as its best one.
public struct SearchHit: Sendable, Hashable, Identifiable {
    public var thread: String
    public var title: String
    public var kind: HitKind
    /// The message's seq (none for a title): where its thread opens.
    public var seq: Int64?
    /// A line around the match; for a title, the title.
    public var snippet: String
    /// What matched in `snippet`: UTF-8 byte ranges, on character boundaries, in order.
    public var marks: [Range<Int>]
    /// When it was said (ms); for a title, when its thread was last active.
    public var at: Int64?
    /// Who it's from when Flotilla brought it, not you ("From <a thread>", "2 agents answered"),
    /// as the thread shows it.
    public var from: String?
    /// How many of the thread's messages match as well as this one (a message hit).
    public var hits: Int

    public var id: String { "\(thread):\(seq.map(String.init) ?? "title")" }

    public init(thread: String, title: String, kind: HitKind, seq: Int64?, snippet: String, marks: [Range<Int>] = [], at: Int64? = nil, from: String? = nil, hits: Int = 0) {
        self.thread = thread
        self.title = title
        self.kind = kind
        self.seq = seq
        self.snippet = snippet
        self.marks = marks
        self.at = at
        self.from = from
        self.hits = hits
    }

    /// Read leniently: a kind this phone doesn't know isn't shown (nil), and a mark that isn't
    /// a range in the snippet on its characters is dropped.
    init?(json: JSON) {
        guard let thread = json["thread"]?.string, let kind = json["kind"]?.string.flatMap(HitKind.init(rawValue:)) else { return nil }
        let snippet = json["snippet"]?.string ?? ""
        let utf8 = Array(snippet.utf8)
        // A byte that starts a character: not a UTF-8 continuation byte (10xxxxxx).
        let boundary = { (i: Int) in i == utf8.count || (i >= 0 && i < utf8.count && utf8[i] & 0xC0 != 0x80) }
        let marks: [Range<Int>] = (json["marks"]?.array ?? []).compactMap { m in
            guard let pair = m.array, pair.count == 2, let a = pair[0].int, let b = pair[1].int, a < b, b <= utf8.count, boundary(Int(a)), boundary(Int(b)) else { return nil }
            return Int(a)..<Int(b)
        }
        self.init(thread: thread, title: json["title"]?.string ?? "", kind: kind, seq: json["seq"]?.int, snippet: snippet, marks: marks, at: json["at"]?.int, from: json["from"]?.string, hits: Int(json["hits"]?.int ?? 0))
    }

    /// The snippet's matched parts as ranges of the string itself.
    public var markedRanges: [Range<String.Index>] { Search.stringRanges(snippet, marks) }
}

/// How often and how recently a thread was opened, and what it was last opened for from a
/// search (`search::Opens`): the server keeps it, so every device ranks by the same.
public struct Opens: Sendable, Hashable {
    /// The last open that counted (ms).
    public var at: Int64
    /// Opens, each halving every two weeks (as of `at`).
    public var score: Double
    /// The latest queries it was picked for, newest first.
    public var picks: [SearchPick]

    public init(at: Int64, score: Double, picks: [SearchPick] = []) {
        self.at = at
        self.score = score
        self.picks = picks
    }

    init?(json: JSON?) {
        guard let j = json, let at = j["at"]?.int else { return nil }
        self.init(at: at, score: j["score"]?.double ?? 0, picks: (j["picks"]?.array ?? []).compactMap { p in
            guard let q = p["query"]?.string, let a = p["at"]?.int else { return nil }
            return SearchPick(query: q, at: a)
        })
    }
}

/// A query a thread was opened for from a search, and when.
public struct SearchPick: Sendable, Hashable {
    public var query: String
    public var at: Int64

    public init(query: String, at: Int64) {
        self.query = query
        self.at = at
    }
}

public enum Search {
    /// Hits asked for at once (`search::LIMIT`).
    public static let limit = 50
    /// Threads the search lists by title, and by their messages, at most.
    public static let threadRows = 20
    public static let messageRows = 10
    /// Words this long (in characters) are looked up in the index.
    public static let indexed = 3

    // MARK: Characters

    typealias S = Unicode.Scalar

    /// A letter or a digit, of any script (`\p{L}\p{N}`).
    static func alnum(_ c: S) -> Bool {
        if c.isASCII { return c.properties.isAlphabetic || (c.value >= 0x30 && c.value <= 0x39) }
        switch c.properties.generalCategory {
        case .uppercaseLetter, .lowercaseLetter, .titlecaseLetter, .modifierLetter, .otherLetter, .decimalNumber, .letterNumber, .otherNumber: return true
        default: return false
        }
    }

    static func lower(_ c: S) -> Bool { c.isASCII ? (c.value >= 0x61 && c.value <= 0x7A) : c.properties.generalCategory == .lowercaseLetter }
    static func upper(_ c: S) -> Bool { c.isASCII ? (c.value >= 0x41 && c.value <= 0x5A) : c.properties.generalCategory == .uppercaseLetter }

    static func digit(_ c: S) -> Bool {
        if c.isASCII { return c.value >= 0x30 && c.value <= 0x39 }
        return [.decimalNumber, .letterNumber, .otherNumber].contains(c.properties.generalCategory)
    }

    static func letter(_ c: S) -> Bool {
        if c.isASCII { return (c.value >= 0x41 && c.value <= 0x5A) || (c.value >= 0x61 && c.value <= 0x7A) }
        return [.uppercaseLetter, .lowercaseLetter, .titlecaseLetter, .modifierLetter, .otherLetter].contains(c.properties.generalCategory)
    }

    static func mark(_ c: S) -> Bool { !c.isASCII && [.nonspacingMark, .spacingMark, .enclosingMark].contains(c.properties.generalCategory) }
    /// Emoji and other symbols (`\p{So}`): they start words too.
    static func symbol(_ c: S) -> Bool { !c.isASCII && c.properties.generalCategory == .otherSymbol }
    static func apostrophe(_ c: S) -> Bool { c == "'" || c == "’" }
    static func whitespace(_ c: S) -> Bool { c.properties.isWhitespace }

    /// A script written without spaces (Chinese, Japanese, Thai, Lao): each character starts a word.
    static func unspaced(_ c: S) -> Bool {
        switch c.value {
        case 0x0E00...0x0EFF, 0x2E80...0x9FFF, 0xF900...0xFAFF, 0xFF66...0xFF9F, 0x20000...0x3FFFF: return true
        default: return false
        }
    }

    /// Two Chinese, Japanese or Korean characters already make a common word.
    static func cjkWord(_ c: S) -> Bool {
        (!(0x0E00...0x0EFF).contains(c.value) && unspaced(c)) || (0xAC00...0xD7AF).contains(c.value)
    }

    static func lowercase(_ c: S) -> [S] { Array(c.properties.lowercaseMapping.unicodeScalars) }

    /// A character as search compares it (`search::fold`): lower case, without its accent (its
    /// decomposition's marks, a stroke), `ς` as `σ`, curly apostrophes straight. One in, one out.
    static func fold(_ c: S) -> S {
        if c.isASCII { return upper(c) ? S(c.value + 32)! : c }
        let decomposed = String(c).decomposedStringWithCompatibilityMapping.unicodeScalars.filter { !mark($0) }
        let folded: [S] = decomposed.flatMap(lowercase).map { c in
            switch c {
            case "ς": "σ"
            case "‘", "’": "'"
            case "đ": "d"
            case "ħ": "h"
            case "ı": "i"
            case "ł": "l"
            case "ø": "o"
            case "ŧ": "t"
            default: c
            }
        }
        if folded.count == 1 { return folded[0] }
        let low = lowercase(c)
        return low.count == 1 ? low[0] : c
    }

    static func foldAll(_ text: [S]) -> [S] { text.map(fold) }

    /// Whether a word starts at `c`, after `prev` and `before`, with `next` after it
    /// (`search::starts_word`).
    static func startsWord(_ before: S?, _ prev: S?, _ c: S, _ next: S?) -> Bool {
        let afterApostrophe = prev.map(apostrophe) == true && before.map(alnum) == true
        guard (alnum(c) || symbol(c)) && !afterApostrophe else { return false }
        guard let p = prev else { return true }
        return !alnum(p)
            || (lower(p) && upper(c))
            || (upper(p) && upper(c) && next.map(lower) == true)
            || digit(p) != digit(c)
            || unspaced(c)
            || unspaced(p)
    }

    static func wordStarts(_ src: [S]) -> [Bool] {
        let at = { (i: Int) -> S? in i >= 0 && i < src.count ? src[i] : nil }
        return src.indices.map { i in startsWord(at(i - 2), at(i - 1), src[i], at(i + 1)) }
    }

    /// Text as titles are matched: folded, and where its words start.
    struct Field {
        let text: [S]
        let starts: [Bool]

        init(_ source: String) {
            let src = Array(source.unicodeScalars)
            starts = wordStarts(src)
            text = foldAll(src)
        }

        func wordEnd(_ at: Int) -> Int {
            var end = at + 1
            while end < text.count && !starts[end] && alnum(text[end]) { end += 1 }
            return end
        }
    }

    /// Where `needle` is in `hay` at or after `from`.
    static func indexOf(_ hay: [S], _ needle: [S], _ from: Int) -> Int? {
        guard let first = needle.first, needle.count <= hay.count, from <= hay.count - needle.count else { return nil }
        for i in from...(hay.count - needle.count) where hay[i] == first {
            if hay[i..<(i + needle.count)].elementsEqual(needle) { return i }
        }
        return nil
    }

    static func starts(_ hay: ArraySlice<S>, with prefix: ArraySlice<S>) -> Bool {
        hay.count >= prefix.count && hay.prefix(prefix.count).elementsEqual(prefix)
    }

    static let quotes: Set<S> = ["\"", "'", "`", "“", "”", "‘", "’"]

    /// A pasted query's quotes, and the spaces around them, gone.
    static func unquoted(_ query: String) -> [S] {
        var s = Array(query.unicodeScalars)
        while let f = s.first, separates(f) { s.removeFirst() }
        while let l = s.last, separates(l) { s.removeLast() }
        while let f = s.first, quotes.contains(f) { s.removeFirst() }
        while let l = s.last, quotes.contains(l) { s.removeLast() }
        return s
    }

    static func string(_ s: [S]) -> String {
        var out = ""
        out.unicodeScalars.append(contentsOf: s)
        return out
    }

    /// What separates a query's words (`search::separates`): whitespace, and control characters
    /// (a NUL), which nothing has to find.
    static func separates(_ c: S) -> Bool { c.properties.isWhitespace || c.properties.generalCategory == .control }

    static func split(_ s: [S]) -> [[S]] {
        s.split(whereSeparator: separates).map(Array.init)
    }

    /// A query as titles are matched against it (`search::fold_query`): folded, its words one
    /// space apart; empty when there's nothing to match.
    public static func foldQuery(_ query: String) -> String {
        let composed = string(unquoted(query)).precomposedStringWithCanonicalMapping
        let folded = foldAll(Array(composed.unicodeScalars))
        return split(folded).map(string).joined(separator: " ")
    }

    // MARK: Titles

    static let wholeWord = 1.0
    static let wholeName = 1.5
    static let midWord = 0.3
    static let typo = 0.45
    static let joinedWords = 0.6

    struct TokenMatch {
        var score: Double
        var start: Int
        var end: Int
        var letters: [Range<Int>] = []
    }

    /// One substitution, insertion, deletion or swap of neighbours; with `swapOnly`, only a swap.
    static func withinOneEdit(_ left: ArraySlice<S>, _ right: ArraySlice<S>, _ swapOnly: Bool) -> Bool {
        if abs(left.count - right.count) > (swapOnly ? 0 : 1) { return false }
        let left = Array(left), right = Array(right)
        var i = 0
        while i < left.count && i < right.count && left[i] == right[i] { i += 1 }
        let l = Array(left[i...]), r = Array(right[i...])
        let from = { (s: [S], k: Int) -> [S] in k < s.count ? Array(s[k...]) : [] }
        let swapped = l.count > 1 && r.count > 1 && l[0] == r[1] && l[1] == r[0] && from(l, 2) == from(r, 2)
        if swapOnly { return swapped }
        return swapped || l == r || from(l, 1) == from(r, 1) || from(l, 1) == r || l == from(r, 1)
    }

    /// `token` in `field` (`search::match_token`).
    static func matchToken(_ field: Field, _ token: [S], typos: Bool, primary: Bool, midWord allowMid: Bool) -> TokenMatch? {
        guard let first = token.first else { return nil }
        var best: TokenMatch?
        let ownBoundary = !alnum(first) && !symbol(first)
        let len = field.text.count
        var found = indexOf(field.text, token, 0)
        while let i = found {
            if field.starts[i] || ownBoundary {
                let end = i + token.count
                let whole = end >= len || field.starts[end] || !alnum(field.text[end])
                var runEnd = end
                while runEnd < len && alnum(field.text[runEnd]) { runEnd += 1 }
                let score = whole ? wholeWord : 0.7 + 0.2 * Double(token.count) / Double(runEnd - i)
                if best == nil || score > best!.score { best = TokenMatch(score: score, start: i, end: end) }
                if score == wholeWord { return best }
            } else if allowMid && token.count >= 4 && best == nil {
                best = TokenMatch(score: midWord, start: i, end: i + token.count)
            }
            found = indexOf(field.text, token, i + 1)
        }
        if (best == nil || best!.score <= midWord) && primary, let joined = joinedWords(field, token) { return joined }
        if (best.map { $0.score > midWord } ?? false) || !typos || token.count < 3 { return best }
        if token.contains(where: letter) {
            for i in 0..<len where field.starts[i] {
                let f = field.text[i]
                let kept = f == token[0] || (token.count > 3 && f == token[1] && i + 1 < len && field.text[i + 1] == token[0])
                if !kept { continue }
                var runEnd = i + 1
                while runEnd < len && alnum(field.text[runEnd]) { runEnd += 1 }
                let word = field.text[i..<runEnd]
                let short = token.count <= 4
                let lengths = short ? [token.count, token.count + 1] : [token.count, token.count - 1, token.count + 1]
                for n in lengths where n <= word.count {
                    if withinOneEdit(token[...], word.prefix(n), short && n == token.count) { return TokenMatch(score: typo, start: i, end: i + n) }
                }
            }
        }
        return best
    }

    /// Whole words typed together (`search::joined_words`).
    static func joinedWords(_ field: Field, _ token: [S]) -> TokenMatch? {
        let len = field.text.count
        let firstWord = field.starts.firstIndex(of: true)
        for start in 0..<len where field.starts[start] && field.text[start] == token[0] {
            var runs: [Range<Int>] = []
            var at = start
            var rest = token[...]
            while !rest.isEmpty && at < len {
                let end = field.wordEnd(at)
                if rest.count <= end - at && starts(field.text[at...], with: rest) {
                    runs.append(at..<(at + rest.count))
                    rest = []
                } else if starts(rest, with: field.text[at..<end]) {
                    runs.append(at..<end)
                    rest = rest.dropFirst(end - at)
                    at = end
                    while at < len && !field.starts[at] { at += 1 }
                } else {
                    break
                }
            }
            guard let last = runs.last, rest.isEmpty, runs.count >= 2 else { continue }
            if last.upperBound == field.wordEnd(last.lowerBound) || last.count >= 3 || start == firstWord || runs.dropLast().allSatisfy({ $0.count >= 3 }) {
                return TokenMatch(score: joinedWords, start: start, end: last.upperBound, letters: runs)
            }
        }
        return nil
    }

    struct Weighted {
        var value: String
        var weight: Double
        var wholeWords = false
        var keyword = false
        var context = false
        var midWord = false
    }

    struct FieldsMatch {
        var quality: Double
        var strong: Bool
        var ranges: [Range<Int>]
    }

    static let filler: Set<String> = ["a", "an", "and", "for", "in", "of", "on", "the", "to", "with"]

    static func merged(_ ranges: [Range<Int>]) -> [Range<Int>] {
        var out: [Range<Int>] = []
        for r in ranges.sorted(by: { $0.lowerBound < $1.lowerBound }) {
            if let last = out.last, r.lowerBound <= last.upperBound {
                out[out.count - 1] = last.lowerBound..<max(last.upperBound, r.upperBound)
            } else {
                out.append(r)
            }
        }
        return out
    }

    /// Every token must match some field (`search::match_fields`).
    static func matchFields(_ query: String, _ fields: [Weighted], typos: Bool) -> FieldsMatch? {
        let tokens = query.split(separator: " ").map { Array(String($0).unicodeScalars) }
        if tokens.isEmpty { return nil }
        let primarySet = fields.first.map { !$0.value.isEmpty } ?? false
        let prepared = fields.filter { !$0.value.isEmpty }.map { (Field($0.value), $0) }
        let isFiller = { (t: [S]) in filler.contains(string(t)) }
        let fillerOptional = tokens.contains { !isFiller($0) }
        var ranges: [Range<Int>] = []
        var total = 0.0, counted = 0, strong = true
        for token in tokens {
            var best = 0.0
            var bestPrimary: TokenMatch?
            var atWordStart = false
            for (k, (field, w)) in prepared.enumerated() {
                let primary = primarySet && k == 0
                if best >= w.weight && !primary && atWordStart { continue }
                guard let m = matchToken(field, token, typos: typos && !w.keyword, primary: primary, midWord: w.midWord) else { continue }
                if m.score < (w.wholeWords ? wholeWord : w.keyword ? 0.7 : 0) { continue }
                if m.score >= 0.7 && !w.context { atWordStart = true }
                if m.score * w.weight > best { best = m.score * w.weight }
                if primary { bestPrimary = m }
            }
            if best == 0 {
                if fillerOptional && isFiller(token) { continue }
                return nil
            }
            strong = strong && atWordStart
            if let m = bestPrimary {
                if m.letters.isEmpty { ranges.append(m.start..<m.end) } else { ranges.append(contentsOf: m.letters) }
            }
            total += best
            counted += 1
        }
        var quality = total / Double(counted)
        if primarySet, let (primary, _) = prepared.first {
            let q = Array(query.unicodeScalars)
            if primary.text == q {
                quality += wholeName - wholeWord
            } else if tokens.count > 1 && indexOf(primary.text, q, 0) != nil {
                quality += 0.25
            }
        }
        return FieldsMatch(quality: quality, strong: strong, ranges: merged(ranges))
    }

    // MARK: Use and picks

    static let dayMs: Int64 = 24 * 60 * 60 * 1000
    static let halfLifeMs = 14 * dayMs
    static let pickLength = 32
    static let pickWindowMs = 21 * dayMs
    static let pickPrefix = 3

    /// `query` as a pick is kept and compared (`search::picked_query`).
    public static func pickedQuery(_ query: String) -> String {
        var out: [S] = []
        var units = 0
        for c in foldQuery(query).unicodeScalars {
            units += c.utf16.count
            if units > pickLength { break }
            out.append(c)
        }
        while let l = out.last, whitespace(l) { out.removeLast() }
        while let f = out.first, whitespace(f) { out.removeFirst() }
        return string(out)
    }

    static func decay(_ ageMs: Int64) -> Double { pow(2, -Double(max(ageMs, 0)) / Double(halfLifeMs)) }

    static func activePick(_ opens: Opens?, _ query: String, _ now: Int64) -> SearchPick? {
        let query = pickedQuery(query)
        let prefix = query.unicodeScalars.count >= pickPrefix
        var exact: SearchPick?, latest: SearchPick?
        for pick in opens?.picks ?? [] {
            if now - pick.at > pickWindowMs { continue }
            if pick.query == query && (exact == nil || pick.at > exact!.at) { exact = pick }
            if (pick.query == query || (prefix && pick.query.unicodeScalars.starts(with: query.unicodeScalars))) && (latest == nil || pick.at > latest!.at) { latest = pick }
        }
        return exact ?? latest
    }

    /// How much use lifts a thread (`search::usage_boost`).
    public static func usageBoost(activity: Int64?, opens: Opens?, now: Int64) -> Double {
        let recency = activity.map { decay(now - $0) } ?? 0
        let opened = opens.map { $0.score * decay(now - $0.at) } ?? 0
        return min(2.5, 1 + recency + 0.5 * log2(1 + opened))
    }

    // MARK: Threads

    /// A thread as search sees it.
    public struct Subject {
        public var id: String?
        public var title: String
        public var branch: String?
        /// Its project (`owner/repo`).
        public var project: String?
        public var prs: [PrRef]
        /// When you last wrote in it, else when it began (ms).
        public var activity: Int64?
        public var opens: Opens?

        public init(id: String? = nil, title: String, branch: String? = nil, project: String? = nil, prs: [PrRef] = [], activity: Int64? = nil, opens: Opens? = nil) {
            self.id = id
            self.title = title
            self.branch = branch
            self.project = project
            self.prs = prs
            self.activity = activity
            self.opens = opens
        }
    }

    public struct PrRef {
        public var number: Int64
        public var title: String?
        public var references: [String]

        public init(number: Int64, title: String? = nil, references: [String] = []) {
            self.number = number
            self.title = title
            self.references = references
        }
    }

    /// Why a thread matched when its title shows nothing.
    public struct Hint: Hashable {
        /// "PR", "Branch", "Project", or "ID or link".
        public var label: String
        public var text: String
        /// Scalar ranges of `text`.
        public var ranges: [Range<Int>]
    }

    public struct ThreadMatch {
        public var score: Double
        public var quality: Double
        public var strong: Bool
        /// What matched in the title (scalar ranges).
        public var ranges: [Range<Int>]
        public var hint: Hint?
        public var pickedAt: Int64
        public var pickedExactly: Bool
        public var groupQuality: Double
    }

    static let branchPrefix = Array("flotilla/".unicodeScalars)

    static func branchShown(_ branch: String) -> String {
        let b = Array(branch.unicodeScalars)
        if starts(b[...], with: branchPrefix[...]), b.count > branchPrefix.count, !whitespace(b[branchPrefix.count]) { return string(Array(b[branchPrefix.count...])) }
        return branch
    }

    static func fieldQuery(_ query: String) -> String {
        var q = Array(query.unicodeScalars)[...]
        for prefix in ["pull request", "pull", "pr"] {
            let p = Array(prefix.unicodeScalars)
            if starts(q, with: p[...]) {
                var rest = q.dropFirst(p.count)
                while let f = rest.first, whitespace(f) { rest = rest.dropFirst() }
                let num = rest.first == "#" ? rest.dropFirst() : rest
                if let d = num.first, d.isASCII, digit(d) {
                    q = rest
                    break
                }
            }
        }
        if q.first == "#" {
            q = q.dropFirst()
            while let f = q.first, whitespace(f) { q = q.dropFirst() }
        }
        return branchShown(string(Array(q)))
    }

    static func asciiDigit(_ c: S?) -> Bool { c.map { $0.value >= 0x30 && $0.value <= 0x39 } ?? false }

    static func matchReference(_ query: String, _ subject: Subject) -> Bool {
        let q = Array(query.unicodeScalars)
        if q.count < 8 { return false }
        let anywhere = q.contains { asciiDigit($0) }
        let refs = (subject.id.map { [$0] } ?? []) + subject.prs.flatMap(\.references)
        return refs.contains { ref in
            let r = Array(ref.lowercased().unicodeScalars)
            if anywhere && starts(q[...], with: r[...]) && !asciiDigit(r.count < q.count ? q[r.count] : nil) { return true }
            guard let at = indexOf(r, q, 0) else { return false }
            if anywhere { return !asciiDigit(at + q.count < r.count ? r[at + q.count] : nil) }
            return at == 0
        }
    }

    static func hint(_ query: String, _ subject: Subject, typos: Bool) -> Hint? {
        var fields: [(String, String)] = subject.prs.map { p in ("PR", "#\(p.number)" + (p.title.map { " \($0)" } ?? "")) }
        if let b = subject.branch { fields.append(("Branch", branchShown(b))) }
        if let p = subject.project { fields.append(("Project", p)) }
        let holds = { (text: String, q: String) in matchFields(q, [Weighted(value: text, weight: 1)], typos: typos)?.ranges }
        let hintFor = { (q: String) -> Hint? in
            for (label, text) in fields {
                if let ranges = holds(text, q) { return Hint(label: label, text: text, ranges: ranges) }
            }
            return nil
        }
        if let h = hintFor(query) { return h }
        let rest = query.split(separator: " ").map(String.init).filter { t in !(subject.project.map { holds($0, t) != nil } ?? false) }.joined(separator: " ")
        return !rest.isEmpty && rest != query ? hintFor(rest) : nil
    }

    /// How `subject` matches `query` (folded), or nil (`search::score_thread`).
    public static func scoreThread(_ query: String, _ subject: Subject, now: Int64, typos: Bool, picks: Bool = true) -> ThreadMatch? {
        var fields = [Weighted(value: subject.title, weight: 1, midWord: true)]
        for p in subject.prs {
            fields.append(Weighted(value: "#\(p.number)", weight: 1))
            if let t = p.title { fields.append(Weighted(value: t, weight: 0.5)) }
        }
        if let b = subject.branch { fields.append(Weighted(value: branchShown(b), weight: 0.6)) }
        if let p = subject.project { fields.append(Weighted(value: p, weight: 0.25, context: true)) }
        let fq = fieldQuery(query)
        let byFields = matchFields(fq, fields, typos: typos)
        let byReference = byFields == nil && matchReference(query, subject)
        guard let m = byFields ?? (byReference ? FieldsMatch(quality: 0.6, strong: true, ranges: []) : nil) else { return nil }
        var h: Hint?
        if !m.ranges.isEmpty {
            h = nil
        } else if byReference {
            h = Hint(label: "ID or link", text: query, ranges: [])
        } else {
            h = hint(fq, subject, typos: typos)
            // Several words, none in a field that explains them: a coincidence.
            if h == nil && fq.contains(" ") { return nil }
        }
        let pick = picks ? activePick(subject.opens, query, now) : nil
        return ThreadMatch(
            score: m.quality * usageBoost(activity: subject.activity, opens: subject.opens, now: now),
            quality: m.quality,
            strong: m.strong,
            ranges: m.ranges,
            hint: h,
            pickedAt: pick?.at ?? 0,
            pickedExactly: pick?.query == pickedQuery(query),
            groupQuality: min(m.quality, wholeName - 0.01)
        )
    }

    static func matchClass(_ q: Double) -> Int { q >= wholeName ? 3 : q >= 0.7 ? 2 : q >= 0.4 ? 1 : 0 }

    /// Whether `l` comes before `r` (`search::compare`).
    static func before(_ l: ThreadMatch, _ r: ThreadMatch) -> Bool {
        let word = { (m: ThreadMatch) in min(matchClass(m.quality), 2) }
        let exact = { (m: ThreadMatch) in m.pickedExactly ? m.pickedAt : 0 }
        if word(l) != word(r) { return word(l) > word(r) }
        if exact(l) != exact(r) { return exact(l) > exact(r) }
        if matchClass(l.quality) != matchClass(r.quality) { return matchClass(l.quality) > matchClass(r.quality) }
        if l.pickedAt != r.pickedAt { return l.pickedAt > r.pickedAt }
        return l.score > r.score
    }

    /// Items ranked by their threads' matches for `query` (`search::rank`): typos only when
    /// nothing matches at a word start; the order kept on ties.
    public static func rank<T>(_ items: [T], _ query: String, now: Int64, subject: (T) -> Subject) -> [(item: T, match: ThreadMatch)] {
        let q = foldQuery(query)
        let subjects = items.map(subject)
        let pass = { (typos: Bool) in subjects.map { scoreThread(q, $0, now: now, typos: typos) } }
        var found = pass(false)
        if !found.contains(where: { $0?.strong == true }) { found = pass(true) }
        let ranked = zip(items, found).enumerated().compactMap { k, pair in pair.1.map { (k, pair.0, $0) } }
        return ranked.sorted { l, r in before(l.2, r.2) || (!before(r.2, l.2) && l.0 < r.0) }.map { (item: $0.1, match: $0.2) }
    }

    // MARK: Messages

    /// A query's words for messages (`search::message_tokens`): `long` ones are looked up and
    /// must all match, `short` ones only narrow.
    public static func messageTokens(_ query: String) -> (long: [String], short: [String], words: [String]) {
        let straight = unquoted(query).map { $0 == "‘" || $0 == "’" ? S("'") : $0 }
        let q = string(straight).precomposedStringWithCanonicalMapping.lowercased()
        let words = split(Array(q.unicodeScalars)).map(string)
        let searchable = { (t: String) in
            let n = t.unicodeScalars.count
            return n >= indexed || (n == 2 && t.unicodeScalars.contains(where: cjkWord))
        }
        return (words.filter(searchable), words.filter { !searchable($0) }, words)
    }

    /// Whether the server finds messages for this query, not titles only.
    public static func findsMessages(_ query: String) -> Bool { !messageTokens(query).long.isEmpty }

    // MARK: Places

    /// Scalar ranges of `text` as UTF-8 byte ranges.
    public static func byteRanges(_ text: String, _ ranges: [Range<Int>]) -> [Range<Int>] {
        var bytes = [0]
        for c in text.unicodeScalars { bytes.append(bytes.last! + c.utf8.count) }
        return ranges.compactMap { r in
            guard r.lowerBound < r.upperBound, r.upperBound < bytes.count else { return nil }
            return bytes[r.lowerBound]..<bytes[r.upperBound]
        }
    }

    /// UTF-8 byte ranges of `text` as ranges of the string.
    public static func stringRanges(_ text: String, _ marks: [Range<Int>]) -> [Range<String.Index>] {
        let u = text.utf8
        return marks.compactMap { r in
            guard let a = u.index(u.startIndex, offsetBy: r.lowerBound, limitedBy: u.endIndex),
                  let b = u.index(u.startIndex, offsetBy: r.upperBound, limitedBy: u.endIndex),
                  let lo = String.Index(a, within: text), let hi = String.Index(b, within: text) else { return nil }
            return lo..<hi
        }
    }
}

extension Request {
    /// Threads for `query`: by title first, then by their messages, one hit a thread. Answers
    /// `hits`.
    public static func search(_ query: String, limit: Int = Search.limit) -> Request {
        Request(method: "search", params: ["query": .string(query), "limit": .int(Int64(limit))])
    }

    /// The thread was opened on screen; `query`, the search it was picked from, if any.
    public static func opened(_ thread: String, query: String? = nil) -> Request {
        Request(method: "thread.opened", params: ["thread": .string(thread), "query": query.map(JSON.string) ?? .null])
    }
}
