//! Views: what a client shows, computed where the engine runs. Each replaces one group of the
//! UI's old SQLite reads. A view is sent whole when what it shows changes; stage 2 diffs them
//! (a thread's messages by `seq`, every other field replaced whole).

use crate::history::PageMark;
use crate::{
    AccountLimits, ContextUse, Environment, GhStatus, HookRun, Lineage, Message, Project,
    ScriptFailure, SentPrompt, SetupRun, SetupScript, StartStages, Thread, ThreadServices,
    ThreadSettings, ThreadSummary, Usage, UserSkill,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How syncing with boat is going.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    pub error: Option<String>,
    pub syncing: bool,
}

/// Whether the engine (boat polling, actions, GitHub) is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    Running,
    /// Dev: no engine, the database as it is.
    Offline,
    /// Not running: why (no boat key, or it couldn't start).
    Stopped(String),
}

/// The sidebar, and what the window around it needs: every thread's row, who handed whom
/// work, the projects and environments, and how syncing, GitHub and the account are doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidebar {
    /// Every thread, newest activity first.
    pub threads: Vec<ThreadSummary>,
    /// Threads handed their task by another: child id → parent id.
    pub parents: BTreeMap<String, String>,
    /// Threads waiting on an answer from a thread they handed work to.
    pub waiting: Vec<String>,
    pub projects: Vec<Project>,
    /// The boat environments a thread with No project can start in.
    pub environments: Vec<Environment>,
    pub status: SyncStatus,
    pub github: GhStatus,
    /// The account's VMs and starts against its plan's limits, once read.
    pub usage: Option<Usage>,
    #[serde(deserialize_with = "engine_or_offline")]
    pub engine: EngineState,
    /// The agent accounts' usage limits (Claude's, ChatGPT's), as last read in a VM.
    #[serde(default)]
    pub limits: Vec<AccountLimits>,
}

/// One thread: everything its page shows. `thread` is None when there's no such thread (it
/// was deleted, or a `thread.new` hasn't written it yet).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadDetail {
    pub id: String,
    pub thread: Option<Thread>,
    /// Its newest messages, in `seq` order, from seq `older` on: whole pages
    /// (`crate::history`), at least `WINDOW` of them. Tool inputs and results are cut to `CAP`.
    pub messages: Vec<Message>,
    /// How many messages come before these (a multiple of `PAGE`): older pages are fetched
    /// over HTTP as they're scrolled to.
    #[serde(default)]
    pub older: i64,
    /// Every page's mark, oldest first: a copy of a page is good while its mark matches.
    #[serde(default)]
    pub pages: Vec<PageMark>,
    /// What was typed and sent here (still-sending placeholders too), with its files.
    pub sent: Vec<SentPrompt>,
    /// Why its start failed, when it has no VM.
    pub start_error: Option<String>,
    /// A starting project thread's progress.
    pub stages: Option<StartStages>,
    /// A project thread's project.
    pub project: Option<ThreadProject>,
    /// The repo's `.agents/resume` and `.agents/archive` from this VM's last wake or archive.
    pub resume: Option<HookRun>,
    pub archive: Option<HookRun>,
    /// The setup script that failed when it started.
    pub setup_failure: Option<ScriptFailure>,
    /// Its services (`.agents/services.yml`) as last seen.
    pub services: Option<ThreadServices>,
    /// Its ports whose links anyone may open now (`thread.make_public`), by port.
    #[serde(default)]
    pub public: Vec<crate::PublicLink>,
    /// Its subagents as its harness recorded them, once read.
    pub lineage: Option<Lineage>,
    /// The thread that handed it its task: (id, title).
    pub parent: Option<(String, String)>,
    /// The threads it handed work to, and the agents it started in its own VM.
    pub children: Vec<ChildCard>,
    /// How full its context is, as its harness last recorded it.
    #[serde(default)]
    pub context: Option<ContextUse>,
}

/// A project thread's project, and where its branch stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadProject {
    pub project: Project,
    /// How many commits its base is behind the default branch (for the base it has now).
    pub behind: Option<i64>,
    /// The branch its work merges into.
    pub into: String,
}

/// A thread another handed work to, or an agent it started in its own VM, as the card in the
/// one that started it shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildCard {
    /// The child thread's id, or the agent's request id.
    pub id: String,
    pub title: String,
    pub provider: String,
    /// The model's id (the catalog names it).
    pub model: String,
    #[serde(deserialize_with = "child_or_working")]
    pub state: ChildState,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildState {
    Starting,
    Working,
    Done,
    Error,
    /// Its VM or disk is gone.
    Gone,
    DidntStart,
    Failed,
    Stopped,
}

impl ChildState {
    /// Starting or at work.
    pub fn running(self) -> bool {
        matches!(self, ChildState::Starting | ChildState::Working)
    }
}

/// One project: its settings page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDetail {
    pub id: String,
    /// None once it's removed.
    pub project: Option<Project>,
    /// Its latest setup runs, newest first.
    pub runs: Vec<SetupRun>,
    pub scripts: Vec<SetupScript>,
    /// Why GitHub couldn't be read for it last time.
    pub github_error: Option<String>,
}

/// Your settings, kept where the engine runs and the same on every device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Defaults for new threads, once chosen.
    pub defaults: Option<ThreadSettings>,
    /// Idle VMs stop after this many minutes.
    pub idle_minutes: u32,
    /// New branches' names start with this ("" for none).
    pub branch_prefix: String,
    /// The model titles and reply previews come from; "" turns them off.
    pub summary_model: String,
    /// Archive a thread when its pull request merges.
    pub archive_on_merge: bool,
    /// Archive threads idle for this many days (0: never).
    pub archive_idle_days: u32,
    #[serde(deserialize_with = "summaries_or_unknown")]
    pub summaries: SummaryStatus,
    /// Your instructions for every agent, and the skills they all get.
    pub instructions: String,
    pub skills: Vec<UserSkill>,
    /// Why the skills last couldn't go to a VM.
    pub skills_error: Option<String>,
    /// Every project, with its latest setup run.
    pub projects: Vec<ProjectRow>,
    /// The database, on the machine that runs the engine.
    pub database: String,
    /// The saved boat key's last four characters (never the key), if one is saved.
    pub boat_key: Option<String>,
    /// What isn't set up where the engine runs, and what to run there; none until it's checked.
    pub doctor: Option<Doctor>,
}

/// What `flotilla doctor` found where the engine runs. It checks at startup, now and then, and
/// when asked (`doctor.run`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doctor {
    /// The checks this version knows: one a newer server added is left out.
    #[serde(deserialize_with = "known_checks")]
    pub checks: Vec<Diagnosis>,
    /// When it checked (ms since the epoch).
    pub at_ms: i64,
}

impl Doctor {
    /// Every check that matters passed (a warning or an optional part that's off is fine).
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.health != Health::Fail)
    }

    /// A check threads can't run without failed: clients show the setup where New thread
    /// would be.
    pub fn stops_threads(&self) -> bool {
        self.checks
            .iter()
            .any(|c| c.health == Health::Fail && c.check.threads_need())
    }

    /// How many checks passed, of those that are on ("5/5"): an optional part that's off
    /// (push, Tailscale, remote MCP) isn't one to pass, so it doesn't make the setup look
    /// unfinished.
    pub fn tally(&self) -> (usize, usize) {
        let on = self.checks.iter().filter(|c| c.health != Health::Off);
        let (ok, of) = on.fold((0, 0), |(ok, of), c| {
            (ok + usize::from(c.health == Health::Ok), of + 1)
        });
        (ok, of)
    }
}

/// One check: how it went, what was found, and the commands that fix it, to run where the
/// engine runs, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnosis {
    pub check: CheckOf,
    #[serde(deserialize_with = "health_or_warn")]
    pub health: Health,
    pub says: String,
    pub fix: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOf {
    BoatKey,
    GitHub,
    Claude,
    Tailscale,
    Database,
    /// Information, never a failure: VMs on the boat account that another Flotilla (a server,
    /// the app on another Mac) made, which this one leaves alone.
    Others,
    /// Notifications to paired phones (Apple's push service): optional, off until its key is
    /// set up.
    Push,
    /// Remote MCP (assistants in other clouds, over HTTPS): optional, off until it's turned
    /// on, and then whether it's public.
    Mcp,
}

impl CheckOf {
    /// Threads can't run while it fails: their VMs need the boat key, and everything they do
    /// is written to the database. The rest leave threads running (GitHub only matters to a
    /// project's, titles, Tailscale, push and remote MCP to none).
    pub fn threads_need(self) -> bool {
        matches!(self, CheckOf::BoatKey | CheckOf::Database)
    }

    pub fn label(self) -> &'static str {
        match self {
            CheckOf::BoatKey => "boat API key",
            CheckOf::GitHub => "GitHub",
            CheckOf::Claude => "Titles and previews",
            CheckOf::Tailscale => "Tailscale",
            CheckOf::Database => "Database",
            CheckOf::Others => "Other Flotillas",
            CheckOf::Push => "Push",
            CheckOf::Mcp => "Remote MCP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    /// Works, but something needs a look (a key expiring soon, boat.dev not answering).
    Warn,
    Fail,
    /// Optional and not set up (Tailscale), or turned off (summaries).
    Off,
}

/// A project and its latest setup run, for Settings' list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRow {
    pub project: Project,
    pub last_run: Option<SetupRun>,
}

/// How the last summary went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryStatus {
    /// None has run yet.
    Unknown,
    /// Last worked at this time (RFC 3339).
    Working(String),
    Failed(String),
}

/// The devices paired with the server, for Settings → Devices. Which of them is this one, a
/// client knows from its own pairing (and `welcome.device`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Devices {
    /// Most recently seen first.
    pub devices: Vec<DeviceRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRow {
    pub id: String,
    pub name: String,
    /// `macos`, `linux`, `ios`…
    pub platform: String,
    pub paired_ms: i64,
    pub last_seen_ms: i64,
    /// It has a connection open now.
    pub connected: bool,
    /// What it may do: an agent's (an assistant through `flotilla mcp`) shows as one.
    #[serde(default)]
    pub scope: crate::Scope,
}

/// Reading what a newer server sends: a value of a kind this version doesn't know (a variant
/// added since) reads as `fallback`, as the iPhone reads it, rather than failing the whole view
/// (which no amount of asking again would fix).
fn or<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned>(
    d: D,
    fallback: T,
) -> Result<T, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or(fallback))
}

/// Not running, with no reason this version can read.
fn engine_or_offline<'de, D: serde::Deserializer<'de>>(d: D) -> Result<EngineState, D::Error> {
    or(d, EngineState::Offline)
}

/// Each as the iPhone reads one it doesn't know.
pub(crate) fn activity_or_idle<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<crate::Activity, D::Error> {
    or(d, crate::Activity::Idle)
}

fn child_or_working<'de, D: serde::Deserializer<'de>>(d: D) -> Result<ChildState, D::Error> {
    or(d, ChildState::Working)
}

fn health_or_warn<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Health, D::Error> {
    or(d, Health::Warn)
}

fn summaries_or_unknown<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SummaryStatus, D::Error> {
    or(d, SummaryStatus::Unknown)
}

fn known_checks<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Diagnosis>, D::Error> {
    let all: Vec<serde_json::Value> = Vec::deserialize(d)?;
    Ok(all
        .into_iter()
        .filter_map(|c| serde_json::from_value(c).ok())
        .collect())
}
