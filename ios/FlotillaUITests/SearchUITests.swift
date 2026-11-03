import XCTest

/// Search on the Threads list: threads by title as you type, threads found by their messages
/// below (each once, at its best message), a hit far up a long thread (`dev-db.py --stress`)
/// opening the thread at that message, and a pick remembered by the server. Run after
/// `test1Pairing` against a server on `dev-db.py --pr --states --project --fix --delegate --stress`.
final class SearchUITests: FlotillaUITestCase {
    func note(_ line: String) {
        note(line, tag: "search", file: "search.txt")
    }

    /// On screen, not only somewhere in the list.
    func onScreen(_ e: XCUIElement) -> Bool {
        let screen = app.windows.firstMatch.frame
        return e.exists && e.frame.minY >= screen.minY && e.frame.maxY <= screen.maxY && e.frame.height > 0
    }

    func search(_ text: String) {
        let field = app.searchFields.firstMatch
        if !field.exists { app.swipeDown() }
        XCTAssertTrue(field.waitForExistence(timeout: 5), "the search field")
        field.tap()
        if let typed = field.value as? String, !typed.isEmpty, typed != field.placeholderValue {
            field.buttons["Clear text"].firstMatch.tap()
        }
        field.typeText(text)
    }

    /// Typing lists the threads by title at once, and below them the threads found by what was
    /// said in them, each once: one listed by its title isn't listed again.
    func test50SearchTitlesAndMessages() {
        launchToThreads()
        shot("50-threads")
        search("uname")
        let hits = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'hit:'"))
        XCTAssertTrue(hits.firstMatch.waitForExistence(timeout: 10), "threads by their messages, from the server")
        sleep(1)
        XCTAssertTrue(app.descendants(matching: .any)["thread-\(DevDB.stress)"].exists == false, "a thread without the word isn't listed")
        let titles = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'thread-'"))
        XCTAssertEqual(titles.count, 1, "the thread with the word in its title")
        XCTAssertEqual(hits.count, 2, "two more threads, by their messages, once each")
        let ids = (0..<hits.count).map { hits.element(boundBy: $0).identifier.split(separator: ":")[1] }
        XCTAssertEqual(Set(ids).count, ids.count, "one row a thread")
        XCTAssertFalse(ids.contains { titles.firstMatch.identifier.hasSuffix(String($0)) }, "not the one its title lists")
        note("\"uname\": \(titles.count) thread by title, \(hits.count) by their messages")
        shot("51-search-titles-and-messages")
    }

    /// A hit on page 3 of 62 opens the thread at it: the pages down to it come, and its row is
    /// on screen, marked, and stays.
    func test51HitOpensTheThreadAtIt() {
        launchToThreads()
        search("Turn 12:")
        let hit = app.descendants(matching: .any)["hit:\(DevDB.stress):157"]
        XCTAssertTrue(hit.waitForExistence(timeout: 10), "turn 12's prompt, found")
        sleep(1)
        shot("52-search-a-hit-far-up")
        let started = Date()
        hit.tap()
        let prompt = showing("Turn 12: look at the retry module")
        let deadline = Date().addingTimeInterval(60)
        while !onScreen(prompt) && Date() < deadline { usleep(50_000) }
        let ms = Int(Date().timeIntervalSince(started) * 1000)
        XCTAssertTrue(onScreen(prompt), "turn 12's prompt on screen")
        XCTAssertFalse(onScreen(showing("of 220.")), "not the end of the thread")
        note("tap → turn 12's prompt on screen (page 3 of 62, 57 pages fetched): \(ms) ms, at y \(prompt.frame.minY)")
        shot("53-opened-at-the-hit-marked")
        sleep(3)
        XCTAssertTrue(onScreen(prompt), "it stays on screen as the pages around it settle")
        note("3 s later: turn 12's prompt at y \(prompt.frame.minY)")
        shot("54-opened-at-the-hit-after")
        // The turn's reply follows it.
        XCTAssertTrue(showing("Turn 12 done.").exists)
    }

    /// A query with no words (a lone quote) is no query: the plain list, in its groups. And a
    /// message Flotilla brought says who from, as the thread does, without the marker and ids the
    /// agent reads.
    func test52NoWordsAndWhoFlotillaBroughtItFrom() {
        launchToThreads()
        search("\"")
        let idle = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'Idle, '")).firstMatch
        XCTAssertTrue(idle.waitForExistence(timeout: 5), "the plain list, in its groups")
        XCTAssertFalse(showing("Nothing matches").exists)
        sleep(1)
        shot("55-search-a-lone-quote")
        search("agents answered")
        let hits = { (has: String) in self.app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'hit:' AND label CONTAINS %@", has)) }
        XCTAssertTrue(hits("2 agents answered").firstMatch.waitForExistence(timeout: 10), "the agents' answers, said as the thread says them")
        XCTAssertEqual(hits("[flotilla]").count, 0, "no marker")
        XCTAssertEqual(hits("You:").count, 0, "not yours")
        note("\"agents answered\": \(hits("2 agents answered").count) hits from \"2 agents answered\"")
        sleep(1)
        shot("56-search-who-flotilla-brought-it-from")
    }

    /// A thread opened from a search is remembered for that query, by the server: typed again,
    /// it comes first among the threads whose titles have the word.
    func test53APickComesFirstWhenTypedAgain() {
        launchToThreads()
        search("check")
        let rows = app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH 'thread-'"))
        XCTAssertTrue(rows.element(boundBy: 1).waitForExistence(timeout: 5), "two threads by title")
        let order = { (0..<rows.count).map { rows.element(boundBy: $0) }.sorted { $0.frame.minY < $1.frame.minY }.map(\.identifier) }
        let before = order()
        note("\"check\" before: \(before)")
        shot("57-search-before-a-pick")
        let second = app.descendants(matching: .any)[before[1]]
        second.tap()
        let back = app.navigationBars.buttons.element(boundBy: 0)
        XCTAssertTrue(back.waitForExistence(timeout: 10), "the thread opened")
        sleep(2)
        shot("58-search-picked-thread-open")
        back.tap()
        XCTAssertTrue(app.searchFields.firstMatch.waitForExistence(timeout: 10), "back on Threads")
        search("check")
        XCTAssertTrue(rows.element(boundBy: 1).waitForExistence(timeout: 5))
        let deadline = Date().addingTimeInterval(10)
        while order().first != before[1] && Date() < deadline { usleep(100_000) }
        let after = order()
        note("\"check\" after picking \(before[1]): \(after)")
        XCTAssertEqual(after.first, before[1], "the pick comes first")
        shot("59-search-after-a-pick")
    }
}
