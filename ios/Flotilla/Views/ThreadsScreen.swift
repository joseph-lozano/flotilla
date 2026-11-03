import FlotillaKit
import SwiftUI

/// The threads, grouped as on the Mac: Unread, Working and Idle, with Archived folded.
struct ThreadsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var showArchived = false
    @State private var asking: ThreadAsk?
    @State private var showUsage = false
    @State private var showLimits = false
    @Environment(\.dynamicTypeSize) private var typeSize
    /// What's typed in the search field, and what the server found for it.
    @State private var query = ""
    @State private var found = Found()

    var body: some View {
        Group {
            if model.startsHere { startHere } else { list }
        }
        .sheet(isPresented: $showUsage) { UsageSheet() }
        .sheet(isPresented: $showLimits) { UsageSheet(tab: .agents) }
        // A thread came: a refused start's words don't wait for the next time there's none.
        .onChange(of: model.startsHere) { _, here in if !here { model.startHere = nil } }
    }

    /// No thread yet: New thread itself, with Start in the bar beside the usage, or the server's
    /// setup while a check threads need fails.
    @ViewBuilder private var startHere: some View {
        Group {
            if model.settings?.stopsThreads == true {
                SetupScreen()
            } else {
                // A refused start comes back here, with what was written and why.
                NewThreadSheet(start: model.startHere ?? NewThreadStart(project: model.lastProject)) { AnyView(bottomBar($0)) }
                    .id(model.startHere?.id)
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            if case .offline = model.connection {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text("Reconnecting…")
                }
                .font(Typeface.meta)
                .foregroundStyle(Palette.dim)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 4)
                .background(Palette.background)
            }
        }
        .navigationTitle("")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { topBar }
    }

    @ViewBuilder private var list: some View {
        let groups = Groups(model.sidebar, hiding: model.deleting)
        let starting = startingRows
        List {
            if case .refused(let why) = model.connection {
                Section {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(why).font(Typeface.label).foregroundStyle(Palette.bad)
                        Button("Pair again") { model.showPairing = true }.font(Typeface.label.weight(.semibold))
                    }
                    .listRowBackground(Palette.background)
                }
            } else if case .offline = model.connection {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.mini)
                    Text("Reconnecting…")
                }
                .font(Typeface.meta)
                .foregroundStyle(Palette.dim)
                .listRowBackground(Palette.background)
                .listRowSeparator(.hidden)
                .listRowInsets(EdgeInsets(top: 0, leading: 20, bottom: 0, trailing: 16))
            }
            // A query with no words (a lone quote) is no query: the plain list, as on the desktop.
            if searching {
                searchResults
            } else {
            group("Unread", groups.unread)
            group("Working", groups.working, starting: starting)
            group("Idle", groups.idle)
            if !groups.archived.isEmpty {
                Section {
                    if showArchived { rows(groups.archived) }
                } header: {
                    Button {
                        showArchived.toggle()
                    } label: {
                        HStack(spacing: 6) {
                            Text("Archived")
                            Text("\(groups.archived.count)")
                            Spacer()
                            Image(systemName: "chevron.right").rotationEffect(.degrees(showArchived ? 90 : 0))
                        }
                        .modifier(SectionTitle())
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .pinnedHeader()
                    .accessibilityLabel("Archived, \(groups.archived.count) threads")
                    .accessibilityHint(showArchived ? "Folds them" : "Shows them")
                }
            }
            }
            if model.sidebar == nil, !refused {
                ProgressView().frame(maxWidth: .infinity).listRowBackground(Palette.background).listRowSeparator(.hidden)
            }
        }
        .listStyle(.plain)
        // The mockup's groups follow on closely: no extra gap above each heading.
        .listSectionSpacing(0)
        .scrollContentBackground(.hidden)
        .background(Palette.background)
        .navigationTitle("Threads")
        .toolbar {
            scheduledButton
            topBar
        }
        .searchable(text: $query, prompt: "Threads and messages")
        .autocorrectionDisabled()
        .textInputAutocapitalization(.never)
        // As typing pauses, the server is asked for the messages; titles are matched here.
        .task(id: query) {
            let q = query.trimmingCharacters(in: .whitespaces)
            guard Search.findsMessages(q) else {
                found = Found()
                return
            }
            found.searching = true
            try? await Task.sleep(for: .milliseconds(160))
            guard !Task.isCancelled else { return }
            let result = await model.search(q)
            guard !Task.isCancelled else { return }
            switch result {
            case .success(let hits): found = Found(query: q, hits: hits)
            case .failure(let e): found = Found(query: q, hits: found.query == q ? found.hits : [], problem: e.message)
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(spacing: 0) {
                if let notice = model.archived {
                    ArchivedBanner(notice: notice) { model.undoArchive() }
                        .transition(reduceMotion ? .opacity : .move(edge: .bottom).combined(with: .opacity))
                }
                // Searching: the results, not New thread, above the field.
                if !searching { bottomBar(newThreadButton) }
            }
            .animation(reduceMotion ? nil : .snappy(duration: 0.28), value: model.archived)
        }
        .threadAsks($asking)
    }

    /// Scheduled prompts, once there are any.
    @ToolbarContentBuilder private var scheduledButton: some ToolbarContent {
        if !model.schedules.isEmpty {
            ToolbarItem(placement: .topBarTrailing) {
                Button { model.showSchedules = true } label: { Image(systemName: "clock") }
                    .tint(Palette.dim)
                    .accessibilityLabel("Scheduled, \(model.schedules.count)")
                    .accessibilityIdentifier("openScheduled")
            }
        }
    }

    @ToolbarContentBuilder private var topBar: some ToolbarContent {
        ToolbarItem(placement: .topBarLeading) {
            Text("Alpha")
                .font(Typeface.meta)
                .foregroundStyle(Palette.dim)
                .padding(.horizontal, 6)
                .padding(.vertical, 1)
                .overlay(RoundedRectangle(cornerRadius: 4).stroke(Palette.border))
                .fixedSize()
        }
        .sharedBackgroundVisibility(.hidden)
        ToolbarItem(placement: .topBarTrailing) {
            Button { model.showSettings = true } label: { Image(systemName: "gearshape") }
                .tint(Palette.dim)
                .accessibilityLabel("Settings")
                .accessibilityIdentifier("openSettings")
        }
    }

    /// Whether what's typed has words to search for.
    private var searching: Bool { !Search.foldQuery(query).isEmpty }

    /// A thread as search sees it.
    private func subject(_ row: ThreadSummary) -> Search.Subject {
        let project = row.thread.repoProjectID.flatMap { p in model.sidebar?.projects.first { $0.id == p } }.map { "\($0.owner)/\($0.repo)" }
        let prs = row.pr.map { pr in [Search.PrRef(number: pr.number, title: pr.title, references: [pr.url] + (project.map { ["\($0)#\(pr.number)"] } ?? []))] } ?? []
        // What you last wrote, else when it began.
        let activity = row.promptedMs ?? Format.ms(row.thread.createdAt)
        return Search.Subject(id: row.id, title: row.thread.title, branch: row.thread.branch, project: project, prs: prs, activity: activity, opens: row.opens)
    }

    /// Searching: the threads by title (or PR, branch, project), ranked as on the Mac, then the
    /// threads found by what was said in them, each once at its best message, unless its title
    /// already lists it. Only what the server found for what's typed now: while it's asked
    /// again, "Searching messages…" stands in.
    @ViewBuilder private var searchResults: some View {
        let q = query.trimmingCharacters(in: .whitespaces)
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let all = (model.sidebar?.threads ?? []).filter { !model.deleting.contains($0.id) }.map(shown)
        let ranked = Search.rank(all, q, now: now) { subject($0) }.prefix(Search.threadRows)
        let titles = ranked.map(\.item)
        let hints = Dictionary(ranked.compactMap { r in r.match.hint.map { (r.item.id, "\($0.label): \($0.text)") } }, uniquingKeysWith: { a, _ in a })
        let folded = Search.foldQuery(q)
        let titled = Set(all.filter { Search.scoreThread(folded, subject($0), now: now, typos: false, picks: false) != nil }.map(\.id))
        let current = found.query == q ? found.hits : []
        let hits = Array(current.filter { $0.kind != .title && $0.seq != nil && !titled.contains($0.thread) }.prefix(Search.messageRows))
        if !titles.isEmpty {
            Section {
                rows(titles, pickedFor: q, hints: hints)
            } header: {
                Text("Threads").modifier(SectionTitle()).pinnedHeader()
            }
        }
        if !hits.isEmpty || found.searching {
            Section {
                if hits.isEmpty {
                    Text("Searching messages…")
                        .font(Typeface.label)
                        .foregroundStyle(Palette.dim)
                        .listRowBackground(Palette.background)
                        .listRowSeparator(.hidden)
                }
                ForEach(hits) { hit in
                    Button {
                        if let seq = hit.seq {
                            model.pickQuery = q
                            model.open(hit.thread, at: seq)
                        }
                    } label: {
                        HitRow(hit: hit)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("hit:\(hit.thread):\(hit.seq ?? -1)")
                    .listRowBackground(Palette.background)
                    .listRowSeparatorTint(Palette.border)
                    .listRowInsets(EdgeInsets(top: 9, leading: 20, bottom: 10, trailing: 16))
                }
            } header: {
                HStack(spacing: 6) {
                    Text("In messages")
                    if found.searching { ProgressView().controlSize(.mini) }
                }
                .modifier(SectionTitle())
                .pinnedHeader()
            }
        }
        if let problem = found.problem {
            Text("Couldn't search the messages: \(problem)")
                .font(Typeface.label)
                .foregroundStyle(Palette.bad)
                .listRowBackground(Palette.background)
                .listRowSeparator(.hidden)
        } else if titles.isEmpty && hits.isEmpty && !found.searching {
            Text("Nothing matches")
                .font(Typeface.body)
                .foregroundStyle(Palette.dim)
                .frame(maxWidth: .infinity)
                .padding(.top, 40)
                .listRowBackground(Palette.background)
                .listRowSeparator(.hidden)
        }
    }

    @ViewBuilder private func group(_ title: String, _ rows: [ThreadSummary], starting: [(id: String, starting: Starting)] = []) -> some View {
        let count = rows.count + starting.count
        if count > 0 {
            Section {
                ForEach(starting, id: \.id) { s in
                    NavigationLink(value: s.id) {
                        StartingRow(id: s.id, starting: s.starting, catalog: model.catalog)
                    }
                    .navigationLinkIndicatorVisibility(.hidden)
                    .listRowBackground(Palette.background)
                    .listRowSeparatorTint(Palette.border)
                    .listRowInsets(EdgeInsets(top: 9, leading: 20, bottom: 10, trailing: 16))
                }
                self.rows(rows)
            } header: {
                HStack(spacing: 6) {
                    Text(title)
                    Text("\(count)")
                }
                .modifier(SectionTitle())
                .pinnedHeader()
                .accessibilityElement(children: .combine)
                .accessibilityLabel("\(title), \(count) threads")
            }
        }
    }

    /// Threads this phone started that the server hasn't listed yet, newest first (several
    /// started together in the order their models were chosen).
    private var startingRows: [(id: String, starting: Starting)] {
        let listed = Set(model.sidebar?.threads.map(\.id) ?? [])
        return model.starting.filter { !listed.contains($0.key) }.map { (id: $0.key, starting: $0.value) }
            .sorted { ($0.starting.at, -$0.starting.rank) > ($1.starting.at, -$1.starting.rank) }
    }

    /// Rows that open their threads; `pickedFor`: the search they were found by, which the
    /// server remembers when one is opened, and `hints` why each matched if its title doesn't
    /// show it (in place of its preview).
    private func rows(_ rows: [ThreadSummary], pickedFor query: String? = nil, hints: [String: String] = [:]) -> some View {
        ForEach(rows) { row in
            Group {
                if let query {
                    Button { model.open(row.id, pickedFor: query) } label: {
                        ThreadRow(row: shown(row), catalog: model.catalog, waiting: model.sidebar?.waiting.contains(row.id) == true, repo: repo(row), hint: hints[row.id])
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                } else {
                    NavigationLink(value: row.id) {
                        ThreadRow(row: shown(row), catalog: model.catalog, waiting: model.sidebar?.waiting.contains(row.id) == true, repo: repo(row))
                    }
                    .navigationLinkIndicatorVisibility(.hidden)
                }
            }
            .contextMenu { ThreadMenuItems(summary: row) { asking = $0 } }
            .listRowBackground(Palette.background)
            .listRowSeparatorTint(Palette.border)
            .listRowInsets(EdgeInsets(top: 9, leading: 20, bottom: 10, trailing: 16))
            .swipeActions(edge: .trailing) {
                if row.activity == .archived {
                    Button("Unarchive", systemImage: "tray.and.arrow.up") { model.archive(row.id, false) }.tint(Palette.link)
                } else if row.activity != .expired {
                    Button("Archive", systemImage: "archivebox") { model.archive(row.id, true) }.tint(Palette.dim)
                }
            }
            .swipeActions(edge: .leading) {
                if row.activity == .unread {
                    Button("Read", systemImage: "envelope.open") { model.markRead(row.id) }.tint(Palette.ring)
                } else {
                    Button("Unread", systemImage: "envelope.badge") { model.markUnread(row.id) }.tint(Palette.ring)
                }
            }
        }
    }

    /// The row with a title just asked for.
    private func shown(_ row: ThreadSummary) -> ThreadSummary {
        guard let title = model.renaming[row.id] else { return row }
        var row = row
        row.thread.title = title
        return row
    }

    private var refused: Bool {
        if case .refused = model.connection { return true }
        return false
    }

    private func repo(_ row: ThreadSummary) -> String? {
        guard let p = row.thread.repoProjectID else { return nil }
        return model.sidebar?.projects.first { $0.id == p }?.repo
    }

    /// The usage and agent limits, then `trailing`: New thread, or New thread's Start when it's
    /// in the list's place.
    private func bottomBar(_ trailing: some View) -> some View {
        // At an accessibility type size the counts take a row of their own over New thread:
        // beside it they'd be crushed to a letter a line.
        let stacked = typeSize.isAccessibilitySize
        let layout = stacked ? AnyLayout(VStackLayout(alignment: .leading, spacing: 6)) : AnyLayout(HStackLayout())
        return layout {
            HStack(spacing: 8) {
                if model.sidebar?.usage != nil {
                    // The counts and their info button: either opens the usage in full.
                    Button {
                        showUsage = true
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: "gauge.with.dots.needle.33percent").foregroundStyle(Palette.dim)
                            if let usage = Format.usage(model.sidebar?.usage) {
                                Text("\(Text(usage.used).foregroundStyle(tone(usage.tone)).fontWeight(.medium))\(Text(usage.rest).foregroundStyle(Palette.dim))")
                            }
                            Image(systemName: "info.circle").foregroundStyle(Palette.dim)
                        }
                        .font(.footnote)
                        .frame(minHeight: 44)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(usageSpoken)
                    .accessibilityHint("Shows the full usage")
                    .accessibilityIdentifier("usage")
                }
                LimitsButton(limits: model.sidebar?.limits ?? []) { showLimits = true }
            }
            if !stacked { Spacer() }
            trailing
                // Its size first: the counts and the agent readout give way.
                .layoutPriority(1)
        }
        // The whole width, stacked too: else its ground covers only the stack, the rows showing
        // on either side.
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 18)
        .padding(.top, 12)
        .padding(.bottom, 4)
        .background {
            Palette.background.opacity(0.86).background(.ultraThinMaterial).ignoresSafeArea(edges: .bottom)
        }
        .overlay(alignment: .top) { Rectangle().fill(Palette.border).frame(height: 1) }
    }

    private var newThreadButton: some View {
        Button {
            model.newThread = NewThreadStart(project: model.lastProject)
        } label: {
            Label("New thread", systemImage: "plus")
                .font(.callout.weight(.semibold))
                .foregroundStyle(Palette.onPrimary)
                .padding(.leading, 16)
                .padding(.trailing, 20)
                // Taller with the text: a fixed height let it spill onto the search field.
                .frame(minHeight: 48)
                .background(Capsule().fill(Palette.primary))
        }
        .buttonStyle(.plain)
        .disabled(model.catalog == nil || model.sidebar == nil)
        .accessibilityIdentifier("newThread")
    }

    /// "Usage, 7 of 556 VM hours", or why VMs can't start.
    private var usageSpoken: String {
        guard let u = model.sidebar?.usage else { return "Usage" }
        if u.blocked != nil { return "Usage, can't start VMs" }
        return Format.vmHoursUsed(u).map { "Usage, \($0.used) of \($0.limit) VM hours" } ?? "Usage"
    }

    private func tone(_ t: Format.Tone) -> Color {
        switch t {
        case .bad: Palette.bad
        case .warn: Palette.warn
        default: Palette.text
        }
    }
}

/// What was just archived, with Undo: a quiet card over the New thread bar (or a thread's
/// composer) for a few seconds.
struct ArchivedBanner: View {
    let notice: ArchiveNotice
    let undo: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "archivebox").font(.footnote).foregroundStyle(Palette.dim)
            Text(notice.text).font(Typeface.label).foregroundStyle(Palette.text).lineLimit(1)
            Spacer(minLength: 8)
            Button(action: undo) {
                Text("Undo")
                    .font(Typeface.label.weight(.semibold))
                    .foregroundStyle(Palette.link)
                    .padding(.horizontal, 12)
                    .frame(maxHeight: .infinity)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("undoArchive")
        }
        .padding(.leading, 14)
        .padding(.trailing, 2)
        .frame(height: 44)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Palette.card))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(Palette.border, lineWidth: 1))
        .shadow(color: Palette.plum.opacity(0.12), radius: 10, y: 3)
        .padding(.horizontal, 14)
        .padding(.bottom, 8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("archivedNotice")
    }
}

/// A thread this phone asked for that the server hasn't listed yet: what was asked, its model,
/// starting.
struct StartingRow: View {
    let id: String
    let starting: Starting
    let catalog: Catalog?
    @Environment(\.dynamicTypeSize) private var textSize

    var body: some View {
        let s = starting.settings
        let model = Format.model(provider: s.provider, model: s.model, effort: s.effort, fast: s.fast, catalog: catalog)
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 8) {
                Pip(color: Palette.ok)
                Text(starting.prompt.split(separator: "\n").first.map(String.init) ?? starting.prompt)
                    .font(Typeface.rowTitle)
                    .foregroundStyle(Palette.text)
                    .lineLimit(textSize.isAccessibilitySize ? 3 : 1)
                Spacer(minLength: 6)
                Text(Format.ago(starting.at)).font(.footnote).foregroundStyle(Palette.dim)
            }
            HStack(spacing: 5) {
                ProviderLogo(provider: s.provider, size: 13)
                Text(model).lineLimit(1)
                Text("·").opacity(0.5)
                Text("starting").foregroundStyle(Palette.warn)
            }
            .font(.caption)
            .foregroundStyle(Palette.dim)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Working. \(starting.prompt). \(Format.providerName(s.provider)), \(model). starting")
        .accessibilityIdentifier("thread-\(id)")
    }
}

private struct SectionTitle: ViewModifier {
    func body(content: Content) -> some View {
        content
            .font(.caption.weight(.semibold))
            .textCase(.uppercase)
            .kerning(0.6)
            .foregroundStyle(Palette.dim)
            .padding(.top, 8)
    }
}

extension View {
    /// A plain list's section header, full width on the page's ground: pinned at the top as
    /// the list scrolls, it covers the rows going under it.
    func pinnedHeader() -> some View {
        padding(.horizontal, 16)
            .padding(.bottom, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.background)
            .listRowInsets(EdgeInsets())
    }
}

/// The sidebar's groups, as the phone shows them.
/// What the server found for a search, while it's what's typed.
struct Found {
    var query = ""
    var hits: [SearchHit] = []
    var searching = false
    var problem: String?
}

/// A thread a search found by its messages: its title and when its best message was said, then
/// who said it (or who Flotilla brought it from) and the line around what matched (a command in
/// monospace, the match in bold).
struct HitRow: View {
    let hit: SearchHit

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "bubble.left").font(.caption).foregroundStyle(Palette.dim).frame(width: 14)
                Text(hit.title.isEmpty ? "New thread" : hit.title)
                    .font(Typeface.body)
                    .foregroundStyle(Palette.text)
                    .lineLimit(1)
                Spacer(minLength: 8)
                if let at = hit.at { Text(Format.ago(at)).font(.footnote).foregroundStyle(Palette.dim) }
            }
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                (Text("\(who): ").foregroundStyle(Palette.dim) + Text(snippet).font(hit.kind == .tool ? Typeface.mono(.footnote) : Typeface.meta).foregroundStyle(Palette.text))
                    .font(Typeface.meta)
                    .lineLimit(2)
                Spacer(minLength: 0)
                // There's no find in a thread yet: how many of its messages match.
                if hit.hits > 1 { Text("\(hit.hits) matches").font(.footnote).foregroundStyle(Palette.dim).fixedSize() }
            }
            .padding(.leading, 22)
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    /// Who said it, or who Flotilla brought it from.
    private var who: String {
        if let from = hit.from { return from }
        return switch hit.kind {
        case .prompt, .steer: "You"
        case .text: "Reply"
        case .tool: "Command"
        case .title: "Title"
        }
    }

    private var snippet: AttributedString {
        var s = AttributedString(hit.snippet)
        for r in hit.markedRanges {
            guard let lo = AttributedString.Index(r.lowerBound, within: s), let hi = AttributedString.Index(r.upperBound, within: s) else { continue }
            s[lo..<hi].inlinePresentationIntent = .stronglyEmphasized
            s[lo..<hi].backgroundColor = Palette.lavender.opacity(0.3)
        }
        return s
    }
}

struct Groups {
    var unread: [ThreadSummary] = []
    var working: [ThreadSummary] = []
    var idle: [ThreadSummary] = []
    var archived: [ThreadSummary] = []

    var isEmpty: Bool { unread.isEmpty && working.isEmpty && idle.isEmpty && archived.isEmpty }

    /// `hiding`: threads being deleted.
    init(_ s: Sidebar?, hiding: Set<String> = []) {
        guard let s else { return }
        let present = Set(s.threads.map(\.id)).subtracting(hiding)
        let waiting = Set(s.waiting)
        for row in s.threads where !hiding.contains(row.id) {
            // A thread handed its task by another lives in that one's card.
            if let parent = s.parents[row.id], present.contains(parent) { continue }
            switch row.activity {
            case .unread: unread.append(row)
            case .working: working.append(row)
            case .idle where waiting.contains(row.id): working.append(row)
            case .idle: idle.append(row)
            case .archived, .expired: archived.append(row)
            }
        }
        working.sort { ($0.turnStartedMs ?? $0.lastActivityMs) > ($1.turnStartedMs ?? $1.lastActivityMs) }
    }
}

/// A thread's row: title and time; its latest reply (or what you said, while it works); then
/// its agent and model, and its VM.
struct ThreadRow: View {
    let row: ThreadSummary
    let catalog: Catalog?
    let waiting: Bool
    let repo: String?
    /// Why a search found it when its title doesn't show it ("PR: #42 …"), in place of its preview.
    var hint: String? = nil
    @Environment(\.dynamicTypeSize) private var textSize

    var body: some View {
        let unread = row.activity == .unread
        let working = row.activity == .working
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 8) {
                if unread { Pip(color: Palette.ring) } else if working { Pip(color: Palette.ok) }
                Text(row.thread.title.isEmpty ? "New thread" : row.thread.title)
                    .font(Typeface.rowTitle)
                    .foregroundStyle(row.activity == .expired ? Palette.dim : Palette.text)
                    // Large text: room for more than its first words.
                    .lineLimit(textSize.isAccessibilitySize ? 3 : 1)
                Spacer(minLength: 6)
                if working, let start = row.turnStartedMs {
                    TimelineView(.periodic(from: .now, by: 1)) { _ in
                        Text(Format.elapsed(since: start)).monospacedDigit().foregroundStyle(Palette.ok)
                    }
                    .font(.footnote)
                } else {
                    Text(Format.ago(row.lastActivityMs)).font(.footnote).foregroundStyle(Palette.dim)
                }
            }
            if let preview {
                Text(preview)
                    .font(.subheadline)
                    .foregroundStyle(unread ? Palette.text : Palette.dim)
                    .lineLimit(2)
            }
            HStack(spacing: 5) {
                ProviderLogo(provider: row.thread.provider, size: 13)
                    .opacity(row.activity == .expired ? 0.5 : 1)
                if let repo {
                    // Its project, and its model: threads started together on several models
                    // differ only by that.
                    Text(repo).lineLimit(1)
                    Text("·").opacity(0.5)
                    Text(Format.modelName(provider: row.thread.provider, model: row.thread.model, catalog: catalog))
                        .lineLimit(1)
                        .layoutPriority(1)
                } else {
                    Text(Format.model(row.thread, catalog: catalog)).lineLimit(1)
                }
                if let pr = row.pr {
                    Text("#\(pr.number)").foregroundStyle(pr.state == "MERGED" ? Palette.link : pr.state == "CLOSED" ? Palette.bad : Palette.ok)
                        .fixedSize()
                }
                if let (words, color) = vm {
                    Text("·").opacity(0.5)
                    Text(words).foregroundStyle(color).lineLimit(1)
                }
            }
            .font(.caption)
            .foregroundStyle(Palette.dim)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spoken)
        .accessibilityIdentifier("thread-\(row.id)")
    }

    /// What it last said, or what you said while it works.
    private var preview: String? {
        if let hint { return hint }
        if row.activity == .working, let p = row.lastPrompt {
            if let members = ThreadPlan.parseReport(p) {
                return members.count == 1 ? "From \(members[0].title)" : "From \(members.count) agents"
            }
            if let a = ThreadPlan.parseAnswer(p) { return "From \(a.title): \(a.answer)" }
            if let c = ThreadPlan.parseCouldntStart(p) { return "Couldn't start \(c.title): \(c.why)" }
            if ThreadPlan.isUnreadable(p) { return ThreadPlan.unreadableLine }
            if let task = ThreadPlan.taskOf(p) { return "From another thread: \(task)" }
            return "You: \(p)"
        }
        return row.lastReply?.nilIfBlank
    }

    /// The VM, in words, when it says something.
    private var vm: (String, Color)? {
        let t = row.thread
        if row.startFailed { return ("not started", Palette.bad) }
        if row.activity == .working { return waiting ? ("waiting on a hand-off", Palette.dim) : ("running", Palette.dim) }
        switch t.state {
        case .stopped:
            if let d = Format.daysLeft(t), d <= 3 { return ("\(max(d, 0))d left", Palette.warn) }
            return ("stopped", Palette.dim)
        case .error: return (t.sandboxID == nil ? "not started" : "error", Palette.bad)
        case .expired: return ("expired", Palette.dim)
        case .creating, .starting: return ("starting", Palette.warn)
        case .stopping: return ("stopping", Palette.dim)
        case .idle, .running:
            if waiting { return ("waiting on a hand-off", Palette.dim) }
            if t.keepRunning { return ("kept running", Palette.dim) }
            return nil
        }
    }

    private var spoken: String {
        var parts: [String] = []
        switch row.activity {
        case .unread: parts.append("Unread")
        case .working: parts.append("Working")
        default: break
        }
        parts.append(row.thread.title)
        if row.activity == .working, let s = row.turnStartedMs { parts.append("for \(Format.elapsed(since: s))") } else { parts.append(Format.agoSpoken(row.lastActivityMs)) }
        if let preview { parts.append(preview) }
        if let repo { parts.append(repo) }
        parts.append("\(Format.providerName(row.thread.provider)), \(Format.model(row.thread, catalog: catalog))")
        if let (w, _) = vm { parts.append(w) }
        return parts.joined(separator: ". ")
    }
}

