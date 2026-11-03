//! The messages between the app and its backend: requests and subscriptions in; answers,
//! views, live text and notes out. The app speaks only these, whether the engine's host runs
//! in its process or on a server: the backend's client turns them into the wire protocol
//! (`crate::wire`, with revisions and deltas) and the views back into whole ones.

use crate::{
    AgentUsage, BranchChoices, Catalog, Devices, GhStatus, Mentions, OnBranch, ProjectDetail,
    RawEvent, Reply, RepoInfo, Request, Schedules, Settings, Sidebar, Suggestions, ThreadDetail,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A request's id, the client's own: its answer carries it back.
pub type ReqId = u64;

/// Client → engine side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Req {
        id: ReqId,
        #[serde(flatten)]
        req: Request,
    },
    /// Send this view now and whenever it changes. Subscribing to a thread also marks it the
    /// one on screen: polled every second, and read as soon as anything new arrives.
    Sub {
        #[serde(flatten)]
        view: ViewKey,
    },
    Unsub {
        #[serde(flatten)]
        view: ViewKey,
    },
}

/// Engine side → client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// A request's answer. It comes after the views it changed.
    Res {
        id: ReqId,
        result: Result<Reply, String>,
    },
    /// A view, whole.
    Snap {
        #[serde(flatten)]
        view: Box<View>,
    },
    /// A thread's streaming text (only for a thread subscribed to). Never stored or replayed.
    Live {
        thread: String,
        partials: Vec<RawEvent>,
    },
    /// Something to tell the user.
    Note(Note),
    /// Part of request `id`'s answer, which comes in pieces (repos a page at a time) or later.
    Progress { id: ReqId, note: Note },
    /// How the connection to the engine's host is (a server's, or why this one can't run).
    Connection(Connection),
}

/// Whether the backend is reaching the engine's host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connection {
    /// Connected, and what's shown is caught up.
    Online,
    /// Not connected (`error`: why the last try failed); trying again.
    Offline { error: String },
    /// It won't work until something changes elsewhere: why.
    Refused { reason: String },
}

/// What a view is of.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "view", content = "key", rename_all = "snake_case")]
pub enum ViewKey {
    Sidebar,
    Catalog,
    Settings,
    Thread(String),
    /// A thread's skills and files for the composer's menu, as last read in its VM.
    Mentions(String),
    Project(String),
    /// The devices paired with the server.
    Devices,
    /// What the agents spent (`crate::spend`). Subscribing checks the VMs that are up, at most
    /// every five minutes.
    AgentUsage,
    /// Every scheduled prompt.
    Schedules,
}

impl ViewKey {
    /// What a connection limited to [`Scope::Agent`](crate::Scope) may subscribe to: the
    /// threads (with the projects and environments the sidebar carries), each thread, and the
    /// models. Not settings, devices, a project's setup, usage, schedules or a VM's files.
    pub fn agent_may(&self) -> bool {
        matches!(
            self,
            ViewKey::Sidebar | ViewKey::Catalog | ViewKey::Thread(_)
        )
    }

    /// The view's kind, as the wire names it.
    pub fn name(&self) -> &'static str {
        match self {
            ViewKey::Sidebar => "sidebar",
            ViewKey::Catalog => "catalog",
            ViewKey::Settings => "settings",
            ViewKey::Thread(_) => "thread",
            ViewKey::Mentions(_) => "mentions",
            ViewKey::Project(_) => "project",
            ViewKey::Devices => "devices",
            ViewKey::AgentUsage => "agent_usage",
            ViewKey::Schedules => "schedules",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "view", content = "data", rename_all = "snake_case")]
pub enum View {
    Sidebar(Sidebar),
    /// boat's model catalog, once read.
    Catalog(Option<Catalog>),
    Settings(Settings),
    Thread(Box<ThreadDetail>),
    Mentions {
        thread: String,
        mentions: Option<Mentions>,
    },
    Project(ProjectDetail),
    Devices(Devices),
    AgentUsage(AgentUsage),
    Schedules(Schedules),
}

impl View {
    /// The view from its key and its data (a snapshot's `data`, or a replica's copy).
    pub fn from_data(key: &ViewKey, data: serde_json::Value) -> Result<View, String> {
        let tag = serde_json::to_value(key)
            .map_err(|e| e.to_string())?
            .get("view")
            .cloned()
            .unwrap_or_default();
        let view: View = serde_json::from_value(serde_json::json!({"view": tag, "data": data}))
            .map_err(|e| format!("not a {} view: {e}", key.name()))?;
        if &view.key() != key {
            return Err(format!("a view of {:?} sent as {key:?}", view.key()));
        }
        Ok(view)
    }

    /// Its data alone, as a snapshot carries it.
    pub fn data(&self) -> serde_json::Value {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(mut v)) => v.remove("data").unwrap_or_default(),
            _ => serde_json::Value::Null,
        }
    }

    pub fn key(&self) -> ViewKey {
        match self {
            View::Sidebar(_) => ViewKey::Sidebar,
            View::Catalog(_) => ViewKey::Catalog,
            View::Settings(_) => ViewKey::Settings,
            View::Thread(t) => ViewKey::Thread(t.id.clone()),
            View::Mentions { thread, .. } => ViewKey::Mentions(thread.clone()),
            View::Project(p) => ViewKey::Project(p.id.clone()),
            View::Devices(_) => ViewKey::Devices,
            View::AgentUsage(_) => ViewKey::AgentUsage,
            View::Schedules(_) => ViewKey::Schedules,
        }
    }
}

/// What the client is told that isn't a view or an answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Note {
    /// Something the engine started itself (a delegate's start, a message to a thread) ended.
    Done {
        request: Request,
        result: Result<Reply, String>,
    },
    /// A turn the engine watched running has ended.
    TurnFinished(String),
    /// A thread's agent asked you something (an ask message, waiting) that wasn't seen before.
    Asked(String),
    /// The last summary (a title, a reply's preview, a test): when it worked, or why not.
    Summaries(Result<String, String>),
    /// gh was looked at (Check again, or the engine's own look): what it can do now. The
    /// sidebar view carries it too, but this says the look is over even when nothing changed.
    GitHub(GhStatus),
    /// Threads with a message that was still sending when Flotilla quit and that boat never
    /// got: it's marked "not sent" in place, with Send again and Discard.
    Unsent(Vec<String>),
    /// A thread's pull request was merged. `archived`: the thread was archived for it (Settings'
    /// "Archive a thread when its pull request merges").
    PrMerged {
        thread: String,
        number: i64,
        title: String,
        branch: String,
        #[serde(default)]
        archived: bool,
    },
    /// Threads archived because they sat idle for as long as Settings says.
    IdleArchived(Vec<String>),
    /// A project's setup run ended. `recovered`: it passed after the previous one failed.
    /// `fixable`: the run, when a script failed, so a code change could fix it.
    SetupDone {
        project: String,
        name: String,
        ok: bool,
        error: Option<String>,
        recovered: bool,
        fixable: Option<String>,
    },
    /// `github.repos`: the list so far after each page (`done` false), then all of it.
    /// `login`: whose list it is, on the last one.
    Repos {
        result: Result<Vec<RepoInfo>, String>,
        done: bool,
        login: Option<String>,
    },
    /// `github.suggest`: setup scripts for a repo (`owner/name`).
    Suggested {
        repo: String,
        result: Result<Suggestions, String>,
    },
    /// `github.branches`.
    Branches {
        project: String,
        result: Result<BranchChoices, String>,
    },
    /// `thread.on_branch`: `Ok` to open New thread on the branch, or why not.
    BranchChecked {
        project: String,
        branch: OnBranch,
        prompt: Option<String>,
        result: Result<(), String>,
    },
    /// `db.rebuild` finished: where the old database went, or why it failed.
    Rebuilt(Result<String, String>),
    /// A scheduled prompt's run didn't happen (`why`), or its send or start failed. Said once
    /// for a run of failures with the same reason. `thread`: the thread it was for, if one.
    ScheduleFailed {
        schedule: String,
        prompt: String,
        thread: Option<String>,
        why: String,
    },
    /// Something worth a notification, as the server decided for the note just before it.
    /// `id` is this alert's alone (the server's run and a count), so a device shows it once.
    Alert { id: String, alert: Alert },
}

/// A notification, in the words a phone is sent too (`flotilla_core::push::alert` decides).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    pub kind: AlertKind,
    pub title: String,
    pub body: String,
    /// The thread a click opens.
    pub thread: Option<String>,
    /// What it's about, for grouping and replacing: a thread's id, or a project's.
    pub about: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// A turn finished (not a delegated thread's: its answer reaches the thread that asked).
    TurnFinished,
    /// Agents' answers reached a thread, or a thread it handed work to couldn't start.
    AgentsAnswered,
    PrMerged,
    /// A project's setup failed (`about` is the project; no thread).
    SetupFailed,
    /// A thread's VM or first prompt didn't start, whoever started it.
    StartFailed,
    /// A scheduled prompt didn't run.
    ScheduleFailed,
    /// A thread's agent asked you something with its ask tool and waits on the answer.
    Asked,
}

impl AlertKind {
    /// Its name on the wire, and in a phone's notification.
    pub fn as_str(self) -> &'static str {
        match self {
            AlertKind::TurnFinished => "turn_finished",
            AlertKind::AgentsAnswered => "agents_answered",
            AlertKind::PrMerged => "pr_merged",
            AlertKind::SetupFailed => "setup_failed",
            AlertKind::StartFailed => "start_failed",
            AlertKind::ScheduleFailed => "schedule_failed",
            AlertKind::Asked => "asked",
        }
    }
}

/// Send `x` through JSON and back, as the wire would: what comes back, or why it didn't
/// survive (it must also write the same JSON again).
pub fn through_json<T: Serialize + DeserializeOwned>(x: &T) -> Result<T, String> {
    let json = serde_json::to_string(x).map_err(|e| format!("doesn't serialize: {e}"))?;
    let back: T =
        serde_json::from_str(&json).map_err(|e| format!("doesn't read back: {e}: {json}"))?;
    let again =
        serde_json::to_string(&back).map_err(|e| format!("doesn't serialize again: {e}"))?;
    if again != json {
        return Err(format!("changes on the way: {json} became {again}"));
    }
    Ok(back)
}
