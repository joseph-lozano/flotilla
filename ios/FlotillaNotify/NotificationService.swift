import FlotillaKit
import Foundation
import UserNotifications

/// Puts in the words of a notification that came through the push relay: the relay sends only
/// "Flotilla: New activity" with the server's ciphertext (`e`), which this phone's key opens
/// into the notification its server wrote (title, body, grouping, and the `flotilla` fields a
/// tap reads). One it can't open (no key here, another server's, a key since forgotten), or one
/// sealed more than a day ago (replayed), shows as it came: the neutral alert.
nonisolated final class NotificationService: UNNotificationServiceExtension {
    private var deliver: ((UNNotificationContent) -> Void)?
    private var content: UNMutableNotificationContent?

    override func didReceive(_ request: UNNotificationRequest, withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        let content = (request.content.mutableCopy() as? UNMutableNotificationContent) ?? UNMutableNotificationContent()
        deliver = contentHandler
        self.content = content
        if let sealed = content.userInfo["e"] as? String,
           let note = Self.open(sealed, keys: RelayKeychain.load(group: Self.group).state?.keys ?? []),
           note.fresh(at: Date()) {
            content.title = note.title
            content.body = note.body
            if let thread = note.threadID { content.threadIdentifier = thread }
            var info = content.userInfo
            info["e"] = nil
            info["flotilla"] = note.flotilla
            content.userInfo = info
        }
        contentHandler(content)
        deliver = nil
    }

    /// Out of time: what it has (the neutral alert, if it got no further).
    override func serviceExtensionTimeWillExpire() {
        if let deliver, let content { deliver(content) }
        deliver = nil
    }

    /// The Keychain group the app keeps the relay's state in (Info.plist).
    static var group: String? {
        (Bundle.main.object(forInfoDictionaryKey: "FlotillaKeychainGroup") as? String).flatMap { $0.isEmpty || $0.hasPrefix("$(") ? nil : $0 }
    }

    static func open(_ sealed: String, keys: [String]) -> PushNote? {
        for key in keys {
            if let data = PushSeal.open(sealed, key: key), let note = PushNote(json: data) { return note }
        }
        return nil
    }
}
