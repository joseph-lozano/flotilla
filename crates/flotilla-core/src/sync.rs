//! Keeping SQLite in step with boat.
//!
//! A forward cursor alone ends a turn with stale and ghost rows (events change in place,
//! partials are deleted, prompt ids change), so a poll re-reads everything after the last
//! *settled* event and reconciles it, and the cursor only moves once every turn has settled.

use crate::boat::{BoatClient, EventsPage, PromptRun, Sandbox};
use crate::error::{BoatError, Error, Result};
use crate::event::{RawEvent, encode_cursor, is_terminal_status};
use crate::lifecycle;
use crate::store::{Store, SyncState, Thread, ThreadState};
use crate::util::{now_iso, parse_iso};
use std::collections::{HashMap, HashSet};

const PAGE_LIMIT: u32 = 200;
const MAX_PAGES: usize = 500;
/// Settle a turn anyway once `done` has been true this long without a terminal prompt event.
const DONE_GRACE_SECONDS: i64 = 10;

/// The boat calls a poll makes. [`BoatClient`] is the real one; tests replay M0 captures.
pub trait PollApi {
    fn events_page(
        &self,
        sandbox_id: &str,
        conversation_id: &str,
        cursor: Option<&str>,
        limit: u32,
        if_none_match: Option<&str>,
    ) -> Result<EventsPage, BoatError>;
    fn prompt_status(&self, sandbox_id: &str, prompt_id: &str) -> Result<PromptRun, BoatError>;
    /// Runs when a turn this process saw running has settled and the VM is up.
    fn after_turn(&self, store: &Store, thread_id: &str) -> Result<()>;
}

impl PollApi for BoatClient {
    fn events_page(
        &self,
        sandbox_id: &str,
        conversation_id: &str,
        cursor: Option<&str>,
        limit: u32,
        if_none_match: Option<&str>,
    ) -> Result<EventsPage, BoatError> {
        BoatClient::events_page(
            self,
            sandbox_id,
            conversation_id,
            cursor,
            limit,
            if_none_match,
        )
    }

    fn prompt_status(&self, sandbox_id: &str, prompt_id: &str) -> Result<PromptRun, BoatError> {
        BoatClient::prompt_status(self, sandbox_id, prompt_id)
    }

    fn after_turn(&self, store: &Store, thread_id: &str) -> Result<()> {
        lifecycle::after_turn(self, store, thread_id)
    }
}

/// Per-thread state that lives in memory only.
#[derive(Debug, Default, Clone)]
pub struct LiveState {
    /// Streaming partials from the last fetch, for live display. Never written to `events`.
    pub partials: Vec<RawEvent>,
    /// Turns this process saw unfinished; their settling triggers the after-turn hook.
    seen_running: HashSet<String>,
}

#[derive(Debug, Default, Clone)]
pub struct PollOutcome {
    /// Stored events (and so messages) changed.
    pub changed: bool,
    /// The events page came back `304`.
    pub not_modified: bool,
    /// Turns that settled in this poll.
    pub settled_turns: Vec<String>,
    /// Some turn is still unsettled.
    pub active: bool,
    /// A turn this process saw running has just finished (not one first seen already over,
    /// as on a rebuild).
    pub finished_here: bool,
    /// Non-fatal problems (e.g. the after-turn TTL refresh failed).
    pub warnings: Vec<String>,
}

/// Thread state from the sandbox.
pub fn state_from_sandbox(sb: &Sandbox, turn_active: bool) -> ThreadState {
    if sb.data_expired_at.is_some() {
        return ThreadState::Expired;
    }
    match sb.state.as_str() {
        "archived" => ThreadState::Stopped,
        "archiving" => ThreadState::Stopping,
        "error" | "cancelled" => ThreadState::Error,
        "ready" | "idle" if turn_active => ThreadState::Running,
        "ready" | "idle" => ThreadState::Idle,
        "running" => ThreadState::Running,
        _ => ThreadState::Starting,
    }
}

/// Copy what boat says about a sandbox onto its thread row (state, title, deadlines).
pub fn apply_sandbox(thread: &mut Thread, sb: &Sandbox, turn_active: bool) {
    thread.sandbox_id = Some(sb.id.clone());
    thread.state = state_from_sandbox(sb, turn_active);
    if let Some(n) = sb.name.as_deref().filter(|n| !n.is_empty()) {
        thread.title = crate::flavor::title_of(n).to_string();
    }
    if sb.environment.is_some() {
        thread.environment_name = sb.environment.clone();
    }
    thread.archive_after = sb.archive_after.clone();
    if sb.stopped_at.is_some() {
        thread.last_stopped_at = sb.stopped_at.clone();
    }
    thread.data_expired_at = sb.data_expired_at.clone();
}

/// Fetch every event after the stored cursor (all pages), or `None` on `304`.
/// Returns the events, and the first page's etag when the whole fetch was one page.
fn fetch(
    boat: &(impl PollApi + ?Sized),
    sandbox_id: &str,
    conversation_id: &str,
    ss: &SyncState,
) -> Result<Option<(Vec<RawEvent>, Option<String>)>> {
    let mut by_key: HashMap<String, RawEvent> = HashMap::new();
    let mut cursor = ss.cursor.clone();
    let mut etag = None;
    for page_no in 0..MAX_PAGES {
        // The etag only covers the first page, so it's only sent there.
        let inm = if page_no == 0 {
            ss.etag.as_deref()
        } else {
            None
        };
        match boat.events_page(
            sandbox_id,
            conversation_id,
            cursor.as_deref(),
            PAGE_LIMIT,
            inm,
        )? {
            EventsPage::NotModified => return Ok(None),
            EventsPage::Page {
                events,
                has_more,
                etag: tag,
                next_cursor,
            } => {
                if page_no == 0 {
                    etag = tag;
                }
                // Page on from the last event that can't vanish before the next request:
                // boat silently restarts from the first event for a cursor naming a gone id.
                let next = events
                    .iter()
                    .rev()
                    .find(|e| !e.is_partial() && !e.is_queued_placeholder())
                    .map(|e| encode_cursor(e.timestamp, &e.id))
                    // A page of nothing but partials: boat's own cursor is all there is.
                    .or(next_cursor);
                for e in events {
                    by_key.insert(
                        if e.is_partial() {
                            format!("partial:{}", e.id)
                        } else {
                            e.key()
                        },
                        e,
                    );
                }
                match next {
                    Some(n) if has_more => {
                        // A multi-page fetch: a later page can change without the first page's
                        // etag changing, so don't keep it.
                        etag = None;
                        cursor = Some(n);
                    }
                    _ => {
                        let mut all: Vec<RawEvent> = by_key.into_values().collect();
                        all.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
                        return Ok(Some((all, etag)));
                    }
                }
            }
        }
    }
    Err(Error::Invalid(
        "Couldn't load all of this thread's history".into(),
    ))
}

/// One poll of one thread: fetch, apply, rebuild messages, check turns, maybe settle.
pub fn poll_thread(
    boat: &(impl PollApi + ?Sized),
    store: &Store,
    thread_id: &str,
    live: &mut LiveState,
) -> Result<PollOutcome> {
    let mut out = PollOutcome::default();
    let Some(mut thread) = store.thread(thread_id)? else {
        return Err(Error::NotFound(format!("thread {thread_id}")));
    };
    let (Some(sandbox_id), Some(conversation_id)) =
        (thread.sandbox_id.clone(), thread.conversation_id.clone())
    else {
        out.active = !store.unsettled_turns(thread_id)?.is_empty();
        return Ok(out);
    };
    let mut ss = store.sync_state(thread_id)?;

    // 1–2. Fetch and apply.
    let mut new_terminal = Vec::new();
    match fetch(boat, &sandbox_id, &conversation_id, &ss)? {
        None => out.not_modified = true,
        Some((events, etag)) => {
            live.partials = events.iter().filter(|e| e.is_partial()).cloned().collect();
            let applied = store.apply_events(thread_id, &events, true)?;
            out.changed = applied.changed;
            new_terminal = applied.new_terminal_turns;
            ss.etag = etag;
        }
    }

    // 4. Check turn status.
    let stopped = matches!(thread.state, ThreadState::Stopped | ThreadState::Expired);
    let mut statuses_changed = false;
    for mut turn in store.unsettled_turns(thread_id)? {
        let prompt_event = store.event(thread_id, &turn.prompt_id)?;
        let event_status = prompt_event
            .as_ref()
            .and_then(|e| e.prompt_status())
            .map(str::to_string);
        let terminal = event_status.as_deref().is_some_and(is_terminal_status);
        if let Some(s) = &event_status {
            turn.status = s.clone();
        }
        if terminal && new_terminal.contains(&turn.prompt_id) {
            // First seen already over (e.g. on a rebuild): no need to ask boat.
            turn.done = true;
            turn.settled = true;
            turn.done_at.get_or_insert_with(now_iso);
        } else {
            if !terminal {
                live.seen_running.insert(turn.prompt_id.clone());
            }
            match boat.prompt_status(&sandbox_id, &turn.prompt_id) {
                Ok(run) => {
                    if event_status.is_none() && turn.status != run.status {
                        turn.status = run.status.clone();
                        statuses_changed = true;
                    }
                    if run.done && !turn.done {
                        turn.done = true;
                        turn.done_at = Some(now_iso());
                    }
                }
                // A stopped VM runs nothing, so whatever boat recorded is final.
                Err(_) if stopped => {
                    turn.done = true;
                    turn.done_at.get_or_insert_with(now_iso);
                }
                Err(e) if e.status() == Some(404) && terminal => {
                    turn.done = true;
                    turn.done_at.get_or_insert_with(now_iso);
                }
                // Leave the turn unsettled and try again next poll.
                Err(e) => out
                    .warnings
                    .push(format!("prompt {} status: {e}", turn.prompt_id)),
            }
            let done_long = turn
                .done_at
                .as_deref()
                .and_then(parse_iso)
                .is_some_and(|t| (chrono::Utc::now() - t).num_seconds() > DONE_GRACE_SECONDS);
            if turn.done && (terminal || done_long || stopped) {
                turn.settled = true;
            }
        }
        if turn.settled {
            out.settled_turns.push(turn.prompt_id.clone());
        }
        store.update_turn(&turn)?;
    }

    // 3. Rebuild messages if anything changed.
    if out.changed || statuses_changed {
        store.rebuild_messages(thread_id)?;
    }

    // 5. With no turn unsettled, settle the events and move the cursor.
    let unsettled = store.unsettled_turns(thread_id)?;
    out.active = !unsettled.is_empty();
    if unsettled.is_empty() {
        store.settle_all_events(thread_id)?;
        let cursor = store
            .cursor_event(thread_id)?
            .map(|(ts, id)| encode_cursor(ts, &id));
        if cursor != ss.cursor {
            ss.cursor = cursor;
            ss.etag = None;
        }
        live.partials.clear();
        // The row was read before the fetch: write only the state, not the rest of it.
        if thread.state == ThreadState::Running {
            thread.state = ThreadState::Idle;
            store.set_thread_state(thread_id, ThreadState::Idle)?;
        }
        let ran_here = out
            .settled_turns
            .iter()
            .any(|t| live.seen_running.remove(t));
        out.finished_here = ran_here;
        let up = matches!(
            thread.state,
            ThreadState::Idle | ThreadState::Running | ThreadState::Starting
        );
        if ran_here
            && up
            && let Err(e) = boat.after_turn(store, thread_id)
        {
            out.warnings
                .push(format!("after-turn TTL refresh failed: {e}"));
        }
    } else if thread.state == ThreadState::Idle {
        store.set_thread_state(thread_id, ThreadState::Running)?;
    }
    ss.synced_at = Some(now_iso());
    store.set_sync_state(thread_id, &ss)?;
    Ok(out)
}

#[derive(Debug, Default, Clone)]
pub struct FullSyncReport {
    pub environments: usize,
    pub sandboxes: usize,
    pub created: Vec<String>,
    pub expired: Vec<String>,
    pub polled: usize,
    /// Threads whose turn this process saw running and that this sync found finished.
    pub finished: Vec<String>,
    pub errors: Vec<String>,
    /// Threads whose history on boat this sync read in full: their events, or boat having no
    /// conversation for their VM. Settling what a quit left behind needs it.
    pub synced: HashSet<String>,
    /// The thread VMs listed that are another Flotilla's, said as the setup checks say it.
    pub others: Option<String>,
}

/// On launch, every few minutes, and for a rebuild: projects, threads from labelled
/// sandboxes, conversation ids, then every thread's events.
pub fn full_sync(
    boat: &BoatClient,
    store: &Store,
    lives: &mut HashMap<String, LiveState>,
) -> Result<FullSyncReport> {
    let mut report = FullSyncReport::default();

    // 1. Environments (id, name, is_default only).
    let envs = boat.environments()?;
    store.replace_environments(&envs)?;
    report.environments = envs.len();

    // 2. Threads from sandboxes labelled flotilla=1 (flotilla=dev in a dev build; stopped
    // ones included) that are this Flotilla's: another on this boat account syncs, and acts
    // on, its own.
    let owner = crate::owner::Owner::load(store)?;
    // Each thread's VM before the list is read: one a thread gets meanwhile isn't in it.
    let vm_before: HashMap<String, String> = store
        .threads()?
        .into_iter()
        .filter_map(|t| Some((t.id, t.sandbox_id?)))
        .collect();
    let (sandboxes, others): (Vec<_>, Vec<_>) = boat
        .list_sandboxes(&[("flotilla", crate::flavor::vm_label())])?
        .into_iter()
        .partition(|sb| owner.mine(sb));
    report.sandboxes = sandboxes.len();
    report.others = owner.others(&others);
    let mut listed = HashSet::new();
    for sb in &sandboxes {
        listed.insert(sb.id.clone());
        let Some(thread_id) = sb.label("flotilla.thread") else {
            continue;
        };
        let active = !store.unsettled_turns(&thread_id)?.is_empty();
        match store.thread(&thread_id)? {
            Some(mut t) => {
                apply_sandbox(&mut t, sb, active);
                store.update_thread(&t)?;
                crate::lifecycle::seen_vm(store, &t.id, sb, t.state);
                // The auto-stop as the thread has it: a Keep running change or a start's setup
                // stop that a quit cut short never got to boat. Only on a VM that's up (a resume
                // sets it anyway) and not while it's starting (setup has its own).
                if matches!(t.state, ThreadState::Idle | ThreadState::Running)
                    && store.start_stages(&t.id)?.is_none()
                {
                    let want = crate::lifecycle::idle_ttl(store, &t);
                    let differs = match want {
                        crate::boat::Ttl::Never => sb.ttl_seconds.is_some(),
                        crate::boat::Ttl::Seconds(n) => sb.ttl_seconds != Some(i64::from(n)),
                        crate::boat::Ttl::Omit => false,
                    };
                    if differs {
                        let _ = boat.update_sandbox(&sb.id, want, None);
                    }
                }
            }
            None => match thread_from_sandbox(store, &thread_id, sb) {
                Ok(t) => {
                    store.insert_thread(&t)?;
                    store.mark_viewed(&t.id)?;
                    if let Some(parent) = sb.label("flotilla.parent") {
                        crate::orchestrate::set_parent(store, &t.id, &parent)?;
                    }
                    report.created.push(t.id);
                }
                Err(e) => report.errors.push(format!("{}: {e}", sb.id)),
            },
        }
    }

    // 3. Local threads whose sandbox is gone: deleted on boat, now read-only. Only a VM the
    // thread had before the list was read: one made since can't be in it.
    for mut t in store.threads()? {
        if let Some(sb) = &t.sandbox_id
            && vm_before.get(&t.id) == Some(sb)
            && !listed.contains(sb)
            && t.state != ThreadState::Expired
        {
            t.state = ThreadState::Expired;
            store.update_thread(&t)?;
            report.expired.push(t.id.clone());
        }
    }

    // 4. Conversation ids (exactly one conversation per Flotilla sandbox).
    for t in store.threads()? {
        if t.conversation_id.is_some() || t.state == ThreadState::Expired {
            continue;
        }
        let Some(sb) = t.sandbox_id.clone() else {
            continue;
        };
        match boat.conversations(&sb) {
            Ok(mut convs) => {
                convs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
                match convs.into_iter().next() {
                    Some(c) => store.set_conversation_id(&t.id, &c.id)?,
                    // Boat has nothing of this thread yet: that's its whole history.
                    None if listed.contains(&sb) => {
                        report.synced.insert(t.id.clone());
                    }
                    None => {}
                }
            }
            Err(e) => report.errors.push(format!("{}: conversations: {e}", t.id)),
        }
    }

    // 5. Events for every thread.
    for t in store.threads()? {
        if t.conversation_id.is_none()
            || t.sandbox_id.is_none()
            || !listed.contains(t.sandbox_id.as_deref().unwrap_or(""))
        {
            continue;
        }
        let live = lives.entry(t.id.clone()).or_default();
        match poll_thread(boat, store, &t.id, live) {
            Ok(o) => {
                report.polled += 1;
                report.synced.insert(t.id.clone());
                if o.finished_here {
                    report.finished.push(t.id.clone());
                }
                report.errors.extend(o.warnings);
            }
            Err(e) => report.errors.push(format!("{}: events: {e}", t.id)),
        }
    }
    Ok(report)
}

/// A thread row rebuilt from a sandbox's labels and fields.
fn thread_from_sandbox(store: &Store, thread_id: &str, sb: &Sandbox) -> Result<Thread> {
    let need = |k: &str| {
        sb.label(k)
            .ok_or_else(|| Error::Invalid(format!("sandbox has no {k} label")))
    };
    let env = match &sb.environment {
        Some(name) => store.environment(name)?,
        None => None,
    };
    // A project thread: kept only if that project is still here.
    let project = sb
        .label("flotilla.project")
        .filter(|id| store.project(id).ok().flatten().is_some());
    let now = now_iso();
    let mut t = Thread {
        id: thread_id.to_string(),
        environment_id: env.map(|p| p.id),
        environment_name: sb.environment.clone(),
        title: sb
            .name
            .as_deref()
            .map(crate::flavor::title_of)
            .unwrap_or("(untitled)")
            .to_string(),
        sandbox_id: Some(sb.id.clone()),
        conversation_id: None,
        provider: need("flotilla.provider")?,
        model: need("flotilla.model")?,
        effort: sb.label("flotilla.effort"),
        fast: sb.label("flotilla.fast").as_deref() == Some("true"),
        state: ThreadState::Starting,
        // Keep running is the only way a Flotilla VM gets no auto-stop (`ttlSeconds: null`).
        keep_running: sb.ttl_seconds.is_none(),
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: sb.created_at.clone().unwrap_or_else(|| now.clone()),
        updated_at: now,
        branch: project.as_ref().and(sb.label("flotilla.branch")),
        repo_project_id: project,
        base_sha: None,
    };
    apply_sandbox(&mut t, sb, false);
    Ok(t)
}
