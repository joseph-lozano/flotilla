//! What the agents spent: their tokens by model, as their harnesses recorded each request in
//! the thread's VM (`flotilla_core::spend`), and what they'd cost at the API's list prices, as
//! T3 Code's Usage estimates it. An estimate, not the subscription's bill. A number that isn't
//! known (a model with no public price) is left out, never guessed.

use serde::{Deserialize, Serialize};

/// The agents' use, hour by hour (UTC) over the last 31 days, each hour's tokens priced. Each
/// device adds the hours up into its own days (`flotilla_model::spend`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentUsage {
    /// Oldest first.
    pub hours: Vec<UsageHour>,
    /// When the VMs were last checked (ms since the epoch); None before the first check.
    #[serde(default)]
    pub read_ms: Option<i64>,
    /// When the model prices were fetched, if they have been.
    #[serde(default)]
    pub prices_ms: Option<i64>,
}

/// A quarter hour's use of one model at one speed. One may have more than one for a model and
/// speed (requests priced at a long context's rates, or whose cost the harness gave, are kept
/// apart): they add up.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageHour {
    /// The quarter hour's start (ms since the epoch, on a quarter hour in UTC, so every time
    /// zone's days begin at one; from servers before them, on the hour).
    pub hour_ms: i64,
    /// The harness: "claude-code", "codex" or "opencode".
    pub provider: String,
    /// The model as the harness ran it.
    pub model: String,
    #[serde(default)]
    pub speed: Speed,
    pub tokens: TokenTotals,
    /// The requests.
    pub requests: u64,
    /// What it would cost at list prices, or None for a model with no known price.
    #[serde(default)]
    pub cost: Option<UsageCost>,
}

/// Tokens by kind, as T3 counts them. Input is only what wasn't read from the cache; output
/// includes reasoning, which is also counted apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenTotals {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    #[serde(default)]
    pub reasoning: u64,
}

impl TokenTotals {
    /// Every token processed: input, cached or not, and output (reasoning is in output).
    pub fn total(&self) -> u64 {
        self.input + self.cache_read + self.cache_write + self.output
    }

    pub fn add(&mut self, o: &TokenTotals) {
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
        self.reasoning += o.reasoning;
    }
}

/// The speed a request ran at: Fast and Ultrafast cost more than Standard.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    #[default]
    Standard,
    Fast,
    Ultrafast,
}

impl Speed {
    pub fn as_str(self) -> &'static str {
        match self {
            Speed::Standard => "standard",
            Speed::Fast => "fast",
            Speed::Ultrafast => "ultrafast",
        }
    }

    pub fn parse(s: &str) -> Speed {
        match s {
            "fast" => Speed::Fast,
            "ultrafast" => Speed::Ultrafast,
            _ => Speed::Standard,
        }
    }
}

/// An estimated cost in US dollars, by kind of token. `other` is a cost the harness reported
/// for a model with no public price, which can't be split. `premium` is the part of the whole
/// that Fast or Ultrafast added over Standard (None when that can't be known: a cost the
/// harness gave at Fast for a model with no public price); `saved` what reading from the cache
/// saved over sending it all as input. Neither is added to the whole again.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UsageCost {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
    #[serde(default)]
    pub other: f64,
    #[serde(default)]
    pub premium: Option<f64>,
    #[serde(default)]
    pub saved: f64,
}

impl Default for UsageCost {
    /// Nothing spent: a premium of nothing, known.
    fn default() -> UsageCost {
        UsageCost {
            input: 0.,
            cache_read: 0.,
            cache_write: 0.,
            output: 0.,
            other: 0.,
            premium: Some(0.),
            saved: 0.,
        }
    }
}

impl UsageCost {
    /// The whole cost.
    pub fn total(&self) -> f64 {
        self.input + self.cache_read + self.cache_write + self.output + self.other
    }

    pub fn add(&mut self, o: &UsageCost) {
        self.input += o.input;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.output += o.output;
        self.other += o.other;
        // Unknown in a part is unknown in the whole.
        self.premium = self.premium.zip(o.premium).map(|(a, b)| a + b);
        self.saved += o.saved;
    }
}

/// Where each provider shows its account's own usage: the only way out of the usage view.
pub fn usage_page(account: &str) -> Option<&'static str> {
    match account {
        "claude" => Some("https://claude.ai/settings/usage"),
        "chatgpt" => Some("https://chatgpt.com/codex/settings/usage"),
        _ => None,
    }
}
