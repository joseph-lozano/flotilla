import Foundation
import Testing
@testable import FlotillaKit

/// Preview: the requests as Rust's serde reads them, a thread's services as it writes them
/// (`phone_preview_and_services_on_the_wire` pins both on the Rust side), and the desktop's
/// rules for Preview's menu (`thread_view.rs`).
@Suite struct ServicesTests {
    /// A thread view's services, as `serde_json` writes `flotilla_proto::ThreadServices`.
    static let wire = #"""
    {"sandbox":"bx_1","file":".agents/services.yml","error":null,"services":[{"name":"web","port":4100,"pid":12,"state":"ready","exit_code":null,"error":null,"output":"  VITE ready in 312 ms\n\n  Local: http://localhost:4100/  \n","preview":"https://4100-bx1.example.com/?_token=t","links":[{"title":"web","url":"https://4100-bx1.example.com/?_token=t"},{"title":"Admin","url":"https://4100-bx1.example.com/admin?_token=t"}],"started_at":"2026-11-01T12:00:00Z","out_at":40,"err_at":0},{"name":"worker","port":4101,"pid":null,"state":"stopped","exit_code":null,"error":null,"output":"","preview":null,"links":[],"started_at":null,"out_at":0,"err_at":0},{"name":"api","port":4102,"pid":null,"state":"exited","exit_code":1,"error":null,"output":"Error: listen EADDRINUSE\n","preview":"https://4102-bx1.example.com/?_token=t","links":[],"started_at":"2026-11-01T12:00:00Z","out_at":9,"err_at":24}]}
    """#

    func thread(state: String, sandbox: String? = "bx_1") throws -> FlotillaKit.Thread {
        try Thread(json: ["id": "t1", "state": .string(state), "sandbox_id": JSON(sandbox)])
    }

    @Test func `preview and the services commands, as Rust reads them`() {
        #expect(Up.req(id: 7, .preview("t1", port: 3000)).json.text == #"{"id":7,"m":"thread.preview","p":{"port":3000,"thread":"t1"},"t":"req"}"#)
        #expect(Up.req(id: 8, .restartService("t1", name: "web")).json.text == #"{"id":8,"m":"services","p":{"restart":{"name":"web","thread":"t1"}},"t":"req"}"#)
        #expect(Up.req(id: 9, .stopService("t1", name: "web")).json.text == #"{"id":9,"m":"services","p":{"stop":{"name":"web","thread":"t1"}},"t":"req"}"#)
        #expect(Up.req(id: 10, .ensureServices("t1")).json.text == #"{"id":10,"m":"services","p":{"ensure":"t1"},"t":"req"}"#)
        // Preview's answer.
        #expect(Reply(json: ["url": "https://3000-bx1.example.com/?_token=t"]) == .url("https://3000-bx1.example.com/?_token=t"))
    }

    @Test func `a thread view's services, as Rust writes them`() throws {
        let d = try ThreadDetail(json: JSON(parsing: #"{"id": "t1", "messages": [], "services": \#(Self.wire)}"#))
        let s = try #require(d.services)
        #expect(s.sandbox == "bx_1" && s.file == ".agents/services.yml" && s.error == nil)
        #expect(s.services.map(\.name) == ["web", "worker", "api"])
        let web = s.services[0]
        #expect(web.port == 4100 && web.state == "ready" && web.preview == "https://4100-bx1.example.com/?_token=t")
        #expect(web.links == [ServiceLink(title: "web", url: "https://4100-bx1.example.com/?_token=t"), ServiceLink(title: "Admin", url: "https://4100-bx1.example.com/admin?_token=t")])
        #expect(s.services[2].exitCode == 1)
        // None, null, or a server from before services.
        #expect(try ThreadDetail(json: JSON(parsing: #"{"id": "t1", "services": null}"#)).services == nil)
        #expect(try ThreadDetail(json: JSON(parsing: #"{"id": "t1"}"#)).services == nil)
        // A view whose services change isn't the same view.
        #expect(try ThreadDetail(json: JSON(parsing: #"{"id": "t1"}"#)) != d)
    }

    @Test func `read leniently: what's left out is empty, only a wrong shape is a doubt`() throws {
        let s = try ThreadServices(json: JSON(parsing: #"{"services": [{"name": "web", "links": [{"url": "https://a.example.com/"}, {"title": "no url"}], "newer": 1}]}"#))
        #expect(s.sandbox == "" && s.services[0].port == 0 && s.services[0].state == "" && s.services[0].output == "")
        #expect(s.services[0].links == [ServiceLink(title: "", url: "https://a.example.com/")], "a link without a URL is left out")
        // A service without a name has nothing to act on: left out.
        #expect(try ThreadServices(json: JSON(parsing: #"{"services": [{"port": 1}, {"name": "web"}]}"#)).services.map(\.name) == ["web"])
        #expect(throws: ReadError.self) { try ThreadServices(json: JSON(parsing: #"{"services": {}}"#)) }
        #expect(throws: ReadError.self) { try ThreadServices(json: JSON(parsing: #"{"services": ["web"]}"#)) }
        #expect(throws: ReadError.self) { try ThreadDetail(json: JSON(parsing: #"{"id": "t1", "services": []}"#)) }
    }

    /// `thread_view::current_services`: the VM it has now, while it's up, with something to show.
    @Test func `Preview's menu shows only while the VM they run on is up`() throws {
        let s = try ThreadServices(json: JSON(parsing: Self.wire))
        #expect(ThreadServices.current(s, thread: try thread(state: "idle")) == s)
        #expect(ThreadServices.current(s, thread: try thread(state: "running")) == s)
        for state in ["stopped", "stopping", "starting", "creating", "expired", "error"] {
            #expect(ThreadServices.current(s, thread: try thread(state: state)) == nil, "\(state): plain Preview, which wakes it")
        }
        #expect(ThreadServices.current(s, thread: try thread(state: "idle", sandbox: "bx_2")) == nil, "a fork's or a rebuild's new VM runs none of them")
        #expect(ThreadServices.current(nil, thread: try thread(state: "idle")) == nil)
        #expect(ThreadServices.current(s, thread: nil) == nil)
        // Nothing to show, unless the file has a problem.
        let none = ThreadServices(sandbox: "bx_1", services: [])
        #expect(ThreadServices.current(none, thread: try thread(state: "idle")) == nil)
        let broken = ThreadServices(sandbox: "bx_1", error: ".agents/services.yml: service web: no command", services: [])
        #expect(ThreadServices.current(broken, thread: try thread(state: "idle")) == broken)
    }

    /// `thread_view::service_state`, `service_problem` and `ServiceRun::live`.
    @Test func `a service's state in words, whether it needs you, and whether it runs`() {
        let run = { (state: String) in ServiceRun(name: "web", port: 4100, state: state) }
        #expect(run("ready").words == "ready · :4100")
        #expect(run("starting").words == "starting…")
        #expect(run("not_responding").words == "not responding on :4100")
        #expect(run("exited").words == "exited early")
        #expect(ServiceRun(name: "web", port: 4100, state: "exited", exitCode: 1).words == "exited 1")
        #expect(run("stopped").words == "stopped")
        #expect(run("failed").words == "failed")
        #expect(ServiceRun(name: "web", port: 4100, state: "failed", error: "no such file").words == "failed: no such file")
        #expect(["starting", "ready", "not_responding"].allSatisfy { run($0).live })
        #expect(!["exited", "stopped", "failed"].contains { run($0).live })
        #expect(["exited", "failed", "not_responding"].allSatisfy { run($0).problem })
        #expect(!["starting", "ready", "stopped"].contains { run($0).problem })
    }

    /// `services_menu`: Open is the first link; a service without links opens its preview.
    @Test func `Open's link, then the rest by their titles`() throws {
        let s = try ThreadServices(json: JSON(parsing: Self.wire))
        #expect(s.services[0].openLinks.map(\.title) == ["web", "Admin"])
        #expect(s.services[1].openLinks.isEmpty, "no portal: nothing to open")
        #expect(s.services[2].openLinks == [ServiceLink(title: "api", url: "https://4102-bx1.example.com/?_token=t")])
    }

    /// `flotilla_model::setup::tail`: the last lines that aren't blank, trailing spaces gone.
    @Test func `Output is the end of what it printed`() throws {
        let web = try ThreadServices(json: JSON(parsing: Self.wire)).services[0]
        #expect(web.tail() == ["  VITE ready in 312 ms", "  Local: http://localhost:4100/"])
        let long = ServiceRun(name: "web", port: 1, state: "ready", output: (1...100).map { "line \($0)" }.joined(separator: "\r\n"))
        #expect(long.tail().count == 80 && long.tail().first == "line 21" && long.tail().last == "line 100")
        #expect(long.tail(2) == ["line 99", "line 100"])
        #expect(ServiceRun(name: "web", port: 1, state: "ready", output: " \n\t\n").tail().isEmpty)
    }

    @Test func `the port field takes 1 to 65535`() {
        #expect(previewPort("3000") == 3000)
        #expect(previewPort(" 8080 ") == 8080)
        #expect(previewPort("65535") == 65535)
        for bad in ["", "0", "65536", "-1", "30 00", "3000a", "http"] {
            #expect(previewPort(bad) == nil, "\(bad)")
        }
    }
}
