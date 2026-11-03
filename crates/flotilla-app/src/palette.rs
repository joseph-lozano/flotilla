//! ⌘K (Ctrl+K off a Mac): one list to search, run and open. The open thread's commands and
//! the app's, threads by title (or PR, branch, project), and threads by what was said in them
//! (the server's search, `Request::Search`), all by T3 Code's rules (`flotilla_proto::search`).
//! The keyboard drives it: ↑ and ↓ move, Enter picks, Escape clears what's typed and then closes
//! it. Typed after `>`: commands only.
//!
//! - Groups order by their best match (a name before a typo), keeping their order on ties.
//!   Commands found only by their keywords come after the threads.
//! - A thread found by its messages is listed once, at its best message, unless its title
//!   already lists it; picking it opens the thread at that message.
//! - What you open from a search is remembered for that query, on the server (`thread.opened`):
//!   typed again, it comes first among word matches.
//! - Nothing is highlighted until a word is typed or an arrow pressed, so ⌘K then Enter does
//!   nothing.
//! - The highlight stays on its row (by what it picks, not its place) as rows come and go.
//! - While the server is asked, what it found for what was typed before isn't shown: a quiet
//!   "Searching messages…" row stands in, and the palette keeps its height. Enter then waits
//!   for the answer and picks the highlighted row (the first, unless an arrow moved it).

use crate::app::FlotillaApp;
use crate::thread_menu::ThreadCommand;
use flotilla_proto::search::{self, HitKind, Ranked, SearchHit};
use flotilla_proto::{Activity, ThreadSummary};
use gpui_kit::assets::IconName;
use gpui_kit::base::actions::Confirm;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme, Disableable as _, Icon, IndexPath, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;

/// Threads shown with nothing typed: the most recently active.
const RECENT: usize = 6;
/// How long typing pauses before the server is asked.
const PAUSE: Duration = Duration::from_millis(140);

/// What a row does when it's picked.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    App(AppCommand),
    /// One of the open thread's commands.
    Thread(String, ThreadCommand),
    Open(String),
    /// A message: its thread, open at it.
    Message {
        thread: String,
        seq: i64,
    },
}

impl Pick {
    /// The thread it opens, if it opens one.
    fn opens(&self) -> Option<&str> {
        match self {
            Pick::Open(t) | Pick::Message { thread: t, .. } => Some(t),
            _ => None,
        }
    }
}

/// The app's own commands.
#[derive(Clone, Debug, PartialEq)]
pub enum AppCommand {
    NewThread,
    Settings,
    ConnectServer,
    UseThisMachine,
    UseServer,
    AddProject,
    SyncNow,
    ToggleSidebar,
    NextThread,
    PreviousThread,
    ArchiveAndNext,
    Scheduled,
}

/// A command as a row: what it says, what else finds it, its icon and its shortcut.
pub struct Entry {
    pub label: String,
    pub keywords: &'static str,
    pub icon: IconName,
    pub key: Option<Box<dyn Action>>,
    pub pick: Pick,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Entry {
            label: self.label.clone(),
            keywords: self.keywords,
            icon: self.icon,
            key: self.key.as_ref().map(|k| k.boxed_clone()),
            pick: self.pick.clone(),
        }
    }
}

impl Entry {
    /// How it matches `query` (folded): by what it says, else by its keywords only.
    fn matched(&self, query: &str, typos: bool) -> Option<Found> {
        let keywords: Vec<&str> = self.keywords.split_whitespace().collect();
        search::score_terms(query, &self.label, &keywords, typos).map(|m| Found {
            score: m.quality,
            quality: m.quality,
            group_quality: m.group_quality,
            strong: m.strong,
            picked_at: 0,
            picked_exactly: false,
            ranges: m.ranges,
            hint: None,
        })
    }
}

/// A thread as a row.
#[derive(Clone)]
pub struct ThreadRow {
    pub id: String,
    pub title: String,
    /// Its project (`owner/repo`), or none.
    pub project: Option<String>,
    pub branch: Option<String>,
    /// Its pull request: number, title and link.
    pub pr: Option<(i64, String, String)>,
    pub archived: bool,
    pub at: i64,
    /// When it began, and when you last wrote in it (ms).
    pub created: i64,
    pub prompted: Option<i64>,
    pub opens: Option<search::Opens>,
}

impl ThreadRow {
    pub fn of(s: &ThreadSummary, project: Option<String>) -> ThreadRow {
        ThreadRow {
            id: s.thread.id.clone(),
            title: s.thread.title.clone(),
            branch: s.thread.branch.clone(),
            pr: s
                .pr
                .as_ref()
                .map(|(pr, _)| (pr.number, pr.title.clone(), pr.url.clone())),
            archived: matches!(s.activity, Activity::Archived | Activity::Expired),
            at: s.last_activity_ms,
            created: flotilla_model::util::iso_to_ms(&s.thread.created_at).unwrap_or(0),
            prompted: s.prompted_ms,
            opens: s.opens.clone(),
            project,
        }
    }

    /// As search sees it.
    fn subject(&self) -> search::Subject<'_> {
        let prs = self
            .pr
            .iter()
            .map(|(n, title, url)| search::PrRef {
                number: *n,
                title: Some(title.as_str()),
                references: [
                    Some(url.clone()),
                    self.project.as_ref().map(|p| format!("{p}#{n}")),
                ]
                .into_iter()
                .flatten()
                .collect(),
            })
            .collect();
        search::Subject {
            id: Some(&self.id),
            title: &self.title,
            branch: self.branch.as_deref(),
            project: self.project.as_deref(),
            prs,
            // What you last wrote, else when it began.
            activity: Some(self.prompted.unwrap_or(self.created)),
            opens: self.opens.as_ref(),
        }
    }
}

/// How a row matched the query, for ranking and marking.
#[derive(Clone, Debug)]
struct Found {
    score: f64,
    quality: f64,
    group_quality: f64,
    strong: bool,
    picked_at: i64,
    picked_exactly: bool,
    /// What matched in its title or name (character ranges).
    ranges: Vec<(usize, usize)>,
    /// Why a thread matched when its title shows nothing.
    hint: Option<search::Hint>,
}

impl Ranked for Found {
    fn score(&self) -> f64 {
        self.score
    }
    fn quality(&self) -> f64 {
        self.quality
    }
    fn strong(&self) -> bool {
        self.strong
    }
    fn picked_at(&self) -> i64 {
        self.picked_at
    }
    fn picked_exactly(&self) -> bool {
        self.picked_exactly
    }
    fn group_quality(&self) -> f64 {
        self.group_quality
    }
}

impl Found {
    fn of_thread(m: search::ThreadMatch) -> Found {
        Found {
            score: m.score,
            quality: m.quality,
            group_quality: m.group_quality,
            strong: m.strong,
            picked_at: m.picked_at,
            picked_exactly: m.picked_exactly,
            ranges: m.ranges,
            hint: m.hint,
        }
    }
}

pub enum PaletteEvent {
    /// Ask the server for threads by these words, this many at most.
    Search(String, u32),
    /// A row picked, and the query it was found by (none with nothing typed, or for commands).
    Picked(Pick, Option<String>),
}

impl EventEmitter<PaletteEvent> for PaletteView {}

/// What a row shows.
#[derive(Clone)]
enum Shows {
    /// A command, and what matched in its name.
    Command(Entry, Vec<(usize, usize)>),
    /// A thread, what matched in its title, and why it matched if the title shows nothing.
    Thread(ThreadRow, Vec<(usize, usize)>, Option<search::Hint>),
    /// A thread found by its messages, at the best one.
    Hit(SearchHit),
    Searching,
}

/// A row: what it picks (none for one that only says something), and what it shows.
#[derive(Clone)]
struct Row {
    pick: Option<Pick>,
    shows: Shows,
}

impl Row {
    fn command(e: &Entry, ranges: Vec<(usize, usize)>) -> Row {
        Row {
            pick: Some(e.pick.clone()),
            shows: Shows::Command(e.clone(), ranges),
        }
    }
}

pub struct PaletteView {
    state: Entity<CommandState>,
    lists: Lists,
    /// What each row shown picks, by its group and place, as last drawn.
    shown: Vec<Vec<Option<Pick>>>,
    /// The palette's height when last drawn with nothing being searched, kept while searching.
    height: Pixels,
    pause: Option<Task<()>>,
}

/// What the palette lists and highlights, apart from drawing it.
struct Lists {
    /// The open thread's commands (under its title), and the app's.
    here: Option<(String, Vec<Entry>)>,
    commands: Vec<Entry>,
    threads: Vec<ThreadRow>,
    query: String,
    /// What the server found for `found_for`, and whether a search for the query now is out.
    hits: Vec<SearchHit>,
    found_for: String,
    searching: bool,
    error: Option<String>,
    /// When the palette opened: ranking is as of then, so rows don't reorder as time passes.
    now: i64,
    /// The highlighted row, by what it picks: kept as rows come and go.
    chosen: Option<Pick>,
    /// Whether the highlight was moved (an arrow, the pointer) since the query changed.
    moved: bool,
    /// Enter was pressed while a search was out: the highlighted row is picked when it's answered.
    enter: bool,
}

impl Lists {
    fn new(
        here: Option<(String, Vec<Entry>)>,
        commands: Vec<Entry>,
        threads: Vec<ThreadRow>,
        now: i64,
    ) -> Lists {
        Lists {
            here,
            commands,
            threads,
            query: String::new(),
            hits: vec![],
            found_for: String::new(),
            searching: false,
            error: None,
            now,
            chosen: None,
            moved: false,
            enter: false,
        }
    }

    /// The query as typed: commands only after `>`, and the query folded (empty: nothing typed).
    fn parsed(&self) -> (bool, String) {
        let q = self.query.trim();
        match q.strip_prefix('>') {
            Some(rest) => (true, search::fold_query(rest)),
            None => (false, search::fold_query(q)),
        }
    }

    /// The query a pick was found by, for the server to remember: none with nothing typed, or
    /// after `>`.
    fn pick_query(&self) -> Option<String> {
        let (commands_only, folded) = self.parsed();
        (!commands_only && !folded.is_empty()).then(|| self.query.trim().to_string())
    }

    /// `query` typed: what to ask the server for (the query, and how many hits), if anything.
    fn typed(&mut self, query: &str) -> Option<(String, u32)> {
        self.query = query.to_string();
        // A new query: the first row, unless an arrow moves it; Enter waits for it only.
        (self.chosen, self.moved, self.enter, self.error) = (None, false, false, None);
        let (commands_only, _) = self.parsed();
        // Only a word long enough for the index finds messages; titles are found here.
        self.searching = !commands_only && search::finds_messages(self.query.trim());
        if !self.searching {
            self.hits.clear();
            self.found_for.clear();
            return None;
        }
        Some((self.query.trim().to_string(), search::LIMIT))
    }

    /// The server's answer for `query`. None if it's not for what's typed now; else, if Enter
    /// was pressed while it was out, the row to pick now.
    fn answered(
        &mut self,
        query: &str,
        result: Result<Vec<SearchHit>, String>,
    ) -> Option<Option<Pick>> {
        if query != self.query.trim() {
            return None;
        }
        self.searching = false;
        match result {
            Ok(hits) => {
                self.hits = hits;
                self.found_for = query.to_string();
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
        let enter = std::mem::take(&mut self.enter);
        Some(enter.then(|| self.target(&self.rows())).flatten())
    }

    /// The hits for what's typed; none while a search for it is out (those held are for what was
    /// typed before).
    fn hits_now(&self) -> &[SearchHit] {
        if self.found_for == self.query.trim() {
            &self.hits
        } else {
            &[]
        }
    }

    /// The rows, in their groups: the open thread's commands and the app's (those found by what
    /// they say), the threads, commands found only by their keywords, then the threads found by
    /// their messages. With nothing typed, every command and the latest threads.
    fn rows(&self) -> Vec<(SharedString, Vec<Row>)> {
        let (commands_only, q) = self.parsed();
        let here = self
            .here
            .as_ref()
            .map(|(title, _)| SharedString::from(title.clone()));
        let none: Vec<Entry> = Vec::new();
        let here_entries = self.here.as_ref().map_or(&none, |(_, e)| e);
        if q.is_empty() {
            let mut groups: Vec<(SharedString, Vec<Row>)> = Vec::new();
            if let Some(h) = here {
                groups.push((
                    h,
                    here_entries
                        .iter()
                        .map(|e| Row::command(e, vec![]))
                        .collect(),
                ));
            }
            groups.push((
                "Commands".into(),
                self.commands
                    .iter()
                    .map(|e| Row::command(e, vec![]))
                    .collect(),
            ));
            if !commands_only {
                let latest = self.threads.iter().filter(|t| !t.archived).take(RECENT);
                groups.push((
                    "Threads".into(),
                    latest
                        .map(|t| Row {
                            pick: Some(Pick::Open(t.id.clone())),
                            shows: Shows::Thread(t.clone(), vec![], None),
                        })
                        .collect(),
                ));
            }
            groups.retain(|(_, rows)| !rows.is_empty());
            return groups;
        }
        // Typos only when nothing matches at a word start anywhere.
        let thread = |t: &ThreadRow, typos: bool| {
            search::score_thread(&q, &t.subject(), self.now, typos, true).map(Found::of_thread)
        };
        let strong = here_entries
            .iter()
            .chain(&self.commands)
            .filter_map(|e| e.matched(&q, false))
            .any(|m| m.strong)
            || (!commands_only
                && self
                    .threads
                    .iter()
                    .filter_map(|t| thread(t, false))
                    .any(|m| m.strong));
        let typos = !strong;
        // Commands by what they say, and by their keywords only (after the threads).
        let mut by_keywords: Vec<(Row, Found)> = Vec::new();
        let mut commands = |entries: &[Entry]| -> Vec<(Row, Found)> {
            let mut rows = Vec::new();
            for e in entries {
                let Some(m) = e.matched(&q, typos) else {
                    continue;
                };
                let row = Row::command(e, m.ranges.clone());
                if m.ranges.is_empty() {
                    by_keywords.push((row, m))
                } else {
                    rows.push((row, m))
                }
            }
            rows
        };
        let mut groups: Vec<(SharedString, Vec<(Row, Found)>)> = Vec::new();
        if let Some(h) = here {
            groups.push((h, commands(here_entries)));
        }
        groups.push(("Commands".into(), commands(&self.commands)));
        if !commands_only {
            let mut threads: Vec<(Row, Found)> = self
                .threads
                .iter()
                .filter_map(|t| {
                    thread(t, typos).map(|m| {
                        (
                            Row {
                                pick: Some(Pick::Open(t.id.clone())),
                                shows: Shows::Thread(t.clone(), m.ranges.clone(), m.hint.clone()),
                            },
                            m,
                        )
                    })
                })
                .collect();
            threads.sort_by(|(_, l), (_, r)| search::compare(l, r));
            threads.truncate(search::THREAD_ROWS);
            groups.push(("Threads".into(), threads));
        }
        let more = if groups.iter().any(|(_, rows)| {
            rows.iter()
                .any(|(r, _)| matches!(r.shows, Shows::Command(..)))
        }) {
            "More commands"
        } else {
            "Commands"
        };
        groups.push((more.into(), by_keywords));
        for (_, rows) in &mut groups {
            rows.sort_by(|(_, l), (_, r)| search::compare(l, r));
        }
        // By their best match, keeping their order on ties.
        let mut ranked: Vec<(u8, usize, SharedString, Vec<Row>)> = groups
            .into_iter()
            .enumerate()
            .filter(|(_, (_, rows))| !rows.is_empty())
            .map(|(k, (h, rows))| {
                (
                    search::group_class(rows.iter().map(|(_, m)| m)),
                    k,
                    h,
                    rows.into_iter().map(|(r, _)| r).collect(),
                )
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut groups: Vec<(SharedString, Vec<Row>)> = ranked
            .into_iter()
            .map(|(_, _, h, rows)| (h, rows))
            .collect();
        if !commands_only {
            // A thread its title already lists isn't listed again for its messages.
            let titled: std::collections::HashSet<&str> = self
                .threads
                .iter()
                .filter(|t| {
                    search::score_thread(&q, &t.subject(), self.now, false, false).is_some()
                })
                .map(|t| t.id.as_str())
                .collect();
            let mut messages: Vec<Row> = self
                .hits_now()
                .iter()
                .filter(|h| h.kind != HitKind::Title && !titled.contains(h.thread.as_str()))
                .filter_map(|h| {
                    Some(Row {
                        pick: Some(Pick::Message {
                            thread: h.thread.clone(),
                            seq: h.seq?,
                        }),
                        shows: Shows::Hit(h.clone()),
                    })
                })
                .take(search::MESSAGE_ROWS)
                .collect();
            if self.searching {
                messages.push(Row {
                    pick: None,
                    shows: Shows::Searching,
                });
            }
            if !messages.is_empty() {
                groups.push(("In messages".into(), messages));
            }
        }
        groups
    }

    /// The row to highlight: none with nothing typed until an arrow is pressed; else the one
    /// highlighted if it's still there, else the first.
    fn target(&self, rows: &[(SharedString, Vec<Row>)]) -> Option<Pick> {
        if self.parsed().1.is_empty() && !self.moved {
            return None;
        }
        let mut picks = rows
            .iter()
            .flat_map(|(_, r)| r)
            .filter_map(|r| r.pick.as_ref());
        match &self.chosen {
            Some(c) if picks.clone().any(|p| p == c) => Some(c.clone()),
            _ => picks.next().cloned(),
        }
    }
}

impl PaletteView {
    pub fn new(
        here: Option<(String, Vec<Entry>)>,
        commands: Vec<Entry>,
        threads: Vec<ThreadRow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        PaletteView {
            state,
            lists: Lists::new(here, commands, threads, flotilla_model::util::now_ms()),
            shown: vec![],
            height: px(0.),
            pause: None,
        }
    }

    fn query_changed(&mut self, query: &str, cx: &mut Context<Self>) {
        self.pause = self.lists.typed(query).map(|(q, limit)| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(PAUSE).await;
                let _ = this.update(cx, |_, cx| cx.emit(PaletteEvent::Search(q, limit)));
            })
        });
        cx.notify();
    }

    /// The server's answer for `query`: shown if it's still what's typed. If Enter was pressed
    /// while it was out, the highlighted row is picked now.
    pub fn set_hits(
        &mut self,
        query: &str,
        result: Result<Vec<SearchHit>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(enter) = self.lists.answered(query, result) else {
            return;
        };
        self.state
            .update(cx, |s, cx| s.set_loading(false, window, cx));
        match enter {
            Some(pick) => self.pick(pick, cx),
            None => cx.notify(),
        }
    }

    fn picked(&mut self, at: IndexPath, cx: &mut Context<Self>) {
        if let Some(pick) = self
            .shown
            .get(at.section)
            .and_then(|g| g.get(at.row))
            .cloned()
            .flatten()
        {
            self.pick(pick, cx);
        }
    }

    fn pick(&mut self, pick: Pick, cx: &mut Context<Self>) {
        let query = pick.opens().and_then(|_| self.lists.pick_query());
        cx.emit(PaletteEvent::Picked(pick, query));
    }

    /// The highlight moved to `at` (an arrow, the pointer, or this putting it there).
    fn selected(&mut self, at: IndexPath, cx: &mut Context<Self>) {
        let pick = self
            .shown
            .get(at.section)
            .and_then(|g| g.get(at.row))
            .cloned()
            .flatten();
        self.lists.moved = true;
        if pick != self.lists.chosen {
            self.lists.chosen = pick;
            // Its marks follow the highlight.
            cx.notify();
        }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Rows shown, messages found for what's typed, whether a search is out, and what's highlighted.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_state(&self) -> (usize, usize, bool, Option<Pick>) {
        let found = self
            .lists
            .hits_now()
            .iter()
            .filter(|h| h.kind != HitKind::Title)
            .count();
        (
            self.shown.iter().map(Vec::len).sum(),
            found,
            self.lists.searching,
            self.lists.chosen.clone(),
        )
    }
}

impl Render for PaletteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = self.lists.searching;
        if self.state.read(cx).is_loading() != searching {
            self.state
                .update(cx, |s, cx| s.set_loading(searching, window, cx));
        }
        let rows = self.lists.rows();
        let target = self.lists.target(&rows);
        // On Day's light lavender highlight, marks need more of it to show.
        let strong = !cx.theme().mode.is_dark();
        let me = cx.entity().downgrade();
        let (on_query, on_pick, on_select) = (me.clone(), me.clone(), me.clone());
        let error = self.lists.error.clone();
        let mut command = Command::new(&self.state)
            .filterable(false)
            .bordered(false)
            .max_h(px(440.))
            .placeholder("Search threads, messages and commands")
            .on_query(move |q, _, cx| {
                let _ = on_query.update(cx, |this, cx| this.query_changed(q, cx));
            })
            .on_select(move |at, _, cx| {
                let _ = on_select.update(cx, |this, cx| this.selected(at, cx));
            })
            .on_confirm(move |at, _, cx| {
                let _ = on_pick.update(cx, |this, cx| this.picked(at, cx));
            })
            .empty(|_, _, cx| {
                div()
                    .py_6()
                    .w_full()
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Nothing matches")
            })
            .when_some(error, |c, e| {
                c.footer(move |_, _, cx| {
                    div()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Couldn't search the messages: {e}"))
                })
            });
        let mut at = None;
        let mut shown = Vec::new();
        for (g, (heading, group)) in rows.into_iter().enumerate() {
            let mut items = Vec::new();
            let mut picks = Vec::new();
            for (r, row) in group.into_iter().enumerate() {
                let selected = row.pick.is_some() && row.pick == target;
                if selected {
                    at = Some(IndexPath::new(r).section(g));
                }
                items.push(match row.shows {
                    Shows::Command(e, ranges) => {
                        command_item(&e, &ranges, selected && strong, window, cx)
                    }
                    Shows::Thread(t, ranges, hint) => {
                        thread_item(&t, &ranges, hint.as_ref(), selected && strong)
                    }
                    Shows::Hit(h) => hit_item(&h, selected && strong),
                    Shows::Searching => CommandItem::new()
                        .label("Searching messages…")
                        .disabled(true),
                });
                picks.push(row.pick);
            }
            command = command.group(CommandGroup::new().label(heading).items(items));
            shown.push(picks);
        }
        self.shown = shown;
        self.lists.chosen = target;
        // Put in now (not as the element is laid out), so the highlight can go to its row in
        // these rows before they're drawn. What it gives back is the state, drawn below.
        drop(RenderOnce::render(command, window, cx));
        if self.state.read(cx).selected_index() != at {
            self.state
                .update(cx, |s, cx| s.set_selected_index(at, window, cx));
        }
        div()
            .w_full()
            .relative()
            .bg(cx.theme().popover)
            .when(searching, |el| el.min_h(self.height))
            // Enter while the server is asked: picked once it answers.
            .capture_action(cx.listener(|this, _: &Confirm, _, cx| {
                if this.lists.searching {
                    this.lists.enter = true;
                    cx.stop_propagation();
                }
            }))
            .child(self.state.clone())
            .child(
                canvas(
                    move |bounds, _, cx| {
                        if !searching {
                            let _ = me.update(cx, |this, _| this.height = bounds.size.height);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}

/// A command's row: its icon, what it does (what matched marked), and its shortcut.
fn command_item(
    e: &Entry,
    ranges: &[(usize, usize)],
    strong: bool,
    window: &mut Window,
    _: &mut App,
) -> CommandItem {
    let kbd = e
        .key
        .as_ref()
        .and_then(|a| Kbd::binding_for_action(a.as_ref(), None, window));
    let (label, icon) = (e.label.clone(), e.icon);
    let marks = search::byte_ranges(&label, ranges);
    CommandItem::new()
        .label(e.label.clone())
        .child(move |_, cx| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    Icon::new(icon)
                        .size_4()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(marked(&label, &marks, strong, cx)),
                )
                .children(kbd.clone().map(|k| k.ml_auto()))
        })
}

/// A thread's row: its title (what matched marked), then why it matched if the title shows
/// nothing (its PR, branch or project, marked), else its project; and how long ago. `strong`:
/// marks that show on the highlight.
fn thread_item(
    t: &ThreadRow,
    ranges: &[(usize, usize)],
    hint: Option<&search::Hint>,
    strong: bool,
) -> CommandItem {
    let t = t.clone();
    let marks = search::byte_ranges(&t.title, ranges);
    let hint = hint.map(|h| {
        (
            format!("{}: {}", h.label, h.text),
            h.label.chars().count() + 2,
            search::byte_ranges(&h.text, &h.ranges),
        )
    });
    CommandItem::new()
        .label(t.title.clone())
        .child(move |_, cx| {
            let theme = cx.theme();
            let now = flotilla_model::util::now_ms();
            let detail: AnyElement = match &hint {
                // The label's bytes before the field's text: its marks move by them.
                Some((line, lead, field)) => {
                    let shift = line
                        .char_indices()
                        .nth(*lead)
                        .map_or(line.len(), |(i, _)| i) as u32;
                    let field: Vec<[u32; 2]> =
                        field.iter().map(|[a, b]| [a + shift, b + shift]).collect();
                    marked(line, &field, strong, cx).into_any_element()
                }
                None => [
                    t.project.clone(),
                    t.archived.then(|| "Archived".to_string()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
                .into_any_element(),
            };
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    Icon::new(if t.archived {
                        IconName::Archive
                    } else {
                        IconName::MessageSquare
                    })
                    .size_4()
                    .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(marked(&t.title, &marks, strong, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(detail),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(crate::fmt::ago(t.at, now)),
                )
        })
}

/// A thread found by its messages: its title and when its best message was said, then who
/// said it (or who Flotilla brought it from) and the line around what matched (a command in
/// monospace). `strong`: marks that show on the highlight.
fn hit_item(h: &SearchHit, strong: bool) -> CommandItem {
    let h = h.clone();
    CommandItem::new()
        .label(h.title.clone())
        .child(move |_, cx| {
            let theme = cx.theme();
            let now = flotilla_model::util::now_ms();
            let who = match (&h.from, h.kind) {
                (Some(from), _) => from.as_str(),
                (None, HitKind::Prompt | HitKind::Steer) => "You",
                (None, HitKind::Text) => "Reply",
                (None, HitKind::Tool) => "Command",
                (None, HitKind::Title) => "Title",
            };
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_start()
                .child(
                    div().pt(px(2.)).child(
                        Icon::new(IconName::MessageSquare)
                            .size_4()
                            .text_color(theme.muted_foreground),
                    ),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(1.))
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(div().flex_1().min_w_0().truncate().child(h.title.clone()))
                                .children(h.at.map(|at| {
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(crate::fmt::ago(at, now))
                                })),
                        )
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(div().flex_none().child(format!("{who}:")))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .when(h.kind == HitKind::Tool, |el| {
                                            el.font_family(theme.mono_font_family.clone())
                                        })
                                        .child(marked(&h.snippet, &h.marks, strong, cx)),
                                )
                                // There's no find in a thread yet: how many of its messages match.
                                .when(h.hits > 1, |el| {
                                    el.child(div().flex_none().child(matches(h.hits)))
                                }),
                        ),
                )
        })
}

/// "12 matches": a thread's messages that match, on its hit's row.
fn matches(n: u32) -> String {
    format!("{n} matches")
}

/// `text` with the matched ranges in bold on the selection's tint; `strong`, twice as much of
/// it (Day's highlighted row is that tint already).
fn marked(text: &str, marks: &[[u32; 2]], strong: bool, cx: &App) -> StyledText {
    let theme = cx.theme();
    let tint = if strong {
        theme.selection.opacity(2.)
    } else {
        theme.selection
    };
    let style = HighlightStyle {
        font_weight: Some(FontWeight::SEMIBOLD),
        background_color: Some(tint),
        ..Default::default()
    };
    let ranges = marks
        .iter()
        .map(|[a, b]| (*a as usize, *b as usize))
        .filter(|(a, b)| {
            a < b && *b <= text.len() && text.is_char_boundary(*a) && text.is_char_boundary(*b)
        })
        .map(|(a, b)| (a..b, style));
    StyledText::new(text.to_string()).with_highlights(ranges)
}

/// Open the palette over the window, focused on its field.
pub fn open(view: Entity<PaletteView>, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::dialog::DialogEntrance;
    let body = view.clone();
    // At once, no motion: it's opened from the keyboard many times a day.
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(640.))
            .margin_top(px(72.))
            .p_0()
            .close_button(false)
            .entrance(DialogEntrance::None)
            .child(body.clone())
    });
    let state = view.read(cx).state.clone();
    state.update(cx, |s, cx| s.focus(window, cx));
}

/// Whether `app` has a palette open.
pub fn is_open(app: &FlotillaApp) -> bool {
    app.palette.as_ref().is_some_and(|p| p.upgrade().is_some())
}

#[cfg(test)]
mod tests {
    use super::{AppCommand, Entry, HitKind, Lists, Pick, SearchHit, ThreadRow, search};
    use gpui_kit::assets::IconName;

    const NOW: i64 = 1_793_000_000_000;

    fn hit(thread: &str, seq: i64) -> SearchHit {
        SearchHit {
            thread: thread.into(),
            title: thread.into(),
            kind: HitKind::Text,
            seq: Some(seq),
            snippet: "…".into(),
            marks: vec![],
            at: None,
            from: None,
            hits: 1,
        }
    }

    fn message(thread: &str, seq: i64) -> Pick {
        Pick::Message {
            thread: thread.into(),
            seq,
        }
    }

    fn row(id: &str, title: &str, at: i64, archived: bool) -> ThreadRow {
        ThreadRow {
            id: id.into(),
            title: title.into(),
            project: None,
            branch: None,
            pr: None,
            archived,
            at,
            created: at,
            prompted: None,
            opens: None,
        }
    }

    fn entry(label: &str, keywords: &'static str, pick: AppCommand) -> Entry {
        Entry {
            label: label.into(),
            keywords,
            icon: IconName::Settings,
            key: None,
            pick: Pick::App(pick),
        }
    }

    fn lists() -> Lists {
        let commands = vec![
            entry("New thread", "start create", AppCommand::NewThread),
            entry("Settings…", "preferences options", AppCommand::Settings),
        ];
        let threads = vec![
            row("t7", "Thread 7: palette", NOW - 70, false),
            row("t12", "Thread 12: palette", NOW - 120, false),
            row("s", "Slugify function", NOW - 10, false),
        ];
        Lists::new(None, commands, threads, NOW)
    }

    /// Each group's heading and what its rows pick.
    fn shown(l: &Lists) -> Vec<(String, Vec<Option<Pick>>)> {
        l.rows()
            .into_iter()
            .map(|(h, rows)| (h.to_string(), rows.into_iter().map(|r| r.pick).collect()))
            .collect()
    }

    fn group(l: &Lists, heading: &str) -> Vec<Option<Pick>> {
        shown(l)
            .into_iter()
            .find(|(h, _)| h == heading)
            .map(|(_, rows)| rows)
            .unwrap_or_default()
    }

    #[test]
    fn while_a_search_is_out_the_last_answer_isnt_shown_and_enter_waits_for_this_one() {
        let mut l = lists();
        assert_eq!(
            l.typed("zanzibar"),
            Some(("zanzibar".into(), search::LIMIT))
        );
        assert_eq!(
            l.answered("zanzibar", Ok(vec![hit("t12", 1990), hit("t7", 40)])),
            Some(None)
        );
        assert_eq!(
            shown(&l),
            [(
                "In messages".into(),
                vec![Some(message("t12", 1990)), Some(message("t7", 40))]
            )]
        );
        // Another query: what "zanzibar" found isn't listed, nor can it be picked.
        l.typed("quokka");
        assert_eq!(
            shown(&l),
            [("In messages".into(), vec![None])],
            "only \"Searching messages…\""
        );
        assert_eq!(l.target(&l.rows()), None);
        // Enter now waits; a late answer for the old query changes nothing.
        l.enter = true;
        assert_eq!(l.answered("zanzibar", Ok(vec![hit("t12", 1990)])), None);
        assert_eq!(
            l.answered("quokka", Ok(vec![hit("t7", 1000)])),
            Some(Some(message("t7", 1000))),
            "picks the first row once it's here"
        );
        // Nothing found: Enter picks nothing.
        l.typed("nothing at all");
        l.enter = true;
        assert_eq!(l.answered("nothing at all", Ok(vec![])), Some(None));
    }

    #[test]
    fn the_highlight_stays_on_its_row_by_what_it_picks() {
        let mut l = lists();
        l.typed("zanzibar marker");
        l.answered("zanzibar marker", Ok(vec![hit("t7", 40), hit("t20", 1024)]));
        assert_eq!(
            l.target(&l.rows()),
            Some(message("t7", 40)),
            "the first, to start with"
        );
        l.chosen = Some(message("t20", 1024));
        // The rows change under it (more come, in another order): it stays on its row.
        l.hits = vec![hit("t20", 1024), hit("t3", 5), hit("t7", 40)];
        assert_eq!(l.target(&l.rows()), Some(message("t20", 1024)));
        // Its row gone: the first.
        l.hits = vec![hit("t3", 5), hit("t7", 40)];
        assert_eq!(l.target(&l.rows()), Some(message("t3", 5)));
    }

    #[test]
    fn nothing_is_highlighted_without_words_until_an_arrow_is_pressed() {
        let mut l = lists();
        assert!(!l.rows().is_empty());
        assert_eq!(l.target(&l.rows()), None, "⌘K then Enter does nothing");
        // A query with no words (a lone quote) is the same as none: the plain list.
        assert_eq!(l.typed("\""), None);
        assert_eq!(shown(&l), shown(&lists()));
        assert_eq!(l.target(&l.rows()), None);
        l.moved = true;
        assert_eq!(l.target(&l.rows()), Some(Pick::App(AppCommand::NewThread)));
        // Words: the first row at once.
        l.typed("set");
        assert_eq!(l.target(&l.rows()), Some(Pick::App(AppCommand::Settings)));
    }

    #[test]
    fn threads_rank_by_t3s_rules_whole_words_then_starts_then_inside_words() {
        let mut l = Lists::new(
            None,
            vec![],
            vec![
                row("a", "Fix oauth redirect", NOW, false),
                row("b", "Authentication cleanup", NOW - 9, false),
                row("c", "Share auth across worktrees", NOW - 99, false),
                row("d", "Old café notes", NOW - 5, true),
            ],
            NOW,
        );
        l.typed("auth");
        assert_eq!(
            group(&l, "Threads"),
            [
                Some(Pick::Open("c".into())),
                Some(Pick::Open("b".into())),
                Some(Pick::Open("a".into()))
            ]
        );
        l.typed("CAFE");
        assert_eq!(
            group(&l, "Threads"),
            [Some(Pick::Open("d".into()))],
            "archived ones are found"
        );
        // Nothing at a word start: one typo is forgiven.
        l.typed("cleanpu");
        assert_eq!(group(&l, "Threads"), [Some(Pick::Open("b".into()))]);
        // Three letters must start a word: "uth" isn't in "oauth".
        l.typed("uth");
        assert!(group(&l, "Threads").is_empty());
        // Nothing typed: the latest, not archived.
        l.typed("");
        assert_eq!(
            group(&l, "Threads"),
            [
                Some(Pick::Open("a".into())),
                Some(Pick::Open("b".into())),
                Some(Pick::Open("c".into()))
            ]
        );
    }

    #[test]
    fn a_thread_its_title_lists_isnt_listed_again_for_its_messages_and_each_comes_once() {
        let mut l = lists();
        l.typed("palette marker");
        l.answered("palette marker", Ok(vec![hit("t7", 40), hit("t9", 3)]));
        // "Thread 7: palette" matches only "palette": its title doesn't list it, so it stays.
        l.typed("palette");
        l.answered("palette", Ok(vec![hit("t7", 40), hit("t9", 3)]));
        assert_eq!(
            group(&l, "Threads"),
            [
                Some(Pick::Open("t7".into())),
                Some(Pick::Open("t12".into()))
            ]
        );
        assert_eq!(
            group(&l, "In messages"),
            [Some(message("t9", 3))],
            "t7 is listed by its title"
        );
        // At most ten threads by their messages.
        let many: Vec<SearchHit> = (0..15).map(|k| hit(&format!("m{k}"), k)).collect();
        l.typed("zanzibar");
        l.answered("zanzibar", Ok(many));
        assert_eq!(group(&l, "In messages").len(), search::MESSAGE_ROWS);
    }

    #[test]
    fn commands_found_only_by_their_keywords_come_after_the_threads() {
        let mut l = lists();
        l.typed("s");
        let groups: Vec<String> = shown(&l).into_iter().map(|(h, _)| h).collect();
        assert_eq!(
            groups,
            ["Commands", "Threads"],
            "Settings…, Slugify (a one-letter keyword start doesn't count)"
        );
        l.typed("start");
        l.answered("start", Ok(vec![]));
        assert_eq!(
            shown(&l).into_iter().map(|(h, _)| h).collect::<Vec<_>>(),
            ["Commands"],
            "no command says it: they're the commands"
        );
        // A thread named for the query lists above a command with it as a keyword.
        let mut l = Lists::new(
            None,
            vec![entry(
                "Restart agent session",
                "reload mcp",
                AppCommand::SyncNow,
            )],
            vec![row("m", "Trim MCP model lists", NOW, false)],
            NOW,
        );
        l.typed("mcp");
        l.answered("mcp", Ok(vec![]));
        assert_eq!(
            shown(&l).into_iter().map(|(h, _)| h).collect::<Vec<_>>(),
            ["Threads", "Commands"]
        );
        // A command named for the query stays first.
        let mut l = Lists::new(
            None,
            vec![entry("Reload MCP servers", "", AppCommand::SyncNow)],
            vec![row("m", "Trim MCP model lists", NOW, false)],
            NOW,
        );
        l.typed("mcp");
        l.answered("mcp", Ok(vec![]));
        assert_eq!(
            shown(&l).into_iter().map(|(h, _)| h).collect::<Vec<_>>(),
            ["Commands", "Threads"]
        );
    }

    #[test]
    fn commands_match_their_words_and_keywords() {
        let e = entry("Archive", "thread hide", AppCommand::SyncNow);
        let m = |q: &str| {
            e.matched(&search::fold_query(q), false)
                .map(|m| !m.ranges.is_empty())
        };
        assert_eq!(m("arch"), Some(true));
        assert_eq!(m("HIDE thread"), Some(false), "only with its keywords");
        assert_eq!(m("archive hide"), Some(true));
        assert_eq!(m("delete"), None);
        assert_eq!(m("chive"), None, "commands never match inside a word");
    }

    #[test]
    fn a_pick_carries_the_query_it_was_found_by() {
        let mut l = lists();
        assert_eq!(l.pick_query(), None);
        l.typed("  palette ");
        assert_eq!(l.pick_query().as_deref(), Some("palette"));
        l.typed(">arch");
        assert_eq!(l.pick_query(), None, "commands only");
        // A pick for this query comes first among word matches, typed again.
        let mut picked = row("t12", "Thread 12: palette", NOW - 120, false);
        picked.opens = Some(search::record_open(None, NOW - 1000, Some("palette")));
        let mut l = Lists::new(
            None,
            vec![],
            vec![row("t7", "Thread 7: palette", NOW - 70, false), picked],
            NOW,
        );
        l.typed("palette");
        assert_eq!(
            group(&l, "Threads").first(),
            Some(&Some(Pick::Open("t12".into())))
        );
    }

    #[test]
    fn a_thread_matched_by_its_pull_request_or_branch_says_so() {
        let mut t = row("p", "Trim model lists", NOW, false);
        t.pr = Some((
            22889,
            "Smaller MCP payloads".into(),
            "https://github.com/acme/app/pull/22889".into(),
        ));
        t.branch = Some("flotilla/cleanup-payloads".into());
        let mut l = Lists::new(None, vec![], vec![t], NOW);
        l.typed("#22889");
        let rows = l.rows();
        let hint = rows
            .iter()
            .flat_map(|(_, r)| r)
            .find_map(|r| match &r.shows {
                super::Shows::Thread(_, _, hint) => hint.clone(),
                _ => None,
            });
        assert_eq!(
            hint.map(|h| (h.label, h.text)),
            Some(("PR", "#22889 Smaller MCP payloads".into()))
        );
    }
}
