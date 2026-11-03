import CoreGraphics
import CoreImage
import CoreImage.CIFilterBuiltins
import Vision

/// QR codes: reading a pairing link from a picture of one (a photo, or a frame from a camera
/// that has no reader of its own), and drawing one for Add device.
public enum QR {
    /// The texts the QR codes in `image` hold: Vision's, else Core Image's (Vision finds none
    /// on some machines without a neural engine, the simulator among them).
    public static func texts(in image: CGImage) async -> [String] {
        var request = DetectBarcodesRequest()
        request.symbologies = [.qr]
        let found = ((try? await request.perform(on: image)) ?? []).compactMap(\.payloadString)
        return found.isEmpty ? coreImageTexts(in: image) : found
    }

    /// Core Image's QR detector, on the CPU.
    static func coreImageTexts(in image: CGImage) -> [String] {
        let detector = CIDetector(ofType: CIDetectorTypeQRCode, context: CIContext(options: [.useSoftwareRenderer: true]), options: [CIDetectorAccuracy: CIDetectorAccuracyHigh])
        return (detector?.features(in: CIImage(cgImage: image)) ?? []).compactMap { ($0 as? CIQRCodeFeature)?.messageString }
    }

    /// The pairing link in `image`, if a QR code in it holds one with a code.
    public static func pairing(in image: CGImage) async -> Pairing.Target? {
        await texts(in: image).lazy.compactMap(pairing(from:)).first
    }

    /// A scanned text as a pairing link with its code, if it's one.
    public static func pairing(from text: String) -> Pairing.Target? {
        guard let t = try? Pairing.read(text), t.code != nil else { return nil }
        return t
    }

    /// `text` as a QR code, each module `scale` pixels, with a quiet zone of 4 modules, dark
    /// on light.
    public static func image(for text: String, scale: Int = 8) -> CGImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let code = filter.outputImage else { return nil }
        let quiet = code.extent.insetBy(dx: -4, dy: -4)
        let padded = code.composited(over: CIImage(color: .white).cropped(to: quiet))
        let big = padded.transformed(by: CGAffineTransform(scaleX: CGFloat(scale), y: CGFloat(scale)))
        return CIContext(options: [.useSoftwareRenderer: false]).createCGImage(big, from: big.extent)
    }
}
