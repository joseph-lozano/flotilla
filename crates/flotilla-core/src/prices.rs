//! The models' list prices, as T3 Code's Usage gets them: LiteLLM's public table
//! (`model_prices_and_context_window.json`), fetched by the machine that runs the engine at
//! most once a day (a refresh asks again after a minute, as does a failed fetch), kept beside
//! the database for when it can't be fetched. A model the table doesn't price, or that only a
//! family name names ("opus"), has no price: its tokens count, its cost isn't guessed.
//! Pricing follows T3's `usagePricing.ts`: each kind of token at its rate (a missing cache
//! rate is the input rate), Fast and Ultrafast at their own rates (or a multiple of the
//! standard ones), and a cost the harness gave kept as the total. Beyond T3, as LiteLLM prices
//! a request: cache writes kept for an hour at their own rate, and a request whose prompt is
//! longer than a threshold the table gives (`_above_272k_tokens`) at that tier's rates.

use crate::util::now_ms;
use flotilla_proto::{Speed, TokenTotals, UsageCost};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Where the table comes from, and the same through GitHub's API when that fails.
pub const SOURCE: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const SOURCE_API: &str = "https://api.github.com/repos/BerriAI/litellm/contents/model_prices_and_context_window.json?ref=main";
/// How long a fetched table is good for.
const FRESH_MS: i64 = 24 * 3_600_000;
/// The least time between fetches when one is asked for, or after one failed.
const FLOOR_MS: i64 = 60_000;
const FILE: &str = "model-prices.json";

/// A model's rates per token. `cache_write` is a write kept five minutes, `cache_write_1h` one
/// kept an hour.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub cache_write_1h: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelRate {
    pub standard: Rates,
    pub fast: Option<Rates>,
    pub ultrafast: Option<Rates>,
    /// The long-context tiers, by threshold, lowest first: a request whose prompt (its input,
    /// cached or not) is longer than a threshold is at that tier's rates, every kind of token.
    pub tiers: Vec<(u64, ModelRate)>,
}

impl ModelRate {
    fn at(&self, speed: Speed) -> Rates {
        match speed {
            Speed::Standard => None,
            Speed::Fast => self.fast,
            Speed::Ultrafast => self.ultrafast,
        }
        .unwrap_or(self.standard)
    }

    /// The rates for requests kept at `tier` ([`RateTable::tier`]): the highest tier at or below
    /// it (a tier the table no longer gives prices at the one below), else the base rates.
    fn tier(&self, tier: u64) -> &ModelRate {
        self.tiers
            .iter()
            .rev()
            .find(|(above, _)| *above <= tier)
            .map_or(self, |(_, r)| r)
    }
}

/// Prices by model, its name in lower case (and its bare name, after any `provider/`, when
/// that's unambiguous).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateTable {
    rates: HashMap<String, ModelRate>,
}

fn finite(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|n| n.is_finite())
}

const WRITE_1H: &str = "cache_creation_input_token_cost_above_1hr";

/// An entry's rates with `suffix` ("", "_priority", "_ultrafast"). A cache rate it doesn't
/// give follows the standard one's ratio to input, or else is the input rate (an hour's write,
/// the five minutes' rate).
fn rates(e: &Value, suffix: &str, standard: Option<&Rates>) -> Option<Rates> {
    let input = finite(e.get(format!("input_cost_per_token{suffix}")))?;
    let output = finite(e.get(format!("output_cost_per_token{suffix}")))?;
    let cache = |name: &str, of: fn(&Rates) -> f64, or: f64| {
        finite(e.get(format!("{name}{suffix}"))).unwrap_or(match standard {
            Some(s) if s.input > 0. => of(s) / s.input * input,
            _ => or,
        })
    };
    let cache_write = cache("cache_creation_input_token_cost", |r| r.cache_write, input);
    Some(Rates {
        input,
        output,
        cache_read: cache("cache_read_input_token_cost", |r| r.cache_read, input),
        cache_write,
        cache_write_1h: cache(WRITE_1H, |r| r.cache_write_1h, cache_write),
    })
}

/// An entry's rates past a threshold (`tier`: "_above_272k_tokens") with `suffix`, if it gives
/// that input rate. A rate it doesn't give there is `base`'s, as LiteLLM prices it (an hour's
/// write follows the tier's five-minute one, as `base`'s does).
fn tier_rates(e: &Value, tier: &str, suffix: &str, base: &Rates) -> Option<Rates> {
    let rate = |name: &str| finite(e.get(format!("{name}{tier}{suffix}")));
    let cache_write = rate("cache_creation_input_token_cost").unwrap_or(base.cache_write);
    let ratio = if base.cache_write > 0. {
        base.cache_write_1h / base.cache_write
    } else {
        1.
    };
    Some(Rates {
        input: rate("input_cost_per_token")?,
        output: rate("output_cost_per_token").unwrap_or(base.output),
        cache_read: rate("cache_read_input_token_cost").unwrap_or(base.cache_read),
        cache_write,
        cache_write_1h: rate(WRITE_1H).unwrap_or(cache_write * ratio),
    })
}

/// The thresholds an entry gives (`input_cost_per_token_above_272k_tokens`): (tokens, suffix).
fn thresholds(e: &serde_json::Map<String, Value>) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = e
        .keys()
        .filter_map(|k| {
            let label = k
                .strip_prefix("input_cost_per_token_above_")?
                .strip_suffix("_tokens")?;
            let (digits, by) = label.strip_suffix('k').map_or((label, 1), |d| (d, 1000));
            let n = digits.parse::<u64>().ok()?.checked_mul(by)?;
            (n > 0).then(|| (n, format!("_above_{label}_tokens")))
        })
        .collect();
    out.sort();
    out.dedup_by_key(|(n, _)| *n);
    out
}

fn scaled(r: Rates, by: f64) -> Rates {
    Rates {
        input: r.input * by,
        output: r.output * by,
        cache_read: r.cache_read * by,
        cache_write: r.cache_write * by,
        cache_write_1h: r.cache_write_1h * by,
    }
}

fn bare(key: &str) -> &str {
    key.rsplit_once('/').map_or(key, |(_, b)| b)
}

/// Family names say no generation, and a placeholder was never billed.
const UNPRICEABLE: [&str; 6] = [
    "<synthetic>",
    "synthetic",
    "opus",
    "sonnet",
    "haiku",
    "fable",
];

impl RateTable {
    /// LiteLLM's table. An entry without both an input and an output rate is dropped (it
    /// would understate the cost).
    pub fn parse(doc: &Value) -> RateTable {
        let mut rates = HashMap::new();
        let Some(all) = doc.as_object() else {
            return RateTable::default();
        };
        for (name, e) in all {
            let Some(fields) = e.as_object() else {
                continue;
            };
            let Some(standard) = self::rates(e, "", None) else {
                continue;
            };
            let key = name.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            let multiple = finite(e.get("provider_specific_entry").and_then(|p| p.get("fast")))
                .filter(|m| *m > 0.);
            let fast = match multiple {
                Some(m) => Some(scaled(standard, m)),
                None => self::rates(e, "_priority", Some(&standard)),
            };
            let ultrafast = self::rates(e, "_ultrafast", Some(&standard));
            // Each tier's speeds as the base's: a multiple of its standard rates, or their own
            // (a kind not given there at the base's rate at that speed).
            let tiers = thresholds(fields)
                .into_iter()
                .filter_map(|(above, tier)| {
                    let at = tier_rates(e, &tier, "", &standard)?;
                    let at_fast = match multiple {
                        Some(m) => Some(scaled(at, m)),
                        None => tier_rates(e, &tier, "_priority", fast.as_ref().unwrap_or(&at)),
                    };
                    let at_ultrafast =
                        tier_rates(e, &tier, "_ultrafast", ultrafast.as_ref().unwrap_or(&at));
                    Some((
                        above,
                        ModelRate {
                            standard: at,
                            fast: at_fast,
                            ultrafast: at_ultrafast,
                            tiers: vec![],
                        },
                    ))
                })
                .collect();
            rates.insert(
                key,
                ModelRate {
                    standard,
                    fast,
                    ultrafast,
                    tiers,
                },
            );
        }
        // `provider/model` also answers to `model`, unless two disagree on its price (a
        // reseller that leaves out a speed doesn't disagree).
        let mut aliases: HashMap<String, Option<ModelRate>> = HashMap::new();
        for (key, rate) in &rates {
            let alias = bare(key);
            if alias.is_empty() || alias == key || rates.contains_key(alias) {
                continue;
            }
            match aliases.get(alias) {
                None => {
                    aliases.insert(alias.to_string(), Some(rate.clone()));
                }
                Some(Some(held)) => {
                    let merged = merge(held, rate);
                    aliases.insert(alias.to_string(), merged);
                }
                Some(None) => {}
            }
        }
        rates.extend(aliases.into_iter().filter_map(|(k, r)| r.map(|r| (k, r))));
        RateTable { rates }
    }

    pub fn len(&self) -> usize {
        self.rates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rates.is_empty()
    }

    /// A model's prices, by the name its harness gave (Claude's bracketed variants, "[1m]", at
    /// the model's prices: a long prompt is at its tier).
    pub fn lookup(&self, model: &str) -> Option<&ModelRate> {
        let key = model.trim().to_lowercase();
        let key = key.split('[').next().unwrap_or("");
        let name = bare(key);
        if name.is_empty() || UNPRICEABLE.contains(&name) {
            return None;
        }
        self.rates.get(key)
    }

    /// A request's context tier: the highest threshold of `model`'s that a prompt of `prompt`
    /// tokens (its input, cached or not) is longer than; 0 for none. Decided as each request
    /// is counted, and kept with it, as rows of requests can't be split again.
    pub fn tier(&self, model: &str, prompt: u64) -> u64 {
        self.lookup(model)
            .and_then(|r| r.tiers.iter().rev().find(|(above, _)| prompt > *above))
            .map_or(0, |(above, _)| *above)
    }
}

fn merge(a: &ModelRate, b: &ModelRate) -> Option<ModelRate> {
    let fast = a.fast.or(b.fast);
    let ultrafast = a.ultrafast.or(b.ultrafast);
    let tiers = if a.tiers.is_empty() {
        &b.tiers
    } else {
        &a.tiers
    };
    (a.standard == b.standard
        && b.fast.is_none_or(|f| Some(f) == fast)
        && b.ultrafast.is_none_or(|u| Some(u) == ultrafast)
        && (b.tiers.is_empty() || &b.tiers == tiers))
        .then(|| ModelRate {
            standard: a.standard,
            fast,
            ultrafast,
            tiers: tiers.clone(),
        })
}

/// By kind; `write_1h` of the cache writes were kept an hour.
fn by_kind(t: &TokenTotals, write_1h: u64, r: &Rates) -> [f64; 4] {
    let hour = write_1h.min(t.cache_write);
    let write = (t.cache_write - hour) as f64 * r.cache_write + hour as f64 * r.cache_write_1h;
    [
        t.input as f64 * r.input,
        t.cache_read as f64 * r.cache_read,
        write,
        t.output as f64 * r.output,
    ]
}

/// What `tokens` of `model` at `speed` cost at list prices, by kind (reasoning is in output, so
/// it isn't charged again): at the context tier they were kept at ([`RateTable::tier`]), with
/// `write_1h` of their cache writes at an hour's rate. `reported`: the cost the harness gave,
/// which is the total (split as the list prices split it, or, for a model with no price, as
/// `other`). None: no price known.
pub fn price(
    table: &RateTable,
    model: &str,
    speed: Speed,
    tier: u64,
    tokens: &TokenTotals,
    write_1h: u64,
    reported: Option<f64>,
) -> Option<UsageCost> {
    let reported = reported.filter(|r| r.is_finite());
    // A reported cost that can't be split: what Fast added to it isn't known.
    let unsplit = |r: f64| UsageCost {
        other: r,
        premium: (speed == Speed::Standard).then_some(0.),
        ..UsageCost::default()
    };
    let Some(rate) = table.lookup(model) else {
        return reported.map(unsplit);
    };
    let rate = rate.tier(tier);
    let at = rate.at(speed);
    let list = by_kind(tokens, write_1h, &at);
    let list_total: f64 = list.iter().sum();
    if let Some(r) = reported
        && list_total <= 0.
    {
        return Some(unsplit(r));
    }
    let premium = if speed == Speed::Standard {
        0.
    } else {
        list_total
            - by_kind(tokens, write_1h, &rate.standard)
                .iter()
                .sum::<f64>()
    };
    let scale = reported.map_or(1., |r| r / list_total);
    Some(UsageCost {
        input: list[0] * scale,
        cache_read: list[1] * scale,
        cache_write: list[2] * scale,
        output: list[3] * scale,
        other: 0.,
        premium: Some(premium * scale),
        saved: tokens.cache_read as f64 * (at.input - at.cache_read),
    })
}

/// The table for a database, kept beside it: what's loaded, when it was fetched, and when a
/// fetch last failed.
pub struct Prices {
    path: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    table: Arc<RateTable>,
    fetched_ms: Option<i64>,
    failed_ms: Option<i64>,
    loaded: bool,
}

/// The prices for the database at `db`: one copy per database in a process.
pub fn for_db(db: &Path) -> Arc<Prices> {
    static ALL: OnceLock<Mutex<HashMap<PathBuf, Arc<Prices>>>> = OnceLock::new();
    let path = db.with_file_name(FILE);
    let mut all = ALL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    all.entry(path.clone())
        .or_insert_with(|| {
            Arc::new(Prices {
                path,
                state: Mutex::default(),
            })
        })
        .clone()
}

impl Prices {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s.loaded {
            s.loaded = true;
            if let Some((fetched, doc)) = std::fs::read_to_string(&self.path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| Some((v["fetched_ms"].as_i64()?, v.get("document")?.clone())))
            {
                let table = RateTable::parse(&doc);
                if !table.is_empty() {
                    s.table = Arc::new(table);
                    s.fetched_ms = Some(fetched);
                }
            }
        }
        s
    }

    /// The table as it is (read from beside the database the first time), and when it was
    /// fetched.
    pub fn table(&self) -> (Arc<RateTable>, Option<i64>) {
        let s = self.state();
        (s.table.clone(), s.fetched_ms)
    }

    /// The table to count requests by (each one's context tier is decided as it's counted),
    /// fetched first if there's none yet. None while there's still none: nothing is counted
    /// then, so the records are read again next time rather than kept at the base tier.
    pub fn for_counting(&self) -> Option<Arc<RateTable>> {
        self.for_counting_with(now_ms(), fetch)
    }

    pub fn for_counting_with(
        &self,
        now: i64,
        fetch: impl Fn() -> Option<Value>,
    ) -> Option<Arc<RateTable>> {
        if self.table().0.is_empty() {
            self.ensure_with(false, now, fetch);
        }
        Some(self.table().0).filter(|t| !t.is_empty())
    }

    /// Fetch the table if it's due: older than a day, or `force` (a refresh) and older than a
    /// minute; not within a minute of a failed fetch. Whether it changed.
    pub fn ensure(&self, force: bool) -> bool {
        self.ensure_with(force, now_ms(), fetch)
    }

    pub fn ensure_with(&self, force: bool, now: i64, fetch: impl Fn() -> Option<Value>) -> bool {
        let mut s = self.state();
        let age = if force { FLOOR_MS } else { FRESH_MS };
        if s.fetched_ms.is_some_and(|f| now - f < age)
            || s.failed_ms.is_some_and(|f| now - f < FLOOR_MS)
        {
            return false;
        }
        let Some(doc) = fetch() else {
            s.failed_ms = Some(now);
            return false;
        };
        let table = RateTable::parse(&doc);
        if table.is_empty() {
            s.failed_ms = Some(now);
            return false;
        }
        let kept = serde_json::json!({"fetched_ms": now, "document": doc});
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, kept.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
        s.table = Arc::new(table);
        s.fetched_ms = Some(now);
        s.failed_ms = None;
        true
    }
}

/// LiteLLM's table, from GitHub (its API if the raw file fails), in 10 seconds at most each.
fn fetch() -> Option<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("flotilla")
        .build()
        .ok()?;
    let get = |url: &str, accept: &str| -> Option<Value> {
        let res = client.get(url).header("accept", accept).send().ok()?;
        if !res.status().is_success() {
            return None;
        }
        res.json::<Value>().ok().filter(|v| v.is_object())
    };
    get(SOURCE, "application/json").or_else(|| get(SOURCE_API, "application/vnd.github.raw+json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/prices/litellm.json");

    fn table() -> RateTable {
        RateTable::parse(&serde_json::from_str(&std::fs::read_to_string(DATA).unwrap()).unwrap())
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn prices_each_kind_of_token_at_its_rate() {
        let t = table();
        // Recorded from LiteLLM on 2026-11-01: $0.10 in, $0.50 out, $0.01 cache read, $0.125
        // cache write per million.
        let tokens = TokenTotals {
            input: 2_000_000,
            cache_read: 10_000_000,
            cache_write: 1_000_000,
            output: 1_000_000,
            reasoning: 0,
        };
        let c = price(&t, "claude-haiku-5-5", Speed::Standard, 0, &tokens, 0, None).unwrap();
        assert!(
            close(c.input, 0.2)
                && close(c.cache_read, 0.1)
                && close(c.cache_write, 0.125)
                && close(c.output, 0.5),
            "{c:?}"
        );
        assert!(
            close(c.total(), 0.925)
                && close(c.premium.unwrap(), 0.)
                && close(c.saved, 10. * (0.1 - 0.01)),
            "{c:?}"
        );
        // A bracketed variant at the base price; a provider's prefix by the bare name.
        assert_eq!(
            price(
                &t,
                "claude-haiku-5-5[1m]",
                Speed::Standard,
                0,
                &tokens,
                0,
                None
            ),
            Some(c)
        );
        // A reseller's entry is its own; one without rates (ChatGPT's) isn't a price.
        assert_eq!(
            t.lookup("azure/gpt-6-luna").map(|r| r.standard),
            t.lookup("gpt-6-luna").map(|r| r.standard)
        );
        assert!(t.lookup("chatgpt/gpt-6-luna").is_none());
    }

    #[test]
    fn fast_costs_its_premium_and_unknown_models_have_no_price() {
        let t = table();
        let tokens = TokenTotals {
            input: 1_000_000,
            output: 1_000_000,
            ..TokenTotals::default()
        };
        // Opus 5.5 is twice the price at Fast (LiteLLM's multiple).
        let c = price(&t, "claude-opus-5-5", Speed::Fast, 0, &tokens, 0, None).unwrap();
        assert!(
            close(c.total(), 48.) && close(c.premium.unwrap(), 24.),
            "{c:?}"
        );
        // GPT-6.1 Sol's priority and ultrafast rates are its own.
        let c = price(&t, "gpt-6.1-sol", Speed::Fast, 0, &tokens, 0, None).unwrap();
        assert!(
            close(c.total(), 24.) && close(c.premium.unwrap(), 12.),
            "{c:?}"
        );
        let c = price(&t, "gpt-6.1-sol", Speed::Ultrafast, 0, &tokens, 0, None).unwrap();
        assert!(
            close(c.total(), 72.) && close(c.premium.unwrap(), 60.),
            "{c:?}"
        );
        // A speed with no rates of its own is priced at Standard: no premium is made up.
        let c = price(&t, "claude-haiku-5-5", Speed::Fast, 0, &tokens, 0, None).unwrap();
        assert!(close(c.premium.unwrap(), 0.), "{c:?}");
        // No price: nothing, unless the harness gave one, which can't be split.
        assert_eq!(
            price(&t, "some-new-model", Speed::Standard, 0, &tokens, 0, None),
            None
        );
        assert_eq!(
            price(&t, "opus", Speed::Standard, 0, &tokens, 0, None),
            None,
            "a family name"
        );
        assert_eq!(
            price(&t, "<synthetic>", Speed::Standard, 0, &tokens, 0, None),
            None
        );
        assert_eq!(
            price(
                &t,
                "some-new-model",
                Speed::Standard,
                0,
                &tokens,
                0,
                Some(1.5)
            ),
            Some(UsageCost {
                other: 1.5,
                ..Default::default()
            })
        );
        // At Fast, what Fast added to a cost that can't be split isn't known.
        assert_eq!(
            price(&t, "some-new-model", Speed::Fast, 0, &tokens, 0, Some(1.5))
                .map(|c| (c.total(), c.premium)),
            Some((1.5, None))
        );
        // A reported cost is the total, split as the list prices split it.
        let c = price(
            &t,
            "claude-haiku-5-5",
            Speed::Standard,
            0,
            &tokens,
            0,
            Some(1.2),
        )
        .unwrap();
        assert!(
            close(c.total(), 1.2) && close(c.input / c.output, 0.2),
            "{c:?}"
        );
    }

    /// A long prompt is at its tier's rates, every kind of token; an hour's cache write at its
    /// own rate. Haiku 5.5's prices past 100k tokens and GPT-6.1 Sol's past 272k, as recorded.
    #[test]
    fn prices_long_prompts_at_their_tier_and_hour_long_writes_at_theirs() {
        let t = table();
        // The tier is the highest threshold a prompt is longer than.
        assert_eq!(
            (
                t.tier("claude-haiku-5-5", 100_000),
                t.tier("claude-haiku-5-5", 100_001)
            ),
            (0, 100_000)
        );
        assert_eq!(
            (
                t.tier("claude-haiku-5-5[1m]", 200_000),
                t.tier("claude-opus-5-5", 900_000),
                t.tier("mystery-1", 900_000)
            ),
            (100_000, 0, 0)
        );
        assert_eq!(t.tier("gpt-6.1-sol", 300_000), 272_000);
        // 200k uncached in and 1k out: $0.50 and $2.50 a million past 100k ($0.10 and $0.50 below).
        let long = TokenTotals {
            input: 200_000,
            output: 1_000,
            ..TokenTotals::default()
        };
        let c = price(
            &t,
            "claude-haiku-5-5",
            Speed::Standard,
            100_000,
            &long,
            0,
            None,
        )
        .unwrap();
        assert!(
            close(c.total(), 0.1025) && close(c.input, 0.1) && close(c.output, 0.0025),
            "{c:?}"
        );
        assert!(close(
            price(&t, "claude-haiku-5-5", Speed::Standard, 0, &long, 0, None)
                .unwrap()
                .total(),
            0.0205
        ));
        // Claude's 1M-context name is the model, at its tier too.
        assert_eq!(
            price(
                &t,
                "claude-haiku-5-5[1m]",
                Speed::Standard,
                100_000,
                &long,
                0,
                None
            ),
            Some(c)
        );
        // A tier the table no longer gives is at the highest it does below it.
        assert_eq!(
            price(
                &t,
                "claude-haiku-5-5",
                Speed::Standard,
                150_000,
                &long,
                0,
                None
            ),
            Some(c)
        );
        assert!(close(
            price(
                &t,
                "claude-haiku-5-5",
                Speed::Standard,
                50_000,
                &long,
                0,
                None
            )
            .unwrap()
            .total(),
            0.0205
        ));
        // Cache writes kept an hour: $0.20 a million ($0.125 for five minutes), and $1 past 100k.
        let writes = TokenTotals {
            cache_write: 10_000,
            ..TokenTotals::default()
        };
        let c = price(
            &t,
            "claude-haiku-5-5",
            Speed::Standard,
            0,
            &writes,
            4_000,
            None,
        )
        .unwrap();
        assert!(
            close(c.cache_write, 6_000. * 0.125e-6 + 4_000. * 0.2e-6),
            "{c:?}"
        );
        let c = price(
            &t,
            "claude-haiku-5-5",
            Speed::Standard,
            100_000,
            &writes,
            4_000,
            None,
        )
        .unwrap();
        assert!(
            close(c.cache_write, 6_000. * 0.625e-6 + 4_000. * 1e-6),
            "{c:?}"
        );
        // More hour-long writes than writes: no more than all of them.
        let c = price(
            &t,
            "claude-haiku-5-5",
            Speed::Standard,
            0,
            &writes,
            99_999,
            None,
        )
        .unwrap();
        assert!(close(c.cache_write, 10_000. * 0.2e-6), "{c:?}");
        // Fast as a multiple: an hour's write too (Opus 5.5's $8 a million, twice at Fast).
        let c = price(&t, "claude-opus-5-5", Speed::Fast, 0, &writes, 10_000, None).unwrap();
        assert!(
            close(c.cache_write, 10_000. * 16e-6) && close(c.premium.unwrap(), 10_000. * 8e-6),
            "{c:?}"
        );
        // GPT-6.1 Sol past 272k at its priority tier's rates ($8 and $30 a million), its premium
        // over the tier's standard ($4 and $15).
        let long = TokenTotals {
            input: 300_000,
            output: 1_000,
            ..TokenTotals::default()
        };
        let c = price(&t, "gpt-6.1-sol", Speed::Fast, 272_000, &long, 0, None).unwrap();
        assert!(
            close(c.total(), 2.43) && close(c.premium.unwrap(), 1.215),
            "{c:?}"
        );
        let c = price(&t, "gpt-6.1-sol", Speed::Ultrafast, 272_000, &long, 0, None).unwrap();
        assert!(close(c.total(), 300_000. * 24e-6 + 1_000. * 90e-6), "{c:?}");
    }

    #[test]
    fn reads_litellms_table_as_t3_does() {
        let doc = json!({
            "a/m1": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6},
            "b/m1": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6, "input_cost_per_token_priority": 2e-6, "output_cost_per_token_priority": 4e-6},
            "a/m2": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6},
            "b/m2": {"input_cost_per_token": 3e-6, "output_cost_per_token": 2e-6},
            "no-output": {"input_cost_per_token": 1e-6},
            "cached": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6, "cache_read_input_token_cost": 1e-7, "input_cost_per_token_priority": 2e-6, "output_cost_per_token_priority": 4e-6},
            "sample_spec": "not a model"
        });
        let t = RateTable::parse(&doc);
        // Resellers that agree make an alias (one leaving out a speed doesn't disagree);
        // ones that disagree don't.
        assert_eq!(
            t.lookup("m1").and_then(|r| r.fast).map(|f| f.input),
            Some(2e-6)
        );
        assert!(t.lookup("m2").is_none());
        assert!(t.lookup("no-output").is_none());
        // A missing cache rate is the input rate, or the standard one's ratio at a speed; an
        // hour's write, the five minutes' rate.
        let r = t.lookup("CACHED").unwrap();
        assert!(
            close(r.standard.cache_write, 1e-6)
                && close(r.fast.unwrap().cache_read, 2e-7)
                && close(r.standard.cache_write_1h, 1e-6)
        );
    }

    /// Tiers as LiteLLM reads them: each threshold its own (`_above_32k_tokens`, or a count
    /// without `k`), a kind a tier doesn't give at the base rate for it (at that speed), an
    /// hour's write in a tier at the base's ratio to five minutes'.
    #[test]
    fn reads_tiers_and_falls_back_on_the_base_rates() {
        let doc = json!({
            "m": {
                "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6, "cache_creation_input_token_cost": 1e-6, "cache_creation_input_token_cost_above_1hr": 2e-6,
                "input_cost_per_token_priority": 3e-6, "output_cost_per_token_priority": 6e-6,
                "input_cost_per_token_above_32k_tokens": 4e-6, "cache_creation_input_token_cost_above_32k_tokens": 5e-6,
                "input_cost_per_token_above_256000_tokens": 7e-6, "output_cost_per_token_above_256000_tokens": 8e-6,
                "input_cost_per_token_above_32k_tokens_priority": 9e-6,
                "input_cost_per_token_above_0k_tokens": 1.0, "input_cost_per_token_above_xk_tokens": 1.0, "input_cost_per_token_above_64k_tokens_flex": 1.0
            }
        });
        let t = RateTable::parse(&doc);
        let r = t.lookup("m").unwrap();
        assert_eq!(
            r.tiers.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            [32_000, 256_000]
        );
        assert_eq!(
            (
                t.tier("m", 32_000),
                t.tier("m", 32_001),
                t.tier("m", 256_001)
            ),
            (0, 32_000, 256_000)
        );
        let (low, high) = (&r.tiers[0].1, &r.tiers[1].1);
        assert!(
            close(low.standard.input, 4e-6)
                && close(low.standard.output, 2e-6)
                && close(low.standard.cache_write, 5e-6)
                && close(low.standard.cache_write_1h, 10e-6),
            "{low:?}"
        );
        assert!(
            close(high.standard.input, 7e-6)
                && close(high.standard.output, 8e-6)
                && close(high.standard.cache_write, 1e-6)
                && close(high.standard.cache_write_1h, 2e-6),
            "{high:?}"
        );
        // Priority in the first tier, its output the base priority's; none in the second, so
        // Fast there is at the tier's standard rates (no premium made up).
        assert!(
            close(low.fast.unwrap().input, 9e-6) && close(low.fast.unwrap().output, 6e-6),
            "{low:?}"
        );
        assert!(high.fast.is_none() && high.ultrafast.is_none());
    }

    #[test]
    fn fetches_at_most_daily_or_a_minute_after_a_refresh_or_failure() {
        let dir = std::env::temp_dir().join(format!(
            "flotilla-prices-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = for_db(&dir.join("flotilla.db"));
        let doc =
            || Some(json!({"m": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}));
        let fetched = std::cell::Cell::new(0);
        let counting = || {
            fetched.set(fetched.get() + 1);
            doc()
        };
        assert!(p.ensure_with(false, 1_000_000, counting));
        assert!(
            !p.ensure_with(false, 1_000_000 + FRESH_MS - 1, counting),
            "fresh for a day"
        );
        assert!(
            !p.ensure_with(true, 1_000_000 + FLOOR_MS - 1, counting),
            "a refresh waits a minute"
        );
        assert!(p.ensure_with(true, 1_000_000 + FLOOR_MS, counting));
        assert_eq!(fetched.get(), 2);
        // A failed fetch keeps the table and waits a minute.
        assert!(!p.ensure_with(true, 10_000_000, || None));
        assert!(!p.ensure_with(true, 10_000_000 + FLOOR_MS - 1, counting));
        assert_eq!(fetched.get(), 2);
        assert_eq!(p.table().0.len(), 1);
        // Kept beside the database for the next start.
        let again = Prices {
            path: dir.join(FILE),
            state: Mutex::default(),
        };
        assert_eq!(
            (again.table().0.len(), again.table().1),
            (1, Some(1_000_000 + FLOOR_MS))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
