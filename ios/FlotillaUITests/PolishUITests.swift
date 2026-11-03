import XCTest

/// The finish work after using the app on a real iPhone: naming the phone when it pairs and
/// later, the long-press menus, Jump to latest. Against `flotilla serve --offline` on
/// `ios/script/dev-db.py --pr --states --project --fix --delegate`; screenshots go to
/// `$FLOTILLA_SHOTS`, as in the other suites. `test40PairWithAName` pairs first
/// (`TEST_RUNNER_FLOTILLA_LINK`).
final class PolishUITests: FlotillaUITestCase {
    /// To Threads, the notifications question answered (asked the first time it connects).
    func launch() {
        launchToThreads(DevDB.slug, allow: 2)
    }

    /// Clear a field and type into it.
    func replace(_ field: XCUIElement, with text: String) {
        // The caret at the end, then back over all of it.
        field.coordinate(withNormalizedOffset: CGVector(dx: 0.97, dy: 0.5)).tap()
        let had = (field.value as? String)?.count ?? 0
        field.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: had + 4) + text)
    }

    /// The text field in the alert titled `title`.
    func alertField(_ title: String) -> XCUIElement {
        let alert = app.alerts[title]
        if !alert.waitForExistence(timeout: 5) {
            dump("no-alert-\(title)")
            XCTFail("the \(title) alert")
        }
        let field = alert.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        return field
    }

    func menuItem(_ label: String) -> XCUIElement {
        let item = app.buttons[label]
        XCTAssertTrue(item.waitForExistence(timeout: 5), "\(label) in the menu")
        return item
    }

    // MARK: The phone's name

    /// Pairing asks what to call the phone ("iPhone", as iOS tells apps, to start with); the
    /// server lists it so, and Settings → Devices renames it.
    func test40PairWithAName() throws {
        try startPairing()
        let name = app.textFields["pairName"]
        XCTAssertTrue(name.waitForExistence(timeout: 5))
        XCTAssertEqual(name.value as? String, "iPhone", "named as iOS names it, to start with")
        shot("70-pair-device-name")
        replace(name, with: "Test iPhone")
        sleep(1)
        shot("71-pair-device-named")
        name.typeText("\n")
        waitForList()
        allowNotifications(timeout: 3)

        app.buttons["openSettings"].tap()
        let mine = app.buttons["device-Test iPhone"]
        XCTAssertTrue(mine.waitForExistence(timeout: 10), "this phone, by the name it paired with")
        sleep(1)
        shot("72-settings-devices-named")
        mine.tap()
        sleep(1)
        shot("73-device-menu")
        menuItem("Rename…").tap()
        let field = alertField("Rename device")
        replace(field, with: "Commute phone")
        sleep(1)
        shot("74-rename-device")
        app.alerts.buttons["Rename"].firstMatch.tap()
        XCTAssertTrue(app.buttons["device-Commute phone"].waitForExistence(timeout: 10), "the server's new name for it")
        sleep(1)
        shot("75-device-renamed")
    }

    // MARK: Long-press menus

    /// A thread's row: read or unread, archive, rename, its pull request, delete.
    func test41ThreadRowMenu() {
        launch()
        let row = any("thread-\(DevDB.spawn)")
        row.press(forDuration: 1.0)
        XCTAssertTrue(app.buttons["Rename…"].waitForExistence(timeout: 5), "the row's menu")
        sleep(1)
        shot("76-thread-row-menu")
        app.buttons["Rename…"].tap()
        let field = alertField("Rename thread")
        expectation(for: NSPredicate(format: "value BEGINSWITH 'Spawn two'"), evaluatedWith: field)
        waitForExpectations(timeout: 5)
        replace(field, with: "System checks in parallel")
        sleep(1)
        shot("77-rename-thread")
        app.alerts.buttons["Rename"].firstMatch.tap()
        // An offline server can't rename (boat has the name too): it says so at once, and the
        // title stays.
        refusedOffline("Couldn't rename the thread", shot: "78-rename-refused-offline")
        XCTAssertTrue(row.label.contains("Spawn two"), row.label)

        // Delete asks first; offline, it's refused, and the row comes back.
        let doomed = any("thread-\(DevDB.uname)")
        doomed.press(forDuration: 1.0)
        menuItem("Delete…").tap()
        let confirm = app.alerts.buttons["Delete"].firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5), "Delete asks first")
        sleep(1)
        shot("79-delete-asks")
        confirm.tap()
        refusedOffline("Couldn't delete the thread", shot: "80-delete-refused-offline")
        XCTAssertTrue(doomed.waitForExistence(timeout: 5), "still listed")
    }

    /// The alert an offline server's refusal brings, dismissed.
    func refusedOffline(_ what: String, shot name: String) {
        let alert = app.alerts["\(what): Not available offline"]
        XCTAssertTrue(alert.waitForExistence(timeout: 5), "\(what), at once")
        sleep(1)
        shot(name)
        alert.buttons["OK"].tap()
    }

    /// The thread's ⋯ has the same commands.
    func test42ThreadHeaderMenu() {
        launch()
        open(DevDB.slug)
        app.buttons["threadMenu"].tap()
        XCTAssertTrue(app.buttons["Rename…"].waitForExistence(timeout: 5))
        sleep(1)
        shot("81-thread-header-menu")
    }

    /// A reply, a prompt and a tool call copy; a hand-off card opens its thread.
    func test43MessageMenus() {
        launch()
        open(DevDB.slug)
        for _ in 0..<5 { app.swipeDown(velocity: .fast) }
        sleep(1)
        let prompt = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'You, '")).firstMatch
        XCTAssertTrue(prompt.waitForExistence(timeout: 5))
        prompt.press(forDuration: 1.0)
        XCTAssertTrue(app.buttons["Copy"].waitForExistence(timeout: 5), "Copy on the prompt")
        sleep(1)
        shot("82-prompt-menu")
        app.buttons["Select text"].tap()
        XCTAssertTrue(any("selectableText").waitForExistence(timeout: 5))
        sleep(1)
        shot("83-select-text")
        app.buttons["Done"].tap()
        sleep(1)

        unfoldCalls()
        let tool = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Bash'")).firstMatch
        XCTAssertTrue(tool.waitForExistence(timeout: 5))
        tool.press(forDuration: 1.0)
        XCTAssertTrue(app.buttons["Copy output"].waitForExistence(timeout: 5), "a tool call's menu")
        sleep(1)
        shot("84-tool-menu")
        app.buttons["Copy output"].tap()
        sleep(1)

        let reply = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Both bugs show up'")).firstMatch
        XCTAssertTrue(reply.waitForExistence(timeout: 5))
        reply.press(forDuration: 1.0)
        XCTAssertTrue(app.buttons["Select text"].waitForExistence(timeout: 5), "a reply's menu")
        sleep(1)
        shot("85-reply-menu")
        app.buttons["Copy"].tap()
        sleep(1)

        let card = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Review slug.py (Codex)'")).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        card.press(forDuration: 1.0)
        XCTAssertTrue(app.buttons["Open thread"].waitForExistence(timeout: 5), "the hand-off card's menu")
        sleep(1)
        shot("86-handoff-card-menu")
        app.buttons["Open thread"].tap()
        XCTAssertTrue(app.staticTexts["From Planting bugs and getting reviews"].waitForExistence(timeout: 10), "the delegate's own thread")
        sleep(1)
        shot("87-handoff-opened")
    }

    /// Two groups, close together as in the mockup (one thread marked unread from its menu).
    func test47Threads() {
        launch()
        let row = any("thread-\(DevDB.median)")
        if !(row.label.hasPrefix("Unread")) {
            row.press(forDuration: 1.0)
            menuItem("Mark as unread").tap()
        }
        sleep(2)
        shot("96-threads")
    }

    // MARK: Live (a real engine: `FLOTILLA_DEV=1 flotilla serve`, one VM)

    /// Pair with the live server, start a thread on Claude Code's Haiku, rename it from its
    /// ⋯, then delete it from its row: the VM goes with it.
    func test48LiveRenameAndDelete() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForList()
        allowNotifications(timeout: 3)

        let new = app.buttons["newThread"]
        expectation(for: NSPredicate(format: "isEnabled == true"), evaluatedWith: new)
        waitForExpectations(timeout: 30)
        new.tap()
        let prompt = any("newPrompt")
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.typeText("Reply with just the word PLUM. Don't run any tools.")
        pickModel(search: "haiku 5.5", id: "claude-code/claude-haiku-5-5", effort: "Low")
        if app.buttons["projectChip"].exists {
            app.buttons["projectChip"].tap()
            let none = app.buttons["No project"]
            if none.waitForExistence(timeout: 5) { none.tap() }
        }
        app.buttons["startThread"].tap()
        XCTAssertTrue(any("composer").waitForExistence(timeout: 15), "the thread opens at once")
        // Its ⋯ comes once the server has written it.
        let menu = app.buttons["threadMenu"]
        XCTAssertTrue(menu.waitForExistence(timeout: 90), "the thread, written")
        let plum = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'PLUM'")).firstMatch
        _ = plum.waitForExistence(timeout: 180)
        sleep(2)
        shot("97-live-answered")

        menu.tap()
        menuItem("Rename…").tap()
        replace(alertField("Rename thread"), with: "Live rename test")
        app.alerts.buttons["Rename"].firstMatch.tap()
        XCTAssertTrue(app.staticTexts["Live rename test"].waitForExistence(timeout: 10), "the header's new title")
        sleep(3)
        shot("98-live-renamed")

        app.navigationBars.buttons.firstMatch.tap()
        let row = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'thread-' AND label CONTAINS 'Live rename test'")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 15), "the row, renamed by the server")
        row.press(forDuration: 1.0)
        menuItem("Delete…").tap()
        let confirm = app.alerts.buttons["Delete"].firstMatch
        XCTAssertTrue(confirm.waitForExistence(timeout: 5))
        confirm.tap()
        XCTAssertFalse(row.exists, "gone from the list at once")
        // boat deletes the VM; the server answers; nothing comes back.
        sleep(30)
        XCTAssertFalse(row.exists, "still gone")
        XCTAssertFalse(app.alerts.firstMatch.exists, "no Couldn't delete")
        shot("99-live-deleted")
    }

    // MARK: The icon

    /// The home screen's Dark and Tinted icons (Edit → Customize), then back to Default.
    func test46IconStyles() {
        XCUIDevice.shared.press(.home)
        sleep(1)
        // The page with Flotilla on it.
        if !springboard.icons["Flotilla"].isHittable { springboard.swipeLeft() }
        sleep(1)
        springboard.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.6)).press(forDuration: 1.5)
        let edit = springboard.buttons["Edit"]
        if !edit.waitForExistence(timeout: 5) {
            try? springboard.debugDescription.write(toFile: "\(env["FLOTILLA_SHOTS"] ?? "/tmp")/springboard.txt", atomically: true, encoding: .utf8)
            XCTFail("the home screen's Edit")
            return
        }
        edit.tap()
        let customize = springboard.buttons["Customize"]
        XCTAssertTrue(customize.waitForExistence(timeout: 5))
        customize.tap()
        sleep(2)
        try? springboard.debugDescription.write(toFile: "\(env["FLOTILLA_SHOTS"] ?? "/tmp")/customize.txt", atomically: true, encoding: .utf8)
        for (style, name) in [("Dark", "94-icon-dark"), ("Tinted", "95-icon-tinted")] {
            let b = springboard.buttons[style]
            XCTAssertTrue(b.waitForExistence(timeout: 5), style)
            b.tap()
            sleep(2)
            shot(name)
        }
        springboard.buttons["Default"].firstMatch.tap()
        sleep(1)
        XCUIDevice.shared.press(.home)
    }

    // MARK: Jump to latest

    func test44JumpToLatest() {
        launch()
        open(DevDB.slug)
        sleep(1)
        XCTAssertFalse(any("jumpToLatest").exists, "not at the newest")
        app.swipeDown(velocity: .slow)
        app.swipeDown(velocity: .slow)
        let jump = any("jumpToLatest")
        XCTAssertTrue(jump.waitForExistence(timeout: 5), "away from the newest")
        sleep(1)
        shot("88-jump-to-latest")
        jump.tap()
        sleep(2)
        XCTAssertFalse(jump.exists, "back at the newest")
        shot("89-jumped")
    }

    /// The PR bar's thread, scrolled up: the button sits above the PR bar and composer.
    func test45JumpAbovePRBar() {
        launch()
        open(DevDB.median)
        sleep(1)
        app.swipeDown(velocity: .slow)
        app.swipeDown(velocity: .slow)
        XCTAssertTrue(any("jumpToLatest").waitForExistence(timeout: 5))
        sleep(1)
        shot("90-jump-above-pr-bar")
        any("composer").tap()
        sleep(2)
        shot("91-jump-with-keyboard")
    }
}
