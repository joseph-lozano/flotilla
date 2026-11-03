import Foundation
import os

/// Who's connecting, for the hello.
public struct ClientInfo: Sendable {
    public var app: String
    public var version: String
    public var platform: String

    public init(app: String, version: String, platform: String) {
        self.app = app
        self.version = version
        self.platform = platform
    }
}

/// A connection to a Flotilla server, kept up until stopped, as `flotilla_client::Client`
/// keeps one: it sends the device's token on the upgrade, says hello with the views it holds,
/// keeps them up to date (`Replica`), sends again what didn't arrive after a drop, and skips
/// what it already had. When the connection drops it says so, and reconnects with jittered
/// backoff; `resume()` reconnects at once (the app coming back to the foreground).
///
/// What the server sends comes out of `events`, in order.
public actor FlotillaClient {
    public nonisolated let events: AsyncStream<ClientEvent>
    private let sink: AsyncStream<ClientEvent>.Continuation

    private let url: URL
    private let token: String?
    private let info: ClientInfo
    private let transport: any Transport
    private let timing: Timing
    private var core: SessionCore
    private var nextID: ReqID = 1
    private var run: Run = .idle
    /// The connection, while there is one.
    private var conn: Conn?
    /// Connections made, so a late callback from an old one is ignored.
    private var generation = 0
    private var attempt: UInt32 = 0
    private var retry: Task<Void, Never>?
    /// Callers waiting on an answer (`request`).
    private var waiters: [ReqID: CheckedContinuation<Result<Reply, ServerError>, Never>] = [:]
    private let log = Logger(subsystem: "com.useflotilla.flotilla", category: "client")

    /// What the client counts, for tests and diagnostics.
    public struct Stats: Sendable, Hashable {
        public var connects = 0
        public var resumed = 0
        public var resyncs = 0
    }
    public private(set) var stats = Stats()

    /// How long things take. Tests shorten them.
    public struct Timing: Sendable {
        public var pingEvery: Duration = .seconds(20)
        public var silentFor: Duration = .seconds(45)
        public var welcomeWithin: Duration = .seconds(10)
        /// After the app comes back to the front, how long a ping may take before the
        /// connection counts as dead.
        public var aliveWithin: Duration = .seconds(3)
        public var backoffFirst: Duration = .milliseconds(250)
        public var backoffMax: Duration = .seconds(10)
        public init() {}
    }

    private enum Run {
        case idle, running, paused, stopped
        /// Its token is no good: no try will work until it's paired again.
        case unpaired
    }

    private struct Conn {
        let generation: Int
        let socket: any WebSocket
        var synced = false
        /// What's to be written, in order, and whether a writer is at it.
        var queue: [Up] = []
        var writing = false
        var heard = ContinuousClock.now
        var tasks: [Task<Void, Never>] = []
    }

    /// `server`: `http(s)://host[:port]`, as pairing gives it. `token`: the device's.
    public init(server: URL, token: String?, info: ClientInfo, transport: any Transport = URLSessionTransport(), timing: Timing = Timing(), session: String = UUID().uuidString.lowercased()) {
        var c = URLComponents(url: server, resolvingAgainstBaseURL: false) ?? URLComponents()
        c.scheme = c.scheme == "https" ? "wss" : "ws"
        c.path = wsPath
        c.query = nil
        c.fragment = nil
        url = c.url ?? server
        self.token = token
        self.info = info
        self.transport = transport
        self.timing = timing
        core = SessionCore(id: session)
        (events, sink) = AsyncStream.makeStream(of: ClientEvent.self)
    }

    // MARK: The app's side

    public func start() {
        guard case .idle = run else { return }
        run = .running
        sink.yield(.connection(.connecting))
        connect()
    }

    /// Stop for good: the connection closes, waiting requests fail, `events` ends.
    public func stop() {
        run = .stopped
        retry?.cancel()
        end(reason: nil)
        for (_, w) in waiters { w.resume(returning: .failure(ServerError("Disconnected from the server"))) }
        waiters = [:]
        sink.finish()
    }

    /// The app went to the background: close the connection (so this device stops counting
    /// as viewing its open thread) and don't reconnect until `resume`.
    public func pause() {
        guard case .running = run else { return }
        run = .paused
        retry?.cancel()
        end(reason: nil)
    }

    /// The app is in front again: reconnect now (resuming from what it holds), or, if the
    /// connection looks alive, check it with a ping.
    public func resume() {
        switch run {
        case .paused:
            run = .running
        case .running:
            if let conn, core.phase == .live {
                let g = conn.generation
                let socket = conn.socket
                let within = timing.aliveWithin
                // A socket from before the phone slept may be dead without knowing it.
                let check = Task {
                    let pong = Task { await self.ping(g, socket) }
                    try? await Task.sleep(for: within)
                    pong.cancel()
                    self.checkAlive(g, since: within)
                }
                self.conn?.tasks.append(check)
                return
            }
        default:
            return
        }
        retry?.cancel()
        attempt = 0
        if conn == nil, pendingGeneration == nil {
            sink.yield(.connection(.connecting))
            connect()
        }
    }

    /// Send this view now and as it changes. Subscribing to a thread marks it on screen.
    public func subscribe(_ view: ViewKey) { take(.sub(view)) }

    public func unsubscribe(_ view: ViewKey) { take(.unsub(view)) }

    /// Send a request; its answer comes as `.answer` in `events`.
    @discardableResult
    public func send(_ request: Request) -> ReqID {
        let id = nextID
        nextID += 1
        take(.req(id: id, request))
        return id
    }

    /// Send a request and wait for its answer (it also comes in `events`).
    public func request(_ request: Request) async -> Result<Reply, ServerError> {
        switch run {
        case .stopped: return .failure(ServerError("Disconnected from the server"))
        // No answer will come until it's paired again.
        case .unpaired: return .failure(ServerError(TransportError.unauthorized.description))
        default: break
        }
        let id = nextID
        nextID += 1
        return await withCheckedContinuation { c in
            waiters[id] = c
            take(.req(id: id, request))
        }
    }

    private func take(_ msg: ClientMsg) {
        if case .stopped = run { return }
        for up in core.app(msg) { write(up) }
    }

    // MARK: The connection

    private func connect() {
        guard case .running = run, conn == nil, pendingGeneration == nil else { return }
        generation += 1
        let g = generation
        var request = URLRequest(url: url)
        request.setValue(String(protocolVersion), forHTTPHeaderField: protocolHeader)
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        // The client's own pings and the welcome's deadline say when it's gone; this only
        // bounds a stall in the system's stack.
        request.timeoutInterval = 120
        let transport = self.transport
        Task {
            do throws(TransportError) {
                let socket = try await transport.open(request)
                self.opened(g, socket)
            } catch {
                self.ended(g, error)
            }
        }
        // Opening: a second connect waits for this one.
        pendingGeneration = g
    }

    /// The connection being opened, before its socket is there.
    private var pendingGeneration: Int?

    private func opened(_ g: Int, _ socket: any WebSocket) {
        guard g == generation, pendingGeneration == g, case .running = run else {
            socket.close()
            return
        }
        pendingGeneration = nil
        var c = Conn(generation: g, socket: socket)
        let timing = self.timing
        c.tasks.append(Task {
            while true {
                let text: String
                do throws(TransportError) {
                    text = try await socket.receive()
                } catch {
                    self.ended(g, error)
                    return
                }
                self.frame(g, text)
            }
        })
        c.tasks.append(Task {
            try? await Task.sleep(for: timing.welcomeWithin)
            self.welcomeDue(g)
        })
        c.tasks.append(Task {
            while !Task.isCancelled {
                try? await Task.sleep(for: timing.pingEvery)
                guard self.stillHeard(g) else { return }
                Task { await self.ping(g, socket) }
            }
        })
        conn = c
        write(core.hello(app: info.app, version: info.version, platform: info.platform))
    }

    private func frame(_ g: Int, _ text: String) {
        guard var c = conn, c.generation == g else { return }
        c.heard = .now
        conn = c
        guard let down = Down(text: text) else { return }
        switch core.received(down) {
        case .welcomed(let w, let ups, let lost):
            stats.connects += 1
            if w.resumed { stats.resumed += 1 }
            for id in lost {
                deliver(.answer(id: id, .failure(ServerError("Lost the connection to the server before it answered"))))
            }
            for up in ups { write(up) }
        case .deliver(let events):
            for e in events { deliver(e) }
        case .resync(let v, let why):
            log.notice("asking for \(v.description, privacy: .public) again: \(why, privacy: .public)")
            stats.resyncs += 1
            write(.resync(v))
        case .unreadable(let v, let why):
            log.notice("can't read the \(v.description, privacy: .public) view this server sends: \(why, privacy: .public)")
        case .refuse(let msg, let event):
            // The server won't take this client: say why once, close, and keep trying slowly.
            if let event { deliver(event) }
            ended(g, .failed(msg))
        case .fail(let why):
            ended(g, .failed(why))
        case .skipped, .nothing:
            break
        }
    }

    private func deliver(_ e: ClientEvent) {
        switch e {
        case .answer(let id, let result):
            waiters.removeValue(forKey: id)?.resume(returning: result)
        case .connection(.online):
            conn?.synced = true
        default:
            break
        }
        sink.yield(e)
    }

    /// A pong is word from the server; a ping that fails ends the connection.
    private func ping(_ g: Int, _ socket: any WebSocket) async {
        do throws(TransportError) {
            try await socket.ping()
            heard(g)
        } catch {
            ended(g, error)
        }
    }

    private func welcomeDue(_ g: Int) {
        guard conn?.generation == g, core.phase == .hello else { return }
        ended(g, .failed("The server didn't answer"))
    }

    private func heard(_ g: Int) {
        guard conn?.generation == g else { return }
        conn?.heard = .now
    }

    /// Still hearing from the server; if not, end the connection.
    private func stillHeard(_ g: Int) -> Bool {
        guard let c = conn, c.generation == g else { return false }
        if ContinuousClock.now - c.heard > timing.silentFor {
            ended(g, .failed("The server stopped answering"))
            return false
        }
        return true
    }

    /// After `resume`'s ping: nothing heard within the time means the socket is dead.
    private func checkAlive(_ g: Int, since: Duration) {
        guard let c = conn, c.generation == g else { return }
        if ContinuousClock.now - c.heard >= since {
            ended(g, .failed("The server stopped answering"))
        }
    }

    // MARK: Writing

    private func write(_ up: Up) {
        guard conn != nil else {
            // Gone meanwhile: it waits for the next connection.
            core.lost(unsent: [up])
            return
        }
        conn!.queue.append(up)
        if !conn!.writing {
            conn!.writing = true
            let g = conn!.generation
            Task { await self.drain(g) }
        }
    }

    /// Write what's queued, in order. A failure leaves it, and what's after it, for the next
    /// connection.
    private func drain(_ g: Int) async {
        while let c = conn, c.generation == g, let up = c.queue.first {
            do throws(TransportError) {
                try await c.socket.send(up.json.text)
            } catch {
                ended(g, error)
                return
            }
            guard conn?.generation == g else { return }
            conn!.queue.removeFirst()
        }
        if conn?.generation == g { conn!.writing = false }
    }

    // MARK: Ending

    private func ended(_ g: Int, _ error: TransportError) {
        guard g == generation else { return }
        if error == .unauthorized {
            run = .unpaired
            if case .refuse(_, let event?) = core.refuse(error.description) { sink.yield(event) }
            end(reason: nil)
            for (_, w) in waiters { w.resume(returning: .failure(ServerError(error.description))) }
            waiters = [:]
            return
        }
        let wasSynced = conn?.synced ?? false
        end(reason: error.description)
        if wasSynced { attempt = 0 }
        guard case .running = run else { return }
        let wait = backoff(attempt)
        attempt = min(attempt + 1, 16)
        retry?.cancel()
        retry = Task {
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            self.connect()
        }
    }

    /// Close the connection; what wasn't written waits for the next one. `reason`: what to
    /// tell the app (nil: say nothing).
    private func end(reason: String?) {
        pendingGeneration = nil
        if let c = conn {
            for t in c.tasks { t.cancel() }
            c.socket.close()
            // The one being written may have gone: a request stays in flight (the next
            // welcome's `seen` says whether it arrived), and a subscribe is in the next hello.
            core.lost(unsent: c.writing ? Array(c.queue.dropFirst()) : c.queue)
            conn = nil
        }
        // Not the current generation any more: a late callback is ignored.
        generation += 1
        if let reason, core.refused == nil, !isStopped { sink.yield(.connection(.offline(reason))) }
    }

    private var isStopped: Bool {
        if case .stopped = run { return true }
        return false
    }

    /// Before try `attempt` (from 0): doubling up to the most, give or take a third, so
    /// clients dropped together don't all come back at once.
    private func backoff(_ attempt: UInt32) -> Duration {
        let base = min(timing.backoffFirst * Int(1 << min(attempt, 8)), timing.backoffMax)
        return base * Double.random(in: 0.67...1.33)
    }
}
