import XCTest

/// Milestone 2's screens, against `flotilla serve --offline` on `ios/script/dev-db.py --pr
/// --states --project --fix --delegate` (paired first with `FlotillaUITests/test1Pairing`).
/// Screenshots go to `$FLOTILLA_SHOTS`, as there.
final class MilestoneTwoUITests: FlotillaUITestCase {
    /// To Threads, the notifications question answered (asked the first time it connects).
    func launch() {
        launchToThreads(allow: 3)
    }

    /// The server Settings says the phone is paired with.
    func paired(_ name: String) -> Bool {
        let row = app.staticTexts["server"]
        return row.waitForExistence(timeout: 20) && row.label == name
    }

    /// The servers this phone has a token for, by id (a debug build launched with
    /// `-FlotillaKeychain`).
    func tokens() -> [String] {
        let probe = any("keychain")
        XCTAssertTrue(probe.waitForExistence(timeout: 10), "the Keychain probe")
        return probe.label == "none" ? [] : probe.label.components(separatedBy: ",")
    }

    /// Pair with the server in a pairing link (`FLOTILLA_LINK`), from Threads.
    func pairFromLink(_ link: String, server: String) throws {
        try openPairingLink(link, server: server)
        app.buttons["pairConnect"].tap()
        waitForList()
    }

    /// The photo library: the newest photo (it lists them newest first), or the oldest.
    func pickPhoto(newest: Bool) {
        let photos = app.images.matching(NSPredicate(format: "label BEGINSWITH 'Photo'"))
        if !photos.firstMatch.waitForExistence(timeout: 15) {
            dump("picker")
            XCTFail("the photo picker's photos")
        }
        sleep(1)
        (newest ? photos.firstMatch : photos.element(boundBy: photos.count - 1)).tap()
        let add = app.buttons["Add"]
        if add.waitForExistence(timeout: 3) { add.tap() }
    }

    func test20NewThread() {
        launch()
        app.buttons["newThread"].tap()
        let prompt = any("newPrompt")
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.typeText("Make the sidebar remember folded groups for each server")
        XCTAssertTrue(app.buttons["projectChip"].exists)
        // The project, then its branch picker (the lists come from GitHub, through the engine).
        app.buttons["projectChip"].tap()
        let playground = app.buttons["flotilla-playground"]
        XCTAssertTrue(playground.waitForExistence(timeout: 5))
        playground.tap()
        XCTAssertTrue(app.buttons["branchChip"].waitForExistence(timeout: 5))
        sleep(1)
        shot("40-new-thread")
        app.buttons["modelChip"].tap()
        let search = app.searchFields["Search models"]
        XCTAssertTrue(search.waitForExistence(timeout: 5), "the model picker")
        sleep(1)
        shot("41-new-thread-models")
        search.tap()
        search.typeText("opus 5.5")
        let opus = app.buttons["model-claude-code/opus-5-5"]
        XCTAssertTrue(opus.waitForExistence(timeout: 5))
        sleep(1)
        shot("41b-new-thread-model-search")
        opus.tap()
        app.buttons["effortChip"].tap()
        sleep(1)
        shot("42-new-thread-effort")
        let high = app.buttons["High"]
        if high.waitForExistence(timeout: 3) { high.tap() }
        app.buttons["branchChip"].tap()
        sleep(2)
        shot("43-branch-picker")
    }

    func test21AttachPhoto() {
        launch()
        open(DevDB.median)
        app.buttons["attach"].tap()
        app.buttons["Photo Library"].tap()
        pickPhoto(newest: false)
        let chip = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'pending-photo-'")).firstMatch
        XCTAssertTrue(chip.waitForExistence(timeout: 15), "the photo waits in the composer")
        let composer = any("composer")
        composer.tap()
        composer.typeText("What's in this photo?")
        sleep(1)
        shot("44-photo-attached")
    }

    /// Pairing with a different server takes the first one's place: Settings names only the
    /// new one, and the first one's token leaves the Keychain. With `FLOTILLA_LINK` (the first
    /// server's), it pairs back with that one at the end, for the tests after.
    func test22PairWithPhotoReplaces() throws {
        app.launchArguments = ["-FlotillaKeychain"]
        launch()
        let before = tokens()
        XCTAssertEqual(before.count, 1, "the first server's token")
        settings()
        XCTAssertTrue(paired("127.0.0.1:7791"), "on the first server")
        sleep(1)
        shot("45a-settings-server")
        app.buttons["pairDifferent"].tap()
        XCTAssertTrue(any("viewfinder").waitForExistence(timeout: 10))
        sleep(1)
        shot("45-pair")
        app.buttons["pairPhoto"].tap()
        pickPhoto(newest: true)
        let server = app.textFields["pairServer"]
        let filled = NSPredicate(format: "value == 'http://127.0.0.1:7792'")
        expectation(for: filled, evaluatedWith: server)
        waitForExpectations(timeout: 15)
        sleep(1)
        shot("46-pair-from-photo")
        app.buttons["pairConnect"].tap()
        waitForList()
        XCTAssertFalse(any("viewfinder").exists)
        let after = tokens()
        XCTAssertEqual(after.count, 1, "one token: \(after)")
        XCTAssertNotEqual(after, before, "the first server's token is gone")
        sleep(2)
        settings()
        XCTAssertTrue(paired("127.0.0.1:7792"), "on the second server")
        XCTAssertFalse(app.staticTexts["127.0.0.1:7791"].exists, "the first server is gone from Settings")
        XCTAssertFalse(app.buttons["Forget 127.0.0.1:7791"].exists)
        sleep(1)
        shot("47-replaced")
        if let link = env["FLOTILLA_LINK"] {
            app.buttons["Done"].tap()
            try pairFromLink(link, server: "http://127.0.0.1:7791")
            XCTAssertTrue(any("thread-\(DevDB.median)").waitForExistence(timeout: 20), "the first server's threads again")
            XCTAssertEqual(tokens().count, 1)
            XCTAssertFalse(tokens() == after, "the second server's token is gone")
        }
    }

    func test23Settings() {
        launch()
        settings()
        // The checks are one line ("6/7") until it's opened, none failing.
        let toggle = app.buttons["setupToggle"]
        XCTAssertTrue(toggle.waitForExistence(timeout: 10))
        XCTAssertFalse(any("check-push").exists, "folded")
        sleep(1)
        shot("48a-settings-folded")
        toggle.tap()
        XCTAssertTrue(any("check-push").waitForExistence(timeout: 10), "the server's checks")
        XCTAssertTrue(app.buttons["addDevice"].waitForExistence(timeout: 10))
        sleep(1)
        shot("48-settings")
        app.buttons["addDevice"].tap()
        XCTAssertTrue(any("pairQR").waitForExistence(timeout: 10))
        sleep(1)
        shot("49-add-device")
    }

    func test24FixFillsTheComposer() {
        launch()
        let row = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS 'Spawn two parallel subagents'")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 20))
        row.tap()
        let fix = app.buttons["pr-Fix"]
        XCTAssertTrue(fix.waitForExistence(timeout: 10), "Fix on the PR bar")
        XCTAssertTrue(app.buttons["pr-Resolve"].exists && app.buttons["pr-Address review"].exists)
        shot("50-pr-actions")
        fix.tap()
        let composer = any("composer")
        expectation(for: NSPredicate(format: "value BEGINSWITH 'Fix the failing checks on PR #43'"), evaluatedWith: composer)
        waitForExpectations(timeout: 5)
        sleep(1)
        shot("51-fix-filled-composer")
    }

    func test25OpenDelegatesThread() {
        launch()
        open(DevDB.slug)
        let card = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Review slug.py (Codex)'")).firstMatch
        // To the top, so the card is clear of the navigation bar.
        for _ in 0..<5 { app.swipeDown(velocity: .fast) }
        sleep(1)
        XCTAssertTrue(card.waitForExistence(timeout: 10), "the delegate's card")
        card.tap()
        sleep(1)
        shot("52-delegate-card")
        let openThread = app.buttons["openThread-\(DevDB.codexReview)"]
        if !openThread.waitForExistence(timeout: 5) {
            dump("card")
            XCTFail("Open thread on the card")
        }
        openThread.tap()
        XCTAssertTrue(app.staticTexts["From Planting bugs and getting reviews"].waitForExistence(timeout: 10), "the delegate's own thread, its task from its parent")
        sleep(1)
        shot("53-delegate-thread")
    }

    /// The host sends `simctl push` once the app is in the background (it waits for
    /// `$FLOTILLA_PUSH_READY`, which this writes); the banner is tapped, and opens the thread.
    /// `test32PushFromAnotherServer` is the same with a push from a server the phone left.
    func test26PushOpensItsThread() throws {
        let ready = try XCTUnwrap(env["FLOTILLA_PUSH_READY"])
        launch()
        XCUIDevice.shared.press(.home)
        sleep(1)
        try "ready".write(toFile: ready, atomically: true, encoding: .utf8)
        // The host pushes a second after the marker; a banner stays about five seconds.
        sleep(3)
        shot("54-push-banner")
        // Where the banner is: the top of the screen.
        springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.085)).tap()
        if !any("composer").waitForExistence(timeout: 15) {
            shot("55-after-tap")
            dump("after-tap")
            XCTFail("the thread it's about")
        }
        XCTAssertTrue(app.staticTexts["Median function with subtle bug"].waitForExistence(timeout: 10))
        sleep(2)
        shot("55-push-opened-thread")
    }

    // MARK: The live run (a real engine: `FLOTILLA_DEV=1 flotilla serve`)

    /// Pair with the live server from its link, opened while paired with another (which it
    /// replaces).
    func test27PairLive() throws {
        let link = try pairingLink()
        launch()
        try pairFromLink(link, server: "http://127.0.0.1:7793")
        sleep(3)
        shot("60-live-threads")
        settings()
        XCTAssertTrue(paired("127.0.0.1:7793"), "on the live server")
    }

    /// New thread's branch picker with the project's pull requests and branches from GitHub.
    func test28LiveBranches() {
        app.launch()
        waitForList()
        let new = app.buttons["newThread"]
        expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: new)
        waitForExpectations(timeout: 20)
        new.tap()
        app.buttons["projectChip"].tap()
        app.buttons["flotilla-playground"].tap()
        app.buttons["branchChip"].tap()
        let loading = app.staticTexts["Loading pull requests and branches…"]
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: loading)
        waitForExpectations(timeout: 30)
        sleep(1)
        shot("61-live-branch-picker")
    }

    /// Start a thread from the phone on Claude Code Haiku with a photo, and watch the agent
    /// describe it.
    func test29LiveNewThreadWithPhoto() {
        app.launch()
        waitForList()
        let new = app.buttons["newThread"]
        expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: new)
        waitForExpectations(timeout: 20)
        new.tap()
        let prompt = any("newPrompt")
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.typeText("Describe the photo I attached in two or three sentences. Don't run any tools.")
        // Claude Code's Haiku, low effort, no project.
        pickModel(search: "haiku 5.5", id: "claude-code/claude-haiku-5-5", effort: "Low")
        app.buttons["projectChip"].tap()
        let none = app.buttons["No project"]
        XCTAssertTrue(none.waitForExistence(timeout: 5))
        none.tap()
        app.buttons["attach"].tap()
        app.buttons["Photo Library"].tap()
        pickPhoto(newest: false)
        let chip = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'pending-photo-'")).firstMatch
        XCTAssertTrue(chip.waitForExistence(timeout: 15), "the photo waits in New thread")
        sleep(1)
        shot("62-live-new-thread-photo")
        app.buttons["startThread"].tap()
        XCTAssertTrue(any("composer").waitForExistence(timeout: 15), "the thread opens at once")
        sleep(2)
        shot("63-live-starting")
        // The VM starts, the photo goes up, Haiku answers.
        for i in 0..<36 {
            sleep(5)
            if i % 6 == 5 { shot(String(format: "64-live-%02d", i)) }
        }
        shot("65-live-answered")
    }

    /// Forget the server (the live one, stopped by now, so it can't be told): the phone is
    /// back at pairing at once, with no token left.
    func test30Forget() {
        app.launchArguments = ["-FlotillaKeychain"]
        app.launch()
        waitForList()
        XCTAssertEqual(tokens().count, 1)
        settings()
        let forget = app.buttons["forgetServer"]
        XCTAssertTrue(forget.waitForExistence(timeout: 5))
        forget.tap()
        let confirm = app.buttons["Forget"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        confirm.tap()
        XCTAssertTrue(app.textFields["pairServer"].waitForExistence(timeout: 5), "pairing, at once")
        XCTAssertEqual(tokens(), [], "its token is gone")
        sleep(1)
        shot("56-forgot-the-server")
        // Launched again: still not paired.
        app.terminate()
        app.launch()
        XCTAssertTrue(app.textFields["pairServer"].waitForExistence(timeout: 10), "pairing after a launch")
    }

    /// The usage in full, from the counts at the bottom of Threads (`dev-db.py --usage` or
    /// `--usage-tight`): every count with its meter, Refresh and boat.dev's billing.
    func test31UsageSheet() {
        launch()
        let usage = app.buttons["usage"]
        XCTAssertTrue(usage.waitForExistence(timeout: 10), "the counts' button")
        XCTAssertTrue(usage.label.hasPrefix("Usage, "), usage.label)
        usage.tap()
        XCTAssertTrue(any("usageSheet").waitForExistence(timeout: 5), "the usage sheet")
        XCTAssertTrue(app.links["Billing on boat.dev"].exists || app.buttons["Billing on boat.dev"].exists, "boat.dev's billing")
        XCTAssertTrue(app.staticTexts["VMs running"].exists || any("VMs running").exists)
        sleep(1)
        shot("60-usage")
    }

    /// A notification from a server this phone isn't paired with (one it left, which couldn't
    /// be told): the host pushes one about a thread there once the app is in the background
    /// (as `test26PushOpensItsThread`); tapped, it opens the app on its threads, and no thread.
    func test32PushFromAnotherServer() throws {
        let ready = try XCTUnwrap(env["FLOTILLA_PUSH_READY"])
        launch()
        XCUIDevice.shared.press(.home)
        sleep(1)
        try "ready".write(toFile: ready, atomically: true, encoding: .utf8)
        sleep(3)
        shot("57-push-from-another-server")
        springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.085)).tap()
        XCTAssertTrue(app.navigationBars["Threads"].waitForExistence(timeout: 15), "the app, on its threads")
        XCTAssertFalse(any("composer").waitForExistence(timeout: 5), "no thread opened")
        sleep(1)
        shot("58-push-from-another-server-tapped")
    }
}
