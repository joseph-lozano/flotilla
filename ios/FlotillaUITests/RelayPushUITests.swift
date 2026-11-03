import XCTest

/// Notifications through the push relay (relay/README.md), on the simulator: the app registers
/// with a relay on this Mac (`relay/script/local`) and gives the server what it got; a note the
/// server sends through the relay arrives as APNs would deliver it (the fake APNs' copy, pushed
/// through APNs' sandbox, or `xcrun simctl push`), and the extension opens it into its real
/// words. See ios/README.md for the run: `test1Pair`, then the host pushes once each test writes
/// `$TEST_RUNNER_FLOTILLA_PUSH_READY`.
final class RelayPushUITests: FlotillaUITestCase {
    /// The relay, and a token for the simulator (APNs gives it none).
    var relayArguments: [String] {
        ["-FlotillaPushRelay", env["FLOTILLA_RELAY"] ?? "http://127.0.0.1:8796", "-FlotillaPushToken", "f1".padding(toLength: 64, withPad: "a1", startingAt: 0)]
    }

    /// Paired from `$FLOTILLA_LINK`, with notifications allowed: the app registers with the relay.
    func test1Pair() throws {
        let link = try pairingLink()
        app.launchArguments = ["-FlotillaForgetServer"] + relayArguments
        app.launch()
        try openPairingLink(link)
        app.buttons["pairConnect"].tap()
        waitForThreads()
        allowNotifications(timeout: 10)
        // Time to register with the relay and give the server what it got.
        sleep(5)
        shot("r1-paired")
    }

    /// What the server sent through the relay shows with its real words, and a tap opens it.
    func test2RelayedPushOpensItsThread() throws {
        let ready = try XCTUnwrap(env["FLOTILLA_PUSH_READY"])
        app.launchArguments = relayArguments
        app.launch()
        waitForThreads()
        XCUIDevice.shared.press(.home)
        sleep(1)
        try "ready".write(toFile: ready, atomically: true, encoding: .utf8)
        let words = springboard.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", "Median function with subtle bug")).firstMatch
        if !words.waitForExistence(timeout: 30) {
            shot("r2-no-banner")
            dump("r2-springboard", springboard)
            XCTFail("the notification, in its real words")
        }
        XCTAssertFalse(springboard.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", "New activity")).firstMatch.exists)
        shot("r2-relayed-banner")
        words.tap()
        if !any("composer").waitForExistence(timeout: 15) {
            shot("r2-after-tap")
            dump("r2-after-tap")
            XCTFail("the thread it's about")
        }
        XCTAssertTrue(app.staticTexts["Median function with subtle bug"].waitForExistence(timeout: 10))
        sleep(2)
        shot("r3-relayed-opened-thread")
    }

    /// One the phone can't open (sealed with another key) shows as the relay sent it.
    func test3UnopenedPushIsNeutral() throws {
        let ready = try XCTUnwrap(env["FLOTILLA_PUSH_READY"])
        app.launchArguments = relayArguments
        app.launch()
        waitForThreads()
        XCUIDevice.shared.press(.home)
        sleep(1)
        try "ready".write(toFile: ready, atomically: true, encoding: .utf8)
        let neutral = springboard.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", "New activity")).firstMatch
        if !neutral.waitForExistence(timeout: 30) {
            shot("r4-no-banner")
            dump("r4-springboard", springboard)
            XCTFail("the neutral alert")
        }
        shot("r4-neutral-banner")
        neutral.tap()
        XCTAssertTrue(app.navigationBars["Threads"].waitForExistence(timeout: 15), "the app, on its threads")
        XCTAssertFalse(any("composer").waitForExistence(timeout: 3), "no thread opened")
        shot("r5-neutral-tapped")
    }

    /// Forget: the phone asks the relay to forget its handle (the host checks the relay has none).
    func test4ForgetLeavesTheRelay() {
        app.launchArguments = relayArguments
        app.launch()
        waitForThreads()
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

    /// Launched on a server that no longer sends through the relay (the host turned it off and
    /// restarted it): the phone leaves the relay (the host checks it has no handle, and the
    /// server no relay registration).
    func test5LeavesWhenTheServerDoesnt() {
        app.launchArguments = relayArguments
        app.launch()
        waitForThreads()
        sleep(5)
    }
}
