import FlotillaKit
import SwiftUI

/// New thread, as the mockup has it: the prompt, with the agent's model, effort and fast mode
/// under it, then the project and branch. Start opens the thread at once. With several models
/// chosen, one thread starts on each.
struct NewThreadSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let start: NewThreadStart
    /// In Threads' place, with no thread yet: no Cancel, and Start goes in this bar (Threads'
    /// own, with the usage).
    var bar: ((AnyView) -> AnyView)? = nil
    /// The model chosen (effort and fast are its), and others chosen beside it, in order.
    @State private var choice: ThreadSettings?
    @State private var also: [ModelKey] = []
    /// Why Start won't start these now.
    @State private var refusal: String?
    @State private var project: String?
    @State private var branch: OnBranch?
    @State private var pickingBranch = false
    @State private var pickingModel = false
    @State private var ready = false
    @FocusState private var typing: Bool
    @Environment(\.dynamicTypeSize) private var textSize
    /// Send later's sheet: the prompt to a new thread each run, on each model chosen.
    @State private var later: ScheduleDraft?

    var body: some View {
        if bar == nil {
            NavigationStack {
                form.toolbar {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                    // Large text: Send later and Start in the bar at the top, clear of the page.
                    if textSize.isAccessibilitySize {
                        ToolbarItem(placement: .primaryAction) {
                            Button(action: sendLater) { Image(systemName: "clock") }
                                .disabled(choice == nil)
                                .accessibilityLabel("Send later")
                                .accessibilityIdentifier("startLater")
                        }
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Start", action: startNow)
                                .disabled(empty || choice == nil)
                                .accessibilityIdentifier("startThread")
                        }
                    }
                }
            }
        } else {
            form
        }
    }

    /// What's written (kept with the model, so it outlives the sheet).
    private var prompt: Binding<String> {
        Binding(get: { model.drafts[""] ?? "" }, set: { model.drafts[""] = $0 })
    }

    private var form: some View {
        ScrollView {
            VStack(spacing: 0) {
                PixelMark(cell: 4).padding(.top, 18)
                Text("New thread").font(Typeface.body).foregroundStyle(Palette.dim).padding(.top, 12).padding(.bottom, 18)
                VStack(alignment: .leading, spacing: 0) {
                    TextField("What should the agent do?", text: prompt, axis: .vertical)
                        .font(.body)
                        .foregroundStyle(Palette.text)
                        // Large text: fewer empty lines, so the project and branch fit above the keyboard.
                        .lineLimit(textSize.isAccessibilitySize ? 2...8 : 4...12)
                        .focused($typing)
                        .padding(.horizontal, 14)
                        .padding(.top, 14)
                        .padding(.bottom, 8)
                        .accessibilityIdentifier("newPrompt")
                    PendingFiles(key: "").padding(.horizontal, 12).padding(.bottom, 6)
                    chips.padding(.horizontal, 8).padding(.bottom, 10)
                }
                .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(Palette.card))
                .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).stroke(Palette.border, lineWidth: 1))
                where_.padding(.horizontal, 10)
                if let why = refusal ?? start.problem {
                    Text(why).font(Typeface.label).foregroundStyle(Palette.bad).frame(maxWidth: .infinity, alignment: .leading).padding(.top, 10).padding(.horizontal, 4)
                        .accessibilityIdentifier("startProblem")
                }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 80)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(Palette.background)
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if let bar {
                bar(AnyView(HStack(spacing: 4) {
                    laterButton
                    startButton
                }))
            } else if !textSize.isAccessibilitySize {
                HStack {
                    Spacer()
                    laterButton
                    startButton
                }
                .padding(.horizontal, 12)
                .padding(.top, 6)
                .padding(.bottom, 8)
                // Opaque: the page stops above Start, never showing under it.
                .background(Palette.background)
            }
        }
        .sheet(item: $later) { draft in
            // Kept: the prompt goes (unless it's been changed since), and New thread with it.
            ScheduleSheet(draft: draft) { sent in
                guard (model.drafts[""] ?? "").trimmingCharacters(in: .whitespacesAndNewlines) == sent else { return }
                model.drafts[""] = nil
                dismiss()
            }
        }
        .sheet(isPresented: $pickingBranch) {
            if let p = chosen { BranchPicker(project: p, chosen: $branch) }
        }
        .sheet(isPresented: $pickingModel) {
            if let catalog = model.catalog {
                ModelPicker(catalog: catalog, chosen: keys) { p, m in
                    pick(provider: p, model: m, catalog: catalog)
                    also = []
                } pickSeveral: { pickSeveral($0, catalog: catalog) }
            }
        }
        .onAppear(perform: setUp)
        .onChange(of: model.catalog) { _, _ in setUp() }
        // Something else chosen: a refusal said before may not hold.
        .onChange(of: keys) { _, _ in refusal = nil }
        .onChange(of: branch) { _, _ in refusal = nil }
        .onChange(of: project) { _, _ in refusal = nil }
    }

    /// Nothing written yet.
    private var empty: Bool { prompt.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    /// Start one thread per model chosen, unless they can't start now (and why).
    private func startNow() {
        guard let catalog = model.catalog, let choice else { return }
        let models = models(catalog, choice)
        if let why = FanOut.cantStart(models.count, onBranch: chosen == nil ? nil : branch, room: model.sidebar?.usage?.startRoom) {
            refusal = why
            return
        }
        model.start(prompt: prompt.wrappedValue, models: models, project: chosen, onBranch: branch)
    }

    /// Send later: the prompt to a new thread each run, on each model chosen.
    private var laterButton: some View {
        Button(action: sendLater) {
            Image(systemName: "clock")
                .font(.system(size: 17))
                .foregroundStyle(Palette.dim)
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(choice == nil)
        .accessibilityLabel("Send later")
        .accessibilityIdentifier("startLater")
    }

    /// Send later's form, for what Start would start.
    private func sendLater() {
        guard let catalog = model.catalog, let choice else { return }
        guard (model.pending[""] ?? []).isEmpty else {
            refusal = "Send later takes no files."
            return
        }
        guard let environment = chosen?.environmentName.nilIfBlank ?? (model.sidebar?.environments.first { $0.isDefault } ?? model.sidebar?.environments.first)?.name else {
            refusal = "No boat environment. Make one in boat's dashboard."
            return
        }
        let picked = models(catalog, choice)
        let targets = picked.map { ScheduleTarget.new(FreshThread(environment: environment, projectID: chosen?.id, provider: $0.provider, model: $0.model, effort: $0.effort, fast: $0.fast, onBranch: chosen == nil ? nil : branch)) }
        let names = picked.map { catalog.modelLabel(provider: $0.provider, model: $0.model) ?? $0.model }.joined(separator: " + ")
        typing = false
        later = ScheduleDraft(prompt: prompt.wrappedValue, targets: targets, to: (["New thread", names] + [chosen?.repo].compactMap { $0 }).joined(separator: " · "))
    }

    private var startButton: some View {
        Button(action: startNow) {
            HStack(spacing: 7) {
                Text("Start")
                Image(systemName: "arrow.up").font(.system(size: 14, weight: .bold))
            }
            .font(.callout.weight(.semibold))
            .foregroundStyle(empty ? Palette.dim : Palette.onPrimary)
            .padding(.horizontal, 18)
            // Taller with the text (in Threads' bar, the first screen, it stays at large sizes).
            .frame(minHeight: 44)
            .background(Capsule().fill(empty ? Palette.muted : Palette.primary))
        }
        .buttonStyle(.plain)
        .disabled(empty || choice == nil)
        .accessibilityIdentifier("startThread")
    }

    /// Every model chosen, as `ModelPicker` shows them.
    private var keys: [ModelKey] {
        (choice.map { [ModelKey(provider: $0.provider, model: $0.model)] } ?? []) + also
    }

    /// The threads' settings: the model chosen as chosen, then each other one with an effort
    /// and fast that suit it (`Catalog.fit`).
    private func models(_ catalog: Catalog, _ c: ThreadSettings) -> [ThreadSettings] {
        [c] + also.compactMap { catalog.fit(provider: $0.provider, model: $0.model, effort: c.effort, fast: c.fast) }
    }

    private var chosen: Project? { project.flatMap { id in model.sidebar?.projects.first { $0.id == id } } }

    /// Your defaults (else Claude Code's), the project asked for (else none), its branch.
    private func setUp() {
        guard !ready, let catalog = model.catalog else { return }
        ready = true
        let d = (start.settings ?? model.settings?.defaults).flatMap { catalog.validate(provider: $0.provider, model: $0.model, effort: $0.effort, fast: $0.fast) }
        choice = d.flatMap { Catalog.offered($0.provider) ? $0 : nil } ?? catalog.defaultSettings
        also = start.also.map { ModelKey(provider: $0.provider, model: $0.model) }.filter { k in
            Catalog.offered(k.provider) && catalog.validate(provider: k.provider, model: k.model, effort: nil, fast: false) != nil && k != keys.first
        }
        project = start.project.flatMap { id in model.sidebar?.projects.contains { $0.id == id } == true ? id : nil }
        branch = project == nil ? nil : start.branch
        if let project { model.loadBranches(project) }
        // A sheet opened to write in; the first screen waits for a tap.
        if bar == nil { typing = true }
    }

    // MARK: The agent

    /// Attach, then the model, effort and fast; the model on a line of its own when they don't
    /// fit on one (several models' names).
    private var chips: some View {
        HStack(alignment: .top, spacing: 6) {
            AttachButton(key: "")
            if let catalog = model.catalog, let c = choice {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 6) {
                        modelMenu(catalog, c)
                        options(catalog, c)
                        Spacer(minLength: 0)
                    }
                    VStack(alignment: .leading, spacing: 6) {
                        modelMenu(catalog, c)
                        HStack(spacing: 6) {
                            options(catalog, c)
                            Spacer(minLength: 0)
                        }
                    }
                }
            }
            Spacer(minLength: 0)
        }
    }

    /// Effort and fast, for the (first) model chosen.
    @ViewBuilder private func options(_ catalog: Catalog, _ c: ThreadSettings) -> some View {
        if let m = catalog.provider(c.provider)?.models.first(where: { $0.id == c.model }) {
            if let effort = m.effort {
                Menu {
                    ForEach(effort.supported, id: \.self) { level in
                        Button {
                            choice?.effort = level
                        } label: {
                            if level == c.effort { Label(level.capitalized, systemImage: "checkmark") } else { Text(level.capitalized) }
                        }
                    }
                } label: {
                    chip { Text((c.effort ?? "effort").capitalized) }
                }
                .accessibilityLabel("Effort, \(c.effort ?? "none")")
                .accessibilityIdentifier("effortChip")
            }
            if m.fastMode {
                Button {
                    choice?.fast.toggle()
                } label: {
                    chip(on: c.fast) {
                        Image(systemName: "bolt").font(.caption.weight(.semibold))
                        Text("Fast").foregroundStyle(c.fast ? Palette.text : Palette.dim)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityLabel(c.fast ? "Fast mode on" : "Fast mode off")
                .accessibilityIdentifier("fastChip")
            }
        }
    }

    /// The model, or every model chosen: "Opus 5.5 + GPT-6.1 Sol".
    private func modelMenu(_ catalog: Catalog, _ c: ThreadSettings) -> some View {
        let names = keys.map { Format.modelName(provider: $0.provider, model: $0.model, catalog: catalog) }
        let agents = keys.map(\.provider).reduce(into: [String]()) { if !$0.contains($1) { $0.append($1) } }
        return Button {
            pickingModel = true
        } label: {
            chip {
                HStack(spacing: 2) {
                    ForEach(agents, id: \.self) { ProviderLogo(provider: $0, size: 14) }
                }
                Text(names.joined(separator: " + ")).foregroundStyle(Palette.text)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(keys.count > 1 ? "Models, \(names.formatted(.list(type: .and)))" : "Model, \(Format.providerName(c.provider)) \(names.first ?? c.model)")
        .accessibilityIdentifier("modelChip")
    }

    /// Several models from the picker: the first is the one effort and fast are for.
    private func pickSeveral(_ picked: [ModelKey], catalog: Catalog) {
        guard let first = picked.first else { return }
        if first != keys.first, let m = catalog.provider(first.provider)?.models.first(where: { $0.id == first.model }) {
            pick(provider: first.provider, model: m, catalog: catalog)
        }
        also = Array(picked.dropFirst())
    }

    /// A new model keeps the effort if it takes it (else its default), and fast only if it has it.
    private func pick(provider: String, model m: Catalog.Model, catalog: Catalog) {
        let effort: String? = switch m.effort {
        case let o? where choice?.effort.map(o.supported.contains) == true: choice?.effort
        case let o?: o.defaultLevel ?? o.supported.first
        case nil: nil
        }
        choice = catalog.validate(provider: provider, model: m.id, effort: effort, fast: m.fastMode && choice?.fast == true)
    }

    private func chip(on: Bool = false, @ViewBuilder _ content: () -> some View) -> some View {
        HStack(spacing: 5) { content() }
            .font(Typeface.label)
            .foregroundStyle(Palette.dim)
            .lineLimit(1)
            .padding(.horizontal, 10)
            .frame(height: 30)
            .background(Capsule().fill(on ? Palette.selection : Palette.muted))
            .overlay(Capsule().stroke(on ? Palette.ring : .clear, lineWidth: 1))
    }

    // MARK: Where

    /// The project and its branch, joined under the box.
    private var where_: some View {
        HStack(spacing: 4) {
            Menu {
                ForEach(model.sidebar?.projects ?? [], id: \.id) { p in
                    Button {
                        setProject(p.id)
                    } label: {
                        if p.id == project { Label(p.repo, systemImage: "checkmark") } else { Text(p.repo) }
                    }
                }
                Divider()
                Button {
                    setProject(nil)
                } label: {
                    if project == nil { Label("No project", systemImage: "checkmark") } else { Text("No project") }
                }
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "shippingbox").font(.caption)
                    Text(chosen?.repo ?? "No project").fontWeight(chosen == nil ? .regular : .medium).foregroundStyle(chosen == nil ? Palette.dim : Palette.text)
                }
                .padding(.horizontal, 8)
                .frame(height: 30)
                .contentShape(Rectangle())
            }
            .accessibilityLabel("Project, \(chosen?.repo ?? "No project")")
            .accessibilityIdentifier("projectChip")
            if chosen != nil {
                Button {
                    pickingBranch = true
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: branch == nil ? "arrow.triangle.branch" : "arrow.triangle.pull").font(.caption)
                        Text(branch?.name ?? "New branch").font(branch == nil ? Typeface.label : Typeface.mono(.footnote)).foregroundStyle(branch == nil ? Palette.dim : Palette.text).lineLimit(1).truncationMode(.middle)
                    }
                    .padding(.horizontal, 8)
                    .frame(height: 30)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Branch, \(branch?.name ?? "a new branch")")
                .accessibilityIdentifier("branchChip")
            }
            Spacer(minLength: 0)
        }
        .font(Typeface.label)
        .foregroundStyle(Palette.dim)
        .padding(.horizontal, 4)
        .background(UnevenRoundedRectangle(bottomLeadingRadius: 12, bottomTrailingRadius: 12, style: .continuous).fill(Palette.muted))
        .overlay(UnevenRoundedRectangle(bottomLeadingRadius: 12, bottomTrailingRadius: 12, style: .continuous).stroke(Palette.border, lineWidth: 1))
    }

    /// Another project: back to a new branch, and its lists fetched.
    private func setProject(_ id: String?) {
        guard id != project else { return }
        project = id
        branch = nil
        if let id { model.loadBranches(id) }
    }
}

/// A project's branch to start on: a new one, an open pull request's, or another on GitHub.
struct BranchPicker: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let project: Project
    @Binding var chosen: OnBranch?
    @State private var query = ""

    var body: some View {
        let lists = model.branches[project.id]
        let rows = BranchChoice.rows(project, lists: lists, inUse: inUse, chosen: chosen).filter { $0.matches(query, owner: project.owner, repo: project.repo) }
        NavigationStack {
            List(rows) { row in
                Button {
                    chosen = BranchChoice.onBranch(project, lists: lists, value: row.value)
                    dismiss()
                } label: {
                    line(row)
                }
                .disabled(row.disabled)
                .listRowBackground(Palette.background)
                .accessibilityIdentifier("branch-\(row.value.isEmpty ? "new" : row.value)")
                if row.value == "#error" {
                    Button("Try again") { model.loadBranches(project.id) }
                        .font(Typeface.body.weight(.medium))
                        .tint(Palette.link)
                        .listRowBackground(Palette.background)
                        .accessibilityIdentifier("branchesTryAgain")
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(Palette.background)
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search, #12 or a PR link")
            .navigationTitle(project.repo)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
        .presentationDetents([.medium, .large])
    }

    /// The branches other threads in the project are on.
    private var inUse: Set<String> {
        Set((model.sidebar?.threads ?? []).filter { $0.thread.repoProjectID == project.id && $0.thread.state != .expired }.compactMap(\.thread.branch))
    }

    private func line(_ row: BranchChoice) -> some View {
        let picked = (chosen?.name ?? "") == row.value
        let (icon, main, detail): (String, String, String?) = switch row.kind {
        case .new(let from): ("plus", "New branch", "from \(from)")
        case .pr(let pr): ("arrow.triangle.pull", "#\(pr.number) \(pr.title)", "\(pr.head) → \(pr.base)\(pr.draft ? " · draft" : "")\(pr.fromFork ? " · from a fork" : "")")
        case .branch(let b): ("arrow.triangle.branch", b, nil)
        case .note(let n): ("info.circle", n, nil)
        }
        let more = [detail, row.inUse ? "another thread is on it" : nil].compactMap { $0 }.joined(separator: " · ")
        return HStack(spacing: 10) {
            Image(systemName: icon).font(.footnote).foregroundStyle(Palette.dim).frame(width: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text(main)
                    // A branch's name is data; the rest is words.
                    .font({ if case .branch = row.kind { Typeface.mono(.subheadline) } else { Typeface.body } }())
                    .foregroundStyle(row.disabled ? Palette.dim : Palette.text)
                    .lineLimit(1)
                if !more.isEmpty { Text(more).font(Typeface.meta).foregroundStyle(Palette.dim).lineLimit(1) }
            }
            Spacer(minLength: 4)
            if picked { Image(systemName: "checkmark").font(.footnote.weight(.semibold)).foregroundStyle(Palette.link) }
        }
        .contentShape(Rectangle())
    }
}

/// A model, by agent.
struct ModelKey: Hashable {
    var provider: String
    var model: String
}

/// The agent and model: every model of the agents Flotilla offers, by agent, searchable. A tap
/// picks one; Select picks several (one thread starts on each), in the order tapped.
struct ModelPicker: View {
    @Environment(\.dismiss) private var dismiss
    let catalog: Catalog
    /// The models chosen now (the first is the one effort and fast are for).
    let chosen: [ModelKey]
    let pick: (String, Catalog.Model) -> Void
    let pickSeveral: ([ModelKey]) -> Void
    @State private var query = ""
    @State private var selecting: Bool
    @State private var selected: [ModelKey]

    init(catalog: Catalog, chosen: [ModelKey], pick: @escaping (String, Catalog.Model) -> Void, pickSeveral: @escaping ([ModelKey]) -> Void) {
        self.catalog = catalog
        self.chosen = chosen
        self.pick = pick
        self.pickSeveral = pickSeveral
        // Several chosen already: open as they were picked.
        _selecting = State(initialValue: chosen.count > 1)
        _selected = State(initialValue: chosen)
    }

    var body: some View {
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        let providers = catalog.providers.filter { Catalog.offered($0.key) }
        NavigationStack {
            ScrollViewReader { proxy in
            List {
                ForEach(providers, id: \.key) { p in
                    let models = p.models.filter { m in
                        q.isEmpty || [m.label ?? "", m.id, Format.providerName(p.key)].contains { $0.lowercased().contains(q) }
                    }
                    if !models.isEmpty {
                        Section {
                            ForEach(models, id: \.id) { m in row(p.key, m) }
                        } header: {
                            Text(Format.providerName(p.key)).font(.caption.weight(.semibold)).textCase(.uppercase).kerning(0.6).foregroundStyle(Palette.dim)
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(Palette.background)
            // Open at the model chosen now.
            .onAppear { if let c = chosen.first { proxy.scrollTo("\(c.provider)/\(c.model)", anchor: .center) } }
            }
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search models")
            // Choosing several: Done and the count stay up while searching.
            .searchPresentationToolbarBehavior(selecting ? .avoidHidingContent : .automatic)
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                if selecting {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Done") {
                            pickSeveral(selected)
                            dismiss()
                        }
                        .disabled(selected.isEmpty)
                        .accessibilityIdentifier("modelsDone")
                    }
                } else {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Select") {
                            selected = chosen
                            selecting = true
                        }
                        .accessibilityIdentifier("selectModels")
                    }
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private var title: String {
        guard selecting else { return "Model" }
        return switch selected.count {
        case 0: "Select models"
        case 1: "1 model"
        case let n: "\(n) models"
        }
    }

    private func row(_ provider: String, _ m: Catalog.Model) -> some View {
        let key = ModelKey(provider: provider, model: m.id)
        let on = selecting ? selected.contains(key) : chosen.contains(key)
        return Button {
            if selecting {
                if let i = selected.firstIndex(of: key) { selected.remove(at: i) } else { selected.append(key) }
            } else {
                pick(provider, m)
                dismiss()
            }
        } label: {
            HStack(spacing: 10) {
                if selecting {
                    Image(systemName: on ? "checkmark.circle.fill" : "circle")
                        .font(.title3)
                        .foregroundStyle(on ? Palette.link : Palette.dim.opacity(0.5))
                }
                ProviderLogo(provider: provider, size: 16)
                let label = m.label ?? Catalog.readable(m.id)
                Text(label).font(Typeface.body).foregroundStyle(Palette.text)
                if label != m.id { Text(m.id).font(Typeface.mono(.caption)).foregroundStyle(Palette.dim).lineLimit(1) }
                Spacer(minLength: 4)
                if !selecting && on {
                    Image(systemName: "checkmark").font(.footnote.weight(.semibold)).foregroundStyle(Palette.link)
                }
            }
            .contentShape(Rectangle())
        }
        .listRowBackground(Palette.card)
        .id("\(provider)/\(m.id)")
        .accessibilityAddTraits(on ? .isSelected : [])
        .accessibilityIdentifier("model-\(provider)/\(m.id)")
    }
}
