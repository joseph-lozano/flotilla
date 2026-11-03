// Draw the text in a file as a QR code PNG, for pairing from a photo in the simulator:
//   swift ios/script/qr.swift <file with the link> <out.png>
//   xcrun simctl addmedia <simulator> <out.png>
// The link is read from a file so the code never shows in a command line.
import CoreImage
import CoreImage.CIFilterBuiltins
import Foundation
import ImageIO
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count == 3, let text = try? String(contentsOfFile: args[1], encoding: .utf8) else {
    FileHandle.standardError.write(Data("usage: qr.swift <file with the link> <out.png>\n".utf8))
    exit(2)
}
let filter = CIFilter.qrCodeGenerator()
filter.message = Data(text.trimmingCharacters(in: .whitespacesAndNewlines).utf8)
filter.correctionLevel = "M"
let code = filter.outputImage!
let quiet = code.extent.insetBy(dx: -4, dy: -4)
// A photo of a screen: the code a little small, on a light ground.
let padded = code.composited(over: CIImage(color: .white).cropped(to: quiet)).transformed(by: CGAffineTransform(scaleX: 14, y: 14))
let ground = CIImage(color: CIColor(red: 0.96, green: 0.94, blue: 0.97)).cropped(to: CGRect(x: 0, y: 0, width: 1200, height: 1600))
let photo = padded.transformed(by: CGAffineTransform(translationX: (1200 - padded.extent.width) / 2, y: (1600 - padded.extent.height) / 2)).composited(over: ground)
let cg = CIContext().createCGImage(photo, from: photo.extent)!
let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: args[2]) as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(dest, cg, nil)
CGImageDestinationFinalize(dest)
