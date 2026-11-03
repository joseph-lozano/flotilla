import XCTest

/// A long thread (`dev-db.py --stress`): how long it takes to open, older messages loading as
/// the page scrolls up, and a cut tool output shown whole. Run after `test1Pairing` against a
/// server on that database. Timings go to the test's log and to `$FLOTILLA_SHOTS/timings.txt`.
final class PagingUITests: FlotillaUITestCase {
    func note(_ line: String) {
        note(line, tag: "paging", file: "timings.txt")
    }

    /// The stress thread's row, scrolled to.
    func stressRow() -> XCUIElement {
        let row = app.descendants(matching: .any)["thread-\(DevDB.stress)"]
        for _ in 0..<6 where !(row.exists && row.isHittable) {
            app.swipeUp()
        }
        XCTAssertTrue(row.waitForExistence(timeout: 10), "the stress thread's row")
        return row
    }

    /// Tap the stress thread and time it until its last reply is on screen.
    func test30OpenStress() {
        launchToThreads()
        let row = stressRow()
        let started = Date()
        row.tap()
        let last = showing("of 220.")
        XCTAssertTrue(last.waitForExistence(timeout: 120), "the thread's last reply")
        let ms = Int(Date().timeIntervalSince(started) * 1000)
        note("open stress thread: \(ms) ms to its last reply")
        sleep(1)
        shot("30-stress-open")
    }

    /// Scrolled up to the oldest message shown, the page above loads, and what's on screen
    /// stays where it is.
    func test31ScrollUpLoadsOlder() {
        // Pages slowed to 6 s, so what's on screen can be seen before and after one goes in.
        app.launchArguments = ["-FlotillaSlowHistory", "6000"]
        launchToThreads()
        stressRow().tap()
        XCTAssertTrue(showing("of 220.").waitForExistence(timeout: 60), "the thread's last reply")
        let loader = app.descendants(matching: .any)["olderMessages"]
        for _ in 0..<40 where !loader.exists {
            app.swipeDown(velocity: .fast)
        }
        XCTAssertTrue(loader.exists, "scrolled up to the oldest message shown")
        // A reply on screen now, to see it stay put; the turn two before it is on the page above.
        let screen = app.windows.firstMatch.frame
        let shown = (200...216).reversed().lazy.map { self.showing("Turn \($0) done.") }.first { $0.exists && $0.frame.minY > screen.minY + 120 && $0.frame.maxY < screen.maxY - 200 }
        let anchor = shown ?? showing("of 220.")
        XCTAssertNotNil(shown, "a reply on screen")
        let label = anchor.label
        let before = anchor.frame
        let turn = Int(label.dropFirst(5).prefix { $0.isNumber }) ?? 0
        let older = showing("Turn \(turn - 2) done.")
        XCTAssertFalse(older.exists, "turn \(turn - 2)'s reply isn't loaded yet")
        // From a snapshot: the loader can go from the tree as a page pushes it up.
        note("anchor \(label.prefix(16)) at \(before.minY), loader at \((try? loader.snapshot())?.frame.minY ?? .nan)")
        shot("31-loading-earlier-messages")
        sleep(9)
        shot("32-older-messages-loaded")
        let same = app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", label)).firstMatch
        note("after: anchor exists \(same.exists) at \(same.exists ? same.frame.minY : -1), loader exists \(loader.exists)")
        XCTAssertTrue(same.exists)
        let moved = abs(same.frame.minY - before.minY)
        XCTAssertLessThanOrEqual(moved, 2, "the page keeps its place as older messages go in above")
        for _ in 0..<8 where !older.exists {
            app.swipeDown()
        }
        XCTAssertTrue(older.exists, "turn \(turn - 2)'s reply, from the page above")
        note("scrolled up: the page above turn \(turn) loaded, turn \(turn - 2)'s reply shown; what was on screen moved \(moved) pt as it went in")
        shot("33-older-messages-scrolled-to")
    }

    /// Flung up page after page: older messages keep coming, and whenever "Loading earlier
    /// messages…" is on screen with the finger off, the page above comes (a loader left in sight
    /// after a page went in used to stay there for good, two or three pages up).
    func test33FlingUpKeepsLoading() {
        launchToThreads()
        stressRow().tap()
        XCTAssertTrue(showing("220 of 220.").waitForExistence(timeout: 60), "the thread's last reply")
        sleep(1)
        let loader = app.descendants(matching: .any)["olderMessages"]
        let screen = app.windows.firstMatch.frame
        // Read from one snapshot each: rows come and go as pages go in.
        func loaderTop() -> CGFloat? { (try? loader.snapshot())?.frame.minY }
        func inSight() -> Bool {
            guard let s = try? loader.snapshot() else { return false }
            return s.frame.maxY > screen.minY && s.frame.minY < screen.maxY && s.frame.height > 0
        }
        // The oldest turn whose reply is drawn.
        func oldest() -> Int {
            guard let root = try? app.snapshot() else { return 220 }
            var turns: [Int] = []
            func walk(_ e: XCUIElementSnapshot) {
                if e.label.hasPrefix("Turn "), e.label.contains(" done."), let n = Int(e.label.dropFirst(5).prefix { $0.isNumber }) { turns.append(n) }
                e.children.forEach(walk)
            }
            walk(root)
            return turns.min() ?? 220
        }
        // Ten pages and more above the view (its newest 100-149 messages are turns 210 on).
        let goal = 175
        var turn = 220
        var swipes = 0
        var waits = 0
        while turn > goal && swipes < 60 {
            app.swipeDown(velocity: .fast)
            swipes += 1
            turn = min(turn, oldest())
            guard inSight() else { continue }
            // The loader on screen, the finger off: the page above has to come. Seen as older
            // turns drawn, or the loader pushed well up by a page going in under it (the page
            // stays where it is); losing sight of it otherwise doesn't count.
            let was = turn
            let top = loaderTop()
            let started = Date()
            var came = false
            while Date().timeIntervalSince(started) < 15 {
                if oldest() < was {
                    came = true
                    break
                }
                if let top, let now = loaderTop(), now < top - 100 {
                    came = true
                    break
                }
                usleep(250_000)
            }
            waits += 1
            turn = min(turn, oldest())
            note("swipe \(swipes): loader on screen above turn \(was); \(came ? "the page above came in \(Int(Date().timeIntervalSince(started) * 1000)) ms" : "nothing came")")
            if !came { shot("37-fling-stuck") }
            XCTAssertTrue(came, "the page above turn \(was) loads while the loader's on screen")
        }
        note("flung up: turn \(turn) after \(swipes) swipes, the loader on screen \(waits) times")
        shot("37-flung-up")
        XCTAssertLessThanOrEqual(turn, goal, "older messages kept coming")
    }

    /// A tool output cut on the wire: its row says so, and Show all brings the whole of it.
    func test32CutOutputShowsAll() {
        launchToThreads()
        stressRow().tap()
        XCTAssertTrue(showing("of 220.").waitForExistence(timeout: 60), "the thread's last reply")
        let row = app.buttons.matching(NSPredicate(format: "label CONTAINS 'cat logs/retry.log'")).firstMatch
        for _ in 0..<30 where !(row.exists && row.isHittable) {
            unfoldCalls()
            if !(row.exists && row.isHittable) { app.swipeDown() }
        }
        row.tap()
        let showAll = app.buttons["showAll"]
        XCTAssertTrue(showAll.waitForExistence(timeout: 5), "Show all on the cut output")
        sleep(1)
        shot("34-cut-output")
        showAll.tap()
        let size = app.staticTexts["wholeSize"]
        XCTAssertTrue(size.waitForExistence(timeout: 30), "the whole output")
        let text = app.textViews["wholeText"]
        let value = text.value as? String ?? ""
        XCTAssertTrue(value.contains("[000000]") && value.contains("[005199]"), "every line, the last included")
        note("show all: \(size.label)")
        sleep(1)
        shot("35-cut-output-whole")
        app.buttons["wholeToEnd"].tap()
        sleep(2)
        shot("36-cut-output-whole-end")
    }
}
