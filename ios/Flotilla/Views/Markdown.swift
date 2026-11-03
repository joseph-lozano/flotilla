import SwiftUI

/// An agent's reply: paragraphs, headings, lists, quotes, code blocks and tables, with inline
/// bold, italics, code and links (SwiftUI's own markdown is inline only).
struct MarkdownText: View {
    let source: String
    /// A caret after the last block, while it streams.
    var streaming = false

    var body: some View {
        let blocks = Self.parse(source)
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { i, block in
                view(block, last: i == blocks.count - 1)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    enum Block: Hashable {
        case paragraph(String)
        case heading(Int, String)
        case item(indent: Int, marker: String, text: String)
        case quote(String)
        case code(lang: String, text: String)
        case table([String])
        case rule
    }

    @ViewBuilder private func view(_ block: Block, last: Bool) -> some View {
        switch block {
        case .paragraph(let t):
            inline(t, caret: streaming && last).font(Typeface.body)
        case .heading(let level, let t):
            inline(t, caret: streaming && last).font(level <= 2 ? .headline : .subheadline.weight(.semibold))
        case .item(let indent, let marker, let t):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(marker).font(Typeface.body).foregroundStyle(Palette.dim).frame(minWidth: 12, alignment: .trailing)
                inline(t, caret: streaming && last).font(Typeface.body)
            }
            .padding(.leading, CGFloat(indent) * 14)
        case .quote(let t):
            inline(t, caret: streaming && last).font(Typeface.body).foregroundStyle(Palette.dim)
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Palette.border).frame(width: 3) }
        case .code(let lang, let t):
            VStack(alignment: .leading, spacing: 4) {
                if !lang.isEmpty { Text(lang).font(Typeface.meta).foregroundStyle(Palette.dim) }
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(t).font(Typeface.mono(.footnote)).foregroundStyle(Palette.text).textSelection(.enabled).fixedSize()
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 6).fill(Palette.muted))
        case .table(let lines):
            ScrollView(.horizontal, showsIndicators: false) {
                Text(lines.joined(separator: "\n")).font(Typeface.mono(.caption)).fixedSize()
            }
        case .rule:
            Rectangle().fill(Palette.border).frame(height: 1)
        }
    }

    private func inline(_ s: String, caret: Bool) -> Text {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace, failurePolicy: .returnPartiallyParsedIfPossible)
        var attributed = (try? AttributedString(markdown: s, options: options)) ?? AttributedString(s)
        for run in attributed.runs where run.link != nil {
            attributed[run.range].foregroundColor = Palette.link
        }
        let text = Text(attributed).foregroundStyle(Palette.text)
        return caret ? Text("\(text)\(Text(" ▍").foregroundStyle(Palette.ring))") : text
    }

    /// The blocks of `source`, line by line.
    static func parse(_ source: String) -> [Block] {
        var blocks: [Block] = []
        var para: [String] = []
        var lines = source.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")[...]
        func flush() {
            if !para.isEmpty { blocks.append(.paragraph(para.joined(separator: "\n"))) }
            para = []
        }
        while let line = lines.popFirst() {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                flush()
                let fence = String(trimmed.prefix(3))
                var code: [String] = []
                while let l = lines.popFirst(), !l.trimmingCharacters(in: .whitespaces).hasPrefix(fence) { code.append(l) }
                blocks.append(.code(lang: String(trimmed.dropFirst(3)).trimmingCharacters(in: .whitespaces), text: code.joined(separator: "\n")))
            } else if trimmed.isEmpty {
                flush()
            } else if let h = heading(trimmed) {
                flush()
                blocks.append(h)
            } else if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                flush()
                blocks.append(.rule)
            } else if let item = item(line) {
                flush()
                blocks.append(item)
            } else if trimmed.hasPrefix(">") {
                flush()
                var quote = [String(trimmed.dropFirst()).trimmingCharacters(in: .whitespaces)]
                while let next = lines.first, next.trimmingCharacters(in: .whitespaces).hasPrefix(">") {
                    quote.append(String(next.trimmingCharacters(in: .whitespaces).dropFirst()).trimmingCharacters(in: .whitespaces))
                    lines.removeFirst()
                }
                blocks.append(.quote(quote.joined(separator: "\n")))
            } else if trimmed.hasPrefix("|") {
                flush()
                var rows = [trimmed]
                while let next = lines.first, next.trimmingCharacters(in: .whitespaces).hasPrefix("|") {
                    rows.append(next.trimmingCharacters(in: .whitespaces))
                    lines.removeFirst()
                }
                blocks.append(.table(rows))
            } else if case .item(let indent, let marker, let text)? = blocks.last, para.isEmpty, line.hasPrefix("  ") {
                // A list item's continuation line.
                blocks[blocks.count - 1] = .item(indent: indent, marker: marker, text: text + "\n" + trimmed)
            } else {
                para.append(line.hasSuffix("  ") ? String(line.dropLast(2)) : line)
            }
        }
        flush()
        return blocks
    }

    private static func heading(_ t: String) -> Block? {
        let hashes = t.prefix { $0 == "#" }.count
        guard (1...6).contains(hashes), t.dropFirst(hashes).first == " " else { return nil }
        return .heading(hashes, String(t.dropFirst(hashes + 1)))
    }

    private static func item(_ line: String) -> Block? {
        let indent = line.prefix { $0 == " " }.count / 2
        let t = line.drop { $0 == " " }
        for bullet in ["- ", "* ", "+ "] where t.hasPrefix(bullet) {
            return .item(indent: indent, marker: "•", text: String(t.dropFirst(2)))
        }
        let digits = t.prefix { $0.isNumber }
        if !digits.isEmpty, digits.count < 4, t.dropFirst(digits.count).hasPrefix(". ") {
            return .item(indent: indent, marker: "\(digits).", text: String(t.dropFirst(digits.count + 2)))
        }
        return nil
    }
}
