import XCTest

/// A question the agent waits on (its options, Other, Send) and one answered, against
/// `flotilla serve --offline` on `ios/script/dev-db.py --ask` (`test51…`), then on another
/// server on `--ask answered` (`test53Answered`, after pairing with it). `test50Pair` pairs from
/// `$FLOTILLA_LINK`. `test56Live` answers a live thread's question (`$FLOTILLA_THREAD`) on a
/// server that runs it.
final class AskUITests: FlotillaUITestCase {
    func openQuestion() {
        open(DevDB.ask)
        XCTAssertTrue(app.staticTexts["Which file name should I create?"].waitForExistence(timeout: 10), "the question")
    }

    func test50Pair() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForList()
        allowNotifications(timeout: 5)
        waitForThreads(DevDB.ask)
    }

    /// Waiting on you: under Unread in the list; in the thread, its options in the tool's order,
    /// Send only once each question has an answer, Other to type one; offline, Send is refused
    /// and the answer stays to send again.
    func test51Waiting() {
        launchToThreads(DevDB.ask)
        let row = any("thread-\(DevDB.ask)")
        XCTAssertTrue(row.label.contains("Unread"), "a question waiting counts as unread: \(row.label)")
        sleep(1)
        shot("ask-0-list")
        openQuestion()
        let send = app.buttons["ask-send"]
        XCTAssertTrue(send.waitForExistence(timeout: 5))
        XCTAssertFalse(send.isEnabled, "nothing picked yet")
        XCTAssertTrue(app.buttons["ask-option-0-alpha.txt (recommended)"].exists, "the recommended one")
        sleep(1)
        shot("ask-1-waiting")
        app.buttons["ask-option-0-beta.txt"].tap()
        XCTAssertTrue(send.isEnabled, "one picked")
        sleep(1)
        shot("ask-2-picked")
        app.buttons["ask-other-0"].tap()
        let field = any("ask-other-field-0")
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        XCTAssertFalse(send.isEnabled, "Other, nothing typed yet: the pick went")
        field.typeText("gamma.txt")
        XCTAssertTrue(send.isEnabled)
        sleep(1)
        shot("ask-3-other")
        send.tap()
        let refused = app.alerts.firstMatch
        XCTAssertTrue(refused.waitForExistence(timeout: 10), "an offline server refuses it")
        XCTAssertTrue(refused.label.contains("Couldn't send the answer"), refused.label)
        sleep(1)
        shot("ask-4-refused")
        refused.buttons.firstMatch.tap()
        XCTAssertTrue(send.waitForExistence(timeout: 5))
        expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: send)
        waitForExpectations(timeout: 5)
        XCTAssertEqual(field.value as? String, "gamma.txt", "what was typed stays to send again")
    }

    /// Answered: what was asked, and what was chosen, then the agent's turn on it, without the
    /// answer said again in a bubble of yours under the card.
    func test53Answered() {
        launchToThreads(DevDB.ask)
        openQuestion()
        XCTAssertTrue(any("Answered: beta.txt").waitForExistence(timeout: 5), "what was chosen")
        XCTAssertFalse(app.buttons["ask-send"].exists, "nothing to send any more")
        XCTAssertTrue(showing("Created `beta.txt`").waitForExistence(timeout: 5) || showing("Created beta.txt").exists, "the agent's turn on it")
        XCTAssertFalse(saidAgain(), "the answer said again under its card")
        sleep(1)
        shot("ask-5-answered")
    }

    /// A bubble of yours saying the question's answer again ("You, …: Which file name…").
    func saidAgain(_ question: String = "Which file name should I create?") -> Bool {
        app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'You' AND label CONTAINS %@", question)).firstMatch.exists
    }

    /// A live thread's question (`$FLOTILLA_THREAD`, its question `$FLOTILLA_QUESTION`): waiting,
    /// then answered with `$FLOTILLA_ANSWER` (an option's label), with no bubble of yours saying
    /// it again. Screenshots `ask-live-…`.
    func test56Live() throws {
        let (thread, question, answer) = try live()
        launchToThreads(thread)
        open(thread)
        XCTAssertTrue(showing(question).waitForExistence(timeout: 15), "the question")
        let option = app.buttons["ask-option-0-\(answer)"]
        XCTAssertTrue(option.waitForExistence(timeout: 10), "its option \(answer)")
        sleep(2)
        shot("ask-live-1-waiting")
        option.tap()
        app.buttons["ask-send"].tap()
        XCTAssertTrue(any("Answered: \(answer)").waitForExistence(timeout: 15), "answered")
        XCTAssertFalse(saidAgain(question), "the answer said again under its card")
        sleep(2)
        shot("ask-live-2-answered")
    }

    /// The same thread again, once the agent's turn on the answer is over: still no bubble of
    /// yours under the card.
    func test57LiveAnswered() throws {
        let (thread, question, answer) = try live()
        launchToThreads(thread)
        open(thread)
        XCTAssertTrue(any("Answered: \(answer)").waitForExistence(timeout: 15), "answered")
        XCTAssertFalse(saidAgain(question), "the answer said again under its card")
        sleep(2)
        shot("ask-live-3-replied")
    }

    func live() throws -> (String, String, String) {
        (try XCTUnwrap(env["FLOTILLA_THREAD"], "set TEST_RUNNER_FLOTILLA_THREAD"), try XCTUnwrap(env["FLOTILLA_QUESTION"]), try XCTUnwrap(env["FLOTILLA_ANSWER"]))
    }
}
