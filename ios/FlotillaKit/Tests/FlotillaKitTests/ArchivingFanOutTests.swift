import Foundation
import Testing
@testable import FlotillaKit

/// Settings → Archiving and the notes for archives the server does by itself, as
/// `flotilla_proto` writes them.
@Suite struct Archiving {
    @Test func `settings set writes the key and value Rust reads`() {
        #expect(Up.req(id: 5, .set(.archiveOnMerge(false))).json.text == #"{"id":5,"m":"settings.set","p":{"key":"archive_on_merge","value":false},"t":"req"}"#)
        #expect(Up.req(id: 6, .set(.archiveIdleDays(14))).json.text == #"{"id":6,"m":"settings.set","p":{"key":"archive_idle_days","value":14},"t":"req"}"#)
        #expect(Setting.idleDayChoices == [0, 3, 7, 14, 30])
    }

    @Test func `the settings view's archiving, and its defaults when a server leaves them out`() throws {
        guard case .settings(let s) = try ServerView(key: .settings, data: JSON(parsing: #"{"defaults":null,"archive_on_merge":false,"archive_idle_days":7}"#)) else {
            Issue.record("not settings"); return
        }
        #expect(!s.archiveOnMerge && s.archiveIdleDays == 7)
        guard case .settings(let old) = try ServerView(key: .settings, data: JSON(parsing: #"{"defaults":null}"#)) else { Issue.record("not settings"); return }
        #expect(old.archiveOnMerge && old.archiveIdleDays == 0)
    }

    @Test func `pr merged says whether the thread was archived for it`() throws {
        let archived = try #require(Down(text: #"{"t":"note","note":{"kind":"pr_merged","data":{"thread":"t1","number":12,"title":"Fix login","branch":"main","archived":true}},"n":4}"#))
        guard case .note(let n, 4) = archived, let m = n.prMerged else { Issue.record("\(archived)"); return }
        #expect(m == PRMerged(thread: "t1", number: 12, title: "Fix login", branch: "main", archived: true))
        // From a server before archiving: not archived.
        let older = try #require(Down(text: #"{"t":"note","note":{"kind":"pr_merged","data":{"thread":"t1","number":12,"title":"Fix login","branch":"main"}}}"#))
        guard case .note(let o, _) = older else { Issue.record("\(older)"); return }
        #expect(o.prMerged?.archived == false)
        #expect(o.idleArchived == nil)
    }

    @Test func `idle archived lists the threads`() throws {
        let down = try #require(Down(text: #"{"t":"note","note":{"kind":"idle_archived","data":["t1","t2"]},"n":9}"#))
        guard case .note(let n, _) = down else { Issue.record("\(down)"); return }
        #expect(n.idleArchived == ["t1", "t2"])
        #expect(n.prMerged == nil)
    }
}

/// One prompt on several models: New thread's requests and refusals, as the desktop's.
@Suite struct FanOutStarts {
    /// Rust's `catalog::tests::catalog`, as the catalog view carries it.
    static let catalog = try! Catalog(json: JSON(parsing: """
        {"providers":[
          {"key":"claude-code","cli_name":"claude","aliases":[],"default_model":"opus","models":[
            {"id":"opus","label":"Opus 5.5","fast_mode":true,"effort":{"supported":["low","medium","high","max"],"default":"high"}},
            {"id":"haiku","label":"Haiku 5.5","fast_mode":false,"effort":{"supported":["low","medium","high"],"default":"medium"}}]},
          {"key":"codex","cli_name":"codex","aliases":[],"default_model":null,"models":[
            {"id":"gpt-terra","label":"GPT-6.1 Terra","fast_mode":false,"effort":{"supported":["minimal","low","medium","high","xhigh"],"default":"medium"}},
            {"id":"plain","label":null,"fast_mode":false,"effort":null}]}
        ]}
        """))

    /// `catalog::tests::several_models_each_get_an_effort_and_fast_that_suit_them`, case for case.
    @Test func `each model gets an effort and fast that suit it`() {
        let c = Self.catalog
        func fit(_ p: String, _ m: String, _ e: String?, _ fast: Bool) -> (String?, Bool)? {
            c.fit(provider: p, model: m, effort: e, fast: fast).map { ($0.effort, $0.fast) }
        }
        // The same level where the model has it.
        #expect(fit("codex", "gpt-terra", "high", false)! == ("high", false))
        // Max on a model without it: the nearest it has.
        #expect(fit("codex", "gpt-terra", "max", false)! == ("xhigh", false))
        #expect(fit("claude-code", "haiku", "max", false)! == ("high", false))
        #expect(fit("claude-code", "opus", "minimal", false)! == ("low", false))
        // A level nothing knows the order of: the model's default.
        #expect(fit("claude-code", "haiku", "turbo", false)! == ("medium", false))
        // No effort asked: the default; a model without levels takes none.
        #expect(fit("claude-code", "haiku", nil, false)! == ("medium", false))
        #expect(fit("codex", "plain", "high", false)! == (nil, false))
        // Fast only where the model has it.
        #expect(fit("claude-code", "opus", "high", true)! == ("high", true))
        #expect(fit("claude-code", "haiku", "high", true)! == ("high", false))
        // An unknown model is still refused.
        #expect(c.fit(provider: "codex", model: "nope", effort: nil, fast: false) == nil)
    }

    @Test func `the nearest effort takes the lower on a tie`() {
        #expect(Catalog.nearestEffort("medium", supported: ["low", "high"]) == "low")
        #expect(Catalog.nearestEffort("xhigh", supported: ["high", "max"]) == "high")
        #expect(Catalog.nearestEffort("none", supported: ["minimal", "max"]) == "minimal")
        #expect(Catalog.nearestEffort("high", supported: ["turbo"]) == nil)
    }

    @Test func `several models start only on new branches and within boat's limits`() {
        let branch = OnBranch(name: "fix-login", base: "main")
        #expect(FanOut.cantStart(1, onBranch: branch, room: nil) == nil)
        #expect(FanOut.cantStart(2, onBranch: branch, room: nil) == "Several models each need a new branch: pick New branch, or one model.")
        #expect(FanOut.cantStart(3, onBranch: nil, room: nil) == nil)
        #expect(FanOut.cantStart(3, onBranch: nil, room: 3) == nil)
        #expect(FanOut.cantStart(3, onBranch: nil, room: 2) == "boat will start 2 more VMs right now: pick 2 models or fewer.")
        #expect(FanOut.cantStart(2, onBranch: nil, room: 1) == "boat will start 1 more VM right now: pick 1 model or fewer.")
        #expect(FanOut.cantStart(1, onBranch: nil, room: 0) == "boat won't start another VM right now. Try again in a minute.")
    }

    @Test func `the room to start is the tightest limit`() {
        #expect(Usage(vms: Count(3, of: 100), minute: Count(10, of: 12), hour: Count(20, of: 60), day: Count(120, of: 200)).startRoom == 2)
        #expect(Usage(vms: Count(100, of: 100), minute: Count(0, of: 12)).startRoom == 0)
        #expect(Usage(minute: Count(14, of: 12)).startRoom == 0)
        #expect(Usage().startRoom == nil)
        // Extra starts carry on past the windows (billed): only running VMs count, until the cap.
        #expect(Usage(vms: Count(3, of: 100), minute: Count(12, of: 12), extraStarts: ExtraStarts(on: true)).startRoom == 97)
        #expect(Usage(vms: Count(3, of: 100), minute: Count(12, of: 12), extraStarts: ExtraStarts(on: true, capReached: true)).startRoom == 0)
    }

    @Test func `a branch tag only for several models in a project on new branches`() {
        let branch = OnBranch(name: "fix-login", base: "main")
        #expect(FanOut.branchTag(label: "Opus 5.5", count: 2, projectID: "p1", onBranch: nil) == "Opus 5.5")
        #expect(FanOut.branchTag(label: "Opus 5.5", count: 1, projectID: "p1", onBranch: nil) == nil, "one model")
        #expect(FanOut.branchTag(label: "Opus 5.5", count: 2, projectID: nil, onBranch: nil) == nil, "No project")
        #expect(FanOut.branchTag(label: "Opus 5.5", count: 2, projectID: "p1", onBranch: branch) == nil, "an existing branch")
    }

    @Test func `one request per model, each with its own id and its model's tag`() throws {
        let photo = Attached(name: "photo-1.jpg", size: 9, kind: .image, blob: "b_ab")
        let models = [
            ThreadSettings(provider: "claude-code", model: "opus", effort: "max", fast: true),
            ThreadSettings(provider: "codex", model: "plain", effort: nil, fast: false),
        ]
        let reqs = FanOut.threads(ids: ["a", "b"], models: models, catalog: Self.catalog, environment: "env", projectID: "p1", onBranch: nil, prompt: "Fix it", attachments: [photo])
        #expect(reqs.map(\.id) == ["a", "b"])
        // A model without a label is named by its id.
        #expect(reqs.map(\.branchTag) == ["Opus 5.5", "plain"])
        #expect(reqs.allSatisfy { $0.prompt == "Fix it" && $0.attachments == [photo] && $0.projectID == "p1" && $0.environment == "env" })
        #expect(Request.newThread(reqs[0]).params?["branch_tag"] == "Opus 5.5")
        // No project: no tag, and no branch either.
        let loose = FanOut.threads(ids: ["a", "b"], models: models, catalog: Self.catalog, environment: "env", projectID: nil, onBranch: OnBranch(name: "x", base: "main"), prompt: "p", attachments: [])
        #expect(loose.allSatisfy { $0.branchTag == nil && $0.onBranch == nil })
        let one = FanOut.threads(ids: ["a"], models: [models[0]], catalog: Self.catalog, environment: "env", projectID: "p1", onBranch: nil, prompt: "p", attachments: [])
        #expect(one[0].branchTag == nil)
    }

    @Test func `starts that were refused are said plainly, each model named`() {
        #expect(FanOut.problem([("Opus 5.5", "boat said no")], of: 1) == "Couldn't start the thread: boat said no")
        #expect(FanOut.problem([("GPT-6.1 Sol", "quota")], of: 2) == "Couldn't start on GPT-6.1 Sol: quota")
        #expect(FanOut.problem([("Opus 5.5", "a"), ("GPT-6.1 Sol", "b")], of: 3) == "Couldn't start on Opus 5.5: a\nCouldn't start on GPT-6.1 Sol: b")
        // All refused for the same reason: said once.
        #expect(FanOut.problem([("Opus 5.5", "offline"), ("GPT-6.1 Sol", "offline")], of: 2) == "Couldn't start the threads: offline")
    }

    @Test func `thread new leaves branch tag out when there's none`() throws {
        let plain = NewThread(id: "x", environment: "base", projectID: "p1", provider: "codex", model: "gpt", effort: nil, fast: false, prompt: "go")
        let p = try #require(Request.newThread(plain).params?.object)
        #expect(p["branch_tag"] == nil)
        var tagged = plain
        tagged.branchTag = "GPT-6.1 Sol"
        let t = try #require(Request.newThread(tagged).params?.object)
        #expect(t["branch_tag"] == "GPT-6.1 Sol")
        #expect(Set(t.keys).subtracting(p.keys) == ["branch_tag"])
    }
}
