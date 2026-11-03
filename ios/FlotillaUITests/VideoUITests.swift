import XCTest

/// The iPhone scenes of the 90-second video (`script/demo/flotilla-90s.sh phone` and `sync`), each run
/// while the host records the simulator (`simctl io recordVideo`). Against a live server
/// (`FLOTILLA_DEV=1 flotilla serve`) with the video's threads on it. Every tap is written to
/// `$FLOTILLA_TOUCHES` (time, x, y in points, seconds held), so the host can draw a touch
/// where the simulator's recording shows none. Each scene waits for `$FLOTILLA_GO` (a file the
/// host writes once its recorder runs) before it starts.
final class VideoUITests: FlotillaUITestCase {
    /// Write a touch at `point` (points), held `hold` seconds.
    func mark(_ point: CGPoint, hold: Double = 0) {
        guard let path = env["FLOTILLA_TOUCHES"] else { return }
        let line = String(format: "%.3f\t%.1f\t%.1f\t%.2f\n", Date().timeIntervalSince1970, point.x, point.y, hold)
        if let h = FileHandle(forWritingAtPath: path) {
            h.seekToEndOfFile()
            h.write(line.data(using: .utf8)!)
            h.closeFile()
        } else {
            try? line.write(toFile: path, atomically: true, encoding: .utf8)
        }
    }

    override func tap(_ el: XCUIElement) {
        mark(CGPoint(x: el.frame.midX, y: el.frame.midY))
        el.tap()
    }

    func typeInto(_ el: XCUIElement, _ text: String) {
        tap(el)
        el.typeText(text)
    }

    /// Wait for the host's recorder, and mark where the scene starts.
    func go() {
        if let path = env["FLOTILLA_GO"] {
            for _ in 0..<100 where !FileManager.default.fileExists(atPath: path) { usleep(100_000) }
        }
        // The scene starts here (a held time of -1 marks it).
        mark(.zero, hold: -1)
    }

    func pause(_ s: Double) { usleep(UInt32(s * 1_000_000)) }

    /// Not recorded: answer the notifications question once, so no scene shows it.
    func testV0Allow() {
        app.launch()
        allowNotifications(timeout: 8)
        pause(1)
    }

    /// Pair from a link: the form fills in, Connect, the same threads as the Mac.
    func testV1Pair() throws {
        let link = try pairingLink()
        app.launchArguments = ["-FlotillaForgetServer"]
        app.launch()
        XCTAssertTrue(app.textFields["pairServer"].waitForExistence(timeout: 15))
        go()
        pause(1)
        try openPairingLink(link)
        pause(1.8)
        tap(app.buttons["pairConnect"])
        // The list is under the pairing screen all along: wait for its rows.
        let rows = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'thread-'"))
        XCTAssertTrue(rows.firstMatch.waitForExistence(timeout: 20), "the threads, from the server")
        allowNotifications(timeout: 3)
        pause(3.5)
    }

    /// New thread with a photo: Haiku 5.5, no project, a photo from the library, Start.
    func testV4NewThreadPhoto() throws {
        let prompt = try XCTUnwrap(env["FLOTILLA_PROMPT"])
        launchToThreads()
        let new = app.buttons["newThread"]
        expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: new)
        waitForExpectations(timeout: 20)
        go()
        pause(1)
        tap(new)
        let field = any("newPrompt")
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        typeInto(field, prompt)
        pause(0.5)
        tap(app.buttons["modelChip"])
        let search = app.searchFields["Search models"]
        XCTAssertTrue(search.waitForExistence(timeout: 5), "the model picker")
        pause(0.8)
        typeInto(search, "haiku 5.5")
        let haiku = app.buttons["model-claude-code/claude-haiku-5-5"]
        XCTAssertTrue(haiku.waitForExistence(timeout: 5))
        pause(0.5)
        tap(haiku)
        pause(0.5)
        if app.buttons["effortChip"].exists {
            tap(app.buttons["effortChip"])
            let low = app.buttons["Low"]
            if low.waitForExistence(timeout: 3) { tap(low) }
        }
        tap(app.buttons["projectChip"])
        let none = app.buttons["No project"]
        XCTAssertTrue(none.waitForExistence(timeout: 5))
        tap(none)
        pause(0.4)
        tap(app.buttons["attach"])
        tap(app.buttons["Photo Library"])
        let photos = app.images.matching(NSPredicate(format: "label BEGINSWITH 'Photo'"))
        XCTAssertTrue(photos.firstMatch.waitForExistence(timeout: 15), "the photo picker")
        pause(1)
        // The library's oldest: the simulator's own photo of yellow flowers.
        tap(photos.element(boundBy: photos.count - 1))
        let add = app.buttons["Add"]
        if add.waitForExistence(timeout: 3) { tap(add) }
        let chip = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'pending-photo-'")).firstMatch
        XCTAssertTrue(chip.waitForExistence(timeout: 15), "the photo waits in New thread")
        pause(1.5)
        tap(app.buttons["startThread"])
        XCTAssertTrue(any("composer").waitForExistence(timeout: 15), "the thread opens at once")
        pause(Double(env["FLOTILLA_WATCH"] ?? "60") ?? 60)
    }

    /// Hold to merge, by touch.
    func testV5HoldToMerge() throws {
        let thread = try XCTUnwrap(env["FLOTILLA_MERGE_THREAD"])
        launchToThreads()
        open(thread)
        let hold = any("holdToMerge")
        XCTAssertTrue(hold.waitForExistence(timeout: 20), "Hold to merge on the PR bar")
        pause(1)
        go()
        pause(1.5)
        mark(CGPoint(x: hold.frame.midX, y: hold.frame.midY), hold: 1.8)
        hold.press(forDuration: 1.8)
        pause(5)
    }

    /// The phone half of the side-by-side: the thread open, watched without a touch while the
    /// Mac sends to it. Writes `$FLOTILLA_READY` once the thread is on screen, then waits for the
    /// host's go (which starts the Mac's recording too).
    func testV8Sync() throws {
        let thread = try XCTUnwrap(env["FLOTILLA_SYNC_THREAD"])
        let ready = try XCTUnwrap(env["FLOTILLA_READY"])
        launchToThreads()
        let row = any("thread-\(thread)")
        XCTAssertTrue(row.waitForExistence(timeout: 20), "row \(thread)")
        row.tap()
        XCTAssertTrue(any("composer").waitForExistence(timeout: 10))
        pause(2)
        try "ready".write(toFile: ready, atomically: true, encoding: .utf8)
        go()
        pause(Double(env["FLOTILLA_WATCH"] ?? "40") ?? 40)
    }

    /// Preview on the phone: the thread open; on go, the globe, its services sheet, Open on the
    /// first service, and the page in the in-app browser, scrolled once.
    func testV10Preview() throws {
        let thread = try XCTUnwrap(env["FLOTILLA_PREVIEW_THREAD"])
        let service = env["FLOTILLA_PREVIEW_SERVICE"] ?? "web"
        launchToThreads()
        open(thread)
        let globe = app.buttons["preview"]
        XCTAssertTrue(globe.waitForExistence(timeout: 20), "the globe in the header")
        pause(1.5)
        go()
        pause(1.2)
        tap(globe)
        let openButton = app.buttons["service-open-\(service)"]
        XCTAssertTrue(openButton.waitForExistence(timeout: 20), "Open on \(service)")
        expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: openButton)
        waitForExpectations(timeout: 30)
        pause(1.6)
        tap(openButton)
        pause(Double(env["FLOTILLA_PREVIEW_LOAD"] ?? "4") ?? 4)
        let page = app.webViews.firstMatch
        if page.waitForExistence(timeout: 5) {
            let from = page.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.75))
            let to = page.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35))
            mark(CGPoint(x: page.frame.midX, y: page.frame.minY + page.frame.height * 0.75), hold: 0.3)
            from.press(forDuration: 0.05, thenDragTo: to)
        }
        pause(Double(env["FLOTILLA_WATCH"] ?? "4") ?? 4)
    }

    /// The phone half of the question side-by-side: the thread open on the agent's waiting
    /// question; on the host's go (the Mac records too), pick its first option and Send, then
    /// watch the agent take the answer.
    func testV9Ask() throws {
        let thread = try XCTUnwrap(env["FLOTILLA_ASK_THREAD"])
        let ready = try XCTUnwrap(env["FLOTILLA_READY"])
        launchToThreads()
        let row = any("thread-\(thread)")
        XCTAssertTrue(row.waitForExistence(timeout: 20), "row \(thread)")
        row.tap()
        let send = app.buttons["ask-send"]
        XCTAssertTrue(send.waitForExistence(timeout: 20), "the question, waiting")
        let option = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'ask-option-0-'")).firstMatch
        XCTAssertTrue(option.waitForExistence(timeout: 5), "its first option")
        pause(1.5)
        try "ready".write(toFile: ready, atomically: true, encoding: .utf8)
        go()
        pause(Double(env["FLOTILLA_ASK_LEAD"] ?? "2.5") ?? 2.5)
        tap(option)
        pause(1.2)
        tap(send)
        pause(Double(env["FLOTILLA_WATCH"] ?? "14") ?? 14)
    }
}
