//! What the agents' own records say about their use: how full a thread's context is, and
//! the agent accounts' usage limits (Claude's, ChatGPT's). Both come from the harness in the
//! thread's VM (`flotilla_core::meter`), not from boat, whose events carry no token counts.
//! Each number is the harness's or its service's own; one that isn't known is left out.

use serde::{Deserialize, Serialize};

/// How much of its model's context window a thread's conversation fills, as its harness
/// recorded it after its latest reply: that reply's input (cached or not) and output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUse {
    /// Tokens in the context.
    pub used: u64,
    /// The model's window as the harness counts it, once known.
    #[serde(default)]
    pub window: Option<u64>,
    /// The model the harness ran (its own id for it).
    #[serde(default)]
    pub model: Option<String>,
    /// The account whose limits the thread spends ("claude", "chatgpt"), if known.
    #[serde(default)]
    pub account: Option<String>,
    /// When the harness recorded it (ms since the epoch).
    #[serde(default)]
    pub at_ms: Option<i64>,
}

impl ContextUse {
    /// The share of the window used, 0 to 100, once the window is known.
    pub fn percent(&self) -> Option<f64> {
        self.window
            .filter(|w| *w > 0)
            .map(|w| (self.used as f64 / w as f64 * 100.).min(100.))
    }
}

/// An agent account's usage limits, as its service last reported them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLimits {
    /// "claude" or "chatgpt".
    pub account: String,
    /// The plan, when the service names it ("pro").
    #[serde(default)]
    pub plan: Option<String>,
    pub windows: Vec<LimitWindow>,
    /// When they were read (ms since the epoch).
    pub read_ms: i64,
}

impl AccountLimits {
    /// The account's name as people know it.
    pub fn name(&self) -> &str {
        account_name(&self.account)
    }
}

/// An account's name as people know it ("Claude", "ChatGPT").
pub fn account_name(account: &str) -> &str {
    match account {
        "claude" => "Claude",
        "chatgpt" => "ChatGPT",
        other => other,
    }
}

/// One limit: a share of the account's allowance used in a window that resets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitWindow {
    /// Stable within the account ("five_hour", "seven_day", "seven_day_fable", "primary").
    pub id: String,
    /// "5-hour", "Weekly", "Weekly · Fable".
    pub label: String,
    /// Percent used, 0 to 100.
    pub used: u8,
    /// When it resets (RFC 3339), if told.
    #[serde(default)]
    pub resets_at: Option<String>,
    /// Its length in minutes, if told.
    #[serde(default)]
    pub minutes: Option<u32>,
    /// The model it's for alone ("Fable"), when it isn't the whole account's.
    #[serde(default)]
    pub scope: Option<String>,
    /// This window's latest reading from a day or more before this one, as the server kept it:
    /// what its pace over the last day is worked out from. None in its first day, or with
    /// none kept that long ago.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earlier: Option<LimitReading>,
}

/// A window's use as read at a moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitReading {
    /// Percent used, 0 to 100.
    pub used: u8,
    /// When it was read (ms since the epoch).
    pub at_ms: i64,
}

impl LimitWindow {
    /// All used up until it resets.
    pub fn reached(&self) -> bool {
        self.used >= 100
    }

    /// It limits a thread on `model` (the harness's id for it): the whole account's do, and
    /// one for a model alone only threads on that model.
    pub fn applies_to(&self, model: Option<&str>) -> bool {
        match (&self.scope, model) {
            (None, _) => true,
            (Some(scope), Some(model)) => model
                .to_ascii_lowercase()
                .contains(&scope.to_ascii_lowercase()),
            (Some(_), None) => false,
        }
    }
}
