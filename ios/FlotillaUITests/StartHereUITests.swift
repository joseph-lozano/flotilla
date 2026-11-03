import XCTest

/// The first screen on a server with no threads: New thread itself, or the server's setup when
/// a check threads need fails. Against `flotilla serve --offline` on `dev-db.py --empty` (and a
/// copy whose uploads folder can't be written, so the database check fails). `test60FirstScreen`
/// pairs from `$FLOTILLA_LINK` and expects `$FLOTILLA_EXPECT` (`new` or `setup`); screenshots go
/// to `$FLOTILLA_SHOTS`, named by `$FLOTILLA_SHOT_NAME`.
final class StartHereUITests: FlotillaUITestCase {
    /// Whichever first screen comes: New thread's prompt, a setup check, or the old empty list.
    func waitForFirstScreen() {
        let prompt = any("newPrompt"), check = any("check-database"), empty = app.staticTexts["No threads yet"]
        let came = NSPredicate { _, _ in prompt.exists || check.exists || empty.exists }
        expectation(for: came, evaluatedWith: nil)
        waitForExpectations(timeout: 20)
        sleep(1)
    }

    /// Pairs with `$FLOTILLA_LINK`'s server, forgetting the one before.
    func pair() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        // Connected, the app asks once to send notifications: allowed, so it's out of the shot.
        allowNotifications(timeout: 4)
    }

    func test60FirstScreen() throws {
        try pair()
        waitForFirstScreen()
        shot(env["FLOTILLA_SHOT_NAME"] ?? "first-screen")
        XCTAssertFalse(app.staticTexts["No threads yet"].exists, "no empty list")
        XCTAssertTrue(app.buttons["openSettings"].exists, "the gear to Settings")
        switch env["FLOTILLA_EXPECT"] {
        case "new":
            XCTAssertTrue(any("newPrompt").exists, "New thread's prompt")
            XCTAssertTrue(app.buttons["startThread"].exists, "Start")
            XCTAssertFalse(any("check-database").exists)
        case "setup":
            XCTAssertTrue(any("check-database").exists, "the failing check, opened")
            XCTAssertTrue(app.buttons["checkAgain"].exists, "Check again")
            XCTAssertFalse(any("newPrompt").exists, "no New thread while threads can't run")
        default:
            break
        }
    }

    /// A start refused (an offline server refuses every start) comes back to the first screen
    /// with what was written and why, not a sheet over it.
    func test61RefusedStartComesBack() {
        app.launch()
        waitForFirstScreen()
        let prompt = any("newPrompt")
        prompt.tap()
        prompt.typeText("Add a health check endpoint")
        app.buttons["startThread"].tap()
        XCTAssertTrue(any("startProblem").waitForExistence(timeout: 15), "why it didn't start")
        sleep(1)
        shot(env["FLOTILLA_SHOT_NAME"] ?? "refused-start")
        XCTAssertEqual(prompt.value as? String, "Add a health check endpoint", "what was written")
        XCTAssertFalse(app.buttons["Cancel"].exists, "no New thread sheet over it")
    }

    /// With threads, the list as ever; New thread opens the setup while a check threads need
    /// fails (`dev-db.py` with its uploads folder unwritable).
    func test62NewThreadOpensTheSetup() throws {
        try pair()
        XCTAssertTrue(app.navigationBars["Threads"].waitForExistence(timeout: 20), "the threads list")
        let bar = app.buttons["newThread"]
        XCTAssertTrue(bar.waitForExistence(timeout: 10))
        bar.tap()
        XCTAssertTrue(any("check-database").waitForExistence(timeout: 10), "the failing check, opened")
        XCTAssertFalse(any("newPrompt").exists, "no New thread while threads can't run")
        sleep(1)
        shot(env["FLOTILLA_SHOT_NAME"] ?? "new-thread-setup")
        app.buttons["Done"].tap()
        XCTAssertTrue(bar.waitForExistence(timeout: 5))
    }
}
