//! Scheduled prompts: a prompt sent later, once or again and again, to a new thread each run or
//! to one thread as a follow-up. The server keeps and runs them (the engine's schedule
//! worker); `flotilla_model::schedule` works out when. See `design/scheduled-prompts.md`.

use crate::OnBranch;
use serde::{Deserialize, Serialize};

/// A schedule as a client sets it. `schedule.put` makes one, or replaces the one with its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleSpec {
    /// A UUID the client made: put again, it replaces the same one (a resend is harmless).
    pub id: String,
    pub prompt: String,
    pub target: Target,
    pub when: When,
    /// The IANA time zone its times are in, the device's that set it (`Europe/Paris`).
    pub zone: String,
}

/// Where each run's prompt goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// A new thread each run, started as New thread starts one.
    New(Fresh),
    /// This thread, as a follow-up. A run while its turn is running waits for it to end.
    Thread(String),
}

impl Target {
    pub fn thread(&self) -> Option<&str> {
        match self {
            Target::Thread(t) => Some(t),
            Target::New(_) => None,
        }
    }
}

/// What a new thread starts with: what New thread takes, less the prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fresh {
    /// boat environment name or id (a project's own, for a project thread).
    pub environment: String,
    /// The project (GitHub repo); `None`: No project.
    pub project_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub fast: bool,
    pub machine_type: Option<String>,
    /// A project thread on a branch already on GitHub; `None`: a new branch each run.
    pub on_branch: Option<OnBranch>,
}

/// When it runs, in the schedule's zone, at most once a day (a one-off aside). Times of day
/// are minutes after midnight; days are ISO weekdays, 1 (Monday) to 7 (Sunday).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Once, at this moment (ms since the epoch).
    Once {
        at_ms: i64,
    },
    /// Every `every` hours from `from_ms`, its first run. No client makes one any more: kept
    /// so those set before runs had to be a day apart still read (and pause, under 24).
    Hours {
        every: u32,
        from_ms: i64,
    },
    Daily {
        at: u32,
    },
    /// Monday to Friday.
    Weekdays {
        at: u32,
    },
    Weekly {
        days: Vec<u8>,
        at: u32,
    },
    /// Five fields: minute, hour, day of the month, month, day of the week; one time of day.
    Cron {
        expr: String,
    },
}

impl When {
    pub fn once(&self) -> bool {
        matches!(self, When::Once { .. })
    }
}

/// A schedule as the server keeps it: what was set, and how its runs are going.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub prompt: String,
    pub target: Target,
    pub when: When,
    pub zone: String,
    pub paused: bool,
    /// When it runs next (ms); none once a one-off has run, or while it's paused.
    pub next_ms: Option<i64>,
    /// A run due then is waiting for the thread's turn to end.
    pub waiting_ms: Option<i64>,
    /// Its latest run, or the latest that didn't happen.
    pub last: Option<Run>,
    /// How many runs it has made.
    pub runs: u32,
    /// The device that set it up (its name).
    pub from: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl Schedule {
    pub fn spec(&self) -> ScheduleSpec {
        ScheduleSpec {
            id: self.id.clone(),
            prompt: self.prompt.clone(),
            target: self.target.clone(),
            when: self.when.clone(),
            zone: self.zone.clone(),
        }
    }

    /// A new thread's project, if it starts in one.
    pub fn target_project(&self) -> Option<&str> {
        match &self.target {
            Target::New(f) => f.project_id.as_deref(),
            Target::Thread(_) => None,
        }
    }

    /// A one-off that has run (or that didn't, and won't on its own).
    pub fn done(&self) -> bool {
        self.when.once() && self.next_ms.is_none() && !self.paused && self.waiting_ms.is_none()
    }
}

/// One run, or one that didn't happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// When it ran, or was due.
    pub at_ms: i64,
    #[serde(deserialize_with = "outcome_or_failed")]
    pub outcome: RunOutcome,
    /// The thread it went to, or started.
    pub thread: Option<String>,
    /// Why it didn't run, or why it failed.
    pub why: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// Sent to its thread, or a thread started with it.
    Sent,
    /// Its time passed while the server was off: skipped to the next one.
    Missed,
    /// It couldn't run (`why`), or its send or start failed.
    Failed,
}

fn outcome_or_failed<'de, D: serde::Deserializer<'de>>(d: D) -> Result<RunOutcome, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or(RunOutcome::Failed))
}

/// Every schedule: those due soonest first, then paused ones, then one-offs done.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedules {
    pub schedules: Vec<Schedule>,
}
