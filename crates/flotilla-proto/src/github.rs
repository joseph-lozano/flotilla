//! Projects (GitHub repos Flotilla starts threads in), their setup, and what GitHub says.

use serde::{Deserialize, Serialize};

/// The GitHub host gh signs in to.
pub const HOST: &str = "github.com";
/// `gh auth status --json` arrived in 2.81.
pub const MIN_GH: (u32, u32) = (2, 81);

/// A GitHub repo Flotilla starts threads in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub owner: String,
    pub repo: String,
    pub default_branch: String,
    /// The project's own boat environment, which lists the repo (so boat clones it and the
    /// agent starts in it) and passes GitHub access.
    pub environment_name: String,
    pub created_at: String,
    /// The stopped, set-up VM new threads copy, and the commit it's set up at.
    pub template_sandbox_id: Option<String>,
    pub template_sha: Option<String>,
    pub template_built_at: Option<String>,
    pub prev_template_sandbox_id: Option<String>,
    /// The default branch's head on GitHub, last time Flotilla checked. Not noticed: it
    /// changes every minute with no `Changed`, so a view may carry an old value. Nothing
    /// shows it (or `checked_at`); a view that starts to must have them noticed first.
    pub head_sha: Option<String>,
    pub head_etag: Option<String>,
    pub checked_at: Option<String>,
}

impl Project {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// Where boat clones the repo in a VM, relative to `/home/user`.
    pub fn dir(&self) -> &str {
        &self.repo
    }
}

/// One run of a project's setup scripts on a new commit, preparing the VM its new threads
/// copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupRun {
    pub id: String,
    pub project_id: String,
    /// `update` (copy the last prepared VM, pull, rerun the scripts) or `fresh` (a new VM).
    pub kind: String,
    /// Why it ran: `added`, `main`, `merge`, `manual`, `scripts`, `refresh`.
    pub trigger: String,
    pub sha: String,
    pub sandbox_id: Option<String>,
    /// `running`, `done`, `failed` or `cancelled`.
    pub state: String,
    /// The stage running, or the one that failed.
    pub step: Option<String>,
    pub error: Option<String>,
    pub log: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

impl SetupRun {
    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

/// A setup script for a project: run in the repo's folder, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupScript {
    pub command: String,
    /// Run only when setting up from scratch (a fresh clone), not on every update.
    pub fresh_only: bool,
}

impl SetupScript {
    pub fn every(command: &str) -> SetupScript {
        SetupScript {
            command: command.into(),
            fresh_only: false,
        }
    }
}

/// What Add project suggests for a repo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestions {
    pub scripts: Vec<SetupScript>,
    /// The repo has `.agents/setup`, which runs in every setup anyway.
    pub agents_setup: bool,
}

/// A repo gh's account can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    pub owner: String,
    pub name: String,
    pub private: bool,
    pub default_branch: String,
    pub pushed_at: Option<String>,
    pub description: Option<String>,
}

impl RepoInfo {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// An open pull request a thread could start on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPr {
    pub number: i64,
    pub title: String,
    pub url: String,
    /// Its branch, and the branch it merges into.
    pub head: String,
    pub base: String,
    pub draft: bool,
    /// Its branch is in someone's fork, which boat's GitHub token can't push to.
    pub from_fork: bool,
}

/// A project's open pull requests and branches, for New thread's branch picker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchChoices {
    pub prs: Vec<OpenPr>,
    pub branches: Vec<String>,
}

/// What gh can do for Flotilla, where the engine runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GhStatus {
    /// Not looked yet.
    Unknown,
    /// gh isn't installed, or isn't anywhere Flotilla looks.
    NotFound,
    TooOld {
        version: String,
    },
    NotSignedIn {
        version: String,
    },
    /// `version`: gh's, or `GH_TOKEN` when there's only the token.
    SignedIn {
        login: String,
        version: String,
        scopes: Vec<String>,
    },
    /// gh ran but answered something unexpected, or didn't answer in time.
    Failed(String),
}

impl GhStatus {
    pub fn signed_in(&self) -> bool {
        matches!(self, GhStatus::SignedIn { .. })
    }

    pub fn login(&self) -> Option<&str> {
        match self {
            GhStatus::SignedIn { login, .. } => Some(login),
            _ => None,
        }
    }

    /// One line for Settings: whose account, or exactly what to do.
    pub fn message(&self) -> String {
        match self {
            GhStatus::Unknown => "Checking the GitHub CLI…".into(),
            GhStatus::NotFound => "gh not found. Install it (https://cli.github.com), run “gh auth login”, then Check again.".into(),
            GhStatus::TooOld { version } => format!("Flotilla needs gh {}.{} or newer (found {version}). Update gh, then Check again.", MIN_GH.0, MIN_GH.1),
            GhStatus::NotSignedIn { .. } => format!("gh isn't signed in to {HOST}. Run “gh auth login --hostname {HOST}”, then Check again."),
            // Without gh, `version` is the variable the token came from.
            GhStatus::SignedIn { login, version, .. } if version.starts_with(|c: char| c.is_ascii_alphabetic()) => format!("Signed in as {login} ({version})"),
            GhStatus::SignedIn { login, version, .. } => format!("Signed in as {login} (gh {version})"),
            GhStatus::Failed(e) => format!("gh didn't answer as expected ({e}). Run “gh auth status” to see why."),
        }
    }
}

/// A check on a pull request's head commit, reduced to what Flotilla shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    /// `pass`, `fail` or `pending`.
    pub state: String,
    pub url: Option<String>,
    /// The check run's title or the status's description, e.g. "2 tests failed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A thread's pull request.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: i64,
    pub url: String,
    pub title: String,
    /// Its branch, and the branch it merges into.
    pub head: String,
    pub base: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub is_draft: bool,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    pub mergeable: Option<String>,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or none.
    pub review: Option<String>,
    pub additions: i64,
    pub deletions: i64,
    pub checks: Vec<Check>,
    /// When anything on it last changed (RFC 3339), for how often to read it again.
    pub updated_at: Option<String>,
    /// When it merged (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
}

impl PullRequest {
    pub fn failing(&self) -> usize {
        self.checks.iter().filter(|c| c.state == "fail").count()
    }

    pub fn pending(&self) -> usize {
        self.checks.iter().filter(|c| c.state == "pending").count()
    }

    pub fn passing(&self) -> usize {
        self.checks.iter().filter(|c| c.state == "pass").count()
    }

    /// Open, not a draft, no conflicts, no failing or pending checks, and no review owed.
    pub fn can_merge(&self) -> bool {
        self.state == "OPEN"
            && !self.is_draft
            && self.mergeable.as_deref() == Some("MERGEABLE")
            && self.failing() == 0
            && self.pending() == 0
            && !matches!(
                self.review.as_deref(),
                Some("REVIEW_REQUIRED" | "CHANGES_REQUESTED")
            )
    }
}
