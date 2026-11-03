import XCTest

/// Preview, against `flotilla serve --offline` on `ios/script/dev-db.py --project --services`:
/// the services sheet (each service's state, Open, links, Output, Restart / Start / Stop),
/// Output, Open in the in-app Safari view and Done back to the thread, the file's problem, and
/// the port field (Another port… and a thread without services), with what shows while the URL
/// comes (`-FlotillaSlowPreview`: an offline server refuses at once) and the refusal.
/// `test70Pair` pairs from `$FLOTILLA_LINK`; `TEST_RUNNER_FLOTILLA_APPEARANCE` (day or night)
/// sets the app's Appearance.
final class PreviewUITests: FlotillaUITestCase {
    /// "Median function with subtle bug": `web` ready, `worker` stopped, `api` exited.
    let services = DevDB.median
    /// "Check system via subagent": a services file that doesn't check out.
    let broken = DevDB.check
    /// "Planting bugs and getting reviews": up, no services.
    let plain = DevDB.slug
    /// "Subagents run uname, df, ls, whoami": its VM stopped.
    let stopped = DevDB.uname

    override func setUp() {
        super.setUp()
        if let look = env["FLOTILLA_APPEARANCE"] { app.launchArguments += ["-appearance", look] }
        app.launchArguments += ["-FlotillaSlowPreview", "2500"]
    }

    func test70Pair() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForList()
        allowNotifications(timeout: 5)
        waitForThreads(services)
    }

    func openPreview(_ thread: String) {
        launchToThreads(thread)
        open(thread)
        let preview = app.buttons["preview"]
        XCTAssertTrue(preview.waitForExistence(timeout: 10), "Preview in the header")
        preview.tap()
    }

    /// Each service: its state, Open while it runs, its other links, Output, Restart or Start,
    /// and Stop while it runs. Output is the end of what it printed; a refused Restart says so
    /// over the sheet. Open opens the in-app Safari view, and Done comes back to the thread.
    func test71Services() {
        openPreview(services)
        let sheet = any("servicesSheet")
        XCTAssertTrue(sheet.waitForExistence(timeout: 5), "with services, Preview is their sheet")
        XCTAssertTrue(any("service-web").label.contains("ready · :4100"), any("service-web").label)
        XCTAssertTrue(any("service-worker").label.contains("stopped"))
        XCTAssertTrue(any("service-api").label.contains("exited 1"))
        XCTAssertTrue(app.buttons["service-open-web"].isEnabled)
        XCTAssertEqual(app.buttons["service-link-web-0"].label, "Storybook", "its second link, by its title")
        XCTAssertTrue(app.buttons["service-stop-web"].exists && app.buttons["service-restart-web"].exists)
        XCTAssertTrue(app.buttons["service-start-worker"].exists, "one you stopped starts")
        XCTAssertFalse(app.buttons["service-stop-worker"].exists || app.buttons["service-restart-worker"].exists)
        XCTAssertFalse(app.buttons["service-open-worker"].exists, "no portal, nothing to open")
        XCTAssertFalse(app.buttons["service-open-api"].isEnabled, "not running: Open waits")
        XCTAssertFalse(app.buttons["service-stop-api"].exists)
        XCTAssertTrue(app.buttons["previewOtherPort"].exists)
        sleep(1)
        shot("preview-1-services")

        app.buttons["service-output-api"].tap()
        let output = any("serviceOutput")
        XCTAssertTrue(output.waitForExistence(timeout: 5), "Output")
        XCTAssertTrue(app.navigationBars["api · output"].exists)
        XCTAssertTrue(showing("TypeError: Cannot read properties of undefined").exists, "the end of what it printed")
        sleep(1)
        shot("preview-2-output")
        app.buttons["service-output-restart"].tap()
        let refused = app.alerts.firstMatch
        XCTAssertTrue(refused.waitForExistence(timeout: 10), "an offline server refuses it")
        XCTAssertTrue(refused.label.contains("Couldn't restart api"), refused.label)
        sleep(1)
        shot("preview-3-restart-refused")
        refused.buttons["OK"].tap()
        app.navigationBars["api · output"].buttons.firstMatch.tap()
        XCTAssertTrue(app.buttons["service-output-worker"].waitForExistence(timeout: 5))
        app.buttons["service-output-worker"].tap()
        XCTAssertTrue(showing("Nothing printed yet.").waitForExistence(timeout: 5))
        XCTAssertEqual(app.buttons["service-output-restart"].label, "Start")
        app.navigationBars["worker · output"].buttons.firstMatch.tap()

        XCTAssertTrue(app.buttons["service-open-web"].waitForExistence(timeout: 5))
        app.buttons["service-open-web"].tap()
        XCTAssertTrue(sheet.waitForNonExistence(timeout: 5), "the sheet goes")
        let done = app.buttons["Close"]
        XCTAssertTrue(done.waitForExistence(timeout: 10), "the in-app Safari view")
        sleep(2)
        shot("preview-4-in-app-safari")
        // Safari's Done is drawn by another process: a tap on the element doesn't reach it.
        done.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        XCTAssertTrue(done.waitForNonExistence(timeout: 5))
        XCTAssertTrue(any("composer").waitForExistence(timeout: 5), "back on the thread")
        XCTAssertTrue(app.buttons["preview"].isHittable)
    }

    /// Another port… leaves the sheet for the port field.
    func test72AnotherPort() {
        openPreview(services)
        XCTAssertTrue(app.buttons["previewOtherPort"].waitForExistence(timeout: 5))
        app.buttons["previewOtherPort"].tap()
        let alert = app.alerts["Open a preview"]
        XCTAssertTrue(alert.waitForExistence(timeout: 5), "the port field")
        XCTAssertFalse(any("servicesSheet").exists)
        XCTAssertEqual(alert.textFields.firstMatch.value as? String, "3000")
        alert.buttons["Cancel"].tap()
    }

    /// Without services: the port field, at once. Open says it's coming, then the refusal.
    func test73PortField() {
        openPreview(plain)
        let alert = app.alerts["Open a preview"]
        XCTAssertTrue(alert.waitForExistence(timeout: 5), "no services: the port field")
        let field = alert.textFields.firstMatch
        XCTAssertEqual(field.value as? String, "3000")
        sleep(1)
        shot("preview-5-port-field")
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: 4))
        XCTAssertFalse(alert.buttons["Open"].isEnabled, "no port, no Open")
        field.typeText("8080")
        alert.buttons["Open"].tap()
        let notice = any("previewNotice")
        XCTAssertTrue(notice.waitForExistence(timeout: 5), "while the URL comes")
        XCTAssertEqual(notice.label, "Opening port 8080…")
        shot("preview-6-opening")
        let refused = app.alerts.firstMatch
        XCTAssertTrue(refused.waitForExistence(timeout: 10), "an offline server refuses it")
        XCTAssertTrue(refused.label.contains("Couldn't open the preview"), refused.label)
        XCTAssertFalse(notice.exists)
        sleep(1)
        shot("preview-7-refused")
        refused.buttons.firstMatch.tap()
    }

    /// A stopped VM: plain Preview, which says it wakes the VM first.
    func test74Wakes() {
        openPreview(stopped)
        let alert = app.alerts["Open a preview"]
        XCTAssertTrue(alert.waitForExistence(timeout: 5))
        alert.buttons["Open"].tap()
        let notice = any("previewNotice")
        XCTAssertTrue(notice.waitForExistence(timeout: 5))
        XCTAssertEqual(notice.label, "Resuming the VM, then opening port 3000…")
        shot("preview-8-waking")
        XCTAssertTrue(app.alerts.firstMatch.waitForExistence(timeout: 10))
        app.alerts.firstMatch.buttons.firstMatch.tap()
    }

    /// Live, against a real server (`$FLOTILLA_LINK` pairs with it) whose thread
    /// `$FLOTILLA_LIVE_THREAD` serves a page on port 3000 in its VM: Preview opens it in the
    /// in-app Safari view, and Done comes back.
    func test76Live() throws {
        let thread = try XCTUnwrap(env["FLOTILLA_LIVE_THREAD"], "set TEST_RUNNER_FLOTILLA_LIVE_THREAD")
        app.launchArguments.removeAll { $0 == "-FlotillaSlowPreview" || $0 == "2500" }
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForList()
        allowNotifications(timeout: 5)
        waitForThreads(thread)
        open(thread)
        let preview = app.buttons["preview"]
        XCTAssertTrue(preview.waitForExistence(timeout: 10))
        preview.tap()
        let alert = app.alerts["Open a preview"]
        XCTAssertTrue(alert.waitForExistence(timeout: 5))
        alert.buttons["Open"].tap()
        let notice = any("previewNotice")
        if notice.waitForExistence(timeout: 3) {
            note("notice: \(notice.label)", tag: "live", file: "live.txt")
            shot("preview-live-1-opening")
        }
        let done = app.buttons["Close"]
        XCTAssertTrue(done.waitForExistence(timeout: 120), "the in-app Safari view")
        XCTAssertTrue(showing("Served on port 3000.").waitForExistence(timeout: 30), "the page from the VM")
        sleep(1)
        shot("preview-live-2-page")
        done.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        XCTAssertTrue(done.waitForNonExistence(timeout: 5))
        XCTAssertTrue(any("composer").waitForExistence(timeout: 5), "back on the thread")
        sleep(1)
        shot("preview-live-3-back")
    }

    /// A services file that doesn't check out: what's wrong, then Another port….
    func test75FileProblem() {
        openPreview(broken)
        let problem = any("servicesProblem")
        XCTAssertTrue(problem.waitForExistence(timeout: 5), "the file's problem")
        XCTAssertTrue(problem.label.contains("service web: no command"), problem.label)
        XCTAssertTrue(app.buttons["previewOtherPort"].exists)
        sleep(1)
        shot("preview-9-file-problem")
    }
}
