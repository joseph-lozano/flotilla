// simframes.swift <video> <outdir> [scale]
// The frames of a `simctl io recordVideo` file, at their real times. Its H.264 has B-frames with
// decode times far behind their presentation times, and ffmpeg's timestamp guess falls back to the
// decode times part-way through, putting frames out of order. AVFoundation reads them right: this
// writes each frame as a JPEG (scaled by `scale`, default 0.5) and `frames.txt`, an ffmpeg concat
// list with each frame's duration, in presentation order.
import AVFoundation
import CoreImage
import ImageIO
import UniformTypeIdentifiers

let args = CommandLine.arguments
let url = URL(fileURLWithPath: args[1])
let out = URL(fileURLWithPath: args[2])
let scale = args.count > 3 ? Double(args[3])! : 0.5
try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

let asset = AVURLAsset(url: url)
let sem = DispatchSemaphore(value: 0)
var track: AVAssetTrack?
var duration = CMTime.zero
Task {
    track = try? await asset.loadTracks(withMediaType: .video).first
    duration = (try? await asset.load(.duration)) ?? .zero
    sem.signal()
}
sem.wait()
guard let track else { fatalError("no video track") }
let reader = try AVAssetReader(asset: asset)
let output = AVAssetReaderTrackOutput(track: track, outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA])
reader.add(output)
reader.startReading()
let ci = CIContext()
var frames: [(Double, String)] = []
var n = 0
while let sample = output.copyNextSampleBuffer() {
    guard let pixels = CMSampleBufferGetImageBuffer(sample) else { continue }
    let t = CMSampleBufferGetPresentationTimeStamp(sample).seconds
    var image = CIImage(cvPixelBuffer: pixels)
    if scale != 1 { image = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale)) }
    guard let cg = ci.createCGImage(image, from: image.extent) else { continue }
    n += 1
    let name = String(format: "f%05d.jpg", n)
    let dest = CGImageDestinationCreateWithURL(out.appendingPathComponent(name) as CFURL, UTType.jpeg.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, cg, [kCGImageDestinationLossyCompressionQuality: 0.92] as CFDictionary)
    CGImageDestinationFinalize(dest)
    frames.append((t, name))
}
frames.sort { $0.0 < $1.0 }
let end = duration.seconds
var list = ""
for (i, (t, name)) in frames.enumerated() {
    let next = i + 1 < frames.count ? frames[i + 1].0 : end
    list += "file '\(name)'\nduration \(String(format: "%.4f", max(next - t, 0.001)))\n"
}
if let last = frames.last { list += "file '\(last.1)'\n" }
try list.write(to: out.appendingPathComponent("frames.txt"), atomically: true, encoding: .utf8)
print("\(frames.count) frames, \(String(format: "%.2f", end)) s, first at \(String(format: "%.3f", frames.first?.0 ?? 0))")
