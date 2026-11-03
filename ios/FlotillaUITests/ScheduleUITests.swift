import XCTest

/// Scheduled prompts on the phone: Scheduled (from the clock on Threads), a thread's line
/// over its composer and a prompt a schedule sent, Send later from the composer (once, then
/// weekly; nothing more often than daily, and a cron that is refused) and from New thread. Against `flotilla serve --offline` on a database with
/// schedules (`/tmp/flotilla-sched/base.db`, seeded as `design/scheduled-prompts.md`'s proof
/// says); `test60Pair` pairs from `$FLOTILLA_LINK` first; screenshots go to `$FLOTILLA_SHOTS`.
final class ScheduleUITests: FlotillaUITestCase {
    /// "Slugify function with reviews": a daily schedule sends to it, and sent its first prompt.
    static let daily = "a866986f-1097-4727-8a2b-63218953dc43"

    func launch() {
        launchToThreads(Self.daily)
    }

    func test60Pair() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForList()
        allowNotifications(timeout: 5)
        XCTAssertTrue(any("thread-\(Self.daily)").waitForExistence(timeout: 20))
    }

    /// The clock on Threads opens Scheduled: each with its next run, where to and its last
    /// run; a row opens its form.
    func test61Scheduled() {
        launch()
        let clock = app.buttons["openScheduled"]
        XCTAssertTrue(clock.waitForExistence(timeout: 10), "the clock, with schedules on the server")
        shot("schedule-1-threads")
        clock.tap()
        XCTAssertTrue(app.navigationBars["Scheduled"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label BEGINSWITH 'Daily at'")).firstMatch.waitForExistence(timeout: 5), "the daily schedule's row")
        sleep(1)
        shot("schedule-2-scheduled")
        any("schedule-s-weekdays").tap()
        XCTAssertTrue(app.navigationBars["Edit schedule"].waitForExistence(timeout: 5))
        sleep(1)
        shot("schedule-3-edit")
        app.buttons["Cancel"].tap()
        app.buttons["Done"].tap()
    }

    /// A thread with a schedule says what's next over its composer, and its first prompt says
    /// Scheduled. Send later from the composer: once, then weekly; scheduled, the composer
    /// empties and the line counts it.
    func test62SendLater() {
        launch()
        any("thread-\(Self.daily)").tap()
        let line = any("scheduledLine")
        XCTAssertTrue(line.waitForExistence(timeout: 10), "the scheduled line over the composer")
        for _ in 0..<6 { app.swipeDown() }
        sleep(1)
        shot("schedule-4-thread")
        let composer = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        composer.tap()
        composer.typeText("Rebase onto main and run the tests again")
        app.buttons["sendLater"].tap()
        XCTAssertTrue(app.navigationBars["Send later"].waitForExistence(timeout: 5))
        sleep(1)
        shot("schedule-5-send-later")
        app.buttons["Repeat"].tap()
        XCTAssertEqual(app.keyboards.count, 0, "no keyboard over the form")
        let kind = any("laterKind")
        XCTAssertTrue(kind.waitForExistence(timeout: 5))
        kind.tap()
        // Nothing more often than daily.
        XCTAssertTrue(app.buttons["Weekly"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["Every N hours"].exists, "no every N hours")
        sleep(1)
        shot("schedule-6a-repeat-kinds")
        app.buttons["Weekly"].tap()
        sleep(1)
        shot("schedule-6-weekly")
        app.buttons["laterSave"].tap()
        XCTAssertTrue(app.navigationBars["Send later"].waitForNonExistence(timeout: 10), "kept, and the sheet closed")
        XCTAssertTrue(app.staticTexts.containing(NSPredicate(format: "label CONTAINS ' more'")).firstMatch.waitForExistence(timeout: 10), "the line counts it")
        sleep(1)
        shot("schedule-7-scheduled-here")
    }

    /// A cron that runs more often than once a day is refused with why, in sight (Save puts the
    /// keyboard down), and the form stays to edit it.
    func test64CronTooOften() {
        launch()
        any("thread-\(Self.daily)").tap()
        let composer = app.textFields["composer"].exists ? app.textFields["composer"] : app.textViews["composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 10))
        composer.tap()
        composer.typeText("Check the queue")
        app.buttons["sendLater"].tap()
        XCTAssertTrue(app.navigationBars["Send later"].waitForExistence(timeout: 5))
        app.buttons["Repeat"].tap()
        let kind = any("laterKind")
        XCTAssertTrue(kind.waitForExistence(timeout: 5))
        kind.tap()
        app.buttons["Cron"].tap()
        let cron = any("laterCron")
        XCTAssertTrue(cron.waitForExistence(timeout: 5))
        cron.tap()
        cron.typeText("0 */4 * * 1-5")
        app.buttons["laterSave"].tap()
        let problem = any("laterProblem")
        XCTAssertTrue(problem.waitForExistence(timeout: 10), "refused")
        XCTAssertEqual(problem.label, "Runs must be at least a day apart.")
        XCTAssertTrue(app.navigationBars["Send later"].exists, "the form stays")
        sleep(1)
        XCTAssertEqual(app.keyboards.count, 0, "the keyboard down, the refusal in sight")
        shot("schedule-9-cron-too-often")
        app.navigationBars["Send later"].buttons["Cancel"].tap()
    }

    /// Send later from New thread: a new thread each run, on the model chosen.
    func test63NewThreadLater() {
        launch()
        app.buttons["newThread"].tap()
        let prompt = app.textFields["newPrompt"].exists ? app.textFields["newPrompt"] : app.textViews["newPrompt"]
        XCTAssertTrue(prompt.waitForExistence(timeout: 10))
        prompt.tap()
        prompt.typeText("Triage new issues labelled bug and draft fixes for the easy ones")
        app.buttons["startLater"].tap()
        XCTAssertTrue(app.navigationBars["Send later"].waitForExistence(timeout: 5))
        sleep(1)
        shot("schedule-8-new-thread-later")
        app.navigationBars["Send later"].buttons["Cancel"].tap()
    }
}
