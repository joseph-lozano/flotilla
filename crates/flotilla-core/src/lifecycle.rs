//! Thread lifecycle: create, prompt, steer, interrupt, stop, resume, and the after-turn TTL.

use crate::boat::{BoatClient, CreateSandbox, Limits, PromptRequest, Sandbox, Ttl};
use crate::catalog::Catalog;
use crate::error::{Error, Result};
use crate::setup::sh_quote;
use crate::store::{Project, Store, Thread, ThreadState};
use crate::sync::{self, FullSyncReport, LiveState, PollOutcome};
use crate::util::{now_iso, truncate_chars};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// How long an idle VM stays up after its last turn, in minutes, unless "Keep running" is on
/// (then it never auto-stops). Kept on this Mac in `kv` under `IDLE_MINUTES_KEY`.
pub const DEFAULT_IDLE_MINUTES: u32 = 10;
pub const IDLE_MINUTES_KEY: &str = "idle_stop_minutes";
pub use flotilla_model::lifecycle::{
    DISK_RETENTION_DAYS, IDLE_MINUTE_CHOICES, LOCAL_PREFIX, UNSENT_PREFIX, clean_branch_prefix,
    days_left, default_title,
};
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_TIMEOUT: Duration = Duration::from_secs(90);
const NAME_MAX_CHARS: usize = 120;

/// The longest title: boat's limit on VM names, less a dev build's mark on them.
fn title_max() -> usize {
    NAME_MAX_CHARS - crate::flavor::vm_name("").chars().count()
}

pub fn default_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::flavor::app_name())
        .join("flotilla.db")
}

/// Whether archiving may wake a stopped VM to run `.agents/archive` (a VM start spent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waking {
    /// The user archived or deleted it.
    Yes,
    /// Only if boat says it has starts to spare (`Limits::spares_starts`), checked as it wakes.
    IfStarts,
    No,
}

impl Waking {
    /// Whether a VM may be woken now; boat's limits are asked for only if that depends on them.
    pub fn allows(self, limits: impl FnOnce() -> Option<crate::boat::Limits>) -> bool {
        match self {
            Waking::Yes => true,
            Waking::IfStarts => limits().is_some_and(|l| l.spares_starts()),
            Waking::No => false,
        }
    }
}

/// The idle stop from Settings, in minutes.
pub fn idle_minutes(store: &Store) -> u32 {
    store
        .kv_get(IDLE_MINUTES_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .filter(|m| *m > 0)
        .unwrap_or(DEFAULT_IDLE_MINUTES)
}

/// The auto-stop to set on a thread's VM: never with "Keep running", else the idle stop.
pub fn idle_ttl(store: &Store, thread: &Thread) -> Ttl {
    if thread.keep_running {
        Ttl::Never
    } else {
        Ttl::Seconds(idle_minutes(store) * 60)
    }
}

/// What goes before the names of new branches (Settings → Projects), kept in `kv`.
pub const BRANCH_PREFIX_KEY: &str = "branch_prefix";

/// The branch prefix from Settings, or none.
pub fn branch_prefix(store: &Store) -> String {
    store
        .kv_get(BRANCH_PREFIX_KEY)
        .ok()
        .flatten()
        .map(|p| clean_branch_prefix(&p))
        .unwrap_or_default()
}

/// A branch name for a project thread from its first prompt's first words ("Add retry to
/// the webhook sender" → `add-retry-to-the-webhook-sender`), after `prefix`, unique among
/// `taken`. Used until (or unless) the preview model names it (`model_branch`).
/// `tag`: one of several threads started together from one prompt (its model's name), after
/// the words, so their names differ by more than a number.
pub fn branch_name(prompt: &str, prefix: &str, tag: Option<&str>, taken: &[String]) -> String {
    unique_branch(&branch_base(prompt, prefix, tag), "", taken)
}

/// `branch_name` before it's made unique.
fn branch_base(prompt: &str, prefix: &str, tag: Option<&str>) -> String {
    format!(
        "{prefix}{}",
        tagged(branch_slug(prompt).unwrap_or_else(|| "thread".into()), tag)
    )
}

/// `slug` with `tag` after it, cleaned for a branch (lowercase letters and digits joined by
/// hyphens, at most 24 characters); `slug` alone when the tag has nothing usable.
fn tagged(slug: String, tag: Option<&str>) -> String {
    let mut t = String::new();
    for word in tag
        .unwrap_or_default()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if t.len() + word.len() + 1 > 24 {
            break;
        }
        if !t.is_empty() {
            t.push('-');
        }
        t.push_str(&word.to_ascii_lowercase());
    }
    if t.is_empty() || slug.ends_with(&format!("-{t}")) {
        slug
    } else {
        format!("{slug}-{t}")
    }
}

/// Up to six lowercase words of `text` joined by hyphens, at most 40 characters; none if it
/// has no letters or digits.
fn branch_slug(text: &str) -> Option<String> {
    let mut base = String::new();
    for word in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
    {
        if base.len() + word.len() + 1 > 40 {
            break;
        }
        if !base.is_empty() {
            base.push('-');
        }
        base.push_str(&word.to_ascii_lowercase());
    }
    (!base.is_empty()).then_some(base)
}

/// What the preview model answered for a branch name, made usable: its first line, without a
/// `type/` prefix it added anyway, cleaned like a prompt's first words. None if nothing's left
/// or it rambled (more than six words).
pub fn model_branch(answer: &str) -> Option<String> {
    let line = answer.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_matches(|c| c == '`' || c == '"' || c == '\'');
    let line = line.rsplit('/').next().unwrap_or(line);
    let words = line
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .count();
    if words > 6 {
        return None;
    }
    branch_slug(line)
}

/// `prefix` + `base`, with `-2`, `-3`… if another thread has it.
fn unique_branch(base: &str, prefix: &str, taken: &[String]) -> String {
    let base = format!("{prefix}{base}");
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|i| format!("{base}-{i}"))
        .find(|b| !taken.contains(b))
        .expect("a free name")
}

pub use flotilla_proto::{NewThread, OnBranch, Sent, StartStages};

/// The local half of starting a thread: check the settings against `catalog` and the
/// environment (or project) against those already known, then write the thread (still
/// `creating`, no VM yet) and its first prompt as a placeholder. No network, so the app can
/// show the thread at once; `Core::launch_thread` does the rest. Returns the thread and the
/// placeholder's id.
pub fn begin_thread(
    store: &Store,
    catalog: &crate::catalog::Catalog,
    req: &NewThread,
) -> Result<(Thread, String)> {
    let settings = catalog
        .validate(&req.provider, &req.model, req.effort.as_deref(), req.fast)
        .map_err(Error::Invalid)?;
    if req.prompt.trim().is_empty() {
        return Err(Error::Invalid("the first prompt is empty".into()));
    }
    if let Some(why) = crate::attach::over_limit(&req.attachments) {
        return Err(Error::Invalid(why));
    }
    let project = match &req.project_id {
        Some(id) => Some(
            store
                .project(id)?
                .ok_or_else(|| Error::NotFound("that project was removed".into()))?,
        ),
        None => None,
    };
    let env_name = project
        .as_ref()
        .map(|p| p.environment_name.clone())
        .unwrap_or_else(|| req.environment.clone());
    let env = store
        .environment(&env_name)?
        .ok_or_else(|| Error::NotFound(format!("no boat environment named {env_name:?}")))?;
    let branch = match (&project, &req.on_branch) {
        // As is: it's on GitHub already, maybe in another thread too.
        (Some(_), Some(b)) => Some(b.name.clone()),
        (Some(p), None) => Some(branch_name(
            &req.prompt,
            &branch_prefix(store),
            req.branch_tag.as_deref(),
            &store.project_branches(&p.id)?,
        )),
        (None, Some(_)) => {
            return Err(Error::Invalid(
                "only a project thread can start on a branch".into(),
            ));
        }
        (None, None) => None,
    };
    let id = match req.id.as_deref().map(uuid::Uuid::parse_str) {
        Some(Ok(u)) => u.to_string(),
        Some(Err(_)) => return Err(Error::Invalid("a thread id must be a UUID".into())),
        None => uuid::Uuid::new_v4().to_string(),
    };
    let title = truncate_chars(
        &req.title
            .clone()
            .unwrap_or_else(|| default_title(&req.prompt)),
        title_max(),
    );
    let now = now_iso();
    let thread = Thread {
        id: id.clone(),
        environment_id: Some(env.id.clone()),
        environment_name: Some(env.name.clone()),
        title,
        sandbox_id: None,
        conversation_id: None,
        provider: settings.provider.clone(),
        model: settings.model.clone(),
        effort: settings.effort.clone(),
        fast: settings.fast,
        state: ThreadState::Creating,
        keep_running: false,
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: now.clone(),
        updated_at: now,
        repo_project_id: project.as_ref().map(|p| p.id.clone()),
        branch,
        base_sha: project.and_then(|p| p.template_sha),
    };
    store.insert_thread(&thread)?;
    // Threads started at once (one prompt on several models) may have read the same names:
    // claim it again with the write lock held, so each keeps its own.
    let mut thread = thread;
    if let (Some(p), None, Some(_)) = (&thread.repo_project_id, &req.on_branch, &thread.branch) {
        let base = branch_base(
            &req.prompt,
            &branch_prefix(store),
            req.branch_tag.as_deref(),
        );
        thread.branch = Some(store.claim_branch(&id, p, |taken| unique_branch(&base, "", taken))?);
    }
    store.mark_viewed(&id)?;
    crate::orchestrate::seen_from_start(store, &id)?;
    if let Some(parent) = &req.parent {
        crate::orchestrate::set_parent(store, &id, parent)?;
    }
    if let Some(b) = &req.on_branch {
        store.kv_set(&on_branch_key(&id), &b.base)?;
    }
    if let Some(t) = &req.machine_type {
        store.kv_set(&machine_type_key(&id), t)?;
    }
    let local = format!("local-{}", uuid::Uuid::new_v4());
    store.insert_sent_prompt(&id, &local, "prompt", &req.prompt)?;
    store.set_sent_attachments(&id, &local, &req.attachments)?;
    store.rebuild_messages(&id)?;
    Ok((thread, local))
}

/// A message whose send failed after you'd moved to another thread, kept for that thread's
/// composer (`kv`).
pub fn unsent_key(thread_id: &str) -> String {
    format!("unsent:{thread_id}")
}

/// The files of such messages (JSON), kept with their text.
pub fn unsent_files_key(thread_id: &str) -> String {
    format!("unsent_files:{thread_id}")
}

/// The user stopped the thread's VM (Stop, Archive, `flotilla stop`) and hasn't started it
/// again since: Flotilla doesn't start it to say something of its own (`kv`).
/// `stopping <stopped_at>` (boat's last stop before this one, `-` for none) while the stop
/// goes through; once boat has it stopped, `stopped <updated_at> <stopped_at>` as boat had
/// them then.
pub fn stopped_by_user_key(thread_id: &str) -> String {
    format!("stopped_by_user:{thread_id}")
}

pub fn stopped_by_user(store: &Store, thread_id: &str) -> bool {
    store
        .kv_get(&stopped_by_user_key(thread_id))
        .ok()
        .flatten()
        .is_some()
}

/// What boat says of the thread's VM now (`state`, from `sb`), for the user's stop: a stop
/// seen through is noted with boat's times for it; the VM up again later than that (not a
/// reading from before the stop) was started somewhere else, boat's CLI or dashboard, and is
/// no longer stopped by the user.
pub fn seen_vm(store: &Store, thread_id: &str, sb: &crate::boat::Sandbox, state: ThreadState) {
    let key = stopped_by_user_key(thread_id);
    let Some(mark) = store.kv_get(&key).ok().flatten() else {
        return;
    };
    let (updated, stopped) = (
        sb.updated_at.clone().unwrap_or_default(),
        sb.stopped_at.clone().unwrap_or_default(),
    );
    let seen = mark
        .strip_prefix("stopped ")
        .and_then(|r| r.split_once(' '))
        .map(|(u, s)| (u.to_string(), s.to_string()));
    let later = |now: &str, then: &str| !now.is_empty() && now > then;
    // Up, having stopped since the user asked (a later stop than boat had then): started
    // again, without Flotilla seeing it stopped.
    if let Some(before) = mark.strip_prefix("stopping ")
        && matches!(
            state,
            ThreadState::Idle | ThreadState::Running | ThreadState::Starting
        )
        && (before == "-" && !stopped.is_empty() || later(&stopped, before) && before != "-")
    {
        let _ = store.kv_delete(&key);
        return;
    }
    match (state, &seen) {
        // Its stop is through (or a later one is).
        (ThreadState::Stopped, None) => {
            let _ = store.kv_set(&key, &format!("stopped {updated} {stopped}"));
        }
        (ThreadState::Stopped, Some((u, s))) if later(&updated, u) || later(&stopped, s) => {
            let _ = store.kv_set(&key, &format!("stopped {updated} {stopped}"));
        }
        // Up, as boat had it after the stop: started again somewhere else.
        (ThreadState::Idle | ThreadState::Running | ThreadState::Starting, Some((u, s)))
            if later(&updated, u) || (!stopped.is_empty() && stopped >= *s && !s.is_empty()) =>
        {
            let _ = store.kv_delete(&key);
        }
        _ => {}
    }
}

fn not_now() -> Error {
    Error::NotNow("the thread is archived, or its VM stopped".into())
}

/// Whether a message the user didn't send may go to `thread_id` now, checked before each step
/// of its send: not while the user has the thread archived or its VM stopped
/// (`orchestrate::may_wake`), nor while its agent waits on the user's answer to a question,
/// which the message would pass.
fn may_go(store: &Store, thread_id: &str) -> Result<()> {
    if !crate::orchestrate::may_wake(store, thread_id) {
        Err(not_now())
    } else if crate::asks::waiting(store, thread_id) {
        Err(Error::NotNow(
            "a question waits for the user's answer".into(),
        ))
    } else {
        Ok(())
    }
}

/// What `Core::reconcile_after_quit` settled.
#[derive(Debug, Default)]
pub struct Reconciled {
    /// Threads with a message that was still sending when Flotilla quit and that boat never
    /// got: it stays in place, marked "not sent", with Send again and Discard.
    pub not_sent: Vec<String>,
    /// Threads whose start a quit cut short and that boat hadn't got the first prompt of:
    /// they get Try again, the prompt kept.
    pub failed_starts: Vec<String>,
    /// Every thread whose history was read and checked (settled or found fine).
    pub looked_at: Vec<String>,
}

fn is_placeholder(id: &str) -> bool {
    id.starts_with(LOCAL_PREFIX) || id.starts_with(UNSENT_PREFIX)
}

/// How far boat's clock may run behind this Mac's when matching a message to its event.
const CLOCK_SKEW_MS: i64 = 10_000;

/// Pair each of `stale` (placeholders, oldest first) with the prompt or steer event boat made
/// of it, one to one: same kind, exactly its text (for a prompt boat rewrote after a steer,
/// the instruction inside boat's own wrapper, checked as such), no earlier than it was sent,
/// and not an event another send already owns. Returns (placeholder id, event id).
fn match_sent(
    sent: &[crate::store::SentPrompt],
    stale: &[&crate::store::SentPrompt],
    events: &[crate::event::RawEvent],
) -> Vec<(String, String)> {
    let owned: std::collections::HashSet<&str> = sent
        .iter()
        .filter(|s| !is_placeholder(&s.id))
        .map(|s| s.id.as_str())
        .collect();
    let mut candidates: Vec<(&str, &str, i64, String)> = events
        .iter()
        .filter(|e| !e.is_partial() && matches!(e.kind.as_str(), "prompt" | "steer"))
        .filter_map(|e| {
            let key = e.task_id.as_deref().unwrap_or(e.id.as_str());
            let field = if e.kind == "prompt" {
                "prompt"
            } else {
                "message"
            };
            let text = e.data.get(field).and_then(serde_json::Value::as_str)?;
            (!owned.contains(key))
                .then(|| (e.kind.as_str(), key, e.timestamp, text.trim().to_string()))
        })
        .collect();
    candidates.sort_by_key(|c| c.2);
    let mut taken: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut stale: Vec<&&crate::store::SentPrompt> = stale.iter().collect();
    stale.sort_by(|a, b| a.sent_at.cmp(&b.sent_at));
    for s in stale {
        let at = crate::util::iso_to_ms(&s.sent_at).unwrap_or(0);
        let text = s.text.trim();
        let hit = candidates.iter().find(|(kind, key, ts, t)| {
            *kind == s.kind
                && !taken.contains(key)
                && *ts >= at - CLOCK_SKEW_MS
                && (t == text
                    || (*kind == "prompt"
                        && crate::normalize::steer_wrapper_instruction(t) == Some(text)))
        });
        if let Some((_, key, _, _)) = hit {
            taken.insert(key);
            out.push((s.id.clone(), key.to_string()));
        }
    }
    out
}

/// Why a thread's first prompt didn't go, for its Try again card: boat's own words when it
/// refused (without the status, code and request id), and what to do about a model the
/// account can't run.
pub fn first_prompt_refused(e: &Error) -> String {
    let msg = e.plain();
    if msg == SCHEDULED_ARCHIVED {
        ARCHIVED_AS_IT_STARTED.into()
    } else if msg == START_STOPPED {
        STOPPED_AS_IT_STARTED.into()
    } else if e.is("model_not_available") && !msg.to_lowercase().contains("dashboard") {
        format!(
            "{}. Add its key in boat's dashboard, or start a thread with another model.",
            msg.trim_end_matches('.')
        )
    } else {
        msg
    }
}

/// What to start a thread with again after its start failed before it had a VM: its stored
/// settings, project and title, and its unsent first prompt (with that placeholder's id).
pub fn retry_request(store: &Store, thread_id: &str) -> Result<(NewThread, String)> {
    let t = store
        .thread(thread_id)?
        .ok_or_else(|| Error::NotFound(format!("thread {thread_id}")))?;
    if t.sandbox_id.is_some() {
        return Err(Error::Invalid("this thread already has a VM".into()));
    }
    let first = store
        .sent_prompts(thread_id)?
        .into_iter()
        .find(|s| s.kind == "prompt")
        .ok_or_else(|| Error::Invalid("this thread has no prompt to send".into()))?;
    let req = NewThread {
        environment: t.environment_name.clone().unwrap_or_default(),
        project_id: t.repo_project_id.clone(),
        provider: t.provider.clone(),
        model: t.model.clone(),
        effort: t.effort.clone(),
        fast: t.fast,
        title: Some(t.title.clone()),
        machine_type: store
            .kv_get(&machine_type_key(thread_id))?
            .filter(|t| !t.is_empty()),
        prompt: first.text,
        attachments: first.attachments,
        wait_ready: false,
        id: None,
        parent: None,
        // Its branch is named already.
        branch_tag: None,
        on_branch: match (
            &t.branch,
            store
                .kv_get(&on_branch_key(thread_id))?
                .filter(|b| !b.is_empty()),
        ) {
            (Some(name), Some(base)) => Some(OnBranch {
                name: name.clone(),
                base,
            }),
            _ => None,
        },
    };
    Ok((req, first.id))
}

/// After a turn ends: push the auto-stop deadline to now + the idle TTL. boat only extends it
/// on its own if the deadline arrives *while* a prompt runs.
pub fn after_turn(boat: &BoatClient, store: &Store, thread_id: &str) -> Result<()> {
    let Some(t) = store.thread(thread_id)? else {
        return Ok(());
    };
    let Some(sb) = t.sandbox_id.clone() else {
        return Ok(());
    };
    let updated = boat.update_sandbox(&sb, idle_ttl(store, &t), None)?;
    store.set_archive_after(thread_id, updated.archive_after.as_deref())?;
    Ok(())
}

/// `kv`: the branch a thread started on an existing branch merges into (its start checks the
/// branch out instead of making one, and its header counts from this branch).
fn on_branch_key(thread_id: &str) -> String {
    format!("on_branch:{thread_id}")
}

/// The machine size a thread asked for, so Try again asks for it too.
fn machine_type_key(thread_id: &str) -> String {
    format!("machine_type:{thread_id}")
}

/// The branch a project thread's work merges into: its pull request's base if it started on
/// an existing branch, else the project's default branch.
pub fn base_branch(store: &Store, thread_id: &str, default_branch: &str) -> String {
    store
        .kv_get(&on_branch_key(thread_id))
        .ok()
        .flatten()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| default_branch.to_string())
}

/// Whether a project thread started on a branch that was already on GitHub.
fn on_existing_branch(store: &Store, thread_id: &str) -> Result<bool> {
    Ok(store
        .kv_get(&on_branch_key(thread_id))?
        .is_some_and(|b| !b.is_empty()))
}

/// The lines (for `set -e`) that put a thread's repo on `branch` as it is on GitHub, tracking
/// it, and set `head` to where it left `base`. `-C`: a retried start may have made the
/// branch already, with nothing on it yet.
pub fn checkout_existing(branch: &str, base: &str) -> String {
    format!(
        "git fetch -q origin {base}\n\
         git fetch -q origin {refspec} || {{ echo {missing} >&2; exit 1; }}\n\
         git switch -q -C {b} --track origin/{b}\n\
         head=$(git merge-base origin/{base} HEAD)\n",
        b = sh_quote(branch),
        base = sh_quote(base),
        refspec = sh_quote(&format!(
            "+refs/heads/{branch}:refs/remotes/origin/{branch}"
        )),
        missing = sh_quote(&format!("{branch} isn't on GitHub")),
    )
}

/// `kv`: the thread's VM is a fresh clone (no prepared VM to copy), so its setup runs as
/// `fresh`, scripts for a fresh clone included.
/// How long a git step is retried while a fresh fork's disk is being filled in.
const HYDRATE_WAIT: Duration = Duration::from_secs(30);

/// git's complaints when a forked VM's disk isn't filled in yet: the repo folder empty, or
/// still owned by root.
fn disk_filling(stderr: &str) -> bool {
    stderr.contains("dubious ownership") || stderr.contains("not a git repository")
}

fn fresh_vm_key(thread_id: &str) -> String {
    format!("fresh_vm:{thread_id}")
}

pub struct Core {
    pub boat: BoatClient,
    pub store: Store,
    pub live: HashMap<String, LiveState>,
    /// Called when a long action writes progress (a starting thread's stages), so a UI can
    /// redraw before the action ends.
    pub on_progress: Option<Box<dyn Fn() + Send>>,
    /// Running a scheduled prompt, which nobody is waiting on: a VM is started (made or
    /// resumed) only through the gate, while boat has starts to spare beyond its reserve, and
    /// an archived thread is never brought back.
    pub scheduled: Option<std::sync::Arc<StartGate>>,
    /// The thread's turn with the engine's other actions on it (archive, stop, a message), for
    /// a start that doesn't hold it throughout: taken while its VM is made and while it's woken
    /// for the first prompt and that prompt sent (not while the repo is prepared), so an
    /// archive lands before or after, never between a look at it and what it would stop.
    pub turns: Option<std::sync::Arc<std::sync::Mutex<()>>>,
    /// Asked just before a message the user didn't send starts a stopped VM (`ensure_up`):
    /// an error stops it there (a parent's follow-up past its tree's hand-offs).
    pub waking: Option<WakeCheck>,
}

/// What [`Core::waking`] asks.
pub type WakeCheck = Box<dyn Fn(&Store) -> Result<()> + Send>;

/// The starts scheduled prompts may spend: boat's starts left this hour, less those taken by
/// scheduled starts that boat may not have counted yet, must keep [`crate::boat::START_RESERVE`]
/// back. Boat is asked under the lock, so starts taken at once can't all read the same count.
#[derive(Debug, Default)]
pub struct StartGate {
    pending: std::sync::Mutex<i64>,
}

/// A start taken through the gate: counted as pending until it's dropped (once boat has made
/// or resumed the VM, or refused).
#[derive(Debug)]
pub struct StartTicket(std::sync::Arc<StartGate>);

impl Drop for StartTicket {
    fn drop(&mut self) {
        if let Ok(mut p) = self.0.pending.lock() {
            *p -= 1;
        }
    }
}

impl StartGate {
    /// One of the starts to spare, as boat says now (`limits`, asked under the lock), or none.
    pub fn take(
        self: &std::sync::Arc<Self>,
        limits: impl FnOnce() -> Option<crate::boat::Limits>,
    ) -> Option<StartTicket> {
        let mut pending = self.pending.lock().ok()?;
        let l = limits()?;
        let left = l.starts_left()?;
        if l.can_start == Some(false) || left - *pending < crate::boat::START_RESERVE {
            return None;
        }
        *pending += 1;
        Some(StartTicket(self.clone()))
    }
}

/// Why a scheduled prompt's send or start didn't happen (`Core::scheduled`), in words the
/// schedule's row shows.
pub const NO_STARTS: &str = "boat has no VM starts to spare this hour.";
/// Also any start that didn't happen because the thread was archived as it started.
pub const SCHEDULED_ARCHIVED: &str = "The thread is archived.";
/// The Try again card of a thread archived as it started (shown once it's back).
pub const ARCHIVED_AS_IT_STARTED: &str = "Archived before it started.";
/// A start that didn't happen because the user stopped the thread as it started.
pub const START_STOPPED: &str = "The thread is stopped.";
/// The Try again card of a thread stopped as it started.
pub const STOPPED_AS_IT_STARTED: &str = "Stopped before it started.";

/// Who a prompt is from (`prompt_with`), for what it may do to the thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum By {
    /// The user's: it brings the thread back from the archive, and ends their stop.
    User,
    /// A thread's first, from its start or Try again: archived as it started, it isn't sent.
    Start,
    /// Flotilla's own (`send_unasked`).
    Unasked,
}

impl By {
    fn of(unasked: bool) -> By {
        if unasked { By::Unasked } else { By::User }
    }
}
/// A turn started (or the thread is still starting) since the run was due: it waits again.
pub const SCHEDULED_BUSY: &str = "A turn is running.";

impl Core {
    pub fn open(db: &Path) -> Result<Core> {
        let key = crate::secrets::api_key()?;
        Ok(Core {
            boat: BoatClient::new(key)?,
            store: Store::open(db)?,
            live: HashMap::new(),
            on_progress: None,
            scheduled: None,
            turns: None,
            waking: None,
        })
    }

    pub fn new(boat: BoatClient, store: Store) -> Core {
        Core {
            boat,
            store,
            live: HashMap::new(),
            on_progress: None,
            scheduled: None,
            turns: None,
            waking: None,
        }
    }

    pub fn live(&self, thread_id: &str) -> Option<&LiveState> {
        self.live.get(thread_id)
    }

    // ---- reads that refresh caches ----

    /// The model catalog: fetched fresh when possible, else the last good copy from `kv`.
    pub fn catalog(&self) -> Result<Catalog> {
        match self.boat.provider_models() {
            Ok(v) => {
                let c = Catalog::from_value(&v).map_err(Error::Invalid)?;
                self.store.kv_set("catalog", &v.to_string())?;
                Ok(c)
            }
            Err(e) => match self.store.kv_get("catalog")? {
                Some(cached) => {
                    Catalog::from_value(&serde_json::from_str(&cached)?).map_err(Error::Invalid)
                }
                None => Err(e.into()),
            },
        }
    }

    pub fn limits(&self) -> Result<Limits> {
        let l = self.boat.limits()?;
        self.store.kv_set("limits", &l.raw.to_string())?;
        Ok(l)
    }

    pub fn sync_environments(&self) -> Result<()> {
        self.store.replace_environments(&self.boat.environments()?)
    }

    pub fn full_sync(&mut self) -> Result<FullSyncReport> {
        sync::full_sync(&self.boat, &self.store, &mut self.live)
    }

    pub fn poll(&mut self, thread_id: &str) -> Result<PollOutcome> {
        let live = self.live.entry(thread_id.to_string()).or_default();
        sync::poll_thread(&self.boat, &self.store, thread_id, live)
    }

    fn thread(&self, id: &str) -> Result<Thread> {
        self.store
            .thread(id)?
            .ok_or_else(|| Error::NotFound(format!("thread {id}")))
    }

    fn sandbox_id(t: &Thread) -> Result<String> {
        t.sandbox_id
            .clone()
            .ok_or_else(|| Error::Invalid("This thread has no VM yet".into()))
    }

    /// GET the sandbox and copy its state onto the thread.
    pub fn refresh(&self, thread_id: &str) -> Result<(Thread, Sandbox)> {
        let sb = self
            .boat
            .get_sandbox(&Self::sandbox_id(&self.thread(thread_id)?)?)?;
        // Read the row only now: another thread may have saved to it during the request
        // (the first prompt's conversation id, a rename).
        let mut t = self.thread(thread_id)?;
        let active = !self.store.unsettled_turns(thread_id)?.is_empty();
        sync::apply_sandbox(&mut t, &sb, active);
        self.store.update_thread(&t)?;
        seen_vm(&self.store, thread_id, &sb, t.state);
        Ok((t, sb))
    }

    fn mark_expired(&self, thread_id: &str) -> Result<Error> {
        let mut t = self.thread(thread_id)?;
        t.state = ThreadState::Expired;
        self.store.update_thread(&t)?;
        Ok(Error::Expired(thread_id.to_string()))
    }

    /// If the thread's sandbox no longer exists on boat (deleted from the dashboard or CLI),
    /// mark the thread expired, keeping its history, and return true.
    pub fn check_gone(&self, thread_id: &str) -> Result<bool> {
        let t = self.thread(thread_id)?;
        let Some(sb) = &t.sandbox_id else {
            return Ok(false);
        };
        match self.boat.get_sandbox(sb) {
            Err(e) if e.status() == Some(404) => {
                self.mark_expired(thread_id)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Poll GET until the machine is up.
    fn wait_up(&self, thread_id: &str, timeout: Duration) -> Result<Thread> {
        let started = Instant::now();
        loop {
            let (t, sb) = self.refresh(thread_id)?;
            if sb.is_up() {
                return Ok(t);
            }
            if matches!(sb.state.as_str(), "error" | "cancelled") {
                return Err(Error::Invalid(format!("The VM is {}", sb.state)));
            }
            // The user stopped it meanwhile: it won't come up by itself.
            if matches!(sb.state.as_str(), "archiving" | "archived")
                && stopped_by_user(&self.store, thread_id)
            {
                return Err(Error::Invalid(START_STOPPED.into()));
            }
            if started.elapsed() > timeout {
                return Err(Error::Timeout(format!(
                    "sandbox {} still {} after {}s",
                    sb.id,
                    sb.state,
                    timeout.as_secs()
                )));
            }
            sleep(Duration::from_secs(1));
        }
    }

    /// Make sure the VM can take a prompt: resume it if stopped, wait if it's starting.
    /// For a message the user didn't send (`By::Unasked`): checked again just before a resume
    /// (the user may have stopped it meanwhile), and the user's stop stays theirs. For a first
    /// or scheduled prompt: not resumed if the thread was archived meanwhile.
    fn ensure_up(&self, thread_id: &str, by: By) -> Result<Thread> {
        let unasked = by == By::Unasked;
        let (t, sb) = self.refresh(thread_id)?;
        if t.state == ThreadState::Expired {
            return Err(Error::Expired(thread_id.to_string()));
        }
        // A scheduled prompt starts a stopped VM only through the gate, held until boat has
        // resumed it (and counted the start).
        let may_start = || match &self.scheduled {
            Some(gate) => gate
                .take(|| self.boat.limits().ok())
                .map(Some)
                .ok_or_else(|| Error::Invalid(NO_STARTS.into())),
            None => Ok(None),
        };
        // The user's (a scheduled prompt too) starts it, and ends their stop once it's started;
        // a message they didn't send is checked again first, and leaves their stop theirs. A
        // thread's first prompt doesn't undo a stop either: the user stopped it as it started.
        let start = || {
            if unasked {
                may_go(&self.store, thread_id)?;
            }
            if (self.scheduled.is_some() || by == By::Start) && self.store.is_archived(thread_id)? {
                return Err(Error::Invalid(SCHEDULED_ARCHIVED.into()));
            }
            if by == By::Start && stopped_by_user(&self.store, thread_id) {
                return Err(Error::Invalid(START_STOPPED.into()));
            }
            if unasked && let Some(waking) = &self.waking {
                waking(&self.store)?;
            }
            let ticket = may_start()?;
            let t = self.resume_with(thread_id, ticket)?;
            if by == By::User {
                self.store.kv_delete(&stopped_by_user_key(thread_id))?;
            }
            Ok(t)
        };
        match sb.state.as_str() {
            _ if sb.is_up() => Ok(t),
            "archiving" => {
                self.wait_state(thread_id, "archived", STOP_TIMEOUT)?;
                start()
            }
            "archived" => start(),
            "error" | "cancelled" => Err(Error::Invalid(format!("The VM is {}", sb.state))),
            _ => self.wait_up(thread_id, READY_TIMEOUT),
        }
    }

    fn wait_state(&self, thread_id: &str, want: &str, timeout: Duration) -> Result<Thread> {
        let started = Instant::now();
        loop {
            let (t, sb) = self.refresh(thread_id)?;
            if sb.state == want {
                return Ok(t);
            }
            if started.elapsed() > timeout {
                return Err(Error::Timeout(format!(
                    "sandbox {} still {} after {}s",
                    sb.id,
                    sb.state,
                    timeout.as_secs()
                )));
            }
            sleep(Duration::from_secs(1));
        }
    }

    // ---- lifecycle ----

    pub fn new_thread(&mut self, req: &NewThread) -> Result<Thread> {
        let catalog = self.catalog()?;
        if self.store.environment(&req.environment)?.is_none() {
            self.sync_environments()?;
        }
        let (thread, local) = begin_thread(&self.store, &catalog, req)?;
        self.launch_thread(&thread.id, req, Some(&local))
    }

    /// The boat half of starting a thread `begin_thread` wrote: check the limits, create the
    /// VM, send the first prompt (`local`: its placeholder). If no VM was made, the thread stays,
    /// in `error` with its prompt unsent and the reason kept, for `retry_thread`.
    pub fn launch_thread(
        &mut self,
        thread_id: &str,
        req: &NewThread,
        local: Option<&str>,
    ) -> Result<Thread> {
        self.store.set_start_error(thread_id, None)?;
        self.store
            .set_thread_state(thread_id, ThreadState::Creating)?;
        let t = self.thread(thread_id)?;
        let limits = match self.limits() {
            Ok(l) => l,
            Err(e) => return Err(self.start_failed(thread_id, e)),
        };
        if limits.can_start == Some(false) {
            let e = Error::Invalid(format!(
                "boat won't start another sandbox right now ({}): {} of {} active",
                limits
                    .start_blocked_reason
                    .or(limits.blocked_reason)
                    .unwrap_or_else(|| "no reason given".into()),
                limits.active_sandboxes.unwrap_or(-1),
                limits.max_active_sandboxes.unwrap_or(-1),
            ));
            return Err(self.start_failed(thread_id, e));
        }
        // A scheduled thread's VM: through the gate, held until boat has made it.
        let ticket = match self.scheduled.clone() {
            Some(gate) => match gate.take(|| self.limits().ok()) {
                Some(t) => Some(t),
                None => return Err(self.start_failed(thread_id, Error::Invalid(NO_STARTS.into()))),
            },
            None => None,
        };

        let mut labels = vec![
            (
                "flotilla".to_string(),
                crate::flavor::vm_label().to_string(),
            ),
            ("flotilla.v".to_string(), "1".to_string()),
            ("flotilla.thread".to_string(), t.id.clone()),
            ("flotilla.provider".to_string(), t.provider.clone()),
            ("flotilla.model".to_string(), t.model.clone()),
        ];
        // Whose it is, so another Flotilla on this boat account leaves it alone.
        match crate::owner::Owner::load(&self.store) {
            Ok(owner) => labels.extend(owner.labels()),
            Err(e) => return Err(self.start_failed(thread_id, e)),
        }
        if let Some(e) = &t.effort {
            labels.push(("flotilla.effort".to_string(), e.clone()));
        }
        labels.push(("flotilla.fast".to_string(), t.fast.to_string()));
        if let Some(parent) = crate::orchestrate::parent(&self.store, thread_id) {
            labels.push(("flotilla.parent".to_string(), parent));
        }
        let project = match &t.repo_project_id {
            Some(id) => match self.store.project(id)? {
                Some(p) => Some(p),
                None => {
                    return Err(self.start_failed(
                        thread_id,
                        Error::NotFound("its project was removed".into()),
                    ));
                }
            },
            None => None,
        };
        // A new branch is named before the VM starts: its name goes in the VM's labels.
        let t = match &project {
            Some(p) if req.on_branch.is_none() => {
                self.name_branch(t, p, &req.prompt, req.branch_tag.as_deref())?
            }
            _ => t,
        };
        if let (Some(p), Some(branch)) = (&project, &t.branch) {
            labels.push(("flotilla.project".to_string(), p.id.clone()));
            labels.push(("flotilla.branch".to_string(), branch.clone()));
        }

        // Archived as it started: no VM. (An archive's own action takes its turn after this
        // one, and stops a VM made meanwhile.)
        let turns = self.turns.clone();
        let turn = turns
            .as_ref()
            .map(|l| l.lock().unwrap_or_else(|e| e.into_inner()));
        if self.store.is_archived(thread_id)? {
            return Err(self.archived_as_it_started(thread_id));
        }
        let (sb, fresh) = match &project {
            Some(p) => match self.project_vm(
                thread_id,
                &t,
                p,
                &labels,
                req.machine_type.as_deref().or(p.machine_type.as_deref()),
            ) {
                Ok(x) => x,
                Err(e) => {
                    let _ = self.store.set_start_stages(thread_id, None);
                    return Err(self.start_failed(thread_id, e));
                }
            },
            None => {
                let environment = t
                    .environment_name
                    .clone()
                    .unwrap_or_else(|| req.environment.clone());
                let name = crate::flavor::vm_name(&t.title);
                let create = CreateSandbox {
                    environment: &environment,
                    name: &name,
                    ttl: idle_ttl(&self.store, &t),
                    labels: &labels,
                    machine_type: req.machine_type.as_deref(),
                };
                // The thread id doubles as the Idempotency-Key, so a retry after a lost
                // response gets the same sandbox.
                match self.boat.create_sandbox(&create, thread_id) {
                    Ok(sb) => (sb, false),
                    Err(e) => return Err(self.start_failed(thread_id, e)),
                }
            }
        };
        // Made: boat counts the start now.
        drop(ticket);
        // Re-read: the thread was listed while boat created the VM, so it may have changed.
        let mut thread = self.thread(thread_id)?;
        thread.sandbox_id = Some(sb.id.clone());
        thread.state = sync::state_from_sandbox(&sb, false);
        self.store.update_thread(&thread)?;
        if project.is_some() && fresh {
            self.store.kv_set(&fresh_vm_key(thread_id), "1")?;
        }
        // Or while boat made it: the archive stops the VM, once this turn is over. From now
        // on, the first prompt (which never brings the thread back) isn't sent.
        if self.store.is_archived(thread_id)? {
            return Err(self.archived_as_it_started(thread_id));
        }
        drop(turn);

        if let Some(p) = &project
            && let Err(e) = self.prepared(thread_id, p)
        {
            return Err(self.unless_archived(thread_id, e));
        }
        let sent = (|| {
            if project.is_none() && req.wait_ready {
                self.wait_up(thread_id, READY_TIMEOUT)?;
            }
            // Without the wait, prompt straight away; a refusal still falls back to waiting.
            self.prompt_with(
                thread_id,
                &req.prompt,
                &req.attachments,
                req.wait_ready || project.is_some(),
                local,
                By::Start,
            )
        })();
        // Only now can anything else be sent.
        let _ = self.store.set_start_stages(thread_id, None);
        if let Err(e) = sent {
            // The VM exists, so the thread stays, its first prompt kept unsent with why (boat
            // refused it: a model the account has no key for, say), for Try again.
            if local.is_some() {
                self.store
                    .set_start_error(thread_id, Some(&first_prompt_refused(&e)))?;
            }
            return Err(self.unless_archived(thread_id, e));
        }
        self.thread(thread_id)
    }

    /// Let the preview model name a new branch from the first prompt (a stage on the start
    /// card). The first-words name `begin_thread` gave it stays if the model isn't set, fails,
    /// takes longer than a few seconds, or answers something unusable.
    /// `tag`: the thread's model, when it's one of several started from one prompt.
    fn name_branch(
        &self,
        t: Thread,
        p: &Project,
        prompt: &str,
        tag: Option<&str>,
    ) -> Result<Thread> {
        let Some(current) = t.branch.clone() else {
            return Ok(t);
        };
        let Some(model) = crate::summarize::configured_model(&self.store) else {
            return Ok(t);
        };
        self.stage(&t.id, "Name the branch");
        let answer = crate::summarize::Summarizer::new(&model).and_then(|s| s.branch(prompt));
        let Some(slug) = answer.ok().as_deref().and_then(model_branch) else {
            return Ok(t);
        };
        // Other threads' branches, not this one's first-words name, read and written with the
        // write lock held: siblings started from the same prompt are named at the same moment.
        let base = format!("{}{}", branch_prefix(&self.store), tagged(slug, tag));
        let name = self
            .store
            .claim_branch(&t.id, &p.id, |taken| unique_branch(&base, "", taken))?;
        if name == current {
            return Ok(t);
        }
        self.thread(&t.id)
    }

    /// Run the repo's `.agents/resume` in a project thread's VM that just woke, waiting up to
    /// 10 s, before the agent gets a prompt. A failure here never fails the caller.
    fn run_resume(&self, thread_id: &str) {
        self.run_hook(thread_id, crate::agents::Hook::Resume);
    }

    /// Run a hook in a project thread's VM (which must be up) with the thread's variables,
    /// waiting for it. A failure here never fails the caller: the row on the thread says so.
    fn run_hook(&self, thread_id: &str, hook: crate::agents::Hook) {
        let Ok(t) = self.thread(thread_id) else {
            return;
        };
        let Some(pid) = &t.repo_project_id else {
            return;
        };
        let Ok(Some(p)) = self.store.project(pid) else {
            return;
        };
        let notify = || {
            if let Some(f) = &self.on_progress {
                f();
            }
        };
        let _ = crate::agents::run_hook(&self.boat, &self.store, hook, &t, p.dir(), &notify);
    }

    /// Before a project thread is archived or deleted: run `.agents/archive` so it can remove
    /// third-party or remote resources made for the thread. A stopped VM is woken only if the
    /// repo has the script (asked of GitHub, on the thread's branch if it's there; without
    /// `github` it isn't woken), and waking it for this doesn't run `.agents/resume`. Returns
    /// whether it woke the VM. Nothing here stops the archive or delete.
    fn archive_hook(
        &self,
        thread_id: &str,
        github: Option<&crate::github::GitHub>,
        waking: Waking,
    ) -> bool {
        use crate::agents::{ArchivePlan, archive_plan};
        // As boat has it now: the VM may have stopped on its own since the last look.
        let Ok(t) = self
            .refresh(thread_id)
            .map(|(t, _)| t)
            .or_else(|_| self.thread(thread_id))
        else {
            return false;
        };
        let (Some(project_id), Some(_)) = (&t.repo_project_id, &t.sandbox_id) else {
            return false;
        };
        let Ok(Some(p)) = self.store.project(project_id) else {
            return false;
        };
        let up = matches!(t.state, ThreadState::Idle | ThreadState::Running);
        let stopped = matches!(t.state, ThreadState::Stopped | ThreadState::Stopping);
        let has_script = if up || !stopped {
            None
        } else {
            github.and_then(|gh| {
                let on_branch = t
                    .branch
                    .as_deref()
                    .filter(|b| gh.has_branch(&p.owner, &p.repo, b).unwrap_or(false));
                let at = on_branch.unwrap_or(&p.default_branch);
                gh.dir_files(&p.owner, &p.repo, ".agents", at)
                    .ok()
                    .map(|files| files.iter().any(|f| f == "archive"))
            })
        };
        let woke = match archive_plan(up, stopped, has_script) {
            ArchivePlan::Skip => return false,
            ArchivePlan::Run => false,
            ArchivePlan::Wake => {
                if !waking.allows(|| self.boat.limits().ok()) {
                    return false;
                }
                if t.state == ThreadState::Stopping
                    && self
                        .wait_state(thread_id, "archived", STOP_TIMEOUT)
                        .is_err()
                {
                    return false;
                }
                if self.start_vm(thread_id).is_err() {
                    return false;
                }
                true
            }
        };
        self.run_hook(thread_id, crate::agents::Hook::Archive);
        woke
    }

    /// `prepare_repo`, keeping a failure for Try again: the first prompt stays unsent and the
    /// reason is kept, so nothing reaches the agent before the repo is ready. On success the
    /// start stages stay (nothing else can be sent) until the first prompt is in.
    fn prepared(&mut self, thread_id: &str, p: &Project) -> Result<()> {
        // No prompt runs during setup, so nothing pushes the VM's auto-stop back: give it the
        // setup runs' long one meanwhile, then the thread's idle stop again.
        let t = self.thread(thread_id)?;
        let sb = Self::sandbox_id(&t)?;
        let _ = self
            .boat
            .update_sandbox(&sb, Ttl::Seconds(crate::setup::RUN_TTL_SECONDS), None);
        let r = self.prepare_repo(thread_id, p);
        // Re-read: "Keep running" may have been switched on meanwhile.
        if let Ok(t) = self.thread(thread_id) {
            let _ = self
                .boat
                .update_sandbox(&sb, idle_ttl(&self.store, &t), None);
        }
        if let Err(e) = &r {
            self.store.set_start_stages(thread_id, None)?;
            self.store
                .set_start_error(thread_id, Some(&e.to_string()))?;
        }
        if let Some(f) = &self.on_progress {
            f();
        }
        r
    }

    /// The stage running failed: take it off the card without marking it done (the card shows
    /// the failure itself).
    fn stage_failed(&self, thread_id: &str) {
        let mut st: StartStages = self
            .store
            .start_stages(thread_id)
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        st.current = None;
        let _ = self
            .store
            .set_start_stages(thread_id, serde_json::to_string(&st).ok().as_deref());
        if let Some(f) = &self.on_progress {
            f();
        }
    }

    /// Move a starting thread's card to its next stage.
    fn stage(&self, thread_id: &str, label: &str) {
        let mut st: StartStages = self
            .store
            .start_stages(thread_id)
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        if let Some(c) = st.current.take() {
            st.done.push(c);
        }
        st.current = Some(label.to_string());
        let _ = self
            .store
            .set_start_stages(thread_id, serde_json::to_string(&st).ok().as_deref());
        if let Some(f) = &self.on_progress {
            f();
        }
    }

    /// A project thread's VM: a copy of the project's latest prepared VM, straight away (a
    /// setup run in progress isn't waited for: the thread runs setup itself), or, with none,
    /// a fresh clone (`true`).
    fn project_vm(
        &mut self,
        thread_id: &str,
        t: &Thread,
        p: &Project,
        labels: &[(String, String)],
        machine_type: Option<&str>,
    ) -> Result<(Sandbox, bool)> {
        let branch = t.branch.clone().unwrap_or_default();
        self.stage(thread_id, &format!("Start a VM on branch {branch}"));
        // Setup's auto-stop until `prepared` is done (in case the VM isn't up for its PATCH).
        let ttl = Ttl::Seconds(crate::setup::RUN_TTL_SECONDS);
        if let Some(template) = &p.template_sandbox_id {
            match self
                .boat
                .fork_sandbox(template, labels, ttl, machine_type, thread_id)
            {
                Ok(sb) => {
                    // A fork can't be named when it's made.
                    let _ = self.boat.update_sandbox(
                        &sb.id,
                        Ttl::Omit,
                        Some(&crate::flavor::vm_name(&t.title)),
                    );
                    if let Some(sha) = &p.template_sha {
                        self.store.set_thread_base(thread_id, sha)?;
                    }
                    return Ok((sb, false));
                }
                // Deleted, or its disk is gone: start from a fresh clone instead, and forget
                // it so the project's next check sets it up again.
                Err(e) if e.is("snapshot_data_expired") || e.status() == Some(404) => {
                    let _ = self.store.forget_template(&p.id, template);
                }
                Err(e) => return Err(e.into()),
            }
        }
        let name = crate::flavor::vm_name(&t.title);
        let create = CreateSandbox {
            environment: &p.environment_name,
            name: &name,
            ttl,
            labels,
            machine_type,
        };
        Ok((
            self.boat
                .create_sandbox(&create, &format!("{thread_id}-fresh"))?,
            true,
        ))
    }

    /// Put a project thread's repo on its own branch at the latest default branch, then run
    /// setup (the project's scripts, then `.agents/setup`), each a stage on its start card.
    /// (`.agents/resume` isn't run here: it's for waking, and setup just ran.) Setup runs
    /// every time, so it must be quick when nothing changed. A script that fails or times out doesn't stop the start: the card and the
    /// thread's header say so, the rest of setup is skipped, and the agent gets its prompt.
    fn prepare_repo(&mut self, thread_id: &str, p: &Project) -> Result<()> {
        let fresh = self.store.kv_get(&fresh_vm_key(thread_id))?.as_deref() == Some("1");
        let t = self.wait_up(thread_id, READY_TIMEOUT)?;
        let sb = Self::sandbox_id(&t)?;
        let branch = t.branch.clone().unwrap_or_else(|| "flotilla".into());
        let main = &p.default_branch;
        let existing = on_existing_branch(&self.store, thread_id)?;
        let checkout = if existing {
            self.stage(thread_id, &format!("Check out {branch}"));
            checkout_existing(&branch, &base_branch(&self.store, thread_id, main))
        } else {
            self.stage(thread_id, &format!("Pull the latest {main}"));
            format!(
                "git fetch -q origin {m}\nhead=$(git rev-parse origin/{m})\ngit switch -q -c {b} \"$head\" 2>/dev/null || git switch -q {b}\n",
                m = sh_quote(main),
                b = sh_quote(&branch)
            )
        };
        let script = format!(
            "set -e\n{checkout}\
             if [ -e {s} ]; then has=1; else has=0; fi\n\
             echo \"$head $has\"",
            s = sh_quote(crate::agents::SETUP)
        );
        // Right after a fork boat may still be filling in the disk: the repo folder can look
        // empty and root-owned for a moment, and git refuses it. Try again for a while.
        let started = Instant::now();
        let mut pause = Duration::from_secs(1);
        let res = loop {
            let res = self.boat.exec(
                &sb,
                &format!("bash -lc {}", sh_quote(&script)),
                Some(p.dir()),
                300,
            )?;
            if res.ok() || !disk_filling(&res.stderr) || started.elapsed() > HYDRATE_WAIT {
                break res;
            }
            sleep(pause);
            pause = (pause * 2).min(Duration::from_secs(5));
        };
        if !res.ok() {
            let why = res
                .stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("git failed")
                .trim()
                .to_string();
            return Err(Error::Invalid(format!(
                "Couldn't put the repo on branch {branch}: {why}"
            )));
        }
        let first: Vec<&str> = res
            .stdout
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let head = first.first().copied().unwrap_or("");
        let agents_setup = first.get(1) == Some(&"1");
        if !head.is_empty() {
            self.store.set_thread_base(thread_id, head)?;
        }
        // The project's own scripts, then the repo's `.agents/setup`, as in a setup run.
        let mut to_run: Vec<(String, String, Duration)> = self
            .store
            .scripts(&p.id)?
            .into_iter()
            .filter(|s| fresh || !s.fresh_only)
            .map(|s| (s.command.clone(), s.command, crate::setup::SCRIPT_TIMEOUT))
            .collect();
        if agents_setup {
            let kind = if fresh { "fresh" } else { "update" };
            let thread_env = crate::agents::thread_env(thread_id, Some(&branch));
            let mut env: Vec<(&str, &str)> = vec![("FLOTILLA_SETUP", kind)];
            env.extend(thread_env.iter().map(|(k, v)| (*k, v.as_str())));
            let command = crate::agents::command(crate::agents::SETUP, &env);
            to_run.push((
                crate::agents::SETUP.to_string(),
                command,
                crate::agents::SETUP_TIMEOUT,
            ));
        }
        crate::setup::set_thread_setup_failure(&self.store, thread_id, None)?;
        for (label, command, timeout) in to_run {
            self.stage(thread_id, &label);
            if let Err(failure) =
                crate::setup::run_script(&self.boat, &sb, p.dir(), &label, &command, timeout)
            {
                crate::setup::set_thread_setup_failure(&self.store, thread_id, Some(&failure))?;
                self.stage_failed(thread_id);
                break;
            }
        }
        self.stage(thread_id, "Send your message to the agent");
        Ok(())
    }

    /// A thread that didn't get a VM: mark it so, keeping its prompt and the reason (archived
    /// meanwhile, that's why).
    fn start_failed(&mut self, thread_id: &str, e: impl Into<Error>) -> Error {
        let e = e.into();
        if self.store.is_archived(thread_id).unwrap_or(false) {
            return self.archived_as_it_started(thread_id);
        }
        let _ = self.store.set_thread_state(thread_id, ThreadState::Error);
        let _ = self.store.set_start_error(thread_id, Some(&e.to_string()));
        e
    }

    /// A start (or Try again) that failed once its VM was there: archived or stopped by the
    /// user meanwhile (its VM stopped under the repo's setup, say), that's why, as if it had
    /// seen the mark first.
    fn unless_archived(&mut self, thread_id: &str, e: Error) -> Error {
        if self.store.is_archived(thread_id).unwrap_or(false) {
            self.archived_as_it_started(thread_id)
        } else if stopped_by_user(&self.store, thread_id) {
            let _ = self.store.set_start_stages(thread_id, None);
            let _ = self
                .store
                .set_start_error(thread_id, Some(STOPPED_AS_IT_STARTED));
            Error::Invalid(START_STOPPED.into())
        } else {
            e
        }
    }

    /// The thread was archived as it started: it stays put away, its first prompt unsent with
    /// Try again (for once it's back). A VM boat made for it is the archive's to stop (its
    /// action takes the thread's turn after this). A scheduled start says so as any scheduled
    /// prompt into an archived thread does.
    fn archived_as_it_started(&mut self, thread_id: &str) -> Error {
        if self.thread(thread_id).is_ok_and(|t| t.sandbox_id.is_none()) {
            let _ = self.store.set_thread_state(thread_id, ThreadState::Error);
        }
        let _ = self.store.set_start_stages(thread_id, None);
        let _ = self
            .store
            .set_start_error(thread_id, Some(ARCHIVED_AS_IT_STARTED));
        Error::Invalid(SCHEDULED_ARCHIVED.into())
    }

    /// After a restart, once a full sync has read boat's history: settle what actions cut short
    /// by quitting left behind (anything from before `started`, this run's start). Only threads
    /// without a VM, or whose history this sync read (`synced`), are looked at; the rest wait
    /// for a later sync. Nothing is deleted or moved:
    /// - a message still "sending" that boat got takes its event's id, as a finished send does;
    ///   one boat never got stays in place, marked "not sent" (Send again, Discard);
    /// - a start cut short is finished if boat got the first prompt, and otherwise gets Try
    ///   again with the prompt kept (a thread without a VM always does);
    /// - hooks Flotilla was waiting on stop showing as running.
    ///
    /// A message only counts as got when it matches an event one to one: the same kind and
    /// text, no earlier than it was sent, and not already another send's.
    ///
    /// `skip`: threads not to touch (already settled this run, or with an action under way,
    /// such as a Try again that reuses the old first prompt). Every thread looked at is listed
    /// in `looked_at`, so the caller can skip it from then on.
    pub fn reconcile_after_quit(
        &mut self,
        started: &str,
        synced: &std::collections::HashSet<String>,
        skip: &std::collections::HashSet<String>,
    ) -> Result<Reconciled> {
        let mut out = Reconciled::default();
        for t in self.store.threads()? {
            if skip.contains(&t.id)
                || t.state == ThreadState::Expired
                || t.created_at.as_str() >= started
            {
                continue;
            }
            if t.sandbox_id.is_some() && !synced.contains(&t.id) {
                continue;
            }
            out.looked_at.push(t.id.clone());
            if self.store.start_error(&t.id)?.is_some() {
                continue;
            }
            let sent = self.store.sent_prompts(&t.id)?;
            let stale: Vec<&crate::store::SentPrompt> = sent
                .iter()
                .filter(|s| s.id.starts_with(LOCAL_PREFIX) && s.sent_at.as_str() < started)
                .collect();
            let cut_short = self.store.start_stages(&t.id)?.is_some();
            if t.sandbox_id.is_none() {
                // No VM, so boat has nothing of it: the start failed before it got that far.
                if t.state == ThreadState::Creating || cut_short {
                    self.store.set_start_stages(&t.id, None)?;
                    self.store.set_thread_state(&t.id, ThreadState::Error)?;
                    self.store.set_start_error(
                        &t.id,
                        Some("Flotilla quit before this thread started."),
                    )?;
                    out.failed_starts.push(t.id.clone());
                }
                continue;
            }
            if stale.is_empty() && !cut_short {
                continue;
            }
            let events: Vec<crate::event::RawEvent> = self
                .store
                .events(&t.id)?
                .into_iter()
                .map(|e| e.event)
                .collect();
            for (local, key) in match_sent(&sent, &stale, &events) {
                let s = stale
                    .iter()
                    .find(|s| s.id == local)
                    .expect("matched a stale placeholder");
                self.store
                    .record_sent_prompt(&t.id, Some(&local), &key, &s.kind, &s.text)?;
            }
            // Boat has the first prompt once any prompt is recorded under boat's id, or boat's
            // history has one (a thread imported from boat has no records of what it sent).
            let first_got = self
                .store
                .sent_prompts(&t.id)?
                .iter()
                .any(|s| s.kind == "prompt" && !is_placeholder(&s.id))
                || events.iter().any(|e| e.kind == "prompt" && !e.is_partial());
            if cut_short || !first_got {
                self.store.set_start_stages(&t.id, None)?;
                if !first_got {
                    self.store.set_start_error(
                        &t.id,
                        Some("Flotilla quit before this thread's first message reached the agent."),
                    )?;
                    out.failed_starts.push(t.id.clone());
                }
            }
            // What boat never got stays, marked: except a failed start's first prompt, which
            // Try again sends.
            let mut marked = false;
            for s in self
                .store
                .sent_prompts(&t.id)?
                .iter()
                .filter(|s| s.id.starts_with(LOCAL_PREFIX) && s.sent_at.as_str() < started)
            {
                let first_of_failed_start = !first_got && s.kind == "prompt";
                // An answer to the agent's question that never went: it goes, with its
                // message, and the question waits again.
                let answer = !first_of_failed_start
                    && crate::asks::answers(&self.store, &t.id)
                        .values()
                        .any(|a| a.local == s.id);
                if answer {
                    crate::asks::drop_with_message(&self.store, &t.id, &s.id)?;
                } else if !first_of_failed_start {
                    self.store.mark_unsent(&t.id, &s.id)?;
                    marked = true;
                }
            }
            if marked {
                out.not_sent.push(t.id.clone());
            }
            self.store.rebuild_messages(&t.id)?;
        }
        crate::agents::interrupt_stale_hooks(&self.store, started)?;
        Ok(out)
    }

    /// Start again a thread whose start failed before it had a VM (see `retry_request`).
    pub fn retry_thread(&mut self, thread_id: &str) -> Result<Thread> {
        let t = self.thread(thread_id)?;
        // Try again brings the thread back from the archive as it's asked (the host's), so an
        // archive after it stays: archived, it isn't sent.
        // It has its VM but the repo wasn't prepared: prepare it, then send the first prompt.
        if t.sandbox_id.is_some()
            && let Some(pid) = &t.repo_project_id
        {
            let p = self
                .store
                .project(pid)?
                .ok_or_else(|| Error::NotFound("its project was removed".into()))?;
            let first = self
                .store
                .sent_prompts(thread_id)?
                .into_iter()
                .find(|s| s.kind == "prompt")
                .ok_or_else(|| Error::Invalid("this thread has no prompt to send".into()))?;
            // Its VM stopped since (archived as it started, or idle): started again first.
            let up = {
                let turns = self.turns.clone();
                let _turn = turns
                    .as_ref()
                    .map(|l| l.lock().unwrap_or_else(|e| e.into_inner()));
                self.ensure_up(thread_id, By::Start)
            };
            if let Err(e) = up {
                return Err(self.unless_archived(thread_id, e));
            }
            self.store.set_start_error(thread_id, None)?;
            if let Err(e) = self.prepared(thread_id, &p) {
                return Err(self.unless_archived(thread_id, e));
            }
            let sent = self.prompt_with(
                thread_id,
                &first.text,
                &first.attachments,
                true,
                Some(&first.id),
                By::Start,
            );
            self.store.set_start_stages(thread_id, None)?;
            if let Err(e) = sent {
                // Back to Try again, the prompt still unsent.
                self.store
                    .set_start_error(thread_id, Some(&first_prompt_refused(&e)))?;
                return Err(self.unless_archived(thread_id, e));
            }
            return self.thread(thread_id);
        }
        // It has its VM and boat refused the first prompt: send it again (no new VM).
        if t.sandbox_id.is_some() {
            let first = self
                .store
                .sent_prompts(thread_id)?
                .into_iter()
                .find(|s| s.kind == "prompt")
                .ok_or_else(|| Error::Invalid("this thread has no prompt to send".into()))?;
            self.store.set_start_error(thread_id, None)?;
            if let Err(e) = self.prompt_with(
                thread_id,
                &first.text,
                &first.attachments,
                true,
                Some(&first.id),
                By::Start,
            ) {
                self.store
                    .set_start_error(thread_id, Some(&first_prompt_refused(&e)))?;
                return Err(self.unless_archived(thread_id, e));
            }
            return self.thread(thread_id);
        }
        let (req, local) = retry_request(&self.store, thread_id)?;
        self.launch_thread(thread_id, &req, Some(&local))
    }

    /// Steer if a turn is active, otherwise prompt (resuming a stopped VM first).
    pub fn send(&mut self, thread_id: &str, text: &str) -> Result<Sent> {
        self.send_as(thread_id, text, &[], None)
    }

    /// `send`, with `files`, for a message already shown from a placeholder row in
    /// `sent_prompts` (id `local`, written when Send was pressed, so the message appears before
    /// the VM resumes). The placeholder takes boat's id when the send goes through and is
    /// removed if it fails.
    pub fn send_as(
        &mut self,
        thread_id: &str,
        text: &str,
        files: &[crate::attach::Attached],
        local: Option<&str>,
    ) -> Result<Sent> {
        self.send_for(thread_id, text, files, local, false)
    }

    /// `send` for a message the user didn't send (Flotilla's own, like a hand-off's answer, or
    /// an agent's to another): unlike theirs, it never brings the thread back from the archive
    /// or starts a VM the user stopped. That's checked again before each step that could (a
    /// steer that falls back to a prompt, a VM not ready yet); if it can't go, `Error::NotNow`.
    pub fn send_unasked(&mut self, thread_id: &str, text: &str) -> Result<Sent> {
        self.send_for(thread_id, text, &[], None, true)
    }

    fn send_for(
        &mut self,
        thread_id: &str,
        text: &str,
        files: &[crate::attach::Attached],
        local: Option<&str>,
        unasked: bool,
    ) -> Result<Sent> {
        // A project thread still preparing its repo (or that failed to): its first prompt
        // goes first, from the start (or Try again).
        let starting = self.store.start_stages(thread_id)?.is_some()
            || self.store.start_error(thread_id)?.is_some();
        let result = if starting {
            Err(Error::Invalid("the thread hasn't finished starting".into()))
        } else if unasked && let Err(e) = may_go(&self.store, thread_id) {
            Err(e)
        } else if self.store.unsettled_turns(thread_id)?.is_empty() {
            self.prompt_with(thread_id, text, files, true, local, By::of(unasked))
                .map(Sent::Prompt)
        } else if !files.is_empty() {
            // A steer can't carry files.
            Err(Error::Invalid(
                "Files can go once the agent has finished".into(),
            ))
        } else {
            self.steer_as(thread_id, text, local, unasked)
        };
        if result.is_err()
            && let Some(local) = local
        {
            // With any answer it took: that question waits again.
            crate::asks::drop_with_message(&self.store, thread_id, local)?;
            self.store.rebuild_messages(thread_id)?;
        }
        result
    }

    /// A scheduled prompt into a thread: a new turn, never a steer. A turn running (or the
    /// thread still starting) since the run was due fails with [`SCHEDULED_BUSY`], so it waits
    /// again; `local` is its placeholder, gone if it isn't sent.
    pub fn send_later(&mut self, thread_id: &str, text: &str, local: &str) -> Result<Sent> {
        if self.scheduled.is_none() {
            self.scheduled = Some(std::sync::Arc::default());
        }
        let busy = self.store.start_stages(thread_id)?.is_some()
            || self.store.start_error(thread_id)?.is_some()
            || !self.store.unsettled_turns(thread_id)?.is_empty();
        let result = if busy {
            Err(Error::Invalid(SCHEDULED_BUSY.into()))
        } else {
            self.prompt_with(thread_id, text, &[], true, Some(local), By::User)
                .map(Sent::Prompt)
        };
        if result.is_err() {
            self.store.delete_sent_prompt(thread_id, local)?;
            self.store.rebuild_messages(thread_id)?;
        }
        result
    }

    /// Put the thread away: run the repo's `.agents/archive` (a project thread), stop its VM if
    /// it's up, and sort it to the bottom. `github` lets a stopped VM be woken for the script.
    pub fn archive(
        &mut self,
        thread_id: &str,
        github: Option<&crate::github::GitHub>,
    ) -> Result<()> {
        self.archive_as(thread_id, github, Waking::Yes)
    }

    /// `archive`, waking a stopped VM for `.agents/archive` only as `waking` allows.
    pub fn archive_as(
        &mut self,
        thread_id: &str,
        github: Option<&crate::github::GitHub>,
        waking: Waking,
    ) -> Result<()> {
        // Its links are due now first, in their own records (which a rebuild carries, as it
        // doesn't the archive): cut off anywhere after this, they still end.
        crate::public::closing(&self.store, thread_id, crate::util::now_ms())?;
        self.store.set_archived(thread_id, true)?;
        self.put_away(thread_id, github, waking)
    }

    /// `archive_as` for a thread already marked archived as it was asked (the host's Archive,
    /// the engine's own), its action run later: nothing if it's been brought back since (Try
    /// again, Unarchive, a message of yours), and the mark isn't written again.
    pub fn archive_marked(
        &mut self,
        thread_id: &str,
        github: Option<&crate::github::GitHub>,
        waking: Waking,
    ) -> Result<()> {
        if !self.store.is_archived(thread_id)? {
            return Ok(());
        }
        crate::public::closing(&self.store, thread_id, crate::util::now_ms())?;
        self.put_away(thread_id, github, waking)
    }

    /// `.agents/archive`, the links ended and the VM stopped, for an archive.
    fn put_away(
        &mut self,
        thread_id: &str,
        github: Option<&crate::github::GitHub>,
        waking: Waking,
    ) -> Result<()> {
        self.archive_hook(thread_id, github, waking);
        let t = self.thread(thread_id)?;
        // Nothing of an archived thread stays public.
        crate::public::end_all(&self.boat, &self.store, &t);
        if t.sandbox_id.is_some()
            && matches!(
                t.state,
                ThreadState::Idle | ThreadState::Running | ThreadState::Starting
            )
        {
            self.stop(thread_id, true)?;
        }
        Ok(())
    }

    /// Nothing runs here: the next send wakes the VM, and `.agents/resume` brings back what
    /// `.agents/archive` removed.
    pub fn unarchive(&mut self, thread_id: &str) -> Result<()> {
        self.store.set_archived(thread_id, false)
    }

    /// Start a turn with the thread's locked settings. Returns the prompt id.
    /// Sending to an archived thread brings it back.
    pub fn prompt(&mut self, thread_id: &str, text: &str) -> Result<String> {
        self.prompt_with(thread_id, text, &[], true, None, By::User)
    }

    /// `by`: who it's from. A message the user didn't send (`By::Unasked`) is checked before
    /// each step.
    fn prompt_with(
        &mut self,
        thread_id: &str,
        text: &str,
        files: &[crate::attach::Attached],
        check_up: bool,
        local: Option<&str>,
        by: By,
    ) -> Result<String> {
        let t = self.thread(thread_id)?;
        if t.state == ThreadState::Expired {
            return Err(Error::Expired(thread_id.to_string()));
        }
        let unasked = by == By::Unasked;
        // A first prompt wakes the VM and goes in its thread's turn (`turns`).
        let turns = self.turns.clone().filter(|_| by == By::Start);
        let _turn = turns
            .as_ref()
            .map(|l| l.lock().unwrap_or_else(|e| e.into_inner()));
        let may = |store: &Store| {
            if unasked {
                may_go(store, thread_id)
            } else {
                Ok(())
            }
        };
        // Before waking the VM: a file whose copy has expired fails the send here.
        let attachments = crate::attach::load(files, &crate::blobs::Blobs::of(&self.store))?;
        may(&self.store)?;
        // A scheduled prompt never brings an archived thread back (archived since it was due),
        // nor does a thread's first (archived as it started): it's asked again before each step
        // that could wake the VM or send, and never written.
        let scheduled = self.scheduled.is_some();
        let keeps_archive = scheduled || by == By::Start;
        // Nor does a thread's first undo the user's Stop (pressed as it started).
        let put_away = |store: &Store| {
            if keeps_archive && store.is_archived(thread_id)? {
                Err(Error::Invalid(SCHEDULED_ARCHIVED.into()))
            } else if by == By::Start && stopped_by_user(store, thread_id) {
                Err(Error::Invalid(START_STOPPED.into()))
            } else {
                Ok(())
            }
        };
        put_away(&self.store)?;
        // The user's message brings the thread back from the archive (one they scheduled
        // doesn't); no one else's does. Their stop ends once the VM is started, or the message
        // in (below): one that fails leaves it.
        if !unasked && !keeps_archive {
            self.store.set_archived(thread_id, false)?;
        }
        // The agent's files (the `flotilla` command, the instructions) go in first when the VM
        // lacks what it should have now, so it waits for the VM even on a first prompt.
        let provision = crate::provision::needed(&self.store, thread_id);
        let mut t = if check_up || provision {
            self.ensure_up(thread_id, by)?
        } else {
            t
        };
        let sandbox_id = Self::sandbox_id(&t)?;
        // Without them the agent would run missing your instructions and skills: the prompt
        // waits for them (one more try, for a VM still settling after it came up).
        if provision
            && crate::provision::write(&self.boat, &self.store, thread_id, &sandbox_id).is_err()
        {
            sleep(Duration::from_secs(2));
            crate::provision::write(&self.boat, &self.store, thread_id, &sandbox_id)?;
        }
        may(&self.store)?;
        // OpenCode drops a prompt that starts with an unknown /name: keep that one plain text.
        let known: Vec<String> = crate::mentions::stored(&self.store, thread_id)
            .map(|m| {
                m.entries
                    .into_iter()
                    .filter(|e| e.harness == t.provider)
                    .map(|e| e.name)
                    .collect()
            })
            .unwrap_or_default();
        let guarded = crate::mentions::guard_slash(&t.provider, text, &known);
        let text = guarded.as_deref().unwrap_or(text);
        let send = |conv: Option<&str>| {
            self.boat.prompt(
                &sandbox_id,
                &PromptRequest {
                    provider: &t.provider,
                    model: &t.model,
                    reasoning_effort: t.effort.as_deref(),
                    fast: t.fast,
                    // With no conversation yet (e.g. the first prompt failed), start one.
                    conversation_id: conv,
                    prompt: text,
                    attachments: &attachments,
                },
            )
        };
        put_away(&self.store)?;
        // A turn started while the VM woke (a thread made for this prompt has none): it waits.
        if scheduled && !self.store.unsettled_turns(thread_id)?.is_empty() {
            return Err(Error::Invalid(SCHEDULED_BUSY.into()));
        }
        let resp = match send(t.conversation_id.as_deref()) {
            Err(e) if e.is("sandbox_not_promptable") || e.is("sandbox_not_ready") => {
                // It may be stopping because the user stopped (or archived) it just now.
                may(&self.store)?;
                put_away(&self.store)?;
                self.ensure_up(thread_id, by)?;
                may(&self.store)?;
                put_away(&self.store)?;
                send(t.conversation_id.as_deref())?
            }
            r => r?,
        };
        t = self.thread(thread_id)?;
        if t.conversation_id.is_none() {
            t.conversation_id = Some(resp.conversation_id.clone());
        }
        t.state = ThreadState::Running;
        self.store.update_thread(&t)?;
        self.store
            .record_sent_prompt(thread_id, local, &resp.prompt_id, "prompt", text)?;
        self.store
            .set_sent_attachments(thread_id, &resp.prompt_id, files)?;
        let status = resp
            .prompt_run
            .as_ref()
            .map(|r| r.status.clone())
            .or(resp.status.clone())
            .unwrap_or_else(|| "queued".into());
        self.store
            .insert_turn(thread_id, &resp.prompt_id, &status)?;
        self.store.rebuild_messages(thread_id)?;
        if by == By::User {
            self.store.kv_delete(&stopped_by_user_key(thread_id))?;
            // Theirs reached the agent (one they scheduled too, at most once a day): the tree
            // may hand off as many again. One that didn't go doesn't.
            let _ = crate::orchestrate::user_wrote(&self.store, thread_id);
        }
        Ok(resp.prompt_id)
    }

    /// Steer the running turn. Falls back to a prompt if the turn already ended.
    pub fn steer(&mut self, thread_id: &str, text: &str) -> Result<Sent> {
        self.steer_as(thread_id, text, None, false)
    }

    fn steer_as(
        &mut self,
        thread_id: &str,
        text: &str,
        local: Option<&str>,
        unasked: bool,
    ) -> Result<Sent> {
        let t = self.thread(thread_id)?;
        let sandbox_id = Self::sandbox_id(&t)?;
        let Some(conv) = t.conversation_id.clone() else {
            return self
                .prompt_with(thread_id, text, &[], true, local, By::of(unasked))
                .map(Sent::Prompt);
        };
        for _ in 0..10 {
            if unasked {
                may_go(&self.store, thread_id)?;
            }
            match self.boat.steer(&sandbox_id, &conv, text) {
                Ok(r) => {
                    self.store
                        .record_sent_prompt(thread_id, local, &r.prompt_id, "steer", text)?;
                    self.store.rebuild_messages(thread_id)?;
                    if !unasked {
                        self.store.kv_delete(&stopped_by_user_key(thread_id))?;
                        let _ = crate::orchestrate::user_wrote(&self.store, thread_id);
                    }
                    return Ok(Sent::Steer(r.prompt_id));
                }
                Err(e) if e.is("no_running_turn") && e.reason() == Some("turn_not_started") => {
                    sleep(Duration::from_secs(1));
                }
                Err(e) if e.is("no_running_turn") => {
                    return self
                        .prompt_with(thread_id, text, &[], true, local, By::of(unasked))
                        .map(Sent::Prompt);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::Timeout(
            "the turn didn't start within ~10 s, so the steer wasn't delivered".into(),
        ))
    }

    /// The stop button: interrupt this thread's conversation only.
    pub fn interrupt(&mut self, thread_id: &str) -> Result<()> {
        let t = self.thread(thread_id)?;
        let conv = t
            .conversation_id
            .clone()
            .ok_or_else(|| Error::Invalid("The agent hasn't started yet".into()))?;
        self.boat.interrupt(&Self::sandbox_id(&t)?, &conv)?;
        Ok(())
    }

    /// Stop the VM. History stays readable.
    pub fn stop(&mut self, thread_id: &str, wait: bool) -> Result<Thread> {
        let mut t = self.thread(thread_id)?;
        // Marked first, so a message of Flotilla's own about to go sees it; a stop that didn't
        // go through leaves the mark as it was (an earlier Stop's stays). With when boat last
        // had it stopped before this, so a reading with a later stop and the VM up again
        // (started from boat's CLI while Flotilla wasn't looking) ends it.
        let key = stopped_by_user_key(thread_id);
        let earlier = self.store.kv_get(&key)?;
        let before = self
            .refresh(thread_id)
            .ok()
            .map(|(_, sb)| sb.stopped_at)
            .unwrap_or_else(|| t.last_stopped_at.clone());
        self.store.kv_set(
            &key,
            &format!("stopping {}", before.as_deref().unwrap_or("-")),
        )?;
        if let Err(e) =
            Self::sandbox_id(&t).and_then(|sb| self.boat.stop_sandbox(&sb).map_err(Into::into))
        {
            match earlier {
                Some(mark) => self.store.kv_set(&key, &mark)?,
                None => self.store.kv_delete(&key)?,
            }
            return Err(e);
        }
        t.state = ThreadState::Stopping;
        self.store
            .set_thread_state(thread_id, ThreadState::Stopping)?;
        if wait {
            self.wait_state(thread_id, "archived", STOP_TIMEOUT)
        } else {
            Ok(t)
        }
    }

    /// Resume a stopped VM with the idle TTL (a resume otherwise keeps the last TTL), then run
    /// the repo's `.agents/resume` in a project thread (up to 10 s).
    /// The user's own: once it's started, their stop is no longer.
    pub fn resume(&self, thread_id: &str) -> Result<Thread> {
        let t = self.resume_with(thread_id, None)?;
        self.store.kv_delete(&stopped_by_user_key(thread_id))?;
        Ok(t)
    }

    /// Start the VM again (with a scheduled start's ticket, let go once boat has resumed it) and
    /// run `.agents/resume`, leaving the user's stop as it is.
    fn resume_with(&self, thread_id: &str, ticket: Option<StartTicket>) -> Result<Thread> {
        let t = self.start_vm_with(thread_id, ticket)?;
        self.run_resume(thread_id);
        Ok(t)
    }

    /// Start a stopped VM again and wait for it, without `.agents/resume`.
    fn start_vm(&self, thread_id: &str) -> Result<Thread> {
        self.start_vm_with(thread_id, None)
    }

    fn start_vm_with(&self, thread_id: &str, ticket: Option<StartTicket>) -> Result<Thread> {
        let t = self.thread(thread_id)?;
        let resumed = self
            .boat
            .resume_sandbox(&Self::sandbox_id(&t)?, idle_ttl(&self.store, &t));
        // Resumed (or refused): boat has counted it.
        drop(ticket);
        match resumed {
            Ok(()) => {}
            Err(e) if e.is("snapshot_data_expired") => return Err(self.mark_expired(thread_id)?),
            Err(e) => return Err(e.into()),
        }
        let t = self.wait_up(thread_id, READY_TIMEOUT)?;
        // Before anything is sent to it: links that ended while it was stopped go private, the
        // rest are public again.
        crate::public::on_wake(&self.boat, &self.store, &t, crate::util::now_ms());
        Ok(t)
    }

    pub fn rename(&mut self, thread_id: &str, title: &str) -> Result<Thread> {
        let mut t = self.thread(thread_id)?;
        let title = truncate_chars(title.trim(), title_max());
        if title.is_empty() {
            return Err(Error::Invalid("the title is empty".into()));
        }
        // A name-only PATCH leaves the auto-stop deadline alone.
        self.boat.update_sandbox(
            &Self::sandbox_id(&t)?,
            Ttl::Omit,
            Some(&crate::flavor::vm_name(&title)),
        )?;
        self.store.set_thread_title(thread_id, &title)?;
        t.title = title;
        Ok(t)
    }

    /// With the VM up, boat's auto-stop deadline changes first and the switch is saved only
    /// once that worked, so a failure leaves both as they were. With it down, it's only saved
    /// here, for the next resume.
    pub fn set_keep_running(&mut self, thread_id: &str, on: bool) -> Result<Thread> {
        let mut t = self.thread(thread_id)?;
        t.keep_running = on;
        if matches!(t.state, ThreadState::Idle | ThreadState::Running) {
            let sb = self.boat.update_sandbox(
                &Self::sandbox_id(&t)?,
                idle_ttl(&self.store, &t),
                None,
            )?;
            // `null` with Keep running: no deadline any more.
            self.store
                .set_archive_after(thread_id, sb.archive_after.as_deref())?;
            t.archive_after = sb.archive_after;
        }
        self.store.set_keep_running(thread_id, on)?;
        Ok(t)
    }

    /// A preview URL for a port in the VM (resumes it if needed): private, or the public one
    /// while you've made it public (`crate::public`).
    pub fn preview(&mut self, thread_id: &str, port: u16) -> Result<String> {
        let t = self.ensure_up(thread_id, By::User)?;
        crate::public::host(
            &self.boat,
            &self.store,
            thread_id,
            &Self::sandbox_id(&t)?,
            port,
        )
    }

    /// Let anyone with the port's link open it, with no sign-in, `for` this long (resumes the
    /// VM if needed). Returns the link.
    pub fn make_public(
        &mut self,
        thread_id: &str,
        port: u16,
        for_: flotilla_proto::PublicFor,
    ) -> Result<String> {
        // Yours, and deliberate: it brings the thread back from the archive, as a message of
        // yours does (an archived thread keeps nothing public).
        self.store.set_archived(thread_id, false)?;
        let t = self.ensure_up(thread_id, By::User)?;
        crate::public::make_public(
            &self.boat,
            &self.store,
            &t,
            port,
            for_,
            crate::util::now_ms(),
        )
    }

    /// The port's link needs boat's token again: at once, or as a stopped VM wakes (it's never
    /// woken for this).
    pub fn make_private(&mut self, thread_id: &str, port: u16) -> Result<()> {
        let t = self.thread(thread_id)?;
        crate::public::make_private(&self.boat, &self.store, &t, port)
    }

    /// Delete the sandbox on boat and every local row for the thread. A project thread that
    /// wasn't archived runs `.agents/archive` first (archiving already did). A VM that isn't
    /// this Flotilla's is left alone, hook and all, and the thread only forgotten here: a copy
    /// of this database elsewhere lists threads whose VMs another Flotilla runs.
    pub fn delete(
        &mut self,
        thread_id: &str,
        github: Option<&crate::github::GitHub>,
    ) -> Result<()> {
        let t = self.thread(thread_id)?;
        let ours = match &t.sandbox_id {
            None => None,
            Some(sb) => match self.boat.get_sandbox(sb) {
                Ok(vm) => crate::owner::Owner::load(&self.store)?
                    .mine(&vm)
                    .then_some(sb),
                // Gone already.
                Err(e) if e.status() == Some(404) => None,
                Err(e) => return Err(e.into()),
            },
        };
        if let Some(sb) = ours {
            if !self.store.is_archived(thread_id)? {
                self.archive_hook(thread_id, github, Waking::Yes);
            }
            match self.boat.delete_sandbox(sb) {
                Ok(()) => {}
                Err(e) if e.status() == Some(404) => {}
                Err(e) => return Err(e.into()),
            }
        }
        // A VM not this Flotilla's stays up: its links end through boat all the same.
        crate::public::forgetting(&self.store, thread_id, crate::util::now_ms(), None, |s| {
            s.delete_thread(thread_id)
        })?;
        self.live.remove(thread_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    /// A send of the user's that fails before their VM starts (a scheduled one the start gate
    /// or boat refuses, one now, a Resume), or a Stop that fails, leaves their stop as it was: a
    /// message they didn't send can't then start the VM.
    #[test]
    fn a_failed_send_or_resume_leaves_the_users_stop() {
        use super::*;
        let store = Store::open_in_memory().unwrap();
        store
            .insert_thread(&Thread {
                id: "t".into(),
                environment_id: None,
                environment_name: None,
                title: "Check CI".into(),
                sandbox_id: Some("bx_t".into()),
                conversation_id: Some("conv".into()),
                provider: "claude-code".into(),
                model: "sonnet".into(),
                effort: None,
                fast: false,
                state: ThreadState::Stopped,
                keep_running: false,
                archive_after: None,
                last_stopped_at: None,
                data_expired_at: None,
                created_at: now_iso(),
                updated_at: now_iso(),
                repo_project_id: None,
                branch: None,
                base_sha: None,
            })
            .unwrap();
        store
            .kv_set(
                &stopped_by_user_key("t"),
                "stopped 2026-11-01T10:00:00Z 2026-11-01T10:00:00Z",
            )
            .unwrap();
        let boat = BoatClient::with_base(
            "boat_test".into(),
            "http://127.0.0.1:9/api/v1",
            "http://127.0.0.1:9/api/v1/provider-models",
        )
        .unwrap();
        let mut core = Core::new(boat, store);
        core.scheduled = Some(std::sync::Arc::default());
        assert!(core.send_later("t", "Check CI", "local-1").is_err());
        assert!(
            stopped_by_user(&core.store, "t"),
            "a scheduled send that didn't go"
        );
        core.scheduled = None;
        assert!(core.send_as("t", "And now?", &[], None).is_err());
        assert!(stopped_by_user(&core.store, "t"), "a send that didn't go");
        assert!(core.resume("t").is_err());
        assert!(
            stopped_by_user(&core.store, "t"),
            "a resume that didn't start it"
        );
        assert!(!crate::orchestrate::may_wake(&core.store, "t"));
        // A Stop again that boat doesn't take leaves the earlier one's mark; one with none
        // before leaves none.
        let mark = core.store.kv_get(&stopped_by_user_key("t")).unwrap();
        assert!(core.stop("t", false).is_err());
        assert_eq!(core.store.kv_get(&stopped_by_user_key("t")).unwrap(), mark);
        core.store.kv_delete(&stopped_by_user_key("t")).unwrap();
        assert!(core.stop("t", false).is_err());
        assert!(!stopped_by_user(&core.store, "t"));
    }

    /// Only a prompt or steer of the user's that reaches the agent lets the thread's tree hand
    /// off as many again: one they scheduled counts once it goes, but not a send that didn't
    /// (boat refused it), scheduled or not, however often it's tried.
    #[test]
    fn only_a_message_of_yours_that_goes_starts_the_hand_offs_again() {
        use super::*;
        use crate::orchestrate::{MAX_SINCE_YOU, set_parent, spend};
        let made = |store: &Store, id: &str| {
            let t = Thread {
                id: id.into(),
                environment_id: None,
                environment_name: None,
                title: "Check CI".into(),
                sandbox_id: Some(format!("bx_{id}")),
                conversation_id: Some("conv".into()),
                provider: "claude-code".into(),
                model: "sonnet".into(),
                effort: None,
                fast: false,
                state: ThreadState::Idle,
                keep_running: false,
                archive_after: None,
                last_stopped_at: None,
                data_expired_at: None,
                created_at: now_iso(),
                updated_at: now_iso(),
                repo_project_id: None,
                branch: None,
                base_sha: None,
            };
            store.insert_thread(&t).unwrap();
        };
        let spent = |store: &Store| {
            for _ in 0..MAX_SINCE_YOU {
                let _ = spend(store, "c");
            }
            assert!(spend(store, "c").is_err());
        };
        let store = Store::open_in_memory().unwrap();
        made(&store, "t");
        made(&store, "c");
        set_parent(&store, "c", "t").unwrap();
        spent(&store);
        // Boat can't be reached: nothing goes, nothing more may be handed off.
        let down = BoatClient::with_base(
            "boat_test".into(),
            "http://127.0.0.1:9/api/v1",
            "http://127.0.0.1:9/api/v1/provider-models",
        )
        .unwrap();
        let mut core = Core::new(down, store);
        for _ in 0..3 {
            core.scheduled = Some(std::sync::Arc::default());
            assert!(core.send_later("t", "Check CI", "local-1").is_err());
            core.scheduled = None;
            assert!(core.send_as("t", "And now?", &[], None).is_err());
        }
        assert!(
            spend(&core.store, "c").is_err(),
            "a send that didn't go started them again"
        );
        // They go: each starts them again.
        let (up, _) = hooked_boat(|_, _| {});
        let mut core = Core::new(up, core.store);
        core.scheduled = Some(std::sync::Arc::default());
        core.send_later("t", "Check CI", "local-2").unwrap();
        assert_eq!(
            spend(&core.store, "c"),
            Ok(()),
            "a scheduled prompt that went"
        );
        spent(&core.store);
        core.scheduled = None;
        core.store
            .update_turn(&{
                let mut turn = core.store.turns("t").unwrap().into_iter().next().unwrap();
                (turn.done, turn.settled, turn.status) = (true, true, "finished".into());
                turn
            })
            .unwrap();
        core.send_as("t", "And now?", &[], None).unwrap();
        assert_eq!(spend(&core.store, "c"), Ok(()), "yours that went");
    }

    #[test]
    fn archiving_wakes_a_vm_only_as_it_may() {
        let limits = |left: i64| {
            let raw =
                serde_json::json!({"canStart": true, "starts": {"hour": {"remaining": left}}});
            let mut l: crate::boat::Limits = serde_json::from_value(raw.clone()).unwrap();
            l.raw = raw;
            Some(l)
        };
        assert!(Waking::Yes.allows(|| panic!("boat isn't asked")));
        assert!(!Waking::No.allows(|| panic!("boat isn't asked")));
        assert!(Waking::IfStarts.allows(|| limits(crate::boat::START_RESERVE)));
        // Scheduled starts through the gate: those taken and not yet counted by boat count.
        let gate = std::sync::Arc::new(StartGate::default());
        let first = gate
            .take(|| limits(crate::boat::START_RESERVE + 1))
            .expect("one to spare");
        let second = gate
            .take(|| limits(crate::boat::START_RESERVE + 1))
            .expect("and another");
        assert!(
            gate.take(|| limits(crate::boat::START_RESERVE + 1))
                .is_none(),
            "the reserve"
        );
        drop((first, second));
        assert!(gate.take(|| limits(crate::boat::START_RESERVE)).is_some());
        assert!(gate.take(|| None).is_none(), "boat not asked: none");
        // Starts that aren't counted: never short of them.
        let unlimited = || {
            let raw = serde_json::json!({"canStart": true, "starts": {"unlimited": true, "hour": {"used": 1, "limit": 60}}});
            let mut l: crate::boat::Limits = serde_json::from_value(raw.clone()).unwrap();
            l.raw = raw;
            Some(l)
        };
        let taken: Vec<_> = (0..20)
            .map(|_| gate.take(unlimited).expect("unlimited"))
            .collect();
        assert!(Waking::IfStarts.allows(unlimited));
        drop(taken);
        assert!(!Waking::IfStarts.allows(|| limits(crate::boat::START_RESERVE - 1)));
        // boat couldn't be asked.
        assert!(!Waking::IfStarts.allows(|| None));
    }

    #[test]
    fn a_refused_first_prompt_says_why_plainly() {
        let refused = |code: &str, message: &str| {
            Error::Boat(crate::error::BoatError::Api(Box::new(
                crate::error::ApiError {
                    status: 409,
                    code: code.into(),
                    message: message.into(),
                    details: serde_json::Value::Null,
                    request_id: Some("req_1".into()),
                    retry_after_seconds: None,
                },
            )))
        };
        assert_eq!(
            first_prompt_refused(&refused(
                "model_not_available",
                "gpt-5.4-mini needs an OpenAI API key"
            )),
            "gpt-5.4-mini needs an OpenAI API key. Add its key in boat's dashboard, or start a thread with another model."
        );
        assert_eq!(
            first_prompt_refused(&refused("busy", "The VM is busy.")),
            "The VM is busy."
        );
        assert_eq!(first_prompt_refused(&Error::Invalid("no".into())), "no");
    }
    use super::*;

    #[test]
    fn model_branch_names_are_cleaned_or_refused() {
        assert_eq!(
            model_branch("add-retry-jitter").as_deref(),
            Some("add-retry-jitter")
        );
        assert_eq!(
            model_branch("`feat/Add-Retry-Jitter`\nsome sign-off").as_deref(),
            Some("add-retry-jitter")
        );
        assert_eq!(
            model_branch("\"fix login timeout\"").as_deref(),
            Some("fix-login-timeout")
        );
        assert_eq!(model_branch(""), None);
        assert_eq!(model_branch("---"), None);
        assert_eq!(
            model_branch("Sure! Here is a good name for the branch you asked about"),
            None
        );
        // The prefix and uniqueness apply as to first-words names.
        assert_eq!(
            unique_branch(
                "add-retry-jitter",
                "flotilla/",
                &["flotilla/add-retry-jitter".into()]
            ),
            "flotilla/add-retry-jitter-2"
        );
    }

    #[test]
    fn a_fresh_forks_disk_is_waited_for() {
        assert!(disk_filling(
            "fatal: detected dubious ownership in repository at '/home/user/flotilla-playground'"
        ));
        assert!(disk_filling(
            "fatal: not a git repository (or any of the parent directories): .git"
        ));
        assert!(!disk_filling("fatal: couldn't find remote ref main"));
    }

    #[test]
    fn branch_names_come_from_the_prompt() {
        assert_eq!(
            branch_name(
                "Add retry to the webhook sender, at most 5 tries",
                "",
                None,
                &[]
            ),
            "add-retry-to-the-webhook-sender"
        );
        assert_eq!(
            branch_name("Fix: login (flaky!)", "", None, &["fix-login-flaky".into()]),
            "fix-login-flaky-2"
        );
        assert_eq!(branch_name("¿¡!", "", None, &[]), "thread");
    }

    #[test]
    fn branch_prefixes_are_cleaned_and_count_for_uniqueness() {
        assert_eq!(
            branch_name("Add search", "flotilla/", None, &[]),
            "flotilla/add-search"
        );
        // An unprefixed branch of the same name isn't a clash; a prefixed one is.
        assert_eq!(
            branch_name("Add search", "flotilla/", None, &["add-search".into()]),
            "flotilla/add-search"
        );
        assert_eq!(
            branch_name(
                "Add search",
                "flotilla/",
                None,
                &["flotilla/add-search".into()]
            ),
            "flotilla/add-search-2"
        );
    }

    #[test]
    fn existing_branches_are_checked_out_tracking_github() {
        assert_eq!(
            checkout_existing("fix-login", "main"),
            "git fetch -q origin 'main'\n\
             git fetch -q origin '+refs/heads/fix-login:refs/remotes/origin/fix-login' || { echo 'fix-login isn'\\''t on GitHub' >&2; exit 1; }\n\
             git switch -q -C 'fix-login' --track origin/'fix-login'\n\
             head=$(git merge-base origin/'main' HEAD)\n"
        );
    }

    #[test]
    fn a_thread_on_an_existing_branch_keeps_its_name_and_base() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../m0-samples/provider-models.json"
        ))
        .unwrap();
        let catalog =
            crate::catalog::Catalog::from_value(&serde_json::from_str(&raw).unwrap()).unwrap();
        let store = Store::open_in_memory().unwrap();
        let env = crate::boat::Environment {
            id: "env1".into(),
            name: "flotilla-web".into(),
            is_default: false,
        };
        store.upsert_environment(&env).unwrap();
        let project = Project {
            id: "p1".into(),
            owner: "acme".into(),
            repo: "web".into(),
            default_branch: "main".into(),
            environment_name: "flotilla-web".into(),
            created_at: now_iso(),
            template_sandbox_id: None,
            template_sha: None,
            template_built_at: None,
            prev_template_sandbox_id: None,
            head_sha: None,
            head_etag: None,
            checked_at: None,
            machine_type: None,
        };
        store.insert_project(&project, &[]).unwrap();
        let req = NewThread {
            environment: "flotilla-web".into(),
            project_id: Some("p1".into()),
            provider: "claude-code".into(),
            model: "sonnet".into(),
            effort: None,
            fast: false,
            title: None,
            machine_type: None,
            prompt: "Fix the failing checks".into(),
            attachments: vec![crate::attach::Attached {
                path: "/tmp/ci.log".into(),
                name: "ci.log".into(),
                size: 120,
                kind: crate::attach::Kind::Text,
                blob: String::new(),
                from: None,
            }],
            wait_ready: false,
            on_branch: Some(OnBranch {
                name: "release/2.0".into(),
                base: "next".into(),
            }),
            id: None,
            parent: None,
            branch_tag: None,
        };
        let (t, _) = begin_thread(&store, &catalog, &req).unwrap();
        assert_eq!(t.branch.as_deref(), Some("release/2.0"));
        // A thread started here: its agent's requests count however long Flotilla was closed.
        assert_eq!(
            crate::orchestrate::requests_since(&store, &t.id, Some(500)),
            Some(0)
        );
        // One this database first saw later (rebuilt): only requests after the history it found.
        assert_eq!(
            crate::orchestrate::requests_since(&store, "seen-later", None),
            None
        );
        assert_eq!(
            crate::orchestrate::requests_since(&store, "seen-later", Some(500)),
            Some(501)
        );
        assert_eq!(
            crate::orchestrate::requests_since(&store, "seen-later", Some(900)),
            Some(501)
        );
        assert_eq!(base_branch(&store, &t.id, "main"), "next");
        // The first prompt shows its files while it's sending.
        assert_eq!(store.messages(&t.id).unwrap()[0].attachments, ["ci.log"]);
        // Try again after a start that never got a VM starts on the same branch, with the same files.
        let (again, _) = retry_request(&store, &t.id).unwrap();
        assert_eq!(again.on_branch, req.on_branch);
        assert_eq!(again.attachments, req.attachments);
        assert_eq!(again.machine_type, None);
        // A size picked for the thread is asked for again too.
        let (large, _) = begin_thread(
            &store,
            &catalog,
            &NewThread {
                on_branch: Some(OnBranch {
                    name: "bigger".into(),
                    base: "main".into(),
                }),
                machine_type: Some("large".into()),
                ..req.clone()
            },
        )
        .unwrap();
        let (again, _) = retry_request(&store, &large.id).unwrap();
        assert_eq!(again.machine_type.as_deref(), Some("large"));

        let (fresh, _) = begin_thread(
            &store,
            &catalog,
            &NewThread {
                on_branch: None,
                ..req.clone()
            },
        )
        .unwrap();
        assert_eq!(fresh.branch.as_deref(), Some("fix-the-failing-checks"));
        assert_eq!(base_branch(&store, &fresh.id, "main"), "main");

        // One prompt started on two models: each branch carries its model.
        let fan = |tag: &str| NewThread {
            on_branch: None,
            attachments: vec![],
            branch_tag: Some(tag.into()),
            ..req.clone()
        };
        let (opus, _) = begin_thread(&store, &catalog, &fan("Opus 5.5")).unwrap();
        let (terra, _) = begin_thread(&store, &catalog, &fan("GPT-6.1 Terra")).unwrap();
        assert_eq!(
            opus.branch.as_deref(),
            Some("fix-the-failing-checks-opus-5-5")
        );
        assert_eq!(
            terra.branch.as_deref(),
            Some("fix-the-failing-checks-gpt-6-1-terra")
        );
        // The same again (another fan-out of the same prompt): numbered, as any clash is.
        let (again, _) = begin_thread(&store, &catalog, &fan("Opus 5.5")).unwrap();
        assert_eq!(
            again.branch.as_deref(),
            Some("fix-the-failing-checks-opus-5-5-2")
        );
        assert_eq!(
            store.thread(&again.id).unwrap().unwrap().branch,
            again.branch
        );
    }

    #[test]
    fn threads_started_together_get_branches_that_name_their_models() {
        assert_eq!(
            branch_name("Fix the retry", "", Some("Opus 5.5"), &[]),
            "fix-the-retry-opus-5-5"
        );
        assert_eq!(
            branch_name("Fix the retry", "flotilla/", Some("GPT-6.1 Terra"), &[]),
            "flotilla/fix-the-retry-gpt-6-1-terra"
        );
        assert_eq!(
            branch_name(
                "Fix the retry",
                "",
                Some("Opus 5.5"),
                &["fix-the-retry-opus-5-5".into()]
            ),
            "fix-the-retry-opus-5-5-2"
        );
        // Nothing usable in the tag: the words alone.
        assert_eq!(
            branch_name("Fix the retry", "", Some("¿¡!"), &[]),
            "fix-the-retry"
        );
        // The words already end with it: not twice.
        assert_eq!(
            branch_name("Compare with opus", "", Some("Opus"), &[]),
            "compare-with-opus"
        );
        // A long model name is cut at a word, within 24 characters.
        assert_eq!(
            branch_name(
                "Fix",
                "",
                Some("Claude Sonnet 5.5 Extended Thinking Preview"),
                &[]
            ),
            "fix-claude-sonnet-5-5"
        );
        // The model's own branch name (the preview model's) carries it the same way.
        assert_eq!(
            tagged("retry-backoff".into(), Some("Haiku 5.5")),
            "retry-backoff-haiku-5-5"
        );
    }

    #[test]
    fn branches_claimed_at_the_same_moment_never_clash() {
        // Several connections (as each start's own thread has) claim one name at once.
        let dir = std::env::temp_dir().join(format!("flotilla-claim-{}", uuid::Uuid::new_v4()));
        let db = dir.join("flotilla.db");
        let store = Store::open(&db).unwrap();
        store
            .upsert_environment(&crate::boat::Environment {
                id: "env1".into(),
                name: "flotilla-web".into(),
                is_default: false,
            })
            .unwrap();
        let ids: Vec<String> = (0..6).map(|i| format!("t{i}")).collect();
        for id in &ids {
            store
                .insert_thread(&Thread {
                    id: id.clone(),
                    environment_id: Some("env1".into()),
                    environment_name: Some("flotilla-web".into()),
                    title: "Fix the retry".into(),
                    sandbox_id: None,
                    conversation_id: None,
                    provider: "claude-code".into(),
                    model: "sonnet".into(),
                    effort: None,
                    fast: false,
                    state: ThreadState::Creating,
                    keep_running: false,
                    archive_after: None,
                    last_stopped_at: None,
                    data_expired_at: None,
                    created_at: now_iso(),
                    updated_at: now_iso(),
                    repo_project_id: Some("p1".into()),
                    branch: Some("fix-the-retry".into()),
                    base_sha: None,
                })
                .unwrap();
        }
        let names: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = ids
                .iter()
                .map(|id| {
                    let db = db.clone();
                    s.spawn(move || {
                        Store::open(&db)
                            .unwrap()
                            .claim_branch(id, "p1", |taken| {
                                unique_branch("fix-the-retry", "", taken)
                            })
                            .unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut all: Vec<String> = ids
            .iter()
            .map(|id| store.thread(id).unwrap().unwrap().branch.unwrap())
            .collect();
        assert_eq!(
            names
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            6,
            "{names:?}"
        );
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 6, "{all:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A boat for one thread's VM, `bx_t`: each call (`METHOD /path`) is kept and handed to
    /// `hook` (with the VM's state, to change) before it's answered (as boat takes it in); the
    /// VM is idle until it's stopped, and idle again once resumed. A stopped VM takes no
    /// command or prompt.
    fn hooked_boat(
        hook: impl Fn(&str, &std::sync::Mutex<String>) + Send + Sync + 'static,
    ) -> (BoatClient, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let calls: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
        let (kept, hook) = (calls.clone(), std::sync::Arc::new(hook));
        let state = std::sync::Arc::new(std::sync::Mutex::new("idle".to_string()));
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let (kept, hook, state) = (kept.clone(), hook.clone(), state.clone());
                std::thread::spawn(move || {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 16384];
                    loop {
                        let n = conn.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        req.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&req).to_string();
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            let len = head
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                                })
                                .unwrap_or(0);
                            if body.len() >= len {
                                break;
                            }
                        }
                    }
                    let text = String::from_utf8_lossy(&req).to_string();
                    let mut words = text.split_whitespace();
                    let (method, path) = (
                        words.next().unwrap_or("").to_string(),
                        words.next().unwrap_or("").to_string(),
                    );
                    let path = path
                        .strip_prefix("/api/v1")
                        .unwrap_or(&path)
                        .split('?')
                        .next()
                        .unwrap_or("")
                        .to_string();
                    let call = format!("{method} {path}");
                    kept.lock().unwrap().push(call.clone());
                    hook(&call, &state);
                    let vm = |state: &str| serde_json::json!({"id": "bx_t", "state": state});
                    let stopped = *state.lock().unwrap() == "archived";
                    let down = *state.lock().unwrap() == "down";
                    let body = match (method.as_str(), path.as_str()) {
                        _ if down => {
                            serde_json::json!({"ok": false, "error": {"code": "internal", "message": "boat is down", "status": 500}})
                        }
                        ("POST", "/sandboxes/bx_t/commands" | "/sandboxes/bx_t/prompt")
                        | ("GET", "/sandboxes/bx_t/commands/7")
                            if stopped =>
                        {
                            serde_json::json!({"ok": false, "error": {"code": "machine_not_running", "message": "Sandbox bx_t is stopped.", "status": 400}})
                        }
                        ("GET", "/limits") => {
                            serde_json::json!({"canStart": true, "starts": {"hour": {"limit": 60, "remaining": 50}}})
                        }
                        ("POST", "/sandboxes") => vm("idle"),
                        ("GET" | "PATCH", "/sandboxes/bx_t") => vm(&state.lock().unwrap()),
                        ("POST", "/sandboxes/bx_t/stop") => {
                            *state.lock().unwrap() = "archived".into();
                            serde_json::json!({})
                        }
                        ("POST", "/sandboxes/bx_t/resume") => {
                            *state.lock().unwrap() = "idle".into();
                            serde_json::json!({})
                        }
                        // One run in the background is done at once.
                        ("POST", "/sandboxes/bx_t/commands")
                            if text.contains("\"detached\":true") =>
                        {
                            serde_json::json!({"processId": 7})
                        }
                        ("GET", "/sandboxes/bx_t/commands/7") => {
                            serde_json::json!({"running": false, "exitCode": 0, "stdout": "", "stderr": ""})
                        }
                        ("POST", "/sandboxes/bx_t/commands") => {
                            serde_json::json!({"exitCode": 0, "stdout": "", "stderr": ""})
                        }
                        ("POST", "/sandboxes/bx_t/prompt") => {
                            serde_json::json!({"promptId": "pr1", "conversationId": "cv1", "status": "queued"})
                        }
                        _ => {
                            serde_json::json!({"ok": false, "error": {"code": "not_found", "message": "no such path", "status": 404}})
                        }
                    };
                    let status = match body["error"]["status"].as_u64() {
                        Some(400) => "400 Bad Request",
                        Some(500) => "500 Internal Server Error",
                        Some(_) => "404 Not Found",
                        None => "200 OK",
                    };
                    let body = body.to_string();
                    let _ = write!(
                        conn,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        (
            BoatClient::with_base(
                "boat_test".into(),
                &base,
                &format!("{base}/provider-models"),
            )
            .unwrap(),
            calls,
        )
    }

    /// A thread begun as the host begins one (its first prompt showing, unsent), in a database
    /// on disk so another connection (the host's) can archive it meanwhile.
    fn begun(name: &str, wait_ready: bool) -> (PathBuf, NewThread, Thread, String) {
        begun_in(name, wait_ready, None)
    }

    /// `begun`, in a project (one with no prepared VM: a fresh clone) if given.
    fn begun_in(
        name: &str,
        wait_ready: bool,
        project: Option<&str>,
    ) -> (PathBuf, NewThread, Thread, String) {
        let dir = std::env::temp_dir().join(format!(
            "flotilla-archived-start-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        let store = Store::open(&db).unwrap();
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../m0-samples/provider-models.json"
        ))
        .unwrap();
        let catalog =
            crate::catalog::Catalog::from_value(&serde_json::from_str(&raw).unwrap()).unwrap();
        store
            .upsert_environment(&crate::boat::Environment {
                id: "env1".into(),
                name: "base".into(),
                is_default: true,
            })
            .unwrap();
        if let Some(id) = project {
            let p = Project {
                id: id.into(),
                owner: "acme".into(),
                repo: "web".into(),
                default_branch: "main".into(),
                environment_name: "base".into(),
                created_at: now_iso(),
                template_sandbox_id: None,
                template_sha: None,
                template_built_at: None,
                prev_template_sandbox_id: None,
                head_sha: None,
                head_etag: None,
                checked_at: None,
                machine_type: None,
            };
            store.insert_project(&p, &[]).unwrap();
        }
        let req = NewThread {
            environment: "base".into(),
            project_id: project.map(str::to_string),
            provider: "claude-code".into(),
            model: "sonnet".into(),
            effort: None,
            fast: false,
            title: None,
            machine_type: None,
            prompt: "Review the retry logic".into(),
            attachments: vec![],
            wait_ready,
            on_branch: None,
            id: Some(uuid::Uuid::new_v4().to_string()),
            parent: None,
            branch_tag: None,
        };
        let (t, local) = begin_thread(&store, &catalog, &req).unwrap();
        (db, req, t, local)
    }

    /// Stopped by the user as it started (once boat made its VM, or while its repo is set up):
    /// the first prompt doesn't start the VM again, nor is it sent, and the stop stays the
    /// user's. The thread keeps "Stopped before it started." with Try again, which (the stop
    /// ended as Try again is asked, as the host does) wakes the VM and sends it.
    #[test]
    fn a_thread_stopped_as_it_starts_stays_stopped() {
        for (name, project, at) in [
            ("made", None, "POST /sandboxes"),
            ("checkout", Some("p1"), "POST /sandboxes/bx_t/commands"),
        ] {
            let (db, req, t, local) = begun_in(name, false, project);
            let host = std::sync::Mutex::new(Store::open(&db).unwrap());
            let id = t.id.clone();
            let once = std::sync::atomic::AtomicBool::new(false);
            // The user's Stop, as `stop` does it: marked, then boat stops the VM.
            let (boat, calls) = hooked_boat(move |call, state| {
                if call == at && !once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    host.lock()
                        .unwrap()
                        .kv_set(&stopped_by_user_key(&id), "stopping -")
                        .unwrap();
                    *state.lock().unwrap() = "archived".into();
                }
            });
            let mut core = Core::new(boat, Store::open(&db).unwrap());
            let e = core.launch_thread(&t.id, &req, Some(&local)).unwrap_err();
            assert_eq!(e.plain(), START_STOPPED, "{name}");
            let made = calls.lock().unwrap().clone();
            assert!(
                !made
                    .iter()
                    .any(|c| c.ends_with("/resume") || c.ends_with("/prompt")),
                "{name}: {made:?}"
            );
            assert!(
                stopped_by_user(&core.store, &t.id),
                "{name}: the stop is still the user's"
            );
            assert_eq!(
                core.store.start_error(&t.id).unwrap().as_deref(),
                Some(STOPPED_AS_IT_STARTED),
                "{name}"
            );
            assert_eq!(core.store.start_stages(&t.id).unwrap(), None);
            assert!(
                core.store
                    .sent_prompts(&t.id)
                    .unwrap()
                    .iter()
                    .any(|p| p.id == local),
                "{name}: the prompt is kept unsent"
            );
            // Try again while it's still stopped by the user sends nothing; once the stop is
            // ended (as the host's Try again ends it), it wakes the VM and sends the prompt.
            assert_eq!(
                core.retry_thread(&t.id).unwrap_err().plain(),
                START_STOPPED,
                "{name}"
            );
            assert!(
                !calls.lock().unwrap()[made.len()..]
                    .iter()
                    .any(|c| c.ends_with("/resume") || c.ends_with("/prompt")),
                "{name}"
            );
            core.store.kv_delete(&stopped_by_user_key(&t.id)).unwrap();
            core.retry_thread(&t.id).unwrap();
            let calls = calls.lock().unwrap()[made.len()..].to_vec();
            assert!(
                calls.iter().any(|c| c == "POST /sandboxes/bx_t/resume")
                    && calls.iter().any(|c| c == "POST /sandboxes/bx_t/prompt"),
                "{name}: {calls:?}"
            );
            assert_eq!(core.store.start_error(&t.id).unwrap(), None);
            let _ = std::fs::remove_dir_all(db.parent().unwrap());
        }
    }

    /// Archived while boat makes its VM: a start of yours, a delegated thread's, or a scheduled
    /// one doesn't bring it back or send its first prompt (the archive's own action, after it,
    /// stops the VM). Archived before the VM is asked for, none is made; archived once it's
    /// there, the first prompt still doesn't go. Try again brings it back and sends it. What
    /// could race an archive (making the VM, waking it, the first prompt) goes in the thread's
    /// turn.
    #[test]
    fn a_thread_archived_as_it_starts_stays_put_away() {
        for scheduled in [false, true] {
            let (db, req, t, local) = begun("create", false);
            let host = std::sync::Mutex::new(Store::open(&db).unwrap());
            let id = t.id.clone();
            let turns: std::sync::Arc<std::sync::Mutex<()>> = Default::default();
            let (held, in_turn) = (
                turns.clone(),
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            );
            let seen = in_turn.clone();
            let (boat, log) = hooked_boat(move |call, _| {
                if call == "POST /sandboxes" {
                    host.lock().unwrap().set_archived(&id, true).unwrap();
                }
                if [
                    "POST /sandboxes",
                    "POST /sandboxes/bx_t/resume",
                    "POST /sandboxes/bx_t/prompt",
                ]
                .contains(&call)
                {
                    seen.lock()
                        .unwrap()
                        .push((call.to_string(), held.try_lock().is_err()));
                }
            });
            let mut core = Core::new(boat, Store::open(&db).unwrap());
            if scheduled {
                core.scheduled = Some(std::sync::Arc::default());
            } else {
                core.turns = Some(turns.clone());
            }
            let e = core.launch_thread(&t.id, &req, Some(&local)).unwrap_err();
            assert_eq!(e.plain(), SCHEDULED_ARCHIVED, "scheduled: {scheduled}");
            let calls = log.lock().unwrap().clone();
            assert!(
                core.store.is_archived(&t.id).unwrap(),
                "scheduled: {scheduled}"
            );
            assert!(
                !calls
                    .iter()
                    .any(|c| c.ends_with("/prompt") || c.ends_with("/resume")),
                "scheduled: {scheduled}: {calls:?}"
            );
            let after = core.store.thread(&t.id).unwrap().unwrap();
            assert_eq!(after.sandbox_id.as_deref(), Some("bx_t"));
            assert_eq!(
                core.store.start_error(&t.id).unwrap().as_deref(),
                Some(ARCHIVED_AS_IT_STARTED)
            );
            assert_eq!(core.store.start_stages(&t.id).unwrap(), None);
            // Its VM stopped by the archive: Try again while it's still archived (archived after
            // Try again was asked) sends nothing and wakes nothing; back from Archived, its stop
            // ended (as the host's Try again does both), it starts the VM and sends the first
            // prompt.
            if !scheduled {
                core.stop(&t.id, false).unwrap();
                assert_eq!(
                    core.retry_thread(&t.id).unwrap_err().plain(),
                    SCHEDULED_ARCHIVED
                );
                let since = log.lock().unwrap()[calls.len()..].to_vec();
                assert_eq!(since.len(), 2, "a stop's look and the stop: {since:?}");
                core.store.set_archived(&t.id, false).unwrap();
                core.store.kv_delete(&stopped_by_user_key(&t.id)).unwrap();
                core.retry_thread(&t.id).unwrap();
                let calls = log.lock().unwrap()[calls.len()..].to_vec();
                assert!(
                    calls.iter().any(|c| c == "POST /sandboxes/bx_t/resume")
                        && calls.iter().any(|c| c == "POST /sandboxes/bx_t/prompt"),
                    "{calls:?}"
                );
                assert_eq!(core.store.start_error(&t.id).unwrap(), None);
                assert!(!core.store.is_archived(&t.id).unwrap());
                let in_turn = in_turn.lock().unwrap().clone();
                assert_eq!(in_turn.len(), 3, "{in_turn:?}");
                assert!(in_turn.iter().all(|(_, held)| *held), "{in_turn:?}");
                assert!(turns.try_lock().is_ok(), "let go after");
            }
            let _ = std::fs::remove_dir_all(db.parent().unwrap());
        }

        // Archived as boat fails before a VM is asked for: put down to the archive too.
        let (db, req, t, local) = begun("limits", false);
        let host = std::sync::Mutex::new(Store::open(&db).unwrap());
        let id = t.id.clone();
        let (boat, _) = hooked_boat(move |call, state| {
            if call == "GET /limits" {
                host.lock().unwrap().set_archived(&id, true).unwrap();
                *state.lock().unwrap() = "down".into();
            }
        });
        let mut core = Core::new(boat, Store::open(&db).unwrap());
        assert_eq!(
            core.launch_thread(&t.id, &req, Some(&local))
                .unwrap_err()
                .plain(),
            SCHEDULED_ARCHIVED
        );
        assert_eq!(
            core.store.start_error(&t.id).unwrap().as_deref(),
            Some(ARCHIVED_AS_IT_STARTED)
        );
        assert_eq!(
            core.store.thread(&t.id).unwrap().unwrap().state,
            ThreadState::Error
        );
        let _ = std::fs::remove_dir_all(db.parent().unwrap());

        // Archived before boat was asked: no VM, its start failed (Try again, once it's back
        // from Archived).
        let (db, req, t, local) = begun("before", false);
        let (boat, calls) = hooked_boat(|_, _| {});
        let mut core = Core::new(boat, Store::open(&db).unwrap());
        core.store.set_archived(&t.id, true).unwrap();
        assert_eq!(
            core.launch_thread(&t.id, &req, Some(&local))
                .unwrap_err()
                .plain(),
            SCHEDULED_ARCHIVED
        );
        let made = calls.lock().unwrap().clone();
        assert!(!made.iter().any(|c| c == "POST /sandboxes"), "{made:?}");
        assert_eq!(
            core.store.thread(&t.id).unwrap().unwrap().state,
            ThreadState::Error
        );
        assert_eq!(
            core.store.start_error(&t.id).unwrap().as_deref(),
            Some(ARCHIVED_AS_IT_STARTED)
        );
        core.store.set_archived(&t.id, false).unwrap();
        core.retry_thread(&t.id).unwrap();
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| c == "POST /sandboxes/bx_t/prompt")
        );
        let _ = std::fs::remove_dir_all(db.parent().unwrap());

        // Archived once the VM is there, while it comes up: the archive (which sees the VM)
        // stops it; the first prompt never brings the thread back.
        let (db, req, t, local) = begun("waiting", true);
        let host = std::sync::Mutex::new(Store::open(&db).unwrap());
        let id = t.id.clone();
        let (boat, calls) = hooked_boat(move |call, _| {
            if call == "GET /sandboxes/bx_t" {
                host.lock().unwrap().set_archived(&id, true).unwrap();
            }
        });
        let mut core = Core::new(boat, Store::open(&db).unwrap());
        assert_eq!(
            core.launch_thread(&t.id, &req, Some(&local))
                .unwrap_err()
                .plain(),
            SCHEDULED_ARCHIVED
        );
        assert!(core.store.is_archived(&t.id).unwrap());
        let sent = calls.lock().unwrap().clone();
        assert!(!sent.iter().any(|c| c.ends_with("/prompt")), "{sent:?}");
        assert_eq!(
            core.store.start_error(&t.id).unwrap().as_deref(),
            Some(ARCHIVED_AS_IT_STARTED)
        );
        let _ = std::fs::remove_dir_all(db.parent().unwrap());

        // A project thread archived while its repo is set up (as it's put on its branch, or
        // while a setup script runs in the background): the archive stops the VM under it,
        // and what fails is put down to the archive. Try again, back from Archived, wakes the
        // VM, sets the repo up and sends the prompt.
        for (name, at) in [
            ("checkout", "POST /sandboxes/bx_t/commands"),
            ("script", "GET /sandboxes/bx_t/commands/7"),
        ] {
            let (db, req, t, local) = begun_in(name, false, Some("p1"));
            let host = std::sync::Mutex::new(Store::open(&db).unwrap());
            host.lock()
                .unwrap()
                .set_scripts("p1", &[crate::github::SetupScript::every("make deps")])
                .unwrap();
            let id = t.id.clone();
            // The script's look is the second (the checkout's is the first).
            let seen = std::sync::atomic::AtomicUsize::new(0);
            let nth = if name == "script" { 2 } else { 1 };
            let (boat, calls) = hooked_boat(move |call, state| {
                if call == at && seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == nth {
                    host.lock().unwrap().set_archived(&id, true).unwrap();
                    *state.lock().unwrap() = "archived".into();
                }
            });
            let mut core = Core::new(boat, Store::open(&db).unwrap());
            assert_eq!(
                core.launch_thread(&t.id, &req, Some(&local))
                    .unwrap_err()
                    .plain(),
                SCHEDULED_ARCHIVED,
                "{name}"
            );
            assert!(core.store.is_archived(&t.id).unwrap());
            assert_eq!(
                core.store.start_error(&t.id).unwrap().as_deref(),
                Some(ARCHIVED_AS_IT_STARTED),
                "{name}"
            );
            assert_eq!(core.store.start_stages(&t.id).unwrap(), None);
            assert!(
                !calls.lock().unwrap().iter().any(|c| c.ends_with("/prompt")),
                "{name}"
            );
            core.store.set_archived(&t.id, false).unwrap();
            core.retry_thread(&t.id).unwrap();
            let calls = calls.lock().unwrap().clone();
            assert!(
                calls.iter().any(|c| c == "POST /sandboxes/bx_t/resume")
                    && calls.iter().any(|c| c == "POST /sandboxes/bx_t/prompt"),
                "{name}: {calls:?}"
            );
            assert_eq!(core.store.start_error(&t.id).unwrap(), None);
            let _ = std::fs::remove_dir_all(db.parent().unwrap());
        }
    }
}
