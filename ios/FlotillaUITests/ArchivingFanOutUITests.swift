import XCTest

/// Settings → Archiving, Undo for archives (yours and the server's), and one prompt started on
/// several models, against `flotilla serve --offline` on `ios/script/dev-db.py --project
/// --states`. `test40Pair` pairs from `$FLOTILLA_LINK` first; screenshots go to
/// `$FLOTILLA_SHOTS`.
final class ArchivingFanOutUITests: FlotillaUITestCase {
    func launch(note: String? = nil) {
        guard let note else { return launchToThreads(DevDB.median) }
        app.launchEnvironment["FLOTILLA_TEST_NOTE"] = note
        app.launch()
        // Its notice comes in a few seconds and lasts five: straight to it.
        waitForList()
    }

    /// A thread's row, scrolled clear of the New thread bar (the list only holds the rows on
    /// screen).
    func row(_ id: String) -> XCUIElement {
        let row = any("thread-\(id)")
        let bar = app.buttons["newThread"]
        for _ in 0..<5 {
            if row.waitForExistence(timeout: 2), row.isHittable, !bar.exists || row.frame.maxY < bar.frame.minY - 12 { break }
            app.swipeUp()
        }
        XCTAssertTrue(row.exists, "row \(id)")
        return row
    }

    /// Back to the top of the list.
    func top() {
        for _ in 0..<3 { app.swipeDown() }
    }

    func test40Pair() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForList()
        allowNotifications(timeout: 5)
        XCTAssertTrue(any("thread-\(DevDB.median)").waitForExistence(timeout: 20))
    }

    /// Settings → Archiving: on merge (on by default), and idle days from the menu.
    func test41SettingsArchiving() {
        launch()
        settings()
        let onMerge = app.switches["archiveOnMerge"]
        XCTAssertTrue(onMerge.waitForExistence(timeout: 10), "Archiving in Settings")
        XCTAssertEqual(onMerge.value as? String, "1", "on unless turned off")
        let idle = app.buttons["archiveIdleDays"]
        XCTAssertTrue(idle.waitForExistence(timeout: 5))
        // Up to the section, clear of the setup checks.
        if !idle.isHittable { app.swipeUp() }
        sleep(1)
        shot("archiving-1-settings")
        idle.tap()
        let seven = app.buttons["7 days"]
        XCTAssertTrue(seven.waitForExistence(timeout: 5), "the idle days")
        sleep(1)
        shot("archiving-2-idle-days")
        seven.tap()
        expectation(for: NSPredicate(format: "label CONTAINS '7 days'"), evaluatedWith: idle)
        waitForExpectations(timeout: 10)
        sleep(1)
        shot("archiving-3-seven-days")
    }

    /// Two models chosen in the picker (Select), the chip naming both; offline, Start is refused.
    func test42ChooseTwoModels() {
        launch()
        // Threads from this prompt already (a run before).
        let started = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'thread-' AND label CONTAINS 'Make the sidebar remember folded groups'"))
        let before = started.count
        app.buttons["newThread"].tap()
        let prompt = any("newPrompt")
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.typeText("Make the sidebar remember folded groups for each server")
        app.buttons["projectChip"].tap()
        let playground = app.buttons["flotilla-playground"]
        XCTAssertTrue(playground.waitForExistence(timeout: 5))
        playground.tap()
        // One model: a tap picks it.
        pickModel(search: "opus 5.5", id: "claude-code/opus-5-5")
        // Then Select, for another beside it.
        app.buttons["modelChip"].tap()
        let select = app.buttons["selectModels"]
        XCTAssertTrue(select.waitForExistence(timeout: 5), "Select")
        select.tap()
        XCTAssertTrue(app.navigationBars["1 model"].waitForExistence(timeout: 5))
        sleep(1)
        shot("fan-out-1-select")
        let search = app.searchFields["Search models"]
        search.tap()
        search.typeText("6.1 sol")
        let sol = app.buttons["model-codex/gpt-6.1-sol"]
        XCTAssertTrue(sol.waitForExistence(timeout: 5))
        sol.tap()
        XCTAssertTrue(app.navigationBars["2 models"].waitForExistence(timeout: 5), "two chosen")
        sleep(1)
        shot("fan-out-2-two-models")
        app.buttons["modelsDone"].tap()
        let chip = app.buttons["modelChip"]
        expectation(for: NSPredicate(format: "label == 'Models, Opus 5.5 and GPT-6.1 Sol'"), evaluatedWith: chip)
        waitForExpectations(timeout: 5)
        sleep(1)
        shot("fan-out-3-chip")
        // An offline server runs nothing: it refuses both at once, and New thread comes back
        // with the prompt and both models, saying why (live, the first opens at once).
        app.buttons["startThread"].tap()
        let why = any("startProblem")
        XCTAssertTrue(why.waitForExistence(timeout: 15), "the refusal, back on New thread")
        XCTAssertTrue(why.label.contains("Not available offline"), why.label)
        XCTAssertEqual(app.buttons["modelChip"].label, "Models, Opus 5.5 and GPT-6.1 Sol")
        sleep(1)
        shot("fan-out-4-refused-offline")
        XCTAssertEqual(started.count, before, "nothing listed as starting")
    }

    /// The thread menu's Archive: back to the list, which offers Undo.
    func test43ArchiveFromTheThread() {
        launch()
        row(DevDB.uname).tap()
        XCTAssertTrue(any("composer").waitForExistence(timeout: 10))
        app.buttons["threadMenu"].tap()
        let archive = app.buttons["Archive"].firstMatch
        XCTAssertTrue(archive.waitForExistence(timeout: 5), "Archive in the menu")
        sleep(1)
        shot("archive-1-thread-menu")
        archive.tap()
        XCTAssertTrue(app.navigationBars["Threads"].waitForExistence(timeout: 5), "back to the list")
        let undo = app.buttons["undoArchive"]
        XCTAssertTrue(undo.waitForExistence(timeout: 5), "Undo")
        XCTAssertTrue(app.staticTexts["Archived 1 thread"].exists)
        shot("archive-2-back-to-the-list-with-undo")
        // Under Archived now, which is folded.
        expectation(for: NSPredicate(format: "exists == false"), evaluatedWith: any("thread-\(DevDB.uname)"))
        waitForExpectations(timeout: 4)
        undo.tap()
        XCTAssertFalse(undo.exists)
        _ = row(DevDB.uname)
        sleep(1)
        shot("archive-3-undone")
    }

    /// What the server says when a merge archives a thread (a test-only hook feeds the note an
    /// offline server never sends): the notice says it merged, with Undo.
    func test44MergedAndArchived() {
        launch(note: #"{"kind":"pr_merged","data":{"thread":"\#(DevDB.spawn)","number":12,"title":"Check disk space in parallel","branch":"main","archived":true}}"#)
        let undo = app.buttons["undoArchive"]
        XCTAssertTrue(undo.waitForExistence(timeout: 20), "the merge's notice")
        XCTAssertTrue(app.staticTexts["#12 merged · Archived"].exists)
        shot("archive-4-merged-and-archived")
        undo.tap()
        _ = row(DevDB.spawn)
    }

    /// Threads the server archived for sitting idle show the same way.
    func test45IdleArchived() {
        launch(note: #"{"kind":"idle_archived","data":["\#(DevDB.check)","\#(DevDB.uname)"]}"#)
        let undo = app.buttons["undoArchive"]
        XCTAssertTrue(undo.waitForExistence(timeout: 20), "the idle threads' notice")
        XCTAssertTrue(app.staticTexts["Archived 2 threads"].exists)
        shot("archive-5-idle-archived")
        undo.tap()
        _ = row(DevDB.check)
        top()
        _ = row(DevDB.uname)
    }
}
