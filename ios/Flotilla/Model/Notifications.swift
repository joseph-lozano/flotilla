import Foundation
import Observation
import UIKit
import UserNotifications

/// Whether iOS lets Flotilla notify, as Settings shows it.
enum NotificationAccess: Equatable {
    case allowed, denied, notAsked

    init(_ status: UNAuthorizationStatus) {
        switch status {
        case .authorized, .provisional, .ephemeral: self = .allowed
        case .denied: self = .denied
        default: self = .notAsked
        }
    }
}

/// Where that comes from: iOS, or a stand-in.
protocol NotificationPermission {
    func status() async -> UNAuthorizationStatus
    func request() async
    /// For an APNs token, which goes to the server.
    func register()
}

struct SystemNotificationPermission: NotificationPermission {
    func status() async -> UNAuthorizationStatus {
        await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
    }

    func request() async {
        _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
    }

    func register() {
        UIApplication.shared.registerForRemoteNotifications()
    }
}

/// What iOS allows, read when Settings opens and each time the app comes back (from iOS's
/// Settings, say). Once allowed, the phone registers for its APNs token.
@Observable
final class Notifications {
    private(set) var access: NotificationAccess?
    @ObservationIgnored private let permission: any NotificationPermission

    init(permission: any NotificationPermission = Notifications.forLaunch()) {
        self.permission = permission
    }

    func refresh() async {
        let now = NotificationAccess(await permission.status())
        if now == .allowed, access != .allowed { permission.register() }
        access = now
    }

    /// iOS's question, if it hasn't been asked. Allowed, the phone registers whatever was read
    /// before: an earlier try may have failed.
    func ask() async {
        if await permission.status() == .notDetermined { await permission.request() }
        access = NotificationAccess(await permission.status())
        if access == .allowed { permission.register() }
    }

    static func forLaunch() -> any NotificationPermission {
        #if DEBUG
        // UI tests: `-FlotillaNotifications allowed|denied|notAsked`, whatever's asked.
        let given: UNAuthorizationStatus? = switch UserDefaults.standard.string(forKey: "FlotillaNotifications") {
        case "allowed": .authorized
        case "denied": .denied
        case "notAsked": .notDetermined
        default: nil
        }
        if let given { return StandInPermission(given) }
        #endif
        return SystemNotificationPermission()
    }
}

#if DEBUG
/// Says `now`; a request turns it into `answer`, if there is one.
final class StandInPermission: NotificationPermission {
    var now: UNAuthorizationStatus
    let answer: UNAuthorizationStatus?
    private(set) var requests = 0
    private(set) var registered = 0

    init(_ now: UNAuthorizationStatus, answer: UNAuthorizationStatus? = nil) {
        self.now = now
        self.answer = answer
    }

    func status() async -> UNAuthorizationStatus { now }

    func request() async {
        requests += 1
        if let answer { now = answer }
    }

    func register() { registered += 1 }
}
#endif
