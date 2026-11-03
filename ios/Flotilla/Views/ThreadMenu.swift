import FlotillaKit
import SwiftUI

/// A thread's commands, as the Mac's right-click menu and ⋯ have them (`thread_menu.rs`): mark
/// read or unread, archive or unarchive, rename, its pull request, delete. A thread whose VM
/// has expired only opens its pull request and deletes.
struct ThreadMenuItems: View {
    @Environment(AppModel.self) private var model
    let summary: ThreadSummary
    let ask: (ThreadAsk) -> Void

    var body: some View {
        let id = summary.id
        if summary.thread.state != .expired {
            Section {
                if summary.activity == .unread {
                    Button("Mark as read", systemImage: "envelope.open") { model.markRead(id) }
                } else {
                    Button("Mark as unread", systemImage: "envelope.badge") { model.markUnread(id) }
                }
                if summary.activity == .archived {
                    Button("Unarchive", systemImage: "tray.and.arrow.up") { model.archive(id, false) }
                } else {
                    // From the thread's own page: done with it, back to the list (Undo is there).
                    Button("Archive", systemImage: "archivebox") {
                        model.archive(id, true)
                        if model.path.last == id { model.path.removeLast() }
                    }
                }
                Button("Rename…", systemImage: "pencil") { ask(.rename(summary)) }
            }
        }
        if let url = summary.pr?.url, let link = URL(string: url) {
            Section {
                Link(destination: link) { Label("Open the pull request", systemImage: "arrow.up.right.square") }
            }
        }
        Section {
            Button("Delete…", systemImage: "trash", role: .destructive) { ask(.delete(summary)) }
        }
    }
}

/// What a thread's menu asks before it acts: a new title, or whether to delete it.
enum ThreadAsk: Identifiable {
    case rename(ThreadSummary)
    case delete(ThreadSummary)

    var id: String {
        switch self {
        case .rename(let s): "rename-\(s.id)"
        case .delete(let s): "delete-\(s.id)"
        }
    }
}

extension View {
    /// Rename's field and Delete's question, for the thread a menu picked.
    func threadAsks(_ asking: Binding<ThreadAsk?>) -> some View {
        modifier(ThreadAsks(asking: asking))
    }
}

private struct ThreadAsks: ViewModifier {
    @Environment(AppModel.self) private var model
    @Binding var asking: ThreadAsk?
    @State private var title = ""

    private var renaming: ThreadSummary? {
        if case .rename(let s) = asking { return s }
        return nil
    }

    private var deleting: ThreadSummary? {
        if case .delete(let s) = asking { return s }
        return nil
    }

    func body(content: Content) -> some View {
        content
            .onChange(of: renaming?.id) { _, id in
                if let id { title = model.title(id, renaming?.thread.title) ?? "" }
            }
            .alert("Rename thread", isPresented: shown(renaming != nil), presenting: renaming) { s in
                TextField("Title", text: $title)
                    .accessibilityIdentifier("renameField")
                Button("Cancel", role: .cancel) {}
                Button("Rename") { model.rename(s.id, to: title) }
                    .disabled(title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("renameSave")
            }
            .alert(deleteTitle, isPresented: shown(deleting != nil), presenting: deleting) { s in
                Button("Cancel", role: .cancel) {}
                Button("Delete", role: .destructive) { model.delete(s.id) }
                    .accessibilityIdentifier("deleteConfirm")
            } message: { _ in
                Text("Deletes its VM and its history. This can't be undone.")
            }
    }

    private var deleteTitle: String {
        guard let s = deleting else { return "" }
        let t = model.title(s.id, s.thread.title)?.nilIfBlank ?? "New thread"
        return "Delete “\(t)”?"
    }

    private func shown(_ on: Bool) -> Binding<Bool> {
        Binding(get: { on }, set: { if !$0 { asking = nil } })
    }
}
