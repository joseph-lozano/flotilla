import FlotillaKit
import Foundation
import os

/// This phone's registration with the push relay, for the server it's paired with
/// (relay/README.md). A server without the app's APNs key sends notifications through the
/// relay, sealed with a key only this phone and its server have; the extension opens them.
///
/// - Made only when the phone's server says it sends through this phone's relay (`uses`, from
///   `push.register`'s answer), and refreshed once a launch (a new token moves the handle with
///   it; one the relay forgot is made again).
/// - Left on Forget, on pairing again, and when the server says it doesn't use the relay: at
///   once, on this thread, the registration becomes a leftover and its key goes, so nothing
///   from that pairing opens even if the app ends there. The relay is then asked to forget the
///   handle, now and at later launches until it does (or for a week). Each launch also leaves
///   a registration for a server the phone isn't paired with any more (`reconcile`).
/// - Kept in the Keychain group the extension shares (`RelayKeychain`). A build without that
///   group doesn't register at all, and a Keychain that doesn't answer changes nothing.
@MainActor
enum RelayLink {
    private static let log = Logger(subsystem: "com.useflotilla.flotilla", category: "relay")

    /// The relay this build registers with, normalized: Info.plist's (the `FLOTILLA_PUSH_RELAY`
    /// build setting), or a debug build's `-FlotillaPushRelay <url>`. None: no relay.
    static var url: String? {
        var s = Bundle.main.object(forInfoDictionaryKey: "FlotillaPushRelay") as? String
        #if DEBUG
        if let given = UserDefaults.standard.string(forKey: "FlotillaPushRelay") { s = given }
        #endif
        guard let s, !s.isEmpty, !s.hasPrefix("$(") else { return nil }
        return RelayAddress.normalized(s)
    }

    static var group: String? {
        (Bundle.main.object(forInfoDictionaryKey: "FlotillaKeychainGroup") as? String).flatMap { $0.isEmpty || $0.hasPrefix("$(") ? nil : $0 }
    }

    /// Whether a server whose `push.register` answered `relay` sends through this phone's relay.
    static func uses(_ relay: String?) -> Bool {
        guard let relay, let mine = url else { return false }
        return RelayAddress.normalized(relay) == mine
    }

    /// A leftover the relay hasn't forgotten in this long is dropped (it drops a handle after 90
    /// days unused anyway).
    static let leftoverFor: TimeInterval = 7 * 24 * 3600

    /// Handles refreshed this launch.
    private static var refreshed: Set<String> = []
    /// One more for each leave: an `ensure` under way when it began keeps nothing it got.
    private static var epoch = 0
    /// The last thing begun: each waits for the one before, so two never change the state at once.
    private static var last: Task<Void, Never>?

    private static func serially<T: Sendable>(_ op: @escaping @MainActor () async -> T) async -> T {
        let before = last
        let task = Task { @MainActor in
            await before?.value
            return await op()
        }
        last = Task { _ = await task.value }
        return await task.value
    }

    /// What to give the server with this phone's APNs token. Unsettled: the relay or the
    /// Keychain didn't answer, so `registration` is the last one (if any) and it's tried again on
    /// the next connection.
    struct Outcome: Sendable, Equatable {
        let registration: RelayRegistration?
        let settled: Bool
    }

    /// What this phone has with the relay for `server`, as kept (no network). Unreadable: the
    /// Keychain didn't answer, so nothing is known.
    static func kept(for server: String) -> (registration: RelayRegistration?, readable: Bool) {
        guard let url, let group else { return (nil, true) }
        guard let state = RelayKeychain.load(group: group).state else { return (nil, false) }
        guard let c = state.current, c.server == server, c.relay == url else { return (nil, true) }
        return (c, true)
    }

    /// This phone's registration with the relay for `server`, made or refreshed. None (settled):
    /// no relay, or no Keychain group to keep its key in.
    static func ensure(server: String, token: String, env: String, topic: String) async -> Outcome {
        guard let url, let group, let base = URL(string: url) else { return Outcome(registration: nil, settled: true) }
        let began = epoch
        let r: Outcome = await serially {
            let client = RelayClient(url: base)
            guard var state = RelayKeychain.load(group: group).state, epoch == began else { return Outcome(registration: nil, settled: false) }
            var changed = false
            if let c = state.current, c.server != server || c.relay != url {
                state.leave()
                changed = true
            }
            if var c = state.current {
                if c.token == token, c.env == env, refreshed.contains(c.handle) { return Outcome(registration: c, settled: true) }
                let answer = await client.refresh(c, token: token, env: env)
                // Left meanwhile: the leave stands.
                guard epoch == began else { return Outcome(registration: nil, settled: false) }
                switch answer {
                case .done:
                    let moved = c.token != token || c.env != env
                    let before = c
                    c.token = token
                    c.env = env
                    state.current = c
                    if moved || changed, !RelayKeychain.save(state, group: group) {
                        log.error("the Keychain wouldn't keep the push relay's registration")
                        return Outcome(registration: before, settled: false)
                    }
                    refreshed.insert(c.handle)
                    return Outcome(registration: c, settled: true)
                case .gone:
                    state.current = nil
                    changed = true
                case .failed(let why):
                    // The relay may be down: the server keeps what it has until it's back.
                    log.error("the push relay didn't refresh this phone: \(why, privacy: .public)")
                    if changed { RelayKeychain.save(state, group: group) }
                    return Outcome(registration: c, settled: false)
                }
            }
            let got = await client.register(token: token, topic: topic, env: env)
            guard epoch == began else {
                // Left while it registered: the new handle goes too.
                if case .success(let g) = got { add(RelayLeftover(relay: url, handle: g.handle, secret: g.secret)) }
                return Outcome(registration: nil, settled: false)
            }
            guard case .success(let g) = got else {
                if changed { RelayKeychain.save(state, group: group) }
                log.error("the push relay didn't register this phone")
                return Outcome(registration: nil, settled: false)
            }
            let made = RelayRegistration(relay: url, server: server, handle: g.handle, secret: g.secret, key: PushSeal.newKey(), token: token, env: env)
            state.current = made
            guard RelayKeychain.save(state, group: group) else {
                // Nowhere the extension could find the key: the relay forgets the handle at once.
                log.error("the Keychain wouldn't keep the push relay's key")
                _ = await client.unregister(handle: made.handle, secret: made.secret)
                return Outcome(registration: nil, settled: false)
            }
            refreshed.insert(made.handle)
            return Outcome(registration: made, settled: true)
        }
        sweep()
        return r
    }

    /// Leave `server`'s registration (any, with nil), at once: before this returns, it's a
    /// leftover and its key is gone. A Keychain that doesn't answer changes nothing; the next
    /// launch's `reconcile` leaves it then.
    static func leave(server: String?) {
        epoch += 1
        guard let group, case .found(var state) = RelayKeychain.load(group: group),
              let c = state.current, server == nil || c.server == server
        else { return }
        state.leave(server: server)
        if !RelayKeychain.save(state, group: group) { log.error("the Keychain wouldn't keep leaving the push relay") }
        sweep()
    }

    /// At launch: a registration for a server this phone isn't paired with (left while the app
    /// ended, or a Keychain that didn't answer then) is left now.
    static func reconcile(paired server: String?) {
        guard let group, let c = RelayKeychain.load(group: group).state?.current, c.server != server else { return }
        leave(server: c.server)
    }

    /// Add `l` to the handles to ask the relay to forget.
    private static func add(_ l: RelayLeftover) {
        guard let group, var state = RelayKeychain.load(group: group).state else { return }
        state.leftovers.append(l)
        RelayKeychain.save(state, group: group)
        sweep()
    }

    /// Ask the relay to forget the handles this phone left; those it forgot (or never knew), and
    /// those it hasn't in a week, go. Its requests don't hold up a registration.
    static func sweep() {
        guard let group, let state = RelayKeychain.load(group: group).state, !state.leftovers.isEmpty else { return }
        let todo = state.leftovers
        Task {
            var done: Set<String> = []
            for l in todo {
                guard let u = URL(string: l.relay) else {
                    done.insert(l.handle)
                    continue
                }
                switch await RelayClient(url: u).unregister(handle: l.handle, secret: l.secret) {
                case .done, .gone: done.insert(l.handle)
                case .failed: if Date().timeIntervalSince(l.since) > leftoverFor { done.insert(l.handle) }
                }
            }
            guard !done.isEmpty else { return }
            await serially {
                guard case .found(var now) = RelayKeychain.load(group: group) else { return }
                now.leftovers.removeAll { done.contains($0.handle) }
                RelayKeychain.save(now, group: group)
            }
        }
    }
}
