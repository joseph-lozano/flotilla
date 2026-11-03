import FlotillaKit
import PhotosUI
import SwiftUI

/// Connect to a server: scan the QR code `flotilla pair` (or Add device on a paired device)
/// shows, or a photo of it, or type its address and one-time code. A `flotilla://pair` link,
/// scanned or opened, fills the form in; connecting takes a tap. A line says where the code
/// comes from, with a link to the docs on pairing.
struct PairingScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Phone.self) private var phone
    /// Shown over the threads (a server is already paired): it can be cancelled.
    var cancellable = false
    @Environment(\.dismiss) private var dismiss
    @State private var server = ""
    @State private var code = ""
    @State private var name = AppModel.deviceName
    @State private var pairing = false
    @State private var problem: String?
    @State private var camera: QRScanner.State = .starting
    @State private var photo: PhotosPickerItem?
    @State private var scanned = false
    @FocusState private var field: Field?
    @Environment(\.dynamicTypeSize) private var textSize

    enum Field { case server, code, name }

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
            ScrollView {
                VStack(spacing: 0) {
                    PixelMark(cell: 3).padding(.top, 28)
                    Text("Connect to a server")
                        .font(.title2.weight(.bold))
                        .foregroundStyle(Palette.text)
                        .padding(.top, 14)
                        .padding(.bottom, 6)
                        .accessibilityAddTraits(.isHeader)
                    // Where the code comes from (the command on one line), and the docs' pairing
                    // section in the in-app Safari view.
                    Text("Get a code from `flotilla\u{00A0}pair` on your server, or from Settings → Devices on your Mac. [How to pair](https://useflotilla.com/docs/main/iphone.html#pair)")
                        .font(.footnote)
                        .foregroundStyle(Palette.dim)
                        .tint(Palette.link)
                        .multilineTextAlignment(.center)
                        .environment(\.openURL, OpenURLAction { url in
                            InAppBrowser.open(url)
                            return .handled
                        })
                        .padding(.bottom, 18)
                        .accessibilityIdentifier("pairHelp")
                    viewfinder
                    PhotosPicker(selection: $photo, matching: .images, photoLibrary: .shared()) {
                        Text("Use a photo of the code").font(.footnote.weight(.semibold)).foregroundStyle(Palette.link).frame(minHeight: 32)
                    }
                    .accessibilityIdentifier("pairPhoto")
                    .padding(.top, 8)
                    .padding(.bottom, 14)
                    HStack(spacing: 10) {
                        Rectangle().fill(Palette.border).frame(height: 1)
                        Text("or enter it").font(.footnote).foregroundStyle(Palette.dim).fixedSize()
                        Rectangle().fill(Palette.border).frame(height: 1)
                    }
                    .padding(.bottom, 14)
                    VStack(spacing: 10) {
                        box("Server") {
                            // Verbatim: as a key, an address reads as a link, in the link's blue.
                            TextField(text: $server, prompt: Text(verbatim: "https://box.tail1234.ts.net")) { Text("Server") }
                                .font(Typeface.mono(.subheadline))
                                .keyboardType(.URL)
                                .textContentType(.URL)
                                .textInputAutocapitalization(.never)
                                .focused($field, equals: .server)
                                .submitLabel(.next)
                                .onSubmit { field = .code }
                                .onChange(of: server) { _, typed in fillFromLink(typed) }
                                .accessibilityIdentifier("pairServer")
                        }
                        box("Code") {
                            TextField("ABCD-EFGH-JKMN", text: $code)
                                .font(.custom("Menlo", size: 18, relativeTo: .title3))
                                .kerning(2)
                                .textInputAutocapitalization(.characters)
                                .focused($field, equals: .code)
                                .submitLabel(.next)
                                .onSubmit { field = .name }
                                .accessibilityIdentifier("pairCode")
                        }
                        // iOS tells apps only "iPhone": the server lists this phone by this.
                        box("Device name") {
                            TextField("iPhone", text: $name)
                                .font(.subheadline)
                                .textInputAutocapitalization(.words)
                                .focused($field, equals: .name)
                                .submitLabel(.go)
                                .onSubmit(connect)
                                .accessibilityIdentifier("pairName")
                        }
                    }
                    .autocorrectionDisabled()
                    if let problem {
                        Text(problem)
                            .font(Typeface.label)
                            .foregroundStyle(Palette.bad)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.top, 8)
                            .accessibilityIdentifier("pairProblem")
                    }
                    Button(action: connect) {
                        Group {
                            if pairing { ProgressView().tint(Palette.onPrimary) } else { Text("Connect") }
                        }
                        .font(.headline)
                        .foregroundStyle(Palette.onPrimary)
                        .frame(maxWidth: .infinity, minHeight: 50)
                        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Palette.primary))
                    }
                    .buttonStyle(.plain)
                    .disabled(pairing || server.isEmpty || code.isEmpty)
                    .opacity(server.isEmpty || code.isEmpty ? 0.5 : 1)
                    .padding(.top, 16)
                    .padding(.bottom, 12)
                    .id("connect")
                    .accessibilityIdentifier("pairConnect")
                }
                .padding(.horizontal, 22)
            }
            .scrollDismissesKeyboard(.interactively)
            // Typing: Connect stays in sight above the keyboard, under the fields.
            .onChange(of: field) { _, now in
                guard now != nil, !textSize.isAccessibilitySize else { return }
                Task {
                    try? await Task.sleep(for: .milliseconds(350))
                    withAnimation(.smooth(duration: 0.25)) { proxy.scrollTo("connect", anchor: .bottom) }
                }
            }
            }
            .background(Palette.background)
            .toolbar {
                if cancellable {
                    ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                }
            }
        }
        .onAppear(perform: fill)
        .onChange(of: model.pairingLink) { _, _ in fill() }
        .onChange(of: photo) { _, item in
            guard let item else { return }
            photo = nil
            Task { await read(item) }
        }
    }

    /// The camera's picture with the mockup's corners; with no camera, the dark ground alone.
    private var viewfinder: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 24, style: .continuous)
                .fill(RadialGradient(colors: [Color(hex: 0x3A2A40), Color(hex: 0x120B15)], center: UnitPoint(x: 0.4, y: 0.35), startRadius: 0, endRadius: 190))
            QRScanner(found: scannedText, state: { camera = $0 })
                .clipShape(RoundedRectangle(cornerRadius: 24, style: .continuous))
                .opacity(camera == .running ? 1 : 0)
            Corners().stroke(scanned ? Palette.ok : Palette.ring, style: StrokeStyle(lineWidth: 4, lineCap: .round)).padding(30)
            if case .unavailable(let why) = camera {
                Text(why).font(Typeface.meta).foregroundStyle(Color(hex: 0xA898AE)).multilineTextAlignment(.center).padding(.horizontal, 60)
            }
        }
        .frame(width: 250, height: 250)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(camera == .running ? "Camera, looking for a pairing code" : "Camera unavailable")
        .accessibilityIdentifier("viewfinder")
    }

    /// A code the camera read: the form fills in if it's a pairing link.
    private func scannedText(_ text: String) {
        guard let t = QR.pairing(from: text) else { return }
        take(t)
    }

    /// A photo of a code, read here.
    private func read(_ item: PhotosPickerItem) async {
        guard let data = try? await item.loadTransferable(type: Data.self), let image = UIImage(data: data)?.cgImage else {
            problem = "Couldn't open that photo"
            return
        }
        guard let t = await QR.pairing(in: image) else {
            problem = "There's no pairing code in that photo"
            return
        }
        take(t)
    }

    private func take(_ t: Pairing.Target) {
        server = t.server
        if let c = t.code { code = Pairing.display(code: c) }
        problem = nil
        scanned = true
        field = nil
        // A light tap that the camera found it; pairing itself gets the success.
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }

    private func box(_ label: String, @ViewBuilder _ content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label).font(Typeface.meta).foregroundStyle(Palette.dim)
            content()
        }
        .padding(.horizontal, 12)
        .padding(.top, 9)
        .padding(.bottom, 10)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(Palette.muted))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(Palette.border, lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(label)
    }

    /// A link the phone opened.
    private func fill() {
        guard let link = model.pairingLink else { return }
        server = link.server
        if let c = link.code { code = Pairing.display(code: c) }
        problem = nil
    }

    /// A whole link pasted into Server: take it apart.
    private func fillFromLink(_ typed: String) {
        guard typed.contains("://pair") || typed.contains("#"), let t = try? Pairing.read(typed) else { return }
        server = t.server
        if let c = t.code { code = Pairing.display(code: c) }
    }

    private func connect() {
        guard !pairing else { return }
        pairing = true
        problem = nil
        field = nil
        Task {
            do throws(Pairing.Problem) {
                try await phone.pair(server: server, code: code, name: name)
            } catch {
                problem = error.description
            }
            pairing = false
        }
    }
}

/// The viewfinder's four corners.
private nonisolated struct Corners: Shape {
    func path(in r: CGRect) -> Path {
        let l: CGFloat = 34, c: CGFloat = 10
        var p = Path()
        // Top left, top right, bottom right, bottom left: each a short arm, a curve, an arm.
        p.move(to: CGPoint(x: r.minX, y: r.minY + l))
        p.addLine(to: CGPoint(x: r.minX, y: r.minY + c))
        p.addQuadCurve(to: CGPoint(x: r.minX + c, y: r.minY), control: CGPoint(x: r.minX, y: r.minY))
        p.addLine(to: CGPoint(x: r.minX + l, y: r.minY))
        p.move(to: CGPoint(x: r.maxX - l, y: r.minY))
        p.addLine(to: CGPoint(x: r.maxX - c, y: r.minY))
        p.addQuadCurve(to: CGPoint(x: r.maxX, y: r.minY + c), control: CGPoint(x: r.maxX, y: r.minY))
        p.addLine(to: CGPoint(x: r.maxX, y: r.minY + l))
        p.move(to: CGPoint(x: r.maxX, y: r.maxY - l))
        p.addLine(to: CGPoint(x: r.maxX, y: r.maxY - c))
        p.addQuadCurve(to: CGPoint(x: r.maxX - c, y: r.maxY), control: CGPoint(x: r.maxX, y: r.maxY))
        p.addLine(to: CGPoint(x: r.maxX - l, y: r.maxY))
        p.move(to: CGPoint(x: r.minX + l, y: r.maxY))
        p.addLine(to: CGPoint(x: r.minX + c, y: r.maxY))
        p.addQuadCurve(to: CGPoint(x: r.minX, y: r.maxY - c), control: CGPoint(x: r.minX, y: r.maxY))
        p.addLine(to: CGPoint(x: r.minX, y: r.maxY - l))
        return p
    }
}
