// The iPhone icon's Dark and Tinted forms (iOS 18 and later), made from the full-colour icon.
//
//   swift ios/script/icon-variants.swift design/icons/AppIcon.png ios/Flotilla/Assets.xcassets/AppIcon.appiconset
//
// Both take the plum ground away (iOS draws its own behind them) and keep the three sails where
// they are. Dark keeps their colours; Tinted is grey for iOS to tint, each sail as light as it
// is in colour, the lightest white. Each pixel is read as a mix of the ground and the sail it's
// nearest to, so a sail's edge keeps its softness as transparency.

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count == 3 else {
    FileHandle.standardError.write("usage: icon-variants.swift <AppIcon.png> <appiconset dir>\n".data(using: .utf8)!)
    exit(2)
}

typealias RGB = (Double, Double, Double)

func load(_ path: String) -> (w: Int, h: Int, px: [UInt8]) {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
          let img = CGImageSourceCreateImageAtIndex(src, 0, nil)
    else { fatalError("can't read \(path)") }
    let (w, h) = (img.width, img.height)
    var px = [UInt8](repeating: 0, count: w * h * 4)
    let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
    return (w, h, px)
}

func save(_ px: [UInt8], _ w: Int, _ h: Int, _ path: String) {
    var px = px
    let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let img = ctx.makeImage()!
    let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: path) as CFURL, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, img, nil)
    guard CGImageDestinationFinalize(dest) else { fatalError("can't write \(path)") }
}

let (w, h, px) = load(args[1])
func at(_ x: Int, _ y: Int) -> RGB {
    let i = (y * w + x) * 4
    return (Double(px[i]), Double(px[i + 1]), Double(px[i + 2]))
}

// The ground, from the corners; the sails, from their middles (as PRODUCT.md has them:
// coral, amber, lavender).
let corners = [at(8, 8), at(w - 9, 8), at(8, h - 9), at(w - 9, h - 9)]
let ground: RGB = (corners.map(\.0).reduce(0, +) / 4, corners.map(\.1).reduce(0, +) / 4, corners.map(\.2).reduce(0, +) / 4)
let sails: [RGB] = [at(w * 250 / 1024, h * 560 / 1024), at(w * 520 / 1024, h * 450 / 1024), at(w * 800 / 1024, h * 520 / 1024)]

func luminance(_ c: RGB) -> Double { 0.2126 * c.0 + 0.7152 * c.1 + 0.0722 * c.2 }
let lightest = sails.map(luminance).max()!

var dark = [UInt8](repeating: 0, count: w * h * 4)
var tinted = [UInt8](repeating: 0, count: w * h * 4)
for y in 0..<h {
    for x in 0..<w {
        let p = at(x, y)
        // How far along ground → sail this pixel is, for the sail that explains it best.
        var best = (alpha: 0.0, miss: Double.infinity, sail: 0)
        for (n, s) in sails.enumerated() {
            let d = (s.0 - ground.0, s.1 - ground.1, s.2 - ground.2)
            let v = (p.0 - ground.0, p.1 - ground.1, p.2 - ground.2)
            let a = min(max((v.0 * d.0 + v.1 * d.1 + v.2 * d.2) / (d.0 * d.0 + d.1 * d.1 + d.2 * d.2), 0), 1)
            let r = (v.0 - a * d.0, v.1 - a * d.1, v.2 - a * d.2)
            let miss = r.0 * r.0 + r.1 * r.1 + r.2 * r.2
            if miss < best.miss { best = (a, miss, n) }
        }
        // The ground's own grain isn't a sail.
        let alpha = best.alpha < 0.08 ? 0 : best.alpha
        let s = sails[best.sail]
        let i = (y * w + x) * 4
        // Premultiplied.
        dark[i] = UInt8((s.0 * alpha).rounded())
        dark[i + 1] = UInt8((s.1 * alpha).rounded())
        dark[i + 2] = UInt8((s.2 * alpha).rounded())
        dark[i + 3] = UInt8((255 * alpha).rounded())
        let grey = 255 * luminance(s) / lightest * alpha
        tinted[i] = UInt8(grey.rounded())
        tinted[i + 1] = UInt8(grey.rounded())
        tinted[i + 2] = UInt8(grey.rounded())
        tinted[i + 3] = UInt8((255 * alpha).rounded())
    }
}
save(dark, w, h, "\(args[2])/AppIcon-dark.png")
save(tinted, w, h, "\(args[2])/AppIcon-tinted.png")
print("wrote AppIcon-dark.png and AppIcon-tinted.png")
