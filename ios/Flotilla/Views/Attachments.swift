import FlotillaKit
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// The paperclip: a photo from the library, or any file from Files. What's chosen waits in
/// the composer (`key`: its thread, "" for New thread) until it's sent.
struct AttachButton: View {
    @Environment(AppModel.self) private var model
    let key: String
    var disabled = false
    @State private var photos = false
    @State private var files = false
    @State private var picked: [PhotosPickerItem] = []

    var body: some View {
        Menu {
            Button("Photo Library", systemImage: "photo.on.rectangle") { photos = true }
                .accessibilityIdentifier("attachPhotos")
            Button("Files", systemImage: "folder") { files = true }
                .accessibilityIdentifier("attachFiles")
        } label: {
            Image(systemName: "paperclip")
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(Palette.dim)
                .frame(width: 34, height: 30)
                .contentShape(Rectangle())
        }
        .disabled(disabled)
        .accessibilityLabel("Attach")
        .accessibilityIdentifier("attach")
        .photosPicker(isPresented: $photos, selection: $picked, maxSelectionCount: AttachLimits.maxFiles, matching: .images, photoLibrary: .shared())
        .onChange(of: picked) { _, items in
            guard !items.isEmpty else { return }
            picked = []
            Task { await addPhotos(items) }
        }
        .fileImporter(isPresented: $files, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            if case .success(let urls) = result { addFiles(urls) }
        }
    }

    /// Photos go as JPEG (what every model reads), at most 2048 pixels on a side.
    private func addPhotos(_ items: [PhotosPickerItem]) async {
        var out: [PendingFile] = []
        var n = (model.pending[key] ?? []).count { $0.name.hasPrefix("photo-") }
        for item in items {
            guard let data = try? await item.loadTransferable(type: Data.self), let image = UIImage(data: data) else { continue }
            let jpeg = await Task.detached { Self.jpeg(image) }.value
            guard let jpeg else { continue }
            n += 1
            out.append(PendingFile(name: "photo-\(n).jpg", data: jpeg, kind: .image, thumbnail: image.preparingThumbnail(of: CGSize(width: 120, height: 120))))
        }
        model.attach(out, to: key)
    }

    nonisolated static func jpeg(_ image: UIImage) -> Data? {
        let side = max(image.size.width, image.size.height)
        let scale = min(1, 2048 / max(side, 1))
        guard scale < 1 else { return image.jpegData(compressionQuality: 0.85) }
        let size = CGSize(width: (image.size.width * scale).rounded(), height: (image.size.height * scale).rounded())
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let small = UIGraphicsImageRenderer(size: size, format: format).image { _ in image.draw(in: CGRect(origin: .zero, size: size)) }
        return small.jpegData(compressionQuality: 0.85)
    }

    private func addFiles(_ urls: [URL]) {
        var out: [PendingFile] = []
        for url in urls {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            let name = url.lastPathComponent
            // Too big to go is left unread.
            if let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize, Int64(size) > AttachLimits.maxFileMB * AttachLimits.mb {
                model.attachProblem[key] = "\(name) is over \(AttachLimits.maxFileMB) MB"
                continue
            }
            guard let data = try? Data(contentsOf: url) else {
                model.attachProblem[key] = "Couldn't read \(name)"
                continue
            }
            let kind = FileKind(name: name)
            out.append(PendingFile(name: name, data: data, kind: kind, thumbnail: kind == .image ? UIImage(data: data)?.preparingThumbnail(of: CGSize(width: 120, height: 120)) : nil))
        }
        if !out.isEmpty { model.attach(out, to: key) }
    }
}

/// The files waiting in a composer: a photo as its thumbnail, anything else as its name;
/// × takes one off.
struct PendingFiles: View {
    @Environment(AppModel.self) private var model
    let key: String

    var body: some View {
        let files = model.pending[key] ?? []
        if !files.isEmpty || model.attachProblem[key] != nil {
            VStack(alignment: .leading, spacing: 6) {
                if !files.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(files) { f in chip(f) }
                        }
                    }
                }
                if let why = model.attachProblem[key] {
                    Text(why).font(Typeface.meta).foregroundStyle(Palette.bad).accessibilityIdentifier("attachProblem")
                }
            }
        }
    }

    private func chip(_ f: PendingFile) -> some View {
        HStack(spacing: 7) {
            if let t = f.thumbnail {
                Image(uiImage: t).resizable().scaledToFill().frame(width: 30, height: 30).clipShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
            } else {
                Image(systemName: icon(f.kind)).font(.footnote).foregroundStyle(Palette.dim).frame(width: 20)
            }
            VStack(alignment: .leading, spacing: 0) {
                Text(f.name).font(Typeface.label).foregroundStyle(Palette.text).lineLimit(1).truncationMode(.middle)
                Text(Format.bytes(Int64(f.data.count))).font(Typeface.meta).foregroundStyle(Palette.dim)
            }
            .frame(maxWidth: 140, alignment: .leading)
            Button {
                model.detach(f.id, from: key)
            } label: {
                Image(systemName: "xmark").font(.system(size: 10, weight: .bold)).foregroundStyle(Palette.dim).frame(width: 22, height: 22).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Remove \(f.name)")
        }
        .padding(.leading, f.thumbnail == nil ? 9 : 4)
        .padding(.trailing, 3)
        .padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 9, style: .continuous).fill(Palette.muted))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("pending-\(f.name)")
    }

    private func icon(_ k: FileKind) -> String {
        switch k {
        case .image: "photo"
        case .text: "doc.text"
        case .code: "chevron.left.forwardslash.chevron.right"
        case .data: "curlybraces"
        case .sheet: "tablecells"
        case .archive: "archivebox"
        case .video: "film"
        case .audio: "waveform"
        case .binary: "number"
        case .other: "doc"
        }
    }
}
