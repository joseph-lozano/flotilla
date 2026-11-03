import SwiftUI

/// A status pip: a square with corners a quarter of its size, the soft pixel.
struct Pip: View {
    var color: Color
    var size: CGFloat = 7

    var body: some View {
        RoundedRectangle(cornerRadius: size / 4, style: .continuous)
            .fill(color)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

/// The icon's sail: a 6-row stepped triangle on its mast over a 2-row hull, 8 cells wide.
private func sail(_ ctx: inout GraphicsContext, x: CGFloat, y: CGFloat, cell c: CGFloat, color: Color) {
    for i in 0..<6 {
        ctx.fill(Path(CGRect(x: (x + 1) * c, y: (y + CGFloat(i)) * c, width: CGFloat(i + 1) * c, height: c)), with: .color(color))
    }
    ctx.fill(Path(CGRect(x: x * c, y: (y + 7) * c, width: 8 * c, height: 2 * c)), with: .color(color))
}

/// The mark: coral, amber two cells higher, lavender. Drawn from exact cells.
struct PixelMark: View {
    var cell: CGFloat = 3

    var body: some View {
        Canvas { ctx, _ in
            sail(&ctx, x: 0, y: 2, cell: cell, color: Palette.coral)
            sail(&ctx, x: 10, y: 0, cell: cell, color: Palette.amber)
            sail(&ctx, x: 20, y: 2, cell: cell, color: Palette.lavender)
        }
        .frame(width: 28 * cell, height: 11 * cell)
        .accessibilityHidden(true)
    }
}

/// One lavender sail, for a thread's empty state.
struct PixelSail: View {
    var cell: CGFloat = 4

    var body: some View {
        Canvas { ctx, _ in sail(&ctx, x: 0, y: 0, cell: cell, color: Palette.lavender) }
            .frame(width: 8 * cell, height: 9 * cell)
            .accessibilityHidden(true)
    }
}

/// The agent's logo: Claude Code's coral rays, Codex's hexagon, else its first letter.
struct ProviderLogo: View {
    var provider: String
    var size: CGFloat = 14

    var body: some View {
        Group {
            switch provider {
            case "claude-code":
                Canvas { ctx, s in
                    let c = CGPoint(x: s.width / 2, y: s.height / 2)
                    for a in stride(from: 0.0, to: 180.0, by: 30.0) {
                        var ray = Path(roundedRect: CGRect(x: -s.width * 0.057, y: -s.height * 0.43, width: s.width * 0.114, height: s.height * 0.86), cornerRadius: s.width * 0.057)
                        ray = ray.applying(CGAffineTransform(rotationAngle: a * .pi / 180)).applying(CGAffineTransform(translationX: c.x, y: c.y))
                        ctx.fill(ray, with: .color(Color(hex: 0xD97757)))
                    }
                }
            case "codex":
                Canvas { ctx, s in
                    let r = s.width * 0.42
                    var hex = Path()
                    for i in 0..<6 {
                        let a = (Double(i) * 60 - 90) * .pi / 180
                        let p = CGPoint(x: s.width / 2 + r * cos(a), y: s.height / 2 + r * sin(a))
                        if i == 0 { hex.move(to: p) } else { hex.addLine(to: p) }
                    }
                    hex.closeSubpath()
                    ctx.stroke(hex, with: .color(Palette.text), style: StrokeStyle(lineWidth: s.width * 0.1, lineJoin: .round))
                    ctx.fill(Path(ellipseIn: CGRect(x: s.width / 2 - s.width * 0.11, y: s.height / 2 - s.height * 0.11, width: s.width * 0.22, height: s.height * 0.22)), with: .color(Palette.text))
                }
            default:
                Text(Format.providerName(provider).prefix(1).uppercased())
                    .font(.system(size: size * 0.7, weight: .semibold))
                    .foregroundStyle(Palette.dim)
                    .frame(width: size, height: size)
                    .background(Circle().fill(Palette.muted))
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// The subagent's bot, as the mockups draw it.
struct BotIcon: View {
    var size: CGFloat = 16

    var body: some View {
        Canvas { ctx, s in
            let k = s.width / 16
            let line = StrokeStyle(lineWidth: 1.4 * k, lineCap: .round)
            ctx.stroke(Path(roundedRect: CGRect(x: 2.5 * k, y: 5 * k, width: 11 * k, height: 8.5 * k), cornerRadius: 2.2 * k), with: .color(Palette.dim), style: line)
            var p = Path()
            p.move(to: CGPoint(x: 8 * k, y: 5 * k)); p.addLine(to: CGPoint(x: 8 * k, y: 2.5 * k))
            ctx.stroke(p, with: .color(Palette.dim), style: line)
            ctx.fill(Path(roundedRect: CGRect(x: 5.3 * k, y: 8 * k, width: 1.6 * k, height: 1.8 * k), cornerRadius: 0.5 * k), with: .color(Palette.dim))
            ctx.fill(Path(roundedRect: CGRect(x: 9.1 * k, y: 8 * k, width: 1.6 * k, height: 1.8 * k), cornerRadius: 0.5 * k), with: .color(Palette.dim))
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}
