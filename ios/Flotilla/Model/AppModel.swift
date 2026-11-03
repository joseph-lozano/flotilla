import FlotillaKit
import Foundation
import Observation
import UIKit
import os

/// A cut tool output fetched whole: on its way, what its row shows, or why not.
enum Whole: Sendable, Equatable {
    case loading
    case text(String)
    case failed(String)
}

/// Everything the screens show, kept up to date from the server this phone is paired with,
/// and what they can ask it. On the main actor: the client delivers its events here, in order.
///
/// One is made for each pairing (`Phone`), and talks to that one server for its life: pairing
/// with another, or forgetting it, makes a new one, so nothing of this one carries over.
@Observable
final class AppModel {
    /// Nil: not paired (the pairing screen).
    let server: PairedServer?
    private(set) var connection: ConnectionState = .connecting
    private(set) var sidebar: Sidebar?
    private(set) var catalog: Catalog?
    private(set) var details: [String: ThreadDetail] = [:]
    /// Each open thread's older pages beside its view (`History`): its view holds only the
    /// newest messages.
    private(set) var histories: [String: History] = [:]
    /// Tool outputs that went out cut, fetched whole, by their cut strings' marks.
    private(set) var wholes: [String: Whole] = [:]
    /// Bumped just before a page of older messages goes in above a thread's: its screen notes
    /// where it is first, to stay there.
    private(set) var arriving: [String: Int] = [:]
    /// The server's settings (New thread's defaults, its setup checks).
    private(set) var settings: ServerSettings?
    /// The devices paired with the server, while Settings is open.
    /// Every scheduled prompt, due soonest first.
    private(set) var schedules: [Schedule] = []
    /// Scheduled, open.
    var showSchedules = false
    private(set) var devices: [DeviceRow]?
    /// What the agents spent, as last sent: the view is subscribed to while the usage sheet is
    /// open (which checks the VMs, as T3 checks when Usage opens), and kept after.
    private(set) var agentUsage: AgentUsage?
    /// Each open thread's streaming text, whole each time.
    private(set) var live: [String: [RawEvent]] = [:]
    /// What this phone sent that the thread's view doesn't list yet.
    private(set) var localSends: [String: [LocalSend]] = [:]
    /// Threads this phone started that the server hasn't written yet: what was asked.
    private(set) var starting: [String: Starting] = [:]
    /// Threads whose turn this phone asked to stop, until it ends.
    private(set) var stopping: Set<String> = []
    /// Questions whose answer this phone sent, until the server answers.
    private(set) var answering: Set<String> = []
    /// Threads whose pull request is merging, since when.
    private(set) var merging: [String: Date] = [:]
    /// Threads whose review prompt is being read.
    private(set) var reviewing: Set<String> = []
    /// Threads being deleted: gone from the list at once, back if the delete fails.
    private(set) var deleting: Set<String> = []
    /// Previews asked for, by thread, until their URL comes (or doesn't), and Make public…
    /// until it's done.
    private(set) var previewing: [String: PreviewAsk] = [:]
    /// What the top of a thread says for a moment once a link of its went public.
    private(set) var publicNotes: [String: PublicNote] = [:]
    /// New titles asked for, shown until the server answers.
    private(set) var renaming: [String: String] = [:]
    /// Counts sends and starts that failed, for their haptic.
    private(set) var sendFailures = 0
    /// Each project's pull requests and branches, for New thread's branch picker.
    private(set) var branches: [String: Result<BranchChoices, ServerError>] = [:]
    /// The latest ask for each project's branches, so an older one's wait doesn't end a newer.
    private var branchAsks: [String: UUID] = [:]
    /// What a request that failed said, for the screen that sent it.
    var problem: String?
    /// Asks again what got no answer, for the problem it failed with.
    var retry: Retry?
    /// The composer's text, per thread ("" is New thread's), while the app runs.
    var drafts: [String: String] = [:]
    /// Files waiting to go with the next message, per thread ("" is New thread's).
    var pending: [String: [PendingFile]] = [:]
    /// Why the last file added was left out, per composer.
    var attachProblem: [String: String] = [:]
    /// A pairing link the phone opened: fills in the pairing form.
    var pairingLink: Pairing.Target?
    var showPairing = false
    var showSettings = false
    /// Settings' "Pair with a different server": pairing opens once Settings has gone.
    var pairAfterSettings = false
    /// New thread, when it's open, and what it opens with.
    var newThread: NewThreadStart?
    /// What New thread in Threads' place opens with (`startsHere`): a refused start's prompt and
    /// why. Kept apart from the sheet's, so it never opens as a sheet once a thread comes.
    var startHere: NewThreadStart?
    /// The threads screen's stack: the threads open, in order. A thread put on it is opened: the
    /// server is told (`thread.opened`), with the search it was picked from, if any, so search
    /// ranks by how often and what for, as on every device.
    var path: [String] = [] {
        didSet {
            let query = pickQuery
            pickQuery = nil
            guard path.count >= oldValue.count, let top = path.last, top != oldValue.last, let client else { return }
            Task { await client.send(.opened(top, query: query)) }
        }
    }
    /// The search the next thread opened was picked from.
    @ObservationIgnored var pickQuery: String?
    /// Messages to open threads at (a search hit), by thread: its pages down to the message
    /// are fetched, and its screen goes to it, then lets go.
    var jumps: [String: Int64] = [:]
    /// The thread on screen, if any: a notification about it doesn't show.
    private(set) var onScreen: String?
    /// What was just archived, with Undo, for a few seconds.
    private(set) var archived: ArchiveNotice?

    /// The server's client until `stop`, and this phone's token there.
    @ObservationIgnored private var client: FlotillaClient?
    @ObservationIgnored let token: String?
    @ObservationIgnored private var pump: Task<Void, Never>?
    /// The APNs token iOS gave this app, in hex, and whether the server has it.
    @ObservationIgnored private var pushToken: String?
    @ObservationIgnored private var pushed = false
    @ObservationIgnored private var askedForNotifications = false
    /// What iOS allows for notifications, for Settings.
    @ObservationIgnored let notifications = Notifications()
    /// Threads whose merge the archive notice told of, and when: the server's push about the
    /// same merge doesn't show as well while the app is open.
    @ObservationIgnored private var mergesShown: [String: Date] = [:]
    @ObservationIgnored private let log = Logger(subsystem: "com.useflotilla.flotilla", category: "app")
    /// Pages of older messages being fetched, by thread: a jump far back fetches a few at a
    /// time, not dozens at once.
    @ObservationIgnored private var fetching: [String: Int] = [:]
    /// Pages fetched and not put in yet, while others are being fetched.
    @ObservationIgnored private var parked: [String: [HistoryPage]] = [:]
    static let fetches = 4

    /// On `server` with `token`, connecting at once (none: not paired). A server whose token
    /// couldn't be read says so, with Pair again.
    init(server: PairedServer?, token: String?) {
        self.server = server
        self.token = token
        if server != nil, token == nil { connection = .refused("Couldn't read this phone's pairing from the Keychain. Open Flotilla again to try again.") }
        guard let server, let token, let url = URL(string: server.url) else { return }
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
        let c = FlotillaClient(server: url, token: token, info: ClientInfo(app: "flotilla-ios", version: version, platform: "ios"))
        client = c
        pump = Task { [weak self] in
            for await e in c.events { self?.take(e) }
        }
        Task {
            await c.start()
            await c.subscribe(.sidebar)
            await c.subscribe(.catalog)
            await c.subscribe(.settings)
            await c.subscribe(.schedules)
        }
    }

    /// Done with: its client stops, and work still under way (a page, an upload) asks nothing more.
    func stop() {
        let c = client
        client = nil
        pump?.cancel()
        pump = nil
        Task { await c?.stop() }
    }

    /// What this phone is called on its server: the name it last paired or was renamed with,
    /// else the phone's own (iOS gives apps only "iPhone").
    static var deviceName: String {
        UserDefaults.standard.string(forKey: "deviceName")?.nilIfBlank ?? UIDevice.current.name
    }

    /// A `flotilla://pair` link: fill in the form (pairing takes a tap, never a link alone).
    func open(url: URL) {
        guard let target = try? Pairing.read(url.absoluteString) else { return }
        pairingLink = target
        showPairing = true
    }

    /// The app went to the background, or came back.
    func scene(active: Bool) {
        if active { Task { await notifications.refresh() } }
        guard let client else { return }
        Task { active ? await client.resume() : await client.pause() }
    }

    // MARK: What comes in

    private func take(_ e: ClientEvent) {
        switch e {
        case .view(.sidebar(let s)):
            sidebar = s
            // A turn that ended takes its streaming text and its stop with it.
            for row in s.threads where row.turnStartedMs == nil {
                live[row.id] = nil
                stopping.remove(row.id)
            }
            for (t, since) in merging {
                let pr = s.threads.first { $0.id == t }?.pr
                if pr?.open != true || Date().timeIntervalSince(since) > 30 { merging[t] = nil }
            }
            // A thread this phone started is written: it shows as itself now.
            for id in starting.keys where s.threads.contains(where: { $0.id == id }) { starting[id] = nil }
        case .view(.catalog(let c)):
            catalog = c
        case .view(.thread(let d)):
            details[d.id] = d
            if onScreen == d.id || histories[d.id] != nil {
                histories[d.id, default: History()].view(d)
                // Opened at a search hit: every page down to its message is wanted.
                if let seq = jumps[d.id] { _ = histories[d.id]?.reach(seq) }
                // A page held that changed is fetched again.
                fetchOlder(d.id)
            }
            let listed = Set(d.sent.map(\.id))
            localSends[d.id]?.removeAll { listed.contains($0.local) }
            if d.thread != nil { starting[d.id]?.written = true }
        case .view(.settings(let s)):
            settings = s
        case .view(.devices(let d)):
            devices = d
        case .view(.agentUsage(let u)):
            agentUsage = u
        case .view(.schedules(let all)):
            schedules = all
            if all.isEmpty { showSchedules = false }
        case .view(.other):
            break
        case .live(let thread, let partials):
            live[thread] = partials
        case .connection(let state):
            connection = state
            if state == .online { online() }
        case .progress(_, let note):
            if let (project, result) = note.branches { branches[project] = result }
            if let checked = note.branchChecked { branchChecked(checked) }
        case .note(let note):
            heard(note)
        case .answer:
            break
        }
    }

    /// A note from the server. Archives it did by itself show as one you did, with Undo: a
    /// merged pull request's thread (saying so), and threads left idle.
    private func heard(_ note: Note) {
        if let m = note.prMerged, m.archived {
            mergesShown[m.thread] = Date()
            noteArchived([m.thread], merged: [m.thread: m.number])
        }
        if let ids = note.idleArchived, !ids.isEmpty {
            noteArchived(ids)
        }
        // A scheduled prompt that didn't run (said once for a run of failures alike).
        if let f = note.scheduleFailed {
            let first = f.prompt.split(whereSeparator: \.isNewline).first.map(String.init) ?? f.prompt
            problem = "Didn't run: \(first.count > 60 ? first.prefix(59) + "…" : Substring(first))\n\(f.why)"
        }
    }

    // MARK: Scheduled prompts

    /// Keep these (one per target): nil once the server has them all, else why not.
    func putSchedules(_ specs: [ScheduleSpec]) async -> String? {
        guard let client else { return "Not connected to the server" }
        for spec in specs {
            if case .failure(let e) = await answer(client, .putSchedule(spec)) { return e.message }
        }
        return nil
    }

    func pauseSchedule(_ id: String, paused: Bool) { scheduleRequest(.pauseSchedule(id, paused: paused), paused ? "pause" : "resume") }
    func deleteSchedule(_ id: String) { scheduleRequest(.deleteSchedule(id), "delete") }
    func runSchedule(_ id: String) { scheduleRequest(.runSchedule(id), "run") }

    private func scheduleRequest(_ r: Request, _ verb: String) {
        guard let client else { return }
        Task {
            if case .failure(let e) = await answer(client, r) { problem = "Couldn't \(verb) the schedule: \(e.message)" }
        }
    }

    /// Where a schedule goes, in words: its thread's title, or "New thread · Haiku 5.5".
    func scheduleTo(_ target: ScheduleTarget) -> String {
        switch target {
        case .thread(let t): return summary(t)?.thread.title ?? "A thread that's gone"
        case .new(let f):
            let model = catalog?.modelLabel(provider: f.provider, model: f.model) ?? f.model
            let project = f.projectID.flatMap { p in sidebar?.projects.first { $0.id == p }?.repo }
            return (["New thread", model] + [project].compactMap { $0 }).joined(separator: " · ")
        }
    }

    // MARK: Threads

    func summary(_ id: String) -> ThreadSummary? { sidebar?.threads.first { $0.id == id } }

    /// On screen: subscribing marks it read, as on the Mac.
    func open(thread id: String) {
        onScreen = id
        guard let client else { return }
        Task { await client.subscribe(.thread(id)) }
    }

    func close(thread id: String) {
        if onScreen == id { onScreen = nil }
        histories[id] = nil
        jumps[id] = nil
        parked[id] = nil
        guard let client else { return }
        live[id] = nil
        Task { await client.unsubscribe(.thread(id)) }
    }

    /// What a thread shows: its older pages held, then its view's newest messages.
    func paged(_ id: String) -> ThreadDetail? {
        guard var d = details[id] else { return nil }
        if let h = histories[id] { d.messages = h.messages }
        return d
    }

    /// There are older messages than those shown, and whether some are on their way.
    func older(_ id: String) -> (more: Bool, loading: Bool) {
        (histories[id]?.moreAbove ?? false, histories[id]?.loading ?? false)
    }

    /// Thread titles and messages with every word of `query`, from the server.
    func search(_ query: String) async -> Result<[SearchHit], ServerError> {
        guard let client else { return .failure(ServerError("Not connected")) }
        switch await client.request(.search(query)) {
        case .success(.hits(let hits)): return .success(hits)
        case .success: return .success([])
        case .failure(let e): return .failure(e)
        }
    }

    /// Open a thread picked from a search for `query`.
    func open(_ thread: String, pickedFor query: String) {
        pickQuery = query
        if path.last != thread { path.append(thread) } else { pickQuery = nil }
    }

    /// Open `thread` at its message `seq` (a search hit): on top of the list, as tapping its
    /// row would, its older pages fetched down to the message.
    func open(_ thread: String, at seq: Int64) {
        jumps[thread] = seq
        if histories[thread]?.reach(seq) == true { fetchOlder(thread) }
        if path.last != thread { path.append(thread) }
    }

    /// Scrolled up to the oldest message shown: the page above it, if there is one.
    func loadOlder(_ id: String) {
        if histories[id]?.more() == true { fetchOlder(id) }
    }

    /// Fetch the pages a thread's history wants, with this phone's token.
    private func fetchOlder(_ id: String) {
        guard client != nil, let server, let token else { return }
        let pages = histories[id]?.wanted(upTo: Self.fetches - (fetching[id] ?? 0)) ?? []
        for p in pages {
            fetching[id, default: 0] += 1
            Task {
                #if DEBUG
                // UI tests slow pages down, as a far server would be (`-FlotillaSlowHistory <ms>`).
                let slow = UserDefaults.standard.integer(forKey: "FlotillaSlowHistory")
                if slow > 0 { try? await Task.sleep(for: .milliseconds(slow)) }
                #endif
                let got: Result<HistoryPage, Uploads.Problem>
                do throws(Uploads.Problem) {
                    got = .success(try await Reading.page(server: server.url, token: token, thread: id, before: Paging.before(p)))
                } catch {
                    got = .failure(error)
                }
                fetching[id] = max(0, (fetching[id] ?? 1) - 1)
                guard histories[id] != nil else { return }
                switch got {
                case .success(let page):
                    // Put in once what's in flight is in: a jump far back fetches dozens of pages,
                    // and the page is laid out once, not once a page.
                    parked[id, default: []].append(page)
                    fetchOlder(id)
                    await putIn(id)
                case .failure(let e):
                    log.error("couldn't read page \(p) of a thread: \(e.description, privacy: .public)")
                    await putIn(id)
                    // Asked for again in a while, not at once (offline, the server rebuilding):
                    // until then it still counts as asked.
                    try? await Task.sleep(for: .seconds(5))
                    guard histories[id] != nil else { return }
                    histories[id]?.failed(p)
                    fetchOlder(id)
                }
            }
        }
    }

    /// The pages fetched for `id`, put in once none is still being fetched. Said first, and put
    /// in a moment later, so the screen notes its place before the page grows above it.
    private func putIn(_ id: String) async {
        guard (fetching[id] ?? 0) == 0, parked[id]?.isEmpty == false else { return }
        arriving[id, default: 0] += 1
        try? await Task.sleep(for: .milliseconds(30))
        guard histories[id] != nil else { return }
        for page in parked.removeValue(forKey: id) ?? [] { histories[id]?.page(page) }
    }

    /// A cut tool output, whole: what its row shows, fetched once (nil if it couldn't be).
    func wholeText(_ m: Message, in thread: String) async -> String? {
        let key = m.cut.map(\.mark).joined(separator: "+")
        if case .text(let t)? = wholes[key] { return t }
        guard client != nil, let server, let token else { return nil }
        wholes[key] = .loading
        var whole = m
        do throws(Uploads.Problem) {
            for c in m.cut {
                let text = try await Reading.full(server: server.url, token: token, thread: thread, seq: m.seq, cut: c)
                if c.field == "tool_input_json" { whole.toolInputJSON = text } else { whole.toolResultJSON = text }
            }
        } catch {
            wholes[key] = .failed(error.description)
            return nil
        }
        let text = ThreadPlan.toolText(whole)
        wholes[key] = .text(text)
        return text
    }

    func whole(_ m: Message) -> Whole? { wholes[m.cut.map(\.mark).joined(separator: "+")] }

    /// Send a follow-up, or a steer while a turn runs, with the files waiting in its composer.
    /// It shows as sending until the thread lists it.
    func send(_ text: String, to thread: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let client, !text.isEmpty else { return }
        let files = pending[thread] ?? []
        let local = "local-\(UUID().uuidString.lowercased())"
        let steer = summary(thread)?.activity == .working
        localSends[thread, default: []].append(LocalSend(local: local, text: text, steer: steer, at: Format.nowMs(), files: files.map(\.name)))
        drafts[thread] = nil
        pending[thread] = nil
        attachProblem[thread] = nil
        Task {
            var result: Result<Reply, ServerError>
            switch await upload(files) {
            case .success(let attached):
                result = await client.request(.send(thread: thread, text: text, files: attached, local: local))
            case .failure(let e): result = .failure(e)
            }
            localSends[thread]?.removeAll { $0.local == local }
            if case .failure(let e) = result {
                sendFailures += 1
                problem = "Couldn't send: \(e.message)"
                // Back in the composer, as on the Mac.
                if (drafts[thread] ?? "").isEmpty { drafts[thread] = text }
                if (pending[thread] ?? []).isEmpty { pending[thread] = files }
            }
        }
    }

    /// Send your answers to a question the thread's agent waits on (`ask`: its id). It shows as
    /// sending until the server takes it; the thread then shows it answered.
    func answer(thread: String, ask: String, answers: [[String]]) {
        guard let client, !answering.contains(ask) else { return }
        let local = "local-\(UUID().uuidString.lowercased())"
        answering.insert(ask)
        Task {
            let result = await client.request(.answer(thread: thread, ask: ask, answers: answers, local: local))
            answering.remove(ask)
            if case .failure(let e) = result {
                sendFailures += 1
                problem = "Couldn't send the answer: \(e.message)"
            }
        }
    }

    /// Start a thread from New thread on each model chosen (`models`, the first one's settings
    /// as chosen, the rest fitted to them). The first opens at once, its prompt marked as
    /// sending, while the files go up (once for all of them) and the server writes the threads
    /// and boat makes their VMs; the rest start in the background, listed as starting until
    /// the server has them. If none starts, it's back to New thread with what was written; if
    /// only some do, those stay and the others are named.
    func start(prompt: String, models: [ThreadSettings], project: Project?, onBranch: OnBranch?) {
        let prompt = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let client, !prompt.isEmpty, !models.isEmpty else { return }
        guard let environment = project?.environmentName.nilIfBlank ?? (sidebar?.environments.first { $0.isDefault } ?? sidebar?.environments.first)?.name else {
            problem = "No boat environment. Make one in boat's dashboard."
            return
        }
        let files = pending[""] ?? []
        let ids = models.map { _ in UUID().uuidString.lowercased() }
        let now = Format.nowMs()
        for (i, (id, s)) in zip(ids, models).enumerated() { starting[id] = Starting(prompt: prompt, settings: s, files: files.map(\.name), at: now, rank: i) }
        drafts[""] = nil
        pending[""] = nil
        attachProblem[""] = nil
        newThread = nil
        startHere = nil
        if let server { UserDefaults.standard.set(project?.id ?? "", forKey: "lastProject.\(server.id)") }
        path = [ids[0]]
        let onBranch = project == nil ? nil : onBranch
        let catalog = self.catalog
        Task {
            var failed: [(model: String, message: String)] = []
            // Refused before the server wrote them: nothing of them is left.
            var unwritten = 0
            var anyFailed = false
            switch await upload(files) {
            case .success(let attached):
                let reqs = FanOut.threads(ids: ids, models: models, catalog: catalog, environment: environment, projectID: project?.id, onBranch: onBranch, prompt: prompt, attachments: attached)
                await withTaskGroup(of: (Int, Result<Reply, ServerError>).self) { group in
                    for (i, req) in reqs.enumerated() { group.addTask { (i, await client.request(.newThread(req))) } }
                    for await (i, result) in group {
                        guard case .failure(let e) = result else { continue }
                        anyFailed = true
                        if starting[ids[i]] != nil {
                            starting[ids[i]] = nil
                            unwritten += 1
                            if path.last == ids[i] { path.removeLast() }
                        } else if path.last == ids[i] {
                            // Written, and on screen: the thread says why itself.
                            continue
                        }
                        failed.append((Format.modelName(provider: models[i].provider, model: models[i].model, catalog: catalog), e.message))
                    }
                }
            case .failure(let e):
                for id in ids { starting[id] = nil }
                if let id = path.last, ids.contains(id) { path.removeLast() }
                unwritten = models.count
                anyFailed = true
                failed = models.map { (Format.modelName(provider: $0.provider, model: $0.model, catalog: catalog), e.message) }
            }
            if anyFailed { sendFailures += 1 }
            guard !failed.isEmpty else { return }
            let why = FanOut.problem(failed, of: models.count)
            if unwritten == models.count {
                // None started: back to New thread, as it was, saying why.
                drafts[""] = prompt
                pending[""] = files
                let back = NewThreadStart(project: project?.id, branch: onBranch, settings: models[0], also: Array(models.dropFirst()), problem: why)
                if startsHere { startHere = back } else { newThread = back }
            } else {
                problem = why
            }
        }
    }

    /// No thread to list, and none starting here: Threads is New thread itself (or the server's
    /// setup, while threads can't run), not an empty list.
    var startsHere: Bool {
        guard let sidebar else { return false }
        if case .refused = connection { return false }
        return starting.isEmpty && Groups(sidebar, hiding: deleting).isEmpty
    }

    /// The project New thread opens on: the one last used on this server.
    var lastProject: String? {
        guard let server else { return nil }
        return UserDefaults.standard.string(forKey: "lastProject.\(server.id)")?.nilIfBlank
    }

    /// A project's pull requests and branches, for the branch picker. With no word from the
    /// server in a while, they couldn't be read (the picker offers to try again).
    func loadBranches(_ project: String) {
        guard let client else { return }
        if case .failure? = branches[project] { branches[project] = nil }
        let ask = UUID()
        branchAsks[project] = ask
        Task {
            let asked = ContinuousClock.now
            // The lists come as a note; a refusal (an offline server's) comes as the answer.
            let r = await answer(client, .listBranches(project))
            guard branchAsks[project] == ask else { return }
            if case .failure(let e) = r {
                if case nil = branches[project] { branches[project] = .failure(e) }
                return
            }
            // Taken on: the note, or nothing in time.
            try? await Task.sleep(until: asked + Self.patience)
            guard branchAsks[project] == ask, branches[project] == nil else { return }
            branches[project] = .failure(ServerError(Self.noAnswer))
        }
    }

    /// Upload each file to the server in use: what to send in their place.
    private func upload(_ files: [PendingFile]) async -> Result<[Attached], ServerError> {
        guard !files.isEmpty else { return .success([]) }
        guard client != nil, let server, let token else { return .failure(ServerError("This phone isn't paired with the server")) }
        if let why = AttachLimits.overLimit(files.map { ($0.name, Int64($0.data.count)) }) { return .failure(ServerError(why)) }
        var out: [Attached] = []
        for f in files {
            do throws(Uploads.Problem) {
                let blob = try await Uploads.put(f.data, server: server.url, token: token)
                out.append(Attached(name: f.name, size: blob.size, kind: f.kind, blob: blob.id))
            } catch {
                return .failure(ServerError("Couldn't upload \(f.name): \(error.description)"))
            }
        }
        return .success(out)
    }

    /// Add files to a composer's (`key`: its thread, "" for New thread), leaving out any
    /// already there and any that would break boat's limits.
    func attach(_ files: [PendingFile], to key: String) {
        var list = pending[key] ?? []
        var refused: String?
        for f in files {
            if list.contains(where: { $0.name == f.name && $0.data == f.data }) { continue }
            list.append(f)
            if let why = AttachLimits.overLimit(list.map { ($0.name, Int64($0.data.count)) }) {
                list.removeLast()
                refused = refused ?? why
            }
        }
        pending[key] = list
        attachProblem[key] = refused
    }

    func detach(_ file: PendingFile.ID, from key: String) {
        pending[key]?.removeAll { $0.id == file }
        attachProblem[key] = nil
    }

    /// Stop the running turn.
    func interrupt(_ thread: String) {
        guard let client else { return }
        stopping.insert(thread)
        Task {
            if case .failure(let e) = await answer(client, .interrupt(thread)) {
                stopping.remove(thread)
                problem = "Couldn't stop the turn: \(e.message)"
            }
        }
    }

    /// Merge the thread's pull request (Hold to merge's end).
    func merge(_ thread: String) {
        guard let client, merging[thread] == nil else { return }
        merging[thread] = Date()
        Task {
            // GitHub can take a while to merge; but not for good.
            if case .failure(let e) = await answer(client, .mergePR(thread), within: .seconds(60)) {
                merging[thread] = nil
                problem = "Couldn't merge the pull request: \(e.message)"
            }
        }
    }

    // MARK: Asking the agent (the PR bar)

    /// Fill the thread's composer with a prompt to read over and send. With its VM gone, it
    /// goes to a new thread on the branch instead.
    func ask(_ prompt: String, in thread: String) {
        if details[thread]?.thread?.vmGone ?? summary(thread)?.thread.vmGone ?? false {
            guard let client else { return }
            Task {
                if case .failure(let e) = await client.request(.onBranch(thread, prompt: prompt)) { problem = "Couldn't start a thread on the branch: \(e.message)" }
            }
            return
        }
        drafts[thread] = prompt
    }

    /// Address review: the server reads the review and writes the prompt.
    func addressReview(_ thread: String) {
        guard let client, !reviewing.contains(thread) else { return }
        reviewing.insert(thread)
        Task {
            let r = await answer(client, .reviewPrompt(thread))
            reviewing.remove(thread)
            switch r {
            case .success(.prompt(let p)): ask(p, in: thread)
            case .success: break
            case .failure(let e): fail("Couldn't read the review: \(e.message)") { [weak self] in self?.addressReview(thread) }
            }
        }
    }

    /// The branch is there: New thread on it, with the prompt.
    private func branchChecked(_ c: (project: String, branch: OnBranch, prompt: String?, result: Result<Void, ServerError>)) {
        switch c.result {
        case .success:
            if let p = c.prompt { drafts[""] = p }
            newThread = NewThreadStart(project: c.project, branch: c.branch)
        case .failure(let e):
            problem = e.message
        }
    }

    // MARK: Small requests

    func markUnread(_ thread: String) {
        guard let client else { return }
        Task { await client.send(.markUnread(thread)) }
    }

    func markRead(_ thread: String) {
        guard let client else { return }
        Task { await client.send(.markViewed(thread)) }
    }

    /// Archive a thread (offering Undo for a few seconds), or bring one back.
    func archive(_ thread: String, _ on: Bool) {
        guard let client else { return }
        if on { noteArchived([thread]) }
        Task {
            if case .failure(let e) = await client.request(on ? .archive(thread) : .unarchive(thread)) {
                if on { forgetArchived(thread) }
                problem = "Couldn't \(on ? "archive" : "unarchive") the thread: \(e.message)"
            }
        }
    }

    /// Give the thread a new title. It shows at once.
    func rename(_ thread: String, to title: String) {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let client, !title.isEmpty, title != summary(thread)?.thread.title else { return }
        renaming[thread] = title
        Task {
            let r = await client.request(.rename(thread, title: title))
            if renaming[thread] == title { renaming[thread] = nil }
            if case .failure(let e) = r { problem = "Couldn't rename the thread: \(e.message)" }
        }
    }

    /// The title to show: one just asked for, else the server's.
    func title(_ thread: String, _ server: String?) -> String? { renaming[thread] ?? server }

    /// Delete the thread's VM (and its disk) and its history. It leaves the list, and the
    /// screen if it's open, at once; a delete that fails brings it back, saying why.
    func delete(_ thread: String) {
        guard let client else { return }
        deleting.insert(thread)
        path.removeAll { $0 == thread }
        Task {
            let r = await client.request(.delete(thread))
            deleting.remove(thread)
            if case .failure(let e) = r { problem = "Couldn't delete the thread: \(e.message)" }
        }
    }

    // MARK: Preview

    /// A private URL for `port` in the thread's VM, which the server wakes first if it's stopped
    /// (that takes a while). Nil if it didn't come, said as other failures are, or if another
    /// preview of the thread was asked for meanwhile.
    func preview(_ thread: String, port: UInt16) async -> URL? {
        guard let client else { return nil }
        let state = details[thread]?.thread?.state ?? summary(thread)?.thread.state
        let ask = PreviewAsk(port: port, waking: state == .stopped || state == .stopping)
        previewing[thread] = ask
        #if DEBUG
        // UI tests: a wait long enough to see, as a VM waking gives (`-FlotillaSlowPreview <ms>`;
        // an offline server refuses at once).
        let slow = UserDefaults.standard.integer(forKey: "FlotillaSlowPreview")
        if slow > 0 { try? await Task.sleep(for: .milliseconds(slow)) }
        #endif
        let r = await answer(client, .preview(thread, port: port), within: .seconds(120))
        // Another was asked for meanwhile: that one opens (or says why not), not this.
        guard previewing[thread] == ask else { return nil }
        previewing[thread] = nil
        switch r {
        case .success(.url(let u)):
            if let url = URL(string: u) { return url }
            problem = "Couldn't open the preview: the server's link doesn't read"
        case .success:
            problem = "Couldn't open the preview: the server sent no link"
        case .failure(let e):
            problem = "Couldn't open the preview: \(e.message)"
        }
        return nil
    }

    /// A thread's ports whose links anyone may open now.
    func publicLinks(_ id: String) -> [PublicLink] {
        details[id]?.publicLinks ?? []
    }

    /// Make a port's link public for `duration` (the server wakes a stopped VM first, which
    /// takes a while): the link is copied, and the top of the thread says so. Why not, if it
    /// wasn't made public.
    func makePublic(_ thread: String, port: UInt16, for duration: PublicFor) async -> String? {
        guard let client else { return "Not connected to the server" }
        let state = details[thread]?.thread?.state ?? summary(thread)?.thread.state
        let ask = PreviewAsk(port: port, waking: state == .stopped || state == .stopping, making: duration)
        previewing[thread] = ask
        let r = await answer(client, .makePublic(thread, port: port, for: duration), within: .seconds(120))
        if previewing[thread] == ask { previewing[thread] = nil }
        switch r {
        case .success(.url(let u)):
            UIPasteboard.general.string = u
            let note = PublicNote(words: "Port \(port) is public \(duration.words). Link copied.")
            publicNotes[thread] = note
            Task {
                try? await Task.sleep(for: .seconds(4))
                if publicNotes[thread] == note { publicNotes[thread] = nil }
            }
            return nil
        case .success:
            return "the server sent no link"
        case .failure(let e):
            return e.message
        }
    }

    /// The port's link needs the sign-in token again: why not, if it was refused.
    func makePrivate(_ thread: String, port: UInt16) async -> String? {
        guard let client else { return "Not connected to the server" }
        if case .failure(let e) = await answer(client, .makePrivate(thread, port: port)) { return e.message }
        return nil
    }

    /// Restart, start or stop one of a thread's services: why not, if it was refused. Its state
    /// in the thread's view changes as it goes.
    func service(_ request: Request) async -> String? {
        guard let client else { return "Not connected to the server" }
        if case .failure(let e) = await answer(client, request) { return e.message }
        return nil
    }

    // MARK: Archived, with Undo

    /// Offer to undo an archive for a few seconds; archives in that time join the same notice.
    /// `merged`: threads the server archived because their pull request merged, and its number.
    func noteArchived(_ ids: [String], merged: [String: Int64] = [:]) {
        let now = Date()
        if var n = archived, now.timeIntervalSince(n.at) < ArchiveNotice.lasts {
            n.ids += ids.filter { !n.ids.contains($0) }
            n.merged.merge(merged) { _, new in new }
            n.at = now
            archived = n
        } else {
            archived = ArchiveNotice(ids: ids, merged: merged, at: now)
        }
        Task {
            try? await Task.sleep(for: .seconds(ArchiveNotice.lasts))
            if let n = archived, Date().timeIntervalSince(n.at) >= ArchiveNotice.lasts - 0.05 { archived = nil }
        }
    }

    /// An archive that didn't happen leaves the notice.
    private func forgetArchived(_ id: String) {
        archived?.ids.removeAll { $0 == id }
        if archived?.ids.isEmpty == true { archived = nil }
    }

    /// Put the threads in the notice back. Their VMs stay stopped until the next send.
    func undoArchive() {
        guard let n = archived else { return }
        archived = nil
        for id in n.ids { archive(id, false) }
    }

    // MARK: Settings

    /// Change one of the server's settings. The screen shows it at once; the server's view
    /// follows (or the old value comes back, and why).
    func set(_ setting: Setting) {
        guard let client else { return }
        guard let before = settings else { return }
        apply(setting)
        Task {
            if case .failure(let e) = await client.request(.set(setting)) {
                switch setting {
                case .archiveOnMerge: apply(.archiveOnMerge(before.archiveOnMerge))
                case .archiveIdleDays: apply(.archiveIdleDays(before.archiveIdleDays))
                }
                problem = "Couldn't change the setting: \(e.message)"
            }
        }
    }

    private func apply(_ setting: Setting) {
        switch setting {
        case .archiveOnMerge(let on): settings?.archiveOnMerge = on
        case .archiveIdleDays(let d): settings?.archiveIdleDays = d
        }
    }

    /// Settings is on screen: the devices view comes and goes with it.
    func watchDevices(_ on: Bool) {
        guard let client else { return }
        if !on { devices = nil }
        Task { on ? await client.subscribe(.devices) : await client.unsubscribe(.devices) }
    }

    /// Check the server's setup again; returns when it's done.
    func checkAgain() async {
        guard let client else { return }
        if case .failure(let e) = await answer(client, .runDoctor, within: .seconds(30)) { problem = "Couldn't check the setup: \(e.message)" }
    }

    /// The usage sheet is open: what the agents spent comes while it is. Subscribing checks the
    /// VMs that are up (the server waits five minutes between those).
    func watchAgentUsage(_ on: Bool) {
        guard let client else { return }
        Task { on ? await client.subscribe(.agentUsage) : await client.unsubscribe(.agentUsage) }
    }

    /// The Agents tab's Refresh: check every VM that's up, the limits and the prices now. Nil
    /// once it's done, else why not. Reading several VMs takes a while: it gives up after 60 s.
    func refreshAgentUsage() async -> String? {
        guard let client else { return "Not connected to a server" }
        switch await within(.seconds(60), { await client.request(.readAgentUsage) }) {
        case .success?: return nil
        case .failure(let e)?: return e.message
        case nil: return "The server didn't answer in time"
        }
    }

    /// Read the account's usage from boat again: nil once it's in (the sidebar has it), else
    /// why not. Gives up after 15 s, so a Refresh never spins for good.
    func refreshUsage() async -> String? {
        guard let client else { return "Not connected to a server" }
        switch await within(.seconds(15), { await client.request(.readUsage) }) {
        case .success?: return nil
        case .failure(let e)?: return e.message
        case nil: return "The server didn't answer in time"
        }
    }

    /// A code for another device, or why not.
    func pairingCode() async -> Result<PairingCode, ServerError> {
        guard let client else { return .failure(ServerError("Not connected to a server")) }
        switch await answer(client, .pairCode) {
        case .success(.code(let c)): return .success(c)
        case .success: return .failure(ServerError("The server didn't give a code"))
        case .failure(let e): return .failure(e)
        }
    }

    /// Name a device on the server in use (this phone, or another).
    func rename(device id: String, to name: String) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let client, !name.isEmpty else { return }
        let mine = id == server?.device
        Task {
            switch await client.request(.renameDevice(id, name: name)) {
            case .success: if mine { UserDefaults.standard.set(name, forKey: "deviceName") }
            case .failure(let e): problem = "Couldn't rename the device: \(e.message)"
            }
        }
    }

    func revoke(device id: String) {
        guard let client else { return }
        Task {
            if case .failure(let e) = await client.request(.revokeDevice(id)) { problem = "Couldn't revoke the device: \(e.message)" }
        }
    }

    // MARK: Notifications

    /// Connected and caught up: ask for notifications once, and give the server this phone's
    /// address for them.
    private func online() {
        if !askedForNotifications {
            askedForNotifications = true
            Task { await notifications.ask() }
        }
        // Each connection: a server that forgot this phone (a rebuild, a relay that forgot it)
        // has it again.
        pushed = false
        registerPush()
        #if DEBUG
        testNote()
        #endif
    }

    #if DEBUG
    @ObservationIgnored private var testNoteHeard = false

    /// UI tests only (a debug build, `FLOTILLA_TEST_NOTE` set): a note as the server writes it
    /// when it archives threads by itself, which an offline server never does. The threads it
    /// names are archived first, as the server would have, then the note goes through the same
    /// handler as one from the server. Once a run.
    private func testNote() {
        guard !testNoteHeard, let client, let text = ProcessInfo.processInfo.environment["FLOTILLA_TEST_NOTE"],
              case .note(let note, _)? = Down(text: #"{"t":"note","note":"# + text + "}")
        else { return }
        testNoteHeard = true
        let ids = (note.prMerged.map { [$0.thread] } ?? []) + (note.idleArchived ?? [])
        Task {
            for id in ids { await client.send(.archive(id)) }
            // As the server would: the archives, then the note (and time for the test to look).
            try? await Task.sleep(for: .seconds(3))
            heard(note)
        }
    }
    #endif

    #if DEBUG
    /// UI tests: APNs gave this simulator no token (`-FlotillaPushToken <hex>`).
    func noPushToken() {
        guard pushToken == nil, let hex = UserDefaults.standard.string(forKey: "FlotillaPushToken"), hex.count % 2 == 0 else { return }
        pushToken = hex.lowercased()
        pushed = false
        registerPush()
    }
    #endif

    /// iOS gave the app its APNs token.
    func gotPushToken(_ token: Data) {
        let hex = token.map { String(format: "%02x", $0) }.joined()
        if hex != pushToken {
            pushToken = hex
            pushed = false
        }
        registerPush()
    }

    /// The server is given this phone's APNs token, and answers how it sends notifications. If
    /// it sends through this phone's push relay, the phone registers there (`RelayLink`) and
    /// gives the server what it got; if not, the phone leaves the relay.
    private func registerPush() {
        guard let pushToken, !pushed, connection == .online, let client, let server else { return }
        #if DEBUG
        let env = "sandbox"
        #else
        let env = "production"
        #endif
        let topic = Bundle.main.bundleIdentifier ?? "com.useflotilla.flotilla"
        pushed = true
        Task {
            // What it gave this server before, so the server keeps it meanwhile. A Keychain that
            // doesn't answer would read as none, and the server would drop it: again next time.
            let kept = RelayLink.kept(for: server.id)
            guard kept.readable else {
                pushed = false
                return
            }
            let had = kept.registration
            let answer = await client.request(.registerPush(token: pushToken, env: env, topic: topic, relay: had))
            guard case .success(let reply) = answer else {
                if case .failure(let e) = answer { log.error("the server didn't take this phone's notification token: \(e.message, privacy: .public)") }
                pushed = false
                return
            }
            // An older server answers Done: no relay.
            let uses: String? = if case .push(let relay) = reply { relay } else { nil }
            guard RelayLink.uses(uses) else {
                if had != nil {
                    RelayLink.leave(server: server.id)
                    _ = await client.request(.registerPush(token: pushToken, env: env, topic: topic))
                }
                return
            }
            let relay = await RelayLink.ensure(server: server.id, token: pushToken, env: env, topic: topic)
            if relay.registration != had,
               case .failure(let e) = await client.request(.registerPush(token: pushToken, env: env, topic: topic, relay: relay.registration)) {
                pushed = false
                log.error("the server didn't take this phone's push relay: \(e.message, privacy: .public)")
            }
            // The relay or the Keychain didn't answer: again on the next connection.
            if !relay.settled { pushed = false }
        }
    }

    /// What a notification that arrives while the app is open does: nothing if it's from
    /// another server (one this phone left), about the thread on screen, or about a merge the
    /// archive notice already told of.
    func shows(_ info: [AnyHashable: Any]) -> Bool {
        let (server, thread) = Self.about(info)
        guard let server, server == self.server?.id else { return false }
        guard let thread else { return true }
        if thread == onScreen { return false }
        let kind = (info["flotilla"] as? [String: Any])?["kind"] as? String
        return !(kind == "pr_merged" && mergesShown[thread].map { Date().timeIntervalSince($0) < 120 } == true)
    }

    /// A notification was tapped: its thread, if it's from this phone's server (one from a
    /// server it left does nothing).
    func opened(_ info: [AnyHashable: Any]) {
        let (server, thread) = Self.about(info)
        guard let server, server == self.server?.id else { return }
        showPairing = false
        showSettings = false
        newThread = nil
        startHere = nil
        if let thread { path = [thread] } else { path = [] }
    }

    /// The server and thread a notification is about (`flotilla_core::push::payload`).
    static func about(_ info: [AnyHashable: Any]) -> (server: String?, thread: String?) {
        let ours = info["flotilla"] as? [String: Any]
        return (ours?["server"] as? String, ours?["thread"] as? String)
    }
}

/// A thread this phone asked for that isn't written yet.
struct Starting: Hashable {
    let prompt: String
    let settings: ThreadSettings
    let files: [String]
    let at: Int64
    /// Its place among threads started together (one prompt on several models).
    var rank = 0
    /// The server has it (its VM may still be starting).
    var written = false
}

/// What New thread opens with: a project and an existing branch (from a pull request's
/// thread whose VM is gone).
struct NewThreadStart: Identifiable, Hashable {
    let id = UUID()
    var project: String?
    var branch: OnBranch?
    /// The agent chosen last time (a start that was refused), and the other models chosen
    /// beside it.
    var settings: ThreadSettings?
    var also: [ThreadSettings] = []
    /// Why the last start was refused.
    var problem: String?
}

/// Threads just archived, by you or by the server (a merged pull request's, threads left
/// idle), with Undo for a few seconds.
struct ArchiveNotice: Equatable {
    var ids: [String]
    /// The threads the server archived because their pull request merged: its number.
    var merged: [String: Int64] = [:]
    /// When the latest archive joined it.
    var at: Date

    /// Seconds it shows for after the latest archive.
    static let lasts: TimeInterval = 5

    /// "Archived 2 threads", or a merge's "#12 merged · Archived".
    var text: String {
        if ids.count == 1, let n = merged[ids[0]] { return "#\(n) merged · Archived" }
        return "Archived \(ids.count) thread\(ids.count == 1 ? "" : "s")"
    }
}

/// A file waiting to go with a message.
struct PendingFile: Identifiable {
    let id = UUID()
    var name: String
    var data: Data
    var kind: FileKind
    /// A small picture of it, for an image.
    var thumbnail: UIImage?
}

extension AppModel {
    /// How long a request that shows a spinner waits for its answer.
    static let patience: Duration = .seconds(20)
    static let noAnswer = "the server didn't answer"

    /// `request`'s answer, or a failure if none comes within `limit`: an offline server, or
    /// GitHub hanging, never leaves a spinner going for good.
    func answer(_ client: FlotillaClient, _ request: Request, within limit: Duration = patience) async -> Result<Reply, ServerError> {
        await within(limit) { await client.request(request) } ?? .failure(ServerError(Self.noAnswer))
    }

    /// Say what failed, with Try again.
    func fail(_ problem: String, retry: @escaping () -> Void) {
        self.problem = problem
        self.retry = Retry(problem: problem, action: retry)
    }

    /// Try again for the problem on screen: none if another problem has taken its place.
    var retryShown: (() -> Void)? { retry?.again(for: problem) }
}

/// Try again, for the problem it was offered with. Another problem said meanwhile (most say
/// none) isn't what it asks again, so it doesn't show there.
struct Retry {
    let problem: String
    let action: () -> Void

    func again(for shown: String?) -> (() -> Void)? { shown == problem ? action : nil }
}

/// `body`'s result, or nil if it took longer than `limit` (it's left to finish by itself: a
/// request's answer, say, which a client only gives up on when it stops).
func within<T: Sendable>(_ limit: Duration, _ body: @escaping @Sendable () async -> T) async -> T? {
    let once = Once<T?>()
    return await withCheckedContinuation { c in
        once.c = c
        Task { once.resume(await body()) }
        Task {
            try? await Task.sleep(for: limit)
            once.resume(nil)
        }
    }
}

/// A continuation resumed by whichever comes first (on the main actor).
private final class Once<T: Sendable> {
    var c: CheckedContinuation<T, Never>?

    func resume(_ v: T) {
        c?.resume(returning: v)
        c = nil
    }
}
