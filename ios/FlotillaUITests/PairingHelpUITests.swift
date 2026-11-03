import XCTest

/// The first screen on a phone that isn't paired: Connect to a server, in Day and Night and at
/// a large text size, and its
/// line's How to pair, which opens the iPhone docs' pairing section in the in-app Safari view.
/// Needs no server. Screenshots go to `$FLOTILLA_SHOTS`.
final class PairingHelpUITests: FlotillaUITestCase {
    func test1Shots() {
        for look in ["day", "night"] {
            app.launchArguments = ["-FlotillaForgetServer", "-appearance", look]
            app.launch()
            XCTAssertTrue(app.textFields["pairServer"].waitForExistence(timeout: 10), "Connect to a server")
            sleep(1)
            shot("pairing-\(look)")
        }
        // At Accessibility Large, the line wraps and How to pair is still there to tap.
        app.launchArguments = ["-FlotillaForgetServer", "-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityL"]
        app.launch()
        XCTAssertTrue(app.links["How to pair"].waitForExistence(timeout: 10), "How to pair, at a large text size")
        XCTAssertTrue(app.links["How to pair"].isHittable, "How to pair, on screen at a large text size")
        sleep(1)
        shot("pairing-large")
    }

    func test2HowToPair() {
        app.launchArguments = ["-FlotillaForgetServer"]
        app.launch()
        let link = app.links["How to pair"]
        XCTAssertTrue(link.waitForExistence(timeout: 10), "How to pair, in the line under the title")
        link.tap()
        let close = app.buttons["Close"]
        XCTAssertTrue(close.waitForExistence(timeout: 10), "the in-app Safari view")
        // The address the Safari view shows, and the section it opens at (the live site: it needs
        // the network).
        let address = app.buttons["URL"]
        XCTAssertTrue(address.waitForExistence(timeout: 10), "Safari's address")
        XCTAssertTrue((address.value as? String ?? "").hasSuffix("useflotilla.com"), "the docs, at useflotilla.com: \(address.value ?? "")")
        XCTAssertTrue(app.staticTexts["Pair with your server"].waitForExistence(timeout: 15), "the iPhone docs' pairing section")
        sleep(1)
        shot("pairing-docs")
        // Safari's Close is drawn by another process: a tap on the element doesn't reach it.
        close.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        XCTAssertTrue(close.waitForNonExistence(timeout: 5))
        XCTAssertTrue(app.textFields["pairServer"].exists, "back at Connect to a server")
    }
}
