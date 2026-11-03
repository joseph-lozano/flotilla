import Foundation
import Testing
@testable import FlotillaKit

/// Public links: Make public… and Make private as Rust's serde reads them, a thread view's
/// public links as it writes them (`phone_preview_and_services_on_the_wire` pins both on the
/// Rust side), and the time left in the words `flotilla_proto::public::time_left` uses.
@Suite struct PublicLinksTests {
    /// A thread view's public links, as `serde_json` writes `Vec<flotilla_proto::PublicLink>`.
    static let wire = #"[{"port":3000,"url":"https://bx1-3000.example.com","since_ms":1800000000000,"until_ms":1800003600000},{"port":4100,"url":"https://bx1-4100.example.com","since_ms":1800000000000,"until_ms":null}]"#

    @Test func `Make public and Make private, as Rust reads them`() {
        for (i, (duration, word)) in zip(PublicFor.allCases, ["hour", "day", "week", "until_stopped"]).enumerated() {
            let id = ReqID(11 + i)
            #expect(Up.req(id: id, .makePublic("t1", port: 3000, for: duration)).json.text == #"{"id":\#(id),"m":"thread.make_public","p":{"for":"\#(word)","port":3000,"thread":"t1"},"t":"req"}"#)
        }
        #expect(Up.req(id: 15, .makePrivate("t1", port: 3000)).json.text == #"{"id":15,"m":"thread.make_private","p":{"port":3000,"thread":"t1"},"t":"req"}"#)
        #expect(PublicFor.allCases.map(\.label) == ["1 hour", "1 day", "1 week", "Until I stop it"])
    }

    @Test func `a thread view's public links, as Rust writes them`() throws {
        let d = try ThreadDetail(json: JSON(parsing: #"{"id": "t1", "public": \#(Self.wire)}"#))
        #expect(d.publicLinks == [
            PublicLink(port: 3000, url: "https://bx1-3000.example.com", sinceMs: 1_800_000_000_000, untilMs: 1_800_003_600_000),
            PublicLink(port: 4100, url: "https://bx1-4100.example.com", sinceMs: 1_800_000_000_000, untilMs: nil),
        ])
        // A server from before public links sends none; one without a port or URL is left out.
        #expect(try ThreadDetail(json: JSON(parsing: #"{"id": "t1"}"#)).publicLinks.isEmpty)
        #expect(try ThreadDetail(json: JSON(parsing: #"{"id": "t1", "public": [{"port": 1}, {"url": "x"}]}"#)).publicLinks.isEmpty)
        #expect(throws: ReadError.self) { try ThreadDetail(json: JSON(parsing: #"{"id": "t1", "public": [1]}"#)) }
        // A view whose public links change isn't the same view.
        #expect(try ThreadDetail(json: JSON(parsing: #"{"id": "t1"}"#)) != d)
    }

    /// The table in `flotilla_proto::public`'s `time_left_rounds_up_in_the_largest_unit_that_reads`.
    @Test func `time left reads as the desktop says it`() {
        let minute: Int64 = 60_000, hour = 60 * minute, day = 24 * hour
        let table: [(Int64, String)] = [
            (-5, "ending"), (0, "ending"), (1, "less than a minute left"), (59_999, "less than a minute left"),
            (minute, "1 min left"), (minute + 1, "2 min left"), (58 * minute, "58 min left"), (hour - 1, "60 min left"),
            (hour, "1 h left"), (hour + 1, "2 h left"), (day, "24 h left"), (day - 1, "24 h left"), (2 * day, "48 h left"),
            (2 * day + 1, "3 days left"), (7 * day, "7 days left"),
        ]
        for (ms, words) in table {
            #expect(publicTimeLeft(ms) == words, "\(ms)")
        }
        let link = PublicLink(port: 3000, url: "https://x", untilMs: hour)
        #expect(link.words(nowMs: 0) == "Public · 1 h left")
        #expect(PublicLink(port: 3000, url: "https://x").words(nowMs: 0) == "Public until you stop it")
        #expect(PublicLink(port: 3000, url: "https://x").left(nowMs: 0) == nil)
    }
}
