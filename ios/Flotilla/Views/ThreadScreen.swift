import FlotillaKit
import SwiftUI

/// A thread: its header, its messages as they stream, and the composer with its PR.
struct ThreadScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let id: String
    @State private var openCards: Set<String> = []
    @State private var asking: ThreadAsk?
    /// Scrolled up from the newest: Jump to latest shows.
    @State private var awayFromLatest = false
    /// Where the page is, to keep it there as older messages go in above.
    @State private var position = ScrollPosition(idType: String.self)
    @State private var keep = KeepPlace()
    /// "Loading earlier messages…" is on screen, under the bars, or within a screen of them.
    @State private var olderNear = false
    /// The row gone to from search, marked for a moment.
    @State private var marked: String?
    @FocusState private var composing: Bool

    var body: some View {
        let summary = model.summary(id)
        // Its older pages held, then its view's newest messages.
        let detail = model.paged(id)
        let older = model.older(id)
        let plan = detail.map { ThreadPlan.plan($0, summary: summary, partials: model.live[id] ?? [], local: model.localSends[id] ?? [], catalog: model.catalog, schedules: model.schedules) }
        ScrollViewReader { proxy in
            ScrollView {
                if let starting = model.starting[id], detail?.thread == nil || plan?.rows.isEmpty == true {
                    // Asked for from this phone: what was asked, until the thread shows itself.
                    VStack(alignment: .leading, spacing: 14) {
                        PromptCard(from: nil, text: starting.prompt, status: starting.written ? nil : "sending", attachments: starting.files, notKept: false)
                        HStack(spacing: 7) {
                            ProgressView().controlSize(.mini)
                            Text("Starting…").font(Typeface.label).foregroundStyle(Palette.dim)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.top, 8)
                } else if let detail, let plan {
                    if detail.thread == nil {
                        empty("This thread is gone")
                    } else if plan.rows.isEmpty {
                        empty(detail.thread?.conversationID == nil ? "Starting…" : "No messages yet")
                    } else {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            if older.more {
                                // Scrolled up to it: the page above. The lazy stack keeps it while
                                // a page goes in under it, so it may never appear again: while it's
                                // near, each page in asks for the next.
                                OlderRow(loading: older.loading)
                                    .onAppear { model.loadOlder(id) }
                                    .onGeometryChange(for: Bool.self) { g in
                                        // Under the bars counts, and a screen above them: the page
                                        // above is asked for before it's needed.
                                        let f = g.frame(in: .scrollView)
                                        let height = g.bounds(of: .scrollView)?.height ?? 0
                                        return f.maxY > -height && f.minY < height
                                    } action: { olderNear = $0 }
                                    .onDisappear { olderNear = false }
                                    .task(id: OlderAsk(near: olderNear, loading: older.loading)) {
                                        guard olderNear, !older.loading else { return }
                                        // The page just in is laid out first: it may have moved
                                        // the loader away.
                                        try? await Task.sleep(for: .milliseconds(120))
                                        guard !Task.isCancelled else { return }
                                        model.loadOlder(id)
                                    }
                            }
                            ForEach(Array(plan.rows.enumerated()), id: \.element.id) { i, row in
                                rowView(row, proxy: proxy)
                                    // Gone to from search: the selection's lavender behind it.
                                    .background {
                                        if marked == row.id {
                                            RoundedRectangle(cornerRadius: 12)
                                                .fill(Palette.lavender.opacity(0.3))
                                                .stroke(Palette.ring.opacity(0.7), lineWidth: 1.5)
                                                .padding(.horizontal, -8)
                                                .padding(.vertical, -5)
                                        }
                                    }
                                    .padding(.top, i == 0 ? 8 : spacing(row, after: plan.rows[i - 1]))
                                    .id(row.id)
                                    // Where each row drawn is, to keep one still as older
                                    // messages go in above.
                                    .onGeometryChange(for: CGFloat.self) { $0.frame(in: .scrollView).minY } action: { y in
                                        keep.tops[row.id] = y
                                        if let ask = keep.correction(row.id, y) { position.scrollTo(y: ask) }
                                    }
                                    .onDisappear { keep.tops[row.id] = nil }
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.bottom, 12)
                    }
                } else {
                    ProgressView().frame(maxWidth: .infinity).padding(.top, 80)
                }
            }
            .defaultScrollAnchor(.bottom)
            .defaultScrollAnchor(.bottom, for: .sizeChanges)
            .scrollDismissesKeyboard(.interactively)
            .onScrollGeometryChange(for: Bool.self) { g in
                // How far the newest is below the bottom of what shows (which runs under the
                // dock: the bottom inset).
                g.contentSize.height + g.contentInsets.bottom - g.visibleRect.maxY > JumpToLatest.after
            } action: { _, away in
                awayFromLatest = away
            }
            .scrollPosition($position)
            // Opened at a search hit: once its message is shown (its pages in), its row comes to
            // the top, marked for a moment.
            .onChange(of: jumpRow(detail, plan), initial: true) { _, row in
                if let row { go(to: row, proxy: proxy) }
            }
            // Older messages are about to go in above: the first row on screen is noted, and
            // kept exactly where it is as they go in and the lazy stack sizes them, until a
            // finger moves the page or a few seconds pass.
            .onChange(of: model.arriving[id]) {
                keep.note()
            }
            // Sent from here, wherever the page was: down to what was sent.
            .onChange(of: model.localSends[id]?.count ?? 0) { was, now in
                guard now > was, let last = plan?.rows.last?.id else { return }
                Task {
                    // The lazy stack places it first.
                    try? await Task.sleep(for: .milliseconds(60))
                    keep.scroll(proxy, to: last, anchor: .bottom, animation: reduceMotion ? nil : .smooth(duration: 0.35))
                }
            }
            .onScrollPhaseChange { _, phase in
                // A finger on the page: it goes where the finger takes it.
                keep.touched = phase == .tracking || phase == .interacting
                if keep.touched { keep.release() }
            }
            .onScrollGeometryChange(for: KeepPlace.Geometry.self) { g in
                KeepPlace.Geometry(offset: g.contentOffset.y, inset: g.contentInsets.top)
            } action: { _, new in
                keep.last = new
            }
            .overlay(alignment: .bottomTrailing) {
                // Above the dock (the scroll view's safe area ends there), at the thumb's side.
                ZStack {
                    if awayFromLatest, let last = plan?.rows.last?.id {
                        JumpToLatest {
                            keep.scroll(proxy, to: last, anchor: .bottom, animation: .smooth(duration: 0.35))
                        }
                        .transition(.opacity.combined(with: .scale(scale: 0.85, anchor: .center)))
                    }
                }
                .padding(.trailing, 14)
                .padding(.bottom, 10)
                .animation(.easeOut(duration: 0.2), value: awayFromLatest)
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                if let plan, !plan.pinned.isEmpty {
                    Pinned(cards: plan.pinned) { card in
                        show(card.id, anchor: .center, proxy: proxy)
                    }
                }
            }
        }
        .background(Palette.background)
        // A preview on its way: "Opening port 3000…".
        .overlay(alignment: .top) { PreviewNotice(id: id) }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            // An archive here (this thread's merge, or idle ones) can be undone here too.
            VStack(spacing: 0) {
                if let notice = model.archived {
                    ArchivedBanner(notice: notice) { model.undoArchive() }
                        .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                }
                Dock(id: id, focus: $composing)
            }
            .animation(reduceMotion ? nil : .snappy(duration: 0.28), value: model.archived)
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) { Header(id: id) }
            if let summary {
                if PreviewButton.shows(model.details[id]?.thread ?? summary.thread) {
                    ToolbarItem(placement: .topBarTrailing) { PreviewButton(id: id) }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        ThreadMenuItems(summary: summary) { asking = $0 }
                    } label: {
                        Image(systemName: "ellipsis")
                    }
                    .accessibilityLabel("More")
                    .accessibilityIdentifier("threadMenu")
                }
            }
        }
        .threadAsks($asking)
        .onAppear { model.open(thread: id) }
        .onDisappear { model.close(thread: id) }
    }

    /// The row to go to for the message this thread was opened at, once it's shown.
    private func jumpRow(_ detail: ThreadDetail?, _ plan: ThreadPlan.Plan?) -> String? {
        guard let seq = model.jumps[id], let detail, let plan, detail.messages.contains(where: { $0.seq == seq }) else { return nil }
        return plan.row(for: seq)
    }

    private func go(to row: String, proxy: ScrollViewProxy) {
        model.jumps[id] = nil
        // A call folded into a subagent's card, or into a run of calls: it opens.
        if row.hasPrefix("agent:") || row.hasPrefix("sub:") || row.hasPrefix("calls:") { openCards.insert(row) }
        Task {
            // The lazy stack places the rows that just went in first.
            try? await Task.sleep(for: .milliseconds(80))
            keep.scroll(proxy, to: row, anchor: .top)
            marked = row
            try? await Task.sleep(for: .seconds(1.4))
            guard marked == row else { return }
            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.5)) { marked = nil }
        }
    }

    /// Scroll to a card, then open it: opened first, it would push the page down a row at once,
    /// before the scroll starts.
    private func show(_ card: String, anchor: UnitPoint, proxy: ScrollViewProxy) {
        guard !openCards.contains(card) else {
            keep.scroll(proxy, to: card, anchor: anchor, animation: .default)
            return
        }
        keep.scroll(proxy, to: card, anchor: anchor, animation: reduceMotion ? nil : .smooth(duration: 0.35)) {
            _ = withAnimation(reduceMotion ? nil : .easeOut(duration: 0.2)) { openCards.insert(card) }
        }
    }

    private func empty(_ text: String) -> some View {
        VStack(spacing: 14) {
            PixelSail()
            Text(text).font(Typeface.body).foregroundStyle(Palette.dim)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 120)
    }

    /// Gaps: a turn starts with room above it; a run of tool calls sits tight.
    private func spacing(_ row: Row, after prev: Row) -> CGFloat {
        switch row {
        case .prompt, .answered: 22
        case .tool, .calls: prev.isTool ? 7 : 12
        case .card: 10
        default: 12
        }
    }

    @ViewBuilder private func rowView(_ row: Row, proxy: ScrollViewProxy) -> some View {
        switch row {
        case .prompt(_, let from, let text, let status, let attachments, let notKept):
            PromptCard(from: from, text: text, status: status, attachments: attachments, notKept: notKept)
        case .steer(_, let text, let status):
            SteerRow(text: text, status: status)
        case .text(_, let text):
            ReplyText(source: text)
        case .tool(_, let m):
            ToolRow(message: m, thread: id) {
                // Its output goes in under it: bring the row up so the output shows, as a card does.
                Task { keep.scroll(proxy, to: row.id, anchor: .top, animation: .default) }
            }
        case .calls(_, let calls):
            CallsRow(calls: calls, thread: id, open: Binding(get: { openCards.contains(row.id) }, set: { open in
                if open { openCards.insert(row.id) } else { openCards.remove(row.id) }
            })) {
                Task { keep.scroll(proxy, to: row.id, anchor: .top, animation: .default) }
            }
        case .notice(_, let text, let isError):
            NoticeRow(text: text, isError: isError)
        case .ask(_, let m, let answer):
            AskCard(message: m, thread: id, answer: answer)
        case .card(let card):
            CardView(card: card, open: Binding(get: { openCards.contains(card.id) }, set: { open in
                if open {
                    openCards.insert(card.id)
                    // The page keeps its bottom in place as it grows: bring the card back.
                    Task { keep.scroll(proxy, to: card.id, anchor: .top, animation: .default) }
                } else {
                    openCards.remove(card.id)
                }
            }))
        case .answered(_, let text, let target):
            AnsweredRow(text: text, action: target.map { t in
                { show(t, anchor: .top, proxy: proxy) }
            })
        case .partial(_, let text):
            MarkdownText(source: text, streaming: true)
        }
    }

}

/// Older messages going in above the page: the first row on screen when they're about to,
/// and where it was, kept there by scrolling as much as it moves. Not observed: it only carries
/// the note across.
final class KeepPlace {
    struct Geometry: Equatable {
        /// The scroll's offset, and the top inset (the bars) it counts from: `scrollTo(y:)`
        /// takes the content's own y, under the bars, which is their sum.
        var offset: CGFloat
        var inset: CGFloat
    }

    var last: Geometry?
    /// Each row drawn: its top, in the scroll view's coordinates.
    var tops: [String: CGFloat] = [:]
    /// The row kept still, where it was, and until when. Any scroll the screen asks for (Jump
    /// to latest, a card) goes through `scroll`, which lets go of it first.
    private(set) var anchor: (id: String, top: CGFloat)?
    var until = Date.distantPast
    /// A finger is on the page.
    var touched = false

    /// Older messages are about to go in: keep the first row on screen where it is, unless a
    /// finger is moving the page.
    func note() {
        guard !touched, let first = tops.filter({ $0.value >= 0 }).min(by: { $0.value < $1.value }) else { return }
        anchor = (first.key, first.value)
        until = .now + 4
    }

    /// The y to scroll to so that row `id`, now at `top`, is back where it was; nil if it's
    /// not the row kept, it hasn't moved, or the hold is over.
    func correction(_ id: String, _ top: CGFloat) -> CGFloat? {
        guard let a = anchor, a.id == id, let g = last else { return nil }
        guard Date.now < until else {
            release()
            return nil
        }
        guard abs(top - a.top) > 0.5 else { return nil }
        return g.offset + (top - a.top) + g.inset
    }

    /// Lets go of the row kept still.
    func release() {
        anchor = nil
    }

    /// Every scroll the screen asks for: the hold lets go first, or it would pull the page back.
    func scroll(_ proxy: ScrollViewProxy, to row: String, anchor at: UnitPoint, animation: Animation? = nil, completion: (() -> Void)? = nil) {
        release()
        if let completion {
            withAnimation(animation, { proxy.scrollTo(row, anchor: at) }, completion: completion)
        } else {
            withAnimation(animation) { proxy.scrollTo(row, anchor: at) }
        }
    }
}

/// When to ask for the page above: the loader near, and no page on its way.
struct OlderAsk: Equatable {
    var near: Bool
    var loading: Bool
}

/// Above the oldest message shown while there are older ones: loading them as it comes into
/// sight.
struct OlderRow: View {
    let loading: Bool

    var body: some View {
        HStack(spacing: 7) {
            ProgressView().controlSize(.mini)
            Text("Loading earlier messages…").font(Typeface.label).foregroundStyle(Palette.dim)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 14)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("olderMessages")
        .opacity(loading ? 1 : 0.6)
    }
}

/// The header: the VM's pip and the title, then (when there's something to say) the VM's state
/// in words or the running turn's time, and the branch.
struct Header: View {
    @Environment(AppModel.self) private var model
    let id: String

    var body: some View {
        let summary = model.summary(id)
        let thread = model.details[id]?.thread ?? summary?.thread
        let stopping = model.stopping.contains(id)
        let branch = thread?.branch?.nilIfBlank
        let repo = branch == nil ? model.details[id]?.projectRepo : nil
        let worded = summary.map { s in thread.map { VMState.worded(summary: s, thread: $0) } ?? false } ?? false
        VStack(spacing: 1) {
            HStack(spacing: 6) {
                if let summary, let thread {
                    Pip(color: VMState.pip(summary: summary, thread: thread, stopping: stopping))
                }
                Text(model.title(id, thread?.title)?.nilIfBlank ?? (model.starting[id] != nil ? "New thread" : ""))
                    .font(.headline)
                    .foregroundStyle(Palette.text)
                    .lineLimit(1)
            }
            if worded || branch != nil || repo != nil {
                HStack(spacing: 5) {
                    if worded, let summary, let thread {
                        VMState(summary: summary, thread: thread, stopping: stopping)
                    }
                    if worded, branch != nil || repo != nil {
                        Text("·").foregroundStyle(Palette.dim)
                    }
                    if let branch {
                        Text(branch).font(Typeface.mono(.caption2)).foregroundStyle(Palette.dim).lineLimit(1).truncationMode(.middle)
                    } else if let repo {
                        Text(repo).foregroundStyle(Palette.dim).lineLimit(1)
                    }
                }
                .font(Typeface.meta)
            }
        }
        .frame(maxWidth: 260)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

/// The VM's state in words, when they say what its pip (by the title) doesn't: the running
/// turn's time, "stopped", "kept running"… An idle VM's pip says it all.
struct VMState: View {
    let summary: ThreadSummary
    let thread: FlotillaKit.Thread
    var stopping = false

    var body: some View {
        if summary.activity == .working, let start = summary.turnStartedMs {
            Group {
                if stopping {
                    Text("Stopping the turn…").foregroundStyle(Palette.warn)
                } else {
                    TimelineView(.periodic(from: .now, by: 1)) { _ in
                        Text(Format.elapsed(since: start)).monospacedDigit().foregroundStyle(Palette.ok)
                    }
                }
            }
            .accessibilityLabel(stopping ? "Stopping the turn" : "Working")
        } else if let words = Self.words(thread).0 {
            Text(words).foregroundStyle(Self.words(thread).2)
        }
    }

    /// Whether there are words to show.
    static func worded(summary: ThreadSummary, thread: FlotillaKit.Thread) -> Bool {
        (summary.activity == .working && summary.turnStartedMs != nil) || words(thread).0 != nil
    }

    /// The pip's colour: working, or the VM's state.
    static func pip(summary: ThreadSummary, thread: FlotillaKit.Thread, stopping: Bool) -> Color {
        if summary.activity == .working, summary.turnStartedMs != nil { return stopping ? Palette.warn : Palette.ok }
        return words(thread).1
    }

    /// The words for a VM that isn't working (none for one that's simply up), its pip's colour,
    /// and the words' colour.
    static func words(_ t: FlotillaKit.Thread) -> (String?, Color, Color) {
        switch t.state {
        case .creating, .starting: return ("starting", Palette.warn, Palette.warn)
        case .stopping: return ("stopping", Palette.warn, Palette.warn)
        case .stopped: return ("stopped", Palette.dim.opacity(0.6), Palette.dim)
        case .expired: return ("expired", Palette.dim.opacity(0.6), Palette.dim)
        case .error: return (t.sandboxID == nil ? "not started" : "VM error", Palette.bad, Palette.bad)
        case .idle, .running: return (t.keepRunning ? "kept running" : nil, Palette.ok, Palette.dim)
        }
    }
}

/// Running subagents, pinned under the header: one row, or "2 subagents running".
struct Pinned: View {
    let cards: [Card]
    let show: (Card) -> Void

    var body: some View {
        Button {
            if let first = cards.first { show(first) }
        } label: {
            HStack(spacing: 8) {
                BotIcon()
                if cards.count == 1, let c = cards.first {
                    Text(c.title).font(Typeface.label.weight(.semibold)).foregroundStyle(Palette.text).lineLimit(1)
                    Group {
                        if let name = c.latestName {
                            Text("\(Text(name).foregroundStyle(Palette.text)) \(c.latestSummary ?? "")").font(Typeface.mono(.caption))
                        } else {
                            Text(c.meta.joined(separator: " · ")).font(Typeface.meta)
                        }
                    }
                    .foregroundStyle(Palette.dim)
                    .lineLimit(1)
                } else {
                    Text("\(cards.count) subagents running").font(Typeface.label.weight(.semibold)).foregroundStyle(Palette.text).lineLimit(1)
                    Text(cards.map(\.title).joined(separator: ", ")).font(Typeface.meta).foregroundStyle(Palette.dim).lineLimit(1)
                }
                Spacer(minLength: 4)
                ProgressView().controlSize(.mini)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Palette.card))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Palette.border, lineWidth: 1))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 12)
        .padding(.top, 6)
        .padding(.bottom, 4)
        .background(Palette.background.opacity(0.95))
        .accessibilityLabel(cards.count == 1 ? "\(cards[0].title), working" : "\(cards.count) subagents running: \(cards.map(\.title).joined(separator: ", "))")
        .accessibilityHint("Shows the card")
    }
}
