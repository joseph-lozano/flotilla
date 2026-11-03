import XCTest

/// The threads in `ios/script/dev-db.py`'s database.
enum DevDB {
    /// "Planting bugs and getting reviews": two hand-off cards and "2 agents answered".
    static let slug = "31360c1e-cbab-4b44-993c-7dec50d661f5"
    /// "Median function with subtle bug": `--pr` gives it a pull request ready to merge.
    static let median = "3312c93d-97e5-483e-b800-79c5df5c8aec"
    /// "Review slug.py (Codex)": `--delegate` makes it a delegate with a thread of its own.
    static let codexReview = "0af928b2-5236-41ce-9310-6b0d65a4acc4"
    /// "Subagents run uname, df, ls, whoami" (Claude Code).
    static let uname = "f049a40a-ef06-48b1-a1ad-caedbc018d93"
    /// "Spawn two parallel subagents for system checks" (Codex): `--fix` gives it a pull request
    /// that needs fixing.
    static let spawn = "532f3f85-5d3e-4319-a344-48830958c7a7"
    /// "Check system via subagent" (OpenCode).
    static let check = "6bbd614f-ef8d-4cdb-84ef-be81957c9565"
    /// "Stress: a long thread" (`--stress`).
    static let stress = "5a5a5a5a-0000-4000-8000-00000000c0de"
    /// "Pick a file name" (`--ask`): its agent waits on an answer (`--ask answered`: answered).
    static let ask = "a5a5a5a5-0000-4000-8000-0000000a5c00"
}

/// What the suites share. Each drives the app in the simulator against a real `flotilla serve`
/// (see ios/README.md) and writes screenshots to `$FLOTILLA_SHOTS` (pass
/// `TEST_RUNNER_FLOTILLA_SHOTS` to xcodebuild). Pairing takes `TEST_RUNNER_FLOTILLA_LINK`, a link
/// from `flotilla pair`.
@MainActor
class FlotillaUITestCase: XCTestCase {
    let app = XCUIApplication()
    let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    var env: [String: String] { ProcessInfo.processInfo.environment }

    override func setUp() {
        continueAfterFailure = false
    }

    /// A screenshot, attached to the test and written to `$FLOTILLA_SHOTS` as
    /// `$FLOTILLA_SHOT_PREFIX<name>.png` (ios/screenshots and the video's scripts are made so).
    func shot(_ name: String) {
        let png = XCUIScreen.main.screenshot().pngRepresentation
        if let dir = env["FLOTILLA_SHOTS"] {
            try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            let prefix = env["FLOTILLA_SHOT_PREFIX"] ?? ""
            try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(prefix)\(name).png"))
        }
        let a = XCTAttachment(data: png, uniformTypeIdentifier: "public.png")
        a.name = name
        a.lifetime = .keepAlways
        add(a)
    }

    /// What's on screen (the app's, or `of`'s), as `$FLOTILLA_SHOTS/<name>.txt`, for working out
    /// a query.
    func dump(_ name: String, _ of: XCUIApplication? = nil) {
        if let dir = env["FLOTILLA_SHOTS"] {
            try? (of ?? app).debugDescription.write(toFile: "\(dir)/\(name).txt", atomically: true, encoding: .utf8)
        }
    }

    /// A line for the test's log (`[tag] line`) and for `$FLOTILLA_SHOTS/<file>`.
    func note(_ line: String, tag: String, file: String) {
        print("[\(tag)] \(line)")
        guard let dir = env["FLOTILLA_SHOTS"] else { return }
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let url = URL(fileURLWithPath: dir).appendingPathComponent(file)
        let text = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
        try? (text + line + "\n").write(to: url, atomically: true, encoding: .utf8)
    }

    func any(_ id: String) -> XCUIElement { app.descendants(matching: .any)[id] }

    /// Text on screen containing `text`.
    func showing(_ text: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", text)).firstMatch
    }

    /// A tap (`VideoUITests` also writes down where).
    func tap(_ element: XCUIElement) {
        element.tap()
    }

    // MARK: Threads

    /// The Threads list.
    func waitForList() {
        XCTAssertTrue(app.navigationBars["Threads"].waitForExistence(timeout: 20), "the threads list")
    }

    /// The Threads list with the server's threads in it: any, or `thread`'s row.
    func waitForThreads(_ thread: String? = nil) {
        waitForList()
        let row = thread.map { any("thread-\($0)") } ?? app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'thread-'")).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 20), "the threads, from the server")
    }

    /// Launched to Threads with the server's threads in it (any, or `thread`'s row), then, with
    /// `allow`, the notifications question answered if it comes within that many seconds.
    func launchToThreads(_ thread: String? = nil, allow: TimeInterval? = nil) {
        app.launch()
        waitForThreads(thread)
        if let allow { allowNotifications(timeout: allow) }
    }

    /// The system's question, the first time the app connects: allowed, so it covers nothing.
    func allowNotifications(timeout: TimeInterval) {
        let allow = springboard.buttons["Allow"]
        if allow.waitForExistence(timeout: timeout) { allow.tap() }
    }

    /// A thread, from its row on Threads.
    func open(_ thread: String) {
        let row = any("thread-\(thread)")
        XCTAssertTrue(row.waitForExistence(timeout: 20), "row \(thread)")
        tap(row)
        XCTAssertTrue(any("composer").waitForExistence(timeout: 10))
    }

    /// Settings, from the gear on Threads.
    func settings() {
        let gear = app.buttons["openSettings"]
        XCTAssertTrue(gear.waitForExistence(timeout: 10))
        gear.tap()
    }

    /// The model picker from the model chip: `search` typed, model `id` picked (as
    /// `<harness>/<model>`), then `effort` from the effort chip if it shows.
    func pickModel(search text: String, id: String, effort: String? = nil) {
        app.buttons["modelChip"].tap()
        let search = app.searchFields["Search models"]
        XCTAssertTrue(search.waitForExistence(timeout: 5), "the model picker")
        search.tap()
        search.typeText(text)
        let model = app.buttons["model-\(id)"]
        XCTAssertTrue(model.waitForExistence(timeout: 5))
        model.tap()
        if let effort, app.buttons["effortChip"].exists {
            app.buttons["effortChip"].tap()
            let choice = app.buttons[effort]
            if choice.waitForExistence(timeout: 3) { choice.tap() }
        }
    }

    // MARK: Pairing

    /// `$FLOTILLA_LINK`.
    func pairingLink() throws -> String {
        try XCTUnwrap(env["FLOTILLA_LINK"], "set TEST_RUNNER_FLOTILLA_LINK to a link from `flotilla pair`")
    }

    /// A pairing link opened, and the form filled in from it: with `server`, or any address.
    func openPairingLink(_ link: String, server: String? = nil) throws {
        app.open(try XCTUnwrap(URL(string: link)))
        let field = app.textFields["pairServer"]
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        let filled = server.map { NSPredicate(format: "value == %@", $0) } ?? NSPredicate(format: "value BEGINSWITH 'http'")
        expectation(for: filled, evaluatedWith: field)
        waitForExpectations(timeout: 10)
    }

    /// Launched forgetting the server it's paired with, then `$FLOTILLA_LINK` opened and the form
    /// filled in: Connect is next.
    func startPairing() throws {
        let link = try pairingLink()
        app.launchArguments = ["-FlotillaForgetServer"]
        app.launch()
        try openPairingLink(link)
    }
}
