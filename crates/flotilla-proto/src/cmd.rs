//! Requests: everything a client asks for, as methods with their answers. A request with a
//! client-made id (`thread.new`'s thread id, `thread.send`'s `local-…` placeholder) is a no-op
//! when it's seen again, so a client can resend it safely.

use crate::{
    Attached, Mentions, NewThread, RepoInfo, ScheduleSpec, Sent, SetupScript, Skill, ThreadSettings,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "m", content = "p")]
pub enum Request {
    /// Start a thread. `id` must be set: a UUID the client made, which the thread takes. Its
    /// answer comes when the first prompt is in (or the start failed); the thread shows in the
    /// sidebar as soon as it's written.
    #[serde(rename = "thread.new")]
    NewThread(Box<NewThread>),
    /// Start again a thread whose start failed before it got a VM.
    #[serde(rename = "thread.retry")]
    Retry(String),
    /// Send a message (a steer while a turn runs). `local`: the client's `local-<uuid>` id for
    /// it, which shows as "sending" until boat takes it.
    #[serde(rename = "thread.send")]
    Send {
        thread: String,
        text: String,
        files: Vec<Attached>,
        local: String,
    },
    /// Answer a question the thread's agent is waiting on (`ask`: the ask message's
    /// `tool_id`): one answer per question, in order, each the options picked or what was
    /// typed. It goes to the agent as a message (`crate::ask::reply_text`), sent as
    /// `thread.send` sends one, with `local` as its placeholder's id.
    #[serde(rename = "thread.answer")]
    Answer {
        thread: String,
        ask: String,
        answers: Vec<Vec<String>>,
        local: String,
    },
    /// Let go of a message boat never got (`id`: its `local-` or `unsent-` id).
    #[serde(rename = "thread.discard_unsent")]
    DiscardUnsent { thread: String, id: String },
    #[serde(rename = "thread.interrupt")]
    Interrupt(String),
    #[serde(rename = "thread.stop")]
    Stop(String),
    #[serde(rename = "thread.resume")]
    Resume(String),
    #[serde(rename = "thread.rename")]
    Rename { thread: String, title: String },
    #[serde(rename = "thread.keep_running")]
    KeepRunning { thread: String, on: bool },
    #[serde(rename = "thread.preview")]
    Preview { thread: String, port: u16 },
    /// Let anyone with its link open a port in the VM, with no sign-in, `for` this long (from
    /// now, again if it's public already), waking a stopped VM first. Answers `Url`: the link.
    #[serde(rename = "thread.make_public")]
    MakePublic {
        thread: String,
        port: u16,
        #[serde(rename = "for")]
        for_: crate::PublicFor,
    },
    /// The port's link needs boat's sign-in token again, at once. The token is a new one, so
    /// an old private link stops working too. A stopped VM isn't woken: it's done as it wakes.
    #[serde(rename = "thread.make_private")]
    MakePrivate { thread: String, port: u16 },
    /// Move it under Archived at once, then run `.agents/archive` and stop its VM.
    #[serde(rename = "thread.archive")]
    Archive(String),
    #[serde(rename = "thread.unarchive")]
    Unarchive(String),
    /// Delete its VM and its history.
    #[serde(rename = "thread.delete")]
    Delete(String),
    /// Forget a thread here, its VM left as it is (meant for one whose VM is gone). Its public
    /// links still end through boat.
    #[serde(rename = "thread.forget")]
    Forget(String),
    #[serde(rename = "thread.mark_viewed")]
    MarkViewed(String),
    /// It was opened on screen; `query`, the search it was picked from, if any. Search ranks by
    /// how often and recently each thread is opened, and what for (`search::record_open`).
    #[serde(rename = "thread.opened")]
    Opened {
        thread: String,
        query: Option<String>,
    },
    /// Mark it unread; a client showing it stops marking it read until it's opened again.
    #[serde(rename = "thread.mark_unread")]
    MarkUnread(String),
    /// A title for the thread from its messages (Rename's Generate). Answers `Name`.
    #[serde(rename = "thread.suggest_name")]
    SuggestName(String),
    /// New thread on this thread's branch, with `prompt` filled in: GitHub is asked whether
    /// the branch is there, and `Note::BranchChecked` says.
    #[serde(rename = "thread.on_branch")]
    OnBranch {
        thread: String,
        prompt: Option<String>,
    },
    /// The composer's `/` `$` `@` menu opened: read the thread's skills and files again if the
    /// last read is a while ago.
    #[serde(rename = "thread.read_mentions")]
    ReadMentions(String),
    #[serde(rename = "pr.merge")]
    MergePr(String),
    #[serde(rename = "pr.ready")]
    MarkReady(String),
    #[serde(rename = "pr.link")]
    LinkPr { thread: String, number: i64 },
    /// The prompt that asks the thread's agent to address its PR's review. Answers `Prompt`.
    #[serde(rename = "pr.review_prompt")]
    ReviewPrompt(String),
    /// Add a GitHub repo as a project and set it up. Answers `Created` with its id.
    #[serde(rename = "project.add")]
    AddProject {
        repo: Box<RepoInfo>,
        scripts: Vec<SetupScript>,
    },
    #[serde(rename = "project.remove")]
    RemoveProject(String),
    /// Save a project's setup scripts, and run its setup with them if `run`.
    #[serde(rename = "project.set_scripts")]
    SetScripts {
        project: String,
        scripts: Vec<SetupScript>,
        run: bool,
    },
    /// The skills and files last read in the project's threads. Answers `Mentions`.
    #[serde(rename = "project.mentions")]
    ProjectMentions(String),
    #[serde(rename = "setup.run")]
    RunSetup { project: String, fresh: bool },
    #[serde(rename = "setup.cancel")]
    CancelSetup(String),
    /// The prompt for a new thread that fixes a failed setup run. Answers `Prompt`.
    #[serde(rename = "setup.fix_prompt")]
    SetupFixPrompt { project: String, run: String },
    #[serde(rename = "services")]
    Services(ServiceCmd),
    /// Run gh again to see whose it is.
    #[serde(rename = "github.check")]
    CheckGitHub,
    /// The repos gh's account can see: `Note::Repos` brings them, a page at a time.
    #[serde(rename = "github.repos")]
    ListRepos,
    /// The list `github.repos` last brought for the signed-in account. Answers `Repos`.
    #[serde(rename = "github.cached_repos")]
    CachedRepos,
    /// Setup scripts that fit a repo: `Note::Suggested`.
    #[serde(rename = "github.suggest")]
    Suggest(Box<RepoInfo>),
    /// A project's open pull requests and branches: `Note::Branches`.
    #[serde(rename = "github.branches")]
    ListBranches(String),
    #[serde(rename = "sync.now")]
    SyncNow,
    /// Move the database aside and rebuild it from boat: `Note::Rebuilt`.
    #[serde(rename = "db.rebuild")]
    Rebuild,
    /// Check that summaries work with the current model.
    #[serde(rename = "summaries.test")]
    TestSummaries,
    #[serde(rename = "settings.set")]
    Set(Setting),
    /// Save the boat key (it's never sent back) and restart the engine with it.
    #[serde(rename = "settings.set_boat_key")]
    SetBoatKey(Secret),
    /// Your instructions for every agent, from each thread's next prompt.
    #[serde(rename = "instructions.set")]
    SetInstructions(String),
    /// Give every agent this skill, its folder packed and uploaded as `blob`, in place of any
    /// of its name (Update). The server notes which device added it, and when.
    #[serde(rename = "skills.put")]
    PutSkill { skill: Skill, blob: String },
    #[serde(rename = "skills.remove")]
    RemoveSkill(String),
    /// Check again what isn't set up where the engine runs (Settings shows it). Answers when
    /// the checks are done.
    #[serde(rename = "doctor.run")]
    RunDoctor,
    /// Read the account's usage from boat again now. Answers once it's read (the sidebar's
    /// `usage` has it), or with why it couldn't be.
    #[serde(rename = "usage.read")]
    ReadUsage,
    /// Check every VM that's up for what its agents spent, their accounts' limits and the
    /// model prices, now. Answers once it's done (the `agent_usage` view has it).
    #[serde(rename = "agent_usage.read")]
    ReadAgentUsage,
    /// A one-time code (and its link) that pairs another device with this server, valid for
    /// `ttl_secs` (default 5 minutes). Answers `Code`.
    #[serde(rename = "devices.pair")]
    PairCode { ttl_secs: Option<u64> },
    /// Forget a paired device: its token stops working and its connections close.
    #[serde(rename = "devices.revoke")]
    RevokeDevice(String),
    /// Name a paired device: what Devices lists it as. (A phone can't tell the server its own
    /// name, so it pairs as "iPhone" unless it's named.)
    #[serde(rename = "devices.rename")]
    RenameDevice { device: String, name: String },
    /// Send this device notifications through Apple's push service, in place of any token it
    /// registered before. Only a paired device (a phone) registers.
    #[serde(rename = "push.register")]
    RegisterPush(PushToken),
    /// Stop sending this device notifications.
    #[serde(rename = "push.unregister")]
    UnregisterPush,
    /// Threads for `query` (`crate::search`): by title (or PR, branch, project) first, then by
    /// their messages, one hit a thread (its best message), at most `limit` (default
    /// `search::LIMIT`), a fifth of it kept for messages. Answers `Hits`.
    #[serde(rename = "search")]
    Search { query: String, limit: Option<u32> },
    /// Make a scheduled prompt, or change the one with its id (`flotilla_proto::schedule`).
    /// Its next run counts from now unless only the prompt changed.
    #[serde(rename = "schedule.put")]
    PutSchedule(Box<ScheduleSpec>),
    /// Pause it (`paused`), or let it run again, from now.
    #[serde(rename = "schedule.pause")]
    PauseSchedule { schedule: String, paused: bool },
    #[serde(rename = "schedule.delete")]
    DeleteSchedule(String),
    /// Run it now, leaving its next run as it is.
    #[serde(rename = "schedule.run")]
    RunSchedule(String),
}

impl Request {
    /// One with a client-made id (`thread.new`'s thread id, `thread.send`'s and
    /// `thread.answer`'s `local-` id): the server does it once however often it's sent, so it's
    /// safe to send again after a drop.
    pub fn resendable(&self) -> bool {
        matches!(
            self,
            Request::NewThread(_)
                | Request::Send { .. }
                | Request::PutSchedule(_)
                | Request::Answer { .. }
        )
    }

    /// The thread it's about, if one.
    pub fn thread(&self) -> Option<&str> {
        match self {
            Request::NewThread(req) => req.id.as_deref(),
            Request::Retry(t)
            | Request::Send { thread: t, .. }
            | Request::Answer { thread: t, .. }
            | Request::DiscardUnsent { thread: t, .. }
            | Request::Interrupt(t)
            | Request::Stop(t)
            | Request::Resume(t)
            | Request::Rename { thread: t, .. }
            | Request::KeepRunning { thread: t, .. }
            | Request::Preview { thread: t, .. }
            | Request::MakePublic { thread: t, .. }
            | Request::MakePrivate { thread: t, .. }
            | Request::Archive(t)
            | Request::Unarchive(t)
            | Request::Delete(t)
            | Request::Forget(t)
            | Request::MarkViewed(t)
            | Request::Opened { thread: t, .. }
            | Request::MarkUnread(t)
            | Request::SuggestName(t)
            | Request::OnBranch { thread: t, .. }
            | Request::ReadMentions(t)
            | Request::MergePr(t)
            | Request::MarkReady(t)
            | Request::LinkPr { thread: t, .. }
            | Request::ReviewPrompt(t) => Some(t),
            Request::Services(c) => Some(c.thread()),
            _ => None,
        }
    }

    /// Why a connection limited to [`Scope::Agent`](crate::Scope) may not send this; None if
    /// it may. `device`: the paired device asking, if one, which may sign itself out. The
    /// server checks it on every request (`flotilla_engine::host`), whatever the client.
    pub fn agent_refusal(&self, device: Option<&str>) -> Option<String> {
        let files = "Agents can't send files".to_string();
        match self {
            Request::NewThread(n) if n.parent.is_some() => {
                Some("Agents can't start a hand-off for another thread".into())
            }
            Request::NewThread(n) if !n.attachments.is_empty() => Some(files),
            Request::Send { files: f, .. } if !f.is_empty() => Some(files),
            // Named, not left to the rest: nothing an agent asks makes a link public, or
            // changes how long one stays public.
            Request::MakePublic { .. } | Request::MakePrivate { .. } => {
                Some(format!("Agents can't {}", self.verb()))
            }
            Request::NewThread(_)
            | Request::Send { .. }
            | Request::Answer { .. }
            | Request::Interrupt(_)
            | Request::Archive(_)
            | Request::Preview { .. }
            | Request::Search { .. } => None,
            Request::RevokeDevice(d) if device.is_some_and(|me| me == d.trim()) => None,
            Request::RevokeDevice(_) => Some("Agents can't revoke other devices".into()),
            other => Some(format!("Agents can't {}", other.verb())),
        }
    }

    /// What failed, for an error message: "Couldn't {verb}".
    pub fn verb(&self) -> &'static str {
        match self {
            Request::NewThread(_) | Request::Retry(_) => "start the thread",
            Request::Send { .. } => "send",
            Request::Answer { .. } => "send the answer",
            Request::DiscardUnsent { .. } => "discard the message",
            Request::Interrupt(_) => "stop the turn",
            Request::Stop(_) => "stop the VM",
            Request::Resume(_) => "resume the VM",
            Request::Rename { .. } => "rename the thread",
            Request::KeepRunning { .. } => "change Keep running",
            Request::Preview { .. } => "open the preview",
            Request::MakePublic { .. } => "make the link public",
            Request::MakePrivate { .. } => "make the link private",
            Request::Archive(_) => "archive the thread",
            Request::Unarchive(_) => "unarchive the thread",
            Request::Delete(_) => "delete the thread",
            Request::Forget(_) => "forget the thread",
            Request::MarkViewed(_) | Request::MarkUnread(_) | Request::Opened { .. } => {
                "mark the thread"
            }
            Request::SuggestName(_) => "suggest a name",
            Request::OnBranch { .. } => "start a thread on the branch",
            Request::ReadMentions(_) | Request::ProjectMentions(_) => "read the skills and files",
            Request::MergePr(_) => "merge the pull request",
            Request::MarkReady(_) => "mark it ready for review",
            Request::LinkPr { .. } => "link the pull request",
            Request::ReviewPrompt(_) => "read the review",
            Request::AddProject { .. } => "add the project",
            Request::RemoveProject(_) => "remove the project",
            Request::SetScripts { .. } => "save the scripts",
            Request::RunSetup { .. } => "run the setup",
            Request::CancelSetup(_) => "cancel the setup",
            Request::SetupFixPrompt { .. } => "write the fix prompt",
            Request::Services(_) => "change the service",
            Request::CheckGitHub
            | Request::ListRepos
            | Request::CachedRepos
            | Request::Suggest(_)
            | Request::ListBranches(_) => "read GitHub",
            Request::SyncNow => "sync",
            Request::Rebuild => "rebuild",
            Request::TestSummaries => "test summaries",
            Request::Set(_) | Request::SetInstructions(_) => "save the setting",
            Request::PutSkill { .. } => "add the skill",
            Request::RemoveSkill(_) => "remove the skill",
            Request::RunDoctor => "check the setup",
            Request::ReadUsage => "read the usage",
            Request::ReadAgentUsage => "read the agents' usage",
            Request::SetBoatKey(_) => "save the key",
            Request::PairCode { .. } => "make a pairing code",
            Request::RevokeDevice(_) => "revoke the device",
            Request::RenameDevice { .. } => "rename the device",
            Request::RegisterPush(_) => "turn on notifications",
            Request::UnregisterPush => "turn off notifications",
            Request::Search { .. } => "search",
            Request::PutSchedule(_) => "save the schedule",
            Request::PauseSchedule { paused: true, .. } => "pause the schedule",
            Request::PauseSchedule { paused: false, .. } => "resume the schedule",
            Request::DeleteSchedule(_) => "delete the schedule",
            Request::RunSchedule(_) => "run the schedule",
        }
    }
}

/// A request's answer when it worked. (A failure is a message for the user.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Done,
    /// Seen before (its client-made id): nothing more was done.
    Already,
    /// A thread or project was made, with this id.
    Created(String),
    Sent(Sent),
    /// To open (a preview).
    Url(String),
    /// A prompt for you to read over and send.
    Prompt(String),
    /// A suggested title.
    Name(String),
    Mentions(Option<Mentions>),
    Repos(Vec<crate::RepoInfo>),
    /// A pairing code, made for the client that asked.
    Code(PairingCode),
    /// What a search found.
    Hits(Vec<crate::SearchHit>),
    /// `push.register`'s answer: how this server sends notifications, so a phone registers
    /// with a push relay only if this server sends through it.
    Push(PushMode),
}

/// How a server sends notifications, as `push.register` answers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushMode {
    /// The push relay it sends through (normalized: `https://push.useflotilla.com`). None: it
    /// sends to Apple with its own key, or not at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
}

/// A one-time pairing code: shown on one device, typed or scanned on another.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    /// The 12 characters (`crate::pair::display` groups them).
    pub code: String,
    pub expires_ms: i64,
    /// `flotilla://pair?u=…#<code>`, for the server's URL as it gives it out.
    pub link: String,
}

impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingCode")
            .field("expires_ms", &self.expires_ms)
            .finish_non_exhaustive()
    }
}

/// A phone's address with Apple's push service (APNs), for `push.register`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushToken {
    /// The device token iOS gave the app, in hex. Never logged.
    pub token: Secret,
    /// Which APNs it's from: `sandbox` (a development build) or `production`.
    pub env: String,
    /// The app's bundle id, APNs' topic.
    pub topic: String,
    /// The push relay the phone registered with, for a server without its own APNs key to
    /// send through (`flotilla_core::push`). An older app sends none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<PushRelay>,
}

/// What a phone got from a push relay (`relay/README.md`), for its server to send it
/// notifications through. Never logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRelay {
    /// The relay: `https://push.useflotilla.com`.
    pub url: String,
    /// The handle and secret the relay gave the phone.
    pub handle: String,
    pub secret: Secret,
    /// The phone's own key for its notifications (32 bytes, base64): the server encrypts each
    /// one with it, and the relay never sees it.
    pub key: Secret,
}

/// What to do with a thread's services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCmd {
    /// Start what isn't running (services you stopped too) and wait for it to be ready.
    Ensure(String),
    Restart {
        thread: String,
        name: String,
    },
    Stop {
        thread: String,
        name: String,
    },
}

impl ServiceCmd {
    pub fn thread(&self) -> &str {
        match self {
            ServiceCmd::Ensure(t)
            | ServiceCmd::Restart { thread: t, .. }
            | ServiceCmd::Stop { thread: t, .. } => t,
        }
    }
}

/// A setting `settings.set` may change. Only these: anything else in the engine's store is
/// its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "key", content = "value", rename_all = "snake_case")]
pub enum Setting {
    /// Defaults for new threads.
    Defaults(ThreadSettings),
    /// Stop idle VMs after this many minutes (from each thread's next turn, resume or start).
    IdleMinutes(u32),
    /// Start new branches' names with this (already cleaned; "" for none).
    BranchPrefix(String),
    /// The model for titles and reply previews ("" turns them off). Setting one tests it.
    SummaryModel(String),
    /// Archive a thread when its pull request merges.
    ArchiveOnMerge(bool),
    /// Archive threads idle for this many days (0: never).
    ArchiveIdleDays(u32),
}

/// A secret on its way in: sent once, never shown in logs or sent back.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}
