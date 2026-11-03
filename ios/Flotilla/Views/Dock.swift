import FlotillaKit
import SwiftUI

/// The bottom of a thread: its pull request riding on the composer.
struct Dock: View {
    @Environment(AppModel.self) private var model
    let id: String
    var focus: FocusState<Bool>.Binding

    var body: some View {
        let summary = model.summary(id)
        let thread = model.details[id]?.thread ?? summary?.thread
        VStack(spacing: 0) {
            if let pr = summary?.pr {
                PRBar(pr: pr, merging: model.merging[id] != nil, reviewing: model.reviewing.contains(id), onMerge: { model.merge(id) }) { action in
                    switch action {
                    case .fix: model.ask(FixPrompts.checks(pr), in: id)
                    case .resolve: model.ask(FixPrompts.conflicts(pr), in: id)
                    case .review: model.addressReview(id)
                    }
                    focus.wrappedValue = true
                }
                Rectangle().fill(Palette.border).frame(height: 1)
            }
            ScheduledLine(thread: id)
            Composer(id: id, thread: thread, working: summary?.activity == .working, focus: focus)
        }
        .background(Palette.card)
        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).stroke(Palette.border, lineWidth: 1))
        .padding(.horizontal, 10)
        .padding(.bottom, 6)
        .padding(.top, 4)
        // Opaque, as in the mockup: the page stops at the dock, never showing under the composer.
        .background(Palette.background.ignoresSafeArea(edges: .bottom))
    }
}

/// What the PR bar can ask the agent: each fills the composer with a prompt to read over.
enum PRAction {
    case fix, resolve, review
}

/// A thread's pull request: its number and title, checks, size and state; at the right edge
/// what to do next: Fix, Resolve or Address review (they fill the composer, as on the Mac),
/// or Hold to merge when it can merge.
struct PRBar: View {
    let pr: PullRequest
    let merging: Bool
    var reviewing = false
    let onMerge: () -> Void
    var onAsk: (PRAction) -> Void = { _ in }
    @Environment(\.openURL) private var openURL
    @Environment(\.dynamicTypeSize) private var textSize

    /// What there is to ask the agent, in the desktop's order.
    private var actions: [(String, PRAction, String)] {
        var out: [(String, PRAction, String)] = []
        guard pr.open else { return out }
        if pr.failing > 0 { out.append(("Fix", .fix, "Ask the agent to fix the failing checks")) }
        if pr.conflicting { out.append(("Resolve", .resolve, "Ask the agent to resolve the conflicts")) }
        if pr.review == "CHANGES_REQUESTED" { out.append(("Address review", .review, "Ask the agent to address the review")) }
        return out
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            line
            // On a row of their own, so the pull request's line keeps its room.
            if !actions.isEmpty {
                // In a row while they fit; else one under another, each read whole.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 8) {
                        ForEach(actions, id: \.0) { a in ask(a.0, a.1, a.2) }
                    }
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(actions, id: \.0) { a in ask(a.0, a.1, a.2) }
                    }
                }
            }
        }
        .padding(.leading, 12)
        .padding(.trailing, 8)
        .padding(.vertical, 8)
        .background(Palette.muted)
    }

    private var line: some View {
        // Large text: Hold to merge under the pull request, which keeps the width to say itself.
        let layout = textSize.isAccessibilitySize ? AnyLayout(VStackLayout(alignment: .leading, spacing: 8)) : AnyLayout(HStackLayout(spacing: 10))
        return layout {
            Button {
                if let url = URL(string: pr.url) { openURL(url) }
            } label: {
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(Text("#\(pr.number)").font(Typeface.mono(.footnote)).foregroundStyle(Palette.dim)) \(Text(pr.title).font(.footnote.weight(.semibold)).foregroundStyle(Palette.text))")
                        .lineLimit(textSize.isAccessibilitySize ? 3 : 1)
                    // One text, so at large sizes it wraps instead of cutting the state off.
                    status
                        .font(Typeface.meta)
                        .lineLimit(textSize.isAccessibilitySize ? 3 : 1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityHint("Opens the pull request")
            if pr.open && (pr.canMerge || merging) {
                HoldToMerge(merging: merging, onMerge: onMerge)
            }
        }
    }

    /// "2 failing  +12  −3  Conflicts with main".
    private var status: Text {
        var parts: [Text] = []
        if let checks { parts.append(Text(checks.0).foregroundStyle(checks.1)) }
        parts.append(Text("+\(pr.additions)").foregroundStyle(Palette.ok))
        parts.append(Text("\u{2212}\(pr.deletions)").foregroundStyle(Palette.bad))
        if let state { parts.append(Text(state.0).foregroundStyle(state.1)) }
        return parts.dropFirst().reduce(parts[0]) { Text("\($0)\u{2002}\($1)") }
    }

    private func ask(_ label: String, _ action: PRAction, _ hint: String) -> some View {
        Button {
            onAsk(action)
        } label: {
            HStack(spacing: 5) {
                if action == .review && reviewing {
                    ProgressView().controlSize(.mini)
                } else {
                    Image(systemName: "sparkles").font(.caption.weight(.semibold))
                }
                Text(label)
            }
            .font(.footnote.weight(.semibold))
            .foregroundStyle(Palette.text)
            .padding(.horizontal, 11)
            .padding(.vertical, 4)
            .frame(minHeight: 32)
            .background(RoundedRectangle(cornerRadius: 6, style: .continuous).fill(Palette.card))
            .overlay(RoundedRectangle(cornerRadius: 6, style: .continuous).stroke(Palette.border, lineWidth: 1))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(action == .review && reviewing)
        .accessibilityHint(hint)
        .accessibilityIdentifier("pr-\(label)")
    }

    /// "✓ 12 checks", "2 failing", "3/5 checks", while it's open.
    private var checks: (String, Color)? {
        guard pr.open, !pr.checks.isEmpty else { return nil }
        let n = pr.checks.count
        if pr.failing > 0 { return ("\(pr.failing) failing", Palette.bad) }
        if pr.pending > 0 { return ("\(n - pr.pending)/\(n) checks", Palette.warn) }
        return ("✓ \(n) checks", Palette.ok)
    }

    /// The state in words, when it's one that needs saying.
    private var state: (String, Color)? {
        if pr.state == "MERGED" { return ("Merged", Palette.link) }
        if pr.state == "CLOSED" { return ("Closed", Palette.bad) }
        if pr.conflicting { return ("Conflicts with \(pr.base.isEmpty ? "its base" : pr.base)", Palette.bad) }
        if pr.isDraft { return ("Draft", Palette.dim) }
        if pr.review == "CHANGES_REQUESTED" { return ("Changes requested", Palette.warn) }
        if pr.review == "REVIEW_REQUIRED" { return ("Awaiting review", Palette.dim) }
        return nil
    }
}

/// Ask a follow-up, steer the running turn, or stop it; with the thread's model.
struct Composer: View {
    @Environment(AppModel.self) private var model
    let id: String
    let thread: FlotillaKit.Thread?
    let working: Bool
    var focus: FocusState<Bool>.Binding
    @Environment(\.dynamicTypeSize) private var textSize

    private var readOnly: Bool { thread?.state == .expired }
    /// Asked for from here and not written yet: nothing to send to.
    private var notYet: Bool { thread == nil && model.starting[id] != nil }
    /// Send later's sheet, with what's typed.
    @State private var later: ScheduleDraft?

    var body: some View {
        @Bindable var model = model
        let draft = Binding(get: { model.drafts[id] ?? "" }, set: { model.drafts[id] = $0 })
        let empty = draft.wrappedValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let context = model.details[id]?.context
        VStack(alignment: .leading, spacing: 10) {
            ReachedLine(limits: model.sidebar?.limits ?? [], context: context)
            PendingFiles(key: id)
            TextField(placeholder, text: draft, axis: .vertical)
                .font(.callout)
                .foregroundStyle(Palette.text)
                .lineLimit(1...6)
                .focused(focus)
                .disabled(readOnly || notYet)
                .opacity(readOnly ? 0.6 : 1)
                .accessibilityIdentifier("composer")
            HStack(spacing: 12) {
                AttachButton(key: id, disabled: readOnly || notYet).padding(.leading, -8)
                if let thread {
                    HStack(spacing: 5) {
                        ProviderLogo(provider: thread.provider)
                        Text(Format.modelName(provider: thread.provider, model: thread.model, catalog: model.catalog))
                            .foregroundStyle(Palette.text)
                        // Large text: the model alone, so it isn't cut to a word.
                        if !textSize.isAccessibilitySize {
                            if let e = thread.effort, !e.isEmpty, e != "none" { Text(e).foregroundStyle(Palette.dim) }
                            if thread.fast { Text("fast").foregroundStyle(Palette.dim) }
                        }
                    }
                    .font(.footnote)
                    .lineLimit(1)
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel("\(Format.providerName(thread.provider)), \(Format.model(thread, catalog: model.catalog))")
                }
                Spacer(minLength: 0)
                if let context { ContextRing(context: context) }
                // A scheduled prompt takes no files: those uploaded may be gone by then.
                Button {
                    // The composer's keyboard would stay up under the sheet.
                    focus.wrappedValue = false
                    later = ScheduleDraft(prompt: draft.wrappedValue, targets: [.thread(id)], to: thread?.title ?? "")
                } label: {
                    Image(systemName: "clock")
                        .font(.system(size: 16))
                        .foregroundStyle(Palette.dim)
                        .frame(width: 34, height: 34)
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .disabled(readOnly || notYet || !(model.pending[id] ?? []).isEmpty)
                .accessibilityLabel("Send later")
                .accessibilityIdentifier("sendLater")
                if working {
                    Button {
                        model.interrupt(id)
                    } label: {
                        Group {
                            if model.stopping.contains(id) {
                                ProgressView().controlSize(.small)
                            } else {
                                RoundedRectangle(cornerRadius: 2).fill(Palette.text).frame(width: 11, height: 11)
                            }
                        }
                        .frame(width: 34, height: 34)
                        .overlay(Circle().stroke(Palette.border, lineWidth: 1.5))
                        .contentShape(Circle())
                    }
                    .buttonStyle(.plain)
                    .disabled(model.stopping.contains(id))
                    .accessibilityLabel(model.stopping.contains(id) ? "Stopping the turn" : "Stop the turn")
                }
                Button {
                    model.send(draft.wrappedValue, to: id)
                } label: {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 15, weight: .bold))
                        .foregroundStyle(empty ? Palette.dim : Palette.onPrimary)
                        .frame(width: 34, height: 34)
                        .background(Circle().fill(empty ? Palette.muted : Palette.primary))
                        .contentShape(Circle())
                }
                .buttonStyle(.plain)
                .disabled(empty || readOnly || notYet)
                .accessibilityLabel(working ? "Steer" : thread?.state == .stopped ? "Resume and send" : "Send")
                .accessibilityIdentifier("send")
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 11)
        .padding(.bottom, 9)
        .sheet(item: $later) { draft in
            // Scheduled: the composer empties if it still holds what went.
            ScheduleSheet(draft: draft) { sent in
                if (model.drafts[id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines) == sent {
                    model.drafts[id] = nil
                }
            }
        }
    }

    private var placeholder: String {
        if readOnly { return "Read-only: the VM is gone" }
        return working ? "Steer…" : "Ask a follow-up…"
    }
}
