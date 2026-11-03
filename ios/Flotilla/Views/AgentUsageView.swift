import FlotillaKit
import SwiftUI

/// The usage sheet's Agents tab, as the desktop's (`meter.rs`) and T3 Code's Usage: each
/// account's weekly limits; then the last 30 days' estimated cost at API prices (the tokens
/// alone while no cost is known), what the cache saved, the cost by kind and speed, a trend by
/// day, and each model (a tap shows its trend and its rate); Refresh, and each provider's own
/// usage page. Nothing shows for a number that isn't known.
struct AgentsSection: View {
    @Environment(AppModel.self) private var model
    @State private var picked: Pick?
    @State private var refreshing = false
    @State private var failed: String?

    struct Pick: Hashable {
        var provider: String
        var model: String
    }

    var body: some View {
        TimelineView(.periodic(from: .now, by: 30)) { tick in
            let now = Int64(tick.date.timeIntervalSince1970 * 1000)
            VStack(alignment: .leading, spacing: 24) {
                let accounts = (model.sidebar?.limits ?? []).filter { !$0.windows(now: now).isEmpty }
                if !accounts.isEmpty { limits(accounts, now: now) }
                let all = summary(nil)
                if all.any { spent(all) }
                VStack(alignment: .leading, spacing: 18) {
                    if let failed {
                        Text("Couldn't refresh: \(failed)").font(Typeface.label).foregroundStyle(Palette.bad)
                    } else if let why = cantRefresh, !refreshing {
                        Text(why).font(Typeface.label).foregroundStyle(Palette.dim)
                    }
                    footer
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("agentsUsage")
    }

    private func summary(_ only: Pick?) -> Spend.Summary {
        guard let u = model.agentUsage else { return Spend.Summary() }
        return Spend.summarize(u, today: Self.day(Date()), dayOf: { Self.day(Date(timeIntervalSince1970: Double($0) / 1000)) }, only: only.map { ($0.provider, $0.model) })
    }

    /// A day as this phone counts them.
    static func day(_ date: Date) -> Int64 { Int64(Calendar.current.ordinality(of: .day, in: .era, for: date) ?? 0) }

    @ViewBuilder private func limits(_ accounts: [AccountLimits], now: Int64) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Limits").font(Typeface.body.weight(.semibold)).foregroundStyle(Palette.text)
            ForEach(accounts, id: \.account) { l in
                VStack(alignment: .leading, spacing: 10) {
                    Text(l.plan.map { "\(l.name) · \($0.prefix(1).uppercased())\($0.dropFirst())" } ?? l.name)
                        .font(Typeface.label)
                        .foregroundStyle(Palette.dim)
                    ForEach(l.windows(now: now), id: \.id) { w in WindowRow(window: w, now: now) }
                }
            }
        }
    }

    @ViewBuilder private func spent(_ all: Spend.Summary) -> some View {
        let cost = all.cost
        VStack(alignment: .leading, spacing: 12) {
            Text("Last 30 days").font(Typeface.body.weight(.semibold)).foregroundStyle(Palette.text)
            VStack(alignment: .leading, spacing: 4) {
                if let cost {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(Spend.usd(cost.total)).font(.title2.weight(.semibold)).monospacedDigit().foregroundStyle(Palette.text)
                        Text("API estimate").font(Typeface.label).foregroundStyle(Palette.dim)
                    }
                    let line = ["\(Spend.tokens(all.tokens.total)) tokens", cost.saved > 0 ? "\(Spend.usd(cost.saved)) saved by the cache" : nil].compactMap { $0 }
                    Text(line.joined(separator: " · ")).font(Typeface.label).foregroundStyle(Palette.dim)
                } else {
                    Text("\(Spend.tokens(all.tokens.total)) tokens").font(.title2.weight(.semibold)).monospacedDigit().foregroundStyle(Palette.text)
                }
            }
            .accessibilityElement(children: .combine)
            if let cost, cost.total > 0 { CostByKind(cost: cost, fast: all.fast) }
            trend(all)
            VStack(spacing: 2) {
                ForEach(all.models, id: \.self) { m in modelRow(m) }
            }
        }
    }

    /// The days' bars: the model picked, or all of them.
    @ViewBuilder private func trend(_ all: Spend.Summary) -> some View {
        let pick = picked.flatMap { p in all.models.contains { $0.provider == p.provider && $0.model == p.model } ? p : nil }
        let shown = pick.map { summary($0) } ?? all
        let byCost = shown.cost != nil
        let values = shown.days.map { byCost ? $0.cost : Double($0.tokens) }
        let top = values.max() ?? 0
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .bottom, spacing: 2) {
                ForEach(Array(values.enumerated()), id: \.offset) { _, v in
                    let share = top > 0 ? v / top : 0
                    RoundedRectangle(cornerRadius: 1)
                        .fill(share > 0 ? Palette.text.opacity(0.45) : Palette.border)
                        .frame(maxWidth: .infinity)
                        .frame(height: share > 0 ? max(36 * share, 2) : 1)
                }
            }
            .frame(height: 36, alignment: .bottom)
            .accessibilityHidden(true)
            HStack {
                Text(pick.map { "\($0.model) by day" } ?? "By day")
                Spacer()
                Text("\(firstDay) – today")
            }
            .font(Typeface.meta)
            .foregroundStyle(Palette.dim)
        }
    }

    private var firstDay: String {
        let d = Calendar.current.date(byAdding: .day, value: -(Spend.days - 1), to: Date()) ?? Date()
        return d.formatted(.dateTime.month(.abbreviated).day())
    }

    /// A model: its cost (none without a price) and tokens. A tap shows its trend and rate.
    @ViewBuilder private func modelRow(_ m: Spend.ModelSum) -> some View {
        let key = Pick(provider: m.provider, model: m.model)
        let on = picked == key
        VStack(alignment: .leading, spacing: 2) {
            Button {
                picked = on ? nil : key
            } label: {
                HStack(spacing: 8) {
                    ProviderLogo(provider: m.provider, size: 13)
                    Text(m.model).lineLimit(1).truncationMode(.middle).foregroundStyle(Palette.text)
                    Spacer(minLength: 8)
                    if let c = m.cost { Text(Spend.usd(c.total)).monospacedDigit().foregroundStyle(Palette.text) }
                    Text(Spend.tokens(m.tokens.total)).monospacedDigit().foregroundStyle(Palette.dim).frame(minWidth: 44, alignment: .trailing)
                }
                .font(Typeface.body)
                .padding(.horizontal, 6)
                .padding(.vertical, 7)
                .background(on ? Palette.selection : .clear, in: RoundedRectangle(cornerRadius: 8))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(on ? .isSelected : [])
            .accessibilityHint("Shows its trend by day")
            if on {
                let detail = [m.perMillion.map { "\(Spend.usd($0)) per 1M tokens" }, Spend.cacheHit(m.tokens).map { "\(Limits.percent($0 * 100)) cache hit" }].compactMap { $0 }
                if !detail.isEmpty {
                    Text(detail.joined(separator: " · ")).font(Typeface.label).foregroundStyle(Palette.dim).padding(.leading, 27)
                }
            }
        }
    }

    /// Refresh, and each provider's own usage page for the accounts seen here: in a row, or
    /// stacked where large type leaves no room for it.
    private var footer: some View {
        let accounts = Set((model.sidebar?.limits ?? []).map(\.account) + (model.agentUsage?.hours ?? []).compactMap { Spend.accountOf($0.provider) })
        let pages = accounts.sorted().compactMap(Spend.usagePage)
        return ViewThatFits(in: .horizontal) {
            HStack(spacing: 16) {
                refreshButton
                Spacer()
                ForEach(pages, id: \.site) { page in pageLink(page) }
            }
            VStack(alignment: .leading, spacing: 10) {
                refreshButton
                ForEach(pages, id: \.site) { page in pageLink(page) }
            }
        }
        .font(Typeface.body.weight(.medium))
        .tint(Palette.link)
        .padding(.top, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .top) { Rectangle().fill(Palette.border).frame(height: 1) }
    }

    private var refreshButton: some View {
        Button {
            Task {
                refreshing = true
                failed = await model.refreshAgentUsage()
                refreshing = false
            }
        } label: {
            HStack(spacing: 6) {
                if refreshing { ProgressView().controlSize(.small) } else { Image(systemName: "arrow.clockwise") }
                Text(refreshing ? "Refreshing…" : "Refresh")
            }
            .fixedSize()
        }
        .disabled(cantRefresh != nil || refreshing)
        .accessibilityIdentifier("agentsRefresh")
    }

    private func pageLink(_ page: (url: URL, site: String)) -> some View {
        Link(destination: page.url) {
            HStack(spacing: 4) {
                Text(page.site)
                Image(systemName: "arrow.up.right")
            }
            .fixedSize()
        }
        .accessibilityHint("Opens \(page.site)")
    }

    /// Why the VMs can't be checked now: not connected, or the server isn't running its engine.
    private var cantRefresh: String? {
        if model.connection != .online { return "Can't refresh: not connected to the server" }
        if model.sidebar?.engineRunning != true { return "Can't refresh: the server isn't connected to boat" }
        return nil
    }
}

/// The cost by kind of token, as one bar and its legend; and what Fast cost and added.
private struct CostByKind: View {
    let cost: UsageCost
    let fast: Double

    var body: some View {
        let kinds = cost.kinds.filter { $0.value / cost.total >= 0.0005 }
        VStack(alignment: .leading, spacing: 6) {
            GeometryReader { g in
                HStack(spacing: 1) {
                    ForEach(kinds, id: \.label) { k in
                        Rectangle().fill(Palette.text.opacity(k.ink)).frame(width: max(0, (g.size.width - CGFloat(kinds.count - 1)) * k.value / cost.total))
                    }
                }
            }
            .frame(height: 6)
            .clipShape(RoundedRectangle(cornerRadius: 3))
            .accessibilityHidden(true)
            LazyVGrid(columns: [GridItem(.flexible(), alignment: .leading), GridItem(.flexible(), alignment: .leading)], alignment: .leading, spacing: 4) {
                ForEach(kinds, id: \.label) { k in
                    HStack(spacing: 5) {
                        RoundedRectangle(cornerRadius: 1).fill(Palette.text.opacity(k.ink)).frame(width: 7, height: 7)
                        Text(k.label).foregroundStyle(Palette.dim)
                        Text(Spend.usd(k.value)).monospacedDigit().foregroundStyle(Palette.text)
                    }
                    .accessibilityElement(children: .combine)
                }
            }
            .font(Typeface.label)
            if fast > 0 {
                Text("Fast \(Spend.usd(fast))\(cost.premium.map { " · Premium \(Spend.usd($0))" } ?? "")").font(Typeface.label).foregroundStyle(Palette.dim)
            }
        }
    }
}
