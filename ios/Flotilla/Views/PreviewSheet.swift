import FlotillaKit
import SafariServices
import SwiftUI
import UIKit

/// Preview, as the desktop's thread header has it (`thread_view.rs`): the globe beside ⋯ opens
/// a port in the thread's VM (`thread.preview`, waking a stopped VM first) in an in-app Safari
/// view, so Done comes back to the thread. With services (`.agents/services.yml`) on a VM that's
/// up, or a link anyone may open, it opens their sheet instead: each service with its state, Open
/// and its other links, Output, Restart / Stop / Start and Make public…; each public link with
/// how long it's public, Copy link and Make private; the services file's problem; and Another
/// port…. While a link is public the globe is an open lock.
struct PreviewButton: View {
    @Environment(AppModel.self) private var model
    let id: String
    @State private var showServices = false
    @State private var askingPort = false
    @State private var port = PreviewButton.defaultPort
    /// Picked in the services sheet, done once it has gone.
    @State private var then: PreviewPick?
    /// The port Make public… asks how long for.
    @State private var publishing: PublicAsk?
    @State private var refused: String?

    static let defaultPort = "3000"

    /// Whether a thread's header has it: not once its VM is gone.
    static func shows(_ thread: FlotillaKit.Thread?) -> Bool {
        thread.map { !$0.vmGone } ?? false
    }

    var body: some View {
        let isPublic = !model.publicLinks(id).isEmpty
        Button {
            if model.currentServices(id) != nil || isPublic { showServices = true } else { askPort() }
        } label: {
            Image(systemName: isPublic ? "lock.open" : "globe")
        }
        .accessibilityLabel(isPublic ? "Preview, public" : "Preview")
        .accessibilityIdentifier("preview")
        .sheet(isPresented: $showServices, onDismiss: picked) {
            ServicesSheet(id: id) { pick in
                then = pick
                showServices = false
            }
        }
        .alert("Open a preview", isPresented: $askingPort) {
            TextField(Self.defaultPort, text: $port)
                .keyboardType(.numberPad)
            Button("Cancel", role: .cancel) {}
            Button("Make public…") {
                guard let p = previewPort(port) else { return }
                // Once the alert has gone: a dialog asked for while it goes never shows.
                Task {
                    try? await Task.sleep(for: .milliseconds(450))
                    publishing = PublicAsk(port: p, what: "port \(p)")
                }
            }
                .disabled(previewPort(port) == nil)
            Button("Open") { open() }
                .disabled(previewPort(port) == nil)
        } message: {
            Text("Port in the VM")
        }
        .publicChoices($publishing) { ask, duration in
            let (model, id) = (model, id)
            Task {
                if let why = await model.makePublic(id, port: ask.port, for: duration) { refused = "Couldn't make \(ask.what) public: \(why)" }
            }
        }
        .alert(refused ?? "", isPresented: Binding(get: { refused != nil }, set: { if !$0 { refused = nil } })) {
            Button("OK", role: .cancel) {}
        }
    }

    private func askPort() {
        port = Self.defaultPort
        askingPort = true
    }

    private func picked() {
        let pick = then
        then = nil
        switch pick {
        case .open(let url): InAppBrowser.open(url)
        case .port(let p): open(p)
        case .anotherPort: askPort()
        case nil: break
        }
    }

    private func open() {
        guard let p = previewPort(port) else { return }
        open(p)
    }

    private func open(_ p: UInt16) {
        let (model, id) = (model, id)
        Task {
            if let url = await model.preview(id, port: p) { InAppBrowser.open(url) }
        }
    }
}

/// A port Make public… asks about, and what it's called ("web", "port 3000").
struct PublicAsk: Identifiable, Equatable {
    let port: UInt16
    let what: String
    var id: UInt16 { port }
}

extension View {
    /// Make public…'s choices, said plainly as public to anyone with the link: 1 hour, 1 day,
    /// 1 week or Until I stop it.
    func publicChoices(_ ask: Binding<PublicAsk?>, pick: @escaping (PublicAsk, PublicFor) -> Void) -> some View {
        let shown = Binding(get: { ask.wrappedValue != nil }, set: { if !$0 { ask.wrappedValue = nil } })
        return confirmationDialog(ask.wrappedValue.map { "Make \($0.what) public" } ?? "", isPresented: shown, titleVisibility: .visible, presenting: ask.wrappedValue) { a in
            ForEach(PublicFor.allCases, id: \.self) { d in
                Button(d.label) { pick(a, d) }
                    .accessibilityIdentifier("public-\(d.rawValue)")
            }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Anyone with the link can open it, without signing in.")
        }
    }
}

extension AppModel {
    /// The services Preview's sheet shows for a thread, if any (`thread_view::current_services`):
    /// those on the VM it has now, while it's up.
    func currentServices(_ id: String) -> ThreadServices? {
        ThreadServices.current(details[id]?.services, thread: details[id]?.thread ?? summary(id)?.thread)
    }
}

/// What the services sheet was closed for: a link to open, a port to open (waking the VM if
/// it's stopped), or another port.
enum PreviewPick {
    case open(URL)
    case port(UInt16)
    case anotherPort
}

/// A preview asked for, until its URL comes, or Make public… until it's done: the port, whether
/// the VM wakes first, and for how long it's made public.
struct PreviewAsk: Equatable {
    let id = UUID()
    let port: UInt16
    let waking: Bool
    var making: PublicFor? = nil

    /// What shows while it comes, as the desktop says it.
    var words: String {
        switch (making, waking) {
        case (nil, true): "Resuming the VM, then opening port \(port)…"
        case (nil, false): "Opening port \(port)…"
        case (_?, true): "Resuming the VM, then making port \(port) public…"
        case (_?, false): "Making port \(port) public…"
        }
    }
}

/// What the top of a thread says once a link of its went public.
struct PublicNote: Equatable {
    let id = UUID()
    let words: String
}

/// While a preview's URL comes, or a link goes public: what's happening, at the top of the
/// thread; then, for a moment, that it's public and copied.
struct PreviewNotice: View {
    @Environment(AppModel.self) private var model
    let id: String

    var body: some View {
        let ask = model.previewing[id]
        let note = model.publicNotes[id]
        ZStack {
            if ask != nil || note != nil {
                HStack(spacing: 8) {
                    if ask != nil {
                        ProgressView().controlSize(.mini)
                    } else {
                        Image(systemName: "lock.open").font(.caption.weight(.semibold)).foregroundStyle(Palette.warn)
                    }
                    Text(ask?.words ?? note?.words ?? "").font(Typeface.label).foregroundStyle(Palette.text).lineLimit(2)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 9)
                .background(Capsule().fill(Palette.card))
                .overlay(Capsule().stroke(Palette.border, lineWidth: 1))
                .shadow(color: Palette.plum.opacity(0.12), radius: 10, y: 3)
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("previewNotice")
                .transition(.opacity.combined(with: .move(edge: .top)))
            }
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
        .animation(.easeOut(duration: 0.2), value: ask)
        .animation(.easeOut(duration: 0.2), value: note)
    }
}

/// Preview's sheet of a thread's services and public links: a row each, the file's problem,
/// other ports made public, Another port…. It closes once there's nothing left to show (the VM
/// stops, or a new VM runs none of them, and nothing is public), as the desktop's menu does.
struct ServicesSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let id: String
    let pick: (PreviewPick) -> Void
    @State private var path: [String] = []
    /// Services (by name) and ports (as "port <n>") with a command on its way.
    @State private var busy: Set<String> = []
    @State private var refused: String?
    /// The port Make public… asks how long for.
    @State private var publishing: PublicAsk?
    /// How tall the list is: the sheet opens to just that.
    @State private var height: CGFloat = 420

    var body: some View {
        let services = model.currentServices(id)
        let links = model.publicLinks(id)
        let runs = services?.services ?? []
        let others = links.filter { l in !runs.contains { $0.port == l.port } }
        NavigationStack(path: $path) {
            List {
                if let error = services?.error {
                    Section {
                        Label {
                            Text(error).foregroundStyle(Palette.bad)
                        } icon: {
                            Image(systemName: "xmark.circle").foregroundStyle(Palette.bad)
                        }
                        .font(Typeface.label)
                        .accessibilityElement(children: .combine)
                        .accessibilityIdentifier("servicesProblem")
                    }
                    .listRowBackground(Palette.card)
                }
                if !runs.isEmpty {
                    Section {
                        ForEach(runs) { run in
                            ServiceRow(
                                thread: id, run: run, link: links.first { $0.port == run.port }, busy: busy.contains(run.name),
                                open: { pick(.open($0)) }, output: { path.append(run.name) },
                                makePublic: { publishing = PublicAsk(port: UInt16(clamping: run.port), what: run.name) },
                                makePrivate: { makePrivate(UInt16(clamping: run.port), key: run.name, what: run.name) }
                            ) { request, verb in
                                send(request, verb: verb, name: run.name)
                            }
                        }
                    }
                    .listRowBackground(Palette.card)
                }
                if !others.isEmpty {
                    Section {
                        ForEach(others) { link in
                            let key = "port \(link.port)"
                            PublicPortRow(link: link, busy: busy.contains(key), open: { pick(.port(UInt16(clamping: link.port))) }) {
                                makePrivate(UInt16(clamping: link.port), key: key, what: key)
                            }
                        }
                    }
                    .listRowBackground(Palette.card)
                }
                Section {
                    Button {
                        pick(.anotherPort)
                    } label: {
                        Label("Another port…", systemImage: "globe").foregroundStyle(Palette.text)
                    }
                    .accessibilityIdentifier("previewOtherPort")
                }
                .listRowBackground(Palette.card)
            }
            .scrollContentBackground(.hidden)
            .contentMargins(.top, 4, for: .scrollContent)
            .listSectionSpacing(16)
            .onScrollGeometryChange(for: CGFloat.self) { g in
                g.contentSize.height + g.contentInsets.top + g.contentInsets.bottom
            } action: { _, h in
                height = h
            }
            .background(Palette.background)
            .navigationTitle("Preview")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
            .navigationDestination(for: String.self) { name in
                ServiceOutput(id: id, name: name, busy: busy.contains(name)) { request, verb in
                    send(request, verb: verb, name: name)
                }
            }
        }
        .presentationDetents([.height(height), .large])
        .presentationDragIndicator(.visible)
        .presentationBackground(Palette.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("servicesSheet")
        .publicChoices($publishing) { ask, duration in
            busy.insert(ask.what)
            Task {
                let why = await model.makePublic(id, port: ask.port, for: duration)
                busy.remove(ask.what)
                if let why { refused = "Couldn't make \(ask.what) public: \(why)" }
            }
        }
        // Over the sheet, as the alert under it would say it.
        .alert(refused ?? "", isPresented: Binding(get: { refused != nil }, set: { if !$0 { refused = nil } })) {
            Button("OK", role: .cancel) {}
        }
        .onChange(of: services == nil && links.isEmpty, initial: true) { _, gone in
            if gone { dismiss() }
        }
    }

    private func send(_ request: Request, verb: String, name: String) {
        busy.insert(name)
        Task {
            let why = await model.service(request)
            busy.remove(name)
            if let why { refused = "Couldn't \(verb) \(name): \(why)" }
        }
    }

    private func makePrivate(_ port: UInt16, key: String, what: String) {
        busy.insert(key)
        Task {
            let why = await model.makePrivate(id, port: port)
            busy.remove(key)
            if let why { refused = "Couldn't make \(what) private: \(why)" }
        }
    }
}

/// While a link is public: how long for (kept up to date), Copy link and Make private.
private struct PublicLine: View {
    let key: String
    let link: PublicLink
    let busy: Bool
    let makePrivate: () -> Void
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TimelineView(.periodic(from: .now, by: 30)) { context in
                Label(link.words(nowMs: Int64(context.date.timeIntervalSince1970 * 1000)), systemImage: "lock.open")
                    .font(Typeface.meta)
                    .foregroundStyle(Palette.warn)
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(link.words(nowMs: Int64(context.date.timeIntervalSince1970 * 1000)))
            }
            .accessibilityIdentifier("public-\(key)")
            FlowRow(spacing: 8) {
                // Its URL isn't known until boat has answered (boat down as it was made public).
                if !link.url.isEmpty {
                    Button {
                        UIPasteboard.general.string = link.url
                        copied = true
                        Task {
                            try? await Task.sleep(for: .seconds(1.5))
                            copied = false
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                            Text(copied ? "Copied" : "Copy link")
                        }
                    }
                    .accessibilityIdentifier("public-copy-\(key)")
                }
                Button(action: makePrivate) {
                    HStack(spacing: 4) {
                        Image(systemName: "lock")
                        Text("Make private")
                    }
                }
                .accessibilityIdentifier("public-end-\(key)")
            }
            .buttonStyle(.bordered)
            .tint(Palette.text)
            .controlSize(.small)
            .font(.footnote.weight(.medium))
            .disabled(busy)
        }
    }
}

/// A port made public that no service runs on: Open (waking the VM if it's stopped), then how
/// long it's public, Copy link and Make private.
private struct PublicPortRow: View {
    let link: PublicLink
    let busy: Bool
    let open: () -> Void
    let makePrivate: () -> Void

    var body: some View {
        let key = "port-\(link.port)"
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "globe").font(.caption).foregroundStyle(Palette.dim)
                Text(verbatim: "port \(link.port)").font(Typeface.mono(.subheadline)).foregroundStyle(Palette.text)
                Spacer(minLength: 0)
            }
            Button(action: open) {
                HStack(spacing: 4) {
                    Image(systemName: "arrow.up.right")
                    Text("Open")
                }
            }
            .buttonStyle(.borderedProminent)
            .tint(Palette.primary)
            .foregroundStyle(Palette.onPrimary)
            .controlSize(.small)
            .font(.footnote.weight(.medium))
            .accessibilityIdentifier("public-open-\(key)")
            .padding(.leading, 15)
            PublicLine(key: key, link: link, busy: busy, makePrivate: makePrivate)
                .padding(.leading, 15)
        }
        .padding(.vertical, 4)
    }
}

/// One service: its pip, name and state; then Open, its other links, Output, Restart (Start
/// once you've stopped it), Stop and Make public…; while it's public, how long for, Copy link
/// and Make private.
private struct ServiceRow: View {
    let thread: String
    let run: ServiceRun
    let link: PublicLink?
    let busy: Bool
    let open: (URL) -> Void
    let output: () -> Void
    let makePublic: () -> Void
    let makePrivate: () -> Void
    let send: (Request, String) -> Void

    var body: some View {
        let links = run.openLinks.compactMap { l in URL(string: l.url).map { (l.title, $0) } }
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Pip(color: ServiceRow.pip(run))
                    .alignmentGuide(.firstTextBaseline) { $0[.bottom] - 1 }
                Text(run.name).font(Typeface.mono(.subheadline)).foregroundStyle(Palette.text)
                Text(run.words).font(Typeface.meta).foregroundStyle(run.problem ? Palette.bad : Palette.dim).lineLimit(2)
                Spacer(minLength: 0)
            }
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("service-\(run.name)")
            FlowRow(spacing: 8) {
                if let first = links.first {
                    Button {
                        open(first.1)
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "arrow.up.right")
                            Text("Open")
                        }
                    }
                        .buttonStyle(.borderedProminent)
                        .tint(Palette.primary)
                        .foregroundStyle(Palette.onPrimary)
                        .disabled(!run.live)
                        .accessibilityIdentifier("service-open-\(run.name)")
                }
                ForEach(Array(links.dropFirst().enumerated()), id: \.offset) { i, link in
                    Button(link.0) { open(link.1) }
                        .disabled(!run.live)
                        .accessibilityIdentifier("service-link-\(run.name)-\(i)")
                }
                Button("Output", action: output)
                    .accessibilityIdentifier("service-output-\(run.name)")
                if run.state == "stopped" {
                    Button("Start") { send(.restartService(thread, name: run.name), "start") }
                        .accessibilityIdentifier("service-start-\(run.name)")
                } else {
                    Button("Restart") { send(.restartService(thread, name: run.name), "restart") }
                        .accessibilityIdentifier("service-restart-\(run.name)")
                }
                if run.live {
                    Button("Stop") { send(.stopService(thread, name: run.name), "stop") }
                        .accessibilityIdentifier("service-stop-\(run.name)")
                }
                if !links.isEmpty && link == nil {
                    Button("Make public…", action: makePublic)
                        .accessibilityIdentifier("service-public-\(run.name)")
                }
            }
            .buttonStyle(.bordered)
            .tint(Palette.text)
            .controlSize(.small)
            .font(.footnote.weight(.medium))
            .disabled(busy)
            .padding(.leading, 15)
            if let link {
                PublicLine(key: run.name, link: link, busy: busy, makePrivate: makePrivate)
                    .padding(.leading, 15)
            }
        }
        .padding(.vertical, 4)
    }

    /// The desktop's colours: ready green, starting amber, stopped grey, anything else red.
    static func pip(_ run: ServiceRun) -> Color {
        switch run.state {
        case "ready": Palette.ok
        case "starting": Palette.warn
        case "stopped": Palette.dim.opacity(0.6)
        default: Palette.bad
        }
    }
}

/// A service's output, newest at the bottom, with Restart (Start once you've stopped it).
private struct ServiceOutput: View {
    @Environment(AppModel.self) private var model
    let id: String
    let name: String
    let busy: Bool
    let send: (Request, String) -> Void

    var body: some View {
        let run = model.currentServices(id)?.services.first { $0.name == name }
        let lines = run?.tail() ?? []
        ScrollView {
            Group {
                if lines.isEmpty {
                    Text("Nothing printed yet.").font(Typeface.body).foregroundStyle(Palette.dim)
                } else {
                    Text(lines.joined(separator: "\n"))
                        .font(Typeface.mono(.caption))
                        .foregroundStyle(Palette.text)
                        .textSelection(.enabled)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(14)
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Palette.muted))
            .padding(16)
            .accessibilityIdentifier("serviceOutput")
        }
        // Opened at the newest; a short output sits at the top.
        .defaultScrollAnchor(.top, for: .alignment)
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .defaultScrollAnchor(.bottom, for: .sizeChanges)
        .background(Palette.background)
        .navigationTitle("\(name) · output")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                let stopped = run?.state == "stopped"
                Button(stopped ? "Start" : "Restart") { send(.restartService(id, name: name), stopped ? "start" : "restart") }
                    .disabled(busy || run == nil)
                    .accessibilityIdentifier("service-output-restart")
            }
        }
    }
}

/// Buttons in rows that wrap, as many to a row as fit.
struct FlowRow: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = arrange(subviews, width: proposal.width ?? .infinity)
        let width = rows.map { $0.width }.max() ?? 0
        let height = rows.map(\.height).reduce(0, +) + spacing * CGFloat(max(rows.count - 1, 0))
        return CGSize(width: proposal.width ?? width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in arrange(subviews, width: bounds.width) {
            var x = bounds.minX
            for (i, size) in zip(row.items, row.sizes) {
                subviews[i].place(at: CGPoint(x: x, y: y + (row.height - size.height) / 2), proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + spacing
        }
    }

    private struct Row {
        var items: [Int] = []
        var sizes: [CGSize] = []
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> [Row] {
        var rows: [Row] = []
        var row = Row()
        for (i, view) in subviews.enumerated() {
            var size = view.sizeThatFits(.unspecified)
            size.width = min(size.width, width)
            if !row.items.isEmpty, row.width + spacing + size.width > width {
                rows.append(row)
                row = Row()
            }
            row.width += (row.items.isEmpty ? 0 : spacing) + size.width
            row.height = max(row.height, size.height)
            row.items.append(i)
            row.sizes.append(size)
        }
        if !row.items.isEmpty { rows.append(row) }
        return rows
    }
}

/// Opens a page in an in-app Safari view over whatever shows, so Done comes back to it. Anything
/// but a web page goes to the system.
enum InAppBrowser {
    static func open(_ url: URL) {
        guard let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https" else {
            UIApplication.shared.open(url)
            return
        }
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let scene = scenes.first { $0.activationState == .foregroundActive } ?? scenes.first
        guard var top = scene?.keyWindow?.rootViewController else {
            UIApplication.shared.open(url)
            return
        }
        while let next = top.presentedViewController, !next.isBeingDismissed { top = next }
        let safari = SFSafariViewController(url: url)
        safari.preferredControlTintColor = UIColor(Palette.link)
        safari.dismissButtonStyle = .done
        top.present(safari, animated: true)
    }
}
