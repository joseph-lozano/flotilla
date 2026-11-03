import XCTest

/// The usage sheet's Agents tab: what the agents spent and their limits, on `dev-db.py
/// --agent-usage` (30 days made up hour by hour, both accounts' limits, the model prices beside
/// the database). Run after `test1Pairing` against a server on it.
/// `TEST_RUNNER_FLOTILLA_APPEARANCE` (day or night) sets the app's Appearance;
/// `TEST_RUNNER_FLOTILLA_SHOT_PREFIX` names the screenshots.
final class AgentUsageUITests: FlotillaUITestCase {
    override func setUp() {
        super.setUp()
        if let look = env["FLOTILLA_APPEARANCE"] { app.launchArguments += ["-appearance", look] }
    }

    /// boat's counts open on VMs; the tab beside it has the agents: their limits, the 30 days'
    /// cost, and a model picked for its trend. The agent limits beside the counts open there.
    func test70AgentsTab() {
        app.launch()
        allowNotifications(timeout: 3)
        waitForList()
        let usage = app.buttons["usage"]
        XCTAssertTrue(usage.waitForExistence(timeout: 10), "boat's counts")
        usage.tap()
        XCTAssertTrue(app.descendants(matching: .any)["usageSheet"].waitForExistence(timeout: 5))
        let agents = app.buttons["Agents"]
        XCTAssertTrue(agents.waitForExistence(timeout: 10), "the Agents tab, once the agents have something to show")
        sleep(1)
        shot("70-usage-vms")
        agents.tap()
        let tab = app.descendants(matching: .any)["agentsUsage"]
        XCTAssertTrue(tab.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts["Last 30 days"].waitForExistence(timeout: 10), "what the agents spent")
        XCTAssertTrue(app.staticTexts["API estimate"].exists, "priced at the model prices")
        XCTAssertTrue(app.staticTexts["ChatGPT · Pro"].exists && app.staticTexts["Weekly · Fable"].exists, "each account's weekly limits")
        XCTAssertFalse(app.staticTexts["5-hour"].exists, "never a 5-hour window")
        XCTAssertTrue(app.links["claude.ai"].exists && app.links["chatgpt.com"].exists, "each provider's usage page")
        sleep(1)
        shot("71-usage-agents")
        app.swipeUp()
        let opus = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'claude-opus-5-5'")).firstMatch
        XCTAssertTrue(opus.waitForExistence(timeout: 5), "the models")
        opus.tap()
        XCTAssertTrue(app.staticTexts["claude-opus-5-5 by day"].waitForExistence(timeout: 5), "its trend")
        sleep(1)
        shot("72-usage-agents-model")
        // A model with no known price: its tokens, no cost.
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'big-pickle'")).firstMatch.exists)
        app.swipeDown(velocity: .fast)
        app.swipeDown(velocity: .fast)
        // The agent limits beside the counts open the same sheet, on Agents.
        let gauge = app.buttons["agentLimits"]
        XCTAssertTrue(gauge.waitForExistence(timeout: 10))
        gauge.tap()
        XCTAssertTrue(app.descendants(matching: .any)["agentsUsage"].waitForExistence(timeout: 5))
        sleep(1)
        shot("73-agent-limits-open-agents")
    }
}
