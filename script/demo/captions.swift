// captions.swift <captions.tsv> <outdir> <width> <height>
// Renders each caption (lines of "start\ttext"; the last line is the end time) as a dark
// strip PNG, and writes caps.txt, an ffmpeg concat list holding each strip for its duration.
import AppKit

let args = CommandLine.arguments
let tsv = try! String(contentsOfFile: args[1], encoding: .utf8)
let out = URL(fileURLWithPath: args[2])
let width = Int(args[3])!, height = Int(args[4])!
let rows = tsv.split(separator: "\n", omittingEmptySubsequences: true).map { line -> (Double, String) in
    let parts = line.split(separator: "\t", maxSplits: 1, omittingEmptySubsequences: false)
    return (Double(parts[0]) ?? 0, parts.count > 1 ? String(parts[1]) : "")
}
var list = ""
// The recording may start before the first caption: lead with an empty strip.
var entries = rows
if let first = entries.first, first.0 > 0.0001 { entries.insert((0, ""), at: 0) }
for i in 0..<(entries.count - 1) {
    let (start, text) = entries[i]
    let duration = max(entries[i + 1].0 - start, 0.001)
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSColor(calibratedRed: 0.11, green: 0.12, blue: 0.14, alpha: 1).setFill()
    NSRect(x: 0, y: 0, width: width, height: height).fill()
    let font = NSFont.systemFont(ofSize: CGFloat(height) * 0.36, weight: .semibold)
    let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor.white]
    let str = NSAttributedString(string: text, attributes: attrs)
    let size = str.size()
    str.draw(at: NSPoint(x: 24, y: (CGFloat(height) - size.height) / 2))
    NSGraphicsContext.restoreGraphicsState()
    let name = String(format: "cap%03d.png", i)
    try! rep.representation(using: .png, properties: [:])!.write(to: out.appendingPathComponent(name))
    list += "file '\(name)'\nduration \(String(format: "%.4f", duration))\n"
}
list += "file '\(String(format: "cap%03d.png", entries.count - 2))'\n"
try! list.write(to: out.appendingPathComponent("caps.txt"), atomically: true, encoding: .utf8)
print("captions: \(entries.count - 1)")
