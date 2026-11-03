import XCTest

/// The threads list, a thread, the composer and Hold to merge, against `flotilla serve
/// --offline` on `ios/script/dev-db.py --pr --states`, after `test1Pairing`; and the live runs.
final class FlotillaUITests: FlotillaUITestCase {
    func test1Pairing() throws {
        try startPairing()
        shot("01-pairing")
        app.buttons["pairConnect"].tap()
        waitForThreads()
        sleep(1)
        shot("02-threads-after-pairing")
    }

    /// A launch whose Keychain doesn't answer (it can fail for a moment) keeps the pairing: it
    /// says why, and the next launch is on the server again. Before, it forgot the server.
    func test1bKeychainHiccupKeepsThePairing() {
        app.launchArguments = ["-FlotillaKeychainFails"]
        app.launch()
        let why = app.staticTexts["Couldn't read this phone's pairing from the Keychain. Open Flotilla again to try again."]
        XCTAssertTrue(why.waitForExistence(timeout: 10), "why, on Threads")
        XCTAssertFalse(app.textFields["pairServer"].exists, "not back at pairing")
        sleep(1)
        shot("01b-keychain-hiccup")
        app.terminate()
        app.launchArguments = []
        launchToThreads()
    }

    /// Pair again, with the same server, from a launch whose Keychain didn't answer: the old
    /// device is revoked there with the new token (the host checks the server's devices).
    /// `FLOTILLA_LINK_AGAIN`: a second link from `flotilla pair` (a code works once).
    func test1cPairAgainAfterKeychainHiccup() throws {
        let link = try XCTUnwrap(env["FLOTILLA_LINK_AGAIN"], "set TEST_RUNNER_FLOTILLA_LINK_AGAIN to a second link")
        app.launchArguments = ["-FlotillaKeychainFails"]
        app.launch()
        XCTAssertTrue(app.buttons["Pair again"].waitForExistence(timeout: 10))
        try openPairingLink(link)
        app.buttons["pairConnect"].tap()
        waitForThreads()
        sleep(5)
    }

    func test2Threads() {
        launchToThreads()
        sleep(1)
        shot(env["FLOTILLA_SHOT_NAME"] ?? "03-threads")
    }

    func test3Thread() {
        launchToThreads()
        open(DevDB.slug)
        sleep(2)
        shot("05-thread-bottom")
        let answered = app.buttons["2 agents answered"]
        XCTAssertTrue(answered.waitForExistence(timeout: 10), "the agents-answered line")
        app.swipeDown(velocity: .fast)
        app.swipeDown(velocity: .fast)
        app.swipeDown(velocity: .fast)
        sleep(1)
        shot("06-thread-top-cards")
        answered.tap()
        sleep(2)
        shot("07-thread-card-opened")
    }

    func test4Keyboard() {
        launchToThreads()
        open(DevDB.slug)
        let composer = app.descendants(matching: .any)["composer"]
        composer.tap()
        composer.typeText("Does the fix handle an empty title?")
        sleep(1)
        shot("08-composer-keyboard")
    }

    /// A tap never merges; a press let go early drains; holding to the end merges.
    func test5HoldToMerge() {
        launchToThreads()
        open(DevDB.median)
        let hold = app.descendants(matching: .any)["holdToMerge"]
        XCTAssertTrue(hold.waitForExistence(timeout: 10), "Hold to merge on the PR bar")
        shot("09-pr-bar")
        hold.tap()
        sleep(2)
        XCTAssertFalse(app.staticTexts["Merging…"].exists, "a tap mustn't merge")
        // Held part way, a few times, while the host takes screenshots.
        for _ in 0..<4 {
            hold.press(forDuration: 0.9)
            usleep(400_000)
        }
        XCTAssertFalse(app.staticTexts["Merging…"].exists, "a press let go early mustn't merge")
        // Pressed, then slid up onto the messages and held there past the hold's end: backing
        // out of the press never merges.
        let away = app.windows.firstMatch.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.45))
        hold.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
            .press(forDuration: 0.2, thenDragTo: away, withVelocity: .fast, thenHoldForDuration: 1.6)
        // A merge asked for would show as "Merging…", or as an alert at once (an offline server
        // refuses it).
        XCTAssertFalse(app.staticTexts["Merging…"].waitForExistence(timeout: 1), "a press that slides off mustn't merge")
        XCTAssertFalse(app.alerts.firstMatch.exists, "a press that slides off mustn't ask to merge")
        XCTAssertTrue(hold.exists, "Hold to merge, still there to hold")
        if env["FLOTILLA_MERGE"] == "1" {
            hold.press(forDuration: 1.6)
            XCTAssertTrue(app.staticTexts["Merging…"].waitForExistence(timeout: 3), "holding to the end merges")
            shot("11-merging")
        }
    }

    /// A look at `FLOTILLA_THREAD`: its end, then its start.
    func test7Look() throws {
        let thread = try XCTUnwrap(env["FLOTILLA_THREAD"])
        launchToThreads()
        open(thread)
        sleep(2)
        shot("look-bottom")
        for _ in 0..<4 { app.swipeDown(velocity: .fast) }
        sleep(1)
        shot("look-top")
    }

    /// For the live run: start a long turn on `FLOTILLA_THREAD`, steer it, then stop it.
    func test8LiveSteerAndStop() throws {
        let thread = try XCTUnwrap(env["FLOTILLA_THREAD"])
        app.launch()
        waitForList()
        open(thread)
        let composer = app.descendants(matching: .any)["composer"]
        composer.tap()
        composer.typeText("Run `sleep 45` in bash, then reply DONE.")
        app.buttons["send"].tap()
        XCTAssertTrue(app.buttons["Stop the turn"].waitForExistence(timeout: 20), "the turn is running")
        sleep(6)
        composer.tap()
        composer.typeText("When you reply, add the word BANANA.")
        XCTAssertTrue(app.buttons["Steer"].exists, "sending while it runs steers")
        app.buttons["Steer"].tap()
        sleep(4)
        shot("30-live-steered")
        app.buttons["Stop the turn"].tap()
        let stopped = app.buttons["Stop the turn"]
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: stopped)
        waitForExpectations(timeout: 40)
        sleep(2)
        shot("31-live-stopped")
    }

    /// For the live run: send a follow-up to `FLOTILLA_THREAD` and watch the reply stream in.
    func test6LiveFollowUp() throws {
        let thread = try XCTUnwrap(env["FLOTILLA_THREAD"])
        let text = env["FLOTILLA_TEXT"] ?? "Reply with the word PLUM and nothing else."
        app.launch()
        waitForList()
        let row = app.descendants(matching: .any)["thread-\(thread)"]
        XCTAssertTrue(row.waitForExistence(timeout: 30))
        shot("20-live-threads")
        row.tap()
        let composer = app.descendants(matching: .any)["composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText(text)
        app.buttons["send"].tap()
        // Out of the way of the reply.
        app.buttons["send"].swipeDown()
        for i in 0..<45 {
            usleep(1_000_000)
            shot(String(format: "21-live-%02d", i))
        }
    }
}
