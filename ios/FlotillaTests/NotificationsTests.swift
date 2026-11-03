import Testing
import UserNotifications
@testable import Flotilla

/// Settings → Notifications: what iOS allows, read through a stand-in.
struct NotificationsTests {
    @Test func `iOS's answers read as on, off or not asked`() {
        #expect(NotificationAccess(.authorized) == .allowed)
        #expect(NotificationAccess(.provisional) == .allowed)
        #expect(NotificationAccess(.ephemeral) == .allowed)
        #expect(NotificationAccess(.denied) == .denied)
        #expect(NotificationAccess(.notDetermined) == .notAsked)
    }

    @Test func `allowed: on, registered for a token once`() async {
        let p = StandInPermission(.authorized)
        let n = Notifications(permission: p)
        await n.refresh()
        #expect(n.access == .allowed)
        await n.refresh()
        #expect(p.registered == 1 && p.requests == 0)
    }

    /// Read as allowed before the first connection (whose registering could have failed): the
    /// connection's ask registers again.
    @Test func `allowed: the connection's ask registers again`() async {
        let p = StandInPermission(.authorized)
        let n = Notifications(permission: p)
        await n.refresh()
        await n.ask()
        #expect(n.access == .allowed && p.registered == 2 && p.requests == 0)
    }

    @Test func `denied: off, and iOS isn't asked again`() async {
        let p = StandInPermission(.denied, answer: .authorized)
        let n = Notifications(permission: p)
        await n.refresh()
        #expect(n.access == .denied)
        await n.ask()
        #expect(n.access == .denied && p.requests == 0 && p.registered == 0)
    }

    @Test func `not asked: asking asks, then on and registered`() async {
        let p = StandInPermission(.notDetermined, answer: .authorized)
        let n = Notifications(permission: p)
        await n.refresh()
        #expect(n.access == .notAsked)
        await n.ask()
        #expect(n.access == .allowed && p.requests == 1 && p.registered == 1)
    }

    @Test func `not asked, then refused: off`() async {
        let p = StandInPermission(.notDetermined, answer: .denied)
        let n = Notifications(permission: p)
        await n.ask()
        #expect(n.access == .denied && p.requests == 1 && p.registered == 0)
    }

    /// Turned on in iOS's Settings: on when the app comes back, and registered then.
    @Test func `allowed while away: on once read again`() async {
        let p = StandInPermission(.denied)
        let n = Notifications(permission: p)
        await n.refresh()
        p.now = .authorized
        await n.refresh()
        #expect(n.access == .allowed && p.registered == 1)
    }
}
