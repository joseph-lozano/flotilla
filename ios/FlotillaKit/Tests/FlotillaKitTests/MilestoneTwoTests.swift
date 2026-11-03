import CoreGraphics
import CoreImage
import Foundation
import Testing
@testable import FlotillaKit

/// Pairing by QR code: the camera on a phone, a photo here. The simulator has no camera, so
/// these prove the path from a picture of a code to the link the form reads.
@Suite struct QRPairing {
    static let link = "flotilla://pair?u=https%3A%2F%2Fbox.tail1234.ts.net#K8C5CMU9G2VM"

    @Test func `a code drawn for Add device reads back as its link`() async throws {
        let image = try #require(QR.image(for: Self.link))
        #expect(await QR.texts(in: image) == [Self.link])
        let target = try #require(await QR.pairing(in: image))
        #expect(target.server == "https://box.tail1234.ts.net")
        #expect(target.code == "K8C5CMU9G2VM")
    }

    /// As a camera sees one held up to it: small, turned, on a grey background.
    @Test func `a photo of a code at an angle reads too`() async throws {
        let code = CIImage(cgImage: try #require(QR.image(for: Self.link, scale: 4)))
        let turned = code.transformed(by: CGAffineTransform(rotationAngle: 0.21)).transformed(by: CGAffineTransform(scaleX: 0.9, y: 0.9))
        let moved = turned.transformed(by: CGAffineTransform(translationX: -turned.extent.minX + 140, y: -turned.extent.minY + 90))
        let ground = CIImage(color: CIColor(red: 0.42, green: 0.38, blue: 0.45)).cropped(to: CGRect(x: 0, y: 0, width: 900, height: 700))
        let photo = moved.composited(over: ground)
        let image = try #require(CIContext().createCGImage(photo, from: photo.extent))
        #expect(await QR.pairing(in: image)?.code == "K8C5CMU9G2VM")
        // Core Image's detector, which the simulator falls back on, reads it too.
        #expect(QR.coreImageTexts(in: image) == [Self.link])
    }

    @Test func `codes that aren't pairing links are passed over`() async throws {
        let other = try #require(QR.image(for: "https://example.com/menu"))
        #expect(await QR.pairing(in: other) == nil)
        #expect(QR.pairing(from: "flotilla://pair?u=https%3A%2F%2Fa.ts.net") == nil, "no code")
        #expect(QR.pairing(from: "https://a.ts.net/pair#K8C5-CMU9-G2VM")?.code == "K8C5CMU9G2VM")
    }
}

/// The requests the phone makes, as Rust's serde reads them (the Rust side's own tests pin
/// `push.register`'s shape: `push_requests_on_the_wire`).
@Suite struct NewRequests {
    @Test func `push register and unregister`() {
        #expect(Up.req(id: 3, .registerPush(token: "00ff", env: "sandbox", topic: "com.useflotilla.flotilla")).json.text
            == #"{"id":3,"m":"push.register","p":{"env":"sandbox","token":"00ff","topic":"com.useflotilla.flotilla"},"t":"req"}"#)
        #expect(Up.req(id: 4, .unregisterPush).json.text == #"{"id":4,"m":"push.unregister","t":"req"}"#)
    }

    @Test func `rename and delete, a thread's and a device's`() {
        // `the_envelope_reads_as_the_plan_sketches_it` pins `devices.rename` on the Rust side.
        #expect(Up.req(id: 2, .renameDevice("0123456789abcdef", name: "Work phone")).json.text
            == #"{"id":2,"m":"devices.rename","p":{"device":"0123456789abcdef","name":"Work phone"},"t":"req"}"#)
        #expect(Up.req(id: 5, .rename("t1", title: "Fix the flaky test")).json.text
            == #"{"id":5,"m":"thread.rename","p":{"thread":"t1","title":"Fix the flaky test"},"t":"req"}"#)
        #expect(Up.req(id: 6, .delete("t1")).json.text == #"{"id":6,"m":"thread.delete","p":"t1","t":"req"}"#)
    }

    @Test func `thread new carries every field, the client's id and its files`() throws {
        let photo = Attached(name: "photo-1.jpg", size: 1234, kind: .image, blob: "b_" + String(repeating: "ab", count: 32))
        let t = NewThread(id: "6f0c…", environment: "flotilla-web", projectID: "p1", provider: "claude-code", model: "haiku", effort: "low", fast: false, prompt: "Describe it",
                          attachments: [photo], onBranch: OnBranch(name: "fix-login", base: "main"))
        let p = try #require(Request.newThread(t).params)
        #expect(Request.newThread(t).method == "thread.new")
        #expect(Set(p.object?.keys.map { $0 } ?? []) == ["environment", "project_id", "provider", "model", "effort", "fast", "title", "machine_type", "prompt", "attachments", "wait_ready", "on_branch", "id", "parent"])
        #expect(p["id"] == "6f0c…")
        #expect(p["on_branch"] == ["name": "fix-login", "base": "main"])
        #expect(p["attachments"] == [["path": "", "name": "photo-1.jpg", "size": 1234, "kind": "image", "blob": .string(photo.blob)]])
        let none = NewThread(id: "x", environment: "base", projectID: nil, provider: "codex", model: "gpt", effort: nil, fast: true, prompt: "go")
        #expect(Request.newThread(none).params?["project_id"] == .null && Request.newThread(none).params?["on_branch"] == .null)
        #expect(Request.newThread(none).resendable)
        let send = Request.send(thread: "t1", text: "see", files: [photo], local: "local-1")
        #expect(send.params?["files"]?.array?.count == 1)
    }

    @Test func `a pairing code answer, and branches as progress`() throws {
        let reply = try #require(Reply.result(try JSON(parsing: #"{"Ok":{"code":{"code":"K8C5CMU9G2VM","expires_ms":1800000000000,"link":"flotilla://pair?u=x#K8C5CMU9G2VM"}}}"#)))
        guard case .success(.code(let c)) = reply else { Issue.record("\(reply)"); return }
        #expect(c.code == "K8C5CMU9G2VM" && c.expiresMs == 1_800_000_000_000)
        #expect(!"\(c)".contains("K8C5"))
        let down = try #require(Down(text: #"{"t":"progress","id":9,"note":{"kind":"branches","data":{"project":"p1","result":{"Ok":{"prs":[{"number":3,"title":"Fix login","url":"u","head":"fix-login","base":"next","draft":false,"from_fork":false},{"number":4,"title":"Theirs","url":"u","head":"x","base":"main","draft":true,"from_fork":true}],"branches":["spike"]}}}},"n":2}"#))
        guard case .progress(9, let note, 2) = down, let (project, result) = note.branches, case .success(let lists) = result else { Issue.record("\(down)"); return }
        #expect(project == "p1" && lists.prs.map(\.number) == [3, 4] && lists.prs[1].fromFork && lists.branches == ["spike"])
        let failed = try #require(Down(text: #"{"t":"progress","id":9,"note":{"kind":"branches","data":{"project":"p1","result":{"Err":"gh isn't signed in"}}}}"#))
        guard case .progress(_, let n, _) = failed, case .failure(let e)? = n.branches?.result else { Issue.record("\(failed)"); return }
        #expect(e.message == "gh isn't signed in")
        let checked = try #require(Down(text: #"{"t":"progress","id":2,"note":{"kind":"branch_checked","data":{"project":"p1","branch":{"name":"b","base":"main"},"prompt":"Fix it","result":{"Ok":null}}}}"#))
        guard case .progress(_, let c2, _) = checked, let bc = c2.branchChecked else { Issue.record("\(checked)"); return }
        #expect(bc.branch == OnBranch(name: "b", base: "main") && bc.prompt == "Fix it")
    }
}

@Suite struct CatalogChoices {
    static let catalog = try! Catalog(json: JSON(parsing: """
        {"providers":[
          {"key":"claude-code","cli_name":"claude","aliases":["claude-code"],"default_model":"haiku","order":1,"models":[
            {"id":"haiku","label":"Haiku 5.5","sdk_id":null,"fast_mode":false,"effort":{"supported":["low","high"],"default":"low"}},
            {"id":"opus","label":"Opus 5.5","sdk_id":null,"fast_mode":true,"effort":{"supported":["low","medium","high"],"default":"high"}}]},
          {"key":"codex","cli_name":"codex","aliases":[],"default_model":null,"order":2,"models":[{"id":"gpt-6.1-sol","label":"GPT-6.1 Sol","sdk_id":null,"fast_mode":false,"effort":null}]},
          {"key":"pi","cli_name":"pi","aliases":[],"default_model":null,"order":3,"models":[{"id":"m","label":null,"sdk_id":null,"fast_mode":false,"effort":null}]}
        ]}
        """))

    @Test func `settings are checked as Rust checks them`() {
        let c = Self.catalog
        #expect(c.validate(provider: "claude", model: "opus", effort: nil, fast: true) == ThreadSettings(provider: "claude-code", model: "opus", effort: "high", fast: true))
        #expect(c.validate(provider: "claude-code", model: "haiku", effort: "medium", fast: false) == nil)
        #expect(c.validate(provider: "claude-code", model: "haiku", effort: nil, fast: true) == nil)
        #expect(c.validate(provider: "codex", model: "gpt-6.1-sol", effort: "high", fast: false) == nil)
        #expect(c.validate(provider: "codex", model: "gpt-6.1-sol", effort: nil, fast: false)?.effort == nil)
        #expect(c.defaultSettings == ThreadSettings(provider: "claude-code", model: "haiku", effort: "low", fast: false))
        #expect(Catalog.offered("codex") && !Catalog.offered("pi"))
    }

    @Test func `boat's limits on a prompt's files`() {
        let mb = AttachLimits.mb
        #expect(AttachLimits.overLimit([]) == nil)
        #expect(AttachLimits.overLimit([("a.png", 25 * mb)]) == nil)
        #expect(AttachLimits.overLimit([("big.mov", 25 * mb + 1)]) == "big.mov is over 25 MB")
        #expect(AttachLimits.overLimit(Array(repeating: ("a", 1), count: 12)) == nil)
        #expect(AttachLimits.overLimit(Array(repeating: ("a", 1), count: 13)) == "12 files at most")
        #expect(AttachLimits.overLimit(Array(repeating: ("a", 22 * mb), count: 3)) == "Over 64 MB in total")
        #expect(FileKind(name: "IMG_1.HEIC") == .image && FileKind(name: "notes.md") == .text && FileKind(name: "Makefile") == .other)
    }

    @Test func `the settings view's checks and the devices view`() throws {
        let settings = try ServerView(key: .settings, data: JSON(parsing: #"{"defaults":{"provider":"claude-code","model":"haiku","effort":"low","fast":false},"idle_minutes":10,"doctor":{"checks":[{"check":"boat_key","health":"ok","says":"Works","fix":[]},{"check":"push","health":"off","says":"Not set up","fix":["flotilla setup --apns-key AuthKey_<KEY ID>.p8 --apns-team-id <TEAM ID>"]}],"at_ms":5}}"#))
        guard case .settings(let s) = settings else { Issue.record("\(settings)"); return }
        #expect(s.defaults?.model == "haiku" && s.checks?.map(\.label) == ["boat API key", "Push"] && s.checks?[1].health == "off" && s.checkedMs == 5)
        guard case .settings(let unchecked) = try ServerView(key: .settings, data: JSON(parsing: #"{"doctor":null}"#)) else { return }
        #expect(unchecked.checks == nil)
        #expect(!s.stopsThreads && !unchecked.stopsThreads)
        let devices = try ServerView(key: .devices, data: JSON(parsing: #"{"devices":[{"id":"0123456789abcdef","name":"My iPhone","platform":"ios","paired_ms":1,"last_seen_ms":2,"connected":true}]}"#))
        guard case .devices(let d) = devices else { Issue.record("\(devices)"); return }
        #expect(d.map(\.name) == ["My iPhone"] && d[0].connected)
        #expect(throws: ReadError.self) { try ServerView(key: .devices, data: JSON(parsing: #"{"devices":[{"name":"no id"}]}"#)) }
    }
}

/// Which failing checks take New thread's place, as `Doctor::stops_threads` says: only the boat
/// key's or the database's.
@Test func `only a failing check threads need stops them`() throws {
    func settings(_ checks: [(String, String)]) throws -> ServerSettings {
        let list = checks.map { #"{"check":"\#($0.0)","health":"\#($0.1)","says":"","fix":[]}"# }.joined(separator: ",")
        guard case .settings(let s) = try ServerView(key: .settings, data: JSON(parsing: #"{"doctor":{"checks":[\#(list)],"at_ms":1}}"#)) else { throw ReadError("not settings") }
        return s
    }
    #expect(try !settings(["git_hub", "claude", "tailscale", "others", "push", "mcp"].map { ($0, "fail") }).stopsThreads)
    for needed in ["boat_key", "database"] {
        #expect(try settings([("push", "off"), (needed, "fail")]).stopsThreads, "\(needed)")
        for health in ["ok", "warn", "off"] { #expect(try !settings([(needed, health)]).stopsThreads, "\(needed) \(health)") }
    }
}

/// The setup's count, as `Doctor::tally` says: optional parts that are off (remote MCP, push)
/// aren't counted; a warning or a failure is.
@Test func `optional parts that are off leave the setup count whole`() throws {
    func settings(_ checks: [(String, String)]) throws -> ServerSettings {
        let list = checks.map { #"{"check":"\#($0.0)","health":"\#($0.1)","says":"","fix":[]}"# }.joined(separator: ",")
        guard case .settings(let s) = try ServerView(key: .settings, data: JSON(parsing: #"{"doctor":{"checks":[\#(list)],"at_ms":1}}"#)) else { throw ReadError("not settings") }
        return s
    }
    let off = try settings([("boat_key", "ok"), ("database", "ok"), ("push", "off"), ("mcp", "off")])
    #expect(off.tally == (2, 2))
    #expect(off.checks?.last?.label == "Remote MCP")
    #expect(try settings([("boat_key", "ok"), ("claude", "warn"), ("mcp", "fail")]).tally == (1, 3))
}

/// `PUT /v1/blobs` against a stand-in server: the token in its header, the bytes as the body.
@Suite(.serialized) struct BlobUploads {
    final class Stub: URLProtocol, @unchecked Sendable {
        nonisolated(unsafe) static var seen: [(method: String, auth: String?, body: Int)] = []
        nonisolated(unsafe) static var answer: (Int, String) = (200, "")

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
        override func startLoading() {
            var n = request.httpBody?.count ?? 0
            if let stream = request.httpBodyStream {
                stream.open()
                var buf = [UInt8](repeating: 0, count: 4096)
                while stream.hasBytesAvailable {
                    let r = stream.read(&buf, maxLength: buf.count)
                    if r <= 0 { break }
                    n += r
                }
                stream.close()
            }
            Self.seen.append((request.httpMethod ?? "", request.value(forHTTPHeaderField: "Authorization"), n))
            let (status, body) = Self.answer
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        }
        override func stopLoading() {}
    }

    func session() -> URLSession {
        let c = URLSessionConfiguration.ephemeral
        c.protocolClasses = [Stub.self]
        return URLSession(configuration: c)
    }

    @Test func `uploads with the device's token and reads the blob's id`() async throws {
        Stub.seen = []
        Stub.answer = (200, #"{"id":"b_\#(String(repeating: "cd", count: 32))","size":5}"#)
        let blob = try await Uploads.put(Data("hello".utf8), server: "http://127.0.0.1:7791", token: "fdt_x_y", session: session())
        #expect(blob.id.hasPrefix("b_cdcd") && blob.size == 5)
        #expect(Stub.seen.count == 1 && Stub.seen[0].method == "PUT" && Stub.seen[0].auth == "Bearer fdt_x_y" && Stub.seen[0].body == 5)
    }

    @Test func `a refused token says pair again`() async {
        Stub.answer = (401, "This device isn't paired")
        await #expect(throws: Uploads.Problem("This device isn't paired with this server any more. Pair it again.", unpaired: true)) {
            try await Uploads.put(Data("x".utf8), server: "http://127.0.0.1:7791", token: "fdt_x_y", session: session())
        }
        await #expect(throws: Uploads.Problem.self) { try await Uploads.put(Data(), server: "http://127.0.0.1:7791", token: "t", session: session()) }
    }
}
