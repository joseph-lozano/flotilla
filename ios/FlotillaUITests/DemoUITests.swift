import XCTest

/// The demo server (`flotilla serve --demo`), as Apple's reviewer meets it: the address and the
/// standing code typed in, a thread that plays its scripted turn, a notification through the push
/// relay, a follow-up, Hold to merge, and Forget, which leaves the relay. Takes
/// `TEST_RUNNER_FLOTILLA_DEMO_SERVER` and `TEST_RUNNER_FLOTILLA_DEMO_CODE`; run in order.
final class DemoUITests: FlotillaUITestCase {
    /// The demo's thread with pull request #19, open and approved.
    static let withPR = "99422fe0-da4d-4f86-a77d-f76f7bc32202"
    let prompt = "Add a jitter option to retry"

    func test1PairWithTheStandingCode() throws {
        let server = try XCTUnwrap(env["FLOTILLA_DEMO_SERVER"])
        let code = try XCTUnwrap(env["FLOTILLA_DEMO_CODE"])
        app.launchArguments = ["-FlotillaForgetServer"]
        app.launch()
        let field = app.textFields["pairServer"]
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.tap()
        field.typeText(server)
        let codeField = app.textFields["pairCode"]
        codeField.tap()
        codeField.typeText(code)
        shot("d01-pairing")
        app.buttons["pairConnect"].tap()
        waitForThreads(Self.withPR)
        allowNotifications(timeout: 10)
        // Time to register with the relay and give the server what it got.
        sleep(5)
        shot("d02-threads")
    }

    /// A new thread in the project streams its scripted turn: the agent's words, two commands,
    /// a reply that says it's the demo; then the notification comes, through the relay, once the
    /// app is in the background, and a tap opens the thread.
    func test2NewThreadAndItsNotification() {
        launchToThreads()
        app.buttons["newThread"].tap()
        let field = any("newPrompt")
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.typeText(prompt)
        app.buttons["projectChip"].tap()
        let playground = app.buttons["flotilla-playground"]
        XCTAssertTrue(playground.waitForExistence(timeout: 5))
        playground.tap()
        sleep(1)
        shot("d03-new-thread")
        app.buttons["startThread"].tap()
        XCTAssertTrue(showing("look around the project").waitForExistence(timeout: 20), "the turn streaming")
        shot("d04-streaming")
        XCTAssertTrue(showing("this reply is scripted").waitForExistence(timeout: 30), "the reply")
        sleep(2)
        shot("d05-reply")
        // Another, then Home: its notification comes from the server through the relay.
        app.navigationBars.buttons.firstMatch.tap()
        waitForList()
        app.buttons["newThread"].tap()
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        field.typeText("Write a changelog entry for jitter")
        app.buttons["startThread"].tap()
        XCTAssertTrue(any("composer").waitForExistence(timeout: 10))
        XCUIDevice.shared.press(.home)
        let banner = springboard.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", "changelog entry")).firstMatch
        if !banner.waitForExistence(timeout: 60) {
            shot("d06-no-banner")
            dump("d06-springboard", springboard)
            XCTFail("the notification")
        }
        shot("d06-notification")
        banner.tap()
        XCTAssertTrue(any("composer").waitForExistence(timeout: 15), "the thread it's about")
        sleep(1)
        shot("d07-opened-from-notification")
    }

    /// A follow-up plays a second script.
    func test3FollowUp() {
        launchToThreads()
        let row = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'thread-' AND label CONTAINS %@", prompt)).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 10), "the thread started")
        row.tap()
        let composer = any("composer")
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText("Also cap the jitter at a second")
        app.buttons["send"].tap()
        XCTAssertTrue(showing("scripted too").waitForExistence(timeout: 30), "the follow-up's reply")
        // Opened again, with the keyboard down: the reply in sight.
        app.navigationBars.buttons.firstMatch.tap()
        waitForList()
        row.tap()
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        sleep(2)
        shot("d08-follow-up")
    }

    /// Hold to merge #19: merged, and the thread archived for it.
    func test4HoldToMerge() {
        launchToThreads(Self.withPR)
        open(Self.withPR)
        let hold = any("holdToMerge")
        XCTAssertTrue(hold.waitForExistence(timeout: 10), "Hold to merge on the PR bar")
        shot("d09-pr-bar")
        hold.press(forDuration: 1.8)
        XCTAssertTrue(showing("merged").waitForExistence(timeout: 10), "merged")
        shot("d10-merged")
    }

    /// Settings: the setup checks, green.
    func test5Settings() {
        launchToThreads()
        settings()
        sleep(2)
        shot("d11-settings")
    }

    /// Forget: the phone leaves the relay (the host checks the relay has no handle for it).
    func test9Forget() {
        launchToThreads()
        settings()
        let forget = app.buttons["forgetServer"]
        XCTAssertTrue(forget.waitForExistence(timeout: 10))
        forget.tap()
        let confirm = app.buttons["Forget"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        confirm.tap()
        XCTAssertTrue(app.textFields["pairServer"].waitForExistence(timeout: 10), "back at pairing")
        // Time to tell the relay.
        sleep(3)
    }
}
