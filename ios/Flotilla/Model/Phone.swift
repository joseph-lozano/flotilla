import FlotillaKit
import Foundation
import Observation
import UIKit

/// This phone: the one server it's paired with, and the model of what that server shows.
/// Pairing (with the same server or another) and forgetting make the model anew, so nothing
/// one pairing showed, and no late answer to it, reaches the next.
@Observable
final class Phone {
    private(set) var model: AppModel
    /// Pairings that worked, for their haptic.
    private(set) var pairings = 0

    init() {
        let (saved, dropped) = PairedServer.load()
        for d in dropped {
            Self.leave(d.server, token: d.token)
            RelayLink.leave(server: d.server.id)
        }

        var server = saved
        #if DEBUG
        if ProcessInfo.processInfo.arguments.contains("-FlotillaForgetServer") {
            if let s = server {
                Keychain.delete(server: s.id)
                RelayLink.leave(server: s.id)
            }
            server = nil
            PairedServer.save(nil)
            UserDefaults.standard.removeObject(forKey: "deviceName")
        }
        #endif
        var token: String?
        switch server.map({ Keychain.lookup(server: $0.id) }) {
        case .found(let t)?: token = t
        case .missing?:
            // Its token is gone (a restore onto another phone): pair again.
            server = nil
            PairedServer.save(nil)
        case .failed?:
            // The Keychain didn't answer: the pairing stays, for the next launch to try again,
            // and this one says why (`AppModel`).
            break
        case nil: break
        }
        model = AppModel(server: server, token: token)
        // A registration with the push relay for a server this phone isn't paired with goes, and
        // handles left before are asked to go again.
        RelayLink.reconcile(paired: server?.id)
        RelayLink.sweep()
    }

    /// Swap the code for a token and use that server, in place of the one before: its token
    /// leaves the Keychain, and it's asked to revoke this phone. `name`: what the server lists
    /// this phone as.
    func pair(server address: String, code: String, name: String) async throws(Pairing.Problem) {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines).nilIfBlank ?? UIDevice.current.name
        let paired = try await Pairing.pair(server: address, code: code, deviceName: name)
        let url = try Pairing.serverURL(address)
        let old = model.server
        // The one the model connected with, else the Keychain's (the model may have none, if
        // the Keychain didn't answer at launch).
        let oldToken = model.token ?? old.flatMap { Keychain.token(server: $0.id) }
        guard Keychain.set(paired.token, server: paired.server.id) else { throw Pairing.Problem("The phone's Keychain wouldn't keep the token") }
        let server = PairedServer(url: url, id: paired.server.id, device: paired.device)
        PairedServer.save(server)
        if let old, old.id != server.id { Keychain.delete(server: old.id) }
        UserDefaults.standard.set(name, forKey: "deviceName")
        // The push relay's handle for the last pairing goes with it.
        if let old { RelayLink.leave(server: old.id) }
        replace(with: AppModel(server: server, token: paired.token))
        pairings += 1
        // The same server: the new token can revoke the old device (any paired device may).
        if let old { Self.leave(old, token: old.id == server.id ? paired.token : oldToken) }
    }

    /// Leave the server: its token goes from the Keychain, the phone is back at pairing, the
    /// server is asked to revoke this phone (so it stops sending it notifications), and the
    /// push relay to forget the handle it had for it.
    func forget() {
        guard let server = model.server else { return }
        let token = model.token ?? Keychain.token(server: server.id)
        Keychain.delete(server: server.id)
        RelayLink.leave(server: server.id)
        PairedServer.save(nil)
        replace(with: AppModel(server: nil, token: nil))
        Self.leave(server, token: token)
    }

    private func replace(with next: AppModel) {
        model.stop()
        model = next
    }

    /// Ask `server` to revoke this phone's device there, if it answers within four seconds
    /// (it may be off, or gone). Nothing waits for it.
    private static func leave(_ server: PairedServer, token: String?) {
        guard let token, let url = URL(string: server.url) else { return }
        Task {
            let c = FlotillaClient(server: url, token: token, info: ClientInfo(app: "flotilla-ios", version: "0", platform: "ios"))
            await c.start()
            _ = await within(.seconds(4)) { await c.request(.revokeDevice(server.device)) }
            await c.stop()
        }
    }
}
