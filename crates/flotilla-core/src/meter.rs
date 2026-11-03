//! How full a thread's context is, and its agent account's usage limits, from the harness's
//! own records in its VM. boat's events carry no token counts, but each harness writes them in
//! its session: Claude Code each reply's `usage`, Codex its `token_count`s (with the model's
//! window and the account's `rate_limits`), OpenCode each message's `tokens`. `meter.sh` reads
//! them in the same exec that reads the thread's subagents ([`read_with_lineage`]), so they
//! cost no exec of their own.
//!
//! Two things are slower and asked for only when due ([`Ask`]): the model's window where the
//! harness doesn't record it (Claude Code answers its `/context` in seconds and rewrites its
//! settings as it does, so only between turns, then kept for the model: [`known_window`]), and
//! the account's limits as its service tells them now (a request from the VM with the
//! harness's own login, which never leaves it; at most once a minute per account). Codex's
//! limits as of its latest reply come with its counts anyway.

use crate::boat::BoatClient;
use crate::error::{Error, Result};
use crate::lineage::Lineage;
use crate::store::Store;
use crate::util::{ellipsize, iso_to_ms};
use chrono::{SecondsFormat, TimeZone, Utc};
use flotilla_proto::{AccountLimits, ContextUse, LimitWindow};
use serde_json::Value;

const SESSION: &str = include_str!("session.sh");
const SCRIPT: &str = include_str!("meter.sh");

/// What a read should do besides the counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ask {
    /// The model's window as already known (see [`known_window`]): the harness isn't asked.
    pub window: Option<u64>,
    /// Ask the harness for the model's window, if it doesn't record it. Only between turns.
    pub probe: bool,
    /// Ask the account's service for its limits now.
    pub limits: bool,
}

/// What the harness's records say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    pub context: Option<ContextUse>,
    /// The window the harness was asked for ([`Ask::probe`]), to keep for the thread's model.
    pub probed: Option<u64>,
    pub limits: Option<AccountLimits>,
}

/// The account whose limits a harness spends, where Flotilla reads them: Claude Code's
/// Claude login, Codex's ChatGPT one.
pub fn account_of(provider: &str) -> Option<&'static str> {
    match provider {
        "claude-code" => Some("claude"),
        "codex" => Some("chatgpt"),
        _ => None,
    }
}

/// Whether the harness has to be asked for its model's window: Codex records it with its
/// counts.
pub fn asks_window(provider: &str) -> bool {
    provider != "codex"
}

/// The command that prints what `meter.sh` finds for a boat conversation, or None for an id
/// that can't be one.
pub fn command(conversation: &str, ask: &Ask) -> Option<String> {
    if !safe_id(conversation) {
        return None;
    }
    let mut vars = format!("CONV={conversation}");
    if let Some(w) = ask.window {
        vars.push_str(&format!(" WINDOW={w}"));
    }
    if ask.probe {
        vars.push_str(" PROBE=1");
    }
    if ask.limits {
        vars.push_str(" LIMITS=1");
    }
    Some(format!(
        "{vars} bash -c {}",
        crate::setup::sh_quote(&format!("{SESSION}{SCRIPT}"))
    ))
}

fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// What one read of a conversation's records found ([`read_with_lineage`]).
#[derive(Debug, Clone, Default)]
pub struct Records {
    pub lineage: Lineage,
    /// The questions its agent asked with its ask tool (`crate::asks`).
    pub asks: Vec<crate::asks::Asked>,
    pub reading: Option<Reading>,
    /// What `spend::command` printed, when asked for.
    pub spent: Option<String>,
}

/// A conversation's subagents and the questions its agent asked, and what [`command`] finds, in
/// one exec in its VM (which must be up), and with `spend` (`spend::command`), what its agents
/// spent since the last read. The subagents fail as `lineage::read` does; a meter that doesn't
/// parse is None.
pub fn read_with_lineage(
    boat: &BoatClient,
    sandbox: &str,
    conversation: &str,
    ask: &Ask,
    spend: Option<&str>,
) -> Result<Records> {
    let (Some(lineage), Some(meter)) = (
        crate::lineage::command(conversation),
        command(conversation, ask),
    ) else {
        return Ok(Records::default());
    };
    // The probe and the limits' requests take seconds at most; every script always exits 0.
    let spend_part = spend
        .map(|c| format!("; printf '\\036'; {c}"))
        .unwrap_or_default();
    let res = boat.exec(
        sandbox,
        &format!("{lineage}; printf '\\036'; {meter}{spend_part}"),
        None,
        45,
    )?;
    if !res.ok() {
        return Err(Error::Invalid(format!(
            "couldn't read the thread's records: {}",
            ellipsize(res.stderr.trim(), 200)
        )));
    }
    let mut parts = res.stdout.split('\u{1e}');
    let (subagents, counts, spent) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next(),
    );
    let reading = parse(counts, ask, crate::util::now_ms()).ok();
    Ok(Records {
        lineage: crate::lineage::parse(subagents)?,
        asks: crate::asks::parse(subagents),
        reading,
        spent: spend.and(spent).map(str::to_string),
    })
}

/// What `meter.sh` printed. `now_ms`: when it was read (the limits a service gave now).
pub fn parse(out: &str, ask: &Ask, now_ms: i64) -> Result<Reading> {
    let v: Value = serde_json::from_str(out.trim())
        .map_err(|e| Error::Invalid(format!("the thread's counts didn't parse: {e}")))?;
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let at_ms = match v.get("at") {
        Some(Value::String(t)) => iso_to_ms(t),
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    };
    let usage = v.get("usage").filter(|u| u.is_object());
    let window = v.get("window").and_then(Value::as_u64).filter(|w| *w > 0);
    let mut probed = None;
    let (used, window, account, limits) = match v.get("harness").and_then(Value::as_str) {
        Some("claude-code") => {
            let used = usage.map(|u| {
                n(u, "input_tokens")
                    + n(u, "cache_creation_input_tokens")
                    + n(u, "cache_read_input_tokens")
                    + n(u, "output_tokens")
            });
            let text = v
                .get("window_text")
                .and_then(Value::as_str)
                .and_then(tokens_text);
            probed = text.filter(|_| ask.window.is_none());
            let limits = v
                .get("claude_usage")
                .filter(|u| u.is_object())
                .map(|u| claude_limits(u, now_ms));
            (used, window.or(text), Some("claude".to_string()), limits)
        }
        Some("codex") => {
            let used = usage.map(|u| n(u, "total_tokens"));
            let live = v
                .get("chatgpt_usage")
                .filter(|u| u.is_object())
                .and_then(|u| chatgpt_limits(u, now_ms));
            let recorded = || {
                let at = v
                    .get("rate_limits_at")
                    .and_then(Value::as_str)
                    .and_then(iso_to_ms)?;
                codex_limits(v.get("rate_limits")?, at)
            };
            (
                used,
                window,
                Some("chatgpt".to_string()),
                live.or_else(recorded),
            )
        }
        Some("opencode") => {
            // As T3 counts it: what went in (cached or not) and what came out.
            let used = usage.map(|u| {
                let cache = u.get("cache").cloned().unwrap_or(Value::Null);
                n(u, "input") + n(&cache, "read") + n(&cache, "write") + n(u, "output")
            });
            probed = window.filter(|_| ask.window.is_none());
            // Its provider's subscription is the account (an API key has no such limits).
            let account = match (s("provider").as_deref(), s("auth").as_deref()) {
                (Some("openai"), Some("oauth")) => Some("chatgpt".to_string()),
                (Some("anthropic"), Some("oauth")) => Some("claude".to_string()),
                _ => None,
            };
            (used, window, account, None)
        }
        _ => (None, None, None, None),
    };
    let context = used.filter(|u| *u > 0).map(|used| ContextUse {
        used,
        window,
        model: s("model"),
        account,
        at_ms,
    });
    Ok(Reading {
        context,
        probed,
        limits: limits.filter(|l| !l.windows.is_empty()),
    })
}

/// "200k", "1m", "1.5m" (Claude Code's `/context`) as a count.
fn tokens_text(t: &str) -> Option<u64> {
    let t = t.trim().to_ascii_lowercase();
    let (num, scale) = match t.chars().last()? {
        'k' => (&t[..t.len() - 1], 1_000.),
        'm' => (&t[..t.len() - 1], 1_000_000.),
        _ => (t.as_str(), 1.),
    };
    let n: f64 = num.parse().ok()?;
    (n > 0.).then(|| (n * scale).round() as u64)
}

/// A share used, in whole percent, rounded down: only an allowance all used up reads 100.
fn percent(v: &Value) -> Option<u8> {
    v.as_f64()
        .filter(|p| p.is_finite())
        .map(|p| p.clamp(0., 100.).floor() as u8)
}

/// A reset time as RFC 3339 in UTC, to the second.
fn rfc3339(t: &str) -> Option<String> {
    crate::util::parse_iso(t).map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn from_epoch(secs: &Value) -> Option<String> {
    let s = secs.as_i64().filter(|s| *s > 0)?;
    Utc.timestamp_opt(s, 0)
        .single()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// What a window of this many minutes is called, and its id.
fn named(minutes: u32) -> (String, String) {
    match minutes {
        300 => ("five_hour".into(), "5-hour".into()),
        10080 => ("seven_day".into(), "Weekly".into()),
        m if m >= 28 * 24 * 60 => ("monthly".into(), "Monthly".into()),
        m if m % 60 == 0 => (format!("h{}", m / 60), format!("{}-hour", m / 60)),
        m => (format!("m{m}"), format!("{m}-minute")),
    }
}

/// Claude's `/api/oauth/usage`, as `meter.sh` keeps it: the 5-hour and weekly windows, the
/// weekly ones for one model (Opus, Sonnet, and those its `limits` list names), each with
/// its use (0 to 100) and reset.
pub fn claude_limits(u: &Value, now_ms: i64) -> AccountLimits {
    let mut windows: Vec<LimitWindow> = Vec::new();
    let mut add = |id: String,
                   label: String,
                   minutes: u32,
                   scope: Option<&str>,
                   used: Option<u8>,
                   resets: Option<&str>| {
        if let Some(used) = used
            && !windows.iter().any(|w| w.id == id)
        {
            let scope = scope.map(str::to_string);
            windows.push(LimitWindow {
                id,
                label,
                used,
                resets_at: resets.and_then(rfc3339),
                minutes: Some(minutes),
                scope,
            });
        }
    };
    let legacy = [
        ("five_hour", "5-hour", 300, None),
        ("seven_day", "Weekly", 10080, None),
        ("seven_day_opus", "Weekly · Opus", 10080, Some("Opus")),
        ("seven_day_sonnet", "Weekly · Sonnet", 10080, Some("Sonnet")),
    ];
    for (key, label, minutes, scope) in legacy {
        let w = &u[key];
        add(
            key.into(),
            label.into(),
            minutes,
            scope,
            w.get("utilization").and_then(percent),
            w.get("resets_at").and_then(Value::as_str),
        );
    }
    for l in u
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (base, label, minutes) = match l.get("group").and_then(Value::as_str) {
            Some("weekly") => ("seven_day", "Weekly", 10080),
            Some("session" | "five_hour") => ("five_hour", "5-hour", 300),
            _ => continue,
        };
        let scope = l
            .get("scope")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let (id, label) = match scope {
            Some(model) => (
                format!("{base}_{}", slug(model)),
                format!("{label} · {model}"),
            ),
            None => (base.to_string(), label.to_string()),
        };
        add(
            id,
            label,
            minutes,
            scope,
            l.get("percent").and_then(percent),
            l.get("resets_at").and_then(Value::as_str),
        );
    }
    AccountLimits {
        account: "claude".into(),
        plan: None,
        windows,
        read_ms: now_ms,
    }
}

fn slug(s: &str) -> String {
    s.to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// ChatGPT's `/backend-api/wham/usage`, as `meter.sh` keeps it (its plan and `rate_limit`).
pub fn chatgpt_limits(u: &Value, now_ms: i64) -> Option<AccountLimits> {
    let rl = u.get("rate_limit")?;
    let windows = ["primary_window", "secondary_window"]
        .iter()
        .filter_map(|k| {
            let w = rl.get(*k).filter(|w| w.is_object())?;
            let minutes = w
                .get("limit_window_seconds")
                .and_then(Value::as_u64)
                .map(|s| (s / 60) as u32);
            window(minutes, w.get("used_percent")?, w.get("reset_at"))
        })
        .collect();
    Some(AccountLimits {
        account: "chatgpt".into(),
        plan: plan(u),
        windows,
        read_ms: now_ms,
    })
}

/// The `rate_limits` Codex recorded with its latest counts, at `at_ms`. Only the account's
/// own (`limit_id` "codex"): one for a model alone isn't the allowance.
pub fn codex_limits(r: &Value, at_ms: i64) -> Option<AccountLimits> {
    if r.get("limit_id")
        .and_then(Value::as_str)
        .is_some_and(|id| id != "codex")
    {
        return None;
    }
    let windows = ["primary", "secondary"]
        .iter()
        .filter_map(|k| {
            let w = r.get(*k).filter(|w| w.is_object())?;
            let minutes = w
                .get("window_minutes")
                .and_then(Value::as_u64)
                .map(|m| m as u32);
            window(minutes, w.get("used_percent")?, w.get("resets_at"))
        })
        .collect();
    Some(AccountLimits {
        account: "chatgpt".into(),
        plan: plan(r),
        windows,
        read_ms: at_ms,
    })
}

fn plan(v: &Value) -> Option<String> {
    v.get("plan_type")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// A ChatGPT window, named by its length. One the service gives no length for is left out:
/// which window it is would be a guess (the first is 5 hours on some plans, a week on others).
fn window(minutes: Option<u32>, used: &Value, resets: Option<&Value>) -> Option<LimitWindow> {
    let minutes = minutes.filter(|m| *m > 0)?;
    let (id, label) = named(minutes);
    Some(LimitWindow {
        id,
        label,
        used: percent(used)?,
        resets_at: resets.and_then(from_epoch),
        minutes: Some(minutes),
        scope: None,
    })
}

fn context_key(thread_id: &str) -> String {
    format!("context:{thread_id}")
}

/// What was last read for a thread.
pub fn stored_context(store: &Store, thread_id: &str) -> Option<ContextUse> {
    store
        .kv_get(&context_key(thread_id))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
}

/// Keep a thread's context (an older record than the one kept is left); whether it changed.
pub fn store_context(store: &Store, thread_id: &str, c: &ContextUse) -> Result<bool> {
    let old = stored_context(store, thread_id);
    if old.as_ref() == Some(c)
        || old
            .as_ref()
            .and_then(|o| o.at_ms)
            .zip(c.at_ms)
            .is_some_and(|(o, n)| n < o)
    {
        return Ok(false);
    }
    store.kv_set(&context_key(thread_id), &serde_json::to_string(c)?)?;
    Ok(true)
}

fn window_key(provider: &str, model: &str) -> String {
    format!("context_window:{provider}:{model}")
}

/// A model's window, as its harness gave it once.
pub fn known_window(store: &Store, provider: &str, model: &str) -> Option<u64> {
    store
        .kv_get(&window_key(provider, model))
        .ok()
        .flatten()
        .and_then(|w| w.parse().ok())
}

pub fn keep_window(store: &Store, provider: &str, model: &str, window: u64) -> Result<()> {
    store.kv_set(&window_key(provider, model), &window.to_string())
}

/// Every account's limits as last read, by name.
pub fn all_limits(store: &Store) -> Vec<AccountLimits> {
    let mut all: Vec<AccountLimits> = store
        .kv_with_prefix("limits:")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, j)| serde_json::from_str(&j).ok())
        .collect();
    all.sort_by(|a, b| a.account.cmp(&b.account));
    all
}

/// Keep an account's limits, unless those kept were read later; whether they changed.
pub fn store_limits(store: &Store, l: &AccountLimits) -> Result<bool> {
    let key = format!("limits:{}", l.account);
    let old: Option<AccountLimits> = store
        .kv_get(&key)?
        .and_then(|j| serde_json::from_str(&j).ok());
    if old
        .as_ref()
        .is_some_and(|o| o.read_ms > l.read_ms || o == l)
    {
        return Ok(false);
    }
    store.kv_set(&key, &serde_json::to_string(l)?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/meter");
    const NOW: i64 = 1_793_540_000_000;

    fn printed(name: &str) -> String {
        std::fs::read_to_string(format!("{DATA}/printed/{name}.json")).unwrap()
    }

    #[test]
    fn parses_what_the_script_printed_in_each_vm() {
        // Recorded on 2026-11-01 after each thread's second turn, asked for the window and limits.
        let ask = Ask {
            probe: true,
            limits: true,
            ..Ask::default()
        };
        let r = parse(&printed("claude"), &ask, NOW).unwrap();
        let c = r.context.unwrap();
        // The latest reply's input (2 + 1,252 written to the cache + 22,218 read from it) and output.
        assert_eq!(
            (c.used, c.window, c.account.as_deref(), c.model.as_deref()),
            (
                23_767,
                Some(1_000_000),
                Some("claude"),
                Some("claude-haiku-5-5")
            )
        );
        assert_eq!(c.at_ms, iso_to_ms("2026-11-01T13:25:20.284Z"));
        assert_eq!(r.probed, Some(1_000_000));
        let l = r.limits.unwrap();
        assert_eq!(
            (l.account.as_str(), l.plan.as_deref(), l.read_ms),
            ("claude", None, NOW)
        );
        let shown: Vec<(&str, &str, u8, Option<&str>)> = l
            .windows
            .iter()
            .map(|w| {
                (
                    w.id.as_str(),
                    w.label.as_str(),
                    w.used,
                    w.resets_at.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("seven_day", "Weekly", 93, Some("2026-11-01T15:00:00Z")),
                (
                    "seven_day_fable",
                    "Weekly · Fable",
                    26,
                    Some("2026-11-01T14:59:59Z")
                )
            ]
        );
        // A window already known isn't kept again.
        assert_eq!(
            parse(
                &printed("claude"),
                &Ask {
                    window: Some(1_000_000),
                    ..ask
                },
                NOW
            )
            .unwrap()
            .probed,
            None
        );

        let r = parse(&printed("codex"), &ask, NOW).unwrap();
        let c = r.context.unwrap();
        assert_eq!(
            (c.used, c.window, c.account.as_deref()),
            (15_595, Some(828_400), Some("chatgpt"))
        );
        assert_eq!(r.probed, None, "Codex records its window");
        let l = r.limits.unwrap();
        // Asked now (wham), not as of the latest reply.
        assert_eq!((l.plan.as_deref(), l.read_ms), (Some("pro"), NOW));
        assert_eq!(
            l.windows,
            [LimitWindow {
                id: "seven_day".into(),
                label: "Weekly".into(),
                used: 10,
                resets_at: Some("2026-11-04T19:09:31Z".into()),
                minutes: Some(10080),
                scope: None
            }]
        );

        let r = parse(&printed("opencode"), &ask, NOW).unwrap();
        let c = r.context.unwrap();
        // 200 in, 20,480 from the cache, 138 out.
        assert_eq!(
            (c.used, c.window, c.account.as_deref(), c.at_ms),
            (
                20_818,
                Some(922_000),
                Some("chatgpt"),
                Some(1_793_539_513_328)
            )
        );
        assert_eq!((r.probed, r.limits), (Some(922_000), None));

        assert_eq!(parse("{}", &ask, NOW).unwrap(), Reading::default());
        assert!(parse("not json", &ask, NOW).is_err());
    }

    #[test]
    fn reads_each_services_limits() {
        // Codex's own record, without the live answer: as of its latest reply.
        let v: Value = serde_json::from_str(&printed("codex")).unwrap();
        let mut recorded = v.clone();
        recorded["chatgpt_usage"] = Value::Null;
        let l = parse(&recorded.to_string(), &Ask::default(), NOW)
            .unwrap()
            .limits
            .unwrap();
        assert_eq!(
            (l.read_ms, l.windows[0].used, l.windows[0].minutes),
            (
                iso_to_ms("2026-11-01T13:25:13.422Z").unwrap(),
                10,
                Some(10080)
            )
        );
        // A limit for one model alone isn't the account's.
        assert_eq!(
            codex_limits(
                &serde_json::json!({"limit_id": "codex_bengalfox", "primary": {"used_percent": 50.0}}),
                1
            ),
            None
        );
        // Each window by its length; one without a length is left out, not guessed at. A share
        // just short of all is not all.
        let r = serde_json::json!({"primary": {"used_percent": 99.6, "window_minutes": 300, "resets_at": 1793819371}, "secondary": {"used_percent": 100.0}});
        let l = codex_limits(&r, 1).unwrap();
        let ids: Vec<(&str, &str, u8, bool)> = l
            .windows
            .iter()
            .map(|w| (w.id.as_str(), w.label.as_str(), w.used, w.reached()))
            .collect();
        assert_eq!(ids, [("five_hour", "5-hour", 99, false)]);
        // Claude's 5-hour and per-model weekly windows, and none twice.
        let u = serde_json::json!({
            "five_hour": {"utilization": 12.5, "resets_at": "2026-11-01T17:00:00.5+00:00"},
            "seven_day": {"utilization": 40.0, "resets_at": "2026-11-05T15:00:00+00:00"},
            "seven_day_opus": {"utilization": 3.0, "resets_at": null},
            "limits": [
                {"kind": "weekly_all", "group": "weekly", "percent": 40, "resets_at": "2026-11-05T15:00:00+00:00", "scope": null},
                {"kind": "weekly_scoped", "group": "weekly", "percent": 7, "resets_at": "2026-11-05T15:00:00+00:00", "scope": "Fable"},
                {"kind": "spend", "group": "monthly", "percent": 1, "scope": null}
            ]
        });
        let l = claude_limits(&u, 1);
        // (id, label, used, resets, scope)
        type Row<'a> = (&'a str, &'a str, u8, Option<&'a str>, Option<&'a str>);
        let ids: Vec<Row> = l
            .windows
            .iter()
            .map(|w| {
                (
                    w.id.as_str(),
                    w.label.as_str(),
                    w.used,
                    w.resets_at.as_deref(),
                    w.scope.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            ids,
            [
                (
                    "five_hour",
                    "5-hour",
                    12,
                    Some("2026-11-01T17:00:00Z"),
                    None
                ),
                (
                    "seven_day",
                    "Weekly",
                    40,
                    Some("2026-11-05T15:00:00Z"),
                    None
                ),
                ("seven_day_opus", "Weekly · Opus", 3, None, Some("Opus")),
                (
                    "seven_day_fable",
                    "Weekly · Fable",
                    7,
                    Some("2026-11-05T15:00:00Z"),
                    Some("Fable")
                ),
            ]
        );
        assert_eq!(tokens_text("200k"), Some(200_000));
        assert_eq!(tokens_text("1m"), Some(1_000_000));
        assert_eq!(tokens_text("1.5M"), Some(1_500_000));
        assert_eq!(tokens_text(""), None);
        assert_eq!(tokens_text("lots"), None);
    }

    #[test]
    fn keeps_the_newest() {
        let store = Store::open_in_memory().unwrap();
        let c = |used, at| ContextUse {
            used,
            at_ms: Some(at),
            ..Default::default()
        };
        assert!(store_context(&store, "t1", &c(10, 2)).unwrap());
        assert!(!store_context(&store, "t1", &c(10, 2)).unwrap());
        assert!(
            !store_context(&store, "t1", &c(5, 1)).unwrap(),
            "an older record"
        );
        assert!(store_context(&store, "t1", &c(20, 3)).unwrap());
        assert_eq!(stored_context(&store, "t1"), Some(c(20, 3)));
        let l = |used, read_ms| AccountLimits {
            account: "claude".into(),
            windows: vec![LimitWindow {
                used,
                ..Default::default()
            }],
            read_ms,
            plan: None,
        };
        assert!(store_limits(&store, &l(90, 2)).unwrap());
        assert!(!store_limits(&store, &l(80, 1)).unwrap(), "read earlier");
        assert!(store_limits(&store, &l(95, 3)).unwrap());
        assert_eq!(all_limits(&store), [l(95, 3)]);
        assert_eq!(known_window(&store, "claude-code", "m"), None);
        keep_window(&store, "claude-code", "m", 200_000).unwrap();
        assert_eq!(known_window(&store, "claude-code", "m"), Some(200_000));
        store.delete_thread("t1").unwrap();
        assert_eq!(stored_context(&store, "t1"), None);
    }

    /// A VM's home with a harness's records, and stand-ins for the commands that would ask
    /// the network or the harness: `claude` (its /context) and `curl` (the service's limits).
    struct Vm {
        home: PathBuf,
        bin: PathBuf,
    }

    impl Vm {
        fn new(name: &str) -> Vm {
            let root = std::env::temp_dir().join(format!(
                "flotilla-meter-{name}-{}-{}",
                std::process::id(),
                crate::util::now_ms()
            ));
            let (home, bin) = (root.join("home"), root.join("bin"));
            std::fs::create_dir_all(&bin).unwrap();
            Vm { home, bin }
        }

        fn put(&self, at: &str, from: &str) {
            let to = self.home.join(at);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(format!("{DATA}/{from}"), to).unwrap();
        }

        fn write(&self, at: &str, text: &str) {
            let to = self.home.join(at);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::write(to, text).unwrap();
        }

        /// A stand-in command that prints a recording.
        fn answers(&self, command: &str, recording: &str) {
            use std::os::unix::fs::PermissionsExt;
            let path = self.bin.join(command);
            std::fs::write(&path, format!("#!/bin/sh\ncat '{DATA}/{recording}'\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        /// Run what `command` builds, as the exec would.
        fn read(&self, conversation: &str, ask: &Ask, env: &[(&str, &str)]) -> Reading {
            let path = format!(
                "{}:{}",
                self.bin.display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(command(conversation, ask).unwrap())
                .env("HOME", &self.home)
                .env("PATH", path)
                .envs(env.iter().copied())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            parse(&String::from_utf8_lossy(&out.stdout), ask, NOW).unwrap()
        }
    }

    impl Drop for Vm {
        fn drop(&mut self) {
            if let Some(root) = self.home.parent() {
                let _ = std::fs::remove_dir_all(root);
            }
        }
    }

    fn have(tools: &[&str]) -> bool {
        let missing: Vec<&&str> = tools
            .iter()
            .filter(|t| {
                !std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("command -v {t}"))
                    .output()
                    .is_ok_and(|o| o.status.success())
            })
            .collect();
        if !missing.is_empty() {
            eprintln!("skipped: {missing:?} not here");
        }
        missing.is_empty()
    }

    #[test]
    fn the_script_reads_recorded_claude_code_records() {
        if !have(&["bash", "jq"]) {
            return;
        }
        let vm = Vm::new("claude");
        vm.put(
            ".ascii/conversation-state.json",
            "claude/conversation-state.json",
        );
        vm.put(
            ".claude/projects/-home-user/2b36a76d-03f9-4adb-aa05-4433cafe85c8.jsonl",
            "claude/2b36a76d-03f9-4adb-aa05-4433cafe85c8.jsonl",
        );
        vm.answers("claude", "claude/context.json");
        vm.answers("curl", "claude/oauth-usage.json");
        let conv = "a24037a0-ede9-44f8-8bc2-09c4b72e5448";
        let token = [("CLAUDE_CODE_OAUTH_TOKEN", "not-a-token")];
        // Between turns, with the limits due: the session's last whole reply (the line being
        // written after it is skipped), the window from /context and the account's limits.
        let r = vm.read(
            conv,
            &Ask {
                probe: true,
                limits: true,
                ..Ask::default()
            },
            &token,
        );
        let c = r.context.unwrap();
        assert_eq!(
            (c.used, c.window, c.model.as_deref()),
            (23_767, Some(1_000_000), Some("claude-haiku-5-5"))
        );
        assert_eq!(
            c.at_ms,
            iso_to_ms("2026-11-01T13:25:20.284Z"),
            "the newest whole reply"
        );
        assert_eq!(r.probed, Some(1_000_000));
        let l = r.limits.unwrap();
        assert_eq!(
            l.windows
                .iter()
                .map(|w| (w.label.as_str(), w.used))
                .collect::<Vec<_>>(),
            [("Weekly", 87), ("Weekly · Fable", 16)]
        );
        // During a turn: the window as known, nothing asked.
        let r = vm.read(
            conv,
            &Ask {
                window: Some(1_000_000),
                ..Ask::default()
            },
            &token,
        );
        assert_eq!(
            (r.context.unwrap().window, r.probed, r.limits),
            (Some(1_000_000), None, None)
        );
        // An id that can't be a conversation is never sent.
        assert_eq!(command("a; b", &Ask::default()), None);
    }

    #[test]
    fn the_script_reads_recorded_codex_records() {
        if !have(&["bash", "jq"]) {
            return;
        }
        let vm = Vm::new("codex");
        vm.put(
            ".ascii/conversation-state.json",
            "codex/conversation-state.json",
        );
        let rollout = "rollout-2026-11-01T13-05-17-01a19736-dc1a-7c02-9912-459349b8ba1f.jsonl";
        vm.put(
            &format!(".codex/sessions/2026/11/01/{rollout}"),
            &format!("codex/{rollout}"),
        );
        vm.write(".codex/auth.json", r#"{"auth_mode":"chatgpt","tokens":{"access_token":"not-a-token","account_id":"not-an-account"}}"#);
        vm.answers("curl", "codex/wham-usage.json");
        let conv = "1727f125-5df6-4e56-9646-d1d67cd806e3";
        let r = vm.read(
            conv,
            &Ask {
                probe: true,
                limits: true,
                ..Ask::default()
            },
            &[],
        );
        let c = r.context.unwrap();
        assert_eq!(
            (c.used, c.window, c.model.as_deref(), c.account.as_deref()),
            (15_595, Some(828_400), Some("gpt-6-luna"), Some("chatgpt"))
        );
        let l = r.limits.unwrap();
        assert_eq!(
            (
                l.plan.as_deref(),
                l.read_ms,
                l.windows[0].used,
                l.windows[0].label.as_str()
            ),
            (Some("pro"), NOW, 10, "Weekly")
        );
        // Not asked now: the limits Codex recorded with its counts.
        let l = vm.read(conv, &Ask::default(), &[]).limits.unwrap();
        assert_eq!(
            (l.read_ms, l.windows[0].resets_at.as_deref()),
            (
                iso_to_ms("2026-11-01T13:25:13.422Z").unwrap(),
                Some("2026-11-04T19:09:31Z")
            )
        );
        // A later count with one model's own limit (made up here, as Codex writes them)
        // doesn't hide the account's.
        let path = vm
            .home
            .join(format!(".codex/sessions/2026/11/01/{rollout}"));
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.truncate(text.rfind('\n').unwrap() + 1);
        text.push_str(r#"{"timestamp":"2026-11-01T13:26:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex_bengalfox","primary":{"used_percent":5.0,"window_minutes":300,"resets_at":1793819371},"secondary":null,"plan_type":"pro"}}}"#);
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        let l = vm.read(conv, &Ask::default(), &[]).limits.unwrap();
        assert_eq!(
            (l.windows[0].used, l.windows[0].label.as_str()),
            (10, "Weekly")
        );
    }

    #[test]
    fn the_script_reads_recorded_opencode_records() {
        if !have(&["bash", "jq", "sqlite3"]) {
            return;
        }
        let vm = Vm::new("opencode");
        vm.put(
            ".ascii/conversation-state.json",
            "opencode/conversation-state.json",
        );
        vm.put(".cache/opencode/models.json", "opencode/models.json");
        vm.write(
            ".local/share/opencode/auth.json",
            r#"{"openai":{"type":"oauth"}}"#,
        );
        let db = vm.home.join(".local/share/opencode/opencode.db");
        let sql = std::fs::read_to_string(Path::new(DATA).join("opencode/opencode.sql")).unwrap();
        let made = std::process::Command::new("sqlite3")
            .arg(&db)
            .arg(sql)
            .status()
            .unwrap();
        assert!(made.success());
        let conv = "39b15986-8e73-46c2-9031-8bf91adcd399";
        let r = vm.read(
            conv,
            &Ask {
                probe: true,
                ..Ask::default()
            },
            &[],
        );
        let c = r.context.unwrap();
        // GPT-6 Luna's input limit, as T3 takes it (its whole window is 1,050,000).
        assert_eq!(
            (c.used, c.window, c.account.as_deref()),
            (20_818, Some(922_000), Some("chatgpt"))
        );
        assert_eq!(r.probed, Some(922_000));
        assert_eq!(
            vm.read(conv, &Ask::default(), &[]).context.unwrap().window,
            None
        );
    }
}
