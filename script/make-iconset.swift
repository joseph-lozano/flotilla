// Turn a full-bleed 1024×1024 artwork PNG into a macOS .iconset: each size is the artwork
// clipped to the rounded-rect icon body on Apple's grid (824/1024, centred), with a soft
// shadow. Pixels inside are scaled nearest-neighbour so pixel art stays crisp; only the
// rounded edge is anti-aliased.
//
//   swift script/make-iconset.swift <artwork.png> <out.iconset>
import CoreGraphics
import Foundation
import ImageIO

let args = CommandLine.arguments
guard args.count == 3,
      let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: args[1]) as CFURL, nil),
      let art = CGImageSourceCreateImageAtIndex(src, 0, nil)
else {
    FileHandle.standardError.write("usage: make-iconset.swift <artwork.png> <out.iconset>\n".data(using: .utf8)!)
    exit(1)
}
let out = URL(fileURLWithPath: args[2])
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

let sizes: [(Int, String)] = [
    (16, "icon_16x16"), (32, "icon_16x16@2x"), (32, "icon_32x32"), (64, "icon_32x32@2x"),
    (128, "icon_128x128"), (256, "icon_128x128@2x"), (256, "icon_256x256"), (512, "icon_256x256@2x"),
    (512, "icon_512x512"), (1024, "icon_512x512@2x"),
]
for (px, name) in sizes {
    let s = CGFloat(px)
    guard let ctx = CGContext(data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
                              space: CGColorSpace(name: CGColorSpace.sRGB)!,
                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { exit(1) }
    let inset = s * 100 / 1024
    let body = CGRect(x: inset, y: inset, width: s - 2 * inset, height: s - 2 * inset)
    let shape = CGPath(roundedRect: body, cornerWidth: s * 185.4 / 1024, cornerHeight: s * 185.4 / 1024, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -s * 8 / 1024), blur: s * 18 / 1024, color: CGColor(gray: 0, alpha: 0.3))
    ctx.addPath(shape)
    ctx.setFillColor(CGColor(gray: 0, alpha: 1))
    ctx.fillPath()
    ctx.restoreGState()
    ctx.addPath(shape)
    ctx.clip()
    ctx.interpolationQuality = .none
    ctx.draw(art, in: body)
    guard let img = ctx.makeImage(),
          let dest = CGImageDestinationCreateWithURL(out.appendingPathComponent("\(name).png") as CFURL, "public.png" as CFString, 1, nil)
    else { exit(1) }
    CGImageDestinationAddImage(dest, img, nil)
    CGImageDestinationFinalize(dest)
}
