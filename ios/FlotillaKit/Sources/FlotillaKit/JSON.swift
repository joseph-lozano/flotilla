import Foundation

/// A JSON value, with serde_json's meaning of equality: objects are unordered, and a number
/// written with a fraction or exponent (`2.0`) is a float, never equal to the integer `2`.
/// The replica applies deltas to views as these, exactly as Rust's replica does to
/// `serde_json::Value`, so both end with the same views.
public enum JSON: Sendable, Hashable {
    case null
    case bool(Bool)
    case int(Int64)
    case double(Double)
    case string(String)
    case array([JSON])
    case object([String: JSON])
}

// MARK: Reading

extension JSON {
    public subscript(key: String) -> JSON? {
        if case .object(let fields) = self { return fields[key] }
        return nil
    }

    public var string: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    public var int: Int64? {
        switch self {
        case .int(let i): i
        case .double(let d) where d.rounded() == d && abs(d) < 9.2e18: Int64(d)
        default: nil
        }
    }

    public var double: Double? {
        switch self {
        case .int(let i): Double(i)
        case .double(let d): d
        default: nil
        }
    }

    public var bool: Bool? {
        if case .bool(let b) = self { return b }
        return nil
    }

    public var array: [JSON]? {
        if case .array(let a) = self { return a }
        return nil
    }

    public var object: [String: JSON]? {
        if case .object(let o) = self { return o }
        return nil
    }

    public var isNull: Bool {
        if case .null = self { return true }
        return false
    }

    /// The value at `path` (object keys), if every step is there and it isn't null: how a
    /// keyed list's entries are told apart.
    func at(_ path: [String]) -> JSON? {
        var v = self
        for p in path {
            guard let next = v[p] else { return nil }
            v = next
        }
        return v.isNull ? nil : v
    }
}

// MARK: Parsing

public struct JSONError: Error, CustomStringConvertible, Sendable {
    public let description: String
    init(_ description: String) { self.description = description }
}

extension JSON {
    public init(parsing text: String) throws(JSONError) {
        var utf8 = text.utf8.withContiguousStorageIfAvailable { Array($0) } ?? Array(text.utf8)
        self = try JSON(parsing: &utf8)
    }

    public init(parsing data: Data) throws(JSONError) {
        var bytes = [UInt8](data)
        self = try JSON(parsing: &bytes)
    }

    private init(parsing bytes: inout [UInt8]) throws(JSONError) {
        var p = Parser(bytes: bytes)
        p.skipSpace()
        let v = try p.value(depth: 0)
        p.skipSpace()
        guard p.i == p.bytes.count else { throw JSONError("trailing characters at \(p.i)") }
        self = v
    }
}

private struct Parser {
    let bytes: [UInt8]
    var i = 0

    init(bytes: [UInt8]) { self.bytes = bytes }

    mutating func skipSpace() {
        while i < bytes.count, bytes[i] == 0x20 || bytes[i] == 0x0A || bytes[i] == 0x0D || bytes[i] == 0x09 { i += 1 }
    }

    mutating func value(depth: Int) throws(JSONError) -> JSON {
        guard depth < 512 else { throw JSONError("nested too deep") }
        guard i < bytes.count else { throw JSONError("unexpected end") }
        switch bytes[i] {
        case UInt8(ascii: "{"):
            i += 1
            var fields: [String: JSON] = [:]
            skipSpace()
            if i < bytes.count, bytes[i] == UInt8(ascii: "}") { i += 1; return .object(fields) }
            while true {
                skipSpace()
                guard i < bytes.count, bytes[i] == UInt8(ascii: "\"") else { throw JSONError("expected a key at \(i)") }
                let key = try string()
                skipSpace()
                guard i < bytes.count, bytes[i] == UInt8(ascii: ":") else { throw JSONError("expected : at \(i)") }
                i += 1
                skipSpace()
                fields[key] = try value(depth: depth + 1)
                skipSpace()
                guard i < bytes.count else { throw JSONError("unexpected end") }
                if bytes[i] == UInt8(ascii: ",") { i += 1; continue }
                if bytes[i] == UInt8(ascii: "}") { i += 1; return .object(fields) }
                throw JSONError("expected , or } at \(i)")
            }
        case UInt8(ascii: "["):
            i += 1
            var items: [JSON] = []
            skipSpace()
            if i < bytes.count, bytes[i] == UInt8(ascii: "]") { i += 1; return .array(items) }
            while true {
                skipSpace()
                items.append(try value(depth: depth + 1))
                skipSpace()
                guard i < bytes.count else { throw JSONError("unexpected end") }
                if bytes[i] == UInt8(ascii: ",") { i += 1; continue }
                if bytes[i] == UInt8(ascii: "]") { i += 1; return .array(items) }
                throw JSONError("expected , or ] at \(i)")
            }
        case UInt8(ascii: "\""):
            return .string(try string())
        case UInt8(ascii: "t"):
            try literal("true"); return .bool(true)
        case UInt8(ascii: "f"):
            try literal("false"); return .bool(false)
        case UInt8(ascii: "n"):
            try literal("null"); return .null
        default:
            return try number()
        }
    }

    mutating func literal(_ word: StaticString) throws(JSONError) {
        let n = word.utf8CodeUnitCount
        guard i + n <= bytes.count else { throw JSONError("unexpected end") }
        let ok = word.withUTF8Buffer { w in (0..<n).allSatisfy { bytes[i + $0] == w[$0] } }
        guard ok else { throw JSONError("unexpected token at \(i)") }
        i += n
    }

    mutating func number() throws(JSONError) -> JSON {
        let start = i
        var float = false
        if i < bytes.count, bytes[i] == UInt8(ascii: "-") { i += 1 }
        while i < bytes.count {
            let b = bytes[i]
            if b >= 0x30 && b <= 0x39 { i += 1; continue }
            if b == UInt8(ascii: ".") || b == UInt8(ascii: "e") || b == UInt8(ascii: "E") || b == UInt8(ascii: "+") || b == UInt8(ascii: "-") {
                float = true; i += 1; continue
            }
            break
        }
        guard i > start, let text = String(bytes: bytes[start..<i], encoding: .ascii) else { throw JSONError("unexpected token at \(start)") }
        if !float, let n = Int64(text) { return .int(n) }
        guard let d = Double(text) else { throw JSONError("bad number \(text)") }
        return .double(d)
    }

    mutating func string() throws(JSONError) -> String {
        i += 1 // the opening quote
        var out = [UInt8]()
        while true {
            guard i < bytes.count else { throw JSONError("unterminated string") }
            let b = bytes[i]
            if b == UInt8(ascii: "\"") { i += 1; break }
            if b == UInt8(ascii: "\\") {
                i += 1
                guard i < bytes.count else { throw JSONError("unterminated string") }
                let e = bytes[i]
                i += 1
                switch e {
                case UInt8(ascii: "\""): out.append(0x22)
                case UInt8(ascii: "\\"): out.append(0x5C)
                case UInt8(ascii: "/"): out.append(0x2F)
                case UInt8(ascii: "b"): out.append(0x08)
                case UInt8(ascii: "f"): out.append(0x0C)
                case UInt8(ascii: "n"): out.append(0x0A)
                case UInt8(ascii: "r"): out.append(0x0D)
                case UInt8(ascii: "t"): out.append(0x09)
                case UInt8(ascii: "u"):
                    var scalar = try hex4()
                    if (0xD800..<0xDC00).contains(scalar) {
                        // A surrogate pair: the low half must follow.
                        guard i + 1 < bytes.count, bytes[i] == UInt8(ascii: "\\"), bytes[i + 1] == UInt8(ascii: "u") else { throw JSONError("lone surrogate") }
                        i += 2
                        let low = try hex4()
                        guard (0xDC00..<0xE000).contains(low) else { throw JSONError("bad surrogate pair") }
                        scalar = 0x10000 + ((scalar - 0xD800) << 10) + (low - 0xDC00)
                    }
                    guard let u = Unicode.Scalar(scalar) else { throw JSONError("bad \\u escape") }
                    out.append(contentsOf: Array(String(Character(u)).utf8))
                default:
                    throw JSONError("bad escape at \(i)")
                }
                continue
            }
            out.append(b)
            i += 1
        }
        guard let s = String(validating: out, as: UTF8.self) else { throw JSONError("invalid UTF-8") }
        return s
    }

    mutating func hex4() throws(JSONError) -> UInt32 {
        guard i + 4 <= bytes.count else { throw JSONError("bad \\u escape") }
        var v: UInt32 = 0
        for _ in 0..<4 {
            let b = bytes[i]
            let d: UInt32
            switch b {
            case 0x30...0x39: d = UInt32(b - 0x30)
            case 0x41...0x46: d = UInt32(b - 0x41 + 10)
            case 0x61...0x66: d = UInt32(b - 0x61 + 10)
            default: throw JSONError("bad \\u escape")
            }
            v = v << 4 | d
            i += 1
        }
        return v
    }
}

// MARK: Writing

extension JSON {
    /// Compact JSON text, keys sorted (as serde_json's map writes them).
    public var text: String {
        var out = ""
        write(into: &out)
        return out
    }

    private func write(into out: inout String) {
        switch self {
        case .null: out += "null"
        case .bool(let b): out += b ? "true" : "false"
        case .int(let i): out += String(i)
        case .double(let d):
            if d.isFinite {
                // serde_json writes a float that's whole with ".0" (2.0), which Swift does too.
                out += "\(d)"
            } else {
                out += "null"
            }
        case .string(let s): JSON.quote(s, into: &out)
        case .array(let items):
            out += "["
            for (n, item) in items.enumerated() {
                if n > 0 { out += "," }
                item.write(into: &out)
            }
            out += "]"
        case .object(let fields):
            out += "{"
            for (n, key) in fields.keys.sorted().enumerated() {
                if n > 0 { out += "," }
                JSON.quote(key, into: &out)
                out += ":"
                fields[key]!.write(into: &out)
            }
            out += "}"
        }
    }

    private static func quote(_ s: String, into out: inout String) {
        out += "\""
        for u in s.unicodeScalars {
            switch u {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            case let c where c.value < 0x20:
                out += "\\u" + String(format: "%04x", c.value)
            default: out.unicodeScalars.append(u)
            }
        }
        out += "\""
    }
}

// MARK: Literals

extension JSON: ExpressibleByStringLiteral, ExpressibleByIntegerLiteral, ExpressibleByBooleanLiteral, ExpressibleByArrayLiteral, ExpressibleByDictionaryLiteral, ExpressibleByNilLiteral, ExpressibleByFloatLiteral {
    public init(stringLiteral value: String) { self = .string(value) }
    public init(integerLiteral value: Int64) { self = .int(value) }
    public init(booleanLiteral value: Bool) { self = .bool(value) }
    public init(arrayLiteral elements: JSON...) { self = .array(elements) }
    public init(dictionaryLiteral elements: (String, JSON)...) { self = .object(Dictionary(elements, uniquingKeysWith: { _, b in b })) }
    public init(nilLiteral: ()) { self = .null }
    public init(floatLiteral value: Double) { self = .double(value) }
}

extension JSON {
    /// `nil` as JSON null.
    public init(_ s: String?) { self = s.map(JSON.string) ?? .null }
}
