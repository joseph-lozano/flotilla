//! One thread: header (locked settings, VM state, keep-running), messages, composer.

use crate::assets::provider_logo;
use crate::fmt;
use crate::mention_menu::Offer;
use crate::subagents::{self, SubCard};
use crate::thread_menu::{self, ThreadCommand};
use flotilla_model::agents::Hook;
use flotilla_model::lifecycle::days_left;
use flotilla_model::normalize::PROMPT_NOT_KEPT;
use flotilla_model::util::{ellipsize, now_ms};
use flotilla_proto::{
    Attached, HookRun, Lineage, Mentions, Message, PublicFor, PublicLink, RawEvent, Request,
    ServiceCmd, ServiceRun, Thread, ThreadServices, ThreadState, ThreadSummary,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Selectable as _, StyledExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub enum ThreadViewEvent {
    /// Ask for something about the thread (shown at once where it can be: see `perform`).
    Act(Request),
    /// Send a message (a steer while a turn runs).
    Send {
        thread: String,
        text: String,
        files: Vec<Attached>,
    },
    /// Answer a question the agent waits on (`ask`: its id), one answer per question.
    Answer {
        thread: String,
        ask: String,
        answers: Vec<Vec<String>>,
    },
    /// A command from the ⋯ menu, handled like the sidebar's right-click menu.
    Command(String, ThreadCommand),
    /// A prompt for a thread whose VM is gone: it goes to a new thread on its branch.
    OnBranch { thread: String, prompt: String },
    /// Start, restart or stop one of the thread's services.
    Services(ServiceCmd),
    /// A message a quit cut short that boat never got (`id`): send it again, or let it go.
    Unsent {
        thread: String,
        id: String,
        text: String,
        again: bool,
    },
    /// The composer's `/` `$` `@` menu opened: read the thread's skills and files again if
    /// they're a while old.
    ReadMentions(String),
    /// Open another thread: one this one handed work to, or the one that handed it this.
    Open(String),
    /// Scrolled up near the oldest message shown while there are older: fetch the next page.
    Older,
    /// Send later: the composer's text (and this thread) into a schedule's form.
    Later { thread: String, text: String },
    /// Show this scheduled prompt in Scheduled.
    Scheduled(String),
    /// A tool output that went out cut: fetch its cut strings whole (`key` names it, see
    /// [`cut_key`]), for [`ThreadView::set_whole`].
    Whole {
        key: String,
        seq: i64,
        cuts: Vec<flotilla_proto::Cut>,
    },
}

/// How close to the oldest row shown a scroll up asks for the next page.
const NEAR_TOP: usize = 12;

/// A cut tool output's whole text, as its row shows it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Whole {
    /// Cut, and not asked for.
    Cut,
    Loading,
    /// What the expanded row shows, from the whole strings.
    Text(String),
    Failed(String),
}

/// What names a cut message's whole text: its cut strings' marks.
pub fn cut_key(m: &Message) -> String {
    m.cut
        .iter()
        .map(|c| c.mark.as_str())
        .collect::<Vec<_>>()
        .join("+")
}

use crate::drafts::Draft;

/// A thread another handed work to, as the one that handed it shows it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Child {
    pub title: String,
    /// Its agent and model, as the sidebar says them.
    pub agent: String,
    /// How it's going: Starting, Working, Done, Stopped…
    pub state: String,
    /// It's starting or at work.
    pub running: bool,
    /// The task it was handed, and its latest turn: its calls, the latest of them (name,
    /// summary) while it works, its answer once done, and when it started or how long it took.
    pub task: String,
    pub calls: usize,
    pub latest: Option<(String, String)>,
    pub answer: Option<String>,
    pub started_ms: Option<i64>,
    pub duration_ms: Option<u64>,
    /// A thread of its own to open (a delegate), not another conversation in this VM.
    pub thread: bool,
}

/// A `flotilla delegate` call's card, drawn as a subagent's: the thread it started.
#[derive(Clone, PartialEq, Hash)]
struct HandOff {
    child: String,
    card: SubCard,
    running: bool,
    started_ms: Option<i64>,
    /// It has a thread of its own to open.
    thread: bool,
}

#[derive(Clone)]
enum Row {
    /// `actions`: show the hover row (copy, time) under it. `nested`: one of a subagent's own
    /// calls, under its card. `sub`: this call started a subagent: show its card. `files`: a
    /// prompt's files as this Mac sent them.
    /// `from`: a child's task, as its parent (id, title) handed it.
    Message {
        msg: Box<Message>,
        ident: String,
        expanded: bool,
        actions: bool,
        nested: bool,
        sub: Option<Rc<SubCard>>,
        files: Vec<Attached>,
        handoff: Option<Rc<HandOff>>,
        from: Option<Rc<(String, String)>>,
        /// A tool output that went out cut: whether it's been fetched whole.
        whole: Option<Rc<Whole>>,
        /// A prompt a schedule sent: the schedule, and its timing in words if it's still there.
        scheduled: Option<Rc<(String, Option<String>)>>,
        /// A question the agent waits on: your answer in the making.
        ask: Option<Rc<crate::ask_card::AskShown>>,
        /// A question you answered: how the message that took your answer is going (it isn't
        /// shown itself), where there's more to say than that it was answered.
        answer: Option<String>,
    },
    /// What an open subagent card's subagent reported, after its calls.
    Report { ident: String, text: String },
    /// Calls one after another, folded into one line ("Called Bash 9 times"); open, they
    /// follow on their own rows, on a rail under it.
    Calls {
        ident: String,
        calls: Rc<Calls>,
        expanded: bool,
    },
    /// Streaming text, not yet a message.
    Partial { id: String, text: String },
}

impl Row {
    fn ident(&self) -> &str {
        match self {
            Row::Message { ident, .. } | Row::Report { ident, .. } | Row::Calls { ident, .. } => {
                ident
            }
            Row::Partial { id, .. } => id,
        }
    }

    fn signature(&self) -> u64 {
        let mut h = DefaultHasher::new();
        match self {
            Row::Message {
                msg,
                ident,
                expanded,
                actions,
                nested,
                sub,
                files,
                handoff,
                from,
                whole,
                scheduled,
                ask,
                answer,
            } => {
                (
                    ident,
                    &msg.text,
                    &msg.status,
                    msg.pending,
                    msg.is_error,
                    &msg.tool_result_json,
                    &msg.attachments,
                )
                    .hash(&mut h);
                (
                    expanded,
                    actions,
                    nested,
                    sub.as_deref(),
                    files,
                    handoff.as_deref(),
                    from.as_deref(),
                    whole.as_deref(),
                    scheduled.as_deref(),
                    ask.as_deref(),
                    answer,
                )
                    .hash(&mut h)
            }
            Row::Report { ident, text } => (ident, text).hash(&mut h),
            Row::Calls {
                ident,
                calls,
                expanded,
            } => (ident, calls.as_ref(), expanded).hash(&mut h),
            Row::Partial { id, text } => (id, text).hash(&mut h),
        }
        h.finish()
    }
}

/// A run of calls folded into one row, as its line says it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Calls {
    /// "Called Bash 9 times", "Called Bash 6 times, Read twice".
    label: String,
    /// Its calls' messages, by seq: a search hit among them opens it.
    seqs: Vec<i64>,
    /// While one runs: its summary, to see what it's at.
    running: Option<String>,
    /// How many failed.
    failed: usize,
    /// The last one's status in words (interrupted…), once none runs.
    status: Option<String>,
}

/// Calls in a run this long or longer fold into one row.
const FOLD_CALLS: usize = 2;

/// "Called Bash 9 times", or for a run of several tools each with its count, in the order
/// they were first called: "Called Bash 6 times, Read twice, Grep once".
fn called_label<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for name in names {
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, k)) => *k += 1,
            None => counts.push((name, 1)),
        }
    }
    let said: Vec<String> = counts
        .into_iter()
        .map(|(name, k)| match k {
            1 => format!("{name} once"),
            2 => format!("{name} twice"),
            k => format!("{name} {k} times"),
        })
        .collect();
    format!("Called {}", said.join(", "))
}

/// A row that folds into a run of calls: a call of the thread's own (not a subagent's, nor
/// one shown as a card).
fn foldable(r: &Row) -> bool {
    matches!(
        r,
        Row::Message {
            msg,
            nested: false,
            sub: None,
            handoff: None,
            ..
        } if msg.kind == "tool"
    )
}

/// Each run of `FOLD_CALLS` or more calls in a row folds into one line, which opens (it's in
/// `open`) to show them under it. `target`: a message gone to from search, whose run opens if
/// `unfold` (going back to a place doesn't open one: its line is the place).
fn fold_calls(
    rows: Vec<Row>,
    open: &mut HashSet<String>,
    target: Option<i64>,
    unfold: bool,
) -> Vec<Row> {
    let mut out = Vec::with_capacity(rows.len());
    let mut rows = rows.into_iter().peekable();
    while let Some(row) = rows.next() {
        if !foldable(&row) {
            out.push(row);
            continue;
        }
        let mut run = vec![row];
        while let Some(next) = rows.next_if(foldable) {
            run.push(next);
        }
        if run.len() < FOLD_CALLS {
            out.extend(run);
            continue;
        }
        let msgs: Vec<&Message> = run
            .iter()
            .filter_map(|r| match r {
                Row::Message { msg, .. } => Some(&**msg),
                _ => None,
            })
            .collect();
        let ident = format!("calls:{}", run[0].ident());
        let seqs: Vec<i64> = msgs.iter().map(|m| m.seq).collect();
        if unfold && target.is_some_and(|t| seqs.contains(&t)) {
            open.insert(ident.clone());
        }
        let running = msgs
            .iter()
            .rev()
            .find(|m| m.pending)
            .map(|m| m.tool_summary.clone().unwrap_or_default());
        let calls = Calls {
            label: called_label(
                msgs.iter()
                    .map(|m| m.tool_name.as_deref().unwrap_or("tool")),
            ),
            running: running.clone(),
            failed: msgs.iter().filter(|m| m.is_error).count(),
            status: msgs
                .last()
                .and_then(|m| m.status.clone())
                .filter(|_| running.is_none()),
            seqs,
        };
        let expanded = open.contains(&ident);
        out.push(Row::Calls {
            ident,
            calls: Rc::new(calls),
            expanded,
        });
        if expanded {
            out.extend(run.into_iter().map(|mut r| {
                if let Row::Message { nested, .. } = &mut r {
                    *nested = true;
                }
                r
            }));
        }
    }
    out
}

pub struct ThreadView {
    id: String,
    thread: Option<Thread>,
    summary: Option<ThreadSummary>,
    messages: Vec<Message>,
    partials: Vec<RawEvent>,
    turn_started_ms: Option<i64>,
    expanded: HashSet<String>,
    /// Subagents still running: their cards' idents, the cards, and when each started. They
    /// stay pinned under the header however far the thread scrolls on.
    pinned: Vec<(String, Rc<SubCard>, Option<i64>)>,
    /// Pinned cards shown one by one (`true`) or folded into a line, once chosen; until then,
    /// one is shown and several are folded.
    pinned_open: Option<bool>,
    /// The running subagents' calls together, and the latest of them, for the folded line.
    pinned_calls: (usize, Option<(String, String)>),
    /// The thread's subagents as its harness recorded them in the VM, when read.
    lineage: Option<Lineage>,
    /// How full its context is, as its harness last recorded it, and the agent accounts'
    /// limits (to say when its account is used up).
    context: Option<flotilla_proto::ContextUse>,
    limits: Vec<flotilla_proto::AccountLimits>,
    /// The files of the prompts this Mac sent, by prompt id.
    sent_files: HashMap<String, Vec<Attached>>,
    /// The prompts schedules sent: sent id → schedule.
    sent_schedules: HashMap<String, String>,
    /// The scheduled prompts that send to this thread.
    schedules: Vec<flotilla_proto::Schedule>,
    /// The `/` `$` `@` menu, and what it offers: the thread's skills and files as last read
    /// in its VM, ready to search for its harness.
    menu: crate::mention_menu::Menu,
    mentions: Option<Arc<Mentions>>,
    offer: Option<Rc<Offer>>,
    /// The composer's caret and length when the menu last looked.
    caret: (usize, usize),
    /// Each thread this one handed work to: its card's place (and the subagent card it sits
    /// in, if one), by the child's id.
    handoffs: HashMap<String, (String, Option<String>)>,
    /// The thread that handed this one its task (id, title), and the threads this one handed
    /// work to, by id.
    parent: Option<(String, String)>,
    children: HashMap<String, Child>,
    rows: Rc<Vec<Row>>,
    signatures: Vec<u64>,
    scroller: Entity<MessageScrollerState>,
    /// Older messages than those shown exist, and whether a page of them is on its way.
    older: (bool, bool),
    /// A message to go to (a search hit), and since when: scrolled to and marked once its row
    /// is here (given up on after `TARGET_FOR`, as for a message that's gone since).
    target: Option<(i64, Instant)>,
    /// The target is where you were (coming back to the thread): gone to, not marked.
    target_quiet: bool,
    /// The row marked for having been gone to, and since when.
    flash: Option<(String, Instant)>,
    /// Cut tool outputs fetched whole (or on their way), by [`cut_key`]; those shown whole
    /// (Show all; Copy all only copies); and those to copy once they come.
    wholes: HashMap<String, Whole>,
    shown_whole: HashSet<String>,
    copy_whole: HashSet<String>,
    composer: Entity<TextareaState>,
    /// Files to go with the next prompt.
    attachments: crate::attachments::Pending,
    /// The composer placeholder last set, which depends on the thread's state.
    placeholder: &'static str,
    /// The row whose copy button was just used, shown as a check for a moment.
    copied: Option<String>,
    /// While ↑/↓ walk earlier prompts: how far back (0 = the latest).
    recall: Option<usize>,
    /// Stop was pressed; shown until the turn ends (or the interrupt fails).
    interrupting: bool,
    /// The server can't be reached: what's sent waits here for it.
    server_down: bool,
    /// Why the thread's start failed, when it has no VM.
    start_error: Option<String>,
    /// A project thread's project, and how far its base is behind the default branch.
    project: Option<(flotilla_proto::Project, Option<i64>, String)>,
    /// A starting project thread's progress.
    start_stages: Option<flotilla_proto::StartStages>,
    /// The repo's `.agents/resume` from this VM's last wake, and whether its output is open.
    resume: Option<HookRun>,
    resume_open: bool,
    archive: Option<HookRun>,
    archive_open: bool,
    /// The setup script that failed when the thread started, and whether its output is open.
    setup_failure: Option<flotilla_proto::ScriptFailure>,
    setup_open: bool,
    /// The PR bar's checks list is open.
    checks_open: bool,
    /// The thread's services (`.agents/services.yml`) as last seen, and whether Preview's menu
    /// of them is open.
    services: Option<ThreadServices>,
    services_open: bool,
    /// Its ports whose links anyone may open now, by port.
    public: Vec<PublicLink>,
    /// The model as people say it ("Haiku 5.5"), from the catalog.
    model_label: Option<String>,
    /// The PR bar's press-and-hold merge button.
    hold: Hold,
    /// When the hold filled and the merge was asked for: "Merging…" until GitHub says so.
    merging_since: Option<Instant>,
    /// Your answers in the making to the questions the agent waits on, by question id.
    ask_drafts: HashMap<String, crate::ask_card::AskDraft>,
    _subscriptions: Vec<Subscription>,
}

/// Hold to merge: the press is deliberate (linear, since it's progress), letting go early is
/// quick (a strong ease-out drain).
const HOLD_TO_MERGE: Duration = Duration::from_millis(1200);
const HOLD_DRAIN: Duration = Duration::from_millis(200);
/// How long "Merging…" shows while GitHub hasn't reported the merge.
const MERGING_FOR: Duration = Duration::from_secs(30);
/// A message gone to from search is marked this long, then fades this fast.
const FLASH_HOLD: Duration = Duration::from_millis(1400);
const FLASH_FADE: Duration = Duration::from_millis(500);
/// How long a message gone to is waited for once no older page is on its way.
const TARGET_FOR: Duration = Duration::from_secs(30);
/// A prompt sent while the server can't be reached, until it can.
const WAITING_FOR_SERVER: &str = "waiting for the server";

/// What a prompt's or steer's status says, given what's around it: a start that failed (it
/// was never sent), a server out of reach (it's held here), the VM starting ("queued" for
/// nothing but the VM: a new thread's first prompt, or one that woke it).
fn shown_status(
    status: Option<String>,
    first_prompt: bool,
    vm_starting: bool,
    unsent: bool,
    server_down: bool,
) -> Option<String> {
    match status.as_deref() {
        Some("sending") if unsent => Some(flotilla_model::normalize::NOT_SENT.into()),
        Some("sending") if server_down => Some(WAITING_FOR_SERVER.into()),
        Some("queued") if first_prompt || vm_starting => Some("starting".into()),
        _ => status,
    }
}

/// The prompts and steers (by seq) that took your answers to questions whose cards are among
/// `messages`: each card stands for its answer's message. One whose card isn't here (on an
/// older page not loaded yet) is still shown.
fn hidden_answers<'a>(messages: impl Iterator<Item = &'a Message> + Clone) -> HashSet<i64> {
    let cards: HashSet<&str> = messages
        .clone()
        .filter(|m| m.kind == flotilla_proto::ask::KIND)
        .filter_map(|m| m.tool_id.as_deref())
        .collect();
    messages
        .filter(|m| {
            matches!(m.kind.as_str(), "prompt" | "steer")
                && m.answer_to.as_deref().is_some_and(|a| cards.contains(a))
        })
        .map(|m| m.seq)
        .collect()
}

/// What an answered question's card says of the message that took the answer (`status`, as
/// shown): a prompt's turn as its bubble would say it, until it's finished; a steer only until
/// it's delivered.
fn answer_status(m: &Message, status: Option<String>) -> Option<String> {
    let status = status?;
    match m.kind.as_str() {
        "steer"
            if !matches!(
                status.as_str(),
                "sending" | WAITING_FOR_SERVER | flotilla_model::normalize::NOT_SENT
            ) =>
        {
            None
        }
        _ if status == "finished" => None,
        _ => Some(status),
    }
}

/// Room under the last row: as tall as Jump to latest and its gap, so at the end it never
/// covers words.
const LIST_END_ROOM: Pixels = px(48.);

/// The hold-to-merge button: when the press started, or, after letting go early, how full it
/// was and when it started draining.
#[derive(Default)]
struct Hold {
    pressed: Option<Instant>,
    drain: Option<(Instant, f32)>,
    /// The PR the press is for: a refresh that changes it, or makes it unmergeable, lets go.
    pr: Option<i64>,
}

impl Hold {
    /// How full the button is, 0 to 1.
    fn progress(&self, now: Instant) -> f32 {
        if let Some(at) = self.pressed {
            return (now.duration_since(at).as_secs_f32() / HOLD_TO_MERGE.as_secs_f32()).min(1.);
        }
        match self.drain {
            Some((at, from)) => {
                let t = (now.duration_since(at).as_secs_f32() / HOLD_DRAIN.as_secs_f32()).min(1.);
                from * (1. - ease_out(t))
            }
            None => 0.,
        }
    }

    fn animating(&self, now: Instant) -> bool {
        self.pressed.is_some()
            || self
                .drain
                .is_some_and(|(at, _)| now.duration_since(at) < HOLD_DRAIN)
    }
}

/// `cubic-bezier(0.23, 1, 0.32, 1)`, a strong ease-out, at `x` (0 to 1).
fn ease_out(x: f32) -> f32 {
    cubic_bezier(0.23, 1., 0.32, 1., x)
}

/// A CSS cubic-bezier curve's y at `x`, its ends at (0, 0) and (1, 1).
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let at = |a: f32, b: f32, t: f32| {
        3. * a * (1. - t) * (1. - t) * t + 3. * b * (1. - t) * t * t + t * t * t
    };
    // x(t) rises with t when x1 and x2 are in 0..1: bisect for the t that gives x.
    let (mut lo, mut hi) = (0f32, 1f32);
    for _ in 0..32 {
        let mid = (lo + hi) / 2.;
        if at(x1, x2, mid) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(y1, y2, (lo + hi) / 2.)
}

impl EventEmitter<ThreadViewEvent> for ThreadView {}

impl ThreadView {
    pub fn new(id: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .placeholder("Ask a follow-up…")
                .submit_on_enter(true)
        });
        let sub = cx.subscribe_in(
            &composer,
            window,
            |this, _, event, window, cx| match event {
                InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                InputEvent::Change => this.follow_composer(cx),
                InputEvent::Blur => {
                    this.menu.close();
                    cx.notify();
                }
                _ => {}
            },
        );
        composer.update(cx, |c, cx| c.focus(window, cx));
        // The arrows and clicks move the caret without an edit: the menu follows it too.
        let caret = cx.observe(&composer, |this, _, cx| this.caret_moved(cx));
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        // Scrolled up near the oldest row shown: the next page of older messages.
        let near = cx.observe(&scroller, |this, scroller, cx| {
            let s = scroller.read(cx);
            if this.older.0
                && !this.older.1
                && !s.is_following_tail()
                && s.logical_scroll_top().item_ix < NEAR_TOP
            {
                cx.emit(ThreadViewEvent::Older);
            }
        });
        ThreadView {
            id,
            thread: None,
            summary: None,
            messages: vec![],
            partials: vec![],
            turn_started_ms: None,
            pinned: Vec::new(),
            pinned_open: None,
            pinned_calls: (0, None),
            lineage: None,
            context: None,
            limits: Vec::new(),
            sent_files: HashMap::new(),
            sent_schedules: HashMap::new(),
            schedules: Vec::new(),
            menu: Default::default(),
            mentions: None,
            offer: None,
            caret: (0, 0),
            handoffs: HashMap::new(),
            parent: None,
            children: HashMap::new(),
            expanded: HashSet::new(),
            rows: Rc::new(vec![]),
            signatures: vec![],
            scroller,
            older: (false, false),
            target: None,
            target_quiet: false,
            flash: None,
            wholes: HashMap::new(),
            shown_whole: HashSet::new(),
            copy_whole: HashSet::new(),
            composer,
            attachments: Default::default(),
            placeholder: "Ask a follow-up…",
            copied: None,
            recall: None,
            interrupting: false,
            server_down: false,
            start_error: None,
            project: None,
            start_stages: None,
            resume: None,
            resume_open: false,
            archive: None,
            archive_open: false,
            setup_failure: None,
            setup_open: false,
            checks_open: false,
            services: None,
            services_open: false,
            public: Vec::new(),
            model_label: None,
            hold: Hold::default(),
            merging_since: None,
            ask_drafts: HashMap::new(),
            _subscriptions: vec![sub, caret, near],
        }
    }

    /// Go to the message `seq` (a search hit) once it's shown: its row comes to the top of the
    /// list, marked for a moment. Older pages it's on come first (the app asks for them).
    pub fn jump_to(&mut self, seq: i64, _window: &mut Window, cx: &mut Context<Self>) {
        self.target = Some((seq, Instant::now()));
        self.target_quiet = false;
        self.rebuild_rows(cx);
    }

    /// Back where you were in this thread: the message `seq` at the top again (unmarked), its
    /// pages fetched first as for a search hit.
    pub fn return_to(&mut self, seq: i64, cx: &mut Context<Self>) {
        self.target = Some((seq, Instant::now()));
        self.target_quiet = true;
        self.rebuild_rows(cx);
    }

    /// Where you are, to come back to: the first message at the top of what's shown; none at
    /// the end (following what comes).
    pub fn place(&self, cx: &App) -> Option<i64> {
        let s = self.scroller.read(cx);
        if s.is_following_tail() {
            return None;
        }
        self.rows
            .get(s.logical_scroll_top().item_ix..)?
            .iter()
            .find_map(|r| match r {
                Row::Message { msg, .. } => Some(msg.seq),
                Row::Calls { calls, .. } => calls.seqs.first().copied(),
                _ => None,
            })
    }

    /// The server can be reached, or no longer: prompts held here say so.
    pub fn set_server_down(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.server_down != down {
            self.server_down = down;
            self.rebuild_rows(cx);
        }
    }

    /// Whether there are older messages than those shown, and whether some are on their way.
    pub fn set_older(&mut self, more: bool, loading: bool, cx: &mut Context<Self>) {
        if self.older != (more, loading) {
            self.older = (more, loading);
            cx.notify();
        }
    }

    /// A cut tool output's strings, whole (field, text), or why not.
    pub fn set_whole(
        &mut self,
        key: &str,
        got: Result<Vec<(String, String)>, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(m) = self
            .messages
            .iter()
            .find(|m| !m.cut.is_empty() && cut_key(m) == key)
        else {
            return;
        };
        let whole = match got {
            Ok(strings) => {
                let mut m = m.clone();
                for (field, text) in strings {
                    match field.as_str() {
                        "tool_input_json" => m.tool_input_json = Some(text),
                        _ => m.tool_result_json = Some(text),
                    }
                }
                Whole::Text(tool_text(&m, true))
            }
            Err(e) => Whole::Failed(e),
        };
        if let Whole::Text(text) = &whole
            && self.copy_whole.remove(key)
        {
            self.copy_text(key, text.clone(), cx);
        }
        self.copy_whole.remove(key);
        self.wholes.insert(key.to_string(), whole);
        self.rebuild_rows(cx);
    }

    /// Copy a cut tool output's whole text, its row's button showing it did.
    fn copy_text(&mut self, key: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if let Some(ident) = self.rows.iter().find_map(|r| match r {
            Row::Message { msg, ident, .. } if cut_key(msg) == key => Some(ident.clone()),
            _ => None,
        }) {
            self.mark_copied(ident, cx);
        }
    }

    /// Show a cut tool output whole, or (`copy`) copy it whole and leave it cut, fetching it
    /// if need be.
    fn ask_whole(&mut self, m: &Message, copy: bool, cx: &mut Context<Self>) {
        let key = cut_key(m);
        if !copy {
            self.shown_whole.insert(key.clone());
        }
        if let (true, Some(Whole::Text(text))) = (copy, self.wholes.get(&key)) {
            self.copy_text(&key, text.clone(), cx);
            return;
        }
        if copy {
            self.copy_whole.insert(key.clone());
        }
        if !matches!(self.wholes.get(&key), Some(Whole::Loading | Whole::Text(_))) {
            self.wholes.insert(key.clone(), Whole::Loading);
            cx.emit(ThreadViewEvent::Whole {
                key,
                seq: m.seq,
                cuts: m.cut.clone(),
            });
        }
        self.rebuild_rows(cx);
    }

    /// Show less: back to the cut output (its whole text kept, for Show all or Copy all).
    fn cut_again(&mut self, m: &Message, cx: &mut Context<Self>) {
        if self.shown_whole.remove(&cut_key(m)) {
            self.rebuild_rows(cx);
        }
    }

    pub fn set_data(
        &mut self,
        thread: Option<Thread>,
        messages: Vec<Message>,
        summary: Option<ThreadSummary>,
        cx: &mut Context<Self>,
    ) {
        let turn_started_ms = summary.as_ref().and_then(|s| s.turn_started_ms);
        self.thread = thread;
        self.summary = summary;
        // A refresh took the PR away from a press under way (checks went red, another PR):
        // let go, as the button may be gone and can't see the release.
        if self.hold.pressed.is_some() && self.mergeable_pr() != self.hold.pr {
            self.release_merge(cx);
        }
        self.messages = messages;
        // An answer in the making goes once its question stops waiting (answered elsewhere, or
        // passed); one on its way stays until the server says it went (it shows answered
        // meanwhile, and comes back if it didn't go), and one in an older page stays.
        let waiting: HashSet<&str> = self
            .messages
            .iter()
            .filter(|m| {
                m.kind == flotilla_proto::ask::KIND
                    && m.status.as_deref() == Some(flotilla_proto::ask::WAITING)
            })
            .filter_map(|m| m.tool_id.as_deref())
            .collect();
        let shown: HashSet<&str> = self
            .messages
            .iter()
            .filter(|m| m.kind == flotilla_proto::ask::KIND)
            .filter_map(|m| m.tool_id.as_deref())
            .collect();
        self.ask_drafts.retain(|id, d| {
            d.sending || waiting.contains(id.as_str()) || !shown.contains(id.as_str())
        });
        self.turn_started_ms = turn_started_ms;
        if turn_started_ms.is_none() {
            self.partials.clear();
            self.interrupting = false;
        }
        self.rebuild_rows(cx);
    }

    /// The questions of the ask message `ask` (its id), if it's shown and waits.
    fn waiting_questions(&self, ask: &str) -> Option<Vec<flotilla_proto::ask::Question>> {
        self.messages
            .iter()
            .find(|m| {
                m.kind == flotilla_proto::ask::KIND
                    && m.tool_id.as_deref() == Some(ask)
                    && m.status.as_deref() == Some(flotilla_proto::ask::WAITING)
            })
            .map(flotilla_proto::ask::questions)
    }

    fn ask_draft(&mut self, ask: &str) -> Option<&mut crate::ask_card::AskDraft> {
        let n = self.waiting_questions(ask)?.len();
        Some(
            self.ask_drafts
                .entry(ask.to_string())
                .or_insert_with(|| crate::ask_card::AskDraft::new(n)),
        )
    }

    /// Pick an option of question `q` (another where several can be picked; in place of the
    /// one picked, and of what was typed, where one can).
    pub(crate) fn pick_option(
        &mut self,
        ask: &str,
        q: usize,
        label: String,
        multi: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(d) = self.ask_draft(ask).filter(|d| !d.sending) else {
            return;
        };
        let Some(picked) = d.picked.get_mut(q) else {
            return;
        };
        if multi {
            match picked.iter().position(|l| *l == label) {
                Some(i) => {
                    picked.remove(i);
                }
                None => picked.push(label),
            }
        } else {
            *picked = vec![label];
            if let Some(Some(other)) = d.other.get(q) {
                other.update(cx, |o, cx| o.set_value("", window, cx));
            }
        }
        d.error = None;
        self.rebuild_rows(cx);
    }

    /// Other: a field for your own answer to question `q`, focused (Enter sends).
    pub(crate) fn open_other(
        &mut self,
        ask: &str,
        q: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let multi = self
            .waiting_questions(ask)
            .and_then(|qs| qs.get(q).map(|q| q.multi))
            .unwrap_or(false);
        let Some(d) = self.ask_draft(ask).filter(|d| !d.sending) else {
            return;
        };
        if d.other.get(q).is_none_or(|o| o.is_some()) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Your answer"));
        let ask_id = ask.to_string();
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, _, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.send_answer(&ask_id, window, cx),
                InputEvent::Change => {
                    if let Some(d) = this.ask_drafts.get_mut(&ask_id) {
                        d.error = None;
                    }
                    cx.notify();
                }
                _ => {}
            },
        );
        self._subscriptions.push(sub);
        input.update(cx, |i, cx| i.focus(window, cx));
        if let Some(d) = self.ask_drafts.get_mut(ask) {
            // One answer to a single-choice question: what's typed, not what was picked.
            if !multi && let Some(p) = d.picked.get_mut(q) {
                p.clear();
            }
            d.other[q] = Some(input);
            d.error = None;
        }
        self.rebuild_rows(cx);
    }

    /// Send: your answers to the question `ask`, if each of its questions has one.
    pub(crate) fn send_answer(&mut self, ask: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread.as_ref().map(|t| t.id.clone()) else {
            return;
        };
        let Some(questions) = self.waiting_questions(ask) else {
            return;
        };
        let Some(d) = self.ask_draft(ask).filter(|d| !d.sending) else {
            return;
        };
        let answers = d.answers(&questions, cx);
        match flotilla_proto::ask::tidy(&questions, &answers) {
            Ok(answers) => {
                d.sending = true;
                d.error = None;
                cx.emit(ThreadViewEvent::Answer {
                    thread,
                    ask: ask.to_string(),
                    answers,
                });
            }
            Err(e) => d.error = Some(e),
        }
        self.rebuild_rows(cx);
    }

    /// The answer to `ask` went: its draft goes.
    pub fn answer_sent(&mut self, ask: &str, cx: &mut Context<Self>) {
        if self.ask_drafts.remove(ask).is_some() {
            self.rebuild_rows(cx);
        }
    }

    /// The answer to `ask` didn't go: it can be sent again, as it was.
    pub fn answer_failed(&mut self, ask: &str, cx: &mut Context<Self>) {
        if let Some(d) = self.ask_drafts.get_mut(ask) {
            d.sending = false;
            self.rebuild_rows(cx);
        }
    }

    /// The thread's subagents as its harness recorded them, read in the VM (shown from the
    /// next `set_data`).
    pub fn set_lineage(&mut self, lineage: Option<Lineage>) {
        self.lineage = lineage;
    }

    /// How full the thread's context is, and the accounts' limits (shown from the next
    /// `set_data`).
    pub fn set_context(
        &mut self,
        context: Option<flotilla_proto::ContextUse>,
        limits: Vec<flotilla_proto::AccountLimits>,
    ) {
        self.context = context;
        self.limits = limits;
    }

    /// The accounts' limits changed (the sidebar's view).
    pub fn set_limits(
        &mut self,
        limits: Vec<flotilla_proto::AccountLimits>,
        cx: &mut Context<Self>,
    ) {
        self.limits = limits;
        cx.notify();
    }

    /// The thread's id, and what's being written in its composer, if anything.
    pub fn draft(&self, cx: &App) -> (String, Option<Draft>) {
        let text = self.composer.read(cx).value().to_string();
        let files = self.attachments.files.clone();
        (self.id.clone(), Draft::of(text, files))
    }

    /// Put back what was being written when the thread was last open.
    pub fn put_draft(&mut self, draft: Draft, window: &mut Window, cx: &mut Context<Self>) {
        let last = draft.text.split('\n').count().saturating_sub(1) as u32;
        self.composer.update(cx, |c, cx| {
            c.set_value(draft.text, window, cx);
            c.set_cursor_position(
                gpui_kit::base::input::Position::new(last, u32::MAX),
                window,
                cx,
            );
        });
        self.attachments.files = draft.files;
    }

    /// Who handed this thread its task, and the threads it handed work to (shown from the next
    /// `set_data`).
    pub fn set_family(
        &mut self,
        parent: Option<(String, String)>,
        children: HashMap<String, Child>,
    ) {
        self.parent = parent;
        self.children = children;
    }

    /// The thread's skills and files for the composer's menu, as last read in its VM.
    pub fn set_mentions(&mut self, mentions: Option<Arc<Mentions>>, cx: &mut Context<Self>) {
        if mentions.as_ref().map(Arc::as_ptr) == self.mentions.as_ref().map(Arc::as_ptr) {
            return;
        }
        self.mentions = mentions;
        self.offer = None;
        if self.menu.is_open() {
            self.follow_composer(cx);
        }
    }

    /// The menu's offer for this thread's harness, made once per read.
    fn offer(&mut self) -> Option<Rc<Offer>> {
        let provider = self.thread.as_ref().map(|t| t.provider.as_str())?;
        if self.offer.as_ref().is_none_or(|o| o.provider() != provider)
            && let Some(m) = &self.mentions
        {
            self.offer = Some(Rc::new(Offer::new(provider, m)));
        }
        self.offer.clone()
    }

    /// The composer redrew: if its caret moved, the menu follows.
    fn caret_moved(&mut self, cx: &mut Context<Self>) {
        let (cursor, len) = {
            let c = self.composer.read(cx);
            (c.cursor(), c.text().len())
        };
        if self.caret != (cursor, len) {
            self.caret = (cursor, len);
            self.follow_composer(cx);
        }
    }

    /// The composer's text or caret changed: open, narrow or close the menu.
    fn follow_composer(&mut self, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let c = self.composer.read(cx);
            (c.value().to_string(), c.cursor())
        };
        let offer = self.offer();
        if self.menu.update(&text, cursor, offer.as_deref()) {
            cx.emit(ThreadViewEvent::ReadMentions(self.id.clone()));
        }
        cx.notify();
    }

    /// Whether the menu has rows for the word at the caret now: the caret may have moved
    /// without an edit (the arrows, a click), so look again first.
    fn menu_ready(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.menu.is_open() {
            return false;
        }
        self.follow_composer(cx);
        self.menu.has_items()
    }

    /// Put the menu's row `i` (or the highlighted one) in place of the word being typed.
    fn accept_mention(&mut self, i: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.menu.choice(i) else {
            return;
        };
        self.menu.close();
        self.composer.update(cx, |c, cx| {
            choice.apply(c, window, cx);
            c.focus(window, cx);
        });
        // A folder stays open: list what's in it.
        if choice.folder {
            self.follow_composer(cx);
        }
        cx.notify();
    }

    /// The cards for a tool call that ran `flotilla delegate` or `flotilla agent` (one each,
    /// however many one call started): each agent as it is now. Only agents Flotilla started
    /// get one: a line that only looks like a request (a file or a comment the agent read)
    /// started nothing.
    fn handoffs(&self, msg: &Message) -> Vec<Rc<HandOff>> {
        use flotilla_model::orchestrate::{self as orch, Request, Seen};
        orch::requests(std::slice::from_ref(msg))
            .into_iter()
            .filter_map(|s| match s {
                Seen::Whole {
                    request: Request::Delegate { id, task, .. },
                    ..
                } => Some((id, task)),
                Seen::Whole {
                    request: Request::Agent { id, task, .. },
                    ..
                } => Some((id, task)),
                _ => None,
            })
            .filter_map(|(child, task)| self.handoff(child, task))
            .collect()
    }

    fn handoff(&self, child: String, task: String) -> Option<Rc<HandOff>> {
        let info = self.children.get(&child)?.clone();
        let card = SubCard {
            description: info.title.clone(),
            kind: Some(info.agent.clone()),
            prompt: if info.task.is_empty() {
                task
            } else {
                info.task.clone()
            },
            background: false,
            report: info.answer.clone(),
            calls: Some(info.calls),
            latest: info.latest.clone(),
            duration_ms: info.duration_ms,
        };
        Some(Rc::new(HandOff {
            child,
            card,
            running: info.running,
            started_ms: info.started_ms,
            thread: info.thread,
        }))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Dev: what Send later takes from here.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_later(&self, cx: &App) -> (String, String) {
        (self.id.clone(), self.composer.read(cx).value().to_string())
    }

    /// The prompts this Mac sent (their files shown from the next `set_data`).
    pub fn set_sent(&mut self, sent: Vec<flotilla_proto::SentPrompt>) {
        self.sent_schedules = sent
            .iter()
            .filter_map(|s| Some((s.id.clone(), s.schedule.clone()?)))
            .collect();
        self.sent_files = sent
            .into_iter()
            .filter(|s| !s.attachments.is_empty())
            .map(|s| (s.id, s.attachments))
            .collect();
    }

    /// The scheduled prompts that send here (the line over the composer, and the prompts
    /// they sent).
    pub fn set_schedules(&mut self, schedules: Vec<flotilla_proto::Schedule>) {
        self.schedules = schedules;
    }

    pub fn set_partials(&mut self, partials: Vec<RawEvent>, cx: &mut Context<Self>) {
        self.partials = partials;
        self.rebuild_rows(cx);
    }

    fn working(&self) -> bool {
        self.turn_started_ms.is_some()
    }

    pub fn set_start_error(&mut self, error: Option<String>) {
        self.start_error = error;
    }

    pub fn set_project(
        &mut self,
        project: Option<(flotilla_proto::Project, Option<i64>, String)>,
        stages: Option<flotilla_proto::StartStages>,
    ) {
        self.project = project;
        self.start_stages = stages;
    }

    pub fn set_services(&mut self, services: Option<ThreadServices>) {
        self.services = services;
    }

    pub fn set_public(&mut self, public: Vec<PublicLink>) {
        self.public = public;
    }

    /// The thread's services on its current VM (after a fork or rebuild, none are), if its repo
    /// declares any or its services file has a problem.
    /// The services on the thread's VM, while it's up. A stopped VM gets plain Preview… (which
    /// wakes it first); its services carry on when it wakes.
    fn current_services(&self, t: &Thread) -> Option<&ThreadServices> {
        let up = matches!(t.state, ThreadState::Idle | ThreadState::Running);
        self.services.as_ref().filter(|s| {
            up && Some(&s.sandbox) == t.sandbox_id.as_ref()
                && (!s.services.is_empty() || s.error.is_some())
        })
    }

    /// Dev: open Preview's services menu, or a service's output.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_services(
        &mut self,
        logs: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match logs {
            Some(name) => self.open_service_logs(name.to_string(), window, cx),
            None => {
                self.services_open = true;
                cx.notify();
            }
        }
    }

    pub fn set_hooks(&mut self, resume: Option<HookRun>, archive: Option<HookRun>) {
        self.resume = resume;
        self.archive = archive;
    }

    pub fn set_setup_failure(&mut self, failure: Option<flotilla_proto::ScriptFailure>) {
        self.setup_failure = failure;
    }

    pub fn set_model_label(&mut self, label: Option<String>) {
        self.model_label = label;
    }

    /// The press on Hold to merge: when it has lasted HOLD_TO_MERGE, merge.
    fn press_merge(&mut self, cx: &mut Context<Self>) {
        if self.merging() {
            return;
        }
        let Some(pr) = self.mergeable_pr() else {
            return;
        };
        let at = Instant::now();
        self.hold = Hold {
            pressed: Some(at),
            drain: None,
            pr: Some(pr),
        };
        cx.notify();
        let id = self.id.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOLD_TO_MERGE).await;
            let _ = this.update(cx, |v, cx| {
                // Still the same press, on the same PR, and it can still merge: it's full.
                if v.hold.pressed == Some(at) && v.mergeable_pr() == Some(pr) {
                    v.hold = Hold::default();
                    v.merging_since = Some(Instant::now());
                    cx.emit(ThreadViewEvent::Act(Request::MergePr(id.clone())));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Let go of Hold to merge before it filled: drain back.
    fn release_merge(&mut self, cx: &mut Context<Self>) {
        if self.hold.pressed.is_none() {
            return;
        }
        let now = Instant::now();
        let from = self.hold.progress(now);
        self.hold = Hold {
            pressed: None,
            drain: Some((now, from)),
            pr: None,
        };
        cx.notify();
    }

    /// The open PR's number while Hold to merge can merge it.
    fn mergeable_pr(&self) -> Option<i64> {
        let (pr, _) = self.summary.as_ref()?.pr.as_ref()?;
        (pr.state == "OPEN" && !pr.is_draft && pr.can_merge()).then_some(pr.number)
    }

    fn merging(&self) -> bool {
        self.merging_since
            .is_some_and(|t| t.elapsed() < MERGING_FOR)
    }

    /// Setup failed or timed out when the thread started (it started anyway): which script,
    /// with the end of its output a click away. Shown while the VM is up.
    fn render_setup_failure(&self, t: &Thread, cx: &mut Context<Self>) -> Option<AnyElement> {
        let f = self.setup_failure.as_ref()?;
        if self.start_stages.is_some()
            || !matches!(t.state, ThreadState::Idle | ThreadState::Running)
        {
            return None;
        }
        let theme = cx.theme().clone();
        let lines = flotilla_model::setup::tail(&f.output, 8);
        let open = self.setup_open && !lines.is_empty();
        Some(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(
                            Icon::new(if f.timed_out {
                                IconName::Clock
                            } else {
                                IconName::CircleX
                            })
                            .xsmall(),
                        )
                        .child(f.summary())
                        .when(!lines.is_empty(), |el| {
                            el.child(
                                Button::new("setup-output")
                                    .ghost()
                                    .xsmall()
                                    .label(if open { "Hide output" } else { "Show output" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.setup_open = !this.setup_open;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .when(open, |el| el.child(output_box(lines, cx)))
                .into_any_element(),
        )
    }

    /// The repo's `.agents/resume` from the VM's last wake, while the VM is up, only while it runs
    /// (the prompt waits up to 10 s) or if it failed or timed out. One that ran says nothing.
    fn render_resume(&self, t: &Thread, cx: &mut Context<Self>) -> Option<AnyElement> {
        let run = self
            .resume
            .as_ref()
            .filter(|r| Some(&r.sandbox) == t.sandbox_id.as_ref())?;
        // While a wake waits for it, the thread may still show as starting.
        let waking = run.state == "running" && t.state == ThreadState::Starting;
        if !waking && !matches!(t.state, ThreadState::Idle | ThreadState::Running) {
            return None;
        }
        if run.state == "done" {
            return None;
        }
        Some(self.render_hook(Hook::Resume, run, self.resume_open, cx))
    }

    /// The repo's `.agents/archive`: while it runs, and on an archived thread if it failed or
    /// timed out. One that ran says nothing.
    fn render_archive(&self, t: &Thread, cx: &mut Context<Self>) -> Option<AnyElement> {
        let run = self
            .archive
            .as_ref()
            .filter(|r| Some(&r.sandbox) == t.sandbox_id.as_ref())?;
        let archived = self
            .summary
            .as_ref()
            .is_some_and(|s| s.activity == flotilla_proto::Activity::Archived);
        let failed = matches!(run.state.as_str(), "failed" | "timed_out");
        if run.state != "running" && !(archived && failed) {
            return None;
        }
        Some(self.render_hook(Hook::Archive, run, self.archive_open, cx))
    }

    fn render_hook(
        &self,
        hook: Hook,
        run: &HookRun,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let (icon, color, what) = match run.state.as_str() {
            "running" => (IconName::Loader, muted, "running…".to_string()),
            "done" => (IconName::CircleCheck, muted, "ran".to_string()),
            "timed_out" => (
                IconName::Clock,
                theme.danger,
                format!("timed out after {}", hook.timeout_label()),
            ),
            _ => (
                IconName::CircleX,
                theme.danger,
                format!(
                    "exited {}",
                    run.exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "early".into())
                ),
            ),
        };
        let lines = flotilla_model::setup::tail(&run.output, 8);
        let open = open && !lines.is_empty();
        let toggle = match hook {
            Hook::Resume => "resume-output",
            Hook::Archive => "archive-output",
        };
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(icon).xsmall().text_color(color))
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(hook.path()),
                    )
                    .child(
                        div()
                            .text_color(if matches!(run.state.as_str(), "failed" | "timed_out") {
                                theme.danger
                            } else {
                                muted
                            })
                            .child(what),
                    )
                    .when(!lines.is_empty(), |el| {
                        el.child(
                            Button::new(toggle)
                                .ghost()
                                .xsmall()
                                .label(if open { "Hide output" } else { "Show output" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    match hook {
                                        Hook::Resume => this.resume_open = !this.resume_open,
                                        Hook::Archive => this.archive_open = !this.archive_open,
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(open, |el| el.child(output_box(lines, cx)))
            .into_any_element()
    }

    /// The thread's pull request, joined to the top of the composer: what it is and where it
    /// stands on the left, and what to do next at the right edge, full height (Fix, Resolve and
    /// Address review fill the composer just below; Hold to merge merges).
    /// Sent later instead: the composer empties if it still holds what went.
    pub fn scheduled_sent(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.read(cx).value().trim() == text.trim() {
            self.composer
                .update(cx, |c, cx| c.set_value("", window, cx));
        }
    }

    /// Over the composer: what's scheduled to send here next, opening it in Scheduled.
    fn render_scheduled(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live: Vec<&flotilla_proto::Schedule> =
            self.schedules.iter().filter(|s| !s.done()).collect();
        let first = *live.first()?;
        let theme = cx.theme().clone();
        let here = flotilla_model::schedule::here();
        let mut words =
            flotilla_model::schedule::summary(first, flotilla_model::util::now_ms(), &here);
        if live.len() > 1 {
            words = format!("{words} · {} more", live.len() - 1);
        }
        let id = first.id.clone();
        let prompt =
            flotilla_model::util::one_line(first.prompt.lines().next().unwrap_or_default());
        Some(
            h_flex()
                .id("scheduled-here")
                .gap_2()
                .px_3()
                .pt_2()
                .min_w_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|el| el.text_color(theme.foreground))
                .on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(ThreadViewEvent::Scheduled(id.clone()))),
                )
                .child(Icon::new(IconName::Clock).xsmall().flex_none())
                .child(div().flex_none().child(words))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(prompt),
                )
                .into_any_element(),
        )
    }

    fn render_pr_bar(&self, t: &Thread, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pr, _) = self.summary.as_ref()?.pr.clone()?;
        let theme = cx.theme().clone();
        let (icon, color, _) = crate::sidebar::pr_look(&pr, &theme);
        let muted = theme.muted_foreground;
        let open = pr.state == "OPEN";
        let conflicts = open && pr.mergeable.as_deref() == Some("CONFLICTING");
        let (fail, pend, n) = (pr.failing(), pr.pending(), pr.checks.len());
        // A merged or closed PR's checks are history: no red "failing" for it.
        let checks = (n > 0 && open).then(|| {
            let (ic, c, label) = if fail > 0 {
                (IconName::CircleX, theme.danger, format!("{fail} failing"))
            } else if pend > 0 {
                (
                    IconName::CircleDot,
                    theme.warning,
                    format!("{}/{n} checks", n - pend),
                )
            } else {
                (
                    IconName::CircleCheck,
                    theme.success,
                    format!("{n}/{n} checks"),
                )
            };
            h_flex()
                .id("pr-checks")
                .flex_none()
                .gap_1()
                .cursor_pointer()
                .text_color(c)
                .child(Icon::new(ic).xsmall())
                .child(label)
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("Show the checks").build(window, cx)
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.checks_open = !this.checks_open;
                    cx.notify();
                }))
        });
        // Where it stands, in words; problems in red.
        let standing: Option<(Hsla, String)> = match pr.state.as_str() {
            "MERGED" => Some((color, "Merged".into())),
            "CLOSED" => Some((theme.danger, "Closed".into())),
            _ if conflicts => None,
            _ if pr.is_draft => Some((muted, "Draft".into())),
            _ if pr.review.as_deref() == Some("CHANGES_REQUESTED") => {
                Some((theme.warning, "Changes requested".into()))
            }
            _ if pr.can_merge() => Some((theme.success, "Ready to merge".into())),
            _ if pr.review.as_deref() == Some("REVIEW_REQUIRED") => {
                Some((muted, "Awaiting review".into()))
            }
            _ => None,
        };
        let base = if pr.base.is_empty() {
            "its base".to_string()
        } else {
            pr.base.clone()
        };
        let id = t.id.clone();
        // Ask the agent: each fills the composer with a prompt to read over and send. With the
        // VM gone, the prompt goes to a new thread on the branch instead.
        let blocked = self.prompt_blocked(t);
        let gone = t.vm_gone();
        let ask = |key: &'static str, label: &'static str, tooltip: &'static str| {
            let tooltip = if gone {
                "Opens a new thread on this branch"
            } else {
                blocked.unwrap_or(tooltip)
            };
            Button::new(key)
                .outline()
                .icon(IconName::Sparkles)
                .label(label)
                .tooltip(tooltip)
                .disabled(blocked.is_some())
        };
        let fill = move |prompt: String| {
            let id = t.id.clone();
            move |this: &mut ThreadView,
                  _: &ClickEvent,
                  window: &mut Window,
                  cx: &mut Context<ThreadView>| {
                if gone {
                    cx.emit(ThreadViewEvent::OnBranch {
                        thread: id.clone(),
                        prompt: prompt.clone(),
                    });
                } else {
                    this.fill_prompt(&prompt, window, cx);
                }
            }
        };
        let mut actions: Vec<AnyElement> = Vec::new();
        if open && fail > 0 {
            actions.push(
                ask("pr-fix", "Fix", "Ask the agent to fix the failing checks")
                    .on_click(cx.listener(fill(flotilla_model::fix::checks(&pr))))
                    .into_any_element(),
            );
        }
        if conflicts {
            actions.push(
                ask(
                    "pr-resolve",
                    "Resolve",
                    "Ask the agent to resolve the conflicts",
                )
                .on_click(cx.listener(fill(flotilla_model::fix::conflicts(&pr))))
                .into_any_element(),
            );
        }
        if open && pr.review.as_deref() == Some("CHANGES_REQUESTED") {
            let id = id.clone();
            actions.push(
                ask(
                    "pr-review",
                    "Address review",
                    "Ask the agent to address the review",
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(ThreadViewEvent::Act(Request::ReviewPrompt(id.clone())))
                }))
                .into_any_element(),
            );
        }
        if open && pr.is_draft {
            actions.push(
                Button::new("pr-ready")
                    .outline()
                    .label("Ready for review")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ThreadViewEvent::Command(
                            id.clone(),
                            ThreadCommand::MarkReady,
                        ))
                    }))
                    .into_any_element(),
            );
        } else if open && (pr.can_merge() || self.merging()) {
            actions.push(self.hold_to_merge(cx));
        }
        let open_url = pr.url.clone();
        let mut list: Vec<flotilla_proto::Check> = pr.checks.clone();
        list.sort_by_key(|c| match c.state.as_str() {
            "fail" => 0,
            "pending" => 1,
            _ => 2,
        });
        Some(
            v_flex()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.secondary.opacity(0.6))
                .rounded_t_lg()
                // Only an open PR's checks: a merged or closed one has no toggle to fold them.
                .when(open && self.checks_open && !list.is_empty(), |el| {
                    el.child(v_flex().gap(px(2.)).px_3().pt_2().pb_1().children(
                        list.into_iter().enumerate().map(|(i, c)| {
                            let (ic, color) = match c.state.as_str() {
                                "fail" => (IconName::CircleX, theme.danger),
                                "pending" => (IconName::CircleDot, theme.warning),
                                _ => (IconName::CircleCheck, theme.success),
                            };
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .child(Icon::new(ic).xsmall().text_color(color))
                                .child(div().flex_1().min_w_0().truncate().child(c.name.clone()))
                                .children(c.description.clone().map(|d| {
                                    div()
                                        .flex_none()
                                        .max_w(px(260.))
                                        .truncate()
                                        .text_color(muted)
                                        .child(d)
                                }))
                                .children(c.url.clone().map(|u| {
                                    Button::new(("check-details", i))
                                        .ghost()
                                        .xsmall()
                                        .label("Details")
                                        .icon(IconName::ExternalLink)
                                        .on_click(move |_, _, cx| cx.open_url(&u))
                                }))
                        }),
                    ))
                })
                .child(
                    h_flex()
                        .gap_3()
                        .pl_3()
                        .pr_1()
                        .py_1()
                        .text_sm()
                        .child(
                            h_flex()
                                .id("pr-title")
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .gap_2()
                                .cursor_pointer()
                                .child(Icon::new(icon).small().flex_none().text_color(color))
                                .child(
                                    div()
                                        .flex_none()
                                        .font_semibold()
                                        .child(format!("#{}", pr.number)),
                                )
                                .child(div().min_w_0().truncate().child(pr.title.clone()))
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("Open on GitHub")
                                        .build(window, cx)
                                })
                                .on_click(move |_, _, cx| cx.open_url(&open_url)),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_3()
                                .text_xs()
                                .children(checks)
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_color(theme.success)
                                                .child(format!("+{}", pr.additions)),
                                        )
                                        .child(
                                            div()
                                                .text_color(theme.danger)
                                                .child(format!("−{}", pr.deletions)),
                                        ),
                                )
                                .when(conflicts, |el| {
                                    el.child(
                                        h_flex()
                                            .gap_1()
                                            .text_color(theme.danger)
                                            .child(Icon::new(IconName::TriangleAlert).xsmall())
                                            .child(format!("Conflicts with {base}")),
                                    )
                                })
                                .children(
                                    standing.map(|(c, label)| div().text_color(c).child(label)),
                                ),
                        )
                        .when(!actions.is_empty(), |el| {
                            el.child(h_flex().flex_none().gap_1().children(actions))
                        }),
                )
                .into_any_element(),
        )
    }

    /// Press and hold to merge: a tinted button fills with solid green from the left while held
    /// (its label turns white under the fill), merges when full, and drains back if let go early.
    fn hold_to_merge(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let merging = self.merging();
        let fill = if merging {
            1.
        } else {
            self.hold.progress(Instant::now())
        };
        // A fixed width, so the white label inside the fill lines up with the green one under it.
        let width = px(148.);
        let label = |color: Hsla| {
            h_flex()
                .flex_none()
                .w(width)
                .h_full()
                .justify_center()
                .gap_1()
                .text_color(color)
                .child(if merging {
                    Spinner::new().xsmall().into_any_element()
                } else {
                    Icon::new(IconName::GitMerge).xsmall().into_any_element()
                })
                .child(if merging {
                    "Merging…"
                } else {
                    "Hold to merge"
                })
        };
        div()
            .id("pr-merge")
            .relative()
            .flex_none()
            .w(width)
            .h_8()
            .rounded(theme.radius)
            .overflow_hidden()
            .bg(theme.success.opacity(0.14))
            .text_sm()
            .font_semibold()
            .child(label(theme.success))
            // The fill advances in whole 4 px cells, a pixel bar rather than a smooth one.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .bottom_0()
                    .w(px((fill * 148. / 4.).floor() * 4.))
                    .overflow_hidden()
                    .bg(theme.success)
                    .child(label(theme.success_foreground)),
            )
            .when(!merging, |el| {
                el.cursor_pointer()
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Press and hold")
                            .build(window, cx)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.press_merge(cx)),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.release_merge(cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.release_merge(cx)),
                    )
            })
            .into_any_element()
    }

    /// A starting project thread's stages: done, the one running, in order.
    fn render_start_stages(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let st = self.start_stages.as_ref()?;
        let theme = cx.theme().clone();
        Some(
            v_flex()
                .mx_3()
                .mb_2()
                .p_3()
                .gap_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .child(div().text_sm().font_semibold().child("Starting…"))
                .children(st.done.iter().map(|d| {
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            Icon::new(IconName::CircleCheck)
                                .xsmall()
                                .text_color(theme.success),
                        )
                        .child(d.clone())
                }))
                .children(self.setup_failure.as_ref().map(|f| {
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .text_color(theme.danger)
                                .child(
                                    Icon::new(if f.timed_out {
                                        IconName::Clock
                                    } else {
                                        IconName::CircleX
                                    })
                                    .xsmall(),
                                )
                                .child(f.summary()),
                        )
                        .when(!f.output.trim().is_empty(), |el| {
                            el.child(output_box(flotilla_model::setup::tail(&f.output, 4), cx))
                        })
                }))
                .children(st.current.clone().map(|c| {
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            Icon::new(IconName::Loader)
                                .xsmall()
                                .text_color(theme.warning),
                        )
                        .child(c)
                }))
                .into_any_element(),
        )
    }

    /// Its start failed (before boat made a VM, or while preparing a project's repo): the
    /// first prompt is still unsent.
    fn start_failed(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| {
            (t.state == ThreadState::Error && t.sandbox_id.is_none())
                || (self.start_error.is_some() && t.state != ThreadState::Expired)
        })
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let unsent = self.start_failed();
        // Subagents' own messages go under their cards, shown while a card is open; its report
        // follows the last of them (or the card, with none).
        let idents = subagents::idents(&self.messages);
        let plan = subagents::plan(
            &self.messages,
            &idents,
            self.lineage.as_ref(),
            self.working(),
        );
        let hidden = hidden_answers(plan.items.iter().map(|p| &p.msg));
        // A message gone to inside a subagent's card: the card opens, so it has a row. A prompt
        // gone to opens too (a long one in full, the answers Flotilla brought on their line), so
        // what was searched for is on screen.
        // The message that took your answer to a question isn't shown: its card is gone to.
        if let Some((seq, at)) = self.target
            && hidden.contains(&seq)
            && let Some(ask) = plan
                .items
                .iter()
                .find(|p| p.msg.seq == seq)
                .and_then(|p| p.msg.answer_to.clone())
            && let Some(card) = plan.items.iter().find(|p| {
                p.msg.kind == flotilla_proto::ask::KIND
                    && p.msg.tool_id.as_deref() == Some(ask.as_str())
            })
        {
            self.target = Some((card.msg.seq, at));
        }
        if let Some((seq, _)) = self.target
            && let Some(item) = plan.items.iter().find(|p| p.msg.seq == seq)
        {
            if let Some(owner) = item.owner {
                self.expanded.insert(plan.items[owner].ident.clone());
            }
            if flotillas(&item.msg) && item.card.is_none() {
                self.expanded.insert(item.ident.clone());
            }
        }
        // The final reply of each turn, among what's shown at the top level.
        let top: Vec<(usize, &Message)> = plan
            .items
            .iter()
            .enumerate()
            .filter(|(_, p)| p.owner.is_none())
            .map(|(i, p)| (i, &p.msg))
            .collect();
        let finals: HashSet<usize> = final_replies(
            &top.iter().map(|(_, m)| (*m).clone()).collect::<Vec<_>>(),
            self.working(),
        )
        .into_iter()
        .map(|k| top[k].0)
        .collect();
        let expanded = &self.expanded;
        let open =
            |p: usize| plan.items[p].card.is_some() && expanded.contains(&plan.items[p].ident);
        // Where each open card's report goes: after its last message, or itself.
        let mut last_of: HashMap<usize, usize> = HashMap::new();
        for (i, p) in plan.items.iter().enumerate() {
            if p.card.is_some() {
                last_of.entry(i).or_insert(i);
            }
            if let Some(o) = p.owner {
                last_of.insert(o, i);
            }
        }
        let mut reports_after: HashMap<usize, Vec<usize>> = HashMap::new();
        for (card, last) in &last_of {
            reports_after.entry(*last).or_default().push(*card);
        }
        let mut pinned = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        let (mut running_calls, mut running_latest) = plan.running_calls.clone();
        let vm_starting = self
            .thread
            .as_ref()
            .is_some_and(|t| matches!(t.state, ThreadState::Creating | ThreadState::Starting));
        let mut prompts = 0;
        let mut handoffs = HashMap::new();
        // Each answered question's card says how the message that took the answer is going.
        let answers: HashMap<String, String> = plan
            .items
            .iter()
            .filter(|p| hidden.contains(&p.msg.seq))
            .filter_map(|p| {
                Some((
                    p.msg.answer_to.clone()?,
                    answer_status(
                        &p.msg,
                        shown_status(
                            p.msg.status.clone(),
                            false,
                            vm_starting,
                            unsent,
                            self.server_down,
                        ),
                    )?,
                ))
            })
            .collect();
        for (i, p) in plan.items.iter().enumerate() {
            // A thread this one handed work to: noted wherever its card sits (inside a folded
            // subagent's too), and pinned while it works, its calls counted with the subagents'.
            // Each card's own place: the call's for the first, numbered after it for the rest.
            let cards: Vec<(String, Rc<HandOff>)> = if p.msg.kind == "tool" {
                self.handoffs(&p.msg)
                    .into_iter()
                    .enumerate()
                    .map(|(k, h)| {
                        (
                            if k == 0 {
                                p.ident.clone()
                            } else {
                                format!("{}#{k}", p.ident)
                            },
                            h,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            for (card_ident, h) in &cards {
                handoffs.insert(
                    h.child.clone(),
                    (
                        card_ident.clone(),
                        p.owner.map(|o| plan.items[o].ident.clone()),
                    ),
                );
                if h.running {
                    pinned.push((
                        card_ident.clone(),
                        Rc::new(h.card.clone()),
                        h.started_ms.or(p.msg.timestamp),
                    ));
                    running_calls += h.card.calls.unwrap_or(0);
                    running_latest = running_latest.or_else(|| h.card.latest.clone());
                }
            }
            if p.owner.is_some_and(|o| !open(o)) {
                continue;
            }
            // Your answer to a question: its card shows it. (Its turn still starts here: the
            // replies above end theirs, as `final_replies` reads it.)
            if hidden.contains(&p.msg.seq) {
                prompts += usize::from(p.msg.kind == "prompt");
                continue;
            }
            let mut msg = Box::new(p.msg.clone());
            prompts += usize::from(msg.kind == "prompt");
            // The thread's first prompt: the first shown, with nothing older above it.
            let first_prompt = msg.kind == "prompt" && prompts == 1 && !self.older.0;
            msg.status = shown_status(
                msg.status.take(),
                first_prompt,
                vm_starting,
                unsent,
                self.server_down,
            );
            if let Some(card) = &p.card
                && p.msg.pending
            {
                pinned.push((p.ident.clone(), card.clone(), p.msg.timestamp));
            }
            let files = match msg.task_id.as_ref().filter(|_| !msg.attachments.is_empty()) {
                Some(task) => self.sent_files.get(task).cloned().unwrap_or_default(),
                None => Vec::new(),
            };
            let from =
                self.parent
                    .as_ref()
                    .filter(|_| {
                        msg.kind == "prompt"
                            && msg.text.as_deref().is_some_and(|t| {
                                t.ends_with(flotilla_model::orchestrate::CHILD_NOTE)
                            })
                    })
                    .map(|p| Rc::new(p.clone()));
            // A call that handed work on: a card for each agent it started, and its answer under
            // an open one.
            for (card_ident, h) in &cards {
                let open_card = expanded.contains(card_ident);
                rows.push(Row::Message {
                    expanded: open_card,
                    msg: msg.clone(),
                    ident: card_ident.clone(),
                    actions: false,
                    nested: p.owner.is_some(),
                    sub: None,
                    files: Vec::new(),
                    handoff: Some(h.clone()),
                    from: None,
                    whole: None,
                    scheduled: None,
                    ask: None,
                    answer: None,
                });
                if let (true, Some(report)) = (open_card, h.card.report.clone()) {
                    rows.push(Row::Report {
                        ident: format!("{card_ident}:report"),
                        text: report,
                    });
                }
            }
            if cards.is_empty() {
                let whole = (!msg.cut.is_empty()).then(|| {
                    Rc::new(shown_whole(
                        &self.wholes,
                        &self.shown_whole,
                        &self.copy_whole,
                        &cut_key(&msg),
                    ))
                });
                // Sent by a schedule: says so, with its timing while it's still there.
                let scheduled = msg
                    .task_id
                    .as_ref()
                    .filter(|_| msg.kind == "prompt")
                    .and_then(|t| self.sent_schedules.get(t))
                    .map(|id| {
                        let timing = self
                            .schedules
                            .iter()
                            .find(|s| &s.id == id)
                            .map(|s| flotilla_model::schedule::describe(&s.when));
                        Rc::new((id.clone(), timing))
                    });
                let ask = (msg.kind == flotilla_proto::ask::KIND)
                    .then(|| {
                        msg.tool_id
                            .as_ref()
                            .and_then(|id| self.ask_drafts.get(id))
                            .map(|d| Rc::new(d.shown()))
                    })
                    .flatten();
                let answer = msg
                    .tool_id
                    .as_ref()
                    .filter(|_| msg.kind == flotilla_proto::ask::KIND)
                    .and_then(|id| answers.get(id))
                    .cloned();
                rows.push(Row::Message {
                    scheduled,
                    ask,
                    answer,
                    whole,
                    expanded: expanded.contains(&p.ident),
                    msg,
                    ident: p.ident.clone(),
                    actions: finals.contains(&i),
                    nested: p.owner.is_some(),
                    sub: p.card.clone(),
                    files,
                    handoff: None,
                    from,
                });
            }
            for &c in reports_after.get(&i).into_iter().flatten() {
                if let (true, Some(report)) = (
                    open(c),
                    plan.items[c]
                        .card
                        .as_ref()
                        .and_then(|card| card.report.clone()),
                ) {
                    // Its last words, just shown above it, aren't said again.
                    let said = i != c
                        && plan.items[i].msg.kind == "text"
                        && plan.items[i]
                            .msg
                            .text
                            .as_deref()
                            .is_some_and(|t| t.trim() == report.trim());
                    if !said {
                        rows.push(Row::Report {
                            ident: format!("{}:report", plan.items[c].ident),
                            text: report,
                        });
                    }
                }
            }
        }
        if pinned.is_empty() {
            self.pinned_open = None;
        }
        self.pinned = pinned;
        self.pinned_calls = (running_calls, running_latest);
        self.handoffs = handoffs;
        let target = self.target.map(|(seq, _)| seq);
        let mut rows = fold_calls(rows, &mut self.expanded, target, !self.target_quiet);
        let mut partials: Vec<&RawEvent> = self.partials.iter().collect();
        partials.sort_by_key(|p| p.timestamp);
        for p in partials {
            if let Some(text) = p
                .data
                .get("content")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                rows.push(Row::Partial {
                    id: p.id.clone(),
                    text: text.to_string(),
                });
            }
        }
        let signatures: Vec<u64> = rows.iter().map(Row::signature).collect();
        let old_len = self.signatures.len();
        let first = signatures
            .iter()
            .zip(&self.signatures)
            .take_while(|(a, b)| a == b)
            .count();
        if first < old_len || signatures.len() != old_len {
            // The same at the end too: older messages arriving go in above without moving
            // what's on screen.
            let room = old_len.min(signatures.len()) - first;
            let last = signatures
                .iter()
                .rev()
                .zip(self.signatures.iter().rev())
                .take(room)
                .take_while(|(a, b)| a == b)
                .count();
            let (old_end, new_count) = (old_len - last, signatures.len() - first - last);
            // A row at the top that the splice replaces: its new place, to scroll back to.
            let (top, following) = {
                let s = self.scroller.read(cx);
                (s.logical_scroll_top().item_ix, s.is_following_tail())
            };
            let back = (!following && (first..old_end).contains(&top))
                .then(|| self.rows.get(top).map(Row::ident))
                .flatten()
                .and_then(|ident| rows.iter().position(|r| r.ident() == ident));
            self.scroller.update(cx, |s, cx| {
                if s.item_count() != old_len || !s.splice(first..old_end, new_count, cx) {
                    s.reset(signatures.len(), cx);
                } else {
                    // Rows came in above the first: it's no longer first, nor as tall.
                    if first == 0 && old_end == 0 && new_count > 0 && old_len > 0 {
                        s.remeasure_items(new_count..new_count + 1, cx);
                    }
                    if let Some(ix) = back {
                        s.scroll_to_item(ix, cx);
                    }
                }
            });
        }
        self.signatures = signatures;
        self.rows = Rc::new(rows);
        // The message gone to is here now: to the top, marked.
        // (Its own row, else the folded calls it's among.)
        if let Some((seq, _)) = self.target
            && let Some((ix, ident)) = self
                .rows
                .iter()
                .enumerate()
                .find_map(|(i, r)| match r {
                    Row::Message { msg, ident, .. } if msg.seq == seq => Some((i, ident.clone())),
                    _ => None,
                })
                .or_else(|| {
                    self.rows.iter().enumerate().find_map(|(i, r)| match r {
                        Row::Calls { calls, ident, .. } if calls.seqs.contains(&seq) => {
                            Some((i, ident.clone()))
                        }
                        _ => None,
                    })
                })
        {
            #[cfg(feature = "snapshot")]
            if let Some((_, at)) = self.target {
                eprintln!(
                    "[jump] seq {seq} shown {:.0} ms after it was asked for, row {ix} of {}",
                    at.elapsed().as_secs_f64() * 1000.,
                    self.rows.len()
                );
            }
            self.target = None;
            if !std::mem::take(&mut self.target_quiet) {
                self.flash = Some((ident, Instant::now()));
            }
            self.scroller.update(cx, |s, cx| {
                s.scroll_to_item(ix, cx);
            });
        }
        // Not here yet: waited for while its pages come, and a while after (a message gone
        // since), so a page that brings it after a long wait still goes to it.
        if !self.older.1 && self.target.is_some_and(|(_, at)| at.elapsed() > TARGET_FOR) {
            self.target = None;
        }
        cx.notify();
    }

    /// How strongly the row gone to is marked now (1 to 0), and its ident; None once it's
    /// faded. It holds, then fades out (ease-out); with reduced motion it holds, then goes.
    fn flash_now(&mut self, cx: &App) -> Option<(String, f32)> {
        let (ident, at) = self.flash.clone()?;
        let t = at.elapsed();
        let alpha = if t < FLASH_HOLD {
            1.
        } else if t < FLASH_HOLD + FLASH_FADE && !cx.reduce_motion() {
            1. - ease_out((t - FLASH_HOLD).as_secs_f32() / FLASH_FADE.as_secs_f32())
        } else {
            self.flash = None;
            return None;
        };
        Some((ident, alpha))
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn composer_state(&self, window: &Window, cx: &App) -> (String, (u32, u32), bool) {
        let c = self.composer.read(cx);
        let p = c.cursor_position();
        (
            c.value().to_string(),
            (p.line, p.character),
            c.focus_handle(cx).is_focused(window),
        )
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_row_count(&self) -> usize {
        self.rows.len()
    }

    /// Every run of calls opened, so each call has its row.
    #[cfg(feature = "snapshot")]
    fn unfold_calls(&mut self, cx: &mut Context<Self>) {
        let folded: Vec<String> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Calls {
                    ident,
                    expanded: false,
                    ..
                } => Some(ident.clone()),
                _ => None,
            })
            .collect();
        if !folded.is_empty() {
            self.expanded.extend(folded);
            self.rebuild_rows(cx);
        }
    }

    /// The newest cut tool output, a Bash one if there is one (its last lines say it's whole).
    #[cfg(feature = "snapshot")]
    fn last_cut(&self) -> Option<(usize, String, Message)> {
        let cut = |bash: bool| {
            self.rows
                .iter()
                .enumerate()
                .rev()
                .find_map(|(i, r)| match r {
                    Row::Message {
                        msg,
                        ident,
                        whole: Some(_),
                        ..
                    } if !bash || msg.tool_name.as_deref() == Some("Bash") => {
                        Some((i, ident.clone(), (**msg).clone()))
                    }
                    _ => None,
                })
        };
        cut(true).or_else(|| cut(false))
    }

    /// Scroll to the row after the newest cut tool output (its end just above the top).
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_after_cut(&mut self, cx: &mut Context<Self>) {
        self.unfold_calls(cx);
        if let Some((ix, _, _)) = self.last_cut() {
            self.scroller
                .update(cx, |s, cx| s.scroll_to_item(ix + 1, cx));
        }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_expand_cut(&mut self, cx: &mut Context<Self>) {
        self.unfold_calls(cx);
        if let Some((_, ident, _)) = self.last_cut() {
            if !self.expanded.contains(&ident) {
                self.toggle(ident.clone(), cx);
            }
            if let Some(ix) = self.rows.iter().position(|r| r.ident() == ident) {
                self.scroller
                    .update(cx, |s, cx| s.scroll_to_item(ix.saturating_sub(1), cx));
            }
        }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_show_all(&mut self, cx: &mut Context<Self>) {
        self.unfold_calls(cx);
        if let Some((_, _, m)) = self.last_cut() {
            self.ask_whole(&m, false, cx);
        }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_top(&self, cx: &App) -> String {
        let s = self.scroller.read(cx);
        let top = s.logical_scroll_top();
        let what = self.rows.get(top.item_ix).map(|r| match r {
            Row::Message { msg, .. } => format!("{} seq {}", r.ident(), msg.seq),
            _ => r.ident().to_string(),
        });
        format!(
            "{} (row {} of {}, {:.0} px in, following: {})",
            what.unwrap_or_default(),
            top.item_ix,
            self.rows.len(),
            f32::from(top.offset_in_item),
            s.is_following_tail()
        )
    }

    /// Whether it went to the message `seq`: its row marked, and at the top (or the list at
    /// its end, when there's too little below the row for that).
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_jumped(&self, seq: i64, cx: &App) -> bool {
        let Some((ident, _)) = &self.flash else {
            return false;
        };
        let Some(ix) = self.rows.iter().position(
            |r| matches!(r, Row::Message { msg, ident: i, .. } if msg.seq == seq && i == ident),
        ) else {
            return false;
        };
        let s = self.scroller.read(cx);
        s.logical_scroll_top().item_ix == ix
            || (s.is_following_tail() && s.logical_scroll_top().item_ix <= ix)
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_first_seq(&self) -> Option<i64> {
        self.messages.first().map(|m| m.seq)
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn expand_last_tool(&mut self, cx: &mut Context<Self>) {
        self.unfold_calls(cx);
        let first = self.rows.iter().rev().find_map(|r| match r {
            Row::Message { msg, ident, .. } if msg.kind == "tool" => Some(ident.clone()),
            _ => None,
        });
        if let Some(ident) = first {
            self.toggle(ident, cx);
        }
    }

    /// How many subagents are pinned (running), for a demo script's `until:subagents`.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_pinned(&self) -> usize {
        self.pinned.len()
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn unfold_pinned(&mut self, cx: &mut Context<Self>) {
        self.pinned_open = Some(true);
        cx.notify();
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn open_last_subagent(&mut self, cx: &mut Context<Self>) {
        let last = self.rows.iter().rev().find_map(|r| match r {
            Row::Message {
                ident,
                sub: Some(_),
                expanded: false,
                ..
            } => Some(ident.clone()),
            _ => None,
        });
        if let Some(ident) = last {
            self.toggle(ident, cx);
        }
    }

    /// The interrupt didn't go through: offer Stop again.
    pub fn interrupt_failed(&mut self, cx: &mut Context<Self>) {
        self.interrupting = false;
        cx.notify();
    }

    /// Put back a message whose send failed, with its files, unless something new has been
    /// typed or attached since. Returns whether it did, so a caller keeping the text elsewhere
    /// knows it can let go.
    pub fn restore_draft(
        &mut self,
        text: &str,
        files: &[Attached],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.composer.read(cx).value().trim().is_empty() || !self.attachments.is_empty() {
            return false;
        }
        self.composer
            .update(cx, |c, cx| c.set_value(text.to_string(), window, cx));
        self.attachments.files = files.to_vec();
        cx.notify();
        true
    }

    /// Files from the paperclip or dropped on the composer, for the next prompt.
    pub fn add_files(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        self.attachments.add(paths);
        cx.notify();
    }

    /// Files can go with a new prompt, not a steer: boat's steer takes none.
    fn can_attach(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|t| t.state != ThreadState::Expired)
            && self.start_stages.is_none()
            && !self.working()
    }

    /// Earlier prompts and steers, newest first, without repeats in a row. Only yours: not
    /// a child's answer Flotilla brought in, nor the task a parent handed this thread.
    fn sent_history(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in self
            .messages
            .iter()
            .rev()
            .filter(|m| matches!(m.kind.as_str(), "prompt" | "steer"))
        {
            let Some(text) = m
                .text
                .as_deref()
                .filter(|t| !t.trim().is_empty() && *t != PROMPT_NOT_KEPT)
            else {
                continue;
            };
            if text.starts_with("[flotilla] ")
                || text.ends_with(flotilla_model::orchestrate::CHILD_NOTE)
            {
                continue;
            }
            if out.last().map(String::as_str) != Some(text) {
                out.push(text.to_string());
            }
        }
        out
    }

    /// ↑ in an empty composer (or on a prompt it just recalled) brings back an earlier prompt;
    /// ↓ walks forward, and past the newest clears the composer. Anything else, or an edited
    /// recall, leaves the arrow keys to move the cursor. Returns whether it handled the key.
    fn recall_prompt(&mut self, older: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let history = self.sent_history();
        let value = self.composer.read(cx).value().to_string();
        let showing = self
            .recall
            .and_then(|i| history.get(i))
            .is_some_and(|h| *h == value);
        if !showing {
            self.recall = None;
        }
        let next = match (older, self.recall) {
            (true, None) if value.is_empty() && !history.is_empty() => Some(0),
            (true, Some(i)) if showing && i + 1 < history.len() => Some(i + 1),
            (true, Some(_)) if showing => return true,
            (false, Some(0)) if showing => None,
            (false, Some(i)) if showing => Some(i - 1),
            _ => return false,
        };
        self.recall = next;
        let text = next.map(|i| history[i].clone()).unwrap_or_default();
        self.composer
            .update(cx, |c, cx| c.set_value(text, window, cx));
        true
    }

    /// Why the PR row's asks can't be used now. A thread whose VM is gone can: they open a
    /// new thread on its branch.
    fn prompt_blocked(&self, t: &Thread) -> Option<&'static str> {
        if t.vm_gone() {
            None
        } else if self.start_failed() {
            Some("The thread didn't start")
        } else if self.start_stages.is_some() {
            Some("The thread is still starting")
        } else {
            None
        }
    }

    /// Put a prompt Flotilla wrote into the composer after anything already there, and focus
    /// it for you to read over and send. Nothing is sent.
    pub fn fill_prompt(&mut self, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.composer.read(cx).value().to_string();
        let gap = match current.as_str() {
            c if c.trim().is_empty() => "",
            c if c.ends_with("\n\n") => "",
            c if c.ends_with('\n') => "\n",
            _ => "\n\n",
        };
        let last_line = current.split('\n').count().saturating_sub(1) as u32;
        self.composer.update(cx, |c, cx| {
            if current.trim().is_empty() {
                c.set_value(prompt.to_string(), window, cx);
                // At the end, so you can add to it before sending.
                let end = prompt.split('\n').count().saturating_sub(1) as u32;
                c.set_cursor_position(
                    gpui_kit::base::input::Position::new(end, u32::MAX),
                    window,
                    cx,
                );
            } else {
                c.set_cursor_position(
                    gpui_kit::base::input::Position::new(last_line, u32::MAX),
                    window,
                    cx,
                );
                c.insert(format!("{gap}{prompt}"), window, cx);
            }
            c.focus(window, cx);
        });
        cx.notify();
    }

    /// Quote `text` into the composer after anything already there, ready to reply under.
    fn cite(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.composer.read(cx).value().to_string();
        let gap = match current.as_str() {
            c if c.trim().is_empty() || c.ends_with("\n\n") => "",
            c if c.ends_with('\n') => "\n",
            _ => "\n\n",
        };
        let quote = format!("{gap}{}\n\n", block_quote(text));
        let last_line = current.split('\n').count().saturating_sub(1) as u32;
        // Typed in at the end rather than set: the caret ends up under the quote, ready to
        // reply, and ⌘Z can take the quote back out.
        self.composer.update(cx, |c, cx| {
            c.set_cursor_position(
                gpui_kit::base::input::Position::new(last_line, u32::MAX),
                window,
                cx,
            );
            c.insert(quote, window, cx);
        });
    }

    /// Show the copy button as a check for a second.
    fn mark_copied(&mut self, ident: String, cx: &mut Context<Self>) {
        self.copied = Some(ident.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = this.update(cx, |v, cx| {
                if v.copied.as_deref() == Some(ident.as_str()) {
                    v.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Open a subagent's card in the thread and scroll to it.
    /// Open and scroll to the card for the thread `child` was handed (opening the subagent's
    /// card it sits in, if one), else open that thread.
    fn show_handoff(&mut self, child: &str, cx: &mut Context<Self>) {
        match self.handoffs.get(child).cloned() {
            Some((ident, _)) => self.show_subagent(ident, cx),
            None => cx.emit(ThreadViewEvent::Open(child.to_string())),
        }
    }

    fn show_subagent(&mut self, ident: String, cx: &mut Context<Self>) {
        // A hand-off's card inside a folded subagent's: open that one too, or there's no row.
        if let Some(owner) = self
            .handoffs
            .values()
            .find(|(card, _)| *card == ident)
            .and_then(|(_, owner)| owner.clone())
        {
            self.expanded.insert(owner);
        }
        self.expanded.insert(ident.clone());
        self.rebuild_rows(cx);
        let row = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Message { ident: i, .. } if *i == ident));
        if let Some(ix) = row {
            self.scroller.update(cx, |s, cx| {
                s.scroll_to_item(ix, cx);
            });
        }
    }

    /// Running subagents, under the header: one is shown as it goes; several fold into one line
    /// (open it to see each). Clicking one opens its card in the thread.
    fn render_pinned(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.pinned.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let now = now_ms();
        let n = self.pinned.len();
        // One alone always shows; several are folded until opened.
        let open = n == 1 || self.pinned_open.unwrap_or(false);
        let row = |ident: &str, card: &SubCard, started: Option<i64>, cx: &mut Context<Self>| {
            let target = ident.to_string();
            let meta: Vec<String> = [
                card.kind.clone(),
                card.calls.and_then(calls_label),
                started.map(|t| fmt::elapsed(t, now)),
            ]
            .into_iter()
            .flatten()
            .collect();
            h_flex()
                .id(SharedString::from(format!("pinned-{ident}")))
                .gap_2()
                .px_2()
                .py(px(5.))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|el| el.bg(theme.secondary))
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new("Show in the thread")
                        .build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| this.show_subagent(target.clone(), cx)))
                .child(Icon::new(IconName::Bot).small().text_color(theme.primary))
                .child(
                    div()
                        .flex_none()
                        .max_w(px(260.))
                        .truncate()
                        .text_sm()
                        .font_medium()
                        .child(card.description.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(meta.join(" · ")),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(muted)
                        .children(
                            card.latest
                                .clone()
                                .map(|(name, summary)| {
                                    [
                                        div()
                                            .flex_none()
                                            .text_color(theme.foreground)
                                            .child(name)
                                            .into_any_element(),
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .child(summary)
                                            .into_any_element(),
                                    ]
                                })
                                .into_iter()
                                .flatten(),
                        ),
                )
                .child(Spinner::new().xsmall())
                .into_any_element()
        };
        let mut list = v_flex().gap_1();
        if n > 1 {
            let names: Vec<String> = self
                .pinned
                .iter()
                .map(|(_, c, _)| c.description.clone())
                .collect();
            let (calls, latest) = self.pinned_calls.clone();
            let summary = [
                calls_label(calls),
                latest.map(|(name, what)| format!("{name} {what}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            list = list.child(
                h_flex()
                    .id("pinned-fold")
                    .gap_2()
                    .px_2()
                    .py(px(5.))
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(|el| el.bg(theme.secondary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pinned_open = Some(!open);
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(muted),
                    )
                    .child(Icon::new(IconName::Bot).small().text_color(theme.primary))
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .font_medium()
                            .child(format!("{n} subagents running")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(if open || names.is_empty() {
                                summary
                            } else {
                                [names.join(", "), summary]
                                    .into_iter()
                                    .filter(|s| !s.is_empty())
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            }),
                    )
                    .child(Spinner::new().xsmall()),
            );
        }
        if open {
            for (ident, card, started) in &self.pinned {
                let r = row(ident, card, *started, cx);
                list = list.child(if n > 1 {
                    div().pl(px(18.)).child(r).into_any_element()
                } else {
                    r
                });
            }
        }
        Some(
            div()
                .flex_none()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.background)
                .shadow_sm()
                .child(list)
                .into_any_element(),
        )
    }

    fn toggle(&mut self, ident: String, cx: &mut Context<Self>) {
        // Following the newest, an opening card would push its own header up and away: the
        // view holds where it is, and the card grows downward.
        let (following, top) = {
            let s = self.scroller.read(cx);
            (s.is_following_tail(), s.logical_scroll_top())
        };
        let opening = self.expanded.insert(ident.clone());
        if !opening {
            self.expanded.remove(&ident);
        }
        self.rebuild_rows(cx);
        if opening && following {
            self.scroller
                .update(cx, |s, cx| s.scroll_to_item(top.item_ix, cx));
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.start_stages.is_some() {
            return;
        }
        let Some(t) = &self.thread else { return };
        if t.state == ThreadState::Expired {
            return;
        }
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        // A steer goes without the files; they wait for the next prompt.
        let files = if self.working() {
            Vec::new()
        } else {
            std::mem::take(&mut self.attachments).files
        };
        cx.emit(ThreadViewEvent::Send {
            thread: t.id.clone(),
            text,
            files,
        });
        self.composer
            .update(cx, |c, cx| c.set_value("", window, cx));
        self.menu.close();
    }

    /// Dev: type `text` into the composer, caret at the end, as keystrokes would.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_type(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let last = text.split('\n').count().saturating_sub(1) as u32;
        self.composer.update(cx, |c, cx| {
            c.set_value(text.to_string(), window, cx);
            c.set_cursor_position(
                gpui_kit::base::input::Position::new(last, u32::MAX),
                window,
                cx,
            );
            c.focus(window, cx);
        });
        self.follow_composer(cx);
    }

    /// Dev: Enter in the composer.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.submit(window, cx);
    }

    /// Dev: the Preview menu's Open a port….
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_preview(window, cx);
    }

    fn open_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Port, e.g. 3000")
                .default_value("3000")
        });
        let weak = cx.weak_entity();
        let id = self.id.clone();
        let stopped = self
            .thread
            .as_ref()
            .is_some_and(|t| matches!(t.state, ThreadState::Stopped | ThreadState::Stopping));
        let submit = {
            let port = port.clone();
            move |window: &mut Window, cx: &mut App| {
                let Ok(p) = port.read(cx).value().trim().parse::<u16>() else {
                    return;
                };
                let _ = weak.update(cx, |_, cx| {
                    cx.emit(ThreadViewEvent::Act(Request::Preview {
                        thread: id.clone(),
                        port: p,
                    }))
                });
                window.close_dialog(cx);
                // The URL takes a moment (longer if the VM resumes first): say it's coming.
                let note = if stopped {
                    format!("Resuming the VM, then opening port {p}…")
                } else {
                    format!("Opening port {p}…")
                };
                window.push_notification(
                    gpui_kit::component::notification::Notification::info(note).title("Preview"),
                    cx,
                );
            }
        };
        let submit = Rc::new(submit);
        let enter = submit.clone();
        let field = port.clone();
        let weak = cx.weak_entity();
        crate::form::open(
            window,
            cx,
            Some(field),
            move |window, cx| enter(window, cx),
            move |dialog, _, _| {
                let submit = submit.clone();
                let (port, weak) = (port.clone(), weak.clone());
                dialog
                    .title("Open a preview")
                    .width(px(380.))
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().child("Port in the VM"))
                            .child(Input::new(&port)),
                    )
                    .footer(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("open-preview-public")
                                    .ghost()
                                    .icon(IconName::LockOpen)
                                    .label("Make public…")
                                    .on_click(move |_, window, cx| {
                                        let Ok(p) = port.read(cx).value().trim().parse::<u16>()
                                        else {
                                            return;
                                        };
                                        window.close_dialog(cx);
                                        let _ = weak.update(cx, |v, cx| {
                                            let what = v.service_on(p).map_or_else(
                                                || format!("port {p}"),
                                                |r| r.name.clone(),
                                            );
                                            v.open_make_public(p, what, window, cx);
                                        });
                                    }),
                            )
                            .child(
                                Button::new("open-preview")
                                    .primary()
                                    .label("Open")
                                    .on_click(move |_, window, cx| submit(window, cx)),
                            ),
                    )
            },
        );
    }

    /// Dev: Make public… for a port, as the menus open it.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_make_public(
        &mut self,
        port: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let what = self
            .service_on(port)
            .map_or_else(|| format!("port {port}"), |r| r.name.clone());
        self.open_make_public(port, what, window, cx);
    }

    /// The service on `port` of the VM the thread has now, if one.
    fn service_on(&self, port: u16) -> Option<&ServiceRun> {
        let t = self.thread.as_ref()?;
        self.current_services(t)?
            .services
            .iter()
            .find(|r| r.port == port && (r.preview.is_some() || !r.links.is_empty()))
    }

    /// Make public…: for how long, said plainly as public to anyone with the link. `what`
    /// names it ("web", "port 3000"). Again while public: its time starts over.
    fn open_make_public(
        &mut self,
        port: u16,
        what: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.services_open = false;
        let (weak, id) = (cx.weak_entity(), self.id.clone());
        let again = self.public.iter().any(|l| l.port == port);
        let stopped = self
            .thread
            .as_ref()
            .is_some_and(|t| matches!(t.state, ThreadState::Stopped | ThreadState::Stopping));
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let choices = PublicFor::ALL.into_iter().enumerate().map(|(i, for_)| {
                let (weak, id) = (weak.clone(), id.clone());
                Button::new(("make-public", i))
                    .outline()
                    .w_full()
                    .label(for_.label())
                    .on_click(move |_, window, cx| {
                        let req = Request::MakePublic {
                            thread: id.clone(),
                            port,
                            for_,
                        };
                        let _ = weak.update(cx, |_, cx| cx.emit(ThreadViewEvent::Act(req)));
                        window.close_dialog(cx);
                        if stopped {
                            let note = gpui_kit::component::notification::Notification::info(
                                format!("Resuming the VM, then making port {port} public…"),
                            )
                            .title("Public link");
                            window.push_notification(note, cx);
                        }
                    })
            });
            dialog
                .title(if again {
                    format!("Keep {what} public")
                } else {
                    format!("Make {what} public")
                })
                .width(px(340.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("Anyone with the link can open it, without signing in."),
                        )
                        .child(v_flex().gap_2().children(choices)),
                )
        });
        cx.notify();
    }

    /// Two lines: the title with Preview and ⋯ at the right; then, as plain text, the agent, the
    /// project and branch, and the VM; Preview, Keep running and More at the right of line 1. A third line only
    /// for a problem (setup, `.agents/resume` or `.agents/archive` failing), or a hook running.
    fn render_header(&self, t: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let weak = cx.weak_entity();
        let id = t.id.clone();
        // Handed its task by a thread that's there: it's listed inside that one's row.
        let commands = self
            .summary
            .as_ref()
            .map(|s| thread_menu::commands(s, self.parent.is_none()))
            .unwrap_or_default();
        let more = Button::new("more")
            .ghost()
            .icon(IconName::Ellipsis)
            .tooltip("More")
            .dropdown_menu(move |menu, _, _| {
                let (weak, id) = (weak.clone(), id.clone());
                thread_menu::fill(
                    menu,
                    commands.clone(),
                    Rc::new(move |cmd, _, cx| {
                        let _ = weak.update(cx, |_, cx| {
                            cx.emit(ThreadViewEvent::Command(id.clone(), cmd))
                        });
                    }),
                )
            });

        // The VM's state is the pip by the title (its tooltip says it in words); words only for
        // the turn running and what's broken. An idle VM's pip says it all (Keep running has
        // its own button).
        let now = now_ms();
        let (dot, status, tip): (Hsla, Option<String>, String) = match t.state {
            _ if self.working() && self.interrupting => (
                theme.warning,
                Some("Stopping the turn…".into()),
                "Stopping the turn".into(),
            ),
            _ if self.working() => (
                theme.success,
                Some(format!(
                    "turn {}",
                    fmt::elapsed(self.turn_started_ms.unwrap_or(now), now)
                )),
                "Working".into(),
            ),
            ThreadState::Creating | ThreadState::Starting => {
                (theme.warning, None, "Starting the VM…".into())
            }
            ThreadState::Idle | ThreadState::Running => {
                let deadline = t
                    .archive_after
                    .as_deref()
                    .and_then(flotilla_model::util::iso_to_ms);
                match (t.keep_running, deadline) {
                    (true, _) => (theme.success, None, "Kept running".into()),
                    (false, Some(_)) => {
                        let clock = t
                            .archive_after
                            .as_deref()
                            .and_then(fmt::clock)
                            .map(|c| format!(" at {c}"))
                            .unwrap_or_default();
                        (theme.success, None, format!("Stops{clock} when idle"))
                    }
                    (false, None) => (theme.success, None, "Running".into()),
                }
            }
            ThreadState::Stopping => (theme.warning, None, "Stopping the VM…".into()),
            ThreadState::Stopped => {
                let tip = match days_left(t) {
                    Some(d) => format!("Stopped · expires in {}d", d.max(0)),
                    None => "Stopped".into(),
                };
                (muted.opacity(0.6), None, tip)
            }
            ThreadState::Expired => (muted.opacity(0.6), Some("Expired".into()), "Expired".into()),
            ThreadState::Error if t.sandbox_id.is_none() => (
                theme.danger,
                Some("Not started".into()),
                "boat didn't make a VM".into(),
            ),
            ThreadState::Error => (
                theme.danger,
                Some("VM error".into()),
                "Try Resume VM".into(),
            ),
        };
        let sep = || div().flex_none().text_color(muted.opacity(0.5)).child("·");
        // Archived (and open still): it says so, with the way back.
        let archived = self
            .summary
            .as_ref()
            .is_some_and(|s| s.activity == flotilla_proto::Activity::Archived);
        let status = if archived && !self.working() {
            Some("Archived".to_string())
        } else {
            status
        };
        // Nothing before the project needs no dot after it.
        let worded = status.is_some();
        let pip = div()
            .id("vm-state")
            .flex_none()
            .child(crate::pixel::pip(dot, 6.))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            });
        let vm = h_flex()
            .flex_none()
            .children(status)
            // Words or not, the line keeps its height: the header doesn't jump as a turn starts
            // and ends.
            .when(!worded, |el| {
                el.child(div().w_0().overflow_hidden().child("\u{a0}"))
            });
        let unarchive = archived.then(|| {
            let id = t.id.clone();
            Button::new("unarchive")
                .ghost()
                .xsmall()
                .icon(IconName::ArchiveRestore)
                .label("Unarchive")
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(ThreadViewEvent::Command(
                        id.clone(),
                        ThreadCommand::Unarchive,
                    ));
                }))
        });
        // Only what needs you: behind the default branch, or a PR that conflicts with its base.
        // Neither means anything once the PR is merged or closed (a squash merge leaves the
        // branch "behind" by its own commit).
        let pr_done = self
            .summary
            .as_ref()
            .and_then(|s| s.pr.as_ref())
            .is_some_and(|(pr, _)| pr.state != "OPEN");
        let behind = self
            .project
            .as_ref()
            .and_then(|(_, b, _)| *b)
            .filter(|n| *n > 0 && !pr_done);
        let conflicts = self
            .summary
            .as_ref()
            .and_then(|s| s.pr.as_ref())
            .is_some_and(|(pr, _)| {
                pr.state == "OPEN" && pr.mergeable.as_deref() == Some("CONFLICTING")
            });
        let project = self
            .project
            .as_ref()
            .map(|(p, _, _)| div().min_w_0().truncate().child(p.repo.clone()));
        let parent = self.parent.clone().map(|(id, title)| {
            h_flex()
                .id("parent-thread")
                .min_w_0()
                .gap_1()
                .cursor_pointer()
                .hover(|el| el.text_color(theme.foreground))
                .tooltip(|window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(
                        "The thread that handed this one its task",
                    )
                    .build(window, cx)
                })
                .on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(ThreadViewEvent::Open(id.clone()))),
                )
                .child(Icon::new(IconName::CornerLeftUp).xsmall())
                .child(div().min_w_0().truncate().child(title))
        });
        let keep_id = t.id.clone();
        v_flex()
            .px_4()
            .pt_2()
            .pb_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .child(pip)
                    .child({
                        let rename_id = t.id.clone();
                        let renamable = t.state != ThreadState::Expired;
                        div()
                            .id("thread-title")
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_base()
                            .font_semibold()
                            .child(t.title.clone())
                            .when(renamable, |el| {
                                el.on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                                    if event.click_count() >= 2 {
                                        cx.emit(ThreadViewEvent::Command(
                                            rename_id.clone(),
                                            ThreadCommand::Rename,
                                        ));
                                    }
                                }))
                            })
                    })
                    .when(t.state != ThreadState::Expired, |el| {
                        el.child(self.render_preview(t, cx))
                    })
                    .when(t.state != ThreadState::Expired, |el| {
                        let on = t.keep_running;
                        el.child(
                            Button::new("keep-running")
                                .ghost()
                                .icon(IconName::Coffee)
                                .selected(on)
                                .tooltip(if on {
                                    "Keep running: on"
                                } else {
                                    "Keep running: don't stop this VM when idle"
                                })
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(ThreadViewEvent::Act(Request::KeepRunning {
                                        thread: keep_id.clone(),
                                        on: !on,
                                    }));
                                })),
                        )
                    })
                    .child(more),
            )
            .child(
                h_flex().gap_3().text_xs().text_color(muted).child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .gap_2()
                        .child(vm)
                        .children(unarchive)
                        .children(project.map(|p| {
                            h_flex()
                                .min_w_0()
                                .overflow_hidden()
                                .gap_2()
                                .when(worded, |el| el.child(sep()))
                                .child(p)
                        }))
                        .children(parent.map(|p| {
                            h_flex()
                                .min_w_0()
                                .overflow_hidden()
                                .gap_2()
                                .child(sep())
                                .child(p)
                        }))
                        .children(behind.map(|n| {
                            h_flex()
                                .flex_none()
                                .gap_2()
                                .child(sep())
                                .child(div().text_color(theme.danger).child(format!("{n} behind")))
                        }))
                        .when(conflicts, |el| {
                            el.child(
                                h_flex().flex_none().gap_2().child(sep()).child(
                                    h_flex()
                                        .gap_1()
                                        .text_color(theme.danger)
                                        .child(Icon::new(IconName::TriangleAlert).xsmall())
                                        .child("Conflicts"),
                                ),
                            )
                        }),
                ),
            )
            .children(self.render_setup_failure(t, cx))
            .children(self.render_resume(t, cx))
            .children(self.render_archive(t, cx))
            .children(self.render_service_problems(t, cx))
    }

    /// Preview: with services, a menu of them (state, Open, Output, Restart / Stop / Start) and
    /// any other port; without, the port dialog as before.
    fn render_preview(&self, t: &Thread, cx: &mut Context<Self>) -> AnyElement {
        let services = self.current_services(t).cloned();
        let public = self.public.clone();
        if services.is_none() && public.is_empty() {
            return Button::new("preview")
                .outline()
                .icon(IconName::Globe)
                .label("Preview…")
                .on_click(cx.listener(|this, _, window, cx| this.open_preview(window, cx)))
                .into_any_element();
        }
        let weak = cx.weak_entity();
        let id = t.id.clone();
        let close = weak.clone();
        // Something anyone can open: the button says so.
        let (icon, label) = if public.is_empty() {
            (IconName::Globe, "Preview")
        } else {
            (IconName::LockOpen, "Public")
        };
        Popover::new("services")
            .anchor(Anchor::TopRight)
            .open(self.services_open)
            .on_open_change(move |open, _, cx| {
                let _ = close.update(cx, |v, cx| {
                    v.services_open = *open;
                    cx.notify();
                });
            })
            .trigger(
                Button::new("preview")
                    .outline()
                    .icon(icon)
                    .label(label)
                    .dropdown_caret(true),
            )
            .content(move |_, _, cx| preview_menu(services.as_ref(), &public, &id, &weak, cx))
            .into_any_element()
    }

    /// A service's output, newest at the bottom, with Restart.
    fn open_service_logs(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.services_open = false;
        let Some(run) = self
            .services
            .as_ref()
            .and_then(|s| s.services.iter().find(|r| r.name == name))
            .cloned()
        else {
            return;
        };
        let (weak, id) = (cx.weak_entity(), self.id.clone());
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme().clone();
            let lines = flotilla_model::setup::tail(&run.output, 80);
            let (weak, id, name) = (weak.clone(), id.clone(), run.name.clone());
            dialog
                .title(format!("{} · output", run.name))
                .width(px(680.))
                .child(
                    div()
                        .id("service-output")
                        .max_h(px(420.))
                        .overflow_y_scroll()
                        .rounded_md()
                        .bg(theme.secondary)
                        .px_3()
                        .py_2()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .when(lines.is_empty(), |el| {
                            el.text_color(theme.muted_foreground)
                                .child("Nothing printed yet.")
                        })
                        .children(lines.into_iter().map(|l| div().child(l))),
                )
                .footer(
                    h_flex().justify_end().child(
                        Button::new("service-output-restart")
                            .outline()
                            .icon(IconName::RotateCcw)
                            .label("Restart")
                            .on_click(move |_, window, cx| {
                                let cmd = ServiceCmd::Restart {
                                    thread: id.clone(),
                                    name: name.clone(),
                                };
                                let _ = weak
                                    .update(cx, |_, cx| cx.emit(ThreadViewEvent::Services(cmd)));
                                window.close_dialog(cx);
                            }),
                    ),
                )
        });
        cx.notify();
    }

    /// Services that need you: exited, failed or not answering, with Output and Restart; or a
    /// services file that doesn't check out. Ready and starting ones say nothing here.
    fn render_service_problems(&self, t: &Thread, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !matches!(t.state, ThreadState::Idle | ThreadState::Running) {
            return None;
        }
        let s = self.current_services(t)?;
        let problems: Vec<ServiceRun> = s
            .services
            .iter()
            .filter(|r| service_problem(r))
            .cloned()
            .collect();
        if problems.is_empty() && s.error.is_none() {
            return None;
        }
        let theme = cx.theme().clone();
        let file_error = s.error.clone().map(|e| {
            let id = t.id.clone();
            h_flex()
                .gap_1()
                .text_xs()
                .text_color(theme.danger)
                .child(Icon::new(IconName::CircleX).xsmall().flex_none())
                .child(div().min_w_0().truncate().child(e))
                .child(
                    Button::new("services-retry")
                        .ghost()
                        .xsmall()
                        .label("Try again")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ThreadViewEvent::Services(ServiceCmd::Ensure(id.clone())));
                        })),
                )
        });
        let rows = problems.into_iter().enumerate().map(|(i, run)| {
            let (icon, what) = service_state(&run);
            let (logs, restart) = (
                run.name.clone(),
                ServiceCmd::Restart {
                    thread: t.id.clone(),
                    name: run.name.clone(),
                },
            );
            h_flex()
                .gap_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    Icon::new(icon)
                        .xsmall()
                        .flex_none()
                        .text_color(theme.danger),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(theme.mono_font_family.clone())
                        .child(run.name.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.danger)
                        .child(what),
                )
                .child(
                    Button::new(("service-problem-output", i))
                        .ghost()
                        .xsmall()
                        .label("Output")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_service_logs(logs.clone(), window, cx);
                        })),
                )
                .child(
                    Button::new(("service-problem-restart", i))
                        .ghost()
                        .xsmall()
                        .label("Restart")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ThreadViewEvent::Services(restart.clone()));
                        })),
                )
        });
        Some(
            v_flex()
                .gap_1()
                .children(file_error)
                .children(rows)
                .into_any_element(),
        )
    }

    /// In place of the composer when the start failed: why, and Try again or Discard. The
    /// prompt above stays, marked "not sent".
    fn render_start_failed(&self, t: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (retry_id, discard_id) = (t.id.clone(), t.id.clone());
        v_flex()
            .mx_3()
            .mb_3()
            .mt_1()
            .p_3()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.danger.opacity(0.4))
            .bg(theme.danger.opacity(0.06))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .small()
                            .text_color(theme.danger),
                    )
                    .child(div().font_semibold().child("Couldn't start this thread")),
            )
            .child(
                div().text_sm().text_color(theme.muted_foreground).child(
                    self.start_error
                        .clone()
                        .unwrap_or_else(|| "boat didn't make its VM.".into()),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("retry-start")
                            .small()
                            .primary()
                            .icon(IconName::RefreshCw)
                            .label("Try again")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ThreadViewEvent::Act(Request::Retry(retry_id.clone())))
                            })),
                    )
                    .child(
                        Button::new("discard-start")
                            .small()
                            .ghost()
                            .label("Discard")
                            .tooltip("Remove this thread")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ThreadViewEvent::Act(Request::Delete(discard_id.clone())))
                            })),
                    ),
            )
    }

    /// The thread's agent, model, effort (and fast) where New thread has its pickers, read-only:
    /// they're fixed once the thread starts.
    fn render_agent(&self, t: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let model = self.model_label.clone().unwrap_or_else(|| t.model.clone());
        let settings = format!(
            "{} · {}{}{} · locked",
            fmt::provider_name(&t.provider),
            t.model,
            fmt::effort(t.effort.as_deref())
                .map(|e| format!(" · {e}"))
                .unwrap_or_default(),
            if t.fast { " · fast" } else { "" }
        );
        h_flex()
            .id("thread-settings")
            .flex_none()
            .gap_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(provider_icon(&t.provider, theme.mode.is_dark()))
            .child(div().text_color(theme.foreground).child(model))
            .children(fmt::effort(t.effort.as_deref()))
            .when(t.fast, |el| el.child("fast"))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(settings.clone()).build(window, cx)
            })
    }

    fn render_composer(&self, t: &Thread, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let expired = t.state == ThreadState::Expired;
        // A project thread's first prompt waits for its repo; nothing else goes first.
        let starting = self.start_stages.is_some();
        let (tip, label) = if self.working() {
            ("Steer the running turn (Enter)", "Steer")
        } else if matches!(t.state, ThreadState::Stopped | ThreadState::Stopping) {
            ("Enter", "Resume & send")
        } else {
            ("Enter", "Send")
        };
        let id = t.id.clone();
        let pr_bar = self.render_pr_bar(t, cx);
        let scheduled = self.render_scheduled(cx);
        let can_attach = self.can_attach();
        let later_id = t.id.clone();
        let files =
            crate::attachments::pending_row(&self.attachments, cx, |v: &mut ThreadView, i, _| {
                v.attachments.remove(i)
            });
        let waiting = match (&self.mentions, t.state) {
            (Some(_), _) => "",
            (None, ThreadState::Idle | ThreadState::Running) => "Loading…",
            (None, _) => "Shows once the VM is running",
        };
        let weak = cx.weak_entity();
        let menu = self.menu.render(waiting, &theme, move |i, window, cx| {
            let _ = weak.update(cx, |v, cx| v.accept_mention(Some(i), window, cx));
        });
        v_flex()
            .relative()
            .mx_3()
            .mb_3()
            // Room between the last reply's footer (copy, time) and the box.
            .mt_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .when(can_attach, |el| {
                el.drag_over::<ExternalPaths>(|s, _, _, cx| s.border_color(cx.theme().ring))
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                        this.add_files(paths.paths(), cx)
                    }))
            })
            .children(menu.map(|m| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_full()
                    .mb_1()
                    .child(m)
            }))
            .children(pr_bar)
            .children(scheduled)
            .children(files.map(|f| div().px_3().pt_2().child(f)))
            .child(
                div()
                    .px_1()
                    .pt_1()
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::MoveUp, window, cx| {
                            if this.menu_ready(cx) {
                                this.menu.step(-1);
                                cx.notify();
                                cx.stop_propagation();
                            } else if this.recall_prompt(true, window, cx) {
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::MoveDown, window, cx| {
                            if this.menu_ready(cx) {
                                this.menu.step(1);
                                cx.notify();
                                cx.stop_propagation();
                            } else if this.recall_prompt(false, window, cx) {
                                cx.stop_propagation();
                            }
                        },
                    ))
                    // While the menu has rows, Enter and Tab pick one and Escape closes it.
                    .capture_action(cx.listener(
                        |this, e: &gpui_kit::component::input::Enter, window, cx| {
                            if !e.shift && !e.secondary && this.menu_ready(cx) {
                                this.accept_mention(None, window, cx);
                                cx.stop_propagation();
                            } else if !e.shift && !e.secondary && this.menu.is_open() {
                                // Nothing to pick: Enter closes the menu rather than send the sign.
                                this.menu.dismiss();
                                cx.notify();
                                cx.stop_propagation();
                            }
                        },
                    ))
                    // Whole characters (an emoji family, a flag, an accented letter), not code points.
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::MoveLeft, window, cx| {
                            if this.composer.update(cx, |c, cx| {
                                crate::graphemes::handle(c, crate::graphemes::Key::Left, window, cx)
                            }) {
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::MoveRight, window, cx| {
                            if this.composer.update(cx, |c, cx| {
                                crate::graphemes::handle(
                                    c,
                                    crate::graphemes::Key::Right,
                                    window,
                                    cx,
                                )
                            }) {
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::Backspace, window, cx| {
                            if this.composer.update(cx, |c, cx| {
                                crate::graphemes::handle(
                                    c,
                                    crate::graphemes::Key::Backspace,
                                    window,
                                    cx,
                                )
                            }) {
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::Delete, window, cx| {
                            if this.composer.update(cx, |c, cx| {
                                crate::graphemes::handle(
                                    c,
                                    crate::graphemes::Key::Delete,
                                    window,
                                    cx,
                                )
                            }) {
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::IndentInline, window, cx| {
                            if this.menu_ready(cx) {
                                this.accept_mention(None, window, cx);
                                cx.stop_propagation();
                            } else if this.menu.is_open() {
                                this.menu.dismiss();
                                cx.notify();
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .capture_action(cx.listener(
                        |this, _: &gpui_kit::component::input::Escape, _, cx| {
                            if this.menu.is_open() {
                                this.menu.dismiss();
                                cx.notify();
                                cx.stop_propagation();
                            }
                        },
                    ))
                    .when(expired, |el| el.opacity(0.6))
                    .child(
                        Textarea::new(&self.composer)
                            .appearance(false)
                            .disabled(expired || starting),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .pb_2()
                    // Kept in its place while a turn runs (a steer takes no files), so nothing shifts.
                    .child(
                        crate::attachments::button(
                            "attach",
                            cx.listener(|_, _, window, cx| {
                                crate::attachments::pick(
                                    window,
                                    cx,
                                    |v: &mut ThreadView, paths, cx| v.add_files(&paths, cx),
                                )
                            }),
                        )
                        .disabled(!can_attach),
                    )
                    .child(self.render_agent(t, cx))
                    .child(div().flex_1())
                    .children(crate::meter::reached(
                        &self.limits,
                        self.context.as_ref(),
                        cx,
                    ))
                    .children(crate::meter::context(&self.id, self.context.as_ref(), cx))
                    .when(self.working(), |el| {
                        el.child(
                            Button::new("stop-turn")
                                .small()
                                .danger()
                                .outline()
                                .icon(IconName::Square)
                                .label(if self.interrupting {
                                    "Stopping…"
                                } else {
                                    "Stop"
                                })
                                .loading(self.interrupting)
                                .disabled(self.interrupting)
                                .tooltip("Interrupt the running turn")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    // Say so at once: the turn only ends when the agent stops.
                                    this.interrupting = true;
                                    cx.notify();
                                    cx.emit(ThreadViewEvent::Act(Request::Interrupt(id.clone())));
                                })),
                        )
                    })
                    // A scheduled prompt takes no files: those uploaded may be gone by then.
                    .child(
                        Button::new("send-later")
                            .small()
                            .ghost()
                            .icon(IconName::Clock)
                            .tooltip(if self.attachments.files.is_empty() {
                                "Send later"
                            } else {
                                "Send later takes no files"
                            })
                            .disabled(expired || starting || !self.attachments.files.is_empty())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let text = this.composer.read(cx).value().to_string();
                                let _ = window;
                                cx.emit(ThreadViewEvent::Later {
                                    thread: later_id.clone(),
                                    text,
                                });
                            })),
                    )
                    .child(
                        Button::new("send")
                            .small()
                            .primary()
                            .label(label)
                            .tooltip(tip)
                            .disabled(expired || starting)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(t) = self.thread.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("Loading…")
                .into_any_element();
        };
        let placeholder = match t.state {
            ThreadState::Expired => "Read-only: the VM is gone",
            _ if self.start_stages.is_some() => "Starting…",
            _ if self.working() => "Steer the running turn…",
            _ => "Ask a follow-up…",
        };
        if placeholder != self.placeholder {
            self.placeholder = placeholder;
            self.composer
                .update(cx, |c, cx| c.set_placeholder(placeholder, window, cx));
        }
        if self.hold.animating(Instant::now()) {
            window.request_animation_frame();
        }
        // GitHub said merged (or the wait ran out): no more "Merging…".
        if self.merging_since.is_some()
            && (!self.merging()
                || self
                    .summary
                    .as_ref()
                    .and_then(|s| s.pr.as_ref())
                    .is_some_and(|(pr, _)| pr.state != "OPEN"))
        {
            self.merging_since = None;
        }
        let rows = self.rows.clone();
        let weak = cx.weak_entity();
        let copied = self.copied.clone();
        let flash = self.flash_now(cx);
        if let Some((ident, _)) = &flash {
            window.request_animation_frame();
            // The row gone to stays at the top while it's marked: a tall one (a long reply, the
            // answers opened under their line) lays out short at first, the list finds itself at
            // its end and follows the newest again, pinning the row's end, not its start.
            if let Some(ix) = self.rows.iter().position(|r| r.ident() == ident) {
                self.scroller.update(cx, |s, cx| {
                    let top = s.logical_scroll_top();
                    if s.is_following_tail()
                        && (top.item_ix > ix || (top.item_ix == ix && top.offset_in_item > px(0.)))
                    {
                        s.scroll_to_item(ix, cx);
                    }
                });
            }
        }
        // Rows space themselves (see render_row); drop the scroller's default gap.
        let mut row_style = StyleRefinement::default();
        row_style.padding.bottom = Some(px(0.).into());
        // No padding on the list itself: with it, the furthest a scroll goes falls short of
        // the end the list follows at, so it never followed again and Jump to latest stayed.
        // The first and last rows carry that room instead.
        let mut list_style = StyleRefinement::default();
        (list_style.padding.top, list_style.padding.bottom) =
            (Some(px(0.).into()), Some(px(0.).into()));
        let last = rows.len().saturating_sub(1);
        let scroller =
            MessageScroller::new("messages", self.scroller.clone(), move |ix, window, cx| {
                let row = rows.get(ix);
                let marked = flash
                    .as_ref()
                    .filter(|(ident, _)| row.is_some_and(|r| r.ident() == ident))
                    .map(|(_, a)| *a);
                let el = render_row(row, copied.as_deref(), marked, weak.clone(), window, cx);
                div()
                    .when(ix == 0, |d| d.pt_2())
                    .when(ix == last, |d| d.pb(LIST_END_ROOM))
                    .child(el)
                    .into_any_element()
            })
            .with_row_style(row_style)
            .with_list_style(list_style)
            // Raised off the text it floats over.
            .with_jump_button_renderer({
                let (bg, border) = (theme.popover, theme.border);
                move |b| b.bg(bg).border_color(border).shadow_md()
            })
            .flex_1()
            .min_h_0();
        v_flex()
            .size_full()
            .child(self.render_header(&t, cx))
            .children(self.render_pinned(cx))
            .child(if self.rows.is_empty() {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_color(theme.muted_foreground)
                    .child(crate::pixel::sail(rgb(crate::pixel::LAVENDER).into(), 4.))
                    .child(if t.conversation_id.is_some() {
                        "No messages yet"
                    } else {
                        "Starting…"
                    })
                    .into_any_element()
            } else {
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(scroller)
                    .when(self.older.1, |el| {
                        el.child(
                            h_flex().absolute().top_2().w_full().justify_center().child(
                                h_flex()
                                    .gap_2()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .bg(theme.secondary)
                                    .border_1()
                                    .border_color(theme.border)
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(Spinner::new().xsmall())
                                    .child("Loading earlier messages…"),
                            ),
                        )
                    })
                    .into_any_element()
            })
            .children(self.render_start_stages(cx))
            .child(if self.start_failed() {
                self.render_start_failed(&t, cx).into_any_element()
            } else {
                self.render_composer(&t, cx).into_any_element()
            })
            .into_any_element()
    }
}

/// Flotilla's own message (`Message::from_flotilla`), or a prompt that may be, from a server
/// that doesn't mark them: what its text says decides.
fn flotillas(m: &Message) -> bool {
    m.from_flotilla || m.kind == "prompt"
}

/// What a request of the agent's that came through cut short shows as.
pub(crate) const UNREADABLE_LINE: &str = "Couldn't read a request from the agent: it was cut short";

/// One line for something Flotilla told the agent, in place of its message.
fn flotilla_line(line: String, theme: &gpui_kit::component::Theme) -> AnyElement {
    h_flex()
        .gap_2()
        .px_1()
        .items_start()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(
            div()
                .pt(px(2.))
                .child(Icon::new(IconName::Forward).xsmall()),
        )
        .child(div().min_w_0().child(line))
        .into_any_element()
}

/// One row. `marked`: it was gone to from search, marked this strongly (1 to 0).
fn render_row(
    row: Option<&Row>,
    copied: Option<&str>,
    marked: Option<f32>,
    view: WeakEntity<ThreadView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme().clone();
    let Some(row) = row else {
        return div().into_any_element();
    };
    let menu_view = view.clone();
    let body: AnyElement = match row {
        // A column, as a finished message is: in a row the text has no width to wrap at, and
        // each paragraph ran off the right edge until the turn ended.
        Row::Partial { id, text } => v_flex()
            .child(
                TextView::markdown(
                    SharedString::from(format!("partial-{id}")),
                    format!("{text} ▍"),
                )
                .selectable(true),
            )
            .into_any_element(),
        Row::Report { text, ident } => v_flex()
            .gap_1()
            .pb_1()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Report"),
            )
            .child(
                TextView::markdown(SharedString::from(format!("md-{ident}")), text.clone())
                    .selectable(true)
                    .code_block_actions(code_block_actions),
            )
            .into_any_element(),
        Row::Calls {
            ident,
            calls,
            expanded,
        } => calls_line(ident, calls, *expanded, view.clone(), &theme),
        Row::Message {
            msg: m,
            ident,
            expanded,
            handoff: Some(h),
            ..
        } => {
            // Running while the thread it started works.
            let m = Message {
                pending: h.running,
                ..(**m).clone()
            };
            subagent_card(
                &m,
                &h.card,
                ident,
                *expanded,
                h.thread.then_some(h.child.as_str()),
                view.clone(),
                &theme,
            )
        }
        Row::Message {
            msg: m,
            ident,
            expanded,
            sub: Some(card),
            ..
        } => subagent_card(m, card, ident, *expanded, None, view.clone(), &theme),
        Row::Message {
            msg: m,
            ident,
            expanded,
            actions,
            files,
            from,
            whole,
            scheduled,
            ask,
            answer,
            ..
        } => match m.kind.as_str() {
            // Flotilla's own, as a prompt or, into a running turn, a steer: the same either way.
            // (A server from before it marked them marks none: its prompts are known by their
            // text, as they were.)
            _ if flotillas(m)
                && m.text
                    .as_deref()
                    .and_then(flotilla_model::side::parse_report)
                    .is_some() =>
            {
                // The agents it started in its VM answered together: their answers are in their
                // cards; here, one line marking when they came in.
                let text = m.text.clone().unwrap_or_default();
                let members = flotilla_model::side::parse_report(&text).unwrap_or_default();
                let first = members
                    .first()
                    .map(|(_, id)| id.to_string())
                    .unwrap_or_default();
                let line = match members.as_slice() {
                    [(title, _)] => format!("{title} answered"),
                    _ => format!("{} agents answered", members.len()),
                };
                let answers = (*expanded)
                    .then(|| flotilla_model::side::report_text(&text))
                    .flatten();
                let line = h_flex()
                    .id(SharedString::from(format!("answered-{ident}")))
                    .gap_2()
                    .px_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|el| el.text_color(theme.foreground))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Show their cards")
                            .build(window, cx)
                    })
                    .on_click({
                        let view = view.clone();
                        move |_, _, cx| {
                            let first = first.clone();
                            let _ = view.update(cx, |v, cx| v.show_handoff(&first, cx));
                        }
                    })
                    .child(Icon::new(IconName::Forward).xsmall())
                    .child(div().min_w_0().truncate().child(line));
                answered_line(line, answers, ident, view.clone())
            }
            _ if flotillas(m)
                && m.text
                    .as_deref()
                    .and_then(flotilla_model::orchestrate::parse_couldnt_start)
                    .is_some() =>
            {
                // A thread it handed work to couldn't start: one line saying so, without the id.
                let text = m.text.clone().unwrap_or_default();
                let (title, _, why) =
                    flotilla_model::orchestrate::parse_couldnt_start(&text).unwrap_or_default();
                flotilla_line(format!("Couldn't start {title}: {why}"), &theme)
            }
            _ if flotillas(m)
                && m.text
                    .as_deref()
                    .and_then(flotilla_model::orchestrate::parse_unreadable)
                    .is_some() =>
            {
                // A request of the agent's that came through cut short: it's asked to make it again.
                flotilla_line(UNREADABLE_LINE.to_string(), &theme)
            }
            _ if flotillas(m)
                && m.text
                    .as_deref()
                    .and_then(flotilla_model::orchestrate::parse_answer)
                    .is_some() =>
            {
                // A thread it handed work to answered: the answer is in that one's card, as a
                // subagent's report is; here, one line marking when it came in.
                let text = m.text.clone().unwrap_or_default();
                let (title, child, how, answer) =
                    flotilla_model::orchestrate::parse_answer(&text).unwrap_or_default();
                let child = child.to_string();
                let answer = (*expanded && !answer.is_empty()).then(|| answer.to_string());
                let line = format!("{title} {how}");
                let line = h_flex()
                    .id(SharedString::from(format!("answered-{ident}")))
                    .gap_2()
                    .px_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .hover(|el| el.text_color(theme.foreground))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Show its card")
                            .build(window, cx)
                    })
                    .on_click({
                        let view = view.clone();
                        move |_, _, cx| {
                            let child = child.clone();
                            let _ = view.update(cx, |v, cx| v.show_handoff(&child, cx));
                        }
                    })
                    .child(Icon::new(IconName::Forward).xsmall())
                    .child(div().min_w_0().truncate().child(line));
                answered_line(line, answer, ident, view.clone())
            }
            "prompt" => {
                let not_kept = m.text.as_deref() == Some(PROMPT_NOT_KEPT);
                let text = m.text.clone().unwrap_or_default();
                // A child's answer, sent here by Flotilla, or a child's task, from its parent:
                // from that thread, not from you.
                let from_child = flotilla_model::orchestrate::parse_answer(&text)
                    .map(|(title, id, how, answer)| {
                        (
                            title.to_string(),
                            id.to_string(),
                            how.to_string(),
                            answer.to_string(),
                        )
                    })
                    .or_else(|| {
                        let (id, title) = from.as_deref()?;
                        let task = text.strip_suffix(flotilla_model::orchestrate::CHILD_NOTE)?;
                        Some((
                            title.clone(),
                            id.clone(),
                            m.status.clone().unwrap_or_default(),
                            task.to_string(),
                        ))
                    });
                let text = from_child
                    .as_ref()
                    .map(|(_, _, _, a)| a.clone())
                    .unwrap_or(text);
                let long = !not_kept && is_long_prompt(&text);
                let clipped = long && !*expanded;
                let toggle_ident = ident.clone();
                let toggle_view = view.clone();
                let bubble = v_flex()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .child(
                        h_flex()
                            .justify_between()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match &from_child {
                                Some((title, id, _, _)) => {
                                    let (id, open_view) = (id.clone(), view.clone());
                                    h_flex()
                                        .id(SharedString::from(format!("from-{ident}")))
                                        .gap_1()
                                        .cursor_pointer()
                                        .hover(|el| el.text_color(theme.foreground))
                                        .on_click(move |_, _, cx| {
                                            let id = id.clone();
                                            let _ = open_view.update(cx, |_, cx| {
                                                cx.emit(ThreadViewEvent::Open(id))
                                            });
                                        })
                                        .child(Icon::new(IconName::Forward).xsmall())
                                        .child(format!("From {title}"))
                                        .into_any_element()
                                }
                                None => match scheduled.as_deref() {
                                    // Sent by a schedule, not typed just then: which, and a way to it.
                                    Some((id, timing)) => {
                                        let (id, open_view) = (id.clone(), view.clone());
                                        h_flex()
                                            .id(SharedString::from(format!("scheduled-{ident}")))
                                            .gap_1()
                                            .cursor_pointer()
                                            .hover(|el| el.text_color(theme.foreground))
                                            .on_click(move |_, _, cx| {
                                                let id = id.clone();
                                                let _ = open_view.update(cx, |_, cx| {
                                                    cx.emit(ThreadViewEvent::Scheduled(id))
                                                });
                                            })
                                            .child(Icon::new(IconName::Clock).xsmall())
                                            .child(match timing {
                                                Some(t) => format!("Scheduled · {t}"),
                                                None => "Scheduled".to_string(),
                                            })
                                            .into_any_element()
                                    }
                                    None => div().child("You").into_any_element(),
                                },
                            })
                            .child(match &from_child {
                                Some((_, _, how, _)) => how.clone(),
                                None => m.status.clone().unwrap_or_default(),
                            }),
                    )
                    .when(!m.attachments.is_empty(), |el| {
                        el.child(h_flex().flex_wrap().gap_1p5().pt_1().children(
                            m.attachments.iter().enumerate().map(|(i, name)| {
                                // The same file as sent from here; by place first, as two can share a name.
                                let file = files
                                    .get(i)
                                    .filter(|f| f.name == *name)
                                    .or_else(|| files.iter().find(|f| f.name == *name));
                                crate::attachments::tile(
                                    SharedString::from(format!("{ident}-file-{i}")),
                                    name,
                                    file,
                                    None,
                                    &theme,
                                )
                            }),
                        ))
                    })
                    .child(if not_kept {
                        div()
                            .italic()
                            .text_color(theme.muted_foreground)
                            .child("Prompt not available")
                    } else {
                        div()
                            .relative()
                            // A child's answer is the agent's own writing: markdown.
                            .child(match &from_child {
                                Some(_) => TextView::markdown(
                                    SharedString::from(format!("answer-{ident}")),
                                    text.clone(),
                                )
                                .selectable(true)
                                .into_any_element(),
                                None => div().child(text).into_any_element(),
                            })
                            // Long prompts start cut short, fading out above the toggle.
                            .when(clipped, |el| {
                                el.max_h(px(LONG_PROMPT_HEIGHT)).overflow_hidden().child(
                                    div()
                                        .absolute()
                                        .bottom_0()
                                        .left_0()
                                        .right_0()
                                        .h(px(28.))
                                        .bg(linear_gradient(
                                            180.,
                                            linear_color_stop(theme.secondary.opacity(0.), 0.),
                                            linear_color_stop(theme.secondary, 1.),
                                        )),
                                )
                            })
                    })
                    .when(long, |el| {
                        el.child(
                            h_flex().child(
                                Button::new(SharedString::from(format!("more-{toggle_ident}")))
                                    .ghost()
                                    .xsmall()
                                    .label(if clipped {
                                        "Show full message"
                                    } else {
                                        "Show less"
                                    })
                                    .on_click(move |_, _, cx| {
                                        let ident = toggle_ident.clone();
                                        let _ = toggle_view.update(cx, |v, cx| v.toggle(ident, cx));
                                    }),
                            ),
                        )
                    });
                v_flex()
                    .group("message")
                    .child(bubble)
                    .children(unsent_actions(m, &view))
                    .when(!not_kept, |el| {
                        el.child(message_actions(
                            m,
                            ident,
                            "Copy prompt",
                            copied == Some(ident.as_str()),
                            view.clone(),
                            &theme,
                        ))
                    })
                    .into_any_element()
            }
            "steer" => v_flex()
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            Icon::new(IconName::CornerDownRight)
                                .xsmall()
                                .text_color(theme.primary),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.primary)
                                .child("You steered"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(m.text.clone().unwrap_or_default()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(match m.status.as_deref() {
                                    Some("sending") => "sending",
                                    Some(WAITING_FOR_SERVER) => WAITING_FOR_SERVER,
                                    Some(flotilla_model::normalize::NOT_SENT) => {
                                        flotilla_model::normalize::NOT_SENT
                                    }
                                    Some(_) => "delivered",
                                    None => "",
                                }),
                        ),
                )
                .children(unsent_actions(m, &view))
                .into_any_element(),
            "text" => v_flex()
                .group("message")
                .child(
                    TextView::markdown(
                        SharedString::from(format!("md-{ident}")),
                        m.text.clone().unwrap_or_default(),
                    )
                    .selectable(true)
                    .code_block_actions(code_block_actions),
                )
                .when(*actions, |el| {
                    el.child(message_actions(
                        m,
                        ident,
                        "Copy reply",
                        copied == Some(ident.as_str()),
                        view.clone(),
                        &theme,
                    ))
                })
                .into_any_element(),
            "ask" => crate::ask_card::card(
                m,
                ask.as_deref(),
                answer.as_deref(),
                ident,
                view.clone(),
                &theme,
            ),
            "tool" => {
                let (row_ident, footer_view) = (ident.clone(), view.clone());
                let ident = ident.clone();
                let mono = theme.mono_font_family.clone();
                let status: AnyElement = if m.pending {
                    Spinner::new().xsmall().into_any_element()
                } else if let Some(s) = m.status.clone() {
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(s)
                        .into_any_element()
                } else if m.is_error {
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child("error")
                        .into_any_element()
                } else {
                    div().into_any_element()
                };
                v_flex()
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("tool-{ident}")))
                            .gap_2()
                            .px_2()
                            .py(px(3.))
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|el| el.bg(theme.secondary))
                            .font_family(mono.clone())
                            .text_xs()
                            .on_click(move |_, _, cx| {
                                let ident = ident.clone();
                                let _ = view.update(cx, |v, cx| v.toggle(ident, cx));
                            })
                            .child(
                                Icon::new(if *expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .xsmall()
                                .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(if m.is_error {
                                        theme.danger
                                    } else {
                                        theme.foreground
                                    })
                                    .child(m.tool_name.clone().unwrap_or_default()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(m.tool_summary.clone().unwrap_or_default()),
                            )
                            .child(status),
                    )
                    .when(*expanded, |el| {
                        let shown = match whole.as_deref() {
                            Some(Whole::Text(text)) => text.clone(),
                            _ => tool_detail(m),
                        };
                        let detail = div()
                            .id(SharedString::from(format!("detail-{row_ident}")))
                            .ml(px(22.))
                            .mt_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(theme.muted)
                            .font_family(mono)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(shown);
                        // What was cut, and Show all / Copy all, above it: in reach however long it is.
                        el.children(whole.as_deref().map(|w| {
                            cut_footer(
                                m,
                                &row_ident,
                                w,
                                copied == Some(row_ident.as_str()),
                                footer_view.clone(),
                                &theme,
                            )
                        }))
                        .child(detail)
                    })
                    .into_any_element()
            }
            _ => h_flex()
                .gap_2()
                .text_xs()
                .text_color(if m.is_error {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(Icon::new(IconName::Info).xsmall())
                .child(m.text.clone().unwrap_or_default())
                .into_any_element(),
        },
    };
    // Right-click a reply, prompt or steer: copy the selection or the whole message.
    let body = match row {
        Row::Message { msg: m, ident, .. }
            if matches!(m.kind.as_str(), "text" | "prompt" | "steer")
                && m.text.as_deref().is_some_and(|t| t != PROMPT_NOT_KEPT) =>
        {
            let text = m.text.clone().unwrap_or_default();
            div()
                .id(SharedString::from(format!("menu-{ident}")))
                .child(body)
                .context_menu(move |menu, window, cx| {
                    message_menu(menu, &text, menu_view.clone(), window, cx)
                })
                .into_any_element()
        }
        _ => body,
    };
    // A new turn gets room above it; runs of tool calls stay tight.
    let (top, bottom) = match row {
        Row::Message { msg, .. } if msg.kind == "prompt" => (px(14.), px(6.)),
        Row::Message { sub: Some(_), .. } => (px(4.), px(2.)),
        Row::Message { msg, .. } if msg.kind == "tool" => (px(1.), px(1.)),
        Row::Calls { .. } => (px(1.), px(1.)),
        Row::Message { msg, .. } if msg.kind == flotilla_proto::ask::KIND => (px(8.), px(8.)),
        _ => (px(4.), px(4.)),
    };
    // A subagent's own calls and report hang off its card, on a rail under its chevron.
    let nested = matches!(row, Row::Message { nested: true, .. } | Row::Report { .. });
    // Gone to from search: the selection's lavender behind it, for a moment.
    let tint = marked.map(|a| theme.selection.opacity(a));
    if nested {
        return div()
            .px_4()
            .when_some(tint, |el, t| el.bg(t))
            .child(
                div()
                    .ml(px(14.))
                    .pl(px(10.))
                    .border_l_1()
                    .border_color(theme.border)
                    .pt(top)
                    .pb(bottom)
                    .child(body),
            )
            .into_any_element();
    }
    div()
        .px_4()
        .pt(top)
        .pb(bottom)
        .when_some(tint, |el, t| el.bg(t))
        .child(body)
        .into_any_element()
}

/// Calls folded into one line, as a call's own line looks: what was called how often, and
/// while one runs, what it's at. A click opens it (its calls follow on their rows) or folds it.
fn calls_line(
    ident: &str,
    calls: &Calls,
    open: bool,
    view: WeakEntity<ThreadView>,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let status: AnyElement = if calls.running.is_some() {
        Spinner::new().xsmall().into_any_element()
    } else if calls.failed > 0 {
        div()
            .text_xs()
            .text_color(theme.danger)
            .child(format!("{} failed", calls.failed))
            .into_any_element()
    } else if let Some(s) = calls.status.clone() {
        div()
            .text_xs()
            .text_color(theme.warning)
            .child(s)
            .into_any_element()
    } else {
        div().into_any_element()
    };
    let toggle_ident = ident.to_string();
    h_flex()
        .id(SharedString::from(format!("calls-{ident}")))
        .gap_2()
        .px_2()
        .py(px(3.))
        .rounded_md()
        .cursor_pointer()
        .hover(|el| el.bg(theme.secondary))
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .on_click(move |_, _, cx| {
            let ident = toggle_ident.clone();
            let _ = view.update(cx, |v, cx| v.toggle(ident, cx));
        })
        .child(
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .flex_none()
                .text_color(theme.foreground)
                .child(calls.label.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(calls.running.clone().unwrap_or_default()),
        )
        .child(status)
        .into_any_element()
}

/// A subagent call: what it's called, what kind it is, and how it's going (its latest call
/// while it runs, the start of its report once it's done). Open, it shows what it was asked;
/// its own calls and its report follow on their rows.
/// A subagent's card, or (`thread`) a thread this one handed work to, drawn the same way with
/// a link to open it.
/// A line marking answers Flotilla brought in; opened (gone to from search), the answers
/// themselves under it, and Show less.
fn answered_line(
    line: Stateful<Div>,
    answers: Option<String>,
    ident: &str,
    view: WeakEntity<ThreadView>,
) -> AnyElement {
    let Some(answers) = answers else {
        return line.into_any_element();
    };
    let toggle_ident = ident.to_string();
    v_flex()
        .gap_1()
        .child(line)
        .child(
            div().pl_5().child(
                TextView::markdown(SharedString::from(format!("answers-{ident}")), answers)
                    .selectable(true),
            ),
        )
        .child(
            h_flex().pl_4().child(
                Button::new(SharedString::from(format!("less-{ident}")))
                    .ghost()
                    .xsmall()
                    .label("Show less")
                    .on_click(move |_, _, cx| {
                        let ident = toggle_ident.clone();
                        let _ = view.update(cx, |v, cx| v.toggle(ident, cx));
                    }),
            ),
        )
        .into_any_element()
}

fn subagent_card(
    m: &Message,
    card: &SubCard,
    ident: &str,
    open: bool,
    thread: Option<&str>,
    view: WeakEntity<ThreadView>,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let muted = theme.muted_foreground;
    let running = m.pending;
    let calls = card.calls.and_then(calls_label);
    let took = card.duration_ms.filter(|_| !running).map(|ms| {
        let s = (ms / 1000).max(1);
        if s >= 60 {
            format!("{}m {:02}s", s / 60, s % 60)
        } else {
            format!("{s}s")
        }
    });
    let meta: Vec<String> = [
        card.kind.clone(),
        card.background.then(|| "in the background".to_string()),
        calls,
        took,
    ]
    .into_iter()
    .flatten()
    .collect();
    let status: AnyElement = if running {
        Spinner::new().xsmall().into_any_element()
    } else if let Some(s) = m.status.clone() {
        div()
            .text_xs()
            .text_color(theme.warning)
            .child(s)
            .into_any_element()
    } else if m.is_error {
        div()
            .text_xs()
            .text_color(theme.danger)
            .child("error")
            .into_any_element()
    } else {
        div().into_any_element()
    };
    // Closed: a line on how it's going.
    let line: Option<AnyElement> = if open {
        None
    } else if running {
        Some(match &card.latest {
            Some((name, summary)) => h_flex()
                .gap_2()
                .min_w_0()
                .font_family(theme.mono_font_family.clone())
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.foreground)
                        .child(name.clone()),
                )
                .child(div().flex_1().min_w_0().truncate().child(summary.clone()))
                .into_any_element(),
            None => div().child("Working…").into_any_element(),
        })
    } else {
        card.report
            .as_deref()
            .and_then(report_preview)
            .map(|l| div().truncate().child(l).into_any_element())
    };
    let toggle = ident.to_string();
    let open_view = view.clone();
    v_flex()
        .id(SharedString::from(format!("tool-{ident}")))
        .gap_1()
        .px_2()
        .py(px(6.))
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(|el| el.bg(theme.secondary))
        .on_click(move |_, _, cx| {
            let ident = toggle.clone();
            let _ = view.update(cx, |v, cx| v.toggle(ident, cx));
        })
        .child(
            h_flex()
                .gap_2()
                .child(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall()
                    .text_color(muted),
                )
                .child(Icon::new(IconName::Bot).small().text_color(theme.primary))
                .child(
                    div()
                        .flex_none()
                        .max_w(px(360.))
                        .truncate()
                        .text_sm()
                        .font_medium()
                        .child(card.description.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(meta.join(" · ")),
                )
                .child(status),
        )
        .children(line.map(|l| {
            div()
                .pl(px(22.))
                .text_xs()
                .text_color(muted)
                .min_w_0()
                .child(l)
        }))
        .when(open && !card.prompt.is_empty(), |el| {
            el.child(
                v_flex()
                    .pl(px(22.))
                    .gap(px(2.))
                    .text_xs()
                    .child(div().text_color(muted).child("Asked to"))
                    .child(div().text_color(theme.foreground).child(TextView::markdown(
                        SharedString::from(format!("asked-{ident}")),
                        card.prompt.clone(),
                    ))),
            )
        })
        .when_some(thread.map(str::to_string).filter(|_| open), |el, child| {
            el.child(
                h_flex().pl(px(22.)).child(
                    Button::new(SharedString::from(format!("open-{child}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::ArrowUpRight)
                        .label("Open thread")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let child = child.clone();
                            let _ =
                                open_view.update(cx, |_, cx| cx.emit(ThreadViewEvent::Open(child)));
                        }),
                ),
            )
        })
        .into_any_element()
}

/// A report's line for its closed card: the first that says something, not a heading or a
/// label ("1. `uname -a`:") over what follows; its prose first, else its code.
fn report_preview(report: &str) -> Option<String> {
    let mut code = false;
    let (mut prose, mut first_code) = (None, None);
    for line in report.lines().map(str::trim) {
        if line.starts_with("```") {
            code = !code;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if code {
            first_code = first_code.or(Some(line));
        } else if !line.starts_with('#') && !line.ends_with(':') {
            prose = Some(line);
            break;
        }
    }
    prose
        .or(first_code)
        .and_then(flotilla_model::util::preview_line)
        .or_else(|| flotilla_model::util::preview_line(report))
}

/// "1 call", "3 calls"; nothing for none.
fn calls_label(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 call".into()),
        n => Some(format!("{n} calls")),
    }
}

/// Prompts longer than this (T3 Code's thresholds) start collapsed to `LONG_PROMPT_HEIGHT`.
const LONG_PROMPT_LINES: usize = 8;
const LONG_PROMPT_CHARS: usize = 600;
const LONG_PROMPT_HEIGHT: f32 = 176.;

fn is_long_prompt(text: &str) -> bool {
    text.lines().count() > LONG_PROMPT_LINES || text.chars().count() > LONG_PROMPT_CHARS
}

/// How long a used copy button shows its check.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1000);

/// The last reply of each turn, by index into `messages`: a turn ends at the next prompt. The
/// running turn's replies are left out until it ends.
fn final_replies(messages: &[Message], working: bool) -> HashSet<usize> {
    let mut finals = HashSet::new();
    let mut last_text = None;
    for (i, m) in messages.iter().enumerate() {
        match m.kind.as_str() {
            "prompt" => finals.extend(last_text.take()),
            "text" => last_text = Some(i),
            _ => {}
        }
    }
    if !working {
        finals.extend(last_text);
    }
    finals
}

/// The right-click menu on a message: Copy takes the text selection (from anywhere in the
/// window, as Markdown source where the selection is in a reply), Copy message the whole one,
/// and Cite quotes the selection, or the whole message, into the composer.
fn message_menu(
    menu: PopupMenu,
    text: &str,
    view: WeakEntity<ThreadView>,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let selected = gpui_kit::base::TextSelection::selected_text(window, cx);
    let whole = text.to_string();
    let copy_selected = selected.clone();
    let can_cite = view
        .read_with(cx, |v, _| {
            v.thread
                .as_ref()
                .is_some_and(|t| t.state != ThreadState::Expired)
        })
        .unwrap_or(false);
    let (cite_label, quote) = if selected.trim().is_empty() {
        ("Cite message", whole.clone())
    } else {
        ("Cite selection", selected.clone())
    };
    let too_long = quote.chars().count() > MAX_CITE_CHARS;
    menu.item(
        PopupMenuItem::new("Copy")
            .icon(IconName::Copy)
            .disabled(selected.is_empty())
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_selected.clone()))
            }),
    )
    .item(
        PopupMenuItem::new("Copy message")
            .icon(IconName::Copy)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(whole.clone()))
            }),
    )
    .when(can_cite, |menu| {
        menu.separator().item(
            PopupMenuItem::new(if too_long {
                "Selection too long to cite"
            } else {
                cite_label
            })
            .icon(IconName::Quote)
            .disabled(too_long)
            .on_click(move |_, window, cx| {
                let _ = view.update(cx, |v, cx| v.cite(&quote, window, cx));
            }),
        )
    })
}

/// The last code block copied (a hash of its text) and when, for its check mark.
static CODE_COPIED: std::sync::Mutex<Option<(u64, std::time::Instant)>> =
    std::sync::Mutex::new(None);
/// How long a code block's copy button shows its check (T3 Code's timing).
const CODE_COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1200);

/// The corner of a fenced code block: its language and a copy button.
fn code_block_actions(
    block: &gpui_kit::base::text::CodeBlock,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let code = block.code().to_string();
    let mut h = DefaultHasher::new();
    code.hash(&mut h);
    let key = h.finish();
    let copied = CODE_COPIED
        .lock()
        .ok()
        .and_then(|c| *c)
        .is_some_and(|(k, at)| k == key && at.elapsed() < CODE_COPIED_FOR);
    let muted = cx.theme().muted_foreground;
    h_flex()
        .gap_1()
        .text_xs()
        .text_color(muted)
        .children(
            block
                .lang()
                .filter(|l| !l.is_empty())
                .map(|l| div().child(l)),
        )
        .child(
            Button::new(SharedString::from(format!("copy-code-{key}")))
                .ghost()
                .xsmall()
                .icon(if copied {
                    IconName::Check
                } else {
                    IconName::Copy
                })
                .tooltip(if copied { "Copied" } else { "Copy code" })
                .on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                    if let Ok(mut c) = CODE_COPIED.lock() {
                        *c = Some((key, std::time::Instant::now()));
                    }
                    window.refresh();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(CODE_COPIED_FOR).await;
                        cx.update(|cx| cx.refresh_windows());
                    })
                    .detach();
                }),
        )
        .into_any_element()
}

/// T3 Code's limit for a citation.
const MAX_CITE_CHARS: usize = 8000;

/// `text` as a Markdown block quote.
fn block_quote(text: &str) -> String {
    text.trim()
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                ">".to_string()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The row under a message, shown on hover: copy it, and when it was sent.
/// Send again and Discard, under a message a quit cut short that boat never got.
fn unsent_actions(m: &Message, view: &WeakEntity<ThreadView>) -> Option<AnyElement> {
    // The stored send's id (a steer's row key carries a prefix; its task id doesn't).
    let id = m.task_id.clone().filter(|k| {
        k.starts_with(flotilla_model::lifecycle::UNSENT_PREFIX)
            && m.status.as_deref() == Some(flotilla_model::normalize::NOT_SENT)
    })?;
    let text = m.text.clone().unwrap_or_default();
    let act = |again: bool| {
        let (view, id, text) = (view.clone(), id.clone(), text.clone());
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| {
                cx.emit(ThreadViewEvent::Unsent {
                    thread: v.id.clone(),
                    id: id.clone(),
                    text: text.clone(),
                    again,
                })
            });
        }
    };
    Some(
        h_flex()
            .gap_2()
            .pt_1()
            .child(
                Button::new(SharedString::from(format!("send-again-{id}")))
                    .small()
                    .outline()
                    .label("Send again")
                    .on_click(act(true)),
            )
            .child(
                Button::new(SharedString::from(format!("discard-{id}")))
                    .small()
                    .ghost()
                    .label("Discard")
                    .on_click(act(false)),
            )
            .into_any_element(),
    )
}

fn message_actions(
    m: &Message,
    ident: &str,
    copy_label: &'static str,
    copied: bool,
    view: WeakEntity<ThreadView>,
    theme: &gpui_kit::component::Theme,
) -> impl IntoElement {
    let text = m.text.clone().unwrap_or_default();
    let ident = ident.to_string();
    let now = now_ms();
    let time = m
        .timestamp
        .map(|ms| (fmt::message_time(ms, now), fmt::full_date_time(ms)));
    h_flex()
        .gap_1()
        .mt_1()
        .text_xs()
        .text_color(theme.muted_foreground)
        .when(!copied, |el| {
            el.invisible().group_hover("message", |s| s.visible())
        })
        .child(
            Button::new(SharedString::from(format!("copy-{ident}")))
                .ghost()
                .xsmall()
                .icon(if copied {
                    IconName::Check
                } else {
                    IconName::Copy
                })
                .tooltip(if copied { "Copied" } else { copy_label })
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    let ident = ident.clone();
                    let _ = view.update(cx, |v, cx| v.mark_copied(ident, cx));
                }),
        )
        .children(time.map(|(short, full)| {
            div()
                .id(SharedString::from(format!("time-{}", m.seq)))
                .child(short)
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
                })
        }))
}

/// What an expanded tool row shows, from its result, cut short for the row.
fn tool_detail(m: &Message) -> String {
    ellipsize(&tool_text(m, false), 6000)
}

/// What a tool's result says, whole; with `all` (Show all, Copy all), a file read as the file
/// itself rather than its line count.
fn tool_text(m: &Message, all: bool) -> String {
    let r: Value = m
        .tool_result_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match m.tool_result_type.as_deref() {
        Some("bash_output") => {
            let out = s("content");
            if out.is_empty() {
                "(no output)".to_string()
            } else {
                out
            }
        }
        Some("file_write") => format!("wrote {}\n\n{}", s("filePath"), s("content")),
        Some("file_content") if all => match r.get("content").and_then(Value::as_str) {
            Some(text) => text.to_string(),
            None => r
                .get("lines")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| l.get("content").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
        },
        Some("file_content") => {
            let n = r
                .get("lines")
                .and_then(Value::as_array)
                .map(|l| l.len())
                .unwrap_or(0);
            format!("read {} ({n} lines)", s("filePath"))
        }
        Some("file_edit") => {
            let minus: Vec<String> = s("oldString").lines().map(|l| format!("- {l}")).collect();
            let plus: Vec<String> = s("newString").lines().map(|l| format!("+ {l}")).collect();
            format!(
                "{}\n{}\n{}",
                s("filePath"),
                minus.join("\n"),
                plus.join("\n")
            )
        }
        // Plain text (kept as a JSON string), or an `output`.
        _ => match (&r, r.get("output")) {
            (Value::String(o), _) | (_, Some(Value::String(o))) => o.clone(),
            (_, Some(o)) => serde_json::to_string_pretty(o).unwrap_or_default(),
            (_, None) => m.tool_input_json.clone().unwrap_or_default(),
        },
    }
}

/// How a cut tool output shows: whole once Show all asked for it (or loading, or why not);
/// cut otherwise, or loading while Copy all waits for it, or why that failed.
fn shown_whole(
    wholes: &HashMap<String, Whole>,
    shown: &HashSet<String>,
    copying: &HashSet<String>,
    key: &str,
) -> Whole {
    match wholes.get(key) {
        Some(w) if shown.contains(key) => w.clone(),
        Some(Whole::Loading) if copying.contains(key) => Whole::Loading,
        Some(Whole::Failed(e)) => Whole::Failed(e.clone()),
        _ => Whole::Cut,
    }
}

/// Over a cut tool output: how much of it came, and Show all (Show less) / Copy all.
fn cut_footer(
    m: &Message,
    ident: &str,
    whole: &Whole,
    copied: bool,
    view: WeakEntity<ThreadView>,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let total: u64 = m.cut.iter().map(|c| c.bytes).sum();
    let said = match whole {
        Whole::Cut => format!(
            "Cut to {} of {}",
            fmt::bytes(m.cut.len() as u64 * flotilla_proto::history::CAP as u64),
            fmt::bytes(total)
        ),
        Whole::Loading => "Loading the rest…".to_string(),
        Whole::Text(_) => format!("All {}", fmt::bytes(total)),
        Whole::Failed(e) => format!("Couldn't load the rest: {e}"),
    };
    let msg = Rc::new(m.clone());
    let button = |id: &str,
                  label: &'static str,
                  act: fn(&mut ThreadView, &Message, &mut Context<ThreadView>)| {
        let (view, msg) = (view.clone(), msg.clone());
        Button::new(SharedString::from(format!("{id}-{ident}")))
            .ghost()
            .xsmall()
            .label(label)
            .on_click(move |_, _, cx| {
                let _ = view.update(cx, |v, cx| act(v, &msg, cx));
            })
    };
    h_flex()
        .ml(px(22.))
        .mt_1()
        .gap_1()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(div().pr_1().child(said))
        .when(matches!(whole, Whole::Cut | Whole::Failed(_)), |el| {
            el.child(button(
                "show-all",
                if matches!(whole, Whole::Failed(_)) {
                    "Try again"
                } else {
                    "Show all"
                },
                |v, m, cx| v.ask_whole(m, false, cx),
            ))
        })
        .when(matches!(whole, Whole::Text(_)), |el| {
            el.child(button("show-less", "Show less", ThreadView::cut_again))
        })
        .when(!matches!(whole, Whole::Loading), |el| {
            el.child(button(
                "copy-all",
                if copied { "Copied" } else { "Copy all" },
                |v, m, cx| v.ask_whole(m, true, cx),
            ))
        })
        .into_any_element()
}

/// The agent logo, for use outside the sidebar.
pub fn provider_icon(provider: &str, dark: bool) -> AnyElement {
    match provider_logo(provider, dark) {
        Some(path) => img(path).size(px(14.)).into_any_element(),
        None => div()
            .text_xs()
            .child(fmt::monogram(provider))
            .into_any_element(),
    }
}

/// A service that needs you: exited, failed, or running but never answering.
fn service_problem(run: &ServiceRun) -> bool {
    matches!(run.state.as_str(), "exited" | "failed" | "not_responding")
}

/// A service's state in words, with its icon: "ready · :4100", "starting…", "exited 1".
fn service_state(run: &ServiceRun) -> (IconName, String) {
    match run.state.as_str() {
        "ready" => (IconName::CircleCheck, format!("ready · :{}", run.port)),
        "starting" => (IconName::Loader, "starting…".into()),
        "not_responding" => (IconName::Clock, format!("not responding on :{}", run.port)),
        "exited" => (
            IconName::CircleX,
            format!(
                "exited {}",
                run.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "early".into())
            ),
        ),
        "stopped" => (IconName::CircleDot, "stopped".into()),
        _ => (
            IconName::CircleX,
            format!(
                "failed{}",
                run.error
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            ),
        ),
    }
}

/// Preview's menu: the thread's services, a row each (status pip, name, state, then Open, its
/// other links, Output, Restart / Stop / Start and Make public…, and while public, how long
/// for, Copy link and Make private), the file's problem if it has one; other ports made public
/// (Open, Copy link, Make private); and Another port… for anything else listening in the VM.
fn preview_menu(
    services: Option<&ThreadServices>,
    public: &[PublicLink],
    thread: &str,
    view: &WeakEntity<ThreadView>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme().clone();
    let muted = theme.muted_foreground;
    let now = now_ms();
    let send = |cmd: ServiceCmd| {
        let view = view.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| {
                v.services_open = false;
                cx.emit(ThreadViewEvent::Services(cmd.clone()));
                cx.notify();
            });
        }
    };
    let act = |req: Request| {
        let view = view.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| {
                v.services_open = false;
                cx.emit(ThreadViewEvent::Act(req.clone()));
                cx.notify();
            });
        }
    };
    let make_public = |port: u16, what: String| {
        let view = view.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |v, cx| {
                v.open_make_public(port, what.clone(), window, cx)
            });
        }
    };
    // How long it's public, Copy link and Make private, under a row.
    let public_line = |key: usize, link: &PublicLink| {
        let left = link
            .left(now)
            .map(|l| format!("Public · {l}"))
            .unwrap_or_else(|| "Public until you stop it".into());
        let url = link.url.clone();
        h_flex()
            .pl(px(14.))
            .gap_1()
            .child(
                Icon::new(IconName::LockOpen)
                    .xsmall()
                    .text_color(theme.warning),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(left),
            )
            // Its URL isn't known until boat has answered (boat down as it was made public).
            .when(!url.is_empty(), |el| {
                el.child(
                    Button::new(("public-copy", key))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Copy)
                        .label("Copy link")
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                            window.push_notification(
                                gpui_kit::component::notification::Notification::success(
                                    "Link copied",
                                ),
                                cx,
                            );
                        }),
                )
            })
            .child(
                Button::new(("public-end", key))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Lock)
                    .label("Make private")
                    .on_click(act(Request::MakePrivate {
                        thread: thread.to_string(),
                        port: link.port,
                    })),
            )
    };
    let runs: &[ServiceRun] = services.map(|s| s.services.as_slice()).unwrap_or_default();
    let rows = runs.iter().enumerate().map(|(i, run)| {
        let (_, what) = service_state(run);
        let color = match run.state.as_str() {
            "ready" => theme.success,
            "starting" => theme.warning,
            "stopped" => muted.opacity(0.6),
            _ => theme.danger,
        };
        let live = matches!(run.state.as_str(), "starting" | "ready" | "not_responding");
        let name = run.name.clone();
        let restart = ServiceCmd::Restart {
            thread: thread.to_string(),
            name: name.clone(),
        };
        let stop = ServiceCmd::Stop {
            thread: thread.to_string(),
            name: name.clone(),
        };
        // The primary link opens with Open; extra portals are their titles.
        let mut links = run.links.clone();
        if links.is_empty()
            && let Some(url) = &run.preview
        {
            links.push(flotilla_proto::Link {
                title: run.name.clone(),
                url: url.clone(),
            });
        }
        let has_link = !links.is_empty();
        let first = (!links.is_empty()).then(|| links.remove(0));
        let public = public.iter().find(|l| l.port == run.port);
        let logs_view = view.clone();
        v_flex()
            .gap_1()
            .py_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(crate::pixel::pip(color, 6.))
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(run.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(if service_problem(run) {
                                theme.danger
                            } else {
                                muted
                            })
                            .child(what),
                    ),
            )
            .child(
                h_flex()
                    .pl(px(14.))
                    .gap_1()
                    .children(first.map(|l| {
                        let url = l.url.clone();
                        Button::new(("service-open", i))
                            .outline()
                            .xsmall()
                            .icon(IconName::ExternalLink)
                            .label("Open")
                            .disabled(!live)
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    }))
                    .children(links.into_iter().enumerate().map(|(j, l)| {
                        let url = l.url.clone();
                        Button::new(("service-link", i * 16 + j))
                            .ghost()
                            .xsmall()
                            .label(l.title.clone())
                            .disabled(!live)
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    }))
                    .child(
                        Button::new(("service-output", i))
                            .ghost()
                            .xsmall()
                            .label("Output")
                            .on_click(move |_, window, cx| {
                                let _ = logs_view.update(cx, |v, cx| {
                                    v.open_service_logs(name.clone(), window, cx)
                                });
                            }),
                    )
                    .child(if run.state == "stopped" {
                        Button::new(("service-start", i))
                            .ghost()
                            .xsmall()
                            .label("Start")
                            .on_click(send(restart.clone()))
                    } else {
                        Button::new(("service-restart", i))
                            .ghost()
                            .xsmall()
                            .label("Restart")
                            .on_click(send(restart.clone()))
                    })
                    .when(live, |el| {
                        el.child(
                            Button::new(("service-stop", i))
                                .ghost()
                                .xsmall()
                                .label("Stop")
                                .on_click(send(stop.clone())),
                        )
                    })
                    .when(has_link && public.is_none(), |el| {
                        el.child(
                            Button::new(("service-public", i))
                                .ghost()
                                .xsmall()
                                .label("Make public…")
                                .on_click(make_public(run.port, run.name.clone())),
                        )
                    }),
            )
            .children(public.map(|l| public_line(i, l)))
    });
    // Other ports made public (Preview… on a port): Open (waking the VM if it's stopped).
    let others: Vec<&PublicLink> = public
        .iter()
        .filter(|l| !runs.iter().any(|r| r.port == l.port))
        .collect();
    let other_rows = others.into_iter().enumerate().map(|(i, link)| {
        let key = 1000 + i;
        v_flex()
            .gap_1()
            .py_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::Globe).xsmall().text_color(muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(format!("port {}", link.port)),
                    ),
            )
            .child(
                h_flex().pl(px(14.)).gap_1().child(
                    Button::new(("public-open", key))
                        .outline()
                        .xsmall()
                        .icon(IconName::ExternalLink)
                        .label("Open")
                        .on_click(act(Request::Preview {
                            thread: thread.to_string(),
                            port: link.port,
                        })),
                ),
            )
            .child(public_line(key, link))
    });
    let other = view.clone();
    v_flex()
        .w(px(380.))
        .gap_1()
        .children(services.and_then(|s| s.error.clone()).map(|e| {
            h_flex()
                .gap_1()
                .items_start()
                .text_xs()
                .text_color(theme.danger)
                .child(Icon::new(IconName::CircleX).xsmall().flex_none().mt(px(2.)))
                .child(div().min_w_0().child(e))
        }))
        .children(rows)
        .children(other_rows)
        .child(div().my_1().h(px(1.)).bg(theme.border))
        .child({
            let hover = theme.secondary;
            h_flex()
                .id("services-other-port")
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .hover(move |st| st.bg(hover))
                .child(Icon::new(IconName::Globe).small().text_color(muted))
                .child("Another port…")
                .on_click(move |_, window, cx| {
                    let _ = other.update(cx, |v, cx| {
                        v.services_open = false;
                        v.open_preview(window, cx);
                    });
                })
        })
        .into_any_element()
}

fn output_box(lines: Vec<String>, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .w_full()
        .rounded_md()
        .bg(theme.secondary)
        .px_2()
        .py_1()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .text_color(theme.muted_foreground)
        .children(lines.into_iter().map(|l| div().truncate().child(l)))
}

#[cfg(test)]
mod tests {
    use super::{HOLD_DRAIN, HOLD_TO_MERGE, Hold, ease_out};
    use std::time::Instant;

    /// A tool's plain-text output (a JSON string), cut or whole, is shown as its output, not
    /// its input.
    #[test]
    fn a_plain_text_tool_result_shows_as_its_output() {
        let m = flotilla_proto::Message {
            kind: "tool".into(),
            tool_input_json: Some(r#"{"query":"x"}"#.into()),
            tool_result_json: Some(serde_json::Value::String("found 3 results".into()).to_string()),
            ..Default::default()
        };
        assert_eq!(super::tool_text(&m, false), "found 3 results");
        let long = serde_json::Value::String("result line\n".repeat(5000)).to_string();
        let cut = flotilla_proto::history::cap(&flotilla_proto::Message {
            tool_result_json: Some(long),
            ..m
        });
        assert!(super::tool_text(&cut, false).starts_with("result line\nresult line"));
    }

    #[test]
    fn a_run_of_calls_is_said_by_tool_and_count() {
        assert_eq!(super::called_label(["Bash"; 9]), "Called Bash 9 times");
        assert_eq!(
            super::called_label(["Bash", "Read", "Bash", "Grep", "Read", "Bash"]),
            "Called Bash 3 times, Read twice, Grep once"
        );
    }

    /// Calls one after another fold into one line, which opens to show them on its rail; a
    /// lone call, and calls with a reply between them, stay as they were.
    #[test]
    fn runs_of_calls_fold_into_one_line() {
        use super::{Row, fold_calls};
        use std::collections::HashSet;
        let row = |seq: i64, kind: &str, name: &str| Row::Message {
            msg: Box::new(flotilla_proto::Message {
                seq,
                kind: kind.into(),
                tool_name: Some(name.into()),
                is_error: seq == 3,
                ..Default::default()
            }),
            ident: format!("m{seq}"),
            expanded: false,
            actions: false,
            nested: false,
            sub: None,
            files: Vec::new(),
            handoff: None,
            from: None,
            whole: None,
            scheduled: None,
            ask: None,
            answer: None,
        };
        let rows = || {
            vec![
                row(1, "tool", "Bash"),
                row(2, "tool", "Bash"),
                row(3, "tool", "Bash"),
                row(4, "text", ""),
                row(5, "tool", "Read"),
                row(6, "steer", ""),
                row(7, "tool", "Bash"),
                row(8, "tool", "Read"),
            ]
        };
        let shown = |rows: &[Row]| {
            rows.iter()
                .map(|r| match r {
                    Row::Calls { calls, .. } => {
                        format!("[{}, {} failed]", calls.label, calls.failed)
                    }
                    Row::Message {
                        nested: true,
                        ident,
                        ..
                    } => format!("  {ident}"),
                    r => r.ident().to_string(),
                })
                .collect::<Vec<_>>()
        };
        let mut open = HashSet::new();
        assert_eq!(
            shown(&fold_calls(rows(), &mut open, None, true)),
            [
                "[Called Bash 3 times, 1 failed]",
                "m4",
                "m5",
                "m6",
                "[Called Bash once, Read once, 0 failed]",
            ]
        );
        // Opened: its calls under it.
        open.insert("calls:m1".to_string());
        assert_eq!(
            shown(&fold_calls(rows(), &mut open, None, true))[..4],
            ["[Called Bash 3 times, 1 failed]", "  m1", "  m2", "  m3"]
        );
        // A search hit in a folded run opens it; going back to a place doesn't.
        let mut open = HashSet::new();
        fold_calls(rows(), &mut open, Some(8), false);
        assert!(open.is_empty());
        fold_calls(rows(), &mut open, Some(8), true);
        assert!(open.contains("calls:m7"));
    }

    #[test]
    fn ease_out_is_strong_and_ends_where_it_should() {
        assert!(ease_out(0.).abs() < 1e-3);
        assert!((ease_out(1.) - 1.).abs() < 1e-3);
        // A strong ease-out: most of the way there early on.
        assert!(ease_out(0.25) > 0.7, "{}", ease_out(0.25));
        assert!(
            (0..=10)
                .map(|i| ease_out(i as f32 / 10.))
                .collect::<Vec<_>>()
                .windows(2)
                .all(|w| w[0] <= w[1] + 1e-4)
        );
    }

    #[test]
    fn holding_fills_linearly_and_letting_go_drains() {
        let t0 = Instant::now();
        let hold = Hold {
            pressed: Some(t0),
            drain: None,
            pr: Some(7),
        };
        assert!((hold.progress(t0 + HOLD_TO_MERGE / 2) - 0.5).abs() < 0.01);
        assert_eq!(hold.progress(t0 + HOLD_TO_MERGE * 2), 1.);
        assert!(hold.animating(t0));
        let drain = Hold {
            pressed: None,
            drain: Some((t0, 0.6)),
            pr: None,
        };
        assert!((drain.progress(t0) - 0.6).abs() < 1e-3);
        assert!(drain.progress(t0 + HOLD_DRAIN / 2) < 0.2);
        assert!(drain.progress(t0 + HOLD_DRAIN) < 1e-3);
        assert!(!drain.animating(t0 + HOLD_DRAIN));
        assert_eq!(Hold::default().progress(t0), 0.);
    }

    #[test]
    fn a_closed_card_previews_what_its_report_says_not_its_labels() {
        use super::report_preview;
        // Labels over code: the code's first line.
        let labelled = "1. `uname -a`:\n```\nLinux box 6.8.0 x86_64\n```\n\n2. `df -h /`:\n```\n/dev/vda1 79G\n```";
        assert_eq!(
            report_preview(labelled).as_deref(),
            Some("Linux box 6.8.0 x86_64")
        );
        // Prose wins over code, past a heading and a label.
        let prose = "## Summary\nStep 1 (`ls ~`):\n```\nDesktop\n```\nStep 2 (`sleep 16`): completed with no output.";
        assert_eq!(
            report_preview(prose).as_deref(),
            Some("Step 2 (sleep 16): completed with no output.")
        );
        assert_eq!(
            report_preview("Found 3 call sites.").as_deref(),
            Some("Found 3 call sites.")
        );
        // Nothing but labels: the first, as before.
        assert_eq!(report_preview("Done:").as_deref(), Some("Done:"));
        assert_eq!(report_preview("  \n"), None);
    }

    #[test]
    fn copy_all_leaves_a_cut_output_cut() {
        use super::{Whole, shown_whole};
        use std::collections::{HashMap, HashSet};
        let wholes: HashMap<String, Whole> = [
            ("a".into(), Whole::Text("all".into())),
            ("b".into(), Whole::Loading),
            ("c".into(), Whole::Failed("no".into())),
        ]
        .into();
        let none = HashSet::new();
        let shown: HashSet<String> = ["a".to_string(), "b".to_string()].into();
        let copying: HashSet<String> = ["b".to_string()].into();
        // Fetched for Copy all only: still cut.
        assert_eq!(shown_whole(&wholes, &none, &none, "a"), Whole::Cut);
        // Show all: whole (Show less takes it out of `shown` again).
        assert_eq!(
            shown_whole(&wholes, &shown, &none, "a"),
            Whole::Text("all".into())
        );
        assert_eq!(shown_whole(&wholes, &shown, &none, "b"), Whole::Loading);
        assert_eq!(shown_whole(&wholes, &none, &copying, "b"), Whole::Loading);
        assert_eq!(shown_whole(&wholes, &none, &none, "b"), Whole::Cut);
        assert_eq!(
            shown_whole(&wholes, &none, &none, "c"),
            Whole::Failed("no".into())
        );
        assert_eq!(shown_whole(&wholes, &none, &none, "d"), Whole::Cut);
    }

    /// The message that took an answer is hidden only while its question's card is loaded too;
    /// on its own (the card on an older page), it shows. Either way it stays a prompt, so the
    /// replies before it end their turn.
    #[test]
    fn an_answer_hides_behind_its_card_only_when_the_card_is_there() {
        use flotilla_proto::Message;
        let m = |seq: i64, kind: &str, tool_id: Option<&str>, answer_to: Option<&str>| Message {
            seq,
            kind: kind.into(),
            tool_id: tool_id.map(str::to_string),
            answer_to: answer_to.map(str::to_string),
            text: Some("t".into()),
            ..Default::default()
        };
        let all = [
            m(48, "prompt", None, None),
            m(49, "ask", Some("q1"), None),
            m(50, "text", None, None),
            m(51, "prompt", None, Some("q1")),
            m(52, "text", None, None),
            m(53, "steer", None, Some("q2")),
        ];
        assert_eq!(super::hidden_answers(all.iter()), [51].into());
        assert_eq!(
            super::hidden_answers(all[2..].iter()),
            Default::default(),
            "its card on a page not loaded"
        );
        assert_eq!(
            super::final_replies(&all, false),
            [50, 52].into_iter().map(|s| s as usize - 48).collect()
        );
    }

    #[test]
    fn prompt_states_say_what_they_wait_for() {
        use super::{WAITING_FOR_SERVER, shown_status};
        let s = |v: &str| Some(v.to_string());
        // A new thread's first prompt waits for its VM, not behind anything.
        assert_eq!(
            shown_status(s("queued"), true, false, false, false),
            s("starting")
        );
        assert_eq!(
            shown_status(s("queued"), false, true, false, false),
            s("starting")
        );
        assert_eq!(
            shown_status(s("queued"), false, false, false, false),
            s("queued")
        );
        // Sent while the server's out of reach: held here for it.
        assert_eq!(
            shown_status(s("sending"), false, false, false, true),
            s(WAITING_FOR_SERVER)
        );
        assert_eq!(
            shown_status(s("sending"), false, false, false, false),
            s("sending")
        );
        // A failed start never sent it, whatever the connection.
        assert_eq!(
            shown_status(s("sending"), true, false, true, true),
            s(flotilla_model::normalize::NOT_SENT)
        );
        assert_eq!(
            shown_status(s("finished"), true, true, false, true),
            s("finished")
        );
        assert_eq!(shown_status(None, true, true, true, true), None);
    }
}
