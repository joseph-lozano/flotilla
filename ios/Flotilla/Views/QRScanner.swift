import AVFoundation
import SwiftUI
import UIKit

/// The camera, reading QR codes: each text it finds goes to `found`, on the main actor. With
/// no camera (the simulator), or no permission, it shows nothing and says so through `state`.
struct QRScanner: UIViewRepresentable {
    enum State: Equatable {
        case starting, running
        /// No camera on this device, or the camera isn't allowed.
        case unavailable(String)
    }

    let found: (String) -> Void
    var state: (State) -> Void = { _ in }

    func makeUIView(context: Context) -> Preview {
        let view = Preview()
        context.coordinator.start(in: view)
        return view
    }

    func updateUIView(_ view: Preview, context: Context) {
        context.coordinator.found = found
        context.coordinator.state = state
    }

    static func dismantleUIView(_ view: Preview, coordinator: Coordinator) {
        coordinator.stop()
    }

    func makeCoordinator() -> Coordinator { Coordinator(found: found, state: state) }

    /// A view whose layer is the camera's picture.
    final class Preview: UIView {
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        var preview: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }
    }

    final class Coordinator: NSObject, AVCaptureMetadataOutputObjectsDelegate {
        var found: (String) -> Void
        var state: (State) -> Void
        private let session = AVCaptureSession()
        private let queue = DispatchQueue(label: "com.useflotilla.flotilla.camera")
        private var last: String?

        init(found: @escaping (String) -> Void, state: @escaping (State) -> Void) {
            self.found = found
            self.state = state
        }

        func start(in view: Preview) {
            view.preview.session = session
            view.preview.videoGravity = .resizeAspectFill
            // No camera, no question about it.
            guard let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) else {
                // Not while SwiftUI is making the view.
                Task { self.state(.unavailable("No camera")) }
                return
            }
            Task {
                guard await AVCaptureDevice.requestAccess(for: .video) else {
                    state(.unavailable("The camera isn't allowed: allow it in the iPhone's Settings, or enter the code"))
                    return
                }
                guard let input = try? AVCaptureDeviceInput(device: camera) else {
                    state(.unavailable("The camera isn't available"))
                    return
                }
                let output = AVCaptureMetadataOutput()
                session.beginConfiguration()
                guard session.canAddInput(input), session.canAddOutput(output) else {
                    session.commitConfiguration()
                    state(.unavailable("The camera is busy"))
                    return
                }
                session.addInput(input)
                session.addOutput(output)
                output.setMetadataObjectsDelegate(self, queue: .main)
                output.metadataObjectTypes = [.qr]
                session.commitConfiguration()
                let session = self.session
                queue.async { session.startRunning() }
                state(.running)
            }
        }

        func stop() {
            let session = self.session
            queue.async { session.stopRunning() }
        }

        nonisolated func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection) {
            let texts = objects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
            MainActor.assumeIsolated {
                for t in texts where t != last {
                    last = t
                    found(t)
                }
            }
        }
    }
}
