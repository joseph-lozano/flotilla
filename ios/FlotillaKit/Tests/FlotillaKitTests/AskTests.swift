import Foundation
import Testing
@testable import FlotillaKit

/// A question an agent asked (`flotilla_proto::ask`): what the phone reads of its message, and
/// what it sends back.
@Suite struct AskTests {
    /// An ask message as the server writes it (`flotilla_core::normalize`).
    func message(status: String, answers: String? = nil) throws -> Message {
        var j: [String: JSON] = [
            "seq": 1, "kind": "ask", "status": .string(status), "tool_id": "call_1", "text": "Which file name should I create?\nWhich checks?",
            "tool_input_json": #"[{"question":"Which file name should I create?","options":[{"label":"alpha.txt (recommended)"},{"label":"beta.txt","description":"The other one."}]},{"header":"Checks","question":"Which checks?","options":[{"label":"lint"},{"label":"test"}],"multi":true}]"#,
        ]
        if let answers { j["tool_result_json"] = .string(answers) }
        return try Message(json: .object(j))
    }

    @Test func `its questions, options in order, and where it stands`() throws {
        let m = try message(status: "waiting")
        #expect(m.isAsk && m.askState == .waiting)
        let qs = m.askQuestions
        #expect(qs.count == 2)
        #expect(qs[0].options.map(\.label) == ["alpha.txt (recommended)", "beta.txt"])
        #expect(qs[0].options[1].description == "The other one." && !qs[0].multi)
        #expect(qs[1].header == "Checks" && qs[1].multi)
        #expect(m.askAnswers == nil)
        let answered = try message(status: "answered", answers: #"[["beta.txt"],["lint","test"]]"#)
        #expect(answered.askState == .answered && answered.askAnswers == [["beta.txt"], ["lint", "test"]])
        #expect(try message(status: "passed").askState == .passed)
        // Not an ask, or questions that don't read: none.
        #expect(Message(seq: 2, kind: "text", text: "hi").askQuestions.isEmpty)
    }

    /// The message that took an answer, as the server writes it (`flotilla_proto::ask`'s test of
    /// the same): which question's; none on any other.
    @Test func `the message that took an answer says whose`() throws {
        let wire = #"{"attachments":[],"answer_to":"call_1","event_key":null,"is_error":false,"kind":"prompt","model":null,"pending":false,"seq":4,"status":"running","task_id":"pr-2","text":"Which file name should I create?\nbeta.txt","timestamp":null,"tool_id":null,"tool_input_json":null,"tool_name":null,"tool_result_json":null,"tool_result_type":null,"tool_summary":null}"#
        let m = try Message(json: try JSON(parsing: Data(wire.utf8)))
        #expect(m.answerTo == "call_1" && m.kind == "prompt" && m.status == "running")
        #expect(try message(status: "answered").answerTo == nil)
    }

    @Test func `an answer per question, one where only one can be picked`() throws {
        let qs = try message(status: "waiting").askQuestions
        #expect(Ask.tidy(qs, [[" beta.txt "], ["lint", "", "test"]]) == [["beta.txt"], ["lint", "test"]])
        #expect(Ask.tidy(qs, [["beta.txt"], []]) == nil)
        #expect(Ask.tidy(qs, [["alpha.txt (recommended)", "beta.txt"], ["lint"]]) == nil)
        #expect(Ask.tidy(qs, [["beta.txt"]]) == nil)
    }

    @Test func `the answer goes as Rust's thread.answer, safe to send again`() {
        let req = Request.answer(thread: "t1", ask: "call_1", answers: [["beta.txt"], ["lint", "test"]], local: "local-2")
        #expect(req.resendable)
        #expect(Up.req(id: 7, req).json.text == #"{"id":7,"m":"thread.answer","p":{"answers":[["beta.txt"],["lint","test"]],"ask":"call_1","local":"local-2","thread":"t1"},"t":"req"}"#)
    }
}
