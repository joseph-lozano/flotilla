import Foundation
import Testing
@testable import Flotilla

/// The hold that keeps the first row on screen still as older messages go in above it.
@MainActor
@Suite struct KeepPlaceHold {
    func keep(_ tops: [String: CGFloat]) -> KeepPlace {
        let keep = KeepPlace()
        keep.last = .init(offset: 100, inset: 50)
        keep.tops = tops
        return keep
    }

    @Test func `the first row on screen stays where it was until a scroll lets go`() {
        let keep = keep(["m1": -20, "m2": 30, "m3": 300])
        keep.note()
        #expect(keep.anchor?.id == "m2", "the first row whose top is on screen")
        // A page went in above and pushed it down 40 pt: scrolled down as much.
        #expect(keep.correction("m2", 70) == 190)
        #expect(keep.correction("m3", 340) == nil, "only the row kept")
        #expect(keep.correction("m2", 30.2) == nil, "not moved")
        keep.release()
        #expect(keep.anchor == nil)
        #expect(keep.correction("m2", 70) == nil, "let go")
    }

    @Test func `a finger on the page is never held against`() {
        let keep = keep(["m2": 30])
        keep.touched = true
        keep.note()
        #expect(keep.anchor == nil)
    }
}
