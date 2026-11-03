import XCTest

/// A thread's context and the agent accounts' limits, on a database from a live run (Claude
/// Code on Haiku 5.5, Codex and OpenCode on GPT-6 Luna: "Limits probe …"): the gauge beside
/// boat's counts and its sheet, the ring by Send and its numbers. Run after `test1Pairing`
/// against a server on that database. `TEST_RUNNER_FLOTILLA_APPEARANCE` (day or night) sets
/// the app's Appearance.
final class LimitsUITests: FlotillaUITestCase {
    override func setUp() {
        super.setUp()
        if let look = env["FLOTILLA_APPEARANCE"] { app.launchArguments += ["-appearance", look] }
    }

    /// The app asks to send notifications once it's connected: answered, so it covers nothing.
    func waitForThreadsAllowing() {
        allowNotifications(timeout: 3)
        waitForThreads()
    }

    func test60AgentLimits() {
        app.launch()
        waitForThreadsAllowing()
        let gauge = app.buttons["agentLimits"]
        XCTAssertTrue(gauge.waitForExistence(timeout: 10), "the agent limits beside boat's counts")
        XCTAssertTrue(app.buttons["usage"].exists, "boat's counts stay, apart")
        sleep(1)
        shot("60-threads-agent-limits")
        gauge.tap()
        // The usage sheet, on its Agents tab.
        let sheet = app.descendants(matching: .any)["agentsUsage"]
        XCTAssertTrue(sheet.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Claude"].exists && app.staticTexts["ChatGPT · Pro"].exists, "each account")
        XCTAssertTrue(app.descendants(matching: .any).matching(NSPredicate(format: "label == 'Weekly · Fable'")).firstMatch.exists, "a window for one model")
        sleep(1)
        shot("61-agent-limits-sheet")
    }

    /// At the largest type size: the bar keeps New thread whole, and the sheet scrolls.
    func test62LargeType() {
        app.launchArguments += ["-UIPreferredContentSizeCategoryName", "UICTContentSizeCategoryAccessibilityXXXL"]
        app.launch()
        waitForThreadsAllowing()
        let gauge = app.buttons["agentLimits"]
        XCTAssertTrue(gauge.waitForExistence(timeout: 10))
        sleep(1)
        shot("64-large-type-threads")
        gauge.tap()
        XCTAssertTrue(app.descendants(matching: .any)["agentsUsage"].waitForExistence(timeout: 5))
        sleep(1)
        shot("65-large-type-agent-limits")
        app.swipeUp()
        sleep(1)
        shot("66-large-type-agent-limits-scrolled")
    }

    func test61ContextRing() {
        app.launch()
        waitForThreadsAllowing()
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS 'Limits probe Claude'")).firstMatch.tap()
        let ring = app.buttons["contextMeter"]
        XCTAssertTrue(ring.waitForExistence(timeout: 10), "the context ring by Send")
        XCTAssertEqual(ring.value as? String, "2.4% used, 24,068 of 1,000,000 tokens")
        sleep(1)
        shot("62-thread-context-ring")
        ring.tap()
        XCTAssertTrue(app.descendants(matching: .any)["contextBubble"].waitForExistence(timeout: 5), "the numbers in full")
        sleep(1)
        shot("63-context-numbers")
    }
}
