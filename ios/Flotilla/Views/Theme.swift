import SwiftUI
import UIKit

/// DESIGN.md's palette: plum-tinted neutrals, Day and Night following the phone, and the
/// icon's three sails for the few things that should catch the eye.
///
/// Not on the main actor: SwiftUI resolves a dynamic colour on its render thread too (a
/// spinner, a ticking clock), and a main-actor provider traps there.
nonisolated enum Palette {
    static let background = dynamic(0xFCFAFD, 0x1D1322)
    static let rail = dynamic(0xF4EFF6, 0x170F1B)
    static let muted = dynamic(0xF2ECF4, 0x281B2D)
    static let border = dynamic(0xE7DEEB, 0x33243A)
    static let text = dynamic(0x24152A, 0xF1E9F4)
    static let dim = dynamic(0x6C5C73, 0xA898AE)
    static let selection = dynamic(0xEEE5F3, 0x2E2034)
    static let ok = dynamic(0x1E7F48, 0x6ACB92)
    static let okFill = dynamic(0xD9EEE1, 0x1F3A2B)
    static let warn = dynamic(0x9A6410, 0xF6BC63)
    static let bad = dynamic(0xC93D47, 0xF37173)
    static let link = dynamic(0x6B4A94, 0xD3BFEC)
    static let ring = dynamic(0x8E6CB8, 0xC2A7E2)
    static let primary = dynamic(0x311E36, 0xC2A7E2)
    static let onPrimary = dynamic(0xFCFAFD, 0x24152A)
    static let card = dynamic(0xFFFFFF, 0x231828)
    /// What a filled hold-to-merge's label is drawn in.
    static let onOk = dynamic(0xFFFFFF, 0x24152A)

    /// The sails, at full strength: brand art only.
    static let coral = Color(hex: 0xF37173)
    static let amber = Color(hex: 0xF6BC63)
    static let lavender = Color(hex: 0xC2A7E2)
    static let plum = Color(hex: 0x311E36)

    private static func dynamic(_ day: UInt32, _ night: UInt32) -> Color {
        let (d, n) = (UIColor(hex: day), UIColor(hex: night))
        return Color(UIColor { @Sendable traits in traits.userInterfaceStyle == .dark ? n : d })
    }
}

nonisolated extension UIColor {
    convenience init(hex: UInt32) {
        self.init(red: CGFloat(hex >> 16 & 0xFF) / 255, green: CGFloat(hex >> 8 & 0xFF) / 255, blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
    }
}

nonisolated extension Color {
    init(hex: UInt32) { self.init(UIColor(hex: hex)) }
}

/// The type scale, in Dynamic Type's text styles so it grows with the phone's setting. The
/// system font for what people read; Menlo only for real data (branches, commands, ids).
enum Typeface {
    static let rowTitle = Font.callout.weight(.semibold)
    static let title = Font.headline
    static let body = Font.subheadline
    static let meta = Font.caption
    static let label = Font.footnote

    static func mono(_ style: Font.TextStyle = .caption) -> Font {
        let size: CGFloat = switch style {
        case .caption2: 11
        case .caption: 12
        case .footnote: 12.5
        case .subheadline: 14
        default: 15
        }
        return .custom("Menlo", size: size, relativeTo: style)
    }
}
