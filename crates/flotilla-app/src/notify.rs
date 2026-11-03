//! Notifications on this computer: a banner for what the server says is news (`Note::Alert`,
//! the rule phones are pushed by, so there's one), and the count of unread threads on the Dock
//! icon (a launcher's badge on Linux).
//!
//! - **Banners** go through GPUI's system notifications: `UNUserNotificationCenter` on a Mac
//!   (only in an app bundle: a bare `cargo run` binary shows none) and freedesktop
//!   notifications over D-Bus on Linux (none, quietly, without a notification daemon). A click
//!   brings the window forward and opens the thread, or the project for a setup.
//! - **When.** Not while the window is focused: the in-app toasts say it then, so the thread on
//!   screen never gets one. Each alert once (by its id), however many windows or reconnects.
//!   On a Mac a newer one for the same thread replaces the last, and opening the thread (or
//!   coming back to it) clears it; GPUI's Linux notifications can't be replaced or taken back,
//!   so there they age out.
//! - **Asking (Mac).** Not at launch: when a prompt is first sent (it'll finish later), when
//!   Notifications is switched on, or else at the first banner, which waits for the answer.
//! - **The badge** counts the sidebar's Unread threads, so it clears as they're read.
//! - **Settings → Notifications**, kept in `device.json` (on unless switched off), covers both.

use flotilla_proto::{Activity, Alert, AlertKind, ThreadSummary};
use gpui_kit::{App, Global, SharedString, SystemNotification};
use std::collections::{HashMap, VecDeque};

/// Alert ids remembered, so none is shown twice.
const REMEMBER: usize = 512;
/// Banners held while the Mac asks whether it may show them.
const HOLD: usize = 8;
/// Between the parts of a banner's tag.
const SEP: char = '\u{1f}';

/// Whether this computer lets Flotilla show banners.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Permission {
    /// Not looked at yet.
    #[default]
    Unknown,
    /// Never asked (a Mac).
    NotAsked,
    Allowed,
    /// Turned off in System Settings.
    Denied,
    /// None here: a Mac build run outside its app bundle.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Unavailable,
}

/// What a banner opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Thread(String),
    Project(String),
}

/// Whether an alert becomes a banner here: switched on, and the window isn't focused (then the
/// in-app toasts say it, and the thread on screen needs nothing).
pub fn wanted(on: bool, focused: bool) -> bool {
    on && !focused
}

/// A banner's tag: one per thread (or project) per server, so a newer one replaces it and
/// opening the thread clears it; a click reads it back ([`target`]).
pub fn tag(server: &str, target: &Target) -> String {
    match target {
        Target::Thread(id) => format!("{server}{SEP}thread{SEP}{id}"),
        Target::Project(id) => format!("{server}{SEP}project{SEP}{id}"),
    }
}

/// What an alert opens: its thread, or the project whose setup failed.
pub fn target_of(alert: &Alert) -> Target {
    match (&alert.thread, alert.kind) {
        (Some(id), _) => Target::Thread(id.clone()),
        (None, AlertKind::SetupFailed) => Target::Project(alert.about.clone()),
        (None, _) => Target::Thread(alert.about.clone()),
    }
}

/// The server and what a banner's [`tag`] opens.
pub fn target(tag: &str) -> Option<(String, Target)> {
    let mut parts = tag.rsplitn(3, SEP);
    let (id, kind, server) = (parts.next()?, parts.next()?, parts.next()?);
    let target = match kind {
        "thread" => Target::Thread(id.to_string()),
        "project" => Target::Project(id.to_string()),
        _ => return None,
    };
    (!id.is_empty()).then(|| (server.to_string(), target))
}

/// The Dock's count: threads the sidebar lists under Unread, whatever its filter. A thread
/// another handed work to is listed inside that one, not on its own.
pub fn unread(summaries: &[ThreadSummary], parents: &HashMap<String, String>) -> usize {
    let present: std::collections::HashSet<&str> =
        summaries.iter().map(|s| s.thread.id.as_str()).collect();
    summaries
        .iter()
        .filter(|s| {
            s.activity == Activity::Unread
                && !parents
                    .get(&s.thread.id)
                    .is_some_and(|p| present.contains(p.as_str()))
        })
        .count()
}

/// What's been shown, what the computer allows, and what the badge says: one for the app,
/// however many windows it has.
#[derive(Default)]
pub struct Notifier {
    seen: VecDeque<String>,
    pub permission: Permission,
    /// Asked once this run (the Mac asks once ever; then it's System Settings).
    asked: bool,
    held: Vec<SystemNotification>,
    badge: Option<usize>,
    /// Where the platform's answers about permission go (to the main thread).
    #[cfg(target_os = "macos")]
    answers: Option<smol::channel::Sender<Permission>>,
    #[cfg(target_os = "linux")]
    launcher: Option<std::sync::mpsc::Sender<usize>>,
}

impl Global for Notifier {}

impl Notifier {
    /// The first time `id` comes; false after (a replay, another window's copy).
    pub fn first_time(&mut self, id: &str) -> bool {
        if self.seen.iter().any(|s| s == id) {
            return false;
        }
        if self.seen.len() >= REMEMBER {
            self.seen.pop_front();
        }
        self.seen.push_back(id.to_string());
        true
    }

    /// `n` to show now, or none: held until the computer says whether it may be shown (asking,
    /// if it never has), or not allowed.
    fn offer(&mut self, n: SystemNotification) -> Option<SystemNotification> {
        match self.permission {
            Permission::Allowed => Some(n),
            Permission::Unknown | Permission::NotAsked => {
                self.held.retain(|h| h.tag != n.tag);
                if self.held.len() >= HOLD {
                    self.held.remove(0);
                }
                self.held.push(n);
                if self.permission == Permission::NotAsked {
                    self.ask();
                }
                None
            }
            Permission::Denied | Permission::Unavailable => None,
        }
    }

    /// The computer said (or was asked) what it allows: the held banners to show now. Only a
    /// Mac says; elsewhere nothing asks.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn allows(&mut self, p: Permission) -> Vec<SystemNotification> {
        if std::env::var_os("FLOTILLA_LOG").is_some() && p != self.permission {
            eprintln!("[notify] permission {p:?}, {} held", self.held.len());
        }
        self.permission = p;
        match p {
            Permission::Allowed => std::mem::take(&mut self.held),
            // Held for an alert that came before the word that it was never asked.
            Permission::NotAsked if !self.held.is_empty() => {
                self.ask();
                vec![]
            }
            Permission::NotAsked | Permission::Unknown => vec![],
            Permission::Denied | Permission::Unavailable => {
                self.held.clear();
                vec![]
            }
        }
    }

    /// `tag`'s thread was read: a banner held for it won't be shown. Whether one may be on screen
    /// to take back.
    fn read(&mut self, tag: &str) -> bool {
        self.held.retain(|h| h.tag.as_ref() != tag);
        self.permission == Permission::Allowed
    }

    /// Ask the Mac, once this run, whether banners may be shown.
    fn ask(&mut self) {
        if !std::mem::replace(&mut self.asked, true) {
            #[cfg(target_os = "macos")]
            if let Some(tx) = &self.answers {
                mac::request(tx.clone());
            }
        }
    }
}

/// Start, before the window: say who the app is (Linux's notifications are named by it), and
/// find out what the Mac allows, without asking.
pub fn init(cx: &mut App) {
    let flavor = flotilla_model::flavor::get();
    cx.set_app_identity(flavor.app_id(), flavor.app_name());
    #[allow(unused_mut)]
    let mut notifier = Notifier {
        permission: Permission::Allowed,
        ..Notifier::default()
    };
    #[cfg(target_os = "macos")]
    {
        if mac::bundled() {
            let (tx, rx) = smol::channel::unbounded::<Permission>();
            notifier.permission = Permission::Unknown;
            mac::status(tx.clone());
            notifier.answers = Some(tx);
            cx.spawn(async move |cx| {
                while let Ok(p) = rx.recv().await {
                    cx.update(|cx| {
                        for n in cx.global_mut::<Notifier>().allows(p) {
                            cx.show_system_notification(n);
                        }
                    });
                }
            })
            .detach();
        } else {
            notifier.permission = Permission::Unavailable;
        }
    }
    #[cfg(target_os = "linux")]
    {
        notifier.launcher = Some(launcher::start());
    }
    cx.set_global(notifier);
}

/// What a click on a banner does, given its tag.
pub fn on_click(cx: &mut App, f: impl Fn(String, &mut App) + 'static) {
    cx.on_system_notification_response(move |r, cx| f(r.tag.to_string(), cx));
}

/// An alert from the server `server` (its key), if it's the first time it's come.
pub fn alert(cx: &mut App, server: &str, id: &str, alert: &Alert, on: bool, focused: bool) {
    if !cx.has_global::<Notifier>() || !cx.global_mut::<Notifier>().first_time(id) {
        return;
    }
    if !wanted(on, focused) {
        if std::env::var_os("FLOTILLA_LOG").is_some() {
            eprintln!(
                "[notify] {} for {:?}: no banner ({})",
                alert.kind.as_str(),
                alert.about,
                if on { "the window is focused" } else { "off" }
            );
        }
        return;
    }
    let n = SystemNotification {
        tag: SharedString::from(tag(server, &target_of(alert))),
        title: SharedString::from(alert.title.clone()),
        body: SharedString::from(alert.body.clone()),
        actions: vec![],
    };
    if std::env::var_os("FLOTILLA_LOG").is_some() {
        eprintln!(
            "[notify] {} for {:?}: {} ({:?})",
            alert.kind.as_str(),
            alert.about,
            alert.title,
            cx.global::<Notifier>().permission
        );
    }
    if let Some(n) = cx.global_mut::<Notifier>().offer(n) {
        cx.show_system_notification(n);
    }
}

/// Clear what's been shown about this thread (it's open now).
pub fn read(cx: &mut App, server: &str, thread: &str) {
    let tag = tag(server, &Target::Thread(thread.to_string()));
    if cx.has_global::<Notifier>() && cx.global_mut::<Notifier>().read(&tag) {
        cx.dismiss_system_notification(&tag);
    }
}

/// A moment to ask the Mac whether banners may be shown: a prompt was just sent, or
/// Notifications was switched on. Asked only if it never has been.
pub fn moment(cx: &mut App, on: bool) {
    if on && cx.has_global::<Notifier>() {
        let n = cx.global_mut::<Notifier>();
        if n.permission == Permission::NotAsked {
            n.ask();
        }
    }
}

/// Look again at what the Mac allows (its System Settings may have changed it).
pub fn recheck(cx: &mut App) {
    #[cfg(target_os = "macos")]
    if let Some(tx) = cx.try_global::<Notifier>().and_then(|n| n.answers.clone()) {
        mac::status(tx);
    }
    let _ = cx;
}

pub fn permission(cx: &App) -> Permission {
    cx.try_global::<Notifier>()
        .map(|n| n.permission)
        .unwrap_or_default()
}

/// Show `count` on the app's icon (none at 0), if it's not what it says already.
pub fn badge(cx: &mut App, count: usize) {
    let Some(n) = cx.try_global::<Notifier>() else {
        return;
    };
    if n.badge == Some(count) {
        return;
    }
    cx.global_mut::<Notifier>().badge = Some(count);
    if std::env::var_os("FLOTILLA_LOG").is_some() {
        eprintln!("[notify] badge {count}");
    }
    #[cfg(target_os = "macos")]
    mac::badge(count);
    #[cfg(target_os = "linux")]
    if let Some(tx) = &cx.global::<Notifier>().launcher {
        let _ = tx.send(count);
    }
}

/// System Settings' page for this app's notifications (a Mac's: nowhere else refuses them).
pub fn settings_url() -> String {
    format!(
        "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={}",
        flotilla_model::flavor::get().app_id()
    )
}

/// The Mac's side, where GPUI has none: whether banners are allowed, asking, and the Dock.
#[cfg(target_os = "macos")]
mod mac {
    use super::Permission;
    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::runtime::Bool;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNNotificationSettings,
        UNUserNotificationCenter,
    };
    use std::ptr::NonNull;

    /// `UNUserNotificationCenter` aborts the process outside an app bundle; a bundle has an id.
    pub fn bundled() -> bool {
        NSBundle::mainBundle().bundleIdentifier().is_some()
    }

    /// What's allowed now, without asking.
    pub fn status(tx: smol::channel::Sender<Permission>) {
        let done = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the center passes a valid settings object for the call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            let p = match status {
                UNAuthorizationStatus::NotDetermined => Permission::NotAsked,
                UNAuthorizationStatus::Denied => Permission::Denied,
                _ => Permission::Allowed,
            };
            let _ = tx.try_send(p);
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&done);
    }

    /// Ask (the Mac shows its own prompt, once ever), then say what was answered.
    pub fn request(tx: smol::channel::Sender<Permission>) {
        let done = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            let _ = tx.try_send(if granted.as_bool() {
                Permission::Allowed
            } else {
                Permission::Denied
            });
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &done,
            );
    }

    /// The Dock icon's badge: the count, or none. On the main thread (GPUI's).
    pub fn badge(count: usize) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(label.as_deref());
    }
}

/// Linux's launcher badge: Unity's `LauncherEntry` signal, which Ubuntu's dock, Dash to Dock
/// and Plasma's task manager read. Sent on a thread of its own (D-Bus can take a moment), which
/// keeps its connection: a dock drops the count when the sender goes.
#[cfg(target_os = "linux")]
mod launcher {
    use std::collections::HashMap;
    use zbus::zvariant::Value;

    pub fn start() -> std::sync::mpsc::Sender<usize> {
        let (tx, rx) = std::sync::mpsc::channel::<usize>();
        let _ = std::thread::Builder::new()
            .name("flotilla-badge".into())
            .spawn(move || {
                let mut bus: Option<zbus::Connection> = None;
                while let Ok(mut count) = rx.recv() {
                    // Only the latest matters.
                    while let Ok(newer) = rx.try_recv() {
                        count = newer;
                    }
                    if bus.is_none() {
                        bus = smol::block_on(zbus::Connection::session()).ok();
                    }
                    let Some(c) = &bus else { continue };
                    if let Err(e) = smol::block_on(update(c, count)) {
                        eprintln!("[notify] the launcher's count wasn't sent: {e}");
                        bus = None;
                    }
                }
            });
        tx
    }

    async fn update(c: &zbus::Connection, count: usize) -> zbus::Result<()> {
        let uri = format!(
            "application://{}.desktop",
            flotilla_model::flavor::get().app_id()
        );
        let props: HashMap<&str, Value> = HashMap::from([
            ("count", Value::I64(count as i64)),
            ("count-visible", Value::Bool(count > 0)),
        ]);
        c.emit_signal(
            None::<zbus::names::BusName>,
            "/com/useflotilla/flotilla",
            "com.canonical.Unity.LauncherEntry",
            "Update",
            &(uri, props),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_proto::{Thread, ThreadState};

    fn alert(kind: AlertKind, thread: Option<&str>, about: &str) -> Alert {
        Alert {
            kind,
            title: "Fix the retry".into(),
            body: "Finished.".into(),
            thread: thread.map(str::to_string),
            about: about.into(),
        }
    }

    #[test]
    fn a_banner_only_when_the_window_isnt_focused() {
        // Focused: the in-app toast says it, and the thread on screen needs nothing.
        assert!(!wanted(true, true));
        // Away (another app, minimised, hidden): every alert, the open thread's too.
        assert!(wanted(true, false));
        // Switched off in Settings: never.
        assert!(!wanted(false, false) && !wanted(false, true));
    }

    #[test]
    fn each_alert_once() {
        let mut n = Notifier::default();
        assert!(n.first_time("e1-1"));
        // A reconnect's replay, or another window's copy of it.
        assert!(!n.first_time("e1-1"));
        assert!(n.first_time("e1-2"));
        // The server's next run counts again from 1, under a new epoch.
        assert!(n.first_time("e2-1"));
        // Only the last few hundred are kept.
        for i in 0..REMEMBER {
            n.first_time(&format!("x-{i}"));
        }
        assert!(n.first_time("e1-1"));
        assert_eq!(n.seen.len(), REMEMBER);
    }

    #[test]
    fn a_tag_says_what_a_click_opens() {
        let a = alert(AlertKind::TurnFinished, Some("t1"), "t1");
        let t = tag("srv_3f5c1a7e92d4", &target_of(&a));
        assert_eq!(
            target(&t),
            Some(("srv_3f5c1a7e92d4".into(), Target::Thread("t1".into())))
        );
        // One per thread: an answer and then a finished turn replace each other.
        assert_eq!(
            tag(
                "s",
                &target_of(&alert(AlertKind::AgentsAnswered, Some("t1"), "t1"))
            ),
            tag("s", &target_of(&a))
        );
        // A setup's opens its project; a server's key may have anything in it.
        let setup = alert(AlertKind::SetupFailed, None, "p1");
        let key = "url:http://127.0.0.1:7777";
        assert_eq!(
            target(&tag(key, &target_of(&setup))),
            Some((key.into(), Target::Project("p1".into())))
        );
        assert_eq!(target("nonsense"), None);
        assert_eq!(target(&format!("s{SEP}vm{SEP}x")), None);
    }

    fn row(id: &str, activity: Activity) -> ThreadSummary {
        let thread = Thread {
            id: id.into(),
            environment_id: None,
            environment_name: None,
            title: id.into(),
            sandbox_id: None,
            conversation_id: None,
            provider: "claude".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state: ThreadState::Idle,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            repo_project_id: None,
            branch: None,
            base_sha: None,
        };
        ThreadSummary {
            thread,
            activity,
            last_activity_ms: 0,
            turn_started_ms: None,
            last_reply: None,
            last_prompt: None,
            pr: None,
            start_failed: false,
            prompted_ms: None,
            opens: None,
        }
    }

    #[test]
    fn the_badge_counts_unread_threads_as_the_sidebar_lists_them() {
        let mut rows = vec![
            row("a", Activity::Unread),
            row("b", Activity::Idle),
            row("c", Activity::Working),
            row("d", Activity::Unread),
            row("e", Activity::Archived),
        ];
        let mut parents = HashMap::new();
        assert_eq!(unread(&rows, &parents), 2);
        // A delegate listed inside the thread that handed it work isn't counted on its own...
        parents.insert("d".to_string(), "b".to_string());
        assert_eq!(unread(&rows, &parents), 1);
        // ...unless that thread is gone.
        rows.retain(|r| r.thread.id != "b");
        assert_eq!(unread(&rows, &parents), 2);
        // Read: it clears.
        rows.iter_mut().for_each(|r| r.activity = Activity::Idle);
        assert_eq!(unread(&rows, &parents), 0);
    }

    fn banner(tag: &str) -> SystemNotification {
        SystemNotification {
            tag: tag.to_string().into(),
            title: "t".into(),
            body: "b".into(),
            actions: vec![],
        }
    }

    #[test]
    fn banners_wait_for_the_answer_and_go_if_refused() {
        let tags = |v: Vec<SystemNotification>| {
            v.into_iter().map(|n| n.tag.to_string()).collect::<Vec<_>>()
        };
        // Before the Mac has said: held; a newer one for the same thread replaces the older.
        let mut n = Notifier {
            permission: Permission::Unknown,
            ..Notifier::default()
        };
        assert!(
            n.offer(banner("a")).is_none()
                && n.offer(banner("a")).is_none()
                && n.offer(banner("b")).is_none()
        );
        assert!(!n.asked);
        // It was never asked: ask now, still holding them.
        assert!(n.allows(Permission::NotAsked).is_empty());
        assert!(n.asked);
        // Allowed: they show, and later ones at once.
        assert_eq!(tags(n.allows(Permission::Allowed)), ["a", "b"]);
        assert!(n.offer(banner("c")).is_some());
        // Never asked when the first comes: asked then; refused, they go, and so do later ones.
        let mut n = Notifier {
            permission: Permission::NotAsked,
            ..Notifier::default()
        };
        assert!(n.offer(banner("a")).is_none());
        assert!(n.asked);
        assert!(n.allows(Permission::Denied).is_empty());
        assert!(n.held.is_empty() && n.offer(banner("b")).is_none());
        // One read while it waited isn't shown when the answer comes.
        let mut n = Notifier {
            permission: Permission::Unknown,
            ..Notifier::default()
        };
        n.offer(banner("a"));
        n.offer(banner("b"));
        assert!(!n.read("a"));
        assert_eq!(tags(n.allows(Permission::Allowed)), ["b"]);
        assert!(n.read("b"));
        // Only a few wait.
        let mut n = Notifier {
            permission: Permission::Unknown,
            ..Notifier::default()
        };
        for i in 0..20 {
            n.offer(banner(&i.to_string()));
        }
        assert_eq!(n.held.len(), HOLD);
        assert_eq!(n.held[0].tag.as_ref(), "12");
        // Off a bundle (a Mac's `cargo run`): never.
        let mut n = Notifier {
            permission: Permission::Unavailable,
            ..Notifier::default()
        };
        assert!(n.offer(banner("a")).is_none() && n.held.is_empty());
    }
}
