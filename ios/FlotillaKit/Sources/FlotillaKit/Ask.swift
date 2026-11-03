import Foundation

/// A question an agent asked you with its harness's ask tool, as its thread shows it: a message
/// of kind `ask` (`flotilla_proto::ask`). Its questions are its `tool_input_json`, your answers,
/// once given, its `tool_result_json` (a list of strings per question), and its status says where
/// it stands.
public struct AskQuestion: Sendable, Hashable {
    /// A short label above the question, where the tool gives one.
    public var header: String?
    public var question: String
    /// In the tool's order: the recommended one first.
    public var options: [AskChoice]
    /// Several options can be picked.
    public var multi: Bool

    public init(header: String? = nil, question: String, options: [AskChoice], multi: Bool = false) {
        self.header = header
        self.question = question
        self.options = options
        self.multi = multi
    }

    init?(json j: JSON) {
        guard let q = j["question"]?.string else { return nil }
        header = j["header"]?.string
        question = q
        options = (j["options"]?.array ?? []).compactMap { o in o["label"]?.string.map { AskChoice(label: $0, description: o["description"]?.string) } }
        multi = j["multi"]?.bool ?? false
    }
}

public struct AskChoice: Sendable, Hashable {
    public var label: String
    public var description: String?

    public init(label: String, description: String? = nil) {
        self.label = label
        self.description = description
    }
}

public enum AskState: String, Sendable {
    /// Not answered yet, and nothing was sent in the thread since.
    case waiting
    case answered
    /// The thread moved on without an answer.
    case passed
}

extension Message {
    public var isAsk: Bool { kind == "ask" }
    public var askState: AskState { AskState(rawValue: status ?? "") ?? .waiting }

    public var askQuestions: [AskQuestion] {
        guard isAsk, let text = toolInputJSON, let j = try? JSON(parsing: text) else { return [] }
        return (j.array ?? []).compactMap { AskQuestion(json: $0) }
    }

    /// Your answers, once given: one list per question.
    public var askAnswers: [[String]]? {
        guard isAsk, let text = toolResultJSON, let j = try? JSON(parsing: text), let a = j.array else { return nil }
        return a.map { ($0.array ?? []).compactMap(\.string) }
    }
}

public enum Ask {
    /// Each question's answer as it's sent: blanks dropped, trimmed. Nil while a question has
    /// none, or a single-choice one has more than one (`flotilla_proto::ask::tidy`).
    public static func tidy(_ questions: [AskQuestion], _ answers: [[String]]) -> [[String]]? {
        guard answers.count == questions.count else { return nil }
        var out: [[String]] = []
        for (q, a) in zip(questions, answers) {
            let a = a.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
            if a.isEmpty || (a.count > 1 && !q.multi) { return nil }
            out.append(a)
        }
        return out
    }
}
