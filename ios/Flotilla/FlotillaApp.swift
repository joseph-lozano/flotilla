import FlotillaKit
import SwiftUI
import UserNotifications

@main
struct FlotillaApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @Environment(\.scenePhase) private var phase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(delegate.phone)
                .tint(Palette.link)
                .onOpenURL { delegate.phone.model.open(url: $0) }
        }
        .onChange(of: phase) { _, now in
            switch now {
            case .active: delegate.phone.model.scene(active: true)
            case .background: delegate.phone.model.scene(active: false)
            default: break
            }
        }
    }
}

/// What iOS tells the app about notifications: its APNs token, one arriving while it's open,
/// and one tapped. The phone lives here so a tap that launches the app finds its model.
///
/// The notification center calls on the main thread; its `async` forms would hand UIKit
/// their answer from elsewhere, which UIKit traps on, so these take completion handlers.
final class AppDelegate: NSObject, UIApplicationDelegate, @preconcurrency UNUserNotificationCenterDelegate {
    let phone = Phone()

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        phone.model.gotPushToken(deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: any Error) {
        // No network, or no aps-environment.
        #if DEBUG
        phone.model.noPushToken()
        #endif
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification, withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler(phone.model.shows(notification.request.content.userInfo) ? [.banner, .list, .sound] : [])
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse, withCompletionHandler completionHandler: @escaping () -> Void) {
        phone.model.opened(response.notification.request.content.userInfo)
        completionHandler()
    }
}

/// How the app looks: as the phone does, or always Day or Night.
enum Appearance: String, CaseIterable, Identifiable {
    case phone, day, night

    var id: String { rawValue }
    var label: String {
        switch self {
        case .phone: "Phone"
        case .day: "Day"
        case .night: "Night"
        }
    }

    /// Set on the app's windows rather than with `preferredColorScheme`, which leaves a sheet
    /// that's open as it was.
    func apply() {
        let style: UIUserInterfaceStyle = switch self {
        case .phone: .unspecified
        case .day: .light
        case .night: .dark
        }
        for case let scene as UIWindowScene in UIApplication.shared.connectedScenes {
            for window in scene.windows { window.overrideUserInterfaceStyle = style }
        }
    }
}

/// Pairing until there's a server, then its threads: made anew with each model, so nothing
/// on screen (a sheet, a search, a scroll) outlives its pairing.
struct RootView: View {
    @Environment(Phone.self) private var phone
    @AppStorage("appearance") private var appearance: Appearance = .phone

    var body: some View {
        Screens()
            .environment(phone.model)
            .id(ObjectIdentifier(phone.model))
            .onChange(of: appearance, initial: true) { appearance.apply() }
            .sensoryFeedback(.success, trigger: phone.pairings)
            #if DEBUG
            .overlay { KeychainProbe() }
            #endif
    }
}

private struct Screens: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        Group {
            if model.server == nil {
                PairingScreen()
            } else {
                NavigationStack(path: $model.path) {
                    ThreadsScreen()
                        .navigationDestination(for: String.self) { ThreadScreen(id: $0) }
                }
                .sheet(isPresented: $model.showPairing) { PairingScreen(cancellable: true) }
                .sheet(isPresented: $model.showSettings, onDismiss: {
                    if model.pairAfterSettings {
                        model.pairAfterSettings = false
                        model.showPairing = true
                    }
                }) { SettingsScreen() }
                .sheet(item: $model.newThread) { start in
                    // A check threads need failing: the setup, where New thread would be.
                    if model.settings?.stopsThreads == true {
                        NavigationStack {
                            SetupScreen().toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { model.newThread = nil } } }
                        }
                    } else {
                        NewThreadSheet(start: start)
                    }
                }
                .sheet(isPresented: $model.showSchedules) { SchedulesScreen() }
                .alert(model.problem ?? "", isPresented: Binding(get: { model.problem != nil }, set: { if !$0 { model.problem = nil; model.retry = nil } })) {
                    if let again = model.retryShown {
                        Button("Try again") { again() }
                    }
                    Button("OK", role: .cancel) {}
                }
            }
        }
        // Only what means something: a message or start that didn't go (and, above, a pairing
        // that did).
        .sensoryFeedback(.error, trigger: model.sendFailures)
    }
}

#if DEBUG
/// UI tests only (`-FlotillaKeychain`): the servers this phone has a token for, as an
/// element's label, so a test can check one left the Keychain.
private struct KeychainProbe: View {
    @Environment(Phone.self) private var phone

    var body: some View {
        if ProcessInfo.processInfo.arguments.contains("-FlotillaKeychain") {
            // Read again with each model (a pairing, a forget).
            let _ = phone.model
            let ids = Keychain.servers()
            Color.clear
                .frame(width: 1, height: 1)
                .accessibilityElement()
                .accessibilityLabel(ids.isEmpty ? "none" : ids.joined(separator: ","))
                .accessibilityIdentifier("keychain")
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .allowsHitTesting(false)
        }
    }
}
#endif
