import SwiftUI

/// Hold to merge (DESIGN.md's signature): a green-tinted button that fills with solid green
/// from the left while held, 1.2 s linear, in whole 4 pt cells; letting go drains it in 200 ms
/// (ease-out). It merges only when the hold reaches the end: a tap, a press let go early, or
/// one that slides off the button (as a finger backs out of any button) never does.
struct HoldToMerge: View {
    let merging: Bool
    let onMerge: () -> Void

    static let hold: Double = 1.2
    static let drain: Double = 0.2
    /// 148 × 40 at the usual text size; larger with the text, to the screen's width.
    @ScaledMetric(relativeTo: .footnote) private var scaledWidth: CGFloat = 148
    @ScaledMetric(relativeTo: .footnote) private var scaledHeight: CGFloat = 40
    private var width: CGFloat { min(scaledWidth, 300) }
    private var height: CGFloat { min(scaledHeight, 64) }

    /// How far past the button a finger may stray and still be holding it.
    static let slop: CGFloat = 24

    @GestureState private var touch = Touch()
    @State private var phase: Phase = .idle
    @State private var fire: Task<Void, Never>?
    @State private var merges = 0

    enum Phase: Equatable {
        case idle
        case holding(since: Date)
        case draining(from: Double, since: Date)
        /// It reached the end; it stays full until merging shows.
        case done
    }

    /// A finger on the button: down, and whether it has slid off (that press is over, even
    /// if it comes back).
    struct Touch: Equatable {
        var down = false
        var slidOff = false
        var pressing: Bool { down && !slidOff }

        /// The finger at `location`, in the button's own space.
        mutating func moved(to location: CGPoint, in size: CGSize) {
            down = true
            if !CGRect(origin: .zero, size: size).insetBy(dx: -HoldToMerge.slop, dy: -HoldToMerge.slop).contains(location) { slidOff = true }
        }
    }

    var body: some View {
        Group {
            if merging {
                HStack(spacing: 6) {
                    ProgressView().controlSize(.small).tint(Palette.ok)
                    Text("Merging…").font(.footnote.weight(.semibold)).foregroundStyle(Palette.ok)
                }
                .frame(width: width, height: height)
                .background(RoundedRectangle(cornerRadius: 6, style: .continuous).fill(Palette.okFill))
                .accessibilityElement(children: .combine)
            } else {
                button
            }
        }
        .sensoryFeedback(.impact(weight: .medium), trigger: merges)
    }

    private var button: some View {
        TimelineView(.animation(paused: phase == .idle || phase == .done)) { tl in
            let fill = Self.cells(Self.progress(phase, at: tl.date), width: width)
            ZStack(alignment: .leading) {
                Palette.okFill
                label.foregroundStyle(Palette.ok)
                Palette.ok.frame(width: fill)
                label.foregroundStyle(Palette.onOk)
                    .mask(alignment: .leading) { Rectangle().frame(width: fill) }
            }
            .frame(width: width, height: height)
            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        }
        .contentShape(Rectangle())
        .gesture(DragGesture(minimumDistance: 0).updating($touch) { value, touch, _ in
            touch.moved(to: value.location, in: CGSize(width: width, height: height))
        })
        .onChange(of: touch.pressing) { _, down in down ? start() : release() }
        .accessibilityElement()
        .accessibilityLabel("Hold to merge")
        .accessibilityHint("Double-tap and hold to merge the pull request")
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("holdToMerge")
    }

    private var label: some View {
        HStack(spacing: 5) {
            Image(systemName: "arrow.triangle.merge").font(.footnote.weight(.semibold))
            Text("Hold to merge").font(.footnote.weight(.semibold))
        }
        .frame(width: width, height: height)
        .lineLimit(1)
        .minimumScaleFactor(0.8)
    }

    private func start() {
        guard !merging else { return }
        let since = Date()
        phase = .holding(since: since)
        fire?.cancel()
        fire = Task {
            try? await Task.sleep(for: .seconds(Self.hold))
            // Still the same press, never let go.
            guard !Task.isCancelled, phase == .holding(since: since) else { return }
            phase = .done
            merges += 1
            onMerge()
        }
    }

    private func release() {
        fire?.cancel()
        fire = nil
        guard case .holding = phase else {
            if phase == .done { phase = .idle }
            return
        }
        let from = Self.progress(phase, at: Date())
        let since = Date()
        phase = .draining(from: from, since: since)
        Task {
            try? await Task.sleep(for: .seconds(Self.drain))
            if phase == .draining(from: from, since: since) { phase = .idle }
        }
    }

    /// How full it is, 0…1.
    static func progress(_ phase: Phase, at date: Date) -> Double {
        switch phase {
        case .idle: 0
        case .done: 1
        case .holding(let since): min(max(date.timeIntervalSince(since) / hold, 0), 1)
        case .draining(let from, let since): from * (1 - easeOut(min(max(date.timeIntervalSince(since) / drain, 0), 1)))
        }
    }

    /// The fill's width in whole 4 pt cells.
    static func cells(_ progress: Double, width: CGFloat) -> CGFloat {
        (progress * width / 4).rounded(.down) * 4
    }

    /// `cubic-bezier(0.23, 1, 0.32, 1)` at `x`.
    static func easeOut(_ x: Double) -> Double {
        let (x1, y1, x2, y2) = (0.23, 1.0, 0.32, 1.0)
        func bez(_ t: Double, _ a: Double, _ b: Double) -> Double { 3 * a * t * (1 - t) * (1 - t) + 3 * b * t * t * (1 - t) + t * t * t }
        var lo = 0.0, hi = 1.0
        for _ in 0..<24 {
            let mid = (lo + hi) / 2
            if bez(mid, x1, x2) < x { lo = mid } else { hi = mid }
        }
        return bez((lo + hi) / 2, y1, y2)
    }
}
