import FlotillaKit
import SwiftUI

/// What the agents' own records say about their use, as the desktop shows it (`meter.rs`):
/// a thread's context as a ring by Send, its numbers in full on a tap; the agent accounts'
/// limits as a gauge beside boat's counts on Threads, opening the usage sheet's Agents tab
/// (`AgentsSection`). Nothing shows for a number the harness hasn't given.

private func color(_ tone: Limits.Tone, plain: Color) -> Color {
    switch tone {
    case .bad: Palette.bad
    case .warn: Palette.warn
    case .plain: plain
    }
}

/// The thread's context: a ring filling clockwise from the top (the count alone before the
/// window is known). A tap shows the numbers in full.
struct ContextRing: View {
    let context: ContextUse
    @State private var open = false

    var body: some View {
        let tone = context.percent.map(Limits.contextTone) ?? .plain
        Button {
            open = true
        } label: {
            Group {
                if let p = context.percent {
                    ZStack {
                        Circle().stroke(Palette.border, lineWidth: 3)
                        Circle()
                            .trim(from: 0, to: p / 100)
                            .stroke(color(tone, plain: Palette.dim), style: StrokeStyle(lineWidth: 3, lineCap: .round))
                            .rotationEffect(.degrees(-90))
                    }
                    .frame(width: 18, height: 18)
                } else {
                    Text(Limits.tokens(context.used)).font(Typeface.meta).monospacedDigit().foregroundStyle(Palette.dim)
                }
            }
            .frame(minWidth: 34, minHeight: 34)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Context window")
        .accessibilityValue(spoken)
        .accessibilityIdentifier("contextMeter")
        .popover(isPresented: $open) {
            ContextBubble(context: context).presentationCompactAdaptation(.popover)
        }
    }

    private var spoken: String {
        guard let p = context.percent, let w = context.window else { return "\(Limits.exact(context.used)) tokens" }
        return "\(Limits.percent(p)) used, \(Limits.exact(context.used)) of \(Limits.exact(w)) tokens"
    }
}

/// "Context window  2.4% · 24k / 1m", then "23,767 of 1,000,000 tokens".
private struct ContextBubble: View {
    let context: ContextUse

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 16) {
                Text("Context window").fontWeight(.semibold).foregroundStyle(Palette.text)
                Spacer(minLength: 0)
                Text(head).foregroundStyle(Palette.dim).monospacedDigit()
            }
            Text(full).foregroundStyle(Palette.dim).monospacedDigit()
        }
        .font(Typeface.label)
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .fixedSize()
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("contextBubble")
    }

    private var head: String {
        guard let p = context.percent, let w = context.window else { return Limits.tokens(context.used) }
        return "\(Limits.percent(p)) · \(Limits.tokens(context.used)) / \(Limits.tokens(w))"
    }

    private var full: String {
        guard let w = context.window else { return "\(Limits.exact(context.used)) tokens" }
        return "\(Limits.exact(context.used)) of \(Limits.exact(w)) tokens"
    }
}

/// "Claude limit reached · resets 10:00 AM": the thread's account is used up, so a turn would
/// fail until the window resets.
struct LimitReached: View {
    let limits: AccountLimits
    let window: LimitWindow

    var body: some View {
        let when = LimitsFormat.resetAt(window).map { " · resets \($0)" } ?? ""
        Text("\(limits.name) limit reached\(when)")
            .font(Typeface.label)
            .foregroundStyle(Palette.bad)
            .accessibilityIdentifier("limitReached")
    }
}

/// The composer's "limit reached" line, while the thread's account is used up: it goes by
/// itself once the window resets.
struct ReachedLine: View {
    let limits: [AccountLimits]
    let context: ContextUse?

    var body: some View {
        // Nothing to watch unless a window was used up when it was read.
        if Limits.reached(limits, context: context, now: 0) != nil {
            TimelineView(.periodic(from: .now, by: 30)) { tick in
                if let (l, w) = Limits.reached(limits, context: context, now: ms(tick.date)) { LimitReached(limits: l, window: w) }
            }
        }
    }
}

/// A moment as ms since the epoch.
private func ms(_ date: Date) -> Int64 { Int64(date.timeIntervalSince1970 * 1000) }

enum LimitsFormat {
    /// When a limit resets: its time within a day ("10:00 AM"), else its day too ("Wed 2:09 PM").
    static func resetAt(_ w: LimitWindow, now: Int64 = Format.nowMs()) -> String? {
        guard let ms = w.resetsMs else { return nil }
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        return ms - now < 86_400_000
            ? date.formatted(date: .omitted, time: .shortened)
            : date.formatted(.dateTime.weekday(.abbreviated).hour().minute())
    }
}

/// Beside boat's counts on Threads: the agent window closest to its limit, as its account's
/// logo and its share in its tone. It opens the agent limits.
struct LimitsButton: View {
    let limits: [AccountLimits]
    let open: () -> Void

    var body: some View {
        // Looked at again every 30 s: a window that resets goes.
        TimelineView(.periodic(from: .now, by: 30)) { tick in
            if let (l, w) = Limits.tightest(limits, now: ms(tick.date)) {
                let tone = Limits.limitTone(w.used)
                Button(action: open) {
                    // The share, then the logo alone where large type leaves no room for it.
                    ViewThatFits(in: .horizontal) {
                        label(l, w, tone: tone, share: true)
                        label(l, w, tone: tone, share: false)
                    }
                    .font(.footnote)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Agent limits, \(l.name) \(w.label.lowercased()) \(w.used)% used")
                .accessibilityHint("Shows the agents' usage")
                .accessibilityIdentifier("agentLimits")
            }
        }
    }

    private func label(_ l: AccountLimits, _ w: LimitWindow, tone: Limits.Tone, share: Bool) -> some View {
        HStack(spacing: 5) {
            // The account's logo, not a gauge: boat's counts beside it have one.
            if let logo = Limits.logo(l.account) { ProviderLogo(provider: logo) }
            if share {
                Text("\(w.used)%").fontWeight(.medium).monospacedDigit().foregroundStyle(color(tone, plain: Palette.text)).fixedSize()
            }
        }
    }
}

/// "Weekly  92% used  [ten cells]", and under it when it resets.
struct WindowRow: View {
    let window: LimitWindow
    let now: Int64

    var body: some View {
        let tone = Limits.limitTone(window.used)
        let lit = Limits.lit(Double(window.used), cells: 10)
        let used = Text("\(window.used)% used").monospacedDigit().foregroundStyle(color(tone, plain: Palette.text))
        let meter = HStack(spacing: 2) {
            ForEach(0..<10, id: \.self) { i in Pip(color: i < lit ? color(tone, plain: Palette.dim) : Palette.border, size: 6) }
        }
        .accessibilityHidden(true)
        VStack(alignment: .leading, spacing: 3) {
            // In a row; stacked where large type leaves no room for it.
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 10) {
                    Text(window.label).foregroundStyle(Palette.dim).fixedSize()
                    Spacer(minLength: 8)
                    used.fixedSize()
                    meter
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text(window.label).foregroundStyle(Palette.dim)
                    HStack(spacing: 10) { used; meter }
                }
            }
            .font(Typeface.body)
            if let resets {
                Text(resets).font(Typeface.label).foregroundStyle(window.reached ? Palette.bad : Palette.dim)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(window.label)
        .accessibilityValue(["\(window.used)% used", resets].compactMap { $0 }.joined(separator: ", "))
    }

    private var resets: String? {
        guard let ms = window.resetsMs, let at = LimitsFormat.resetAt(window, now: now) else { return nil }
        return "Resets \(at) · in \(Limits.until(ms, now: now))"
    }
}
