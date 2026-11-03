import FlotillaKit
import Foundation
import Testing
@testable import Flotilla

@MainActor
@Suite struct ArchivingNotice {
    @Test func `the notice says what was archived, and a merge that did it`() {
        let now = Date()
        #expect(ArchiveNotice(ids: ["t1"], at: now).text == "Archived 1 thread")
        #expect(ArchiveNotice(ids: ["t1", "t2"], at: now).text == "Archived 2 threads")
        #expect(ArchiveNotice(ids: ["t1"], merged: ["t1": 12], at: now).text == "#12 merged · Archived")
        // A merge joined by other archives: counted with them.
        #expect(ArchiveNotice(ids: ["t1", "t2"], merged: ["t1": 12], at: now).text == "Archived 2 threads")
        // The merge's thread is the one left.
        #expect(ArchiveNotice(ids: ["t2"], merged: ["t1": 12], at: now).text == "Archived 1 thread")
    }

    @Test func `a starting thread's model, as a row says it`() {
        // Before the catalog comes (or for a model it doesn't name): the id, as words.
        #expect(Format.model(provider: "claude-code", model: "opus", effort: "high", fast: true, catalog: nil) == "Opus · high · fast")
        #expect(Format.model(provider: "codex", model: "gpt", effort: "none", fast: false, catalog: nil) == "GPT")
        #expect(Format.model(provider: "claude-code", model: "claude-sonnet-5", effort: nil, fast: false, catalog: nil) == "Sonnet 5")
    }
}
