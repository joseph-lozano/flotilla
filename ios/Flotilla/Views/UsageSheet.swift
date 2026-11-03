import FlotillaKit
import SwiftUI

/// Where boat shows the plan, prices and billing.
let billingURL = URL(string: "https://boat.dev/dashboard/billing")!

/// The usage sheet's tabs: boat's VMs, or the agents (what they spent and their limits).
enum UsageTab: Hashable {
    case vms, agents
}

/// The account's usage in full, from the info button beside the counts on Threads (the
/// desktop's usage popover): every count with a meter, why boat won't start a VM, when the
/// numbers were read, Refresh, and boat.dev for billing. Its Agents tab (`AgentsSection`), once
/// they have something to show, is what the agents spent and their limits; the agent limits
/// beside the counts open on it.
struct UsageSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dynamicTypeSize) private var textSize
    @State private var refreshing = false
    @State private var failed: String?
    /// How tall what it shows is: the sheet opens to just that.
    @State private var height: CGFloat = 400
    @State private var tab: UsageTab

    init(tab: UsageTab = .vms) {
        _tab = State(initialValue: tab)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .firstTextBaseline) {
                        title
                        Spacer()
                        read
                    }
                    VStack(alignment: .leading, spacing: 4) {
                        title
                        read
                    }
                }
                if usage != nil && agentsAny {
                    Picker("Usage of", selection: $tab) {
                        Text("VMs").tag(UsageTab.vms)
                        Text("Agents").tag(UsageTab.agents)
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("usageTabs")
                }
                if shownTab == .agents {
                    AgentsSection()
                } else {
                    vms
                }
            }
            .padding(.horizontal, 22)
            .padding(.top, 26)
            .padding(.bottom, 12)
            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height = $0 }
        }
        .scrollBounceBehavior(.basedOnSize)
        .background(Palette.background)
        // Just as tall as what it shows (large text: the whole screen).
        .presentationDetents(textSize.isAccessibilitySize ? [.large] : [.height(height), .large])
        .presentationDragIndicator(.visible)
        .presentationBackground(Palette.background)
        // A container, so what's in it keeps its own identifiers.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("usageSheet")
        // Open: what the agents spent comes, and the VMs are checked (at most every 5 minutes).
        .onAppear { model.watchAgentUsage(true) }
        .onDisappear { model.watchAgentUsage(false) }
    }

    /// The tab shown: boat's when the agents have nothing to show, the agents' when there's no
    /// count from boat.
    private var shownTab: UsageTab {
        guard agentsAny else { return .vms }
        return usage == nil ? .agents : tab
    }

    /// The agents have something to show: use in the last 30 days, or a weekly limit.
    private var agentsAny: Bool {
        let now = Format.nowMs()
        if (model.sidebar?.limits ?? []).contains(where: { !$0.windows(now: now).isEmpty }) { return true }
        guard let u = model.agentUsage else { return false }
        return Spend.summarize(u, today: AgentsSection.day(Date()), dayOf: { AgentsSection.day(Date(timeIntervalSince1970: Double($0) / 1000)) }).any
    }

    /// boat's counts, Refresh and billing.
    @ViewBuilder private var vms: some View {
        VStack(alignment: .leading, spacing: 18) {
            if let why = usage?.blocked {
                Label("Can't start VMs: \(why)", systemImage: "exclamationmark.triangle.fill")
                    .font(Typeface.label)
                    .foregroundStyle(Palette.bad)
            }
            if let u = usage {
                VStack(alignment: .leading, spacing: 12) { rows(u) }
            } else {
                Text("boat's limits haven't been read yet").font(Typeface.body).foregroundStyle(Palette.dim)
            }
            if let failed {
                Text("Couldn't refresh: \(failed)").font(Typeface.label).foregroundStyle(Palette.bad)
            } else if let why = cantRefresh, !refreshing {
                Text(why).font(Typeface.label).foregroundStyle(Palette.dim).accessibilityIdentifier("usageCantRefresh")
            }
            ViewThatFits(in: .horizontal) {
                HStack {
                    refresh
                    Spacer()
                    billing
                }
                VStack(alignment: .leading, spacing: 12) {
                    refresh
                    billing
                }
            }
            .font(Typeface.body.weight(.medium))
            .tint(Palette.link)
            .padding(.top, 12)
            .overlay(alignment: .top) { Rectangle().fill(Palette.border).frame(height: 1) }
        }
    }

    private var title: some View {
        Text("Usage").font(Typeface.title).foregroundStyle(Palette.text)
    }

    @ViewBuilder private var read: some View {
        if let ms = shownTab == .vms ? usage?.readMs : agentsRead {
            TimelineView(.periodic(from: .now, by: 30)) { _ in
                Text("Read \(Format.agoPhrase(ms))")
                    .accessibilityLabel("Read \(Format.agoSpoken(ms))")
            }
            .font(Typeface.label)
            .foregroundStyle(Palette.dim)
        }
    }

    private var refresh: some View {
        Button {
            Task {
                refreshing = true
                failed = await model.refreshUsage()
                refreshing = false
            }
        } label: {
            HStack(spacing: 6) {
                if refreshing { ProgressView().controlSize(.small) } else { Image(systemName: "arrow.clockwise") }
                Text(refreshing ? "Refreshing…" : "Refresh")
            }
        }
        .disabled(cantRefresh != nil || refreshing)
        .accessibilityIdentifier("usageRefresh")
    }

    private var billing: some View {
        Link(destination: billingURL) {
            HStack(spacing: 4) {
                Text("Billing on boat.dev")
                Image(systemName: "arrow.up.right")
            }
        }
        .accessibilityHint("Opens boat.dev")
    }

    private var usage: Usage? { model.sidebar?.usage }

    private var agentsRead: Int64? {
        model.agentUsage?.readMs ?? (model.sidebar?.limits ?? []).map(\.readMs).max()
    }

    /// Why boat can't be read now: not connected, or the server isn't running its engine.
    private var cantRefresh: String? {
        if model.connection != .online { return "Can't refresh: not connected to the server" }
        if model.sidebar?.engineRunning != true { return "Can't refresh: the server isn't connected to boat" }
        return nil
    }

    @ViewBuilder private func rows(_ u: Usage) -> some View {
        if let v = u.vms { UsageRow(label: "VMs running", count: v) }
        let windows = [(u.minute, "Starts, last minute"), (u.hour, "Starts, last hour"), (u.day, "Starts, last 24 hours")].compactMap { w, label in w.map { ($0, label) } }
        ForEach(Array(windows.enumerated()), id: \.offset) { i, w in
            UsageRow(label: w.1, count: w.0, note: i == windows.count - 1 ? startsFree(u, windows.map(\.0)) : nil)
        }
        if let h = Format.vmHoursUsed(u) {
            // Used against the plan, as every row here is: the meter fills as they go (as on the Mac).
            UsageRow(label: "VM hours used", count: h,
                     note: Format.date(u.renews).map { ("Renews \($0)", .plain) })
        }
        if let x = u.extraStarts { ExtraStartsRow(x: x) }
    }

    /// When a full start window frees up, if boat said (until then).
    private func startsFree(_ u: Usage, _ windows: [Count]) -> (String, Format.Tone)? {
        guard windows.contains(where: { $0.limit > 0 && $0.used >= $0.limit }), let ms = Format.ms(u.startsFree), ms > Format.nowMs(),
              let at = Format.clock(u.startsFree) else { return nil }
        return ("VMs can start again at \(at)", .bad)
    }
}

/// A label, its count against the limit (in its tone when near or at it), ten soft pixel
/// cells, and a line under them when there's more to say. `count` drives the meter and tone;
/// `words` say it instead, when it isn't a count (extra starts: "On · 2 used").
private struct UsageRow: View {
    @Environment(\.dynamicTypeSize) private var textSize
    let label: String
    var count: Count?
    var words: String?
    var tone: Format.Tone?
    var note: (String, Format.Tone)?

    var body: some View {
        let tone = tone ?? count.map(Format.tone) ?? .plain
        let shown = Text(words ?? count.map { "\($0.used) of \($0.limit)" } ?? "")
            .monospacedDigit()
            .foregroundStyle(color(tone, plain: Palette.text))
        VStack(alignment: .leading, spacing: 3) {
            if textSize.isAccessibilitySize {
                // Large text: the label over its value and meter, each read whole.
                Text(label).foregroundStyle(Palette.dim).font(Typeface.body)
                HStack(spacing: 10) {
                    shown
                    if let count { Meter(count: count, tone: tone) }
                }
                .font(Typeface.body)
            } else {
                HStack(spacing: 10) {
                    Text(label).foregroundStyle(Palette.dim)
                    Spacer(minLength: 8)
                    shown
                    if let count { Meter(count: count, tone: tone) } else { Color.clear.frame(width: Meter.width, height: 1) }
                }
                .font(Typeface.body)
            }
            if let (text, tone) = note {
                Text(text).font(Typeface.label).foregroundStyle(color(tone, plain: Palette.dim))
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(spoken(tone))
    }

    private func spoken(_ tone: Format.Tone) -> String {
        var parts = [words ?? count.map { "\($0.used) of \($0.limit)" } ?? ""]
        switch tone {
        case .bad: parts.append("at the limit")
        case .warn: parts.append("near the limit")
        default: break
        }
        if let (text, _) = note { parts.append(text) }
        return parts.joined(separator: ", ")
    }
}

/// Extra starts: on or off, and what's been used and spent this month against the cap
/// (shown whether they're on or off now).
private struct ExtraStartsRow: View {
    let x: ExtraStarts

    var body: some View {
        let spend = x.spendCents.flatMap { x.on || $0.used > 0 || x.capReached ? $0 : nil }
        let state = x.on ? "On" : "Off"
        let note: (String, Format.Tone)? = switch (spend, x.capReached) {
        case (let s?, true): ("Cap reached: \(Format.dollars(s.used)) of \(Format.dollars(s.limit)) this month", .bad)
        case (nil, true): ("Cap reached this month", .bad)
        case (let s?, false): ("\(Format.dollars(s.used)) of \(Format.dollars(s.limit)) this month", .plain)
        case (nil, false): nil
        }
        UsageRow(label: "Extra starts", count: spend, words: x.on || x.used > 0 ? "\(state) · \(x.used) used" : state,
                 tone: x.capReached ? .bad : spend.map(Format.tone) ?? .plain, note: note)
    }
}

/// Ten soft pixel cells: lit in the count's tone (quiet until it's near the limit).
private struct Meter: View {
    let count: Count
    let tone: Format.Tone
    static let width: CGFloat = 10 * 6 + 9 * 2

    var body: some View {
        let lit = Format.litCells(count)
        HStack(spacing: 2) {
            ForEach(0..<10, id: \.self) { i in Pip(color: i < lit ? color(tone, plain: Palette.dim) : Palette.border, size: 6) }
        }
        .accessibilityHidden(true)
    }
}

private func color(_ tone: Format.Tone, plain: Color) -> Color {
    switch tone {
    case .bad: Palette.bad
    case .warn: Palette.warn
    default: plain
    }
}
