import FlotillaKit
import SwiftUI
import UIKit

/// What you sent: "You" (or who handed the task over), its status, the text.
struct PromptCard: View {
    let from: String?
    let text: String
    let status: String?
    let attachments: [String]
    let notKept: Bool
    @State private var full = false

    private var long: Bool { text.count > 600 || text.split(separator: "\n", omittingEmptySubsequences: false).count > 8 }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(from ?? "You")
                Spacer()
                if let status { Text(status).foregroundStyle(status == "not sent" ? Palette.bad : Palette.dim) }
            }
            .font(Typeface.meta)
            .foregroundStyle(Palette.dim)
            if notKept {
                Text("Prompt not available").italic().font(Typeface.body).foregroundStyle(Palette.dim)
            } else {
                Text(text)
                    .font(Typeface.body)
                    .foregroundStyle(Palette.text)
                    .lineLimit(long && !full ? 8 : nil)
                if long {
                    Button(full ? "Show less" : "Show full message") { full.toggle() }
                        .font(Typeface.meta)
                        .foregroundStyle(Palette.link)
                }
            }
            if !attachments.isEmpty {
                Text(attachments.joined(separator: ", "))
                    .font(Typeface.meta)
                    .foregroundStyle(Palette.dim)
                    .lineLimit(1)
            }
        }
        .padding(.horizontal, 13)
        .padding(.top, 10)
        .padding(.bottom, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Palette.selection))
        .copyMenu(notKept ? nil : text, shape: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(from ?? "You"), \(status ?? ""): \(notKept ? "Prompt not available" : text)")
    }
}

/// An agent's reply; long-press to copy it (as Markdown) or select part of it.
struct ReplyText: View {
    let source: String

    var body: some View {
        MarkdownText(source: source)
            .copyMenu(source, shape: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }
}

extension View {
    /// Long-press: Copy the whole of `text`, or open it to select part of it. Nil: no menu.
    func copyMenu(_ text: String?, shape: some Shape = Rectangle()) -> some View {
        modifier(CopyMenu(text: text, shape: shape))
    }
}

private struct CopyMenu<S: Shape>: ViewModifier {
    let text: String?
    let shape: S
    @State private var selecting = false

    func body(content: Content) -> some View {
        if let text {
            content
                .contentShape(.contextMenuPreview, shape)
                .contextMenu {
                    Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = text }
                    Button("Select text", systemImage: "selection.pin.in.out") { selecting = true }
                }
                .sheet(isPresented: $selecting) { SelectText(text: text) }
        } else {
            content
        }
    }
}

/// A message to select part of and copy.
struct SelectText: View {
    let text: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            SelectableText(text: text)
                .background(Palette.background)
                .navigationTitle("Select text")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        .presentationDetents([.medium, .large])
    }
}

/// UIKit's text view, read-only: SwiftUI's `Text` selects only the whole of itself.
private struct SelectableText: UIViewRepresentable {
    let text: String

    func makeUIView(context: Context) -> UITextView {
        let v = UITextView()
        v.isEditable = false
        v.isSelectable = true
        v.backgroundColor = .clear
        v.font = .preferredFont(forTextStyle: .subheadline)
        v.adjustsFontForContentSizeCategory = true
        v.textColor = UIColor(Palette.text)
        v.textContainerInset = UIEdgeInsets(top: 14, left: 16, bottom: 28, right: 16)
        v.text = text
        v.accessibilityIdentifier = "selectableText"
        return v
    }

    func updateUIView(_ v: UITextView, context: Context) {
        if v.text != text { v.text = text }
    }
}

/// Back to the newest message: a round button over the page, above the composer.
struct JumpToLatest: View {
    /// How far above the newest the page has to be before it shows.
    nonisolated static let after: CGFloat = 240
    /// The room it takes at the page's right edge: its width and its margin, less the page's.
    nonisolated static let room: CGFloat = 44 + 14 - 16 + 6
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "arrow.down")
                .font(.system(size: 16, weight: .semibold))
                .foregroundStyle(Palette.text)
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .glassEffect(.regular.interactive(), in: Circle())
        .accessibilityLabel("Jump to latest")
        .accessibilityIdentifier("jumpToLatest")
    }
}

/// A steer of the running turn, on one line.
struct SteerRow: View {
    let text: String
    let status: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "arrow.turn.down.right").font(Typeface.meta).foregroundStyle(Palette.link)
            Text("You steered").font(Typeface.label.weight(.semibold)).foregroundStyle(Palette.link)
            Text(text).font(Typeface.label).foregroundStyle(Palette.text).lineLimit(3)
            Spacer(minLength: 4)
            if let words = statusWords { Text(words).font(Typeface.meta).foregroundStyle(words == "not sent" ? Palette.bad : Palette.dim) }
        }
        .copyMenu(text)
        .accessibilityElement(children: .combine)
    }

    private var statusWords: String? {
        switch status {
        case nil: nil
        case "sending", "not sent": status
        default: "delivered"
        }
    }
}

/// A tool call on one line: its name and what it ran; open for its output.
struct ToolRow: View {
    @Environment(AppModel.self) private var model
    let message: Message
    var thread: String?
    /// Opened: the page brings its output into view.
    var opened: () -> Void = {}
    @State private var open = false
    @State private var showingAll = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                open.toggle()
                if open { opened() }
            } label: {
                HStack(spacing: 7) {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 9, weight: .semibold))
                        .rotationEffect(.degrees(open ? 90 : 0))
                        .foregroundStyle(Palette.dim)
                    Text(message.toolName ?? "tool")
                        .font(Typeface.mono(.footnote))
                        .foregroundStyle(message.isError ? Palette.bad : Palette.text)
                    Text(message.toolSummary ?? "")
                        .font(Typeface.mono(.footnote))
                        .foregroundStyle(Palette.dim)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Spacer(minLength: 0)
                    if message.pending {
                        ProgressView().controlSize(.mini)
                    } else if let s = message.status {
                        Text(s).font(Typeface.meta).foregroundStyle(Palette.warn)
                    } else if message.isError {
                        Text("error").font(Typeface.meta).foregroundStyle(Palette.bad)
                    }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .contentShape(.contextMenuPreview, RoundedRectangle(cornerRadius: 6, style: .continuous))
            .contextMenu {
                let copy = ThreadPlan.toolCopy(message)
                Button(copy.label, systemImage: "doc.on.doc") { UIPasteboard.general.string = copy.text }
                if !message.cut.isEmpty, let thread {
                    // Cut on the wire: the whole of it, from the server.
                    Button("Copy output", systemImage: "doc.on.clipboard") {
                        Task {
                            if let text = await model.wholeText(message, in: thread) { UIPasteboard.general.string = text }
                        }
                    }
                } else if !message.pending, let output = ThreadPlan.toolDetail(message, whole: true).nilIfBlank {
                    Button("Copy output", systemImage: "doc.on.clipboard") { UIPasteboard.general.string = output }
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("\(message.toolName ?? "Tool"): \(message.toolSummary ?? "")\(message.pending ? ", running" : message.isError ? ", error" : "")")
            .accessibilityHint(open ? "Hides the output" : "Shows the output")
            if open {
                if !message.cut.isEmpty, let thread {
                    CutBar(message: message, thread: thread, showAll: { showingAll = true })
                }
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(ThreadPlan.toolDetail(message))
                        .font(Typeface.mono(.caption))
                        .foregroundStyle(Palette.text)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: true, vertical: false)
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 6).fill(Palette.muted))
            }
        }
        .sheet(isPresented: $showingAll) {
            if let thread {
                WholeOutput(message: message, thread: thread)
            }
        }
    }
}

/// Over a tool output that went out cut: how much came, and Show all / Copy all.
struct CutBar: View {
    @Environment(AppModel.self) private var model
    let message: Message
    let thread: String
    let showAll: () -> Void
    @State private var copied = false

    var body: some View {
        let total = message.cut.reduce(Int64(0)) { $0 + $1.bytes }
        HStack(spacing: 12) {
            Text("Cut to \(Format.bytes(Int64(message.cut.count * Paging.cap))) of \(Format.bytes(total))")
                .font(Typeface.meta)
                .foregroundStyle(Palette.dim)
            Spacer(minLength: 0)
            Button("Show all", action: showAll)
                .font(Typeface.meta)
                .accessibilityIdentifier("showAll")
            Button(copied ? "Copied" : "Copy all") {
                Task {
                    if let text = await model.wholeText(message, in: thread) {
                        UIPasteboard.general.string = text
                        copied = true
                        try? await Task.sleep(for: .seconds(1.5))
                        copied = false
                    }
                }
            }
            .font(Typeface.meta)
            .accessibilityIdentifier("copyAll")
        }
        // Clear of Jump to latest, which floats at the page's right edge.
        .padding(.trailing, JumpToLatest.room)
    }
}

/// A cut tool output, whole: fetched when opened, read-only and selectable, with Copy.
struct WholeOutput: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let message: Message
    let thread: String
    /// Asks to jump to the end, counted (a log is read from its end).
    @State private var toEnd = 0

    var body: some View {
        NavigationStack {
            Group {
                switch model.whole(message) {
                case .text(let text)?:
                    VStack(alignment: .leading, spacing: 0) {
                        Text("\(text.split(separator: "\n", omittingEmptySubsequences: false).count.formatted()) lines, all of it")
                            .font(Typeface.meta)
                            .foregroundStyle(Palette.dim)
                            .padding(.horizontal, 16)
                            .padding(.vertical, 8)
                            .accessibilityIdentifier("wholeSize")
                        MonoText(text: text, toEnd: toEnd)
                    }
                case .failed(let why)?:
                    ContentUnavailableView("Couldn't load it", systemImage: "exclamationmark.triangle", description: Text(why))
                default:
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .background(Palette.background)
            .navigationTitle("\(message.toolName ?? "Tool") \(message.toolSummary ?? "")")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItemGroup(placement: .primaryAction) {
                    if case .text(let text)? = model.whole(message) {
                        Button("To the end", systemImage: "arrow.down.to.line") { toEnd += 1 }
                            .accessibilityIdentifier("wholeToEnd")
                        Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = text }
                    }
                }
            }
        }
        .task { _ = await model.wholeText(message, in: thread) }
    }
}

/// Long monospaced text, read-only and selectable: a text view, which scrolls megabytes well.
struct MonoText: UIViewRepresentable {
    let text: String
    /// Bumped to scroll to the end.
    var toEnd = 0

    final class Coordinator {
        var toEnd = 0
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> UITextView {
        let v = UITextView()
        v.isEditable = false
        v.isSelectable = true
        v.backgroundColor = .clear
        v.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        v.textContainerInset = UIEdgeInsets(top: 8, left: 12, bottom: 24, right: 12)
        v.accessibilityIdentifier = "wholeText"
        return v
    }

    func updateUIView(_ v: UITextView, context: Context) {
        if v.text != text { v.text = text }
        if toEnd != context.coordinator.toEnd {
            context.coordinator.toEnd = toEnd
            v.scrollRangeToVisible(NSRange(location: max(0, (v.text as NSString).length - 1), length: 1))
        }
    }
}

struct NoticeRow: View {
    let text: String
    let isError: Bool

    var body: some View {
        Label(text, systemImage: "info.circle")
            .font(Typeface.meta)
            .foregroundStyle(isError ? Palette.bad : Palette.dim)
    }
}

/// A subagent or a hand-off: what it's called, its model, how it went; open for what it was
/// asked and what it said, and a delegate's own thread to open.
struct CardView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dynamicTypeSize) private var textSize
    let card: Card
    @Binding var open: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button {
                withAnimation(.easeOut(duration: 0.2)) { open.toggle() }
            } label: {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(alignment: textSize.isAccessibilitySize ? .top : .center, spacing: 7) {
                        BotIcon()
                        // Large text: the model and calls under the title, so neither is cut to a word.
                        let head = textSize.isAccessibilitySize ? AnyLayout(VStackLayout(alignment: .leading, spacing: 2)) : AnyLayout(HStackLayout(spacing: 7))
                        head {
                            Text(card.title).font(Typeface.label.weight(.semibold)).foregroundStyle(Palette.text).lineLimit(textSize.isAccessibilitySize ? 3 : 1)
                            Text(card.meta.joined(separator: " · ")).font(Typeface.meta).foregroundStyle(Palette.dim).lineLimit(textSize.isAccessibilitySize ? 2 : 1)
                        }
                        Spacer(minLength: 4)
                        if card.running {
                            ProgressView().controlSize(.mini)
                        } else if let p = card.problem {
                            Text(p).font(Typeface.meta).foregroundStyle(Palette.bad)
                        }
                    }
                    if !open { second }
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if open {
                VStack(alignment: .leading, spacing: 8) {
                    if let task = card.task {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Asked to").font(Typeface.meta).foregroundStyle(Palette.dim)
                            Text(task).font(Typeface.label).foregroundStyle(Palette.text).textSelection(.enabled)
                        }
                    }
                    if let report = card.report {
                        Rectangle().fill(Palette.border).frame(height: 1)
                        MarkdownText(source: report)
                    } else if card.running {
                        second
                    }
                    if let thread = card.thread {
                        Button {
                            model.path.append(thread)
                        } label: {
                            Label("Open thread", systemImage: "arrow.up.right")
                                .font(Typeface.label.weight(.semibold))
                                .foregroundStyle(Palette.link)
                                .frame(minHeight: 32)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("openThread-\(thread)")
                    }
                }
                .padding(.top, 4)
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 9)
        .padding(.bottom, 10)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Palette.card))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Palette.border, lineWidth: 1))
        .contentShape(.contextMenuPreview, RoundedRectangle(cornerRadius: 10, style: .continuous))
        .contextMenu {
            if let thread = card.thread {
                Button("Open thread", systemImage: "arrow.up.right") { model.path.append(thread) }
            }
            if let report = card.report {
                Button("Copy answer", systemImage: "doc.on.doc") { UIPasteboard.general.string = report }
            }
        }
        .accessibilityElement(children: open ? .contain : .combine)
        .accessibilityLabel("\(card.title), \(card.meta.joined(separator: ", "))\(card.running ? ", working" : card.problem.map { ", \($0)" } ?? "")")
        .accessibilityHint(open ? "Folds the card" : "Shows what it was asked and its answer")
    }

    /// The line under the title: its latest call while it works, else its answer's first line.
    @ViewBuilder private var second: some View {
        if card.running {
            if let name = card.latestName {
                HStack(spacing: 6) {
                    Text(name).font(Typeface.mono(.caption)).foregroundStyle(Palette.text)
                    Text(card.latestSummary ?? "").font(Typeface.mono(.caption)).foregroundStyle(Palette.dim).lineLimit(1)
                }
            } else {
                Text("Working…").font(Typeface.label).foregroundStyle(Palette.dim)
            }
        } else if let line = card.report.flatMap(Format.reportPreview) {
            Text(line).font(Typeface.label).foregroundStyle(Palette.dim).lineLimit(1)
        }
    }
}

/// "2 agents answered": in place of the prompt that brought their answers.
struct AnsweredRow: View {
    let text: String
    let action: (() -> Void)?

    var body: some View {
        Button {
            action?()
        } label: {
            HStack(spacing: 7) {
                Image(systemName: "arrowshape.turn.up.right").font(Typeface.meta)
                Text(text).font(Typeface.label).lineLimit(1)
            }
            .foregroundStyle(Palette.dim)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(action == nil)
        .accessibilityLabel(text)
        .accessibilityHint(action == nil ? "" : "Shows their cards")
    }
}
