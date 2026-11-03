// ocr.swift <dir>: every .png/.jpg in <dir>, OCR'd with Vision (accurate), as "<file>\t<line>" rows.
import Foundation
import Vision
import AppKit

let dir = URL(fileURLWithPath: CommandLine.arguments[1])
let files = try FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
    .filter { ["png", "jpg", "jpeg"].contains($0.pathExtension.lowercased()) }
    .sorted { $0.lastPathComponent < $1.lastPathComponent }
for f in files {
    guard let img = NSImage(contentsOf: f), let cg = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else { continue }
    let req = VNRecognizeTextRequest()
    req.recognitionLevel = .accurate
    req.usesLanguageCorrection = false
    req.minimumTextHeight = 0.008
    try? VNImageRequestHandler(cgImage: cg).perform([req])
    for obs in req.results ?? [] {
        if let t = obs.topCandidates(1).first?.string { print("\(f.lastPathComponent)\t\(t)") }
    }
}
