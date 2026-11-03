//! Threads as the views carry them: the thread itself, its sidebar row, its messages and
//! what was sent, and what its VM reports (subagents, services, hooks, a failed setup).

use crate::github::PullRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadState {
    /// The row exists but the sandbox doesn't yet.
    Creating,
    Starting,
    Idle,
    Running,
    Stopping,
    Stopped,
    /// The sandbox or its disk is gone. Read-only.
    Expired,
    /// Also a state this version doesn't know (from a newer server), as the iPhone reads it.
    #[serde(other)]
    Error,
}

impl ThreadState {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadState::Creating => "creating",
            ThreadState::Starting => "starting",
            ThreadState::Idle => "idle",
            ThreadState::Running => "running",
            ThreadState::Stopping => "stopping",
            ThreadState::Stopped => "stopped",
            ThreadState::Expired => "expired",
            ThreadState::Error => "error",
        }
    }

    pub fn parse(s: &str) -> ThreadState {
        match s {
            "creating" => ThreadState::Creating,
            "starting" => ThreadState::Starting,
            "idle" => ThreadState::Idle,
            "running" => ThreadState::Running,
            "stopping" => ThreadState::Stopping,
            "stopped" => ThreadState::Stopped,
            "expired" => ThreadState::Expired,
            _ => ThreadState::Error,
        }
    }
}

/// A boat environment (what a sandbox starts with). Threads with No project use the default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub environment_id: Option<String>,
    pub environment_name: Option<String>,
    pub title: String,
    pub sandbox_id: Option<String>,
    pub conversation_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub fast: bool,
    pub state: ThreadState,
    pub keep_running: bool,
    pub archive_after: Option<String>,
    pub last_stopped_at: Option<String>,
    pub data_expired_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// The project (GitHub repo) it was started in; `None`: No project.
    pub repo_project_id: Option<String>,
    /// Its own branch in the repo.
    pub branch: Option<String>,
    /// The default branch's commit it started from.
    pub base_sha: Option<String>,
}

impl Thread {
    /// No VM to prompt: it expired, or the start failed before one was made. Asks for the
    /// agent go to a new thread on the branch instead.
    pub fn vm_gone(&self) -> bool {
        self.state == ThreadState::Expired
            || (self.state == ThreadState::Error && self.sandbox_id.is_none())
    }
}

/// A prompt or steer as it was typed and sent, with its files. Kept only by Flotilla (boat
/// may rewrite a steered turn's prompt).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentPrompt {
    pub id: String,
    pub kind: String,
    pub text: String,
    pub sent_at: String,
    pub attachments: Vec<Attached>,
    /// The scheduled prompt that sent it (its id), if one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
}

/// Sidebar grouping: what the thread needs from the user right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Activity {
    /// A turn ended (or the thread errored) since it was last shown, or it was marked unread.
    Unread,
    Idle,
    /// A turn is queued or running.
    Working,
    /// Put away by the user; its VM is stopped.
    Archived,
    /// The VM or its disk is gone; read-only.
    Expired,
}

/// A thread's row in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub thread: Thread,
    #[serde(deserialize_with = "crate::views::activity_or_idle")]
    pub activity: Activity,
    /// Newest event (ms), else when the thread was created.
    pub last_activity_ms: i64,
    /// When the running turn started (ms), while one is running.
    pub turn_started_ms: Option<i64>,
    /// The model-written summary of the agent's latest reply, else its first line.
    pub last_reply: Option<String>,
    /// The latest prompt or steer, on one line (shown while a turn runs).
    pub last_prompt: Option<String>,
    /// A project thread's pull request, and whether it was linked by hand.
    pub pr: Option<(PullRequest, bool)>,
    /// Its start failed (no VM, its repo not prepared, or boat refused the first prompt): it
    /// shows why, with Try again.
    pub start_failed: bool,
    /// When you last wrote in it (ms): a prompt or steer of yours, not one Flotilla brought.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompted_ms: Option<i64>,
    /// How often and how recently it was opened, and what for from a search: search ranks by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens: Option<crate::search::Opens>,
}

/// One row of a thread as shown: a prompt, steer, reply, tool call or notice, derived from
/// boat's events and what was sent. Ordered by `seq`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Message {
    pub seq: i64,
    pub task_id: Option<String>,
    /// `prompt`, `steer`, `text`, `tool` or `notice`.
    pub kind: String,
    pub text: Option<String>,
    /// Prompt status, steer mode, the notice's event type, or for a tool left pending when
    /// its turn ended, how the turn ended (`interrupted`, `failed`, `unfinished`).
    pub status: Option<String>,
    pub model: Option<String>,
    pub tool_name: Option<String>,
    /// The tool call's id as boat shows it (Claude Code's `toolu_…`, Codex's `exec-…`), which
    /// the harnesses' own records use too.
    pub tool_id: Option<String>,
    pub tool_summary: Option<String>,
    pub tool_input_json: Option<String>,
    pub tool_result_json: Option<String>,
    pub tool_result_type: Option<String>,
    pub is_error: bool,
    pub pending: bool,
    pub event_key: Option<String>,
    pub timestamp: Option<i64>,
    /// The names of the files sent with a prompt.
    pub attachments: Vec<String>,
    /// The strings sent cut short (over `crate::history::CAP`), to fetch whole when wanted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<crate::history::Cut>,
    /// A prompt or steer that took your answer to a question (`crate::ask`): that question's
    /// id (its ask message's `tool_id`). Its card shows the answer, so the apps don't show the
    /// message again; the prompt still starts its turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_to: Option<String>,
    /// Flotilla's own message to the agent, not yours (`[flotilla] …`: a hand-off's answer, the
    /// agents' report, a start or a request that failed), as a prompt or, into a running turn,
    /// a steer. The apps show it as Flotilla's whichever way it went.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_flotilla: bool,
}

/// One event from `GET /sandboxes/{id}/events`, with its raw JSON kept verbatim. Streaming
/// partials reach the client as these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawEvent {
    pub id: String,
    pub kind: String,
    pub timestamp: i64,
    pub task_id: Option<String>,
    pub conversation_id: Option<String>,
    pub data: Value,
    pub raw: Value,
}

impl RawEvent {
    pub fn from_value(mut raw: Value) -> RawEvent {
        // A prompt event carries each attached file whole, as base64: keep only its name and
        // type, so files never land in the database.
        if let Some(files) = raw
            .pointer_mut("/data/attachments")
            .and_then(Value::as_array_mut)
        {
            for f in files.iter_mut().filter_map(Value::as_object_mut) {
                f.remove("base64Data");
            }
        }
        let kind = raw
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let timestamp = raw.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        let task_id = raw
            .get("taskId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let id = match raw.get("id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            // The schema allows events without an id; give them a stable stand-in.
            None => format!(
                "noid-{kind}-{timestamp}-{}",
                task_id.as_deref().unwrap_or("")
            ),
        };
        RawEvent {
            id,
            timestamp,
            task_id,
            conversation_id: raw
                .get("conversationId")
                .and_then(Value::as_str)
                .map(str::to_string),
            data: raw.get("data").cloned().unwrap_or(Value::Null),
            kind,
            raw,
        }
    }

    pub fn from_json(s: &str) -> serde_json::Result<RawEvent> {
        Ok(RawEvent::from_value(serde_json::from_str(s)?))
    }

    pub fn is_prompt(&self) -> bool {
        self.kind == "prompt"
    }

    /// A streaming partial: grows in place, then is deleted and replaced by a final event
    /// with a new id. Never stored.
    pub fn is_partial(&self) -> bool {
        self.data.get("is_streaming").and_then(Value::as_bool) == Some(true)
    }

    /// The `qp-<taskId>` placeholder a prompt event has while it's queued.
    pub fn is_queued_placeholder(&self) -> bool {
        self.id.starts_with("qp-")
    }

    /// Storage key: prompt events change id during a turn, so they're keyed by `taskId`.
    pub fn key(&self) -> String {
        match (&self.task_id, self.is_prompt()) {
            (Some(t), true) => t.clone(),
            _ => self.id.clone(),
        }
    }

    /// A tool call in this event has no result yet (or its result says `pending`).
    pub fn has_pending_tool(&self) -> bool {
        let Some(tools) = self.data.get("tools").and_then(Value::as_array) else {
            return false;
        };
        tools
            .iter()
            .any(|t| match t.get("result").filter(|r| !r.is_null()) {
                None => true,
                Some(r) => {
                    let parsed = match r.get("content") {
                        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
                        other => other.cloned(),
                    };
                    parsed.and_then(|p| p.get("pending").and_then(Value::as_bool)) == Some(true)
                }
            })
    }

    /// The names of the files sent with a prompt event.
    pub fn attachment_names(&self) -> Vec<String> {
        let files = self.data.get("attachments").and_then(Value::as_array);
        files
            .into_iter()
            .flatten()
            .filter_map(|f| {
                f.get("filename")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    /// `data.status` of a prompt event.
    pub fn prompt_status(&self) -> Option<&str> {
        self.data.get("status").and_then(Value::as_str)
    }
}

/// A file to send with a prompt. The client uploads its bytes (`PUT /v1/blobs`) and the
/// prompt is sent from that copy on the server: the server never reads a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Attached {
    /// Where it is on the device that sent it: only that device opens it from here.
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub kind: Kind,
    /// The uploaded copy (`b_<sha256 hex>`); empty until the client has uploaded it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub blob: String,
    /// The paired device that sent it, as the server saw it (none: the server's own machine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// What a file holds, for the icon it shows with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Text,
    Code,
    Data,
    Sheet,
    Archive,
    Video,
    Audio,
    Binary,
    #[default]
    #[serde(other)]
    Other,
}

impl Kind {
    /// What a file with this name holds, going by its extension.
    pub fn of_name(name: &str) -> Kind {
        let ext = std::path::Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff" | "ico"
            | "heic" | "heif" | "avif" => Kind::Image,
            "txt" | "log" | "md" | "markdown" | "rst" | "rtf" | "pdf" | "doc" | "docx" | "odt"
            | "pages" => Kind::Text,
            "rs" | "py" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "go" | "java" | "kt"
            | "swift" | "c" | "h" | "cc" | "cpp" | "hpp" | "m" | "mm" | "cs" | "rb" | "php"
            | "sh" | "bash" | "zsh" | "fish" | "html" | "htm" | "css" | "scss" | "sass"
            | "less" | "sql" | "lua" | "ex" | "exs" | "erl" | "hs" | "ml" | "scala" | "dart"
            | "r" | "jl" | "vue" | "svelte" | "zig" | "pl" | "diff" | "patch" => Kind::Code,
            "json" | "jsonl" | "ndjson" | "yaml" | "yml" | "toml" | "xml" | "ini" | "plist" => {
                Kind::Data
            }
            "csv" | "tsv" | "xls" | "xlsx" | "numbers" | "ods" => Kind::Sheet,
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" => Kind::Archive,
            "mp4" | "mov" | "webm" | "ogv" | "mkv" | "avi" | "m4v" => Kind::Video,
            "mp3" | "wav" | "ogg" | "flac" | "m4a" | "aac" | "opus" | "aiff" => Kind::Audio,
            "exe" | "bin" | "dmg" | "so" | "dylib" | "dll" | "o" | "a" | "wasm" | "class"
            | "jar" | "iso" | "sqlite" | "db" => Kind::Binary,
            _ => Kind::Other,
        }
    }
}

/// How a starting thread is getting on, for its start card.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartStages {
    pub done: Vec<String>,
    pub current: Option<String>,
}

/// A new thread to start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewThread {
    /// boat environment name or id (a project's own, for a project thread).
    pub environment: String,
    /// The project (GitHub repo) to start in; `None`: No project, a blank VM.
    pub project_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub fast: bool,
    pub title: Option<String>,
    /// Machine size, one of [`MACHINE_TYPES`]; `None`: the project's, else boat's default.
    pub machine_type: Option<String>,
    pub prompt: String,
    /// Files sent with the first prompt.
    pub attachments: Vec<Attached>,
    /// Wait for the sandbox to be up before the first prompt. Not needed: boat accepts a
    /// prompt while the sandbox is still `provisioned` (verified in M1), and a refusal falls
    /// back to waiting anyway.
    pub wait_ready: bool,
    /// A project thread on a branch already on GitHub (a pull request's, say); `None`: a new
    /// branch from the default branch, named from the prompt.
    pub on_branch: Option<OnBranch>,
    /// The thread's id, if it must be this one: a delegate's request id, or the id the client
    /// made for `thread.new`, so a request seen twice makes one thread. `None`: a new one.
    pub id: Option<String>,
    /// The thread that delegated this one.
    pub parent: Option<String>,
    /// One of several threads started together from one prompt (one per model): a word for
    /// its new branch's name, after the prompt's words (its model's name), so theirs differ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_tag: Option<String>,
}

/// An existing branch to start a project thread on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnBranch {
    pub name: String,
    /// The branch it merges into: its pull request's base, else the default branch.
    pub base: String,
}

/// How a message went to boat: a new turn, or a steer of the running one (boat's id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sent {
    Prompt(String),
    Steer(String),
}

/// A thread's subagents as its harness recorded them in the VM.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lineage {
    #[serde(default)]
    pub subagents: Vec<Subagent>,
}

/// One subagent, as its harness recorded it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    pub id: String,
    /// The call that started it, as boat shows it: Claude Code's Agent call, OpenCode's task
    /// call. None for Codex, whose spawn calls boat doesn't show.
    #[serde(default)]
    pub parent_call: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// What it was asked to do, when readable (OpenCode; Codex keeps it encrypted, and
    /// Claude Code's is on its Agent call).
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub started_ms: Option<i64>,
    #[serde(default)]
    pub finished_ms: Option<i64>,
    /// Its own tool calls' ids, as boat shows them (OpenCode's aren't shown).
    #[serde(default)]
    pub calls: Vec<String>,
    /// Its latest call (name, summary), for a harness whose calls boat doesn't show.
    #[serde(default)]
    pub latest: Option<(String, String)>,
    /// What it said, in order; the last is its answer.
    #[serde(default)]
    pub texts: Vec<String>,
    /// The agent's own texts written after the call that started it (OpenCode, whose reply
    /// boat stamps too early to place by time).
    #[serde(default)]
    pub replies_after: Vec<String>,
}

/// What a composer offers after `/`, `$` or `@`: the skills and commands found in a thread's
/// VM, and its repo's files.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mentions {
    /// Skills and commands found in the VM, each for one harness.
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// The repo's folder (`/home/user/app`); "" when there's none.
    #[serde(default)]
    pub root: String,
    /// The repo's files (tracked, or new and not ignored), relative to `root`.
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `skill` or `command`.
    pub kind: String,
    /// What the harness calls it.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The harness that can use it: `claude-code`, `codex` or `opencode`.
    #[serde(default)]
    pub harness: String,
    /// The text that runs it: at the very start of a prompt for Claude Code and OpenCode,
    /// anywhere for Codex.
    #[serde(default)]
    pub insert: String,
    /// Another with its name comes first, so this one is never used.
    #[serde(default)]
    pub shadowed: bool,
}

impl Entry {
    /// What to put in the prompt for `provider`'s agent to run this, if it can.
    pub fn insert(&self, provider: &str) -> Option<&str> {
        (self.harness == provider && !self.shadowed && !self.insert.is_empty())
            .then_some(self.insert.as_str())
    }
}

/// A preview link: `url` is a path on the service's preview (starting with `/`) or an
/// absolute http(s) URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub title: String,
    pub url: String,
}

/// One service in a thread's VM.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRun {
    pub name: String,
    pub port: u16,
    /// boat's process id for it, while Flotilla started it on this VM.
    pub pid: Option<i64>,
    /// `starting`, `ready`, `not_responding` (still running, never answered), `exited`
    /// (`exit_code`), `stopped` (by you; ensure leaves it until Restart) or `failed` (couldn't
    /// start: `error`).
    pub state: String,
    pub exit_code: Option<i64>,
    pub error: Option<String>,
    /// The end of what it printed.
    pub output: String,
    /// Its preview URL (with boat's sign-in token; never log it), when it has a portal.
    pub preview: Option<String>,
    /// Its preview links as full URLs, the primary one first.
    pub links: Vec<Link>,
    pub started_at: Option<String>,
    /// How far its output has been read (the engine's, not for showing).
    #[serde(default)]
    pub out_at: u64,
    #[serde(default)]
    pub err_at: u64,
}

impl ServiceRun {
    /// Starting, ready or running without answering.
    pub fn live(&self) -> bool {
        matches!(self.state.as_str(), "starting" | "ready" | "not_responding")
    }
}

/// A thread's services, as last seen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadServices {
    /// The VM they were started on: after a fork or a rebuild, none of them are running.
    pub sandbox: String,
    /// Which file declared them.
    pub file: Option<String>,
    /// What's wrong with the file, if it doesn't check out (the services keep their last state).
    pub error: Option<String>,
    pub services: Vec<ServiceRun>,
}

/// The last run of a hook (`.agents/resume`, `.agents/archive`) in a thread's VM.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRun {
    pub sandbox: String,
    pub pid: i64,
    pub started_at: String,
    /// `running` (while Flotilla waits for it), `done` (exited 0), `failed` (`exit_code`
    /// says how) or `timed_out` (stopped when its time was up).
    pub state: String,
    pub exit_code: Option<i64>,
    /// The end of what it printed.
    pub output: String,
    /// How far its output has been read (the engine's, not for showing).
    #[serde(default)]
    pub out_at: u64,
    #[serde(default)]
    pub err_at: u64,
}

/// A setup script that failed while a thread started. The thread started anyway; its header
/// says so.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptFailure {
    /// The script, e.g. `npm install` or `.agents/setup`.
    pub label: String,
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    /// The end of what it printed (or why boat couldn't run it).
    pub output: String,
}

impl ScriptFailure {
    /// "Setup failed (exit 1) at .agents/setup", or "Setup timed out at …".
    pub fn summary(&self) -> String {
        match (self.timed_out, self.exit_code) {
            (true, _) => format!("Setup timed out at {}", self.label),
            (false, Some(code)) => format!("Setup failed (exit {code}) at {}", self.label),
            (false, None) => format!("Setup failed at {}", self.label),
        }
    }
}

/// A skill: a folder with a SKILL.md, on the device that reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// Its folder's name, which every harness can call it by.
    pub name: String,
    pub description: String,
    pub folder: PathBuf,
}

/// A skill every agent gets: its folder, packed (a gzipped tar) on the device that added it
/// and kept on the server, which copies it into each VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserSkill {
    pub name: String,
    pub description: String,
    /// The packed folder (`b_<sha256 hex>`), and its size.
    pub blob: String,
    pub size: u64,
    /// Where the folder is on the device that added it: Update packs it again from there.
    pub folder: PathBuf,
    /// The paired device that added it (none: the server's own machine), its name then, and
    /// when (ms since the epoch).
    pub from: Option<String>,
    pub from_name: String,
    pub added_ms: i64,
}

/// What the sidebar shows of the account's limits: `(used, limit)` pairs, read leniently
/// from boat's `/limits` (a field boat leaves out is just not shown).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// VMs running, of how many may run at once.
    pub vms: Option<(i64, i64)>,
    /// VM starts in the last minute, hour and 24 hours. None when starts are unlimited.
    pub minute: Option<(i64, i64)>,
    pub hour: Option<(i64, i64)>,
    pub day: Option<(i64, i64)>,
    /// The plan's VM hours left this period, of how many it includes, whole hours.
    pub vm_hours: Option<(i64, i64)>,
    /// When the plan's VM hours renew (RFC 3339).
    pub renews: Option<String>,
    /// Why boat won't start a VM right now.
    pub blocked: Option<String>,
    /// While a start window is full: when a VM can start again (RFC 3339), if boat says.
    #[serde(default)]
    pub starts_free: Option<String>,
    /// Starts past the plan's limits, billed by boat, on plans that offer them.
    #[serde(default)]
    pub extra_starts: Option<ExtraStarts>,
    /// When boat gave these numbers (ms since the epoch).
    #[serde(default)]
    pub read_ms: Option<i64>,
    /// The machine sizes the plan doesn't start (boat's name) and why, e.g. `xlarge`, "needs
    /// the $100/month plan or higher". Left out when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sizes_unavailable: Vec<(String, String)>,
}

/// boat's extra starts: whether they're on, and this billing month's so far.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraStarts {
    pub on: bool,
    /// Extra starts this month.
    pub used: i64,
    /// What they've cost this month, of the monthly cap, in cents.
    pub spend_cents: Option<(i64, i64)>,
    /// The cap is reached: no more extra starts this month.
    pub cap_reached: bool,
}

/// The machine sizes boat starts VMs on: its name for each, what to call it, and what it is.
/// `default` is boat's own default, what a VM gets when no size is asked for.
pub const MACHINE_TYPES: &[(&str, &str, &str)] = &[
    ("small", "Small", "2 vCPU · 4 GB · half rate"),
    ("default", "Medium", "4 vCPU · 8 GB"),
    ("large", "Large", "8 vCPU · 16 GB · 2× rate"),
    ("xlarge", "XL", "16 vCPU · 32 GB · $100+ plan"),
];

/// What to call machine size `t` (boat's name for it); `None` is boat's default.
pub fn machine_type_label(t: Option<&str>) -> &str {
    let t = t.unwrap_or("default");
    MACHINE_TYPES
        .iter()
        .find(|(id, _, _)| *id == t)
        .map(|(_, label, _)| *label)
        .unwrap_or(t)
}
