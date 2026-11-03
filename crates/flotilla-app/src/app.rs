//! The root view: what's selected, and the views the engine side sends. It reads and writes
//! nothing itself: requests go out through the `Backend`, and views come back.

use crate::add_project::{AddProjectEvent, AddProjectView};
use crate::backend::Backend;
use crate::device::{Device, SavedServer};
use crate::drafts::Drafts;
use crate::new_thread::{NewThreadEvent, NewThreadPage};
use crate::project_view::{ProjectEvent, ProjectView};
use crate::schedule_editor::{EditorEvent, ScheduleEditor, Seed};
use crate::schedules_view::{SchedulesEvent, SchedulesView};
use crate::settings::{Appearance, SettingsEvent, SettingsInit, SettingsView, SummaryState};
use crate::thread_menu::ThreadCommand;
use crate::thread_view::{ThreadView, ThreadViewEvent};
use crate::{
    ArchiveAndNext, CloseWindow, ConnectServer, NewThread, NextThread, OpenSettings,
    PreviousThread, SyncNow, UndoArchive,
};
use flotilla_proto::{
    Activity, Attached, Catalog, ClientMsg, EngineState, Environment, GhStatus, Note, Project,
    Reply, ReqId, Request, Secret, Sent, ServerMsg, Setting, SyncStatus, ThreadDetail,
    ThreadSettings, ThreadState, ThreadSummary, View, ViewKey,
};
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, Sizable as _, Theme, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// A thread as an action still out with boat is expected to leave it. Shown over the database
/// until boat answers; then dropped, so success shows the database's new value and failure
/// the old one (with an error). Never written to the database, so a sync can't flip it back
/// meanwhile.
#[derive(Default)]
struct Expected {
    title: Option<String>,
    keep_running: Option<bool>,
    state: Option<ThreadState>,
    deleting: bool,
}

impl Expected {
    fn apply(&self, t: &mut flotilla_proto::Thread) {
        if let Some(title) = &self.title {
            t.title = title.clone();
        }
        if let Some(on) = self.keep_running {
            t.keep_running = on;
        }
        if let Some(state) = self.state {
            t.state = state;
        }
    }

    fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.keep_running.is_none()
            && self.state.is_none()
            && !self.deleting
    }

    /// Its row as it's expected to be. A VM asked to stop runs no turn: it isn't at work from
    /// the moment you ask, though the VM takes seconds to stop (a start's may be made first).
    fn apply_row(&self, s: &mut ThreadSummary) {
        self.apply(&mut s.thread);
        if self.state == Some(ThreadState::Stopping) && s.activity == Activity::Working {
            s.activity = Activity::Idle;
            s.turn_started_ms = None;
        }
    }
}

/// A message whose files are going up: sent with them once they're up.
struct Uploading {
    thread: String,
    text: String,
    files: Vec<Attached>,
}

/// Tags the "Thread finished" toasts, one per thread.
struct FinishedNote;
/// A project's setup toasts share one id per project, so a recovery replaces its failure.
struct SetupNote;

/// How long what was on screen stays while a thread just opened waits for its view.
const HOLD_FOR: Duration = Duration::from_millis(300);

/// How long an archive can be undone (T3 Code's timing).
const UNDO_FOR: std::time::Duration = std::time::Duration::from_secs(5);
/// A server that hasn't answered for this long since the window started using it: the window
/// says so, with what to do, instead of waiting on "Loading…".
const UNREACHED_FOR: Duration = Duration::from_secs(4);

/// The sidebar's "Archived N threads · Undo" line.
pub struct UndoNotice {
    pub ids: Vec<String>,
    at: std::time::Instant,
    /// ⌘Z undoes the archive until another key is pressed (archives you did, not ones the
    /// server did by itself while you may have been typing).
    pub owns_cmd_z: bool,
    /// The thread that was open when you archived it: Undo opens it again.
    open: Option<String>,
}

/// This Mac letting other devices connect to its own server.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum DevicesIn {
    #[default]
    Off,
    Opening,
    Open(flotilla_server::Opened),
    Failed(String),
}

/// The sidebar filter's key for threads with no project.
pub const NO_PROJECT: &str = "none";
pub const SIDEBAR_DEFAULT: f32 = 280.;
pub const SIDEBAR_MIN: f32 = 232.;
pub const SIDEBAR_MAX: f32 = 420.;
/// What the thread keeps however wide the sidebar is dragged.
const MAIN_MIN: f32 = 480.;

/// The open thread and what's held for it: all of it goes when it closes (`close_thread`) or
/// another opens.
struct Open {
    id: String,
    view: Entity<ThreadView>,
    /// Its view as the engine side last sent it; None until that comes.
    detail: Option<ThreadDetail>,
    /// Its older messages fetched (or kept) beside that view, once it has come.
    history: flotilla_proto::History,
    /// Pages of older messages being fetched: a jump far back fetches a few at a time
    /// (`FETCHES`), not dozens at once.
    fetching: usize,
    /// A message to open it at (a search hit, or where you'd scrolled to), until its view has
    /// come.
    jump: Option<i64>,
    /// Marked unread while open: opening it again (clicking it) reads it.
    keep_unread: bool,
    /// Opened a moment ago and its view hasn't come (since when), and the thread that was
    /// showing: that stays on screen meanwhile (up to `HOLD_FOR`), so opening one never
    /// flashes an empty page.
    awaiting: Option<std::time::Instant>,
    previous: Option<Entity<ThreadView>>,
}

/// Where Send later's form came from: what to empty once the server has the schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Later {
    Thread(String),
    NewThread,
    Edit,
}

/// Send later's open form: where it came from, the schedules it's waiting for the server to
/// keep (one per model, as submitted: an answer to an earlier save of the same schedule isn't
/// this one's), and the first refusal among them.
struct LaterForm {
    view: WeakEntity<ScheduleEditor>,
    origin: Later,
    waiting: HashMap<String, flotilla_proto::ScheduleSpec>,
    refused: Option<String>,
}

pub struct FlotillaApp {
    /// Where requests and subscriptions go: This Mac's own server, or one it's paired with.
    backend: Backend,
    /// This Mac's database (its own engine's), beside which `device.json` is kept.
    db: PathBuf,
    /// The server this Mac uses, if not its own engine, and what its things are kept under in
    /// `device.json` and `drafts`.
    server: Option<SavedServer>,
    server_key: String,
    /// This Mac letting other devices in: under way, or where they reach it (or why not).
    devices_in: DevicesIn,
    /// Letting them in (or no longer) is under way, and what was done last (over Tailscale or
    /// not; None: closed). One at a time, until it's what `device.json` wants.
    access_busy: bool,
    access_done: Option<bool>,
    /// This backend's life: what an earlier one finishes is dropped.
    life: u64,
    /// Where each thread you left was scrolled to (its top message), this session: it opens
    /// there again. None kept for one left at its end.
    places: HashMap<String, i64>,
    /// The ⌘K palette, while it's open, and what it says.
    pub palette: Option<WeakEntity<crate::palette::PaletteView>>,
    palette_sub: Option<Subscription>,
    /// The palette's query for the thread it's opening, for the server to remember the pick
    /// (`thread.opened`).
    open_query: Option<String>,
    /// The views were let go of when the server refused this Mac: subscribe again if it
    /// takes it after all.
    forgot: bool,
    /// Asking the server on this machine which it is, to use it (the refusal's Connect to it).
    joining: bool,
    /// That server's paired devices, as it last sent them.
    devices: Option<flotilla_proto::Devices>,
    /// Every scheduled prompt, as the server last sent them.
    pub(crate) schedules: Vec<flotilla_proto::Schedule>,
    /// The open Scheduled dialog, and the open Send later form (and what it came from).
    schedules_view: Option<WeakEntity<SchedulesView>>,
    schedule_editor: Option<LaterForm>,
    /// The open "Connect to a server" dialog.
    connect_view: Option<WeakEntity<crate::pairing::ConnectView>>,
    /// Whether the backend reaches the host (None until it first says).
    pub connection: Option<flotilla_proto::Connection>,
    /// Since when it's been trying to reach the server without getting through (from the
    /// start, until it first does); Retry was pressed and no try has ended since.
    unreached_since: Option<std::time::Instant>,
    retrying: bool,
    /// The setup page's Check again (or key) is waiting on checks newer than the ones shown
    /// then (their time).
    setup_checking: Option<i64>,
    /// This device's own settings and layout, and the file they're kept in.
    device: Device,
    device_path: PathBuf,
    next_req: ReqId,
    /// Requests sent and not yet answered, for when their answers come.
    pending: HashMap<ReqId, Request>,
    /// The latest requests sent, answered or not, for the snapshot harness's `asked:`.
    asked: std::collections::VecDeque<Request>,
    /// Sends whose files are going up, by their `local-` id: shown as sending, asked for once
    /// the files are up.
    uploading: HashMap<String, Uploading>,
    /// Rename dialogs waiting for a suggested name.
    naming: HashMap<ReqId, WeakEntity<crate::rename::RenameView>>,
    /// The sidebar view's rows as sent, before what's expected is shown over them.
    rows: Vec<ThreadSummary>,
    /// Every thread's row, with what's expected applied, newest activity first.
    pub summaries: Vec<ThreadSummary>,
    /// Threads handed their task by another: child id → parent id.
    pub parents: HashMap<String, String>,
    /// Rows held in the group they were in, so none moves out from under you: a thread opened
    /// unread (from the sidebar, or by stepping through it with the keyboard) stays under
    /// Unread, and the open thread whose turn ends under the pointer stays under Working.
    /// Let go when the pointer leaves the sidebar, or a thread is opened another way.
    pub held: HashMap<String, Activity>,
    /// The pointer is over the sidebar.
    pub sidebar_hovered: bool,
    /// A thread was just opened: the sidebar scrolls its row into view (once).
    pub reveal: bool,
    /// The thread Undo reopened while it was still archived: revealed once it isn't.
    reveal_after_undo: Option<String>,
    /// The sidebar's lists: the top one (Unread, Idle) and the bottom one (Working, Archived).
    pub sidebar_scroll: [ScrollHandle; 2],
    /// Threads waiting on an answer from a thread they handed work to.
    pub waiting: HashSet<String>,
    /// What was being written and not sent, in threads not open now and on New thread, kept
    /// per server.
    drafts: Drafts,
    /// The open thread, and what's held for it. None: New thread shows.
    open: Option<Open>,
    /// The new-thread page; kept while another thread is open, so a draft survives.
    new_thread: Option<Entity<NewThreadPage>>,
    /// A thread asked for from the new-thread page that the sidebar doesn't list yet: it's open
    /// at once, shown from what was asked, until the engine side's views take over. The page
    /// keeps its draft until then, in case the start is refused.
    starting: Option<flotilla_proto::NewThread>,
    /// Threads started from the page without opening them (⌘Enter, or the other models of
    /// several): shown in the sidebar from what was asked until it lists them. A refusal
    /// says so, and puts the prompt back on the page.
    background: Vec<flotilla_proto::NewThread>,
    /// Messages sent (by their `local-` ids) that the open thread's view has shown: until then
    /// they're shown from what was sent.
    shown_sends: HashSet<String>,
    pub catalog: Option<Catalog>,
    pub status: SyncStatus,
    /// The account's VMs and starts against its plan's limits, beside Settings.
    pub usage: Option<flotilla_proto::Usage>,
    /// The usage popover: open, a Refresh under way, and why the last one failed.
    pub usage_open: bool,
    pub usage_refreshing: bool,
    pub usage_error: Option<String>,
    /// The agent accounts' limits (Claude's, ChatGPT's), beside the usage; their popover.
    pub limits: Vec<flotilla_proto::AccountLimits>,
    pub limits_open: bool,
    /// The usage popovers' tab: boat's VMs, or what the agents spent and their limits.
    pub usage_tab: crate::meter::UsageTab,
    /// What the agents spent, as last sent: the view is subscribed to while a usage popover is
    /// open (which checks the VMs, as T3 checks when Usage opens), and kept after.
    pub agent_usage: Option<flotilla_proto::AgentUsage>,
    agent_usage_sub: bool,
    /// The model picked for its trend (harness, model).
    pub agent_model: Option<(String, String)>,
    /// A Refresh of the agents' usage under way, and why the last one failed.
    pub agents_refreshing: bool,
    pub agents_error: Option<String>,
    /// Whether the engine runs (and why not), from the sidebar view; None before the first.
    pub engine: Option<EngineState>,
    /// The boat environments for threads with No project.
    environments: Vec<Environment>,
    /// Your settings, as the engine side last sent them.
    settings: Option<flotilla_proto::Settings>,
    /// Add project was opened from New thread: pick the project there once it's added.
    pick_added: bool,
    /// Collapsed sidebar groups. Archived starts collapsed.
    pub collapsed: HashSet<Activity>,
    /// The sidebar's filter: projects (by id, and `NO_PROJECT`) whose threads are hidden.
    /// Empty shows everything, including projects added later. Kept on this device.
    pub hidden_projects: std::collections::BTreeSet<String>,
    /// The filter's menu is open.
    pub filter_open: bool,
    /// The sidebar's width (dragged between SIDEBAR_MIN and SIDEBAR_MAX) and whether it's
    /// hidden down to its rail (⌃⌘S). Kept on this device.
    pub sidebar_width: f32,
    pub sidebar_hidden: bool,
    /// A drag of the sidebar's edge: where the pointer and the width started.
    sidebar_drag: Option<(f32, f32)>,
    /// Projects, for the sidebar's pills and rows.
    pub projects_cache: Vec<Project>,
    /// Each thread's sidebar group at the last update, to spot moves.
    groups: HashMap<String, Activity>,
    /// The last archive, while it can still be undone.
    pub undo: Option<UndoNotice>,
    /// What an action that's still out with boat is expected to do to a thread, shown until
    /// boat answers (see `Expected`).
    expected: HashMap<String, Expected>,
    /// The row just right-clicked: its hover card stays away (it would cover the menu) until
    /// the pointer leaves the row.
    pub card_suppressed: Option<String>,
    /// Threads that just landed in a group (archived, finished, new), for their slide-in.
    pub arrivals: HashMap<String, (u64, std::time::Instant)>,
    arrival_seq: u64,
    summary_state: SummaryState,
    /// What gh can do, where the engine runs.
    pub gh_status: GhStatus,
    /// The open Add project dialog.
    add_project: Option<WeakEntity<AddProjectView>>,
    /// The open project settings dialog, and the project whose page is subscribed to (the
    /// dialog opens when its view first arrives).
    project_view: Option<WeakEntity<ProjectView>>,
    project_sub: Option<String>,
    appearance: Appearance,
    /// The open Settings view, to keep its summary status live.
    settings_view: Option<WeakEntity<SettingsView>>,
    key_input: Entity<InputState>,
    rebuilding: bool,
    focus: FocusHandle,
    /// Dev: where a scripted demo's pointer is, and when it last clicked, drawn over
    /// everything so a recording shows it.
    #[cfg(feature = "snapshot")]
    pub demo_pointer: Option<(Point<Pixels>, Option<std::time::Instant>)>,
    /// Dev: rows the harness shows in another state (`step:sim:`), over what the server sends.
    #[cfg(feature = "snapshot")]
    sim: HashMap<String, (Activity, Option<i64>, Option<String>)>,
    /// Dev: views that came for a thread or project not open here (`expect:strays:`).
    #[cfg(feature = "snapshot")]
    strays: usize,
    /// Dev: the toasts there were at `step:toasts-mark`.
    #[cfg(feature = "snapshot")]
    toasts_mark: HashSet<EntityId>,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl FlotillaApp {
    pub fn new(db: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = Self::build(db, Drafts::default(), window, cx);
        #[cfg(feature = "snapshot")]
        if let Some(dir) = std::env::var_os("FLOTILLA_SNAPSHOT_DIR") {
            crate::snapshot::start(dir.into(), window, cx);
        }
        app
    }

    /// The app on this Mac's own engine, or on the server `device.json` says to use, with what
    /// was being written for each server.
    fn build(db: PathBuf, mut drafts: Drafts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let device_path = Device::path_for(&db);
        let device = Device::load(&device_path);
        let appearance = Appearance::parse(&device.appearance);
        apply_appearance(appearance, window, cx);
        let (to_ui, from_host) = smol::channel::unbounded::<ServerMsg>();
        // Dev: FLOTILLA_SERVER (and FLOTILLA_TOKEN, a device's token) overrides it.
        let dev = std::env::var("FLOTILLA_SERVER")
            .ok()
            .filter(|t| !t.trim().is_empty());
        let server = match &dev {
            Some(target) => Some(SavedServer {
                id: String::new(),
                url: target.clone(),
                device: String::new(),
            }),
            None => device.current_server().cloned(),
        };
        // This Mac's last server is still finishing its work: nothing yet (see `await_retired`).
        let waiting = dev.is_none() && server.is_none() && crate::backend::retiring();
        let backend = match (&dev, &server) {
            (Some(target), _) => Backend::server(
                target,
                std::env::var("FLOTILLA_TOKEN").ok().map(Secret),
                to_ui,
            ),
            (None, Some(s)) => {
                let token = match flotilla_client::tokens::device_token(&s.id) {
                    Ok(t) => t.map(Secret),
                    Err(e) => {
                        eprintln!(
                            "[flotilla] couldn't read this device's token for {}: {e}",
                            s.name()
                        );
                        None
                    }
                };
                Backend::server(&s.url, token, to_ui)
            }
            (None, None) if waiting => Backend::detached(),
            (None, None) => {
                Backend::this_mac(&db, std::env::var_os("FLOTILLA_OFFLINE").is_some(), to_ui)
            }
        };
        let server_key = Device::key(server.as_ref());
        drafts.switch(&server_key);
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("boat_…")
                .masked(true)
        });
        let key_sub = cx.subscribe_in(&key_input, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.save_key(window, cx);
            }
        });
        // Any keystroke but ⌘Z (Ctrl+Z off a Mac) ends its claim on undoing an archive. An
        // interceptor sees keys before their actions run, which key-down listeners don't when
        // an action takes the key.
        let app = cx.weak_entity();
        let keys_sub = cx.intercept_keystrokes(move |event, _, cx| {
            let k = &event.keystroke;
            let held = if cfg!(target_os = "macos") {
                k.modifiers.platform
            } else {
                k.modifiers.control
            };
            if !(held && k.key == "z") {
                let _ = app.update(cx, |app, _| {
                    if let Some(u) = &mut app.undo {
                        u.owns_cmd_z = false;
                    }
                });
            }
        });
        let appearance_sub = cx.observe_window_appearance(window, |this, window, cx| {
            if this.appearance == Appearance::System {
                Theme::sync_system_appearance(Some(window), cx);
            }
        });
        // What the computer allows, as it's told or asked: Settings says when it's refused.
        let permission_sub = cx.observe_global::<crate::notify::Notifier>(|this, cx| {
            if let Some(view) = this.settings_view.as_ref().and_then(|v| v.upgrade()) {
                let p = crate::notify::permission(cx);
                view.update(cx, |v, cx| v.set_notification_permission(p, cx));
            }
        });
        // Back from elsewhere: the open thread's banner is read now, and what the Mac allows
        // may have changed (in System Settings, say).
        let activation_sub = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                if let Some(id) = this.selected() {
                    crate::notify::read(cx, &this.server_key, id);
                }
                crate::notify::recheck(cx);
            }
        });
        let messages = cx.spawn_in(window, async move |this, cx| {
            while let Ok(msg) = from_host.recv().await {
                if this
                    .update_in(cx, |app, window, cx| app.on_msg(msg, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        // Re-render once a second while anything is working, for the elapsed-time column, and
        // every 30 s for the open thread's time left before its idle stop, and for the usage
        // popover's read time; and once an agent limit's window has just reset, so it goes.
        let ticker = cx.spawn(async move |this, cx| {
            let mut ticks: u32 = 0;
            // When the last tick ran: a reset since then is redrawn, however late this one is.
            let mut last = flotilla_model::util::now_ms();
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                ticks = ticks.wrapping_add(1);
                let (since, now) = (last, flotilla_model::util::now_ms());
                last = now;
                let alive = this.update(cx, |app, cx| {
                    let working = app.summaries.iter().any(|s| s.turn_started_ms.is_some());
                    let counting = ticks.is_multiple_of(30)
                        && (app.usage_open
                            || app.limits_open
                            || app.summaries.iter().any(|s| {
                                app.selected() == Some(s.thread.id.as_str())
                                    && s.thread.archive_after.is_some()
                                    && !s.thread.keep_running
                            }));
                    let reset = flotilla_model::limits::reset_between(&app.limits, since, now);
                    if working || counting || reset {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        // Quitting stops This Mac's server here, not with the process: Tailscale stops
        // forwarding to it and its socket goes.
        let quit_sub = cx.on_app_quit(|this, _| {
            this.backend = Backend::detached();
            crate::backend::wait_retired();
            async {}
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let collapsed = match &device.collapsed {
            Some(groups) => groups.iter().filter_map(|g| group_of(g)).collect(),
            None => HashSet::from([Activity::Archived]),
        };
        let hidden_projects = device.for_server(&server_key).hidden_projects;
        let mut app = FlotillaApp {
            backend,
            db,
            server,
            server_key,
            devices_in: DevicesIn::Off,
            access_busy: false,
            access_done: None,
            life: {
                static LIVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                LIVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            places: HashMap::new(),
            palette: None,
            palette_sub: None,
            open_query: None,
            forgot: false,
            joining: false,
            devices: None,
            schedules: Vec::new(),
            schedules_view: None,
            schedule_editor: None,
            connect_view: None,
            connection: None,
            unreached_since: None,
            retrying: false,
            setup_checking: None,
            sidebar_width: device
                .sidebar_width
                .unwrap_or(SIDEBAR_DEFAULT)
                .clamp(SIDEBAR_MIN, SIDEBAR_MAX),
            sidebar_hidden: device.sidebar_hidden,
            hidden_projects,
            device,
            device_path,
            next_req: 1,
            pending: HashMap::new(),
            asked: Default::default(),
            uploading: HashMap::new(),
            naming: HashMap::new(),
            rows: vec![],
            summaries: vec![],
            parents: HashMap::new(),
            held: HashMap::new(),
            sidebar_hovered: false,
            reveal: false,
            reveal_after_undo: None,
            sidebar_scroll: Default::default(),
            waiting: HashSet::new(),
            drafts,
            open: None,
            new_thread: None,
            starting: None,
            background: Vec::new(),
            shown_sends: HashSet::new(),
            catalog: None,
            status: SyncStatus::default(),
            usage: None,
            usage_open: false,
            usage_refreshing: false,
            usage_error: None,
            limits: Vec::new(),
            limits_open: false,
            usage_tab: crate::meter::UsageTab::Vms,
            agent_usage: None,
            agent_usage_sub: false,
            agent_model: None,
            agents_refreshing: false,
            agents_error: None,
            engine: None,
            environments: vec![],
            settings: None,
            pick_added: false,
            collapsed,
            filter_open: false,
            sidebar_drag: None,
            projects_cache: Vec::new(),
            groups: HashMap::new(),
            undo: None,
            expected: HashMap::new(),
            card_suppressed: None,
            arrivals: HashMap::new(),
            arrival_seq: 0,
            summary_state: SummaryState::Unknown,
            gh_status: GhStatus::Unknown,
            add_project: None,
            project_view: None,
            project_sub: None,
            appearance,
            settings_view: None,
            key_input,
            rebuilding: false,
            focus,
            #[cfg(feature = "snapshot")]
            demo_pointer: None,
            #[cfg(feature = "snapshot")]
            sim: HashMap::new(),
            #[cfg(feature = "snapshot")]
            strays: 0,
            #[cfg(feature = "snapshot")]
            toasts_mark: HashSet::new(),
            _subscriptions: vec![
                key_sub,
                appearance_sub,
                activation_sub,
                permission_sub,
                keys_sub,
                quit_sub,
            ],
            _tasks: vec![messages, ticker],
        };
        app.subscribe_all();
        app.sync_devices(cx);
        if waiting {
            app.await_retired(window, cx);
        }
        // A server that doesn't answer for a while says so (see `unreached`).
        if app.server.is_some() {
            app.unreached_since = Some(std::time::Instant::now());
            let life = app.life;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(UNREACHED_FOR).await;
                let _ = this.update(cx, |app, cx| {
                    if app.life == life {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        app
    }

    /// The server hasn't been reached since this window started using it, for `UNREACHED_FOR`
    /// or more: why (the last try's error; empty while none has ended). It's still tried
    /// meanwhile, and everything comes as soon as it answers.
    /// A check threads can't run without failed: the setup shows where New thread would.
    fn setup_stops_threads(&self) -> bool {
        self.settings
            .as_ref()
            .and_then(|s| s.doctor.as_ref())
            .is_some_and(|d| d.stops_threads())
    }

    pub fn unreached(&self) -> Option<String> {
        let since = self.unreached_since?;
        if self.server.is_none() || self.engine.is_some() || since.elapsed() < UNREACHED_FOR {
            return None;
        }
        Some(match &self.connection {
            Some(flotilla_proto::Connection::Offline { error }) => error.clone(),
            _ => String::new(),
        })
    }

    /// Unreached: try again now rather than after the wait.
    fn retry_server(&mut self, cx: &mut Context<Self>) {
        self.retrying = true;
        self.backend.retry();
        cx.notify();
    }

    /// This Mac's last server is still finishing its engine's work (it holds the database until
    /// then): start the next once it's done, waiting off the main thread. The window shows
    /// nothing of either meanwhile.
    fn await_retired(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let life = self.life;
        let wait = cx.background_spawn(async { crate::backend::wait_retired() });
        cx.spawn_in(window, async move |this, cx| {
            wait.await;
            let _ = this.update_in(cx, |app, window, cx| {
                if app.life == life {
                    app.switch(window, cx);
                }
            });
        })
        .detach();
    }

    /// Let other devices connect to This Mac's server, or no longer, as `device.json` says. Off
    /// the main thread (Tailscale can take a while), one step at a time: a step finished after
    /// the wish changed is followed by another.
    fn sync_devices(&mut self, cx: &mut Context<Self>) {
        let Some(access) = self.backend.access() else {
            return;
        };
        let want = self
            .device
            .let_devices_in
            .then_some(self.device.over_tailscale);
        if self.access_busy
            || (want == self.access_done && !matches!(self.devices_in, DevicesIn::Failed(_)))
        {
            return;
        }
        self.access_busy = true;
        if want.is_some() {
            self.devices_in = DevicesIn::Opening;
            self.show_devices_in(cx);
        }
        let life = self.life;
        let task = cx.background_spawn(async move {
            match want {
                Some(tailscale) => Some(access.open(crate::backend::devices_port(), tailscale)),
                None => {
                    access.close();
                    None
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let done = task.await;
            let _ = this.update(cx, |app, cx| {
                if app.life != life {
                    return;
                }
                app.access_busy = false;
                app.access_done = want;
                app.devices_in = match done {
                    None => DevicesIn::Off,
                    Some(Ok(opened)) => DevicesIn::Open(opened),
                    Some(Err(e)) => DevicesIn::Failed(e),
                };
                // Turned off or on again meanwhile.
                let still = app
                    .device
                    .let_devices_in
                    .then_some(app.device.over_tailscale);
                if still != want {
                    app.sync_devices(cx);
                }
                app.show_devices_in(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Settings → Devices on This Mac, as it is now.
    fn show_devices_in(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
            let state = (
                self.device.let_devices_in,
                self.device.over_tailscale,
                self.devices_in.clone(),
            );
            view.update(cx, |v, cx| v.set_devices_in(state, cx));
        }
    }

    /// Settings' switches on This Mac: let other devices in, and over Tailscale.
    fn let_devices_in(&mut self, on: bool, tailscale: bool, cx: &mut Context<Self>) {
        let was = self.device.let_devices_in;
        self.device.let_devices_in = on;
        self.device.over_tailscale = tailscale;
        self.save_device();
        if on && !was {
            self.backend.send(ClientMsg::Sub {
                view: ViewKey::Devices,
            });
        } else if !on && was {
            self.backend.send(ClientMsg::Unsub {
                view: ViewKey::Devices,
            });
            self.devices = None;
        }
        if !on {
            self.devices_in = DevicesIn::Off;
        }
        // A failure is tried again when it's asked for again.
        if matches!(self.devices_in, DevicesIn::Failed(_)) {
            self.access_done = None;
        }
        self.sync_devices(cx);
        self.show_devices_in(cx);
        cx.notify();
    }

    /// The views every screen needs; Devices on a server, or while This Mac lets them in; the
    /// agents' usage while a usage popover is open.
    fn subscribe_all(&self) {
        for view in [
            ViewKey::Sidebar,
            ViewKey::Catalog,
            ViewKey::Settings,
            ViewKey::Schedules,
        ] {
            self.backend.send(ClientMsg::Sub { view });
        }
        if self.server.is_some() || self.device.let_devices_in {
            self.backend.send(ClientMsg::Sub {
                view: ViewKey::Devices,
            });
        }
        if self.agent_usage_sub {
            self.backend.send(ClientMsg::Sub {
                view: ViewKey::AgentUsage,
            });
        }
    }

    /// Subscribe to what the agents spent while a usage popover is open, and only then:
    /// subscribing checks the VMs that are up (the server waits five minutes between those).
    pub(crate) fn sync_agent_usage(&mut self) {
        let want = self.usage_open || self.limits_open;
        if want != self.agent_usage_sub {
            self.agent_usage_sub = want;
            let view = ViewKey::AgentUsage;
            self.backend.send(if want {
                ClientMsg::Sub { view }
            } else {
                ClientMsg::Unsub { view }
            });
        }
    }

    /// The Agents tab's Refresh: check every VM that's up, the limits and the prices now.
    pub(crate) fn refresh_agents(&mut self, cx: &mut Context<Self>) {
        self.agents_refreshing = true;
        self.agents_error = None;
        self.ask(Request::ReadAgentUsage);
        cx.notify();
    }

    /// Start again on the server `device.json` now says (or this Mac's own engine), keeping
    /// what was being written. The old backend goes first: This Mac's server finishes its
    /// engine's work under way and lets go of its database (off this thread) before This Mac's
    /// can start again, and nothing the old one sends reaches the new.
    fn switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.switch_with(|_| {}, window, cx);
    }

    /// [`Self::switch`], changing which server is paired or in use first: once what's kept for
    /// the one being left is written, so none of it is written back for a server forgotten.
    fn switch_with(
        &mut self,
        change: impl FnOnce(&mut Device),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.close_all_dialogs(cx);
        self.keep_drafts(cx);
        self.save_device();
        change(&mut self.device);
        self.device.save(&self.device_path);
        self.keep_drafts_kept();
        std::mem::replace(&mut self.backend, Backend::detached()).retire();
        let drafts = std::mem::take(&mut self.drafts);
        *self = Self::build(self.db.clone(), drafts, window, cx);
        cx.notify();
    }

    /// Change which server is paired or in use, and start again on it (deferred: the whole
    /// app is made again).
    fn change_server(
        &mut self,
        change: impl FnOnce(&mut Device) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.defer_in(window, move |this, window, cx| {
            this.switch_with(change, window, cx)
        });
    }

    /// Put what the open composers have in with the drafts: the open thread's, and New
    /// thread's unless it's being started (then it's been sent).
    fn keep_drafts(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.thread_view().cloned() {
            self.keep_draft(&view, cx);
        }
        // A start whose files are still going up hasn't been asked for: it goes back to New
        // thread, as written.
        let asked = |id: &Option<String>| {
            self.pending
                .values()
                .any(|p| matches!(p, Request::NewThread(n) if &n.id == id))
        };
        let interrupted = self
            .starting
            .as_ref()
            .filter(|r| !asked(&r.id))
            .map(|r| crate::drafts::Draft::of(r.prompt.clone(), r.attachments.clone()));
        // So is one started in the background (⌘Enter), if nothing new is written.
        let behind = self
            .background
            .iter()
            .find(|r| !asked(&r.id))
            .and_then(|r| crate::drafts::Draft::of(r.prompt.clone(), r.attachments.clone()));
        if let Some(page) = &self.new_thread {
            let draft = interrupted
                .unwrap_or_else(|| page.read(cx).draft(cx))
                .or(behind);
            self.drafts.keep_new_thread(draft);
        }
        // So are messages whose files are still going up: back in their composers, as a failed
        // send's are.
        for (_, up) in std::mem::take(&mut self.uploading) {
            self.device
                .for_server_mut(&self.server_key)
                .keep_unsent(&up.thread, &up.text, &up.files);
        }
        self.save_device();
    }

    /// The server won't have this Mac (revoked, unpaired, another Flotilla on the database):
    /// nothing of what it showed stays, and what was being written is kept for when it's back.
    fn forget_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.close_all_dialogs(cx);
        self.keep_drafts(cx);
        self.close_thread(cx);
        self.close_project();
        self.new_thread = None;
        self.starting = None;
        self.background.clear();
        self.shown_sends.clear();
        self.rows.clear();
        self.summaries.clear();
        self.parents.clear();
        self.held.clear();
        self.waiting.clear();
        self.groups.clear();
        self.arrivals.clear();
        self.expected.clear();
        self.undo = None;
        self.catalog = None;
        self.status = SyncStatus::default();
        self.usage = None;
        self.usage_open = false;
        self.usage_refreshing = false;
        self.usage_error = None;
        self.limits.clear();
        self.limits_open = false;
        self.agent_usage = None;
        self.agent_usage_sub = false;
        self.agent_model = None;
        self.agents_refreshing = false;
        self.agents_error = None;
        self.engine = None;
        self.environments.clear();
        self.settings = None;
        self.devices = None;
        self.projects_cache.clear();
        self.gh_status = GhStatus::Unknown;
        self.summary_state = SummaryState::Unknown;
        self.forgot = true;
        cx.notify();
    }

    /// Back to this Mac's own engine.
    pub(crate) fn use_this_mac(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.change_server(|d| d.on_server = false, window, cx);
    }

    /// Use the server this Mac is paired with.
    fn use_paired(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.device.paired.is_some() {
            self.change_server(|d| d.on_server = true, window, cx);
        }
    }

    /// Forget the server this Mac is paired with (its token, what was kept for it, and its
    /// pairing there); if it was in use, back to This Mac.
    fn forget_paired(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let forget = |d: &mut Device| {
            if let Some(old) = d.unpair() {
                crate::backend::forget(old);
            }
        };
        if self.device.on_server {
            self.change_server(forget, window, cx);
        } else {
            forget(&mut self.device);
            self.save_device();
            self.keep_drafts_kept();
        }
    }

    /// Drafts only for This Mac and the paired server (and a dev `FLOTILLA_SERVER`'s while it's
    /// shown): a server forgotten takes its own with it.
    fn keep_drafts_kept(&mut self) {
        let mut keep = vec![crate::device::THIS_MAC.to_string()];
        keep.extend(self.device.paired.as_ref().map(|p| Device::key(Some(p))));
        keep.extend(
            self.server
                .as_ref()
                .filter(|s| s.id.is_empty())
                .map(|s| Device::key(Some(s))),
        );
        self.drafts.keep_only(&keep);
    }

    /// "Connect to a server…": a link (one just opened, say) or its address and a code.
    pub(crate) fn open_connect(
        &mut self,
        prefill: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.close_all_dialogs(cx);
        let view = cx.new(|cx| crate::pairing::ConnectView::new(prefill, window, cx));
        self.connect_view = Some(view.downgrade());
        let sub = cx.subscribe_in(
            &view,
            window,
            |_, _, crate::pairing::Paired(saved), window, cx| {
                let (saved, name) = (saved.clone(), saved.name().to_string());
                cx.defer_in(window, move |this, window, cx| {
                    this.switch_with(|d| pair_in_place(d, saved), window, cx);
                    window.push_notification(
                        Notification::success(format!("Connected to {name}")),
                        cx,
                    );
                });
            },
        );
        self._subscriptions.push(sub);
        let (body, first, enter) = (
            view.clone(),
            view.read(cx).first_field(cx),
            view.downgrade(),
        );
        crate::form::open(
            window,
            cx,
            Some(first),
            move |window, cx| {
                let _ = enter.update(cx, |v, cx| v.submit(window, cx));
            },
            move |dialog, _, _| {
                dialog
                    .title("Connect to a server")
                    .width(px(480.))
                    .child(body.clone())
            },
        );
    }

    /// A `flotilla://pair…` link opened on this Mac: pair once it's confirmed.
    pub(crate) fn open_link(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if flotilla_proto::pair::read(&url).is_ok() {
            self.open_connect(Some(url), window, cx);
        }
    }

    /// The usage popover's Refresh: read boat's limits again now.
    pub(crate) fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        self.usage_refreshing = true;
        self.usage_error = None;
        self.ask(Request::ReadUsage);
        cx.notify();
    }

    /// Send a request; its answer comes back to `on_answer`.
    pub(crate) fn ask(&mut self, req: Request) -> ReqId {
        let id = self.next_req;
        self.next_req += 1;
        self.pending.insert(id, req.clone());
        if self.asked.len() == 32 {
            self.asked.pop_front();
        }
        self.asked.push_back(req.clone());
        self.backend.send(ClientMsg::Req { id, req });
        id
    }

    /// Write this device's file.
    fn save_device(&mut self) {
        self.device.for_server_mut(&self.server_key).hidden_projects = self.hidden_projects.clone();
        self.device.sidebar_hidden = self.sidebar_hidden;
        self.device.appearance = self.appearance.as_str().to_string();
        let mut groups: Vec<String> = self
            .collapsed
            .iter()
            .map(|g| group_name(*g).to_string())
            .collect();
        groups.sort();
        self.device.collapsed = Some(groups);
        self.device.save(&self.device_path);
    }

    /// Open the first thread whose title contains `needle`.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_open(
        &mut self,
        needle: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self
            .summaries
            .iter()
            .find(|s| s.thread.title.contains(needle))
            .map(|s| s.thread.id.clone())
        {
            self.select(id, window, cx);
        }
    }

    /// How many rows the open thread's page has built (0 until its view arrives).
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_rows(&self, cx: &App) -> usize {
        // Its own view, not the page shown from what was asked.
        self.open
            .as_ref()
            .filter(|o| o.detail.as_ref().is_some_and(|d| d.thread.is_some()))
            .map_or(0, |o| o.view.read(cx).snapshot_row_count())
    }

    /// The open thread's rows, and the oldest message's seq it shows.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_rows_first(&self, cx: &App) -> (usize, Option<i64>) {
        self.thread_view().map_or((0, None), |v| {
            let v = v.read(cx);
            (v.snapshot_row_count(), v.snapshot_first_seq())
        })
    }

    /// The open thread's composer text and caret.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_composer(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<(String, (u32, u32), bool)> {
        self.thread_view()
            .map(|v| v.read(cx).composer_state(window, cx))
    }

    /// Act as if a turn just finished on the first thread whose title contains `needle`.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_finished(
        &mut self,
        needle: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self
            .summaries
            .iter()
            .find(|s| s.thread.title.contains(needle))
            .map(|s| s.thread.id.clone())
        {
            self.thread_toast(id, "Thread finished", window, cx);
        }
    }

    /// Act as if the server sent an alert that a turn finished on the first thread whose title
    /// contains `needle` (a banner if the window isn't focused: `step:hide` first).
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_alert(
        &mut self,
        needle: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self
            .summaries
            .iter()
            .find(|s| s.thread.title.contains(needle))
            .map(|s| s.thread.clone())
        else {
            return;
        };
        let alert = flotilla_proto::Alert {
            kind: flotilla_proto::AlertKind::TurnFinished,
            title: t.title,
            body: "Finished.".into(),
            thread: Some(t.id.clone()),
            about: t.id,
        };
        let id = format!("snapshot-{}", uuid::Uuid::new_v4());
        crate::notify::alert(
            cx,
            &self.server_key,
            &id,
            &alert,
            self.device.notifications_on(),
            window.is_window_active(),
        );
    }

    /// Act as if an action the engine started itself on the open thread (else the first whose
    /// title contains `needle`) ended, as the server says so: `Note::Done`, with the request
    /// the client would have sent (see `flotilla_engine::host::request_of`). `what`: `steer`
    /// (a message it sent went in as a steer), `send-failed`, `interrupt-failed`, `start-failed`
    /// (a delegate's start, its hand-off prompt with it) or `archived` (on merge).
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_engine_done(
        &mut self,
        what: &str,
        needle: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let thread = match needle {
            Some(n) => self
                .summaries
                .iter()
                .find(|s| s.thread.title.contains(n))
                .map(|s| s.thread.id.clone()),
            None => self.selected().map(String::from),
        };
        let Some(thread) = thread else { return };
        let send = |text: &str| Request::Send {
            thread: thread.clone(),
            text: text.into(),
            files: Vec::new(),
            local: String::new(),
        };
        let (request, result) = match what {
            "steer" => (
                send("[flotilla] The reviewer answered."),
                Ok(Reply::Sent(Sent::Steer("evt-1".into()))),
            ),
            "send-failed" => (
                send("[flotilla] The reviewer answered."),
                Err("boat didn't take the message".to_string()),
            ),
            "interrupt-failed" => (
                Request::Interrupt(thread),
                Err("no turn to stop".to_string()),
            ),
            "start-failed" => {
                let Some(t) = self
                    .summaries
                    .iter()
                    .find(|s| s.thread.id == thread)
                    .map(|s| s.thread.clone())
                else {
                    return;
                };
                let new = flotilla_proto::NewThread {
                    environment: String::new(),
                    project_id: None,
                    provider: t.provider,
                    model: t.model,
                    effort: None,
                    fast: false,
                    title: Some(t.title),
                    machine_type: None,
                    prompt: "Check the kernel version and report back.".into(),
                    attachments: Vec::new(),
                    wait_ready: false,
                    on_branch: None,
                    id: Some(thread),
                    parent: None,
                    branch_tag: None,
                };
                (
                    Request::NewThread(Box::new(new)),
                    Err("boat refused the prompt".to_string()),
                )
            }
            "archived" => (Request::Archive(thread), Ok(Reply::Done)),
            _ => return,
        };
        self.on_note(Note::Done { request, result }, window, cx);
    }

    /// The rename dialog for the first thread in the list.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<crate::rename::RenameView>> {
        let id = self.summaries.first()?.thread.id.clone();
        Some(self.open_rename(id, window, cx))
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_summary_model(
        &mut self,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
            view.update(cx, |v, cx| v.save_summary_model_for_test(model, window, cx));
        }
    }

    /// The open thread's view, as last sent.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_detail(&self) -> Option<&ThreadDetail> {
        self.open.as_ref()?.detail.as_ref()
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_new_thread(
        &mut self,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(page) = self.open_new_thread(window, cx) {
            page.update(cx, |p, cx| {
                p.type_prompt(prompt, window, cx);
                p.submit(window, cx);
            });
        }
    }

    /// Dev: put `text` in New thread's prompt, as typing would.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_type(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (self.thread_view(), &self.new_thread) {
            (Some(view), _) => view.update(cx, |v, cx| v.snapshot_type(text, window, cx)),
            (_, Some(page)) => page.update(cx, |p, cx| p.type_prompt(text, window, cx)),
            _ => {}
        }
    }

    /// What the harness's `expect:` checks, now: whether a dialog is open, which field has
    /// focus, or anything `until:` waits for.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_expect(
        &mut self,
        what: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let connect = self.connect_view.as_ref().and_then(|v| v.upgrade());
        let focused = |field: Option<FocusHandle>| field.is_some_and(|f| f.is_focused(window));
        match what {
            // The open thread's title has this in it (a banner's click opened it, say).
            w if w.starts_with("selected:") => {
                let needle = &w["selected:".len()..];
                self.selected().is_some_and(|id| {
                    self.summaries
                        .iter()
                        .any(|s| s.thread.id == id && s.thread.title.contains(needle))
                })
            }
            "dialog" => window.has_active_dialog(cx),
            "no-dialog" => !window.has_active_dialog(cx),
            "focus:connect-link" => focused(connect.map(|v| v.read(cx).link.focus_handle(cx))),
            "focus:connect-code" => focused(connect.map(|v| v.read(cx).code.focus_handle(cx))),
            // The field the last form opened with.
            "focus:form" => focused(crate::form::FIRST.with(|f| f.borrow().clone())),
            "focus:key" => focused(Some(self.key_input.focus_handle(cx))),
            // The refusal offers a server here, or the one the database moved to.
            "offer:socket" => matches!(
                self.backend.instead(),
                Some(flotilla_server::Instead::Socket(_))
            ),
            "offer:moved" => matches!(
                self.backend.instead(),
                Some(flotilla_server::Instead::Moved(_))
            ),
            // A form's request went out with what was typed (offline, it's refused at once).
            s if s.starts_with("asked:") => {
                let want = &s["asked:".len()..];
                self.asked.iter().any(|r| match r {
                    Request::Rename { title, .. } => want == format!("rename:{title}"),
                    Request::LinkPr { number, .. } => want == format!("link-pr:{number}"),
                    Request::Preview { port, .. } => want == format!("preview:{port}"),
                    Request::MakePublic { port, for_, .. } => {
                        want == format!("make-public:{port}:{for_:?}")
                    }
                    Request::MakePrivate { port, .. } => want == format!("make-private:{port}"),
                    Request::AddProject { repo, .. } => {
                        want == format!("add-project:{}", repo.full_name())
                    }
                    Request::Suggest(repo) => want == format!("suggest:{}", repo.full_name()),
                    Request::SetScripts { scripts, run, .. } => {
                        want == format!(
                            "scripts:{}:{run}",
                            scripts
                                .iter()
                                .map(|s| s.command.as_str())
                                .collect::<Vec<_>>()
                                .join(",")
                        )
                    }
                    _ => false,
                })
            }
            // Which forms are still open (their views live only while their dialogs do).
            "settings-open" => self
                .settings_view
                .as_ref()
                .is_some_and(|v| v.upgrade().is_some()),
            "settings-closed" => !self
                .settings_view
                .as_ref()
                .is_some_and(|v| v.upgrade().is_some()),
            // The open Settings shows idle VMs stopping after `n` minutes.
            s if s.starts_with("settings-idle:") => {
                let shown = self
                    .settings_view
                    .as_ref()
                    .and_then(|v| v.upgrade())
                    .and_then(|v| v.read(cx).snapshot_idle_minutes());
                eprintln!("[script] settings idle: {shown:?}");
                shown.is_some_and(|m| s["settings-idle:".len()..].parse() == Ok(m))
            }
            "project-open" => self
                .project_view
                .as_ref()
                .is_some_and(|v| v.upgrade().is_some()),
            "add-project-open" => self
                .add_project
                .as_ref()
                .is_some_and(|v| v.upgrade().is_some()),
            s if s.starts_with("prefix:") => self
                .settings
                .as_ref()
                .is_some_and(|v| v.branch_prefix == s["prefix:".len()..]),
            "moved" => crate::backend::moved(&self.db),
            "not-moved" => !crate::backend::moved(&self.db),
            // The ⌘K palette: open, its field focused, rows shown and messages found.
            "palette" => crate::palette::is_open(self),
            "no-palette" => !crate::palette::is_open(self),
            "focus:palette" => self
                .palette
                .as_ref()
                .and_then(|p| p.upgrade())
                .is_some_and(|p| p.read(cx).snapshot_focused(window, cx)),
            // What the palette highlights: nothing, or a row whose pick (as debugged) has this in it.
            "palette-highlight:none" => self
                .palette
                .as_ref()
                .and_then(|p| p.upgrade())
                .is_some_and(|p| p.read(cx).snapshot_state().3.is_none()),
            s if s.starts_with("palette-highlight:") => {
                let want = &s["palette-highlight:".len()..];
                self.palette
                    .as_ref()
                    .and_then(|p| p.upgrade())
                    .is_some_and(|p| {
                        p.read(cx)
                            .snapshot_state()
                            .3
                            .is_some_and(|pick| format!("{pick:?}").contains(want))
                    })
            }
            "palette-searching" => self
                .palette
                .as_ref()
                .and_then(|p| p.upgrade())
                .is_some_and(|p| p.read(cx).snapshot_state().2),
            "focus:composer" => self
                .thread_view()
                .is_some_and(|v| v.read(cx).composer_state(window, cx).2),
            // The open thread's composer holds exactly this (`composer:` for empty).
            s if s.starts_with("composer:") => {
                let got = self.snapshot_composer(window, cx).map(|(text, ..)| text);
                eprintln!("[script] composer: {got:?}");
                got.as_deref() == Some(&s["composer:".len()..])
            }
            // How many toasts show that weren't there at `step:toasts-mark`.
            s if s.starts_with("new-toasts:") => {
                let got = window
                    .notifications(cx)
                    .iter()
                    .filter(|n| !self.toasts_mark.contains(&n.entity_id()))
                    .count();
                eprintln!("[script] new toasts: {got}");
                s["new-toasts:".len()..].parse() == Ok(got)
            }
            // No thread's composer has text kept for it (a send that failed, waiting there).
            "no-unsent" => {
                let kept: Vec<String> = self
                    .device
                    .for_server(&self.server_key)
                    .unsent
                    .keys()
                    .cloned()
                    .collect();
                eprintln!("[script] unsent kept for: {kept:?}");
                kept.is_empty()
            }
            // What you asked of the open thread still shows over its row (until it's answered).
            "expecting" => self
                .selected()
                .is_some_and(|id| self.expected.contains_key(id)),
            "not-expecting" => !self
                .selected()
                .is_some_and(|id| self.expected.contains_key(id)),
            // How many views came for threads or projects not open here (subscriptions that
            // should have gone).
            s if s.starts_with("strays:") => {
                eprintln!("[script] strays: {}", self.strays);
                s["strays:".len()..].parse() == Ok(self.strays)
            }
            // The server can't be reached: the window says so.
            "unreached" => self.unreached().is_some(),
            // `row:<title words>=<group>`: the group that thread's row shows under (`none`: no row).
            w if w.starts_with("row:") => {
                let Some((needle, want)) = w["row:".len()..].split_once('=') else {
                    return false;
                };
                let Some(id) = self
                    .summaries
                    .iter()
                    .find(|s| s.thread.title.contains(needle))
                    .map(|s| s.thread.id.clone())
                else {
                    return false;
                };
                let got = crate::sidebar::visible_rows(self)
                    .into_iter()
                    .find(|(r, _)| *r == id)
                    .map(|(_, g)| group_name(g))
                    .unwrap_or("none");
                eprintln!("[script] row {needle}: {got}");
                got == want
            }
            // `selected-row:<title words>`: that thread's row is the one marked open.
            w if w.starts_with("selected-row:") => {
                let needle = &w["selected-row:".len()..];
                self.selected()
                    .map(|s| self.listing().row_for(s))
                    .is_some_and(|r| {
                        self.summaries
                            .iter()
                            .any(|s| s.thread.id == r && s.thread.title.contains(needle))
                    })
            }
            other => self.snapshot_until(other, cx),
        }
    }

    /// Dev: whether the app shows `what` yet, for a demo script's `until:` (see snapshot.rs).
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_until(&mut self, what: &str, cx: &mut Context<Self>) -> bool {
        let open = self
            .summaries
            .iter()
            .find(|s| self.selected() == Some(s.thread.id.as_str()));
        let pr = open.and_then(|s| s.pr.as_ref()).map(|(pr, _)| pr);
        // The first project's latest setup run, as Settings lists it.
        let run = || self.settings.as_ref()?.projects.first()?.last_run.clone();
        match what {
            // The palette has found this many messages.
            s if s.starts_with("palette-hits:") => {
                let want: usize = s["palette-hits:".len()..].parse().unwrap_or(1);
                self.palette
                    .as_ref()
                    .and_then(|p| p.upgrade())
                    .is_some_and(|p| p.read(cx).snapshot_state().1 >= want)
            }
            // The open thread went to this message: its row at the top, marked.
            s if s.starts_with("jumped:") => {
                let seq: i64 = s["jumped:".len()..].parse().unwrap_or(-1);
                self.thread_view()
                    .is_some_and(|v| v.read(cx).snapshot_jumped(seq, cx))
            }
            "idle" => open.is_some_and(|s| matches!(s.activity, Activity::Idle | Activity::Unread)),
            // The open thread's agent asked a question that waits on you.
            "asked" => self
                .open
                .as_ref()
                .and_then(|o| o.detail.as_ref())
                .is_some_and(|d| {
                    d.messages.iter().any(|m| {
                        m.kind == flotilla_proto::ask::KIND
                            && m.status.as_deref() == Some(flotilla_proto::ask::WAITING)
                    })
                }),
            "pr" => pr.is_some(),
            "checks-fail" => pr.is_some_and(|p| p.checks.iter().any(|c| c.state == "fail")),
            "checks-pass" => pr.is_some_and(|p| {
                !p.checks.is_empty() && p.checks.iter().all(|c| c.state == "pass")
            }),
            "merged" => pr.is_some_and(|p| p.state == "MERGED"),
            "conflicts" => pr.is_some_and(|p| p.mergeable.as_deref() == Some("CONFLICTING")),
            "mergeable" => pr.is_some_and(|p| p.mergeable.as_deref() == Some("MERGEABLE")),
            "setup-running" => run().is_some_and(|r| r.state == "running"),
            "setup-done" => run().is_some_and(|r| r.state == "done"),
            "setup-failed" => run().is_some_and(|r| r.state == "failed"),
            "repos" => self
                .add_project
                .as_ref()
                .and_then(|v| v.upgrade())
                .is_some_and(|v| v.read(cx).snapshot_loaded()),
            // The connection to the engine's host, and pairing.
            "online" => matches!(self.connection, Some(flotilla_proto::Connection::Online)),
            "refused" => matches!(
                self.connection,
                Some(flotilla_proto::Connection::Refused { .. })
            ),
            "threads" => !self.summaries.is_empty(),
            // What the Dock's badge counts: some thread is unread, or none is.
            "unread" => crate::notify::unread(&self.summaries, &self.parents) > 0,
            "all-read" => crate::notify::unread(&self.summaries, &self.parents) == 0,
            "devices" => self.devices.is_some(),
            "agent-usage" => self.agent_usage.is_some(),
            "devices-open" => matches!(self.devices_in, DevicesIn::Open(_)),
            // The setup checks came; a skill is listed.
            "doctor" => self.settings.as_ref().is_some_and(|s| s.doctor.is_some()),
            "skill" => self.settings.as_ref().is_some_and(|s| !s.skills.is_empty()),
            "connect-error" => self
                .connect_view
                .as_ref()
                .and_then(|v| v.upgrade())
                .is_some_and(|v| v.read(cx).error().is_some()),
            // A file the script's driver writes when it's done its part (a revoke, a new code).
            f if f.starts_with("file:") => std::path::Path::new(&f["file:".len()..]).exists(),
            "subagents" => self
                .thread_view()
                .is_some_and(|v| v.read(cx).snapshot_pinned() > 0),
            // A child's answer (`woken`, or a second: `woken-again`) reached the open thread,
            // and the turn it started is over.
            "woken" | "woken-again" => {
                let answers = self
                    .open
                    .as_ref()
                    .and_then(|o| o.detail.as_ref())
                    .map_or(0, |d| {
                        d.messages
                            .iter()
                            .filter(|m| {
                                m.kind == "prompt"
                                    && m.text
                                        .as_deref()
                                        .is_some_and(|t| t.starts_with("[flotilla] "))
                            })
                            .count()
                    });
                answers >= if what == "woken" { 1 } else { 2 }
                    && open.is_some_and(|s| s.activity != Activity::Working)
            }
            other => {
                eprintln!("[script] until: unknown {other:?}");
                true
            }
        }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_step(
        &mut self,
        step: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pick = |a: Activity| {
            self.summaries
                .iter()
                .find(|s| s.activity == a)
                .map(|s| s.thread.id.clone())
        };
        match step {
            // Hide the app (⌘H), so its window isn't focused: alerts become banners; and back.
            "hide" => cx.hide(),
            "show" => {
                cx.activate(true);
                window.activate_window();
            }
            // What a finished delete leaves behind: the open thread's rows are gone.
            "deleted" => {
                if let Some(sel) = self.selected().map(String::from) {
                    self.ask(Request::Forget(sel));
                }
            }
            "archived" => {
                if let Some(id) = self
                    .summaries
                    .iter()
                    .find(|s| s.thread.provider == "mistral")
                    .map(|s| s.thread.id.clone())
                {
                    self.run_command(id, ThreadCommand::Archive, window, cx);
                }
                self.collapsed.remove(&Activity::Archived);
                cx.notify();
            }
            "thread" => {
                if let Some(id) = pick(Activity::Unread).or_else(|| pick(Activity::Idle)) {
                    self.select(id, window, cx);
                }
            }
            // Settings' branch prefix, typed and saved.
            s if s.starts_with("prefix:") => {
                if let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
                    view.update(cx, |v, cx| {
                        v.snapshot_prefix(&s["prefix:".len()..], window, cx)
                    });
                }
            }
            // The open thread's menu: Mark as unread, Archive, Unarchive, Stop VM.
            "unread" | "archive" | "unarchive" | "stop-vm" => {
                let cmd = match step {
                    "unread" => ThreadCommand::MarkUnread,
                    "archive" => ThreadCommand::Archive,
                    "stop-vm" => ThreadCommand::StopVm,
                    _ => ThreadCommand::Unarchive,
                };
                if let Some(id) = self.selected().map(String::from) {
                    self.run_command(id, cmd, window, cx);
                }
            }
            "tool-open" => {
                if let Some(v) = self.thread_view() {
                    v.update(cx, |v, cx| v.expand_last_tool(cx));
                }
            }
            // The newest tool output that went out cut: opened and scrolled to, then shown whole.
            "expand-cut" => {
                if let Some(v) = self.thread_view() {
                    v.update(cx, |v, cx| v.snapshot_expand_cut(cx));
                }
            }
            "after-cut" => {
                if let Some(v) = self.thread_view() {
                    v.update(cx, |v, cx| v.snapshot_after_cut(cx));
                }
            }
            "show-all" => {
                if let Some(v) = self.thread_view() {
                    v.update(cx, |v, cx| v.snapshot_show_all(cx));
                }
            }
            // Print the row at the top of the thread's list, and how far into it.
            "top" => {
                if let Some(v) = self.thread_view() {
                    eprintln!("[script] top: {}", v.read(cx).snapshot_top(cx));
                }
            }
            "pinned-open" => {
                if let Some(v) = self.thread_view() {
                    v.update(cx, |v, cx| v.unfold_pinned(cx));
                }
            }
            "subagent-open" => {
                if let Some(v) = self.thread_view() {
                    v.update(cx, |v, cx| v.open_last_subagent(cx));
                }
            }
            "working" => {
                if let Some(id) = pick(Activity::Working) {
                    self.select(id, window, cx);
                }
            }
            // Show a thread's row in another state, as if its turn started or ended:
            // `sim:<title words>|working|<prompt>`, `sim:<title words>|idle|<reply>` (or unread).
            s if s.starts_with("sim:") => {
                let mut parts = s["sim:".len()..].splitn(3, '|');
                let (needle, state, text) = (
                    parts.next().unwrap_or_default(),
                    parts.next().unwrap_or_default(),
                    parts.next().map(str::to_string),
                );
                let activity = match state {
                    "working" => Activity::Working,
                    "unread" => Activity::Unread,
                    _ => Activity::Idle,
                };
                if let Some(id) = self
                    .rows
                    .iter()
                    .find(|s| s.thread.title.contains(needle))
                    .map(|s| s.thread.id.clone())
                {
                    let started =
                        (activity == Activity::Working).then(flotilla_model::util::now_ms);
                    self.sim.insert(id, (activity, started, text));
                    self.show_rows(cx);
                }
            }
            "new-thread" => {
                self.open_new_thread(window, cx);
            }
            "new-thread-typed" => {
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.type_prompt("Add exponential backoff to the webhook sender in src/webhooks.rs, at most 5 tries, and add a test for it.", window, cx));
                }
            }
            // Attach the files in FLOTILLA_SNAPSHOT_ATTACH (`:`-separated) to New thread.
            "new-thread-attached" => {
                let paths: Vec<std::path::PathBuf> = std::env::var_os("FLOTILLA_SNAPSHOT_ATTACH")
                    .map(|v| std::env::split_paths(&v).collect())
                    .unwrap_or_default();
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.add_files(&paths, cx));
                }
            }
            "settings" => {
                window.close_all_dialogs(cx);
                self.open_settings(window, cx);
            }
            // As Settings → Add skill → Choose a folder… does: packed here, uploaded, kept there.
            s if s.starts_with("add-skill:") => {
                if let Some(skill) =
                    flotilla_model::skills::read(std::path::Path::new(&s["add-skill:".len()..]))
                {
                    self.add_skills(vec![skill], window, cx);
                }
            }
            // Idle VMs stop after `n` minutes, as another device (or the CLI) would set it.
            s if s.starts_with("set-idle:") => {
                if let Ok(m) = s["set-idle:".len()..].parse() {
                    self.ask(Request::Set(Setting::IdleMinutes(m)));
                }
            }
            "run-doctor" => {
                self.ask(Request::RunDoctor);
            }
            "add-project" => {
                window.close_all_dialogs(cx);
                self.open_settings(window, cx);
                self.open_add_project(window, cx);
            }
            "check-github" => {
                self.ask(Request::CheckGitHub);
            }
            // This Mac with no boat key, as the engine would say: Connect to boat.
            "no-key" => {
                let why = flotilla_core::Error::NoApiKey(
                    "run `flotilla setup`, or set BOAT_API_KEY".into(),
                );
                self.engine = Some(EngineState::Stopped(why.to_string()));
                cx.notify();
            }
            // The server out of reach, or back, as the connection would say.
            "server-down" | "server-up" => {
                let c = if step == "server-down" {
                    flotilla_proto::Connection::Offline {
                        error: "connection refused".into(),
                    }
                } else {
                    flotilla_proto::Connection::Online
                };
                self.on_msg(ServerMsg::Connection(c), window, cx);
            }
            // The server won't have this Mac (revoked, say), as the connection would say; then
            // `server-up` is it taking this Mac after all.
            "refuse" => self.on_msg(
                ServerMsg::Connection(flotilla_proto::Connection::Refused {
                    reason: "This device was removed".into(),
                }),
                window,
                cx,
            ),
            // What `flotilla forget` does to a thread: its rows gone, on the server.
            s if s.starts_with("forget:") => {
                if let Some(id) = self
                    .summaries
                    .iter()
                    .find(|r| r.thread.title.contains(&s["forget:".len()..]))
                    .map(|r| r.thread.id.clone())
                {
                    self.ask(Request::Forget(id));
                }
            }
            // An action the engine started itself ended (see `snapshot_engine_done`):
            // `engine-done:<what>`, on the open thread, or `engine-done:<what>|<title words>`.
            s if s.starts_with("engine-done:") => {
                let rest = &s["engine-done:".len()..];
                let (what, needle) = rest
                    .split_once('|')
                    .map_or((rest, None), |(w, n)| (w, Some(n)));
                self.snapshot_engine_done(what, needle, window, cx);
            }
            // Toasts from here on are counted (`expect:new-toasts:`).
            "toasts-mark" => {
                self.toasts_mark = window
                    .notifications(cx)
                    .iter()
                    .map(|n| n.entity_id())
                    .collect()
            }
            // A project's setup script saved (from any device): `set-scripts:<repo words>|<command>`.
            s if s.starts_with("set-scripts:") => {
                let (name, command) = s["set-scripts:".len()..]
                    .split_once('|')
                    .unwrap_or((&s["set-scripts:".len()..], "make setup"));
                if let Some(p) = self.projects().into_iter().find(|p| p.repo.contains(name)) {
                    self.ask(Request::SetScripts {
                        project: p.id,
                        scripts: vec![flotilla_proto::SetupScript {
                            command: command.into(),
                            fresh_only: false,
                        }],
                        run: false,
                    });
                }
            }
            // gh found but not signed in, or signed in to `login`, as the engine would say.
            s if s == "gh-signed-out" || s.starts_with("gh-signed-in:") => {
                let status = match s.strip_prefix("gh-signed-in:") {
                    Some(login) => GhStatus::SignedIn {
                        login: login.into(),
                        version: "2.60.0".into(),
                        scopes: Vec::new(),
                    },
                    None => GhStatus::NotSignedIn {
                        version: "2.60.0".into(),
                    },
                };
                self.gh_status = status.clone();
                if let Some(view) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    view.update(cx, |v, cx| v.set_gh(status, cx));
                }
            }
            // The counts beside Settings, as boat might report them: comfortable, then near
            // the limits (the last minute's starts used up, the 24 hours' and the plan's VM
            // hours nearly, extra starts on).
            "usage" | "usage-tight" => {
                let tight = step == "usage-tight";
                let now = flotilla_model::util::now_ms();
                self.usage = Some(flotilla_proto::Usage {
                    vms: Some((3, 100)),
                    minute: Some((if tight { 12 } else { 0 }, 12)),
                    hour: Some((if tight { 22 } else { 7 }, 60)),
                    day: Some((if tight { 187 } else { 120 }, 200)),
                    vm_hours: Some((if tight { 48 } else { 549 }, 556)),
                    renews: Some("2026-11-30T01:01:38.000Z".into()),
                    blocked: None,
                    starts_free: tight.then(|| {
                        chrono::DateTime::from_timestamp_millis(now + 40_000)
                            .unwrap_or_default()
                            .to_rfc3339()
                    }),
                    extra_starts: Some(flotilla_proto::ExtraStarts {
                        on: tight,
                        used: if tight { 7 } else { 0 },
                        spend_cents: Some((if tight { 29 } else { 0 }, 8000)),
                        cap_reached: false,
                    }),
                    read_ms: Some(now - 20_000),
                    sizes_unavailable: vec![(
                        "xlarge".into(),
                        "needs the $100/month plan or higher".into(),
                    )],
                });
                self.push_sizes(cx);
                cx.notify();
            }
            "usage-open" => {
                self.usage_open = true;
                self.usage_tab = crate::meter::UsageTab::Vms;
                self.sync_agent_usage();
                cx.notify();
            }
            // The agent limits beside the counts: the popover on its Agents tab.
            "limits-open" => {
                self.limits_open = true;
                self.usage_tab = crate::meter::UsageTab::Agents;
                self.sync_agent_usage();
                cx.notify();
            }
            // boat's counts' popover, on its Agents tab.
            "agents-open" => {
                self.usage_open = true;
                self.usage_tab = crate::meter::UsageTab::Agents;
                self.sync_agent_usage();
                cx.notify();
            }
            // Pick a model for its trend (`agent-model:<model>`), or none (`agent-model:`).
            s if s.starts_with("agent-model:") => {
                let name = &s["agent-model:".len()..];
                self.agent_model = self
                    .agent_usage
                    .as_ref()
                    .and_then(|u| u.hours.iter().find(|h| h.model == name))
                    .map(|h| (h.provider.clone(), h.model.clone()));
                cx.notify();
            }
            "close-dialogs" => window.close_all_dialogs(cx),
            // Send later from the open thread's composer, and from New thread; Scheduled.
            "later" => {
                if let Some(view) = self.thread_view().cloned() {
                    let (thread, text) = view.read(cx).snapshot_later(cx);
                    let to = self
                        .summaries
                        .iter()
                        .find(|s| s.thread.id == thread)
                        .map(|s| s.thread.title.clone())
                        .unwrap_or_default();
                    let seed = Seed {
                        id: None,
                        prompt: text,
                        targets: vec![flotilla_proto::Target::Thread(thread.clone())],
                        to,
                        when: None,
                        zone: flotilla_model::schedule::here(),
                    };
                    self.open_schedule_editor(seed, Later::Thread(thread), window, cx);
                }
            }
            "new-later" => {
                if let Some(page) = self.new_thread.clone() {
                    page.update(cx, |p, cx| p.later(cx));
                }
            }
            "scheduled" => self.open_scheduled(None, window, cx),
            // The form's own controls: Repeat, a kind, a cron, a day, Save.
            s if s.starts_with("later-") => {
                if let Some(editor) = self.schedule_editor.as_ref().and_then(|f| f.view.upgrade()) {
                    editor.update(cx, |e, cx| {
                        e.snapshot_step(&s["later-".len()..], window, cx)
                    });
                }
            }
            // What the server says when a run couldn't happen.
            s if s.starts_with("schedule-failed:") => {
                let why = s["schedule-failed:".len()..].to_string();
                let first = self.schedules.first().map(|s| {
                    (
                        s.id.clone(),
                        s.prompt.clone(),
                        s.target.thread().map(str::to_string),
                    )
                });
                if let Some((schedule, prompt, thread)) = first {
                    self.on_note(
                        Note::ScheduleFailed {
                            schedule,
                            prompt,
                            thread,
                            why,
                        },
                        window,
                        cx,
                    );
                }
            }
            // The forms: Rename and Link a pull request on the open thread (else the first),
            // and the open thread's Preview → Open a port.
            "rename" | "link-pr" => {
                if let Some(id) = self
                    .selected()
                    .map(String::from)
                    .or_else(|| self.summaries.first().map(|s| s.thread.id.clone()))
                {
                    if step == "rename" {
                        self.open_rename(id, window, cx);
                    } else {
                        self.open_link_pr(id, window, cx);
                    }
                }
            }
            "preview" => {
                if let Some(view) = self.thread_view() {
                    view.update(cx, |v, cx| v.snapshot_preview(window, cx));
                }
            }
            "project-add-script" => {
                if let Some(v) = self.project_view.as_ref().and_then(|v| v.upgrade()) {
                    v.update(cx, |v, cx| v.snapshot_add_script(window, cx));
                }
            }
            "focus-prefix" => {
                if let Some(v) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
                    v.update(cx, |v, cx| v.snapshot_focus_prefix(window, cx));
                }
            }
            // The refusal's offer: the server here, or the one This Mac's database moved to.
            "take-offer" => self.take_offer(window, cx),
            // "Connect to a server…" with a link or address, its code, and Connect.
            s if s.starts_with("connect:") => {
                self.open_connect(Some(s["connect:".len()..].to_string()), window, cx)
            }
            "connect" => self.open_connect(None, window, cx),
            // The same, with the link read from a file now (one made after the script began).
            s if s.starts_with("connect-file:") => {
                let link = std::fs::read_to_string(&s["connect-file:".len()..]).unwrap_or_default();
                self.open_connect(Some(link.trim().to_string()), window, cx);
            }
            s if s.starts_with("connect-code:") => {
                if let Some(v) = self.connect_view.as_ref().and_then(|v| v.upgrade()) {
                    let code = v.read(cx).code.clone();
                    code.update(cx, |c, cx| {
                        c.set_value(s["connect-code:".len()..].to_string(), window, cx)
                    });
                }
            }
            "connect-submit" => {
                if let Some(v) = self.connect_view.as_ref().and_then(|v| v.upgrade()) {
                    v.update(cx, |v, cx| v.submit(window, cx));
                }
            }
            // As if macOS opened a `flotilla://` link.
            s if s.starts_with("open-url:") => {
                self.open_link(s["open-url:".len()..].to_string(), window, cx)
            }
            "add-device" => {
                self.ask(Request::PairCode { ttl_secs: None });
            }
            s if s.starts_with("revoke:") => {
                let name = &s["revoke:".len()..];
                if let Some(d) = self
                    .devices
                    .as_ref()
                    .and_then(|d| d.devices.iter().find(|d| d.name.contains(name)))
                {
                    self.ask(Request::RevokeDevice(d.id.clone()));
                }
            }
            "use-this-mac" => self.use_this_mac(window, cx),
            // Settings' Use, and Forget, on the server this Mac is paired with.
            "use-server" => self.use_paired(window, cx),
            "forget-server" => self.forget_paired(window, cx),
            // Settings' switches on This Mac: let other devices connect (over Tailscale), or not.
            "devices-in" | "devices-in-tailscale" | "devices-out" => {
                self.let_devices_in(step != "devices-out", step == "devices-in-tailscale", cx)
            }
            // Add project's list: a cached one, then the same fetched again.
            "seed-repos" | "refresh-repos" => {
                if let Some(v) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    v.update(cx, |v, cx| v.snapshot_repos(step == "refresh-repos", cx));
                }
            }
            // Stop or restart one of the open thread's services, as the menu does.
            s if s.starts_with("service-stop:") || s.starts_with("service-restart:") => {
                if let Some(thread) = self.selected().map(String::from) {
                    let cmd = match s.split_once(':') {
                        Some(("service-stop", name)) => flotilla_proto::ServiceCmd::Stop {
                            thread,
                            name: name.to_string(),
                        },
                        Some((_, name)) => flotilla_proto::ServiceCmd::Restart {
                            thread,
                            name: name.to_string(),
                        },
                        None => return,
                    };
                    self.ask(Request::Services(cmd));
                }
            }
            // Make public… for a port of the open thread.
            s if s.starts_with("make-public:") => {
                if let (Some(view), Ok(port)) =
                    (self.thread_view(), s["make-public:".len()..].parse::<u16>())
                {
                    view.update(cx, |v, cx| v.snapshot_make_public(port, window, cx));
                }
            }
            // The open thread's Preview menu of services, or one service's output.
            s if s == "services-menu" || s.starts_with("service-output:") => {
                let logs = s.strip_prefix("service-output:").map(str::to_string);
                if let Some(view) = self.thread_view() {
                    view.update(cx, |v, cx| v.snapshot_services(logs.as_deref(), window, cx));
                }
            }
            s if s.starts_with("send:") => {
                if let Some(id) = self.selected().map(String::from) {
                    self.send_message(id, s["send:".len()..].to_string(), Vec::new(), window, cx);
                }
            }
            // Add a file to whichever composer shows, as the paperclip or a drop would.
            s if s.starts_with("attach:") => {
                let path = PathBuf::from(&s["attach:".len()..]);
                if let Some(view) = self.thread_view() {
                    view.update(cx, |v, cx| v.add_files(&[path], cx));
                } else if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.add_files(&[path], cx));
                }
            }
            // Put text in the open thread's composer, as typing would, and send it.
            s if s.starts_with("compose:") => {
                if let Some(view) = self.thread_view() {
                    view.update(cx, |v, cx| {
                        v.fill_prompt(&s["compose:".len()..], window, cx)
                    });
                }
            }
            "submit" => {
                if let Some(view) = self.thread_view() {
                    view.update(cx, |v, cx| v.snapshot_submit(window, cx));
                }
            }
            "pr-ready" | "pr-merge" => {
                if let Some(id) = self.selected().map(String::from) {
                    self.ask(if step == "pr-ready" {
                        Request::MarkReady(id)
                    } else {
                        Request::MergePr(id)
                    });
                }
            }
            // The PR row's Fix / Resolve / Address review, on the open thread.
            "pr-fix" | "pr-resolve" => {
                let open = self
                    .summaries
                    .iter()
                    .find(|s| self.selected() == Some(s.thread.id.as_str()))
                    .cloned();
                if let (Some(view), Some(s)) = (self.thread_view().cloned(), open)
                    && let Some((pr, _)) = &s.pr
                {
                    let prompt = if step == "pr-fix" {
                        flotilla_model::fix::checks(pr)
                    } else {
                        flotilla_model::fix::conflicts(pr)
                    };
                    // As the button does: a thread whose VM is gone sends it to a new thread.
                    if s.thread.vm_gone() {
                        self.new_thread_on_branch(&s.thread.id, Some(prompt));
                    } else {
                        view.update(cx, |v, cx| v.fill_prompt(&prompt, window, cx));
                    }
                }
            }
            "pr-review" => {
                if let Some(id) = self.selected().map(String::from) {
                    self.ask(Request::ReviewPrompt(id));
                }
            }
            // "Fix in a new thread" on a project's latest setup run.
            s if s.starts_with("fix-setup:") => {
                let name = &s["fix-setup:".len()..];
                let run = self
                    .settings
                    .as_ref()
                    .and_then(|s| s.projects.iter().find(|r| r.project.repo.contains(name)))
                    .and_then(|r| Some((r.project.id.clone(), r.last_run.as_ref()?.id.clone())));
                if let Some((project, run)) = run {
                    self.ask(Request::SetupFixPrompt { project, run });
                }
            }
            "new-thread-submit" => {
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.submit(window, cx));
                }
            }
            "add-project-submit" => {
                if let Some(v) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    v.update(cx, |v, cx| v.snapshot_add(cx));
                }
            }
            s if s.starts_with("open-project:") => {
                let name = &s["open-project:".len()..];
                if let Some(p) = self.projects().into_iter().find(|p| p.repo.contains(name)) {
                    window.close_all_dialogs(cx);
                    self.open_settings(window, cx);
                    self.open_project(p.id, window, cx);
                }
            }
            // New thread: pick a project, then a branch (`#N`, a name, or empty for new).
            // New thread's agent: `agent:claude-code/claude-haiku-5-5/low` (no effort: the model's own).
            s if s.starts_with("agent:") => {
                let mut parts = s["agent:".len()..].split('/');
                let (provider, model) = (
                    parts.next().unwrap_or_default().to_string(),
                    parts.next().unwrap_or_default().to_string(),
                );
                let settings = flotilla_proto::ThreadSettings {
                    provider,
                    model,
                    effort: parts.next().map(String::from),
                    fast: false,
                };
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.snapshot_agent(settings, window, cx));
                }
            }
            // New thread: ⇧-click another model, `agent-also:codex/gpt-5.1-codex-mini`.
            s if s.starts_with("agent-also:") => {
                let mut parts = s["agent-also:".len()..].split('/');
                let (provider, model) = (
                    parts.next().unwrap_or_default().to_string(),
                    parts.next().unwrap_or_default().to_string(),
                );
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.snapshot_also(&provider, &model, window, cx));
                }
            }
            // ⌘Enter on New thread: start in the background and stay.
            "new-thread-stay" => {
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.start(true, window, cx));
                }
            }
            // ⌘⇧S.
            "archive-next" => self.archive_and_next(window, cx),
            // What the server says when a thread's pull request merges and it archives the
            // thread for it (`merged-archived:<title>`; the rule itself is the engine's).
            s if s.starts_with("merged-archived:") => {
                let needle = s["merged-archived:".len()..].to_lowercase();
                if let Some(t) = self
                    .summaries
                    .iter()
                    .find(|s| s.thread.title.to_lowercase().contains(&needle))
                    .map(|s| s.thread.clone())
                {
                    self.ask(Request::Archive(t.id.clone()));
                    let title = t.title.clone();
                    cx.defer_in(window, move |this, window, cx| {
                        this.on_note(
                            Note::PrMerged {
                                thread: t.id,
                                number: 42,
                                title,
                                branch: "main".into(),
                                archived: true,
                            },
                            window,
                            cx,
                        )
                    });
                }
            }
            // What the server says when it archives threads that sat idle: the two longest idle.
            "idle-archived" => {
                let mut idle: Vec<&ThreadSummary> = self
                    .summaries
                    .iter()
                    .filter(|s| {
                        s.activity == Activity::Idle
                            && Some(s.thread.id.as_str()) != self.selected()
                    })
                    .collect();
                idle.sort_by_key(|s| s.last_activity_ms);
                let ids: Vec<String> = idle
                    .into_iter()
                    .take(2)
                    .map(|s| s.thread.id.clone())
                    .collect();
                for id in &ids {
                    self.ask(Request::Archive(id.clone()));
                }
                cx.defer_in(window, move |this, window, cx| {
                    this.on_note(Note::IdleArchived(ids), window, cx)
                });
            }
            s if s.starts_with("project:") => {
                let name = s["project:".len()..].to_string();
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.snapshot_project(&name, window, cx));
                }
            }
            s if s.starts_with("branch:") => {
                let pick = s["branch:".len()..].to_string();
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.snapshot_branch(&pick, window, cx));
                }
            }
            // Remove a project as Settings' Remove does, without the confirmation.
            s if s.starts_with("remove-project:") => {
                let name = &s["remove-project:".len()..];
                if let Some(p) = self.projects().into_iter().find(|p| p.repo.contains(name)) {
                    self.ask(Request::RemoveProject(p.id));
                }
            }
            // The open thread's "New thread on this branch".
            "on-branch" => {
                if let Some(id) = self.selected().map(String::from) {
                    self.run_command(id, ThreadCommand::NewThreadOnBranch, window, cx);
                }
            }
            // Filter Add project's repo list (it may still be loading).
            s if s.starts_with("search-repos:") => {
                let q = s["search-repos:".len()..].to_string();
                if let Some(v) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    v.update(cx, |v, cx| v.snapshot_search(&q, window, cx));
                }
            }
            s if s.starts_with("pick-repo:") => {
                let name = s["pick-repo:".len()..].to_string();
                if let Some(v) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    v.update(cx, |v, cx| v.snapshot_pick(&name, cx));
                }
            }
            "dark" => {
                window.close_all_dialogs(cx);
                self.collapsed.clear();
                if let Some(id) = pick(Activity::Unread).or_else(|| pick(Activity::Idle)) {
                    self.close_thread(cx);
                    self.select(id, window, cx);
                }
                Theme::change(gpui_kit::component::ThemeMode::Dark, Some(window), cx);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Ask for something whose result shows at once: a rename, the keep-running switch, the
    /// VM stopping or starting, a deleted thread gone. When boat answers, `on_answer` drops the
    /// expectation and the views show what really happened.
    pub(crate) fn perform(&mut self, req: Request, cx: &mut Context<Self>) {
        let mut close = None;
        if let Some(id) = req.thread().map(str::to_string) {
            let state = self
                .rows
                .iter()
                .find(|s| s.thread.id == id)
                .map(|s| s.thread.state);
            let e = self.expected.entry(id.clone()).or_default();
            match &req {
                Request::Rename { title, .. } => e.title = Some(title.clone()),
                Request::KeepRunning { on, .. } => e.keep_running = Some(*on),
                Request::Stop(_) => e.state = Some(ThreadState::Stopping),
                // Only a VM that may be up is stopped by archiving.
                Request::Archive(_)
                    if !matches!(
                        state,
                        None | Some(ThreadState::Expired | ThreadState::Creating)
                    ) =>
                {
                    e.state = Some(ThreadState::Stopping)
                }
                Request::Resume(_) => e.state = Some(ThreadState::Starting),
                Request::Retry(_) => e.state = Some(ThreadState::Creating),
                Request::Delete(_) => {
                    e.deleting = true;
                    close = self.selected().filter(|s| *s == id).map(str::to_string);
                }
                _ => {}
            }
            if self.expected.get(&id).is_some_and(Expected::is_empty) {
                self.expected.remove(&id);
            }
        }
        if close.is_some() {
            self.close_thread(cx);
        }
        self.ask(req);
        self.show_rows(cx);
    }

    /// Start threads from the new-thread page, one per model chosen. Unless `stay`, the first
    /// opens at once, its first prompt marked as sending, while the engine side writes it and
    /// boat makes the VM and sends the prompt; until the sidebar lists it, it's shown from what
    /// was asked, and the page (hidden) keeps the draft, showing again with it if the start is
    /// refused. The rest (all of them with `stay`) start in the background, listed from what
    /// was asked until the sidebar has them.
    fn start_threads(
        &mut self,
        threads: Vec<flotilla_proto::NewThread>,
        stay: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(first) = threads.first() else { return };
        // They'll finish while you're elsewhere: the moment to ask whether that may be said.
        crate::notify::moment(cx, self.device.notifications_on());
        self.device.for_server_mut(&self.server_key).last_project =
            Some(first.project_id.clone().unwrap_or_default());
        self.save_device();
        let mut reqs = threads;
        for req in &mut reqs {
            req.id = Some(uuid::Uuid::new_v4().to_string());
        }
        let opened = (!stay).then(|| reqs[0].id.clone()).flatten();
        for req in &reqs {
            if req.id == opened {
                self.starting = Some(req.clone());
            } else {
                self.background.push(req.clone());
            }
        }
        let files = reqs[0].attachments.clone();
        if files.is_empty() {
            for req in reqs {
                self.ask(Request::NewThread(Box::new(req)));
            }
        } else {
            // The files go up once for all of them; the threads show from what was asked
            // meanwhile.
            let up = self.backend.uploader();
            let task = cx.background_spawn(async move {
                // The harness can slow uploads down, as a far server would be.
                #[cfg(feature = "snapshot")]
                if let Some(ms) = std::env::var("FLOTILLA_SNAPSHOT_SLOW_UPLOAD")
                    .ok()
                    .and_then(|m| m.parse().ok())
                {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
                flotilla_model::attach::upload(&files, |b| up.put(b))
            });
            let life = self.life;
            cx.spawn_in(window, async move |this, cx| {
                let uploaded = task.await;
                let _ = this.update_in(cx, |app, window, cx| {
                    // Uploaded to a server this Mac has since switched from: the prompt was
                    // kept in its New thread (see `keep_drafts`).
                    if app.life != life {
                        return;
                    }
                    for mut req in reqs {
                        match &uploaded {
                            Ok(files) => {
                                req.attachments = files.clone();
                                if app.starting.as_ref().and_then(|r| r.id.as_deref())
                                    == req.id.as_deref()
                                {
                                    app.starting = Some(req.clone());
                                }
                                app.ask(Request::NewThread(Box::new(req)));
                            }
                            Err(e) => app.on_answer(
                                Request::NewThread(Box::new(req)),
                                Err(e.clone()),
                                window,
                                cx,
                            ),
                        }
                    }
                });
            })
            .detach();
        }
        self.show_rows(cx);
        match opened {
            Some(id) => self.select(id, window, cx),
            None => cx.notify(),
        }
    }

    /// Show the message in the thread at once, marked as sending, then send it. Resuming a
    /// stopped VM can take seconds; the placeholder takes boat's id when the send goes through
    /// and goes away (the text back in the composer) if it fails.
    pub(crate) fn send_message(
        &mut self,
        thread: String,
        text: String,
        files: Vec<Attached>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::notify::moment(cx, self.device.notifications_on());
        let local = format!(
            "{}{}",
            flotilla_model::lifecycle::LOCAL_PREFIX,
            uuid::Uuid::new_v4()
        );
        if files.is_empty() {
            self.ask(Request::Send {
                thread,
                text,
                files,
                local,
            });
            self.refresh_thread_view(cx);
            return;
        }
        // The files go up first (to the server, or this Mac's own store); it shows as sending
        // meanwhile.
        let up = self.backend.uploader();
        let pending = files.clone();
        let task =
            cx.background_spawn(
                async move { flotilla_model::attach::upload(&pending, |b| up.put(b)) },
            );
        self.uploading.insert(
            local.clone(),
            Uploading {
                thread,
                text,
                files,
            },
        );
        cx.spawn_in(window, async move |this, cx| {
            let uploaded = task.await;
            let _ = this.update_in(cx, |app, window, cx| {
                let Some(Uploading {
                    thread,
                    text,
                    files,
                }) = app.uploading.remove(&local)
                else {
                    return;
                };
                match uploaded {
                    Ok(files) => {
                        app.ask(Request::Send {
                            thread,
                            text,
                            files,
                            local,
                        });
                    }
                    // As a failed send: the text and files go back in the composer.
                    Err(e) => app.on_answer(
                        Request::Send {
                            thread,
                            text,
                            files,
                            local,
                        },
                        Err(e),
                        window,
                        cx,
                    ),
                }
                app.refresh_thread_view(cx);
            });
        })
        .detach();
        self.refresh_thread_view(cx);
    }

    /// The device this Mac is to the engine's side (none: This Mac). Only files it sent are
    /// opened from their paths here.
    fn mine(&self, f: &Attached) -> bool {
        f.from == self.backend.device()
    }

    /// Something from the engine side.
    fn on_msg(&mut self, msg: ServerMsg, window: &mut Window, cx: &mut Context<Self>) {
        match msg {
            ServerMsg::Snap { view } => self.on_view(*view, window, cx),
            ServerMsg::Res { id, result } => {
                if let Some(rename) = self.naming.remove(&id) {
                    let _ = rename.update(cx, |v, cx| v.generated(result.clone(), window, cx));
                }
                if let Some(req) = self.pending.remove(&id) {
                    if let Request::Send { local, .. } = &req {
                        self.shown_sends.remove(local);
                    }
                    self.on_answer(req, result, window, cx);
                }
            }
            ServerMsg::Live { thread, partials } => {
                if let Some(open) = self.open.as_ref().filter(|o| o.id == thread) {
                    open.view.update(cx, |v, cx| v.set_partials(partials, cx));
                }
            }
            ServerMsg::Note(note) => self.on_note(note, window, cx),
            ServerMsg::Progress { note, .. } => self.on_note(note, window, cx),
            ServerMsg::Connection(c) => {
                match &c {
                    // Nothing it showed stays.
                    flotilla_proto::Connection::Refused { .. } if !self.forgot => {
                        self.forget_views(window, cx)
                    }
                    // It takes this Mac after all: everything again, from the start.
                    flotilla_proto::Connection::Online if self.forgot => {
                        self.forgot = false;
                        self.subscribe_all();
                    }
                    _ => {}
                }
                if matches!(c, flotilla_proto::Connection::Online) && self.server.is_some() {
                    self.note_moved(cx);
                }
                // A Refresh waits for the connection: no spinner meanwhile.
                if !matches!(c, flotilla_proto::Connection::Online) {
                    self.usage_refreshing = false;
                    self.agents_refreshing = false;
                }
                let down = matches!(c, flotilla_proto::Connection::Offline { .. });
                if let Some(view) = self.thread_view() {
                    view.update(cx, |v, cx| v.set_server_down(down, cx));
                }
                if !down {
                    self.unreached_since = None;
                }
                self.retrying = false;
                self.connection = Some(c);
                cx.notify();
            }
        }
    }

    /// A server that took in This Mac's database (`flotilla import`) runs its threads now.
    /// This Mac's database is marked as moved there, so This Mac's engine never runs them
    /// again beside it ([`crate::backend::note_moved`]).
    fn note_moved(&mut self, cx: &mut Context<Self>) {
        let Some(welcome) = self.backend.welcome().filter(|w| !w.adopted.is_empty()) else {
            return;
        };
        let (db, url) = (self.db.clone(), self.server.as_ref().map(|s| s.url.clone()));
        cx.background_spawn(async move { crate::backend::note_moved(&db, &welcome, url) })
            .detach();
    }

    /// What This Mac's refusal offers instead: the server here that holds its database, or
    /// the one its database moved to (Use it, when this Mac knows it; else Connect to a
    /// server…, with its address filled in).
    fn take_offer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.backend.instead().cloned() {
            Some(flotilla_server::Instead::Socket(address)) => {
                self.join_server_here(address, window, cx)
            }
            Some(flotilla_server::Instead::Moved(moved)) => {
                match self.device.paired.as_ref().filter(|s| s.id == moved.server) {
                    Some(_) => self.use_paired(window, cx),
                    None => {
                        self.open_connect(moved.url.filter(|u| !u.starts_with("unix:")), window, cx)
                    }
                }
            }
            None => {}
        }
    }

    /// Use the server on this machine that holds This Mac's database, through its socket:
    /// this machine's user needs no pairing there. It's kept as the server, as a paired one is
    /// (in place of any paired before).
    fn join_server_here(&mut self, address: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.joining {
            return;
        }
        self.joining = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            let target = flotilla_client::Target::parse(&address)?;
            flotilla_client::describe(&target).map(|d| SavedServer {
                id: d.server.id,
                url: address,
                device: String::new(),
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.joining = false;
                match result {
                    Ok(saved) => this.change_server(|d| pair_in_place(d, saved), window, cx),
                    Err(e) => window
                        .push_notification(Notification::error(e).title("Couldn't connect"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn on_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        match view {
            View::Sidebar(s) => {
                // The first-run screen's key field, ready to paste into.
                if matches!(s.engine, EngineState::Stopped(_))
                    && !matches!(self.engine, Some(EngineState::Stopped(_)))
                    && !window.has_active_dialog(cx)
                {
                    self.key_input.update(cx, |k, cx| k.focus(window, cx));
                }
                self.engine = Some(s.engine);
                self.status = s.status;
                self.usage = s.usage;
                if self.limits != s.limits {
                    self.limits = s.limits;
                    if let Some(view) = self.thread_view().cloned() {
                        let limits = self.limits.clone();
                        view.update(cx, |v, cx| v.set_limits(limits, cx));
                    }
                }
                if self.gh_status != s.github
                    && let Some(view) = self.add_project.as_ref().and_then(|v| v.upgrade())
                {
                    view.update(cx, |v, cx| v.set_gh(s.github.clone(), cx));
                }
                self.gh_status = s.github;
                self.environments = s.environments;
                self.projects_cache = s.projects;
                self.parents = s.parents.into_iter().collect();
                self.waiting = s.waiting.into_iter().collect();
                self.rows = s.threads;
                self.on_sidebar(window, cx);
            }
            View::Thread(d) => match self.open.as_mut().filter(|o| o.id == d.id) {
                Some(open) => {
                    open.history.view(&d);
                    // Opened at a search hit: every page down to its message is wanted.
                    if let Some(seq) = open.jump.take() {
                        open.history.reach(seq);
                    }
                    open.detail = Some(*d);
                    self.refresh_thread_view(cx);
                    // A page held that changed is fetched again.
                    self.fetch_older(cx);
                }
                None => self.stray(),
            },
            View::Mentions { thread, mentions } => {
                match self.open.as_ref().filter(|o| o.id == thread) {
                    Some(open) => open.view.update(cx, |v, cx| {
                        v.set_mentions(mentions.map(std::sync::Arc::new), cx)
                    }),
                    None => self.stray(),
                }
            }
            View::Project(p) => self.on_project(p, window, cx),
            View::Settings(s) => {
                if let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
                    let s = s.clone();
                    view.update(cx, |v, cx| v.set_settings(s, cx));
                }
                if !matches!(self.summary_state, SummaryState::Testing) {
                    self.summary_state = summary_state(&s);
                }
                if self.setup_checking.is_some()
                    && s.doctor.as_ref().map(|d| d.at_ms) != self.setup_checking
                {
                    self.setup_checking = None;
                }
                self.settings = Some(s);
            }
            View::Catalog(c) => {
                self.catalog = c;
                cx.notify();
            }
            View::Devices(d) => {
                if let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
                    let d = d.clone();
                    view.update(cx, |v, cx| v.set_devices(Some(d), cx));
                }
                self.devices = Some(d);
            }
            View::AgentUsage(u) => {
                self.agent_usage = Some(u);
                cx.notify();
            }
            View::Schedules(s) => {
                self.schedules = s.schedules;
                self.refresh_schedules(window, cx);
                self.refresh_thread_view(cx);
                cx.notify();
            }
        }
    }

    /// A new sidebar view: the rows, the filter, a thread just started, the open thread gone.
    fn on_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A removed project drops out of the filter.
        let before = self.hidden_projects.len();
        let ids: HashSet<&str> = self.projects_cache.iter().map(|p| p.id.as_str()).collect();
        self.hidden_projects
            .retain(|k| k == NO_PROJECT || ids.contains(k.as_str()));
        // With no projects the filter has no control, so nothing may stay hidden by it.
        if self.projects_cache.is_empty() {
            self.hidden_projects.clear();
        }
        if self.hidden_projects.len() != before {
            self.save_device();
        }
        // Failed sends kept for threads that are gone go with them.
        let unsent = &self.device.for_server(&self.server_key).unsent;
        let gone: Vec<String> = unsent
            .keys()
            .filter(|t| !self.rows.iter().any(|s| &s.thread.id == *t))
            .cloned()
            .collect();
        if !gone.is_empty() {
            for t in gone {
                self.device
                    .for_server_mut(&self.server_key)
                    .unsent
                    .remove(&t);
            }
            self.save_device();
        }
        // A thread asked for from the page is written: the page is free for the next one.
        if self
            .starting
            .as_ref()
            .and_then(|r| r.id.as_ref())
            .is_some_and(|id| self.rows.iter().any(|s| &s.thread.id == id))
        {
            self.starting = None;
            if let Some(page) = self.new_thread.clone() {
                page.update(cx, |p, cx| p.handed_off(window, cx));
            }
        }
        // Threads started in the background that the sidebar lists now.
        self.background.retain(|r| {
            !r.id
                .as_ref()
                .is_some_and(|id| self.rows.iter().any(|s| &s.thread.id == id))
        });
        if let Some(page) = &self.new_thread {
            let (environments, projects) = (self.environments.clone(), self.projects_cache.clone());
            let room = self.usage.as_ref().and_then(crate::new_thread::start_room);
            page.update(cx, |p, cx| {
                p.set_environments(environments, cx);
                p.set_projects(projects, cx);
                p.set_room(room, cx);
            });
        }
        self.push_sizes(cx);
        self.show_rows(cx);
    }

    /// The machine sizes boat's limits say the plan won't start, and why.
    fn sizes_unavailable(&self) -> Vec<(String, String)> {
        self.usage
            .as_ref()
            .map(|u| u.sizes_unavailable.clone())
            .unwrap_or_default()
    }

    /// The sizes the plan won't start, to New thread and an open project's settings.
    fn push_sizes(&self, cx: &mut Context<Self>) {
        let sizes = self.sizes_unavailable();
        if let Some(page) = &self.new_thread {
            let sizes = sizes.clone();
            page.update(cx, |p, cx| p.set_sizes_unavailable(sizes, cx));
        }
        if let Some(view) = self.project_view.as_ref().and_then(|v| v.upgrade()) {
            view.update(cx, |v, cx| v.set_sizes_unavailable(sizes, cx));
        }
    }

    /// The unread count on the app's icon, when notifications are on.
    fn show_badge(&mut self, cx: &mut Context<Self>) {
        let count = if self.device.notifications_on() {
            crate::notify::unread(&self.summaries, &self.parents)
        } else {
            0
        };
        crate::notify::badge(cx, count);
    }

    /// A banner was clicked: open its thread (or project), if it's this server's. Another
    /// server's only brings the window forward.
    pub(crate) fn open_notification(
        &mut self,
        tag: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((server, target)) = crate::notify::target(tag) else {
            return;
        };
        if server != self.server_key {
            return;
        }
        window.close_all_dialogs(cx);
        match target {
            crate::notify::Target::Thread(id) => {
                if self.summaries.iter().any(|s| s.thread.id == id) {
                    self.select(id, window, cx);
                }
            }
            crate::notify::Target::Project(id) => self.open_project(id, window, cx),
        }
        cx.notify();
    }

    /// Show the rows with what's expected over them, and the open thread with its row.
    fn show_rows(&mut self, cx: &mut Context<Self>) {
        self.summaries = self.rows.clone();
        // A thread just asked for, until the sidebar lists it.
        for req in self.starting.iter().chain(&self.background) {
            if !self
                .rows
                .iter()
                .any(|s| Some(&s.thread.id) == req.id.as_ref())
            {
                self.summaries.insert(0, placeholder_row(req));
            }
        }
        self.summaries
            .retain(|s| !self.expected.get(&s.thread.id).is_some_and(|e| e.deleting));
        #[cfg(feature = "snapshot")]
        self.apply_sim();
        for s in &mut self.summaries {
            if let Some(e) = self.expected.get(&s.thread.id) {
                e.apply_row(s);
            }
        }
        // A hold is for a row read or done with, until the pointer leaves: one archived,
        // deleted or at work again goes, so an Undo or a later finish doesn't bring it back.
        let summaries = &self.summaries;
        self.held.retain(|id, _| {
            summaries.iter().any(|s| {
                &s.thread.id == id && matches!(s.activity, Activity::Idle | Activity::Unread)
            })
        });
        self.hold_open_row();
        self.note_arrivals();
        // A thread Undo reopened is out of Archived (or gone): its row, where it landed.
        if let Some(id) = self.reveal_after_undo.clone() {
            match self
                .summaries
                .iter()
                .find(|s| s.thread.id == id)
                .map(|s| s.activity)
            {
                Some(Activity::Archived) => {}
                Some(_) => {
                    self.reveal_after_undo = None;
                    if self.selected() == Some(id.as_str()) {
                        self.reveal_row(&id);
                    }
                }
                None => self.reveal_after_undo = None,
            }
        }
        self.show_badge(cx);
        if let Some(sel) = self.selected().map(String::from)
            && self.engine.is_some()
            && !self.summaries.iter().any(|s| s.thread.id == sel)
        {
            // Deleted here, on another device, or from the CLI.
            self.close_thread(cx);
        }
        self.refresh_thread_view(cx);
        cx.notify();
    }

    /// Give the open thread's view what the engine side last sent for it, with its row and
    /// what's expected.
    fn refresh_thread_view(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        let (view, sel) = (open.view.clone(), &open.id);
        let summary = self.summaries.iter().find(|s| &s.thread.id == sel).cloned();
        // A thread just asked for: its page from what was asked, until its own view comes.
        // The older pages held are its own view's, never that page's.
        let asked = self
            .starting
            .as_ref()
            .filter(|r| r.id.as_ref() == Some(sel));
        let placeholder;
        let (d, paged) = match (
            open.detail
                .as_ref()
                .filter(|d| &d.id == sel && d.thread.is_some()),
            asked,
        ) {
            (Some(d), _) => (d, true),
            (None, Some(req)) => {
                placeholder = placeholder_detail(req);
                (&placeholder, false)
            }
            (None, None) => return,
        };
        // Messages sent that its view doesn't show yet.
        for p in &d.sent {
            self.shown_sends.insert(p.id.clone());
        }
        let working = summary
            .as_ref()
            .is_some_and(|s| s.activity == Activity::Working);
        // The older pages held, then the view's newest messages.
        let mut messages = if paged {
            open.history.messages()
        } else {
            d.messages.clone()
        };
        let older = if paged {
            (open.history.more_above(), open.history.loading())
        } else {
            (false, false)
        };
        let mut sent = d.sent.clone();
        // Another device's files have that device's paths: nothing to open here.
        for p in &mut sent {
            for f in &mut p.attachments {
                if !self.mine(f) {
                    f.path = PathBuf::new();
                }
            }
        }
        let asked = self.pending.values().filter_map(|r| match r {
            Request::Send {
                thread,
                text,
                files,
                local,
            } => Some((local, thread, text, files)),
            _ => None,
        });
        let uploading = self
            .uploading
            .iter()
            .map(|(local, u)| (local, &u.thread, &u.text, &u.files));
        for (local, thread, text, files) in asked.chain(uploading) {
            if thread == sel && !self.shown_sends.contains(local) {
                messages.push(flotilla_proto::Message {
                    seq: messages.last().map_or(0, |m| m.seq + 1),
                    task_id: Some(local.clone()),
                    kind: if working { "steer" } else { "prompt" }.into(),
                    text: Some(text.clone()),
                    status: Some("sending".into()),
                    event_key: Some(local.clone()),
                    timestamp: Some(flotilla_model::util::now_ms()),
                    attachments: files.iter().map(|f| f.name.clone()).collect(),
                    ..Default::default()
                });
                sent.push(flotilla_proto::SentPrompt {
                    id: local.clone(),
                    kind: "prompt".into(),
                    text: text.clone(),
                    sent_at: String::new(),
                    attachments: files.clone(),
                    schedule: None,
                });
            }
        }
        let mut thread = d.thread.clone();
        if let (Some(t), Some(e)) = (thread.as_mut(), self.expected.get(sel)) {
            e.apply(t);
        }
        let catalog = self.catalog.as_ref();
        // The model as the catalog names it ("Haiku 5.5").
        let model_label = thread
            .as_ref()
            .and_then(|t| model_label(catalog, &t.provider, &t.model));
        let children = d
            .children
            .iter()
            .map(|c| (c.id.clone(), child_info(c, catalog)))
            .collect();
        let project = d.project.clone().map(|p| (p.project, p.behind, p.into));
        let d = d.clone();
        let down = matches!(
            self.connection,
            Some(flotilla_proto::Connection::Offline { .. })
        );
        let limits = self.limits.clone();
        let schedules: Vec<flotilla_proto::Schedule> = self
            .schedules
            .iter()
            .filter(|s| s.target.thread() == Some(sel.as_str()))
            .cloned()
            .collect();
        view.update(cx, |v, cx| {
            v.set_server_down(down, cx);
            v.set_older(older.0, older.1, cx);
            v.set_start_error(d.start_error);
            v.set_project(project, d.stages);
            v.set_hooks(d.resume, d.archive);
            v.set_services(d.services);
            v.set_public(d.public);
            v.set_setup_failure(d.setup_failure);
            v.set_model_label(model_label);
            v.set_lineage(d.lineage);
            v.set_context(d.context, limits);
            v.set_sent(sent);
            v.set_schedules(schedules);
            v.set_family(d.parent, children);
            v.set_data(thread, messages, summary, cx);
        });
        if let Some(open) = &mut self.open
            && open.awaiting.take().is_some()
        {
            open.previous = None;
            cx.notify();
        }
    }

    /// How long a page of older messages that couldn't be read waits to be asked for again.
    const HISTORY_RETRY: std::time::Duration = std::time::Duration::from_secs(5);
    /// Pages of one thread fetched at once, at most (each a request on a pool thread).
    const FETCHES: usize = 4;

    /// Open `thread` at its message `seq` (a search hit): the pages down to it are fetched,
    /// and the thread view goes to it and marks it once it's there.
    pub(crate) fn open_at(
        &mut self,
        thread: String,
        seq: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &mut self.open {
            Some(open) if open.id == thread && open.detail.is_some() => {
                open.history.reach(seq);
                self.fetch_older(cx);
            }
            // Open, its view not here yet.
            Some(open) if open.id == thread => open.jump = Some(seq),
            _ => self.open_jumping(thread, self.sidebar_hovered, Some(seq), window, cx),
        }
        if let Some(view) = self.thread_view() {
            view.update(cx, |v, cx| v.jump_to(seq, window, cx));
        }
    }

    /// Fetch the older pages the open thread's history wants (the user scrolled up to them,
    /// a search hit is among them, or one held changed), off the main thread, a few at once.
    fn fetch_older(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.open.as_mut().filter(|o| o.detail.is_some()) else {
            return;
        };
        // Which open this is: one closed since, or opened again, has its own pages.
        let (thread, me) = (open.id.clone(), open.view.entity_id());
        let pages = open
            .history
            .wanted_up_to(Self::FETCHES.saturating_sub(open.fetching));
        open.fetching += pages.len();
        for page in pages {
            let (reader, id, life) = (self.backend.reader(), thread.clone(), self.life);
            let task = cx.background_spawn(async move {
                // The harness can slow pages down, as a far server would be.
                #[cfg(feature = "snapshot")]
                if let Some(ms) = std::env::var("FLOTILLA_SNAPSHOT_SLOW_HISTORY")
                    .ok()
                    .and_then(|m| m.parse().ok())
                {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                }
                reader.history(&id, flotilla_proto::History::before(page))
            });
            let id = thread.clone();
            cx.spawn(async move |this, cx| {
                let got = task.await;
                let _ = this.update(cx, |app, cx| {
                    if app.life != life {
                        return;
                    }
                    let Some(open) = app.open.as_mut().filter(|o| o.view.entity_id() == me) else {
                        return;
                    };
                    open.fetching = open.fetching.saturating_sub(1);
                    let idle =
                        |app: &FlotillaApp| app.open.as_ref().is_some_and(|o| o.fetching == 0);
                    match got {
                        Ok(p) => {
                            open.history.page(p);
                            app.fetch_older(cx);
                            // Shown once what's in flight is in: a jump far back fetches dozens
                            // of pages, and the rows are built once, not once a page.
                            if idle(app) {
                                app.refresh_thread_view(cx);
                            }
                        }
                        Err(e) => {
                            // Pages that came while this was out show now.
                            if idle(app) {
                                app.refresh_thread_view(cx);
                            }
                            eprintln!("[history] couldn't read page {page} of {id}: {e}");
                            // Asked for again in a while, not at once (offline, the server
                            // rebuilding): until then it still counts as asked.
                            cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(Self::HISTORY_RETRY).await;
                                let _ = this.update(cx, |app, cx| {
                                    if app.life == life
                                        && let Some(open) =
                                            app.open.as_mut().filter(|o| o.view.entity_id() == me)
                                    {
                                        open.history.failed(page);
                                        app.fetch_older(cx);
                                    }
                                });
                            })
                            .detach();
                        }
                    }
                });
            })
            .detach();
        }
        // "Loading earlier messages…" shows at once.
        if let Some(open) = &self.open {
            let older = (open.history.more_above(), open.history.loading());
            open.view
                .update(cx, |v, cx| v.set_older(older.0, older.1, cx));
        }
    }

    /// Fetch a cut tool output's strings whole, for the open thread's view.
    fn fetch_whole(
        &mut self,
        key: String,
        seq: i64,
        cuts: Vec<flotilla_proto::Cut>,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.selected().map(String::from) else {
            return;
        };
        let (reader, life) = (self.backend.reader(), self.life);
        let task = cx.background_spawn(async move {
            cuts.iter()
                .map(|c| {
                    reader
                        .full(&thread, seq, c)
                        .map(|text| (c.field.clone(), text))
                })
                .collect::<Result<Vec<_>, String>>()
        });
        cx.spawn(async move |this, cx| {
            let got = task.await;
            let _ = this.update(cx, |app, cx| {
                if app.life != life {
                    return;
                }
                if let Some(view) = app.thread_view() {
                    view.update(cx, |v, cx| v.set_whole(&key, got, cx));
                }
            });
        })
        .detach();
    }

    /// The open thread's id.
    pub fn selected(&self) -> Option<&str> {
        self.open.as_ref().map(|o| o.id.as_str())
    }

    /// The open thread's page.
    fn thread_view(&self) -> Option<&Entity<ThreadView>> {
        self.open.as_ref().map(|o| &o.view)
    }

    /// No thread open: the one way there. Its subscriptions go (else the server would go on
    /// reading it as on screen: polled, marked read, never Unread), and so does its page,
    /// keeping the draft.
    fn close_thread(&mut self, cx: &mut Context<Self>) {
        if let Some(old) = self.open.take() {
            self.let_go(old, cx);
        }
    }

    /// A thread no longer open: unsubscribed from, and what was being written in it (and
    /// where it was scrolled to) kept for when it opens again. Its page, to show until the
    /// next one's is ready.
    fn let_go(&mut self, old: Open, cx: &mut Context<Self>) -> Entity<ThreadView> {
        self.backend.send(ClientMsg::Unsub {
            view: ViewKey::Thread(old.id.clone()),
        });
        self.backend.send(ClientMsg::Unsub {
            view: ViewKey::Mentions(old.id),
        });
        self.keep_draft(&old.view, cx);
        old.view
    }

    /// Offer to undo an archive for a few seconds; archives in that time join the same notice.
    /// `by_you`: you archived it (⌘Z undoes it), rather than the server by itself.
    fn note_archived(&mut self, id: String, by_you: bool, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        let open = (by_you && self.selected() == Some(id.as_str())).then(|| id.clone());
        match &mut self.undo {
            Some(u) if now.duration_since(u.at) < UNDO_FOR => {
                if !u.ids.contains(&id) {
                    u.ids.push(id);
                }
                u.at = now;
                u.owns_cmd_z |= by_you;
                // The first of them that was open (⇧⌘S twice: where you started).
                u.open = u.open.take().or(open);
            }
            _ => {
                self.undo = Some(UndoNotice {
                    ids: vec![id],
                    at: now,
                    owns_cmd_z: by_you,
                    open,
                })
            }
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_FOR).await;
            let _ = this.update(cx, |app, cx| {
                if app
                    .undo
                    .as_ref()
                    .is_some_and(|u| u.at.elapsed() >= UNDO_FOR)
                {
                    app.undo = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// ⌘1–⌘9: the nth thread as the sidebar lists it.
    fn open_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = crate::sidebar::visible_order(self).into_iter().nth(n) {
            self.open(id, true, window, cx);
        }
    }

    /// ⌘⇧] / ⌘⇧[: the next or previous thread in the sidebar, wrapping around; from the
    /// new-thread page, the first or last.
    fn step_thread(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let order = crate::sidebar::visible_order(self);
        if order.is_empty() {
            return;
        }
        let len = order.len() as isize;
        // A thread listed inside another (handed its task by it) is where that one's row is.
        let at = self.selected().map(|s| self.listing().row_for(s));
        let next = match at.and_then(|s| order.iter().position(|id| *id == s)) {
            Some(i) => (i as isize + by).rem_euclid(len),
            None if by > 0 => 0,
            None => len - 1,
        };
        // Stepping through the list reads threads without moving them, as hovering it does,
        // so the next step goes where the list showed.
        self.open(order[next as usize].clone(), true, window, cx);
    }

    /// ⌘⇧S: archive the open thread and open the next one that needs you: the first unread,
    /// else the next row in the sidebar (else the one before it), else New thread.
    pub(crate) fn archive_and_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected().map(String::from) else {
            return;
        };
        let rows: Vec<(String, Activity)> = crate::sidebar::visible_rows(self);
        let next = crate::sidebar::next_after_archive(&rows, &id);
        let archivable = self.summaries.iter().any(|s| {
            s.thread.id == id && !matches!(s.activity, Activity::Archived | Activity::Expired)
        });
        if archivable {
            self.archive(id, cx);
        }
        // Archiving is a move you asked for: rows held in place go where they belong.
        self.held.clear();
        match next {
            Some(next) => self.open(next, false, window, cx),
            None => {
                self.open_new_thread(window, cx);
            }
        }
    }

    fn undo_owns_cmd_z(&self) -> bool {
        self.undo
            .as_ref()
            .is_some_and(|u| u.owns_cmd_z && u.at.elapsed() < UNDO_FOR)
    }

    /// Put a thread archived by itself back (the merged notice's Undo).
    fn unarchive(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(u) = &mut self.undo {
            u.ids.retain(|i| *i != id);
            if u.ids.is_empty() {
                self.undo = None;
            }
        }
        self.ask(Request::Unarchive(id));
        cx.notify();
    }

    /// Put the threads from the last archive back, and open again the one that was open when
    /// they were archived. Their VMs stay stopped until the next send.
    pub fn undo_archive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(u) = self.undo.take() else { return };
        for id in &u.ids {
            self.ask(Request::Unarchive(id.clone()));
        }
        if let Some(id) = u.open {
            // Coming back out of Archived: Archived needn't open for it, and its row is
            // revealed where it lands, once the sidebar has it back.
            let folded = self.collapsed.clone();
            let back = u.ids.contains(&id);
            self.select(id.clone(), window, cx);
            if back {
                if self.collapsed != folded {
                    self.collapsed = folded;
                    self.save_device();
                }
                self.reveal = false;
                self.reveal_after_undo = Some(id);
            }
        }
        cx.notify();
    }

    /// Dev: the harness's rows in another state (`step:sim:`).
    #[cfg(feature = "snapshot")]
    fn apply_sim(&mut self) {
        // A turn's state only: an archived row stays archived.
        for s in self
            .summaries
            .iter_mut()
            .filter(|s| !matches!(s.activity, Activity::Archived | Activity::Expired))
        {
            if let Some((activity, started, text)) = self.sim.get(&s.thread.id) {
                s.activity = *activity;
                s.turn_started_ms = *started;
                match activity {
                    Activity::Working => s.last_prompt = text.clone().or(s.last_prompt.take()),
                    _ => s.last_reply = text.clone().or(s.last_reply.take()),
                }
            }
        }
    }

    /// A thread being opened: its row in view, its group open if it was folded (an archived
    /// thread opened from the palette, say), even if it's the one open already.
    fn reveal_row(&mut self, id: &str) {
        self.reveal = true;
        let listing = self.listing();
        let group = listing.shown_group(&listing.row_for(id));
        if let Some(group) = group
            && self.collapsed.remove(&group)
        {
            self.save_device();
        }
    }

    /// The pointer left the sidebar: rows held in place go where they belong, sliding in.
    pub(crate) fn release_held(&mut self, cx: &mut Context<Self>) {
        if !self.held.is_empty() {
            self.held.clear();
            self.note_arrivals();
            cx.notify();
        }
    }

    /// The open thread's turn ended (it left Working) under the pointer: its row stays under
    /// Working until the pointer leaves the sidebar, so it doesn't jump away from it.
    fn hold_open_row(&mut self) {
        let Some(sel) = self.selected().filter(|_| self.sidebar_hovered) else {
            return;
        };
        let listing = self.listing();
        let id = listing.row_for(sel);
        let Some(s) = listing.summary(&id) else {
            return;
        };
        let left_working = self.groups.get(&id) == Some(&Activity::Working)
            && listing.shown_group(&id) != Some(Activity::Working);
        if left_working && matches!(s.activity, Activity::Idle | Activity::Unread) {
            self.held.insert(id, Activity::Working);
        }
    }

    /// Record threads whose sidebar group changed since the last reload, or that are new.
    /// Nothing counts as arriving on the first load.
    fn note_arrivals(&mut self) {
        let now = std::time::Instant::now();
        let first = self.groups.is_empty();
        let groups = self.listing().groups();
        for (id, group) in &groups {
            if !first && self.groups.get(id) != Some(group) {
                self.arrival_seq += 1;
                self.arrivals.insert(id.clone(), (self.arrival_seq, now));
            }
        }
        self.arrivals
            .retain(|_, (_, at)| now.duration_since(*at) < crate::sidebar::ARRIVAL * 4);
        self.groups = groups;
    }

    /// Open a thread: from its row (under the pointer, rows stay where they are while it's
    /// on the sidebar), a card, the palette, a notification.
    pub fn select(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let hold = self.sidebar_hovered;
        self.open(id, hold, window, cx);
    }

    /// Open a thread. `hold`: rows stay where they are (the pointer is on the sidebar, or the
    /// keyboard steps through it), so a thread opened unread stays under Unread; otherwise
    /// rows held so far go where they belong.
    fn open(&mut self, id: String, hold: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_jumping(id, hold, None, window, cx);
    }

    /// Open a thread, at message `jump` (a search hit) if one's given.
    fn open_jumping(
        &mut self,
        id: String,
        hold: bool,
        jump: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let again = self.selected() == Some(id.as_str());
        self.reveal_after_undo = None;
        // Rows settle first, so what's revealed is where the row will be.
        if !hold && !self.held.is_empty() {
            self.held.clear();
            self.note_arrivals();
        }
        if hold
            && !again
            && self
                .summaries
                .iter()
                .any(|s| s.thread.id == id && s.activity == Activity::Unread)
        {
            self.held.insert(id.clone(), Activity::Unread);
        }
        self.reveal_row(&id);
        // Opened (the open thread clicked again isn't): search ranks by how often and recently,
        // and remembers what it was picked for.
        if !again {
            let query = self.open_query.take();
            self.ask(Request::Opened {
                thread: id.clone(),
                query,
            });
        }
        if again {
            crate::notify::read(cx, &self.server_key, &id);
            // Clicking the open thread after "Mark as unread" reads it.
            if self
                .open
                .as_mut()
                .is_some_and(|o| std::mem::take(&mut o.keep_unread))
            {
                self.ask(Request::MarkViewed(id));
            }
            cx.notify();
            return;
        }
        let view = cx.new(|cx| ThreadView::new(id.clone(), window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ThreadViewEvent, window, cx| match event {
                ThreadViewEvent::Send {
                    thread,
                    text,
                    files,
                } => this.send_message(thread.clone(), text.clone(), files.clone(), window, cx),
                ThreadViewEvent::Answer {
                    thread,
                    ask,
                    answers,
                } => {
                    crate::notify::moment(cx, this.device.notifications_on());
                    let local = format!(
                        "{}{}",
                        flotilla_model::lifecycle::LOCAL_PREFIX,
                        uuid::Uuid::new_v4()
                    );
                    this.ask(Request::Answer {
                        thread: thread.clone(),
                        ask: ask.clone(),
                        answers: answers.clone(),
                        local,
                    });
                }
                ThreadViewEvent::Act(req) => this.perform(req.clone(), cx),
                ThreadViewEvent::Command(id, cmd) => this.run_command(id.clone(), *cmd, window, cx),
                ThreadViewEvent::OnBranch { thread, prompt } => {
                    this.new_thread_on_branch(thread, Some(prompt.clone()))
                }
                ThreadViewEvent::Services(cmd) => {
                    this.ask(Request::Services(cmd.clone()));
                }
                ThreadViewEvent::ReadMentions(id) => {
                    this.ask(Request::ReadMentions(id.clone()));
                }
                ThreadViewEvent::Open(id) => this.select(id.clone(), window, cx),
                ThreadViewEvent::Older => {
                    if this.open.as_mut().is_some_and(|o| o.history.more()) {
                        this.fetch_older(cx);
                    }
                }
                ThreadViewEvent::Whole { key, seq, cuts } => {
                    this.fetch_whole(key.clone(), *seq, cuts.clone(), cx)
                }
                ThreadViewEvent::Later { thread, text } => {
                    let to = this
                        .summaries
                        .iter()
                        .find(|s| &s.thread.id == thread)
                        .map(|s| s.thread.title.clone())
                        .unwrap_or_default();
                    let seed = Seed {
                        id: None,
                        prompt: text.clone(),
                        targets: vec![flotilla_proto::Target::Thread(thread.clone())],
                        to,
                        when: None,
                        zone: flotilla_model::schedule::here(),
                    };
                    this.open_schedule_editor(seed, Later::Thread(thread.clone()), window, cx);
                }
                ThreadViewEvent::Scheduled(id) => this.open_scheduled(Some(id.clone()), window, cx),
                // A message boat never got: gone either way; Send again sends its text afresh.
                ThreadViewEvent::Unsent {
                    thread,
                    id,
                    text,
                    again,
                } => {
                    let mut files = this
                        .open
                        .as_ref()
                        .and_then(|o| o.detail.as_ref())
                        .filter(|d| &d.id == thread)
                        .and_then(|d| d.sent.iter().find(|s| s.id == *id))
                        .map(|s| s.attachments.clone())
                        .unwrap_or_default();
                    // Sent again from here, they'd count as this Mac's: only its own keep a path.
                    for f in &mut files {
                        if !this.mine(f) {
                            f.path = PathBuf::new();
                        }
                    }
                    this.ask(Request::DiscardUnsent {
                        thread: thread.clone(),
                        id: id.clone(),
                    });
                    if *again {
                        this.send_message(thread.clone(), text.clone(), files, window, cx);
                    }
                }
            },
        );
        self._subscriptions.push(sub);
        // What's on screen is subscribed to, so the engine side reads it as things arrive and
        // polls it every second. The thread open before is let go of, but its page stays until
        // this one's is ready.
        let previous = self.open.take().map(|old| self.let_go(old, cx));
        self.backend.send(ClientMsg::Sub {
            view: ViewKey::Thread(id.clone()),
        });
        self.backend.send(ClientMsg::Sub {
            view: ViewKey::Mentions(id.clone()),
        });
        crate::notify::read(cx, &self.server_key, &id);
        // Back to a thread you'd scrolled up in (not to a search hit): where you were.
        let jump = jump.or_else(|| {
            let seq = *self.places.get(&id)?;
            view.update(cx, |v, cx| v.return_to(seq, cx));
            Some(seq)
        });
        self.open = Some(Open {
            id: id.clone(),
            view: view.clone(),
            detail: None,
            history: flotilla_proto::History::new(),
            fetching: 0,
            jump,
            keep_unread: false,
            awaiting: Some(std::time::Instant::now()),
            previous,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOLD_FOR).await;
            let _ = this.update(cx, |app, cx| {
                if let Some(open) = &mut app.open
                    && open.awaiting.is_some_and(|t| t.elapsed() >= HOLD_FOR)
                {
                    open.awaiting = None;
                    open.previous = None;
                    cx.notify();
                }
            });
        })
        .detach();
        // What you were writing when you last left it.
        if let Some(draft) = self.drafts.take(&id) {
            view.update(cx, |v, cx| v.put_draft(draft, window, cx));
        }
        // A message whose send failed after you'd moved on: back in this composer with its
        // files, and only then let go of.
        if let Some(kept) = self
            .device
            .for_server(&self.server_key)
            .unsent
            .get(&id)
            .cloned()
            && view.update(cx, |v, cx| {
                v.restore_draft(&kept.text, &kept.files, window, cx)
            })
        {
            self.device
                .for_server_mut(&self.server_key)
                .unsent
                .remove(&id);
            self.save_device();
        }
        // A thread just asked for shows at once, from what was asked.
        self.refresh_thread_view(cx);
        cx.notify();
    }

    fn on_note(&mut self, note: Note, window: &mut Window, cx: &mut Context<Self>) {
        match note {
            // A scheduled prompt that didn't run: said here (a banner when away, by the alert).
            Note::ScheduleFailed {
                schedule,
                prompt,
                why,
                ..
            } => {
                let app = cx.weak_entity();
                let note = Notification::warning(why)
                    .title(format!(
                        "Didn't run: {}",
                        flotilla_model::util::ellipsize(
                            &flotilla_model::util::one_line(&prompt),
                            60
                        )
                    ))
                    .action(move |_, _, cx| {
                        let (app, id, note) =
                            (app.clone(), schedule.clone(), cx.entity().downgrade());
                        Button::new("open-schedule").label("Scheduled").on_click(
                            move |_, window, cx| {
                                let _ = app.update(cx, |app, cx| {
                                    app.open_scheduled(Some(id.clone()), window, cx)
                                });
                                let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                            },
                        )
                    });
                window.push_notification(note, cx);
            }
            // Something the engine started itself (a delegate's start, a message to a thread).
            Note::Done { request, result } => self.on_engine_done(&request, &result),
            Note::Summaries(result) => {
                let state = match result {
                    Ok(at) => SummaryState::Working(at),
                    Err(e) => SummaryState::Failed(e),
                };
                self.set_summary_state(state, cx);
            }
            Note::TurnFinished(id) => self.thread_toast(id, "Thread finished", window, cx),
            // In place of its "Thread finished".
            Note::Asked(id) => self.thread_toast(id, "Thread asks", window, cx),
            Note::GitHub(status) => {
                self.gh_status = status.clone();
                if let Some(view) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    view.update(cx, |v, cx| v.set_gh(status.clone(), cx));
                }
                if let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
                    view.update(cx, |v, cx| v.set_gh_status(status, cx));
                }
                cx.notify();
            }
            Note::Repos {
                result,
                done,
                login,
            } => {
                if let Some(view) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    view.update(cx, |v, cx| v.set_repos(result, done, login.as_deref(), cx));
                }
            }
            Note::SetupDone {
                project,
                name,
                ok,
                error,
                recovered,
                fixable,
            } => self.on_setup_done(project, name, ok, error, recovered, fixable, window, cx),
            // Messages still sending when Flotilla quit that boat never got: marked "not sent"
            // where they were, with Send again and Discard.
            Note::Unsent(threads) => {
                for thread in threads {
                    let title = self
                        .summaries
                        .iter()
                        .find(|s| s.thread.id == thread)
                        .map(|s| s.thread.title.clone())
                        .unwrap_or_default();
                    let app = cx.weak_entity();
                    let note = Notification::warning("Send it again from the thread.")
                        .title(format!("Not sent: {title}"))
                        .action(move |_, _, cx| {
                            let (app, id, note) =
                                (app.clone(), thread.clone(), cx.entity().downgrade());
                            Button::new("open-unsent").label("Open").on_click(
                                move |_, window, cx| {
                                    let _ = app
                                        .update(cx, |app, cx| app.select(id.clone(), window, cx));
                                    let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                                },
                            )
                        });
                    window.push_notification(note, cx);
                }
            }
            // Archived for it (Settings → Archiving): the same notice says so, with Undo.
            Note::PrMerged {
                thread,
                number,
                title,
                branch,
                archived,
            } => {
                let app = cx.weak_entity();
                let note = Notification::new()
                    .icon(gpui_kit::component::Icon::new(
                        gpui_kit::assets::IconName::GitMerge,
                    ))
                    .title(if archived {
                        format!("#{number} merged into {branch} · Archived")
                    } else {
                        format!("#{number} merged into {branch}")
                    })
                    .message(title)
                    .action(move |_, _, cx| {
                        let (app, id, note) =
                            (app.clone(), thread.clone(), cx.entity().downgrade());
                        if archived {
                            Button::new("undo-merged").label("Undo").on_click(
                                move |_, window, cx| {
                                    let _ = app.update(cx, |app, cx| app.unarchive(id.clone(), cx));
                                    let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                                },
                            )
                        } else {
                            Button::new("open-merged").label("Open").on_click(
                                move |_, window, cx| {
                                    let _ = app
                                        .update(cx, |app, cx| app.select(id.clone(), window, cx));
                                    let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                                },
                            )
                        }
                    })
                    .autohide(true);
                window.push_notification(note, cx);
            }
            // As an archive of yours shows: the sidebar's "Archived N threads · Undo".
            Note::IdleArchived(threads) => {
                for id in threads {
                    self.note_archived(id, false, cx);
                }
                cx.notify();
            }
            Note::Branches { project, result } => {
                let in_use = self.branches_in_use(&project);
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| {
                        p.set_branches(&project, result, in_use, window, cx)
                    });
                }
            }
            Note::BranchChecked {
                project,
                branch,
                prompt,
                result,
            } => match result {
                Ok(()) => {
                    window.close_all_dialogs(cx);
                    if let Some(page) = self.open_new_thread(window, cx) {
                        page.update(cx, |p, cx| {
                            p.start_on_branch(&project, branch, prompt.as_deref(), window, cx)
                        });
                    }
                }
                Err(e) => window.push_notification(
                    Notification::warning(e).title("Can't start a thread on that branch"),
                    cx,
                ),
            },
            Note::Suggested { repo, result } => {
                if let Some(view) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    view.update(cx, |v, cx| v.set_suggestions(&repo, result, window, cx));
                }
            }
            Note::Alert { id, alert } => {
                crate::notify::alert(
                    cx,
                    &self.server_key,
                    &id,
                    &alert,
                    self.device.notifications_on(),
                    window.is_window_active(),
                );
            }
            Note::Rebuilt(result) => {
                self.rebuilding = false;
                let note = match result {
                    Ok(bak) => Notification::success(format!("The old database is at {bak}"))
                        .title("Rebuilt from boat"),
                    Err(e) => Notification::error(e).title("Rebuild failed"),
                };
                window.push_notification(note, cx);
                cx.notify();
            }
        }
    }

    /// A setup run ended. Only a failure, and the run that fixes it, get a toast.
    #[allow(clippy::too_many_arguments)]
    fn on_setup_done(
        &mut self,
        project: String,
        name: String,
        ok: bool,
        error: Option<String>,
        recovered: bool,
        fixable: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ok && !recovered {
            return;
        }
        let id = SharedString::from(project.clone());
        let note = if ok {
            Notification::success(format!("{name}'s setup passes again"))
                .id1::<SetupNote>(id)
                .autohide(true)
        } else {
            let app = cx.weak_entity();
            // A script failed, so a code change can fix it: offer that (the project's page
            // has the log). Otherwise (boat trouble, say) the page is the place to look.
            Notification::error(error.unwrap_or_else(|| "setup failed".into()))
                .id1::<SetupNote>(id)
                .title(format!("Setup failed: {name}"))
                .action(move |_, _, cx| {
                    let (app, project, note) =
                        (app.clone(), project.clone(), cx.entity().downgrade());
                    match fixable.clone() {
                        Some(run) => Button::new("fix-setup")
                            .icon(gpui_kit::assets::IconName::Sparkles)
                            .label("Fix in a new thread")
                            .on_click(move |_, window, cx| {
                                let _ = app.update(cx, |app, _| {
                                    app.ask(Request::SetupFixPrompt {
                                        project: project.clone(),
                                        run: run.clone(),
                                    })
                                });
                                let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                            }),
                        None => Button::new("view-setup").label("View").on_click(
                            move |_, window, cx| {
                                let _ = app.update(cx, |app, cx| {
                                    app.open_project(project.clone(), window, cx)
                                });
                                let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                            },
                        ),
                    }
                })
        };
        window.push_notification(note, cx);
    }

    /// A thread that isn't open finished a turn: say so, with a way to open it.
    /// (`what`: "Thread finished", or "Thread asks" when its agent asked you something; one
    /// toast a thread, the newer in place of the older.)
    fn thread_toast(
        &mut self,
        id: String,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected() == Some(id.as_str()) {
            return;
        }
        let Some(title) = self
            .summaries
            .iter()
            .find(|s| s.thread.id == id)
            .map(|s| s.thread.title.clone())
        else {
            return;
        };
        let app = cx.weak_entity();
        let note = Notification::new()
            .id1::<FinishedNote>(SharedString::from(id.clone()))
            .title(what)
            .message(title)
            .action(move |_, _, cx| {
                let (app, id, note) = (app.clone(), id.clone(), cx.entity().downgrade());
                Button::new("open-finished")
                    .primary()
                    .label("Open")
                    .on_click(move |_, window, cx| {
                        let _ = app.update(cx, |app, cx| app.select(id.clone(), window, cx));
                        let _ = note.update(cx, |n, cx| n.dismiss(window, cx));
                    })
            })
            // An action normally keeps a toast up; this one can go by itself, as in T3 Code.
            .autohide(true);
        window.push_notification(note, cx);
    }

    /// Something the engine started itself ended: a delegate's start, a message it sent a
    /// thread (a wake, agents' answers, word that a delegate couldn't start), a parent stopping
    /// its delegate, an archive on merge. Nobody here asked for it, so nothing here waits on it
    /// or answers for it: not a composer, a toast, or what's shown over a row for your own
    /// requests. Its views came first, and a start that failed is the server's alert (and its
    /// parent's message). A message the engine couldn't send is never yours to send again.
    fn on_engine_done(&self, req: &Request, result: &Result<Reply, String>) {
        if let Err(e) = result
            && std::env::var_os("FLOTILLA_LOG").is_some()
        {
            eprintln!("[flotilla] the engine couldn't {}: {e}", req.verb());
        }
    }

    /// Your own request was answered: one this window sent, or one that failed before it went
    /// (its files didn't go up).
    fn on_answer(
        &mut self,
        req: Request,
        result: Result<Reply, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // An answer to an agent's question that went: its draft goes (one that didn't comes
        // back, below).
        if let (Request::Answer { ask, .. }, Ok(_)) = (&req, &result)
            && req.thread() == self.selected()
            && let Some(view) = self.thread_view()
        {
            view.update(cx, |v, cx| v.answer_sent(ask, cx));
        }
        // Send later's form waits for the server to keep every one it saved (one per model):
        // closed once it has, or it says why not (those kept stay kept; saved again, they're
        // the same ones).
        if let Request::PutSchedule(spec) = &req
            && let Some(form) = self.schedule_editor.as_mut()
            && form.waiting.get(&spec.id) == Some(spec.as_ref())
        {
            form.waiting.remove(&spec.id);
            if let Err(e) = &result {
                form.refused.get_or_insert_with(|| e.clone());
            }
            if !form.waiting.is_empty() {
                return;
            }
            let Some(editor) = form.view.upgrade() else {
                self.schedule_editor = None;
                return;
            };
            match form.refused.take() {
                None => {
                    let origin = form.origin.clone();
                    self.schedule_editor = None;
                    window.close_dialog(cx);
                    match origin {
                        Later::Thread(thread) => {
                            if let Some(view) = self
                                .thread_view()
                                .cloned()
                                .filter(|v| v.read(cx).id() == thread)
                            {
                                view.update(cx, |v, cx| v.scheduled_sent(&spec.prompt, window, cx));
                            }
                        }
                        Later::NewThread => {
                            if let Some(page) = &self.new_thread {
                                page.update(cx, |p, cx| p.scheduled(window, cx));
                            }
                        }
                        Later::Edit => {}
                    }
                }
                Some(e) => editor.update(cx, |ed, cx| ed.refused(e, cx)),
            }
            return;
        }
        if let Some(id) = req.thread()
            && let Some(e) = self.expected.get_mut(id)
        {
            match &req {
                Request::Rename { .. } => e.title = None,
                Request::KeepRunning { .. } => e.keep_running = None,
                Request::Stop(_) | Request::Archive(_) | Request::Resume(_) | Request::Retry(_) => {
                    e.state = None
                }
                Request::Delete(_) => e.deleting = false,
                _ => {}
            }
            if e.is_empty() {
                self.expected.remove(id);
            }
        }
        if matches!(req, Request::AddProject { .. } | Request::RemoveProject(_)) {
            self.project_done(&req, result, window, cx);
            return;
        }
        // What the palette asked for: it says if it failed.
        if let Request::Search { query, .. } = &req {
            if let Some(palette) = self.palette.as_ref().and_then(|p| p.upgrade()) {
                let hits = result.map(|r| match r {
                    Reply::Hits(hits) => hits,
                    _ => vec![],
                });
                palette.update(cx, |p, cx| p.set_hits(query, hits, window, cx));
            }
            return;
        }
        if matches!(req, Request::PairCode { .. })
            && let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade())
        {
            view.update(cx, |v, cx| v.device_added(cx));
        }
        if let (Request::PutSkill { skill, .. }, Err(_)) = (&req, &result)
            && let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade())
        {
            view.update(cx, |v, cx| v.skill_failed(&skill.name, cx));
        }
        // A key that wasn't saved starts no new checks.
        if matches!(req, Request::SetBoatKey(_)) && result.is_err() {
            self.setup_checking = None;
        }
        if matches!(req, Request::RunDoctor) {
            self.setup_checking = None;
            if let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
                view.update(cx, |v, cx| v.doctor_done(cx));
            }
        }
        // The popover says why a Refresh failed.
        if matches!(req, Request::ReadUsage) {
            self.usage_refreshing = false;
            self.usage_error = result.as_ref().err().cloned();
            cx.notify();
        }
        if matches!(req, Request::ReadAgentUsage) {
            self.agents_refreshing = false;
            self.agents_error = result.as_ref().err().cloned();
            cx.notify();
        }
        let row = req
            .thread()
            .and_then(|id| self.rows.iter().find(|s| s.thread.id == id))
            .cloned();
        match result {
            // Started in the background and refused before the thread was written: say so, and
            // put the prompt back on the page.
            Err(e)
                if matches!(req, Request::NewThread(_))
                    && row.is_none()
                    && self
                        .background
                        .iter()
                        .any(|r| r.id.as_deref() == req.thread()) =>
            {
                self.background.retain(|r| r.id.as_deref() != req.thread());
                if let Request::NewThread(new) = &req {
                    let model = model_label(self.catalog.as_ref(), &new.provider, &new.model)
                        .unwrap_or_else(|| new.model.clone());
                    window.push_notification(
                        Notification::error(e).title(format!("Couldn't start on {model}")),
                        cx,
                    );
                    if let Some(page) = &self.new_thread {
                        page.update(cx, |p, cx| {
                            p.put_back(&new.prompt, &new.attachments, window, cx)
                        });
                    }
                }
            }
            // Refused before the thread was written: back to the page, which has the draft and
            // says why.
            Err(e)
                if matches!(req, Request::NewThread(_))
                    && row.is_none()
                    && self.starting.as_ref().and_then(|r| r.id.as_deref()) == req.thread() =>
            {
                self.starting = None;
                self.close_thread(cx);
                if let Some(page) = &self.new_thread {
                    page.update(cx, |p, cx| p.failed(e, cx));
                }
            }
            // A failed start keeps the thread open: it says why, with Try again and Discard,
            // and keeps the first prompt unsent (no VM, its repo not prepared, or boat refused
            // the prompt). Otherwise the prompt goes back in the composer. The page's draft
            // lives on in the thread either way.
            Err(e) if matches!(req, Request::NewThread(_) | Request::Retry(_)) => {
                let unprepared = row.as_ref().is_some_and(|s| s.start_failed);
                // The thread says why itself; elsewhere, say which thread didn't start, and on
                // which model (one prompt started on several gives them one title).
                if let Some(s) = row
                    .as_ref()
                    .filter(|s| unprepared && self.selected() != Some(s.thread.id.as_str()))
                {
                    let model =
                        model_label(self.catalog.as_ref(), &s.thread.provider, &s.thread.model)
                            .unwrap_or_else(|| s.thread.model.clone());
                    window.push_notification(
                        Notification::error(e.clone())
                            .title(format!("Couldn't start {} on {model}", s.thread.title)),
                        cx,
                    );
                }
                match (&row, &req) {
                    (Some(s), Request::NewThread(new))
                        if s.thread.sandbox_id.is_some() && !unprepared =>
                    {
                        if self.selected() == Some(s.thread.id.as_str())
                            && let Some(view) = self.thread_view()
                        {
                            view.update(cx, |v, cx| {
                                v.restore_draft(&new.prompt, &new.attachments, window, cx)
                            });
                        }
                        window.push_notification(
                            Notification::error(e).title("Couldn't send the first prompt"),
                            cx,
                        );
                    }
                    (None, _) => window.push_notification(
                        Notification::error(e).title(format!("Couldn't {}", req.verb())),
                        cx,
                    ),
                    _ => {}
                }
            }
            Err(e) => {
                if let Some(view) = self.thread_view()
                    && req.thread().is_some()
                    && req.thread() == self.selected()
                {
                    match &req {
                        // Something new is in the composer: the text waits for it to be empty.
                        Request::Send {
                            thread,
                            text,
                            files,
                            ..
                        } => {
                            if !view.update(cx, |v, cx| v.restore_draft(text, files, window, cx)) {
                                self.device
                                    .for_server_mut(&self.server_key)
                                    .keep_unsent(thread, text, files);
                                self.save_device();
                            }
                        }
                        Request::Interrupt(_) => view.update(cx, |v, cx| v.interrupt_failed(cx)),
                        Request::Answer { ask, .. } => {
                            view.update(cx, |v, cx| v.answer_failed(ask, cx))
                        }
                        _ => {}
                    }
                } else if let Request::Send {
                    thread,
                    text,
                    files,
                    ..
                } = &req
                {
                    // You've moved on: the text and files wait for that thread's composer.
                    self.device
                        .for_server_mut(&self.server_key)
                        .keep_unsent(thread, text, files);
                    self.save_device();
                }
                match &req {
                    Request::SetScripts { .. } => window.push_notification(
                        Notification::error(e).title("Couldn't save the scripts"),
                        cx,
                    ),
                    Request::SetMachineType { .. } => window.push_notification(
                        Notification::error(e).title("Couldn't save the machine size"),
                        cx,
                    ),
                    Request::SetBoatKey(_) => window.push_notification(
                        Notification::error(e).title("Couldn't save the key"),
                        cx,
                    ),
                    // Background reads and settings were never reported one by one.
                    r if quiet(r) => eprintln!("[flotilla] couldn't {}: {e}", r.verb()),
                    _ => window.push_notification(
                        Notification::error(e).title(format!("Couldn't {}", req.verb())),
                        cx,
                    ),
                }
            }
            // Started from the page by the engine's own path, which writes the thread later.
            Ok(Reply::Created(id)) if matches!(req, Request::NewThread(_)) => {
                self.background
                    .retain(|r| r.id.as_deref() != Some(id.as_str()));
                if self.starting.as_ref().and_then(|r| r.id.as_deref()) == Some(id.as_str()) {
                    self.starting = None;
                    if let Some(page) = self.new_thread.clone() {
                        page.update(cx, |p, cx| p.handed_off(window, cx));
                    }
                }
            }
            // Made public: the link to share, copied.
            Ok(Reply::Url(url)) if matches!(req, Request::MakePublic { .. }) => {
                if let Request::MakePublic { port, for_, .. } = &req {
                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                    let note = Notification::success(format!(
                        "Port {port} is public {}. Link copied.",
                        for_.words()
                    ))
                    .title("Public link");
                    window.push_notification(note, cx);
                }
            }
            Ok(Reply::Url(url)) => cx.open_url(&url),
            // Over Settings: the code, its link and a QR code, for the other device.
            Ok(Reply::Code(code)) => {
                let body = cx.new(|_| crate::pairing::AddDeviceView::new(code));
                window.open_dialog(cx, move |dialog, _, _| {
                    dialog
                        .title("Add a device")
                        .width(px(420.))
                        .child(body.clone())
                });
            }
            Ok(Reply::Prompt(prompt)) => self.show_prompt(&req, &prompt, window, cx),
            Ok(Reply::Sent(Sent::Steer(_))) => {
                window.push_notification(Notification::success("Steered"), cx);
            }
            Ok(Reply::Mentions(m)) => {
                if let (Request::ProjectMentions(project), Some(page)) = (&req, &self.new_thread) {
                    page.update(cx, |p, cx| {
                        p.set_mentions(project.clone(), m.map(std::sync::Arc::new), cx)
                    });
                }
            }
            Ok(Reply::Repos(repos)) => {
                if let Some(view) = self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    view.update(cx, |v, cx| v.set_cached(repos, cx));
                }
            }
            Ok(_) => match &req {
                Request::Delete(id) if self.selected() == Some(id.as_str()) => {
                    self.close_thread(cx)
                }
                Request::SetScripts { run: false, .. } => {
                    window.push_notification(Notification::success("Scripts saved"), cx)
                }
                Request::SetBoatKey(_) => {
                    self.key_input
                        .update(cx, |i, cx| i.set_value("", window, cx));
                    if self
                        .settings_view
                        .as_ref()
                        .is_some_and(|v| v.upgrade().is_some())
                    {
                        window.close_dialog(cx);
                        window.push_notification(Notification::success("Key saved"), cx);
                    }
                }
                _ => {}
            },
        }
        self.show_rows(cx);
    }

    /// A prompt Flotilla wrote: into the open thread's composer (a review to address), or a
    /// new thread in the project (a failed setup to fix). Never sent from here.
    fn show_prompt(
        &mut self,
        req: &Request,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gone = |id: &str| {
            self.summaries
                .iter()
                .any(|s| s.thread.id == id && s.thread.vm_gone())
        };
        match req {
            Request::ReviewPrompt(id) if gone(id) => {
                self.new_thread_on_branch(&id.clone(), Some(prompt.to_string()))
            }
            Request::ReviewPrompt(id) if self.selected() == Some(id.as_str()) => {
                if let Some(view) = self.thread_view() {
                    view.update(cx, |v, cx| v.fill_prompt(prompt, window, cx));
                }
            }
            Request::SetupFixPrompt { project, .. } => {
                window.close_all_dialogs(cx);
                if let Some(page) = self.open_new_thread(window, cx) {
                    page.update(cx, |p, cx| p.prefill(project, prompt, window, cx));
                }
            }
            _ => {}
        }
    }

    /// Ask before an action that can't be undone.
    fn confirm(&mut self, req: Request, window: &mut Window, cx: &mut Context<Self>) {
        let Request::Delete(id) = &req else {
            self.ask(req);
            return;
        };
        let title = self
            .summaries
            .iter()
            .find(|s| &s.thread.id == id)
            .map(|s| s.thread.title.clone())
            .unwrap_or_default();
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let req = req.clone();
            alert
                .title(format!("Delete “{title}”?"))
                .description("Deletes its VM and its history. This can't be undone.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let _ = this.update(cx, |app, cx| {
                        let Request::Delete(id) = &req else { return };
                        // Deleting the open thread leaves it for New thread.
                        let open = app.selected() == Some(id.as_str());
                        app.perform(req.clone(), cx);
                        if open {
                            app.open_new_thread(window, cx);
                        }
                    });
                    true
                })
        });
    }

    /// Which threads the sidebar lists, under which group, in which order.
    pub fn listing(&self) -> crate::listing::Listing<'_> {
        crate::listing::Listing::build(crate::listing::Inputs {
            summaries: &self.summaries,
            parents: &self.parents,
            waiting: &self.waiting,
            held: &self.held,
            hidden_projects: &self.hidden_projects,
        })
    }

    /// Show or hide one project's threads (`NO_PROJECT` for those without one).
    pub fn set_project_shown(&mut self, key: &str, shown: bool, cx: &mut Context<Self>) {
        if shown {
            self.hidden_projects.remove(key);
        } else {
            self.hidden_projects.insert(key.to_string());
        }
        self.save_filter();
        cx.notify();
    }

    /// Show every project's threads, or none.
    pub fn set_all_projects_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.hidden_projects.clear();
        if !shown {
            self.hidden_projects
                .extend(self.projects_cache.iter().map(|p| p.id.clone()));
            self.hidden_projects.insert(NO_PROJECT.to_string());
        }
        self.save_filter();
        cx.notify();
    }

    fn save_filter(&mut self) {
        self.save_device();
    }

    /// Hide the sidebar down to its rail, or bring it back.
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_hidden = !self.sidebar_hidden;
        self.save_device();
        cx.notify();
    }

    /// Start dragging the sidebar's edge.
    pub fn start_sidebar_drag(&mut self, x: f32, window_width: f32) {
        // From the width shown, which a narrow window may hold below the one chosen.
        let shown = self.sidebar_width_for(window_width);
        self.sidebar_width = shown;
        self.sidebar_drag = Some((x, shown));
    }

    /// Done dragging: keep the width.
    fn end_sidebar_drag(&mut self) {
        if self.sidebar_drag.take().is_some() {
            self.device.sidebar_width = Some(self.sidebar_width.round());
            self.save_device();
        }
    }

    /// The sidebar's width for a window this wide: what was chosen, within the limits, and
    /// never leaving the thread less than MAIN_MIN.
    pub fn sidebar_width_for(&self, window_width: f32) -> f32 {
        self.sidebar_width.clamp(
            SIDEBAR_MIN,
            SIDEBAR_MAX.min((window_width - MAIN_MIN).max(SIDEBAR_MIN)),
        )
    }

    pub fn toggle_group(&mut self, group: Activity, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&group) {
            self.collapsed.insert(group);
        }
        self.save_device();
        cx.notify();
    }

    /// A command from the sidebar's right-click menu, its hover button, or the thread's ⋯ menu.
    pub fn run_command(
        &mut self,
        id: String,
        cmd: ThreadCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match cmd {
            ThreadCommand::MarkUnread => {
                self.ask(Request::MarkUnread(id.clone()));
                if let Some(open) = self.open.as_mut().filter(|o| o.id == id) {
                    open.keep_unread = true;
                }
            }
            ThreadCommand::MarkRead => {
                if let Some(open) = self.open.as_mut().filter(|o| o.id == id) {
                    open.keep_unread = false;
                }
                self.ask(Request::MarkViewed(id));
            }
            ThreadCommand::Archive => {
                // Archiving the open thread leaves it for New thread (Undo opens it again).
                let open = self.selected() == Some(id.as_str());
                self.archive(id, cx);
                if open {
                    self.open_new_thread(window, cx);
                }
            }
            ThreadCommand::Unarchive => {
                self.ask(Request::Unarchive(id));
            }
            ThreadCommand::StopVm => self.perform(Request::Stop(id), cx),
            ThreadCommand::ResumeVm => self.perform(Request::Resume(id), cx),
            ThreadCommand::Rename => {
                self.open_rename(id, window, cx);
            }
            ThreadCommand::CopyThreadId => copy(id, window, cx),
            ThreadCommand::CopyVmId => {
                if let Some(sb) = self
                    .summaries
                    .iter()
                    .find(|s| s.thread.id == id)
                    .and_then(|s| s.thread.sandbox_id.clone())
                {
                    copy(sb, window, cx);
                }
            }
            ThreadCommand::Delete => self.confirm(Request::Delete(id), window, cx),
            ThreadCommand::OpenPr => {
                if let Some((pr, _)) = self
                    .summaries
                    .iter()
                    .find(|s| s.thread.id == id)
                    .and_then(|s| s.pr.clone())
                {
                    cx.open_url(&pr.url);
                }
            }
            ThreadCommand::CopyBranch => {
                if let Some(b) = self
                    .summaries
                    .iter()
                    .find(|s| s.thread.id == id)
                    .and_then(|s| s.thread.branch.clone())
                {
                    copy(b, window, cx);
                }
            }
            ThreadCommand::LinkPr => self.open_link_pr(id, window, cx),
            ThreadCommand::MarkReady => {
                self.ask(Request::MarkReady(id));
            }
            ThreadCommand::NewThreadOnBranch => self.new_thread_on_branch(&id, None),
        }
        cx.notify();
    }

    /// Archive a thread. It moves at once; the engine runs `.agents/archive` (waking a stopped
    /// VM for it if the repo has one) and stops the VM.
    fn archive(&mut self, id: String, cx: &mut Context<Self>) {
        self.note_archived(id.clone(), true, cx);
        self.perform(Request::Archive(id), cx);
    }

    /// New thread on `thread`'s branch (its PR's base, else the branch it merges into), once
    /// GitHub says the branch is there, with `prompt` filled in if there's one.
    fn new_thread_on_branch(&mut self, thread: &str, prompt: Option<String>) {
        let on_branch = self.summaries.iter().any(|s| {
            s.thread.id == thread && s.thread.repo_project_id.is_some() && s.thread.branch.is_some()
        });
        if on_branch {
            self.ask(Request::OnBranch {
                thread: thread.to_string(),
                prompt,
            });
        }
    }

    /// The branches threads in `project` are on (expired ones aside).
    fn branches_in_use(&self, project: &str) -> Vec<String> {
        self.summaries
            .iter()
            .filter(|s| {
                s.thread.repo_project_id.as_deref() == Some(project)
                    && s.thread.state != ThreadState::Expired
            })
            .filter_map(|s| s.thread.branch.clone())
            .collect()
    }

    /// Link a pull request by number (or URL) to a project thread.
    fn open_link_pr(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("PR number or URL"));
        let submit = {
            let (app, input, id) = (cx.weak_entity(), input.clone(), id.clone());
            move |window: &mut Window, cx: &mut App| {
                let text = input.read(cx).value().trim().to_string();
                let number = text
                    .rsplit('/')
                    .next()
                    .and_then(|n| n.trim_start_matches('#').parse::<i64>().ok());
                match number {
                    Some(number) => {
                        let _ = app.update(cx, |app, _| {
                            app.ask(Request::LinkPr {
                                thread: id.clone(),
                                number,
                            })
                        });
                        window.close_dialog(cx);
                    }
                    None => {
                        window.push_notification(Notification::error("Not a PR number or URL"), cx)
                    }
                }
            }
        };
        let submit = Rc::new(submit);
        let enter = submit.clone();
        let field = input.clone();
        crate::form::open(
            window,
            cx,
            Some(input.clone()),
            move |window, cx| enter(window, cx),
            move |dialog, _, _| {
                let submit = submit.clone();
                dialog.title("Link a pull request").width(px(420.)).child(
                    v_flex().gap_2().child(Input::new(&field)).child(
                        h_flex().justify_end().child(
                            Button::new("link-pr")
                                .small()
                                .primary()
                                .label("Link")
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                    ),
                )
            },
        );
    }

    fn open_rename(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::rename::RenameView> {
        let current = self
            .summaries
            .iter()
            .find(|s| s.thread.id == id)
            .map(|s| s.thread.title.clone())
            .unwrap_or_default();
        let model = self.summary_model();
        let view = cx.new(|cx| crate::rename::RenameView::new(current, model, window, cx));
        let generate = cx.subscribe(&view, {
            let id = id.clone();
            move |this, view, _: &crate::rename::Generate, _| {
                let req = this.ask(Request::SuggestName(id.clone()));
                this.naming.insert(req, view.downgrade());
            }
        });
        self._subscriptions.push(generate);
        let title = view.read(cx).title.clone();
        let weak = cx.weak_entity();
        let submit = Rc::new({
            let title = title.clone();
            move |window: &mut Window, cx: &mut App| {
                let t = title.read(cx).value().trim().to_string();
                if t.is_empty() {
                    return;
                }
                let _ = weak.update(cx, |app, cx| {
                    app.perform(
                        Request::Rename {
                            thread: id.clone(),
                            title: t,
                        },
                        cx,
                    )
                });
                window.close_dialog(cx);
            }
        });
        let enter = submit.clone();
        let body = view.clone();
        crate::form::open(
            window,
            cx,
            Some(title),
            move |window, cx| enter(window, cx),
            move |dialog, _, _| {
                let submit = submit.clone();
                dialog
                    .title("Rename thread")
                    .width(px(460.))
                    .child(body.clone())
                    .footer(
                        h_flex().justify_end().child(
                            Button::new("rename")
                                .primary()
                                .label("Rename")
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                    )
            },
        );
        view
    }

    /// Keep what's being written in a thread's page (text and files), and where it's
    /// scrolled to, for when its thread opens again.
    fn keep_draft(&mut self, view: &Entity<ThreadView>, cx: &mut Context<Self>) {
        let (id, draft) = view.read(cx).draft(cx);
        match view.read(cx).place(cx) {
            Some(seq) => self.places.insert(id.clone(), seq),
            None => self.places.remove(&id),
        };
        self.drafts.keep(id, draft);
    }

    /// Show the new-thread page (keeping any draft already there): the page, when it's what
    /// shows. While threads can't run the setup shows in its place, and nothing fills or focuses
    /// the page behind it.
    fn open_new_thread(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<NewThreadPage>> {
        if self.setup_stops_threads() {
            self.close_thread(cx);
            cx.notify();
            return None;
        }
        if self.catalog.is_none() {
            window.push_notification("The model catalog hasn't loaded yet", cx);
            return None;
        }
        self.close_thread(cx);
        let page = self.new_thread_page(window, cx);
        if let Some(page) = &page {
            page.update(cx, |p, cx| p.focus_prompt(window, cx));
            if let Some(project) = page.read(cx).project_id() {
                self.ask(Request::ListBranches(project));
            }
        }
        cx.notify();
        page
    }

    /// The new-thread page, created on first use. With no thread open, the main area shows it.
    fn new_thread_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<NewThreadPage>> {
        if self.new_thread.is_none() {
            let catalog = self.catalog.clone()?;
            // Your defaults come with the settings.
            self.settings.as_ref()?;
            if !matches!(
                self.engine,
                Some(EngineState::Running | EngineState::Offline)
            ) {
                return None;
            }
            let environments = self.environments.clone();
            let defaults = self.defaults();
            let projects = self.projects();
            let last = self
                .device
                .for_server(&self.server_key)
                .last_project
                .filter(|p| !p.is_empty());
            let page = cx.new(|cx| {
                NewThreadPage::new(catalog, defaults, environments, projects, last, window, cx)
            });
            let sizes = self.sizes_unavailable();
            page.update(cx, |p, cx| p.set_sizes_unavailable(sizes, cx));
            // What was written here before a switch to another server.
            if let Some(draft) = self.drafts.take_new_thread() {
                page.update(cx, |p, cx| p.put_draft(draft, window, cx));
            }
            let sub = cx.subscribe_in(
                &page,
                window,
                |this, _, event: &NewThreadEvent, window, cx| match event {
                    NewThreadEvent::Submit { threads, stay } => {
                        this.start_threads(threads.clone(), *stay, window, cx)
                    }
                    NewThreadEvent::AddProject => {
                        window.close_all_dialogs(cx);
                        this.open_add_project(window, cx);
                        this.pick_added = true;
                    }
                    NewThreadEvent::WantBranches(project) => {
                        this.ask(Request::ListBranches(project.clone()));
                    }
                    NewThreadEvent::WantMentions(project) => {
                        this.ask(Request::ProjectMentions(project.clone()));
                    }
                    NewThreadEvent::Later {
                        prompt,
                        targets,
                        labels,
                        project,
                    } => {
                        let to = std::iter::once("New thread".to_string())
                            .chain([labels.join(" + ")])
                            .chain(project.clone())
                            .collect::<Vec<_>>()
                            .join(" · ");
                        let targets = targets
                            .iter()
                            .cloned()
                            .map(flotilla_proto::Target::New)
                            .collect();
                        let seed = Seed {
                            id: None,
                            prompt: prompt.clone(),
                            targets,
                            to,
                            when: None,
                            zone: flotilla_model::schedule::here(),
                        };
                        this.open_schedule_editor(seed, Later::NewThread, window, cx);
                    }
                },
            );
            self._subscriptions.push(sub);
            // The page can open with a project picked (the last one used), before any
            // project change asks for its branches.
            if let Some(project) = page.read(cx).project_id() {
                self.ask(Request::ListBranches(project));
            }
            // On launch it's what shows: typing goes straight into it.
            if self.open.is_none() {
                page.update(cx, |p, cx| p.focus_prompt(window, cx));
            }
            self.new_thread = Some(page);
        }
        self.new_thread.clone()
    }

    /// The model titles and previews come from ("" when they're off).
    fn summary_model(&self) -> String {
        self.settings
            .as_ref()
            .map(|s| s.summary_model.clone())
            .unwrap_or_else(|| flotilla_model::summarize::DEFAULT_MODEL.to_string())
    }

    fn set_summary_state(&mut self, state: SummaryState, cx: &mut Context<Self>) {
        self.summary_state = state.clone();
        if let Some(view) = self.settings_view.as_ref().and_then(|v| v.upgrade()) {
            view.update(cx, |v, cx| v.set_summary_state(state, cx));
        }
    }

    /// Defaults for new threads, from your settings.
    fn defaults(&self) -> Option<ThreadSettings> {
        self.settings.as_ref()?.defaults.clone()
    }

    /// ⌘K: the palette, with the open thread's commands, the app's, and every thread.
    /// Again while it's open: closed. Over Settings, Settings closes first; over another
    /// dialog (a rename, a question), nothing.
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use crate::palette::{AppCommand, Entry, Pick, ThreadRow};
        use gpui_kit::assets::IconName;
        if crate::palette::is_open(self) {
            window.close_dialog(cx);
            self.palette = None;
            return;
        }
        if window.has_active_dialog(cx) {
            if !self
                .settings_view
                .as_ref()
                .is_some_and(|v| v.upgrade().is_some())
            {
                return;
            }
            window.close_dialog(cx);
            self.settings_view = None;
        }
        let entry = |label: &str,
                     keywords: &'static str,
                     icon: IconName,
                     key: Option<Box<dyn Action>>,
                     pick: AppCommand| Entry {
            label: label.to_string(),
            keywords,
            icon,
            key,
            pick: Pick::App(pick),
        };
        let open = self
            .selected()
            .and_then(|id| self.summaries.iter().find(|s| s.thread.id == id))
            .cloned();
        let here = open.as_ref().map(|s| {
            let own_row = self.listing().row_for(&s.thread.id) == s.thread.id;
            let commands = crate::thread_menu::commands(s, own_row)
                .into_iter()
                .flatten()
                .map(|c| Entry {
                    label: c.label().to_string(),
                    keywords: "thread",
                    icon: c.icon(),
                    key: None,
                    pick: Pick::Thread(s.thread.id.clone(), c),
                })
                .collect();
            (
                flotilla_model::util::ellipsize(&s.thread.title, 60),
                commands,
            )
        });
        let mut commands = vec![entry(
            "New thread",
            "start create",
            IconName::SquarePen,
            Some(Box::new(crate::NewThread)),
            AppCommand::NewThread,
        )];
        if open.is_some() {
            commands.push(entry(
                "Archive and open next",
                "thread settle",
                IconName::Archive,
                Some(Box::new(crate::ArchiveAndNext)),
                AppCommand::ArchiveAndNext,
            ));
        }
        commands.extend([
            entry(
                "Next thread",
                "go down",
                IconName::ChevronDown,
                Some(Box::new(crate::NextThread)),
                AppCommand::NextThread,
            ),
            entry(
                "Previous thread",
                "go up",
                IconName::ChevronUp,
                Some(Box::new(crate::PreviousThread)),
                AppCommand::PreviousThread,
            ),
            entry(
                "Show or hide the sidebar",
                "toggle",
                IconName::PanelLeft,
                Some(Box::new(crate::ToggleSidebar)),
                AppCommand::ToggleSidebar,
            ),
            entry(
                "Sync now",
                "refresh reload boat",
                IconName::RefreshCw,
                Some(Box::new(crate::SyncNow)),
                AppCommand::SyncNow,
            ),
            entry(
                "Add a project…",
                "repo github",
                IconName::FolderPlus,
                None,
                AppCommand::AddProject,
            ),
            entry(
                "Settings…",
                "preferences options",
                IconName::Settings,
                Some(Box::new(crate::OpenSettings)),
                AppCommand::Settings,
            ),
            entry(
                connect_label(self.device.paired.as_ref()),
                "pair remote",
                IconName::Server,
                Some(Box::new(crate::ConnectServer)),
                AppCommand::ConnectServer,
            ),
        ]);
        if !self.schedules.is_empty() {
            commands.push(entry(
                "Scheduled prompts",
                "later cron schedule recurring send",
                IconName::Clock,
                None,
                AppCommand::Scheduled,
            ));
        }
        if self.server.is_some() {
            commands.push(entry(
                &format!("Use {}", crate::device::THIS_MACHINE_LOWER),
                "switch local server",
                IconName::Laptop,
                None,
                AppCommand::UseThisMachine,
            ));
        }
        if let Some(paired) = self
            .device
            .paired
            .as_ref()
            .filter(|_| !self.device.on_server)
        {
            commands.push(entry(
                &format!("Use {}", paired.name()),
                "switch server",
                IconName::Server,
                None,
                AppCommand::UseServer,
            ));
        }
        let threads = self
            .summaries
            .iter()
            .map(|s| {
                let project = s
                    .thread
                    .repo_project_id
                    .as_ref()
                    .and_then(|id| self.projects_cache.iter().find(|p| &p.id == id))
                    .map(|p| format!("{}/{}", p.owner, p.repo));
                ThreadRow::of(s, project)
            })
            .collect();
        let view =
            cx.new(|cx| crate::palette::PaletteView::new(here, commands, threads, window, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &crate::palette::PaletteEvent, window, cx| match event {
                crate::palette::PaletteEvent::Search(query, limit) => {
                    this.ask(Request::Search {
                        query: query.clone(),
                        limit: Some(*limit),
                    });
                }
                crate::palette::PaletteEvent::Picked(pick, query) => {
                    // Closed first: focus goes back where it was, then to wherever the pick goes.
                    window.close_dialog(cx);
                    this.palette = None;
                    this.open_query = query.clone();
                    this.pick(pick.clone(), window, cx);
                    this.open_query = None;
                }
            },
        );
        self.palette_sub = Some(sub);
        self.palette = Some(view.downgrade());
        crate::palette::open(view, window, cx);
    }

    /// Do what a palette row says.
    fn pick(&mut self, pick: crate::palette::Pick, window: &mut Window, cx: &mut Context<Self>) {
        use crate::palette::{AppCommand, Pick};
        match pick {
            Pick::Open(id) => self.select(id, window, cx),
            Pick::Message { thread, seq } => self.open_at(thread, seq, window, cx),
            Pick::Thread(id, cmd) => self.run_command(id, cmd, window, cx),
            Pick::App(c) => match c {
                AppCommand::NewThread => {
                    self.open_new_thread(window, cx);
                }
                AppCommand::Settings => self.open_settings(window, cx),
                AppCommand::ConnectServer => self.open_connect(None, window, cx),
                AppCommand::UseThisMachine => self.use_this_mac(window, cx),
                AppCommand::UseServer => self.use_paired(window, cx),
                AppCommand::AddProject => self.open_add_project(window, cx),
                AppCommand::Scheduled => self.open_scheduled(None, window, cx),
                AppCommand::SyncNow => {
                    self.ask(Request::SyncNow);
                }
                AppCommand::ToggleSidebar => self.toggle_sidebar(cx),
                AppCommand::NextThread => self.step_thread(1, window, cx),
                AppCommand::PreviousThread => self.step_thread(-1, window, cx),
                AppCommand::ArchiveAndNext => self.archive_and_next(window, cx),
            },
        }
        cx.notify();
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Before the server's settings have come (it can't be reached, say): what's kept on
        // this device, which is what's needed to use another server or This Mac.
        let init = SettingsInit {
            settings: self.settings.clone(),
            catalog: self.catalog.clone(),
            environments: self.environments.clone(),
            summary_state: self.summary_state.clone(),
            appearance: self.appearance,
            gh_status: self.gh_status.clone(),
            mac_skills: flotilla_model::skills::on_this_mac(),
            me: self.backend.device(),
            server: self.server.clone(),
            paired: self.device.paired.clone(),
            devices: self.devices.clone(),
            devices_in: (
                self.device.let_devices_in,
                self.device.over_tailscale,
                self.devices_in.clone(),
            ),
            notifications: (
                self.device.notifications_on(),
                crate::notify::permission(cx),
            ),
        };
        crate::notify::recheck(cx);
        let view = cx.new(|cx| SettingsView::new(init, window, cx));
        self.settings_view = Some(view.downgrade());
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::SaveKey(key) => {
                    this.ask(Request::SetBoatKey(Secret(key.clone())));
                }
                SettingsEvent::DefaultsChanged(s) => {
                    this.ask(Request::Set(Setting::Defaults(s.clone())));
                }
                SettingsEvent::Appearance(a) => {
                    this.appearance = *a;
                    this.save_device();
                    apply_appearance(*a, window, cx);
                    cx.notify();
                }
                SettingsEvent::Notifications(on) => {
                    this.device.notifications = Some(*on);
                    this.save_device();
                    this.show_badge(cx);
                    crate::notify::moment(cx, *on);
                }
                SettingsEvent::SummaryModel(model) => {
                    // Setting a model tests it.
                    this.ask(Request::Set(Setting::SummaryModel(model.clone())));
                    this.set_summary_state(
                        if model.is_empty() {
                            SummaryState::Off
                        } else {
                            SummaryState::Testing
                        },
                        cx,
                    );
                }
                // From each thread's next prompt.
                SettingsEvent::Instructions(text) => {
                    this.ask(Request::SetInstructions(text.clone()));
                }
                // Packed and uploaded here, then kept on the engine's side.
                SettingsEvent::AddSkills(skills) => this.add_skills(skills.clone(), window, cx),
                SettingsEvent::RemoveSkill(name) => {
                    this.ask(Request::RemoveSkill(name.clone()));
                }
                SettingsEvent::RunDoctor => {
                    this.ask(Request::RunDoctor);
                    this.ask(Request::CheckGitHub);
                }
                // Applies from each thread's next turn, resume or start.
                SettingsEvent::IdleMinutes(m) => {
                    this.ask(Request::Set(Setting::IdleMinutes(*m)));
                }
                SettingsEvent::ArchiveOnMerge(on) => {
                    this.ask(Request::Set(Setting::ArchiveOnMerge(*on)));
                }
                SettingsEvent::ArchiveIdleDays(days) => {
                    this.ask(Request::Set(Setting::ArchiveIdleDays(*days)));
                }
                // Applies to branches named from now on; existing ones keep their names.
                SettingsEvent::BranchPrefix(prefix) => {
                    this.ask(Request::Set(Setting::BranchPrefix(prefix.clone())));
                }
                SettingsEvent::AddProject => this.open_add_project(window, cx),
                SettingsEvent::OpenProject(id) => this.open_project(id.clone(), window, cx),
                SettingsEvent::Rebuild => {
                    window.close_dialog(cx);
                    this.rebuild(window, cx);
                }
                SettingsEvent::ConnectServer => this.open_connect(None, window, cx),
                SettingsEvent::UseThisMac => this.use_this_mac(window, cx),
                SettingsEvent::UseServer => this.use_paired(window, cx),
                SettingsEvent::ForgetServer => this.forget_paired(window, cx),
                SettingsEvent::AddDevice => {
                    this.ask(Request::PairCode { ttl_secs: None });
                }
                SettingsEvent::RevokeDevice(id) => {
                    this.ask(Request::RevokeDevice(id.clone()));
                }
                SettingsEvent::RenameDevice { id, name } => {
                    this.ask(Request::RenameDevice {
                        device: id.clone(),
                        name: name.clone(),
                    });
                }
                SettingsEvent::LetDevicesIn { on, tailscale } => {
                    this.let_devices_in(*on, *tailscale, cx)
                }
            },
        );
        self._subscriptions.push(sub);
        // Enter in a field saves that field (its own handler), and never closes Settings.
        crate::form::open(
            window,
            cx,
            None,
            |_, _| {},
            move |dialog, _, _| dialog.title("Settings").width(px(560.)).child(view.clone()),
        );
    }

    /// Where a schedule goes, in words: its thread's title, or "New thread · Opus 5.5 ·
    /// acme/web".
    fn schedule_to(&self, target: &flotilla_proto::Target) -> String {
        match target {
            flotilla_proto::Target::Thread(t) => self
                .summaries
                .iter()
                .find(|s| &s.thread.id == t)
                .map(|s| s.thread.title.clone())
                .unwrap_or_else(|| "A thread that's gone".into()),
            flotilla_proto::Target::New(f) => {
                let model = model_label(self.catalog.as_ref(), &f.provider, &f.model)
                    .unwrap_or_else(|| f.model.clone());
                let project = f
                    .project_id
                    .as_ref()
                    .and_then(|p| self.projects_cache.iter().find(|x| &x.id == p))
                    .map(|p| p.repo.clone());
                std::iter::once("New thread".to_string())
                    .chain([model])
                    .chain(project)
                    .collect::<Vec<_>>()
                    .join(" · ")
            }
        }
    }

    fn schedule_rows(&self) -> Vec<crate::schedules_view::Row> {
        self.schedules
            .iter()
            .map(|s| {
                // A new thread's: the one its last run started, while it's there.
                let thread = s
                    .target
                    .thread()
                    .map(str::to_string)
                    .or_else(|| s.last.as_ref().and_then(|l| l.thread.clone()))
                    .filter(|t| self.summaries.iter().any(|x| &x.thread.id == t));
                crate::schedules_view::Row {
                    schedule: s.clone(),
                    to: self.schedule_to(&s.target),
                    thread,
                }
            })
            .collect()
    }

    /// Scheduled, open: its rows again; with none left, it closes.
    fn refresh_schedules(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.schedules_view.as_ref().and_then(|v| v.upgrade()) else {
            return;
        };
        if self.schedules.is_empty() && self.schedule_editor.is_none() {
            self.schedules_view = None;
            window.close_dialog(cx);
            return;
        }
        let (rows, this_mac) = (self.schedule_rows(), self.server.is_none());
        view.update(cx, |v, cx| v.set_rows(rows, this_mac, cx));
    }

    /// The Scheduled dialog: every scheduled prompt, `focus` first.
    pub(crate) fn open_scheduled(
        &mut self,
        focus: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.schedules.is_empty() {
            return;
        }
        window.close_all_dialogs(cx);
        let view =
            cx.new(|_| SchedulesView::new(self.schedule_rows(), self.server.is_none(), focus));
        self.schedules_view = Some(view.downgrade());
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &SchedulesEvent, window, cx| match event {
                SchedulesEvent::Pause { id, paused } => {
                    this.ask(Request::PauseSchedule {
                        schedule: id.clone(),
                        paused: *paused,
                    });
                }
                SchedulesEvent::RunNow(id) => {
                    this.ask(Request::RunSchedule(id.clone()));
                }
                SchedulesEvent::Delete(id) => {
                    this.ask(Request::DeleteSchedule(id.clone()));
                }
                SchedulesEvent::Edit(id) => {
                    let Some(s) = this.schedules.iter().find(|s| &s.id == id).cloned() else {
                        return;
                    };
                    let seed = Seed {
                        id: Some(s.id.clone()),
                        prompt: s.prompt.clone(),
                        to: this.schedule_to(&s.target),
                        targets: vec![s.target.clone()],
                        when: Some(s.when.clone()),
                        zone: s.zone.clone(),
                    };
                    this.open_schedule_editor(seed, Later::Edit, window, cx);
                }
                SchedulesEvent::Open(thread) => {
                    window.close_all_dialogs(cx);
                    this.schedules_view = None;
                    this.select(thread.clone(), window, cx);
                }
            },
        );
        self._subscriptions.push(sub);
        crate::form::open(
            window,
            cx,
            None,
            |_, _| {},
            move |dialog, _, _| {
                dialog
                    .title("Scheduled")
                    .width(px(560.))
                    .child(view.clone())
            },
        );
    }

    /// Send later's form, from a thread's composer, New thread, or Scheduled's Edit.
    fn open_schedule_editor(
        &mut self,
        seed: Seed,
        origin: Later,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = if seed.id.is_some() {
            "Edit schedule"
        } else {
            "Send later"
        };
        let back = matches!(origin, Later::Edit)
            .then(|| seed.id.clone())
            .flatten();
        window.close_all_dialogs(cx);
        self.schedules_view = None;
        let view = cx.new(|cx| ScheduleEditor::new(seed, window, cx));
        self.schedule_editor = Some(LaterForm {
            view: view.downgrade(),
            origin,
            waiting: HashMap::new(),
            refused: None,
        });
        let sub = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &EditorEvent, window, cx| match event {
                EditorEvent::Save(specs) => {
                    if let Some(form) = this.schedule_editor.as_mut() {
                        form.waiting = specs.iter().map(|s| (s.id.clone(), s.clone())).collect();
                        form.refused = None;
                    }
                    for spec in specs {
                        this.ask(Request::PutSchedule(Box::new(spec.clone())));
                    }
                }
                EditorEvent::Cancel => {
                    this.schedule_editor = None;
                    window.close_dialog(cx);
                    // An edit goes back to the list it came from.
                    if let Some(id) = back.clone() {
                        this.open_scheduled(Some(id), window, cx);
                    }
                }
            },
        );
        self._subscriptions.push(sub);
        let (first, enter) = (view.read(cx).prompt(), view.downgrade());
        crate::form::open(
            window,
            cx,
            None,
            move |_, cx| {
                let _ = enter.update(cx, |v, cx| v.save(cx));
            },
            move |dialog, _, _| dialog.title(title).width(px(560.)).child(view.clone()),
        );
        // The prompt, ready to change (a textarea: the form focuses one-line fields).
        first.update(cx, |p, cx| p.focus(window, cx));
    }

    /// Pack each skill's folder, upload it, and give it to every agent (in place of one of its
    /// name: Update). Settings shows each as adding until the engine's side lists it.
    fn add_skills(
        &mut self,
        skills: Vec<flotilla_proto::Skill>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let up = self.backend.uploader();
        let task = cx.background_spawn(async move {
            skills
                .into_iter()
                .map(|skill| {
                    let packed = flotilla_model::skills::pack(std::slice::from_ref(&skill))
                        .map_err(|e| e.to_string());
                    let blob = packed.and_then(|p| up.put(&p));
                    (skill, blob)
                })
                .collect::<Vec<_>>()
        });
        let life = self.life;
        cx.spawn_in(window, async move |this, cx| {
            let done = task.await;
            let _ = this.update_in(cx, |app, window, cx| {
                // Uploaded to a server this Mac has since switched from.
                if app.life != life {
                    return;
                }
                for (skill, blob) in done {
                    match blob {
                        Ok(blob) => {
                            app.ask(Request::PutSkill {
                                skill,
                                blob: blob.id,
                            });
                        }
                        Err(e) => {
                            if let Some(view) = app.settings_view.as_ref().and_then(|v| v.upgrade())
                            {
                                view.update(cx, |v, cx| v.skill_failed(&skill.name, cx));
                            }
                            window.push_notification(
                                Notification::error(e)
                                    .title(format!("Couldn't add {}", skill.name)),
                                cx,
                            );
                        }
                    }
                }
            });
        })
        .detach();
    }

    pub fn projects(&self) -> Vec<Project> {
        self.projects_cache.clone()
    }

    /// A project's settings dialog, over Settings: it opens when the project's view arrives.
    pub(crate) fn open_project(
        &mut self,
        id: String,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if let Some(old) = self.project_sub.replace(id.clone()) {
            self.backend.send(ClientMsg::Unsub {
                view: ViewKey::Project(old),
            });
        }
        self.project_view = None;
        self.backend.send(ClientMsg::Sub {
            view: ViewKey::Project(id),
        });
    }

    /// Dev: a view came for a thread or project not open here (`expect:strays:`).
    fn stray(&mut self) {
        #[cfg(feature = "snapshot")]
        {
            self.strays += 1;
        }
    }

    /// No project's dialog: its subscription goes (the one way to none).
    fn close_project(&mut self) {
        self.project_view = None;
        if let Some(old) = self.project_sub.take() {
            self.backend.send(ClientMsg::Unsub {
                view: ViewKey::Project(old),
            });
        }
    }

    /// A project's view: open its dialog the first time, then keep it in step.
    fn on_project(
        &mut self,
        p: flotilla_proto::ProjectDetail,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_sub.as_deref() != Some(p.id.as_str()) {
            self.stray();
            return;
        }
        let Some(project) = p.project else { return };
        if let Some(view) = self.project_view.as_ref().and_then(|v| v.upgrade()) {
            view.update(cx, |v, cx| v.refresh(project, p.runs, p.github_error, cx));
            return;
        }
        // Opened once already and closed since: let it go.
        if self.project_view.is_some() {
            self.close_project();
            return;
        }
        let id = p.id.clone();
        let title = project.repo.clone();
        let view =
            cx.new(|cx| ProjectView::new(project, p.runs, p.scripts, p.github_error, window, cx));
        let sizes = self.sizes_unavailable();
        view.update(cx, |v, cx| v.set_sizes_unavailable(sizes, cx));
        self.project_view = Some(view.downgrade());
        let sub = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &ProjectEvent, window, cx| match event {
                ProjectEvent::RunSetup { fresh } => {
                    this.ask(Request::RunSetup {
                        project: id.clone(),
                        fresh: *fresh,
                    });
                }
                ProjectEvent::Cancel => {
                    this.ask(Request::CancelSetup(id.clone()));
                }
                ProjectEvent::SaveScripts { scripts, run } => {
                    this.ask(Request::SetScripts {
                        project: id.clone(),
                        scripts: scripts.clone(),
                        run: *run,
                    });
                }
                ProjectEvent::SetMachineType(t) => {
                    this.ask(Request::SetMachineType {
                        project: id.clone(),
                        machine_type: Some(t.clone()),
                    });
                }
                ProjectEvent::Remove => this.confirm_remove_project(id.clone(), window, cx),
                ProjectEvent::FixSetup { run } => {
                    this.ask(Request::SetupFixPrompt {
                        project: id.clone(),
                        run: run.clone(),
                    });
                }
            },
        );
        self._subscriptions.push(sub);
        // Not a form to start typing in: Enter in a script saves the scripts.
        let enter = view.downgrade();
        crate::form::open(
            window,
            cx,
            None,
            move |_, cx| {
                let _ = enter.update(cx, |v, cx| v.save_on_enter(cx));
            },
            move |dialog, _, _| {
                dialog
                    .title(title.clone())
                    .width(px(640.))
                    .child(view.clone())
            },
        );
    }

    /// The Add project dialog, over Settings or on its own from New thread.
    fn open_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pick_added = false;
        let taken = self
            .projects()
            .iter()
            .map(|p| p.full_name().to_lowercase())
            .collect();
        let gh = self.gh_status.clone();
        let signed_in = gh.signed_in();
        let view = cx.new(|cx| AddProjectView::new(taken, gh, window, cx));
        self.add_project = Some(view.downgrade());
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &AddProjectEvent, window, cx| match event {
                AddProjectEvent::Load => {
                    this.ask(Request::CachedRepos);
                    this.ask(Request::ListRepos);
                }
                AddProjectEvent::Retry => {
                    this.ask(Request::ListRepos);
                }
                AddProjectEvent::CheckGh => {
                    this.ask(Request::CheckGitHub);
                }
                AddProjectEvent::Suggest(repo) => {
                    this.ask(Request::Suggest(Box::new(repo.clone())));
                }
                AddProjectEvent::Add { repo, scripts } => {
                    this.ask(Request::AddProject {
                        repo: Box::new(repo.clone()),
                        scripts: scripts.clone(),
                    });
                }
                AddProjectEvent::Cancel => {
                    this.add_project = None;
                    this.pick_added = false;
                    window.close_dialog(cx);
                }
            },
        );
        self._subscriptions.push(sub);
        // The signed-in account's last list at once; a fresh one replaces it when it's in.
        // Not signed in, the dialog says how first (and loads once it is); not known yet,
        // gh is asked.
        if signed_in {
            self.ask(Request::CachedRepos);
            self.ask(Request::ListRepos);
        } else if self.gh_status == GhStatus::Unknown {
            self.ask(Request::CheckGitHub);
        }
        let (first, enter) = (view.read(cx).search(), view.downgrade());
        crate::form::open(
            window,
            cx,
            Some(first),
            move |window, cx| {
                let _ = enter.update(cx, |v, cx| v.enter(window, cx));
            },
            move |dialog, _, _| {
                dialog
                    .title("Add a project")
                    .width(px(600.))
                    .child(view.clone())
            },
        );
    }

    fn confirm_remove_project(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.projects_cache.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        let app = cx.weak_entity();
        let name = p.full_name();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (app, id) = (app.clone(), id.clone());
            alert
                .title(format!("Remove {name}?"))
                .description("Threads already started keep their VMs.")
                .confirm()
                .ok_text("Remove")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ = app.update(cx, |app, _| app.ask(Request::RemoveProject(id.clone())));
                    true
                })
        });
    }

    /// Adding or removing a project finished.
    fn project_done(
        &mut self,
        req: &Request,
        result: Result<Reply, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut pick = None;
        match (req, result) {
            (Request::AddProject { repo, .. }, Ok(reply)) => {
                if self.add_project.take().and_then(|v| v.upgrade()).is_some() {
                    window.close_dialog(cx);
                }
                // (The engine sets it up straight away, so the first thread starts ready.)
                if let Reply::Created(id) = reply
                    && std::mem::take(&mut self.pick_added)
                {
                    pick = Some(id);
                }
                window.push_notification(
                    Notification::success(format!("Added {}", repo.full_name())),
                    cx,
                );
            }
            (Request::RemoveProject(_), Ok(_)) => {
                if self.project_view.take().and_then(|v| v.upgrade()).is_some() {
                    window.close_dialog(cx);
                }
                window.push_notification(Notification::success("Project removed"), cx);
            }
            (Request::AddProject { .. }, Err(e)) => {
                match self.add_project.as_ref().and_then(|v| v.upgrade()) {
                    Some(view) => view.update(cx, |v, cx| v.failed(e, cx)),
                    None => window.push_notification(
                        Notification::error(e).title("Couldn't add the project"),
                        cx,
                    ),
                }
            }
            (_, Ok(_)) => {}
            (_, Err(e)) => window.push_notification(
                Notification::error(e).title(format!("Couldn't {}", req.verb())),
                cx,
            ),
        }
        // The views it changed came first: New thread has the project in its list by now.
        if let (Some(id), Some(page)) = (pick, &self.new_thread) {
            page.update(cx, |p, cx| p.pick_project(id, window, cx));
        }
    }

    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.engine, Some(EngineState::Running)) {
            return;
        }
        self.rebuilding = true;
        self.close_thread(cx);
        self.new_thread = None;
        self.rows.clear();
        self.summaries.clear();
        self.ask(Request::Rebuild);
        window.push_notification("Rebuilding from boat…", cx);
        cx.notify();
    }

    fn save_key(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).value().to_string();
        self.ask(Request::SetBoatKey(Secret(key)));
        cx.notify();
    }

    /// Nothing open while a check threads need fails: the setup in New thread's place, as
    /// Settings shows it opened, with Check again (and This Mac's boat key field).
    fn render_setup(&self, doctor: &flotilla_proto::Doctor, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let remote = self.server.is_some();
        let (ok, all) = doctor.tally();
        let key_field = || {
            h_flex()
                .gap_2()
                .pl_6()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.key_input).small()),
                )
                .child(
                    div().flex_none().child(
                        Button::new("setup-save-key")
                            .small()
                            .label("Save")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.setup_checking = this
                                    .settings
                                    .as_ref()
                                    .and_then(|s| s.doctor.as_ref())
                                    .map(|d| d.at_ms);
                                this.save_key(window, cx);
                            })),
                    ),
                )
                .into_any_element()
        };
        let key_field: &dyn Fn() -> AnyElement = &key_field;
        div()
            .id("setup-page")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(560.))
                    .mx_auto()
                    .px_6()
                    .py(px(56.))
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .pb_1()
                            .child(div().text_lg().font_semibold().child(if remote {
                                "Server setup"
                            } else {
                                "Setup"
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(if ok == all {
                                        theme.muted_foreground
                                    } else {
                                        theme.foreground
                                    })
                                    .child(format!("{ok}/{all}")),
                            )
                            .child(
                                Button::new("setup-check")
                                    .small()
                                    .outline()
                                    .label("Check again")
                                    .loading(self.setup_checking.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.setup_checking = this
                                            .settings
                                            .as_ref()
                                            .and_then(|s| s.doctor.as_ref())
                                            .map(|d| d.at_ms);
                                        this.ask(Request::RunDoctor);
                                        this.ask(Request::CheckGitHub);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .children(crate::settings::check_rows(
                        doctor,
                        (!remote).then_some(key_field),
                        &theme,
                    )),
            )
            .into_any_element()
    }

    fn render_main(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        if let Some(flotilla_proto::Connection::Refused { reason }) = self.connection.clone() {
            let (reason, choices) = match (&self.server, self.backend.instead().cloned()) {
                // On a server: pair again, or go back to this Mac.
                (Some(_), _) => (
                    reason,
                    Some(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("refused-connect")
                                    .outline()
                                    .label("Connect to a server…")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_connect(None, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("refused-this-mac")
                                    .outline()
                                    .label(format!("Use {}", crate::device::THIS_MACHINE_LOWER))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.use_this_mac(window, cx)
                                    })),
                            ),
                    ),
                ),
                // This Mac: a server here has its database. Same user, same machine: no pairing.
                (None, Some(flotilla_server::Instead::Socket(_))) => (
                    format!(
                        "Flotilla is already running on {} as a server.",
                        crate::device::THIS_MACHINE_LOWER
                    ),
                    Some(
                        h_flex().child(
                            Button::new("refused-offer")
                                .primary()
                                .label("Connect to it")
                                .loading(self.joining)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.take_offer(window, cx)),
                                ),
                        ),
                    ),
                ),
                // This Mac's database moved to a server: use that one.
                (None, Some(flotilla_server::Instead::Moved(moved))) => {
                    let saved = self.device.paired.as_ref().filter(|s| s.id == moved.server);
                    let place = saved
                        .map(|s| s.name().to_string())
                        .unwrap_or_else(|| moved.place());
                    let label = saved
                        .map(|s| format!("Use {}", s.name()))
                        .unwrap_or_else(|| "Connect to a server…".into());
                    (
                        format!(
                            "{}'s threads moved to {place}.",
                            crate::device::THIS_MACHINE
                        ),
                        Some(
                            h_flex().child(
                                Button::new("refused-offer")
                                    .primary()
                                    .label(label)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.take_offer(window, cx)
                                    })),
                            ),
                        ),
                    )
                }
                (None, None) => (reason, None),
            };
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(div().max_w(px(420.)).child(reason))
                .children(choices)
                .into_any_element();
        }
        if let Some(error) = self.unreached() {
            let name = self
                .server
                .as_ref()
                .map(|s| s.name().to_string())
                .unwrap_or_default();
            // "Can't reach <address>: <why> (os error 61)": the address is in the line above.
            let why = error
                .split_once(": ")
                .filter(|(head, _)| head.starts_with("Can't reach"))
                .map_or(error.as_str(), |(_, why)| why);
            let why = why
                .split(" (os error")
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(theme.muted_foreground)
                .child(div().max_w(px(420.)).child(format!("Can't reach {name}")))
                .when(!why.is_empty(), |el| {
                    el.child(div().max_w(px(420.)).text_xs().child(why))
                })
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("unreached-retry")
                                .outline()
                                .label("Retry")
                                .loading(self.retrying)
                                .on_click(cx.listener(|this, _, _, cx| this.retry_server(cx))),
                        )
                        .child(
                            Button::new("unreached-connect")
                                .outline()
                                .label("Connect to a server…")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_connect(None, window, cx)
                                })),
                        )
                        .child(
                            Button::new("unreached-this-mac")
                                .outline()
                                .label(format!("Use {}", crate::device::THIS_MACHINE_LOWER))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.use_this_mac(window, cx)
                                    }),
                                ),
                        ),
                )
                .into_any_element();
        }
        // This Mac without a key that works: paste one. A server's is set there, which its setup
        // says, in New thread's place.
        if let Some(EngineState::Stopped(err)) = self.engine.clone()
            && !(self.server.is_some() && self.setup_stops_threads())
        {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_lg().font_semibold().child("Connect to boat"))
                .child(
                    div()
                        .max_w(px(420.))
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(if cfg!(target_os = "macos") {
                            "Paste a boat API key. It's kept in your Keychain.".to_string()
                        } else {
                            format!(
                                "Paste a boat API key. It's kept in {}, which only you can read.",
                                kept_at(
                                    &flotilla_core::secrets::store()
                                        .kept_in(flotilla_core::secrets::Name::BoatKey)
                                )
                            )
                        }),
                )
                .child(
                    h_flex()
                        .w(px(420.))
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.key_input)))
                        .child(Button::new("save-key").primary().label("Save").on_click(
                            cx.listener(|this, _, window, cx| this.save_key(window, cx)),
                        )),
                )
                // The command that makes a key with every permission Flotilla uses.
                .child(crate::docs::link(
                    "boat-key-docs",
                    "How to make a key",
                    crate::docs::BOAT_KEY,
                ))
                // Only a real failure: a key that's simply missing is what this screen is for,
                // and its message ("run `flotilla setup`…") is a server's.
                .when(!err.starts_with("no boat API key"), |el| {
                    el.child(
                        div()
                            .max_w(px(420.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(err),
                    )
                })
                .into_any_element();
        }
        if self.rebuilding {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("Rebuilding from boat…")
                .into_any_element();
        }
        if let Some(open) = &self.open {
            let view = &open.view;
            // Opened a moment ago: what was showing stays until its view comes.
            if open.awaiting.is_some() {
                match open.previous.clone() {
                    Some(previous) => return previous.into_any_element(),
                    None => {
                        if let Some(page) = self.new_thread.clone() {
                            return page.into_any_element();
                        }
                    }
                }
            }
            return view.clone().into_any_element();
        }
        if let Some(doctor) = self
            .settings
            .as_ref()
            .and_then(|s| s.doctor.clone())
            .filter(|d| d.stops_threads())
        {
            return self.render_setup(&doctor, cx);
        }
        match self.new_thread_page(window, cx) {
            Some(page) => page.into_any_element(),
            // Before the model catalog first loads.
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child("Loading…")
                .into_any_element(),
        }
    }
}

impl Render for FlotillaApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = self.render_main(window, cx);
        let theme = cx.theme();
        let body = h_flex()
            .id("flotilla")
            .key_context("Flotilla")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NewThread, window, cx| {
                this.open_new_thread(window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &crate::OpenPalette, window, cx| {
                    this.open_palette(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ConnectServer, window, cx| {
                this.open_connect(None, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SyncNow, _, _| {
                this.ask(Request::SyncNow);
            }))
            .on_action(
                cx.listener(|this, _: &NextThread, window, cx| this.step_thread(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousThread, window, cx| {
                    this.step_thread(-1, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread1, window, cx| this.open_nth(0, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread2, window, cx| this.open_nth(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread3, window, cx| this.open_nth(2, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread4, window, cx| this.open_nth(3, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread5, window, cx| this.open_nth(4, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread6, window, cx| this.open_nth(5, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread7, window, cx| this.open_nth(6, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread8, window, cx| this.open_nth(7, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::Thread9, window, cx| this.open_nth(8, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ArchiveAndNext, window, cx| {
                    this.archive_and_next(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &UndoArchive, window, cx| {
                if this.undo_owns_cmd_z() {
                    this.undo_archive(window, cx);
                }
            }))
            // Right after an archive ⌘Z undoes it, even from a text field, until another key
            // is pressed or the notice goes (as in T3 Code); then a field's own undo is back.
            // Other keys release it in the interceptor set up in `new`.
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::Undo, window, cx| {
                    if this.undo_owns_cmd_z() {
                        cx.stop_propagation();
                        this.undo_archive(window, cx);
                    }
                },
            ))
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(cx.listener(|this, _: &crate::ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            // Dragging the sidebar's edge (started on the handle; followed here, so the pointer
            // can run ahead of the edge).
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                let Some((from, width)) = this.sidebar_drag else {
                    return;
                };
                if e.pressed_button != Some(MouseButton::Left) {
                    this.end_sidebar_drag();
                    return;
                }
                let wanted = width + (f32::from(e.position.x) - from);
                this.sidebar_width = wanted.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_sidebar_drag()),
            )
            .size_full()
            .items_stretch()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(crate::sidebar::render(self, window, cx))
            .child(div().flex_1().min_w_0().h_full().child(main))
            .children(self.demo_pointer_overlay());
        #[cfg(not(target_os = "linux"))]
        return body;
        #[cfg(target_os = "linux")]
        return framed(body, window, cx);
    }
}

/// Linux: the window's title bar, with its controls, when the compositor leaves it to the app
/// (GNOME on Wayland). Elsewhere the window manager draws one.
#[cfg(target_os = "linux")]
fn framed(body: impl IntoElement, window: &Window, cx: &App) -> AnyElement {
    if !matches!(window.window_decorations(), Decorations::Client { .. }) {
        return body.into_any_element();
    }
    let title = div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(flotilla_model::flavor::app_name());
    v_flex()
        .size_full()
        .child(gpui_kit::component::TitleBar::new().child(title))
        .child(div().flex_1().min_h_0().child(body))
        .into_any_element()
}

impl FlotillaApp {
    /// Dev: the scripted demo's pointer, above menus and dialogs; darker for a moment after a
    /// click.
    #[cfg(feature = "snapshot")]
    fn demo_pointer_overlay(&self) -> Option<AnyElement> {
        let (at, pressed) = self.demo_pointer?;
        let down = pressed.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(180));
        let size = px(if down { 16. } else { 20. });
        Some(
            deferred(
                anchored()
                    .position(point(at.x - size / 2., at.y - size / 2.))
                    .child(
                        div()
                            .size(size)
                            .rounded_full()
                            .border_2()
                            .border_color(gpui_kit::white())
                            .bg(gpui_kit::black().opacity(if down { 0.75 } else { 0.5 }))
                            .shadow_md(),
                    ),
            )
            .with_priority(10_000)
            .into_any_element(),
        )
    }

    #[cfg(not(feature = "snapshot"))]
    fn demo_pointer_overlay(&self) -> Option<AnyElement> {
        None
    }
}

/// Actions the sidebar needs from the root view.
impl FlotillaApp {
    pub fn new_thread_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_thread(window, cx);
    }

    pub fn settings_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
    }
}

/// A thread another handed work to, or an agent it started in its VM, as its card shows it.
fn child_info(
    c: &flotilla_proto::ChildCard,
    catalog: Option<&Catalog>,
) -> crate::thread_view::Child {
    use flotilla_proto::ChildState;
    let model = model_label(catalog, &c.provider, &c.model).unwrap_or_else(|| c.model.clone());
    let state = match c.state {
        ChildState::Starting => "Starting",
        ChildState::Working => "Working",
        ChildState::Done => "Done",
        ChildState::Error => "Error",
        ChildState::Gone => "Gone",
        ChildState::DidntStart => "Didn't start",
        ChildState::Failed => "Failed",
        ChildState::Stopped => "Stopped",
    };
    crate::thread_view::Child {
        title: c.title.clone(),
        agent: format!("{} · {model}", crate::fmt::provider_name(&c.provider)),
        state: state.into(),
        running: c.state.running(),
        task: c.task.clone(),
        calls: c.calls,
        latest: c.latest.clone(),
        answer: c.answer.clone(),
        started_ms: c.started_ms,
        duration_ms: c.duration_ms,
        thread: c.thread,
    }
}

/// A thread just asked for, as its sidebar row: starting, its first prompt on its way.
fn placeholder_row(req: &flotilla_proto::NewThread) -> ThreadSummary {
    let now = flotilla_model::util::now_iso();
    let thread = flotilla_proto::Thread {
        id: req.id.clone().unwrap_or_default(),
        environment_id: None,
        environment_name: Some(req.environment.clone()),
        title: req
            .title
            .clone()
            .unwrap_or_else(|| flotilla_model::lifecycle::default_title(&req.prompt)),
        sandbox_id: None,
        conversation_id: None,
        provider: req.provider.clone(),
        model: req.model.clone(),
        effort: req.effort.clone(),
        fast: req.fast,
        state: ThreadState::Creating,
        keep_running: false,
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: now.clone(),
        updated_at: now,
        repo_project_id: req.project_id.clone(),
        branch: req.on_branch.as_ref().map(|b| b.name.clone()),
        base_sha: None,
    };
    let now_ms = flotilla_model::util::now_ms();
    ThreadSummary {
        thread,
        activity: Activity::Working,
        last_activity_ms: now_ms,
        turn_started_ms: Some(now_ms),
        last_reply: None,
        last_prompt: Some(flotilla_model::util::one_line(&req.prompt)),
        pr: None,
        start_failed: false,
        prompted_ms: Some(now_ms),
        opens: None,
    }
}

/// A thread just asked for, as its page shows it: its first prompt, sending.
fn placeholder_detail(req: &flotilla_proto::NewThread) -> ThreadDetail {
    let row = placeholder_row(req);
    let key = format!("asked-{}", row.thread.id);
    ThreadDetail {
        id: row.thread.id.clone(),
        messages: vec![flotilla_proto::Message {
            kind: "prompt".into(),
            task_id: Some(key.clone()),
            text: Some(req.prompt.clone()),
            status: Some("sending".into()),
            event_key: Some(key),
            timestamp: row.turn_started_ms,
            attachments: req.attachments.iter().map(|a| a.name.clone()).collect(),
            ..Default::default()
        }],
        older: 0,
        pages: vec![],
        thread: Some(row.thread),
        sent: vec![],
        start_error: None,
        stages: None,
        project: None,
        resume: None,
        archive: None,
        setup_failure: None,
        services: None,
        public: vec![],
        lineage: None,
        context: None,
        parent: None,
        children: vec![],
    }
}

/// A model as the catalog names it ("Haiku 5.5").
fn model_label(catalog: Option<&Catalog>, provider: &str, model: &str) -> Option<String> {
    catalog?
        .provider(provider)?
        .models
        .iter()
        .find(|m| m.id == model)?
        .label
        .clone()
}

/// How summaries are doing, as your settings say (with none chosen: off).
fn summary_state(s: &flotilla_proto::Settings) -> SummaryState {
    if s.summary_model.trim().is_empty() {
        return SummaryState::Off;
    }
    match &s.summaries {
        flotilla_proto::SummaryStatus::Working(at) => SummaryState::Working(at.clone()),
        flotilla_proto::SummaryStatus::Failed(e) => SummaryState::Failed(e.clone()),
        flotilla_proto::SummaryStatus::Unknown => SummaryState::Unknown,
    }
}

/// Requests whose failures were never reported one by one (background reads, settings): a
/// log line, not a toast.
fn quiet(req: &Request) -> bool {
    matches!(
        req,
        Request::MarkViewed(_)
            | Request::Opened { .. }
            | Request::MarkUnread(_)
            | Request::Unarchive(_)
            | Request::DiscardUnsent { .. }
            | Request::ReadMentions(_)
            | Request::ProjectMentions(_)
            | Request::CheckGitHub
            | Request::ListRepos
            | Request::CachedRepos
            | Request::Suggest(_)
            | Request::ListBranches(_)
            | Request::SyncNow
            | Request::TestSummaries
            | Request::Set(_)
            | Request::SetInstructions(_)
            | Request::RunDoctor
            | Request::ReadUsage
            | Request::ReadAgentUsage
            | Request::Services(_)
            | Request::RunSetup { .. }
            | Request::CancelSetup(_)
            | Request::Forget(_)
            | Request::SuggestName(_)
    )
}

/// A sidebar group's name in this device's file.
fn group_name(g: Activity) -> &'static str {
    match g {
        Activity::Unread => "unread",
        Activity::Idle => "idle",
        Activity::Working => "working",
        Activity::Archived => "archived",
        Activity::Expired => "expired",
    }
}

fn group_of(name: &str) -> Option<Activity> {
    [
        Activity::Unread,
        Activity::Idle,
        Activity::Working,
        Activity::Archived,
        Activity::Expired,
    ]
    .into_iter()
    .find(|g| group_name(*g) == name)
}

/// Put `text` on the clipboard and say what was copied.
fn copy(text: String, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
    window.push_notification(format!("Copied {text}"), cx);
}

/// Follow macOS, or fix light or dark.
fn apply_appearance(appearance: Appearance, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::ThemeMode;
    match appearance {
        Appearance::System => Theme::sync_system_appearance(Some(window), cx),
        Appearance::Light => Theme::change(ThemeMode::Light, Some(window), cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
    }
    window.refresh();
}

/// "Connect to a server…", or with one paired, "Connect to a different server…" (it takes
/// that one's place).
pub(crate) fn connect_label(paired: Option<&SavedServer>) -> &'static str {
    if paired.is_some() {
        "Connect to a different server…"
    } else {
        "Connect to a server…"
    }
}

/// Paired with `saved`, in place of the server before ([`crate::device::Replaced`]).
fn pair_in_place(d: &mut Device, saved: SavedServer) {
    let id = saved.id.clone();
    match d.pair(saved) {
        crate::device::Replaced::Nothing => {}
        crate::device::Replaced::Forget(old) => crate::backend::forget(old),
        crate::device::Replaced::Revoke(old) => crate::backend::leave(
            old,
            flotilla_client::tokens::device_token(&id)
                .ok()
                .flatten()
                .map(Secret),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::Expected;
    use flotilla_proto::{Activity, Thread, ThreadState, ThreadSummary};

    fn row(state: ThreadState, activity: Activity) -> ThreadSummary {
        let thread = Thread {
            id: "t1".into(),
            environment_id: None,
            environment_name: None,
            title: "Reply with just the word two.".into(),
            sandbox_id: None,
            conversation_id: None,
            provider: "claude-code".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state,
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
            turn_started_ms: Some(1_000),
            last_reply: None,
            last_prompt: None,
            pr: None,
            start_failed: false,
            prompted_ms: None,
            opens: None,
        }
    }

    /// Stop pressed as a thread starts (its first prompt still sending): from that moment it's
    /// stopping, not at work, though the server takes seconds to say so. Another change
    /// expected (a rename) leaves a turn at work.
    #[test]
    fn a_vm_asked_to_stop_isnt_at_work() {
        let stop = Expected {
            state: Some(ThreadState::Stopping),
            ..Default::default()
        };
        let mut starting = row(ThreadState::Creating, Activity::Working);
        stop.apply_row(&mut starting);
        assert_eq!(
            (
                starting.thread.state,
                starting.activity,
                starting.turn_started_ms
            ),
            (ThreadState::Stopping, Activity::Idle, None)
        );
        let rename = Expected {
            title: Some("Two".into()),
            ..Default::default()
        };
        let mut working = row(ThreadState::Running, Activity::Working);
        rename.apply_row(&mut working);
        assert_eq!(
            (
                working.thread.title.as_str(),
                working.activity,
                working.turn_started_ms
            ),
            ("Two", Activity::Working, Some(1_000))
        );
        // Archived, it stays archived.
        let mut archived = row(ThreadState::Idle, Activity::Archived);
        stop.apply_row(&mut archived);
        assert_eq!(archived.activity, Activity::Archived);
    }
}

/// A path for a message, with the home folder as `~`.
fn kept_at(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod kept_at_tests {
    #[test]
    fn the_home_folder_is_a_tilde() {
        let home = std::env::var("HOME").unwrap();
        let path = format!("{home}/.local/share/Flotilla/secrets/boat-key");
        assert_eq!(
            super::kept_at(&path),
            "~/.local/share/Flotilla/secrets/boat-key"
        );
        assert_eq!(
            super::kept_at("/etc/flotilla/boat-key"),
            "/etc/flotilla/boat-key"
        );
    }
}
