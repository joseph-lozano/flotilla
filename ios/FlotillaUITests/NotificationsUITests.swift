import XCTest

/// Settings → Notifications in each state iOS can be in (a debug build's
/// `-FlotillaNotifications allowed|denied|notAsked`), in Day and Night. Against `flotilla serve
/// --offline` on `ios/script/dev-db.py`; `test1Pair` pairs from `$FLOTILLA_LINK` first.
/// Screenshots go to `$FLOTILLA_SHOTS`.
final class NotificationsUITests: FlotillaUITestCase {
    func test1Pair() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForThreads()
        allowNotifications(timeout: 5)
    }

    func test2EachState() {
        for look in ["day", "night"] {
            for (state, row) in [("allowed", "notificationsOn"), ("denied", "notificationsOff"), ("notAsked", "askNotifications")] {
                app.launchArguments = ["-appearance", look, "-FlotillaNotifications", state]
                launchToThreads()
                settings()
                let shown = any(row)
                // To the end of Settings: Notifications is last.
                for _ in 0..<5 where !shown.isHittable { app.swipeUp() }
                app.swipeUp()
                XCTAssertTrue(shown.waitForExistence(timeout: 5), "\(state), \(look): \(row)")
                XCTAssertEqual(app.buttons["openNotificationSettings"].exists, state == "denied", "Open Settings only when they're off")
                shot("notifications-\(state)-\(look)")
                app.terminate()
            }
        }
    }
}
