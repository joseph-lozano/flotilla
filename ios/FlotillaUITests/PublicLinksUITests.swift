import XCTest

/// Public links, against `flotilla serve --offline` on `ios/script/dev-db.py --project --services
/// --public`: the median thread's `web` is public for the next hour and its port 5173 (no
/// service) until stopped. The header's globe is an open lock; the sheet says how long each is
/// public, with Copy link and Make private; Make public… on a service and in the port field
/// offers 1 hour, 1 day, 1 week or Until I stop it, said as public to anyone with the link; an
/// offline server refuses both. `test80Pair` pairs from `$FLOTILLA_LINK`;
/// `TEST_RUNNER_FLOTILLA_APPEARANCE` (day or night) sets the app's Appearance.
final class PublicLinksUITests: FlotillaUITestCase {
    /// "Median function with subtle bug": `web` public for an hour, port 5173 until stopped.
    let services = DevDB.median
    /// "Planting bugs and getting reviews": up, no services, nothing public.
    let plain = DevDB.slug

    override func setUp() {
        super.setUp()
        if let look = env["FLOTILLA_APPEARANCE"] { app.launchArguments += ["-appearance", look] }
    }

    func test80Pair() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForList()
        allowNotifications(timeout: 5)
        waitForThreads(services)
    }

    func openPreview(_ thread: String) -> XCUIElement {
        launchToThreads(thread)
        open(thread)
        let preview = app.buttons["preview"]
        XCTAssertTrue(preview.waitForExistence(timeout: 10), "Preview in the header")
        return preview
    }

    /// The sheet: `web` public with time left, port 5173 public until stopped, Make public… on
    /// `api` (it has a link) but not on `web` (public already) or `worker` (no link). Copy link
    /// says it copied.
    func test81Public() {
        let preview = openPreview(services)
        XCTAssertEqual(preview.label, "Preview, public", "the header says something is public")
        preview.tap()
        XCTAssertTrue(any("servicesSheet").waitForExistence(timeout: 5))
        let web = any("public-web")
        XCTAssertTrue(web.waitForExistence(timeout: 5))
        XCTAssertTrue(web.label.hasPrefix("Public · ") && web.label.hasSuffix(" left"), web.label)
        XCTAssertEqual(any("public-port-5173").label, "Public until you stop it")
        XCTAssertTrue(app.buttons["public-copy-web"].exists && app.buttons["public-end-web"].exists)
        XCTAssertTrue(app.buttons["public-open-port-5173"].exists && app.buttons["public-end-port-5173"].exists)
        XCTAssertTrue(app.buttons["service-public-api"].exists, "api has a link: it can be made public")
        XCTAssertFalse(app.buttons["service-public-web"].exists, "public already")
        XCTAssertFalse(app.buttons["service-public-worker"].exists, "no link")
        sleep(1)
        shot("public-1-sheet")
        app.buttons["public-copy-web"].tap()
        XCTAssertTrue(app.buttons["public-copy-web"].label.contains("Copied"), app.buttons["public-copy-web"].label)
    }

    /// Make public… on a service: the four choices, said as public to anyone with the link. An
    /// offline server refuses; so it does Make private.
    func test82MakePublic() {
        openPreview(services).tap()
        let make = app.buttons["service-public-api"]
        XCTAssertTrue(make.waitForExistence(timeout: 5))
        make.tap()
        let choices = app.sheets.firstMatch
        XCTAssertTrue(choices.waitForExistence(timeout: 5), "the choices")
        XCTAssertTrue(showing("Make api public").exists)
        XCTAssertTrue(showing("Anyone with the link can open it, without signing in.").exists)
        for label in ["1 hour", "1 day", "1 week", "Until I stop it"] {
            XCTAssertTrue(app.buttons[label].firstMatch.exists, label)
        }
        sleep(1)
        shot("public-2-choices")
        app.buttons["1 day"].firstMatch.tap()
        let refused = app.alerts.firstMatch
        XCTAssertTrue(refused.waitForExistence(timeout: 10), "an offline server refuses it")
        XCTAssertTrue(refused.label.contains("Couldn't make api public"), refused.label)
        refused.buttons["OK"].tap()
        app.buttons["public-end-web"].tap()
        XCTAssertTrue(refused.waitForExistence(timeout: 10))
        XCTAssertTrue(refused.label.contains("Couldn't make web private"), refused.label)
        refused.buttons["OK"].tap()
    }

    /// Without services or public links: the port field, with Make public… beside Open.
    func test83PortField() {
        let preview = openPreview(plain)
        XCTAssertEqual(preview.label, "Preview")
        preview.tap()
        let alert = app.alerts["Open a preview"]
        XCTAssertTrue(alert.waitForExistence(timeout: 5), "no services: the port field")
        XCTAssertTrue(alert.buttons["Make public…"].exists)
        alert.buttons["Make public…"].tap()
        XCTAssertTrue(app.sheets.firstMatch.waitForExistence(timeout: 5), "the choices")
        XCTAssertTrue(showing("Make port 3000 public").exists)
        sleep(1)
        shot("public-3-port-choices")
        app.buttons["Until I stop it"].firstMatch.tap()
        let refused = app.alerts.firstMatch
        XCTAssertTrue(refused.waitForExistence(timeout: 10))
        XCTAssertTrue(refused.label.contains("Couldn't make port 3000 public"), refused.label)
        refused.buttons["OK"].tap()
    }
}
