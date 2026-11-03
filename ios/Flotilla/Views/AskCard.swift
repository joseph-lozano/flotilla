import FlotillaKit
import SwiftUI

/// A question the thread's agent asked you (a message of kind `ask`): waiting, its options to
/// tap (one, or several where the tool allows), Other to type your own, and Send; once answered,
/// what was asked and what you chose. As the desktop's `ask_card.rs`.
struct AskCard: View {
    @Environment(AppModel.self) private var model
    let message: Message
    let thread: String
    /// Once answered, how the message that took the answer is going, where it isn't done.
    var answer: String? = nil

    /// Per question: the options picked, in the order picked.
    @State private var picked: [[String]] = []
    /// Per question: what's typed under Other, once it's open.
    @State private var typed: [String?] = []
    @FocusState private var typing: Int?

    private var questions: [AskQuestion] { message.askQuestions }
    private var ask: String { message.toolID ?? "" }
    private var state: AskState { message.askState }
    private var sending: Bool { model.answering.contains(ask) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 6) {
                Image(systemName: "questionmark.bubble").foregroundStyle(tone)
                Text(questions.count > 1 ? "\(questions.count) questions" : "Question")
                Spacer()
                Text(statusWords).foregroundStyle(tone)
            }
            .font(Typeface.meta)
            .foregroundStyle(Palette.dim)
            ForEach(Array(questions.enumerated()), id: \.offset) { i, q in
                VStack(alignment: .leading, spacing: 6) {
                    if let h = q.header, !h.isEmpty {
                        Text(h).font(Typeface.meta.weight(.semibold)).foregroundStyle(Palette.dim)
                    }
                    MarkdownText(source: q.question)
                        .foregroundStyle(state == .passed ? Palette.dim : Palette.text)
                    switch state {
                    case .waiting: choices(q, i)
                    case .answered:
                        if let a = message.askAnswers?[safe: i] {
                            HStack(alignment: .firstTextBaseline, spacing: 6) {
                                Image(systemName: "checkmark").font(Typeface.meta).foregroundStyle(Palette.ok)
                                Text(a.joined(separator: ", ")).font(Typeface.body).foregroundStyle(Palette.text)
                            }
                            .accessibilityElement(children: .combine)
                            .accessibilityLabel("Answered: \(a.joined(separator: ", "))")
                        }
                    case .passed: EmptyView()
                    }
                }
            }
            if state == .waiting {
                Button {
                    send()
                } label: {
                    Text(sending ? "Sending…" : "Send")
                        .font(Typeface.label.weight(.semibold))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 10)
                        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(ready ? Palette.primary : Palette.muted))
                        .foregroundStyle(ready ? Palette.onPrimary : Palette.dim)
                }
                .buttonStyle(.plain)
                .disabled(!ready || sending)
                .accessibilityIdentifier("ask-send")
            }
        }
        .padding(.horizontal, 13)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(state == .waiting ? Palette.warn.opacity(0.5) : Palette.border))
        .onAppear(perform: fit)
        .onChange(of: questions.count) { fit() }
    }

    private var tone: Color { state == .waiting ? Palette.warn : Palette.dim }

    private var statusWords: String {
        switch state {
        case .waiting: "waiting"
        case .answered: answer.map { "answered · \($0)" } ?? "answered"
        case .passed: "not answered"
        }
    }

    /// One answer per question, ready to send.
    private var answers: [[String]] {
        questions.indices.map { i in
            let typed = (typed[safe: i] ?? nil)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            let picked = picked[safe: i] ?? []
            if typed.isEmpty { return picked }
            return questions[i].multi ? picked + [typed] : [typed]
        }
    }

    private var ready: Bool { Ask.tidy(questions, answers) != nil }

    private func fit() {
        if picked.count != questions.count { picked = Array(repeating: [], count: questions.count) }
        if typed.count != questions.count { typed = Array(repeating: nil, count: questions.count) }
    }

    private func send() {
        guard let tidied = Ask.tidy(questions, answers), !sending else { return }
        typing = nil
        model.answer(thread: thread, ask: ask, answers: tidied)
    }

    @ViewBuilder private func choices(_ q: AskQuestion, _ i: Int) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(q.options.enumerated()), id: \.offset) { _, o in
                let on = (picked[safe: i] ?? []).contains(o.label)
                Button {
                    pick(o.label, in: i, multi: q.multi)
                } label: {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        Image(systemName: q.multi ? (on ? "checkmark.square.fill" : "square") : (on ? "largecircle.fill.circle" : "circle"))
                            .foregroundStyle(on ? Palette.primary : Palette.dim)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(o.label).font(Typeface.body).foregroundStyle(Palette.text)
                            if let d = o.description, !d.isEmpty {
                                Text(d).font(Typeface.meta).foregroundStyle(Palette.dim)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 8)
                    .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(on ? Palette.selection : .clear))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(sending)
                .accessibilityAddTraits(on ? .isSelected : [])
                .accessibilityIdentifier("ask-option-\(i)-\(o.label)")
            }
            if let text = typed[safe: i] ?? nil {
                TextField("Your answer", text: Binding(get: { text }, set: { new in
                    typed[i] = new
                }), axis: .vertical)
                .font(Typeface.body)
                .focused($typing, equals: i)
                .submitLabel(.send)
                .onSubmit(send)
                .padding(.horizontal, 8)
                .padding(.vertical, 8)
                .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Palette.muted))
                .disabled(sending)
                .accessibilityIdentifier("ask-other-field-\(i)")
            } else {
                Button {
                    other(i, multi: q.multi)
                } label: {
                    HStack(spacing: 10) {
                        Image(systemName: "pencil").foregroundStyle(Palette.dim)
                        Text("Other").font(Typeface.body).foregroundStyle(Palette.dim)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 8)
                    .padding(.vertical, 8)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(sending)
                .accessibilityIdentifier("ask-other-\(i)")
            }
        }
    }

    private func pick(_ label: String, in i: Int, multi: Bool) {
        fit()
        if multi {
            if let at = picked[i].firstIndex(of: label) { picked[i].remove(at: at) } else { picked[i].append(label) }
        } else {
            picked[i] = [label]
            // One answer: the option, not what was typed.
            if typed[i] != nil { typed[i] = "" }
        }
    }

    private func other(_ i: Int, multi: Bool) {
        fit()
        if !multi { picked[i] = [] }
        typed[i] = ""
        typing = i
    }
}

extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}
