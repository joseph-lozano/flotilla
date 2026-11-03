//! Flotilla's wire types: the data the views carry, the views themselves, requests and their
//! answers, and what else a client is told. serde only (no SQLite, HTTP or GPUI), so it builds
//! for wasm and iOS. flotilla-core re-exports the data types where they used to live.

pub mod ask;
pub mod catalog;
pub mod cmd;
pub mod delta;
pub mod github;
pub mod history;
pub mod limits;
pub mod msg;
pub mod pair;
pub mod public;
pub mod replica;
pub mod schedule;
pub mod search;
pub mod spend;
pub mod thread;
pub mod views;
pub mod wire;

pub use catalog::{Catalog, EffortOptions, Model, Provider, ThreadSettings};
pub use cmd::{
    PairingCode, PushMode, PushRelay, PushToken, Reply, Request, Secret, ServiceCmd, Setting,
};
pub use github::{
    BranchChoices, Check, GhStatus, OpenPr, Project, PullRequest, RepoInfo, SetupRun, SetupScript,
    Suggestions,
};
pub use history::{Cut, History, HistoryPage, PageMark};
pub use limits::{AccountLimits, ContextUse, LimitReading, LimitWindow};
pub use msg::{
    Alert, AlertKind, ClientMsg, Connection, Note, ReqId, ServerMsg, View, ViewKey, through_json,
};
pub use pair::Scope;
pub use public::{PublicFor, PublicLink};
pub use replica::{Applied, Replica, Taken};
pub use schedule::{Fresh, Run, RunOutcome, Schedule, ScheduleSpec, Schedules, Target, When};
pub use search::{HitKind, SearchHit};
pub use spend::{AgentUsage, Speed, TokenTotals, UsageCost, UsageHour};
pub use thread::{
    Activity, Attached, Entry, Environment, ExtraStarts, HookRun, Kind, Lineage, Link, Mentions,
    Message, NewThread, OnBranch, RawEvent, ScriptFailure, Sent, SentPrompt, ServiceRun, Skill,
    StartStages, Subagent, Thread, ThreadServices, ThreadState, ThreadSummary, Usage, UserSkill,
};
pub use views::{
    CheckOf, ChildCard, ChildState, DeviceRow, Devices, Diagnosis, Doctor, EngineState, Health,
    ProjectDetail, ProjectRow, Settings, Sidebar, SummaryStatus, SyncStatus, ThreadDetail,
    ThreadProject,
};

/// The protocol's version. A client and server that differ refuse each other (the server
/// answers the upgrade with 426); fields added with `#[serde(default)]` don't need a new one.
pub const PROTOCOL: u32 = 1;

#[cfg(test)]
mod tests;
