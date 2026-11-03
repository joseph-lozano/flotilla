//! What the agents spent: every request's tokens, as the harnesses record them in each thread's
//! VM, read the way T3 Code's Usage reads them on a Mac (its Claude Code, Codex and OpenCode
//! readers, ported): Claude Code's replies by message and request id (each content block
//! repeats its reply's usage; advisor calls are their own model's), Codex's `token_count`s
//! checked against its running totals (a re-sent, resumed, compacted or forked count isn't a
//! new request), OpenCode's finished replies.
//!
//! `spend.sh` prints only what's new since the last read of each file (a cursor per thread,
//! kept by its VM: a recreated VM reads from the start), and rides the exec that reads the
//! thread's subagents (`meter::read_with_lineage`), so it costs no exec of its own. Each
//! request is counted once, by its key, however often it's read, in whichever file or VM: a
//! hash of every key counted is kept as long as the totals ([`KEEP_DAYS`]). The totals are
//! kept by the quarter hour (UTC: [`BUCKET_MS`]), harness, model, speed and context tier
//! (decided as each request is counted: a row of requests can't be split again), with the
//! cache writes kept an hour apart, so each device adds them up into its own days; they're priced when the view is made
//! (`crate::prices`), as T3 prices them when asked.

use crate::error::Result;
use crate::prices::RateTable;
use crate::store::Store;
use crate::util::iso_to_ms;
use flotilla_proto::{AgentUsage, Speed, TokenTotals, UsageHour};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

const SCRIPT: &str = include_str!("spend.sh");
const DAY_MS: i64 = 86_400_000;
/// What a row of the totals covers: a quarter hour, so every time zone's days (some are half
/// or three quarters of an hour off UTC) begin at one. (The column is still `hour`.)
const BUCKET_MS: i64 = 15 * 60_000;
/// How long the totals, and what was counted, are kept: as T3's scan cache keeps them.
pub const KEEP_DAYS: i64 = 92;
/// The days the view sends: T3's default period (30 days), and one more so every time zone's
/// 30 days are in it.
pub const VIEW_DAYS: i64 = 31;
/// The most of the session files read at once; the rest is read next time.
const BUDGET: u64 = 8 << 20;
/// The files a thread's cursor remembers once a read has got to the end of everything (the most
/// recently grown, and any not read to its end). One forgotten is passed over until it changes
/// (`SINCE_S`); then it's read from its start, and what was counted already isn't again.
const MAX_FILES: usize = 300;
/// A file changed within this of when the last whole read began may have changed after it was
/// read: it's read again.
const SINCE_SLACK_S: i64 = 2;
/// How far before the newest OpenCode reply read each pass through them starts again.
const OPENCODE_BACK_MS: i64 = 60_000;
const READ_KEY: &str = "spend:read_ms";

/// One request: its tokens, model and speed, and what makes it this request and no other.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub provider: &'static str,
    pub at_ms: i64,
    pub model: String,
    pub tokens: TokenTotals,
    /// Of `tokens.cache_write`, the writes kept for an hour (Claude's), at a higher rate.
    pub cache_write_1h: u64,
    pub speed: Speed,
    /// The cost the harness gave, if it gave one.
    pub reported_usd: Option<f64>,
    pub key: String,
}

/// Where a thread's last read of its VM's records stopped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    /// The VM these are for: another VM starts again.
    pub sandbox: String,
    /// Each file read, by its path.
    #[serde(default)]
    pub files: BTreeMap<String, FileAt>,
    /// OpenCode's finished replies after this one (when it last changed, ms; its id) are read
    /// next; and when the newest of those read last changed. Each pass through them starts a
    /// minute before that, so a reply that finished late with an earlier time isn't missed.
    #[serde(default)]
    pub opencode_ms: i64,
    #[serde(default)]
    pub opencode_id: String,
    #[serde(default)]
    pub opencode_newest: i64,
    /// When the last read that got to the end of everything began, less a little (the VM's
    /// clock, seconds); 0 before one has.
    #[serde(default)]
    pub since_s: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileAt {
    /// A checksum of its first bytes: another file at the same path starts again.
    pub id: String,
    /// Bytes read (whole lines).
    pub at: u64,
    /// Codex's state at that point: its counts depend on lines before them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexState>,
    /// When it last grew (ms), for which to forget first.
    #[serde(default)]
    pub grew_ms: i64,
    /// It was read to its end.
    #[serde(default)]
    pub done: bool,
}

fn cursor_key(thread: &str) -> String {
    format!("spend_cursor:{thread}")
}

/// A thread's cursor for its VM `sandbox`: empty for another VM, or none yet.
pub fn cursor(store: &Store, thread: &str, sandbox: &str) -> Cursor {
    let kept: Option<Cursor> = store
        .kv_get(&cursor_key(thread))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok());
    match kept {
        Some(c) if c.sandbox == sandbox => c,
        _ => Cursor {
            sandbox: sandbox.to_string(),
            ..Cursor::default()
        },
    }
}

/// The command that prints what's new in the VM since `cursor`.
pub fn command(cursor: &Cursor) -> String {
    use base64::Engine as _;
    let lines: String = cursor
        .files
        .iter()
        .filter(|(p, _)| !p.contains(['\t', '\n']))
        .map(|(p, f)| format!("{}\t{}\t{p}\n", f.id, f.at))
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(lines);
    let id = if cursor
        .opencode_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        cursor.opencode_id.as_str()
    } else {
        ""
    };
    format!(
        "CURSOR='{encoded}' SINCE_S={} OC_SINCE={} OC_ID='{id}' BUDGET={BUDGET} bash -c {}",
        cursor.since_s.max(0),
        cursor.opencode_ms.max(0),
        crate::setup::sh_quote(SCRIPT)
    )
}

/// What a read printed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    pub files: Vec<FileRead>,
    /// OpenCode's finished replies, as its database has them, in order; `opencode_full`: as many
    /// as it reads at once, so there may be more; `opencode_read`: its database was read (none
    /// left ends a pass through them).
    pub opencode: Vec<Value>,
    pub opencode_full: bool,
    pub opencode_read: bool,
    /// It stopped short: there's more to read.
    pub more: bool,
    /// When it began, by the VM's clock (seconds).
    pub now_s: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileRead {
    pub path: String,
    pub id: String,
    pub harness: String,
    pub size: u64,
    pub from: u64,
    pub to: u64,
    pub lines: Vec<Value>,
}

/// What `spend.sh` printed; a line that doesn't parse is passed over.
pub fn parse(out: &str) -> Reading {
    let mut r = Reading::default();
    for line in out.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("more").and_then(Value::as_bool) == Some(true) {
            r.more = true;
        } else if let Some(now) = v.get("now").and_then(Value::as_i64) {
            r.now_s = Some(now);
        } else if let Some(rows) = v.get("opencode").and_then(Value::as_array) {
            r.opencode.extend(rows.iter().cloned());
            r.opencode_full |= v["full"].as_bool() == Some(true);
            r.opencode_read = true;
        } else if let (Some(path), Some(id), Some(harness), Some(from), Some(to)) = (
            v["file"].as_str(),
            v["id"].as_str(),
            v["harness"].as_str(),
            v["from"].as_u64(),
            v["to"].as_u64(),
        ) {
            let lines = v["lines"].as_array().cloned().unwrap_or_default();
            let size = v["size"].as_u64().unwrap_or(to);
            r.files.push(FileRead {
                path: path.into(),
                id: id.into(),
                harness: harness.into(),
                size,
                from,
                to,
                lines,
            });
        }
    }
    r
}

/// The records in what was read, and the cursor moved on past them. `fast`: the thread runs
/// at Fast, for a Codex session that doesn't say (as T3 asks its own record of the model it
/// picked).
pub fn records(
    reading: &Reading,
    cursor: Cursor,
    fast: bool,
    now_ms: i64,
) -> (Vec<Record>, Cursor) {
    records_keeping(reading, cursor, fast, now_ms, MAX_FILES)
}

fn records_keeping(
    reading: &Reading,
    mut cursor: Cursor,
    fast: bool,
    now_ms: i64,
    max_files: usize,
) -> (Vec<Record>, Cursor) {
    let mut out = Vec::new();
    for f in &reading.files {
        let kept = cursor
            .files
            .get(&f.path)
            .filter(|k| k.id == f.id && k.at == f.from && f.from > 0);
        let mut state = kept.and_then(|k| k.codex.clone()).unwrap_or_default();
        for line in &f.lines {
            match f.harness.as_str() {
                "claude" => out.extend(claude(line)),
                "codex" => out.extend(codex(line, &mut state, fast)),
                _ => {}
            }
        }
        let codex = (f.harness == "codex").then_some(state);
        cursor.files.insert(
            f.path.clone(),
            FileAt {
                id: f.id.clone(),
                at: f.to,
                codex,
                grew_ms: now_ms,
                done: f.to >= f.size,
            },
        );
    }
    for row in &reading.opencode {
        out.extend(opencode(row));
    }
    // On from the last reply read while there may be more; once there aren't, the next pass
    // starts a minute before the newest, so a reply that finished late with an earlier time
    // (or the same time and a lower id) isn't missed.
    if reading.opencode_read {
        let newest = reading
            .opencode
            .iter()
            .filter_map(|r| r["updated"].as_i64())
            .max();
        cursor.opencode_newest = cursor.opencode_newest.max(newest.unwrap_or(0));
        let last = reading
            .opencode
            .last()
            .and_then(|r| Some((r["updated"].as_i64()?, r["id"].as_str()?.to_string())));
        (cursor.opencode_ms, cursor.opencode_id) = match last {
            Some(last) if reading.opencode_full => last,
            _ => (
                (cursor.opencode_newest - OPENCODE_BACK_MS).max(0),
                String::new(),
            ),
        };
    }
    // Everything has been read to its end: a file that hasn't changed since this read began
    // is passed over, and only files read to their end are forgotten.
    if !reading.more
        && let Some(now) = reading.now_s
    {
        cursor.since_s = now - SINCE_SLACK_S;
        if cursor.files.len() > max_files {
            let mut done: Vec<(i64, String)> = cursor
                .files
                .iter()
                .filter(|(_, f)| f.done)
                .map(|(p, f)| (f.grew_ms, p.clone()))
                .collect();
            done.sort();
            for (_, p) in done.into_iter().take(cursor.files.len() - max_files) {
                cursor.files.remove(&p);
            }
        }
    }
    (out, cursor)
}

/// A count as T3 takes one: a positive number, whole; anything else is none.
fn count(v: &Value) -> u64 {
    v.as_f64()
        .filter(|n| n.is_finite() && *n > 0.)
        .map(|n| n.trunc() as u64)
        .unwrap_or(0)
}

/// Claude Code's placeholder for a reply that never reached the API.
const SYNTHETIC: &str = "<synthetic>";

/// A Claude Code reply's usage (one line per content block, each the whole reply's: counted
/// once by its key), and each advisor call it made, at the advisor's model.
pub fn claude(v: &Value) -> Vec<Record> {
    let msg = &v["message"];
    let usage = &msg["usage"];
    if !usage.is_object() {
        return vec![];
    }
    let Some(at_ms) = v["timestamp"].as_str().and_then(iso_to_ms) else {
        return vec![];
    };
    let model = msg["model"].as_str().unwrap_or("");
    if model.is_empty() || model == SYNTHETIC {
        return vec![];
    }
    let tokens = |u: &Value| TokenTotals {
        input: count(&u["input_tokens"]),
        cache_read: count(&u["cache_read_input_tokens"]),
        cache_write: count(&u["cache_creation_input_tokens"]),
        output: count(&u["output_tokens"]),
        // Anthropic counts thinking in output and doesn't give it apart.
        reasoning: 0,
    };
    // Of the cache writes, those kept an hour.
    let hour = |u: &Value, t: &TokenTotals| {
        count(&u["cache_creation"]["ephemeral_1h_input_tokens"]).min(t.cache_write)
    };
    let executor = tokens(usage);
    let speed = if usage["speed"].as_str() == Some("fast") {
        Speed::Fast
    } else {
        Speed::Standard
    };
    let key = match (msg["id"].as_str(), v["requestId"].as_str()) {
        (None, None) => format!(
            "claude:{}:{at_ms}:{}",
            v["sessionId"].as_str().unwrap_or(""),
            signature(&executor)
        ),
        (id, request) => format!("claude:{}:{}", id.unwrap_or(""), request.unwrap_or("")),
    };
    let reported_usd = v["costUSD"].as_f64().filter(|c| c.is_finite());
    let mut out = vec![Record {
        provider: "claude-code",
        at_ms,
        model: model.to_string(),
        tokens: executor,
        cache_write_1h: hour(usage, &executor),
        speed,
        reported_usd,
        key: key.clone(),
    }];
    for (i, it) in usage["iterations"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if it["type"].as_str() != Some("advisor_message") {
            continue;
        }
        let model = it["model"].as_str().unwrap_or("");
        if model.is_empty() || model == SYNTHETIC {
            continue;
        }
        let t = tokens(it);
        out.push(Record {
            provider: "claude-code",
            at_ms,
            model: model.to_string(),
            tokens: t,
            cache_write_1h: hour(it, &t),
            speed,
            reported_usd: None,
            key: format!("{key}:advisor:{i}"),
        });
    }
    out
}

fn signature(t: &TokenTotals) -> String {
    format!(
        "{}/{}/{}/{}",
        t.input, t.cache_read, t.cache_write, t.output
    )
}

/// Codex's counts as a rollout gives them: the request's own (`last_token_usage`) and the
/// session's so far (`total_token_usage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexUsage {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    total: u64,
}

impl CodexUsage {
    /// T3's reading of them: input and output given; the parts inside them no bigger than them.
    fn read(v: Option<&Value>) -> Option<CodexUsage> {
        let v = v.filter(|v| v.is_object())?;
        let (input, output) = (
            v.get("input_tokens")?.as_u64()?,
            v.get("output_tokens")?.as_u64()?,
        );
        let or = |k: &str, d: u64| match v.get(k) {
            None => Some(d),
            Some(n) => n.as_u64(),
        };
        CodexUsage {
            input,
            cached: or("cached_input_tokens", 0)?,
            cache_write: or("cache_write_input_tokens", 0)?,
            output,
            reasoning: or("reasoning_output_tokens", 0)?,
            total: or("total_tokens", input + output)?,
        }
        .valid()
    }

    fn valid(self) -> Option<CodexUsage> {
        (self.cached + self.cache_write <= self.input
            && self.reasoning <= self.output
            && self.total >= self.input + self.output)
            .then_some(self)
    }

    fn fields(&self) -> [u64; 6] {
        [
            self.input,
            self.cached,
            self.cache_write,
            self.output,
            self.reasoning,
            self.total,
        ]
    }

    fn from_fields(f: [u64; 6]) -> CodexUsage {
        CodexUsage {
            input: f[0],
            cached: f[1],
            cache_write: f[2],
            output: f[3],
            reasoning: f[4],
            total: f[5],
        }
    }

    fn plus(prev: Option<CodexUsage>, last: CodexUsage) -> Option<CodexUsage> {
        let p = prev.map(|p| p.fields()).unwrap_or_default();
        let l = last.fields();
        let mut sum = [0u64; 6];
        for i in 0..6 {
            sum[i] = p[i].checked_add(l[i])?;
        }
        CodexUsage::from_fields(sum).valid()
    }

    fn minus(self, o: CodexUsage) -> Option<CodexUsage> {
        let (a, b) = (self.fields(), o.fields());
        let mut d = [0u64; 6];
        for i in 0..6 {
            d[i] = a[i].checked_sub(b[i])?;
        }
        CodexUsage::from_fields(d).valid()
    }

    fn signature(&self) -> String {
        self.fields().map(|n| n.to_string()).join("/")
    }
}

/// What reading a Codex rollout needs from the lines before: its counts carry no model, tier
/// or session, so they come from the latest `turn_context`, `thread_settings_applied` and the
/// first `session_meta`; and its running totals, to tell a new request from one sent again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexState {
    model: String,
    speed: Speed,
    session: String,
    model_provider: String,
    saw_meta: bool,
    saw_settings: bool,
    last_signature: Option<String>,
    last_total: Option<CodexUsage>,
    sequence_start: Option<String>,
    /// A fork copies its parent's counts, restamped, in a burst: they're the parent's, not
    /// spent again.
    fork_copies: bool,
    fork_anchor_ms: i64,
    pub malformed: u64,
}

/// A fork's copied counts come within this of each other; its own follow a model turn.
const FORK_GAP_MS: i64 = 1000;

fn codex_speed(tier: Option<&Value>) -> Speed {
    match tier.and_then(Value::as_str) {
        Some("priority" | "fast") => Speed::Fast,
        Some("ultrafast") => Speed::Ultrafast,
        _ => Speed::Standard,
    }
}

fn forked(payload: &Value) -> bool {
    payload["forked_from_id"].is_string()
        || payload["source"]["subagent"]["thread_spawn"]["parent_thread_id"].is_string()
}

/// One line of a Codex rollout: a request's tokens when it's a new one (T3's `parseCodexRecord`).
pub fn codex(v: &Value, st: &mut CodexState, fast: bool) -> Option<Record> {
    let payload = v.get("payload").filter(|p| p.is_object())?;
    let at = v["timestamp"].as_str().and_then(iso_to_ms);
    match v["type"].as_str() {
        Some("session_meta") => {
            // Only the first: an ancestor's copied after it would misname what follows.
            if st.saw_meta {
                return None;
            }
            st.saw_meta = true;
            if let Some(id) = payload["id"].as_str().or(payload["session_id"].as_str()) {
                st.session = id.to_string();
            }
            if let Some(p) = payload["model_provider"].as_str() {
                st.model_provider = p.to_string();
            }
            if let Some(at) = at.filter(|_| forked(payload)) {
                st.fork_copies = true;
                st.fork_anchor_ms = at;
            }
            return None;
        }
        Some("turn_context") => {
            if let Some(m) = payload["model"].as_str() {
                st.model = m.to_string();
            }
            return None;
        }
        _ => {}
    }
    match payload["type"].as_str() {
        Some("thread_settings_applied") => {
            st.saw_settings = true;
            if payload["thread_settings"].is_object() {
                st.speed = codex_speed(payload["thread_settings"].get("service_tier"));
            }
            return None;
        }
        Some("token_count") => {}
        _ => return None,
    }
    let info = payload.get("info").filter(|i| i.is_object())?;
    let at = at?;
    if st.model.is_empty() {
        return None;
    }
    let last = CodexUsage::read(info.get("last_token_usage"));
    let has_total = info.get("total_token_usage").is_some_and(|t| !t.is_null());
    let total = if has_total {
        CodexUsage::read(info.get("total_token_usage"))
    } else {
        None
    };
    let rebase = |st: &mut CodexState, t: CodexUsage| {
        st.last_total = Some(t);
        // Counters at zero start again: where the sequence began no longer matters.
        if t.input + t.output == 0 {
            st.sequence_start = None;
        }
    };
    if st.fork_copies {
        if at - st.fork_anchor_ms < FORK_GAP_MS {
            st.fork_anchor_ms = at;
            // The parent's, already counted there, but the child's totals start from them.
            let sig = last.map(|l| l.signature());
            if let Some(t) = total {
                rebase(st, t);
            } else if let Some(l) = last.filter(|_| !has_total && sig != st.last_signature) {
                st.last_total = CodexUsage::plus(st.last_total, l);
            }
            st.last_signature = sig;
            return None;
        }
        st.fork_copies = false;
        st.last_signature = None;
    }
    let previous = st.last_total;
    if let Some(t) = total {
        rebase(st, t);
    }
    let Some(last) = last.filter(|_| !has_total || total.is_some()) else {
        st.malformed += 1;
        return None;
    };
    // Counts with no input or output give the context's size, not a request.
    if last.input + last.output == 0 {
        st.last_signature = None;
        return None;
    }
    if let (Some(t), Some(p)) = (total, previous)
        && t == p
    {
        return None;
    }
    if last.total != last.input + last.output {
        st.malformed += 1;
        return None;
    }
    let sig = last.signature();
    if let Some(t) = total {
        // A session resumed in a new process starts its totals again at this request.
        let restarted = t == last;
        let start = format!("{at}:{sig}");
        if restarted && st.sequence_start.as_deref() == Some(start.as_str()) {
            st.last_total = previous;
            return None;
        }
        let reconciled = match previous {
            None => t.minus(last).is_some(),
            Some(p) => t.minus(p) == Some(last),
        };
        if !restarted && !reconciled {
            st.malformed += 1;
            return None;
        }
        if restarted {
            st.sequence_start = Some(start);
        }
    } else {
        if st.last_signature.as_deref() == Some(sig.as_str()) {
            return None;
        }
        match CodexUsage::plus(previous, last) {
            Some(t) => st.last_total = Some(t),
            None => {
                st.malformed += 1;
                return None;
            }
        }
    }
    st.last_signature = Some(sig.clone());
    let tokens = TokenTotals {
        // Codex's input includes what came from the cache.
        input: last.input - last.cached - last.cache_write,
        cache_read: last.cached,
        cache_write: last.cache_write,
        output: last.output,
        reasoning: last.reasoning,
    };
    if tokens.total() == 0 {
        return None;
    }
    // A session that doesn't say its tier ran at the thread's.
    let speed = if !st.saw_settings && st.model_provider == "openai" && fast {
        Speed::Fast
    } else {
        st.speed
    };
    Some(Record {
        provider: "codex",
        at_ms: at,
        model: st.model.clone(),
        tokens,
        cache_write_1h: 0,
        speed,
        reported_usd: None,
        key: format!("codex:{}:{at}:{sig}", st.session),
    })
}

/// One of OpenCode's finished replies (T3's `parseOpenCodeMessage`). OpenCode gives reasoning
/// apart from output; its cost is 0 for a subscription, so only a cost above 0 is taken.
pub fn opencode(row: &Value) -> Option<Record> {
    let d = &row["data"];
    let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let model = s(&d["model"]["id"])
        .or_else(|| s(&d["model"]["modelID"]))
        .or_else(|| s(&d["modelID"]))?;
    let at_ms = d["time"]["created"]
        .as_i64()
        .or_else(|| row["updated"].as_i64())?;
    let t = &d["tokens"];
    let reasoning = count(&t["reasoning"]);
    let tokens = TokenTotals {
        input: count(&t["input"]),
        cache_read: count(&t["cache"]["read"]),
        cache_write: count(&t["cache"]["write"]),
        output: count(&t["output"]) + reasoning,
        reasoning,
    };
    if tokens.total() == 0 {
        return None;
    }
    let id = s(&row["id"])?;
    let reported_usd = d["cost"].as_f64().filter(|c| c.is_finite() && *c > 0.);
    Some(Record {
        provider: "opencode",
        at_ms,
        model,
        tokens,
        cache_write_1h: 0,
        speed: Speed::Standard,
        reported_usd,
        key: format!("opencode:{id}"),
    })
}

/// A key's hash, as kept: FNV-1a, the same on every machine and version.
fn hash(key: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h as i64
}

/// Count what a read of thread `thread`'s VM found and keep its cursor, in one transaction: a
/// request already counted isn't counted again, and one older than [`KEEP_DAYS`] isn't
/// counted. Each request is kept at its context tier by `rates` (the prices as they are). How
/// many requests were new.
pub fn keep(
    store: &Store,
    thread: &str,
    records: &[Record],
    cursor: &Cursor,
    rates: &RateTable,
    now_ms: i64,
) -> Result<usize> {
    let cutoff = now_ms - KEEP_DAYS * DAY_MS;
    let tx = store.write()?;
    let mut new = 0;
    for r in records.iter().filter(|r| r.at_ms >= cutoff) {
        if tx.execute(
            "INSERT OR IGNORE INTO agent_usage_seen (key, at) VALUES (?1, ?2)",
            params![hash(&r.key), r.at_ms],
        )? == 0
        {
            continue;
        }
        new += 1;
        let t = &r.tokens;
        let tier = rates.tier(&r.model, t.input + t.cache_read + t.cache_write);
        tx.execute(
            "INSERT INTO agent_usage (hour, provider, model, speed, reported, tier, input, cache_read, cache_write, cache_write_1h, output, reasoning, requests, reported_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13)
             ON CONFLICT(hour, provider, model, speed, reported, tier) DO UPDATE SET
               input = input + excluded.input, cache_read = cache_read + excluded.cache_read, cache_write = cache_write + excluded.cache_write,
               cache_write_1h = cache_write_1h + excluded.cache_write_1h, output = output + excluded.output, reasoning = reasoning + excluded.reasoning,
               requests = requests + 1, reported_usd = reported_usd + excluded.reported_usd",
            params![
                r.at_ms - r.at_ms.rem_euclid(BUCKET_MS),
                r.provider,
                r.model,
                r.speed.as_str(),
                r.reported_usd.is_some(),
                tier as i64,
                t.input as i64,
                t.cache_read as i64,
                t.cache_write as i64,
                r.cache_write_1h.min(t.cache_write) as i64,
                t.output as i64,
                t.reasoning as i64,
                r.reported_usd.unwrap_or(0.)
            ],
        )?;
    }
    tx.execute("DELETE FROM agent_usage_seen WHERE at < ?1", [cutoff])?;
    tx.execute(
        "DELETE FROM agent_usage WHERE hour < ?1",
        [cutoff - cutoff.rem_euclid(BUCKET_MS)],
    )?;
    let now = crate::util::now_iso();
    for (key, value) in [
        (cursor_key(thread), serde_json::to_string(cursor)?),
        (READ_KEY.to_string(), now_ms.to_string()),
    ] {
        tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![key, value, now],
        )?;
    }
    tx.commit()?;
    Ok(new)
}

/// When a VM's records were last read.
pub fn read_ms(store: &Store) -> Option<i64> {
    store
        .kv_get(READ_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
}

/// The view: the last [`VIEW_DAYS`] days' hours, each priced with `rates` (an hour's model and
/// speed in a row of its own for each context tier, and for a cost the harness gave). Rows
/// kept before tiers and hour-long cache writes were (migration 16) are at the base rates.
pub fn view(
    store: &Store,
    rates: &RateTable,
    prices_ms: Option<i64>,
    now_ms: i64,
) -> Result<AgentUsage> {
    let since = now_ms - VIEW_DAYS * DAY_MS;
    let mut st = store.conn().prepare(
        "SELECT hour, provider, model, speed, reported, input, cache_read, cache_write, output, reasoning, requests, reported_usd, tier, cache_write_1h
         FROM agent_usage WHERE hour >= ?1 ORDER BY hour, provider, model, speed, reported, tier",
    )?;
    let rows = st.query_map([since - since.rem_euclid(BUCKET_MS)], |r| {
        let n = |i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
        let tokens = TokenTotals {
            input: n(5)?,
            cache_read: n(6)?,
            cache_write: n(7)?,
            output: n(8)?,
            reasoning: n(9)?,
        };
        let speed = Speed::parse(&r.get::<_, String>(3)?);
        let model: String = r.get(2)?;
        let reported = r.get::<_, bool>(4)?.then_some(r.get::<_, f64>(11)?);
        let cost = crate::prices::price(rates, &model, speed, n(12)?, &tokens, n(13)?, reported);
        Ok(UsageHour {
            hour_ms: r.get(0)?,
            provider: r.get(1)?,
            cost,
            model,
            speed,
            tokens,
            requests: n(10)?,
        })
    })?;
    Ok(AgentUsage {
        hours: rows.collect::<rusqlite::Result<_>>()?,
        read_ms: read_ms(store),
        prices_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prices::RateTable;
    use flotilla_proto::UsageCost;
    use serde_json::json;
    use std::path::PathBuf;

    const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata");
    const NOW: i64 = 1_793_540_000_000;
    const CLAUDE: &str = ".claude/projects/-home-user/2b36a76d-03f9-4adb-aa05-4433cafe85c8.jsonl";
    const ROLLOUT: &str = ".codex/sessions/2026/11/01/rollout-2026-11-01T13-05-17-01a19736-dc1a-7c02-9912-459349b8ba1f.jsonl";

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

    /// A VM's home with the harnesses' records (recorded in real VMs on 2026-11-01), and a
    /// thread whose reads of it are kept in a store.
    struct Vm {
        root: PathBuf,
        store: Store,
        sandbox: String,
        max_files: usize,
        rates: RateTable,
    }

    impl Vm {
        fn new(name: &str) -> Vm {
            let root = std::env::temp_dir().join(format!(
                "flotilla-spend-{name}-{}-{}",
                std::process::id(),
                crate::util::now_ms()
            ));
            std::fs::create_dir_all(root.join("home")).unwrap();
            Vm {
                root,
                store: Store::open_in_memory().unwrap(),
                sandbox: "sb1".into(),
                max_files: MAX_FILES,
                rates: prices(),
            }
        }

        fn home(&self) -> PathBuf {
            self.root.join("home")
        }

        fn put(&self, at: &str, from: &str) {
            let to = self.home().join(at);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(format!("{DATA}/{from}"), to).unwrap();
        }

        fn write(&self, at: &str, text: &str) {
            let to = self.home().join(at);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::write(to, text).unwrap();
        }

        fn append(&self, at: &str, text: &str) {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(self.home().join(at))
                .unwrap();
            f.write_all(text.as_bytes()).unwrap();
        }

        fn opencode(&self) {
            let db = self.home().join(".local/share/opencode/opencode.db");
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            let sql =
                std::fs::read_to_string(format!("{DATA}/meter/opencode/opencode.sql")).unwrap();
            assert!(
                std::process::Command::new("sqlite3")
                    .arg(&db)
                    .arg(sql)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        fn sql(&self, sql: &str) {
            let db = self.home().join(".local/share/opencode/opencode.db");
            assert!(
                std::process::Command::new("sqlite3")
                    .arg(&db)
                    .arg(sql)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        /// Run the read as the exec would, with `budget` bytes, and keep what it found: how many
        /// requests were new, and whether there's more.
        fn read_with(&self, budget: Option<u64>) -> (usize, bool) {
            self.read_env(budget, &[]).0
        }

        /// The read, with these in its environment too; and the files it read.
        fn read_env(&self, budget: Option<u64>, env: &[(&str, &str)]) -> ((usize, bool), usize) {
            let c = cursor(&self.store, "t1", &self.sandbox);
            let mut cmd = command(&c);
            if let Some(b) = budget {
                cmd = cmd.replace(&format!("BUDGET={BUDGET}"), &format!("BUDGET={b}"));
            }
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(cmd)
                .env("HOME", self.home())
                .envs(env.iter().copied())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let reading = parse(&String::from_utf8_lossy(&out.stdout));
            let (records, c) = records_keeping(&reading, c, false, NOW, self.max_files);
            (
                (
                    keep(&self.store, "t1", &records, &c, &self.rates, NOW).unwrap(),
                    reading.more,
                ),
                reading.files.len(),
            )
        }

        fn read(&self) -> usize {
            self.read_with(None).0
        }

        /// Each harness's totals kept: (requests, tokens).
        fn totals(&self) -> Vec<(String, String, u64, TokenTotals)> {
            let v = view(&self.store, &RateTable::default(), None, NOW).unwrap();
            let mut by: BTreeMap<(String, String), (u64, TokenTotals)> = BTreeMap::new();
            for h in v.hours {
                let e = by.entry((h.provider, h.model)).or_default();
                e.0 += h.requests;
                e.1.add(&h.tokens);
            }
            by.into_iter()
                .map(|((p, m), (n, t))| (p, m, n, t))
                .collect()
        }

        /// What a harness's requests cost, at the recorded prices.
        fn cost(&self, provider: &str) -> f64 {
            view(&self.store, &self.rates, None, NOW)
                .unwrap()
                .hours
                .iter()
                .filter(|h| h.provider == provider)
                .map(|h| h.cost.unwrap().total())
                .sum()
        }
    }

    impl Drop for Vm {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Recorded in two live VMs on 2026-11-01 (long strings cut to 160 characters): Claude Code
    /// on Haiku 5.5, a turn with a subagent then another with another subagent (its own
    /// files), and Codex on GPT-6 Luna, two turns. Read as the server read them: once each
    /// turn ended (its first half, then the rest), then again with nothing new, and once more
    /// a few kilobytes at a time. Both match what the server kept, and Codex's own total
    /// (52,952 tokens).
    #[test]
    fn replays_a_live_run_turn_by_turn() {
        if !have(&["bash", "jq", "base64"]) {
            return;
        }
        const MAIN: &str = "claude/-home-user/325da5b9-e855-4d34-8eda-abafe637b1a2.jsonl";
        const SUB1: &str = "claude/-home-user/325da5b9-e855-4d34-8eda-abafe637b1a2/subagents/agent-a0e55082b7c440057.jsonl";
        const SUB2: &str = "claude/-home-user/325da5b9-e855-4d34-8eda-abafe637b1a2/subagents/agent-a40088612877b828d.jsonl";
        const CODEX: &str =
            "codex/rollout-2026-11-01T20-21-07-01a198c5-dcb2-7d42-8e21-baedbe8754ba.jsonl";
        let at = |f: &str| match f.strip_prefix("claude/") {
            Some(rest) => format!(".claude/projects/{rest}"),
            None => format!(
                ".codex/sessions/2026/11/01/{}",
                f.trim_start_matches("codex/")
            ),
        };
        let want = [
            (
                "claude-code".to_string(),
                "claude-haiku-5-5".to_string(),
                8,
                t(16, 107_961, 30_589, 788, 0),
            ),
            (
                "codex".into(),
                "gpt-6-luna".into(),
                4,
                t(7_022, 45_824, 0, 106, 0),
            ),
        ];
        let vm = Vm::new("live");
        let whole = |f: &str| std::fs::read_to_string(format!("{DATA}/spend/{f}")).unwrap();
        // The first turn: half of the session and the rollout, the first subagent.
        let halves = |f: &str| {
            let text = whole(f);
            let lines: Vec<&str> = text.lines().collect();
            let half = lines.len() / 2;
            (
                lines[..half].join("\n") + "\n",
                lines[half..].join("\n") + "\n",
            )
        };
        for f in [MAIN, CODEX] {
            let to = vm.home().join(at(f));
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::write(to, halves(f).0).unwrap();
        }
        vm.put(&at(SUB1), &format!("spend/{SUB1}"));
        let first = vm.read();
        assert!(first > 0);
        // The second turn: the rest, and the second subagent.
        for f in [MAIN, CODEX] {
            vm.append(&at(f), &halves(f).1);
        }
        vm.put(&at(SUB2), &format!("spend/{SUB2}"));
        assert_eq!(first + vm.read(), 12);
        assert_eq!(vm.read(), 0);
        assert_eq!(vm.totals(), want);
        // Of Claude Code's 30,589 cache writes, 11,756 were kept an hour ($0.20 a million, not
        // five minutes' $0.125): $0.006181, not the $0.005299 all at five minutes' rate.
        let claude: f64 = 16. * 0.1e-6
            + 107_961. * 0.01e-6
            + (30_589. - 11_756.) * 0.125e-6
            + 11_756. * 0.2e-6
            + 788. * 0.5e-6;
        assert!(
            (vm.cost("claude-code") - claude).abs() < 1e-12,
            "{}",
            vm.cost("claude-code")
        );
        // The same files, whole, read a few kilobytes at a time: the same totals.
        let again = Vm::new("live-again");
        for f in [MAIN, SUB1, SUB2, CODEX] {
            again.put(&at(f), &format!("spend/{f}"));
        }
        let mut reads = 0;
        while again.read_with(Some(24_000)).1 {
            reads += 1;
            assert!(reads < 60);
        }
        assert!(reads > 5);
        assert_eq!(again.totals(), want);
        assert!((again.cost("claude-code") - claude).abs() < 1e-12);
    }

    fn t(
        input: u64,
        cache_read: u64,
        cache_write: u64,
        output: u64,
        reasoning: u64,
    ) -> TokenTotals {
        TokenTotals {
            input,
            cache_read,
            cache_write,
            output,
            reasoning,
        }
    }

    /// LiteLLM's prices for the models recorded here, as fetched on 2026-11-01.
    fn prices() -> RateTable {
        RateTable::parse(
            &serde_json::from_str(
                &std::fs::read_to_string(format!("{DATA}/prices/litellm.json")).unwrap(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn reads_each_harnesses_records_once() {
        if !have(&["bash", "jq", "sqlite3", "base64"]) {
            return;
        }
        let vm = Vm::new("each");
        vm.put(
            CLAUDE,
            "meter/claude/2b36a76d-03f9-4adb-aa05-4433cafe85c8.jsonl",
        );
        vm.put(
            ROLLOUT,
            "meter/codex/rollout-2026-11-01T13-05-17-01a19736-dc1a-7c02-9912-459349b8ba1f.jsonl",
        );
        vm.opencode();
        // Claude Code: four replies, each written once per content block (its usage repeated),
        // and the reply being written as it was copied is left for the next read. Codex: four
        // requests; its input includes the cache. OpenCode: five finished replies.
        assert_eq!(vm.read(), 13);
        let want = [
            (
                "claude-code".to_string(),
                "claude-haiku-5-5".to_string(),
                4,
                t(8, 80_006, 7_855, 1_254, 0),
            ),
            (
                "codex".into(),
                "gpt-6-luna".into(),
                4,
                t(53_979 - 39_680, 39_680, 0, 298, 0),
            ),
            (
                "opencode".into(),
                "gpt-6-luna".into(),
                5,
                t(21_240, 81_152, 0, 279, 0),
            ),
        ];
        assert_eq!(vm.totals(), want);
        // Read again: nothing has grown, so nothing's read or counted.
        assert_eq!(vm.read(), 0);
        assert_eq!(vm.totals(), want);
        // The line being written is finished, and another reply follows half written: the
        // finished one counts, once.
        vm.append(CLAUDE, r#"ens":1,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":4},"id":"msg_mid"},"timestamp":"2026-11-01T13:25:21.000Z"}"#);
        vm.append(CLAUDE, "\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-haiku-5-5\",\"id\":\"msg_next\",\"usa");
        assert_eq!(vm.read(), 1);
        assert_eq!(
            vm.totals()[0],
            (
                "claude-code".into(),
                "claude-haiku-5-5".into(),
                5,
                t(9, 80_008, 7_858, 1_258, 0)
            )
        );
    }

    /// The recorded Claude Code session (Haiku 5.5, four replies, every cache write kept an
    /// hour) priced as LiteLLM's table prices it, worked out by hand: 8 tokens in at $0.10 a
    /// million, 80,006 read from the cache at $0.01, 7,855 written to it for an hour at $0.20
    /// (not five minutes' $0.125), 1,254 out at $0.50.
    #[test]
    fn prices_a_recorded_session_as_billed() {
        if !have(&["bash", "jq", "base64"]) {
            return;
        }
        let vm = Vm::new("billed");
        vm.put(
            CLAUDE,
            "meter/claude/2b36a76d-03f9-4adb-aa05-4433cafe85c8.jsonl",
        );
        assert_eq!(vm.read(), 4);
        let v = view(&vm.store, &vm.rates, None, NOW).unwrap();
        let mut cost = UsageCost {
            premium: Some(0.),
            ..UsageCost::default()
        };
        for h in &v.hours {
            cost.add(&h.cost.unwrap());
        }
        let want: f64 = 8. * 0.1e-6 + 80_006. * 0.01e-6 + 7_855. * 0.2e-6 + 1_254. * 0.5e-6;
        assert!((want - 0.002_998_86).abs() < 1e-12);
        assert!(
            (cost.total() - want).abs() < 1e-12
                && (cost.cache_write - 7_855. * 0.2e-6).abs() < 1e-12,
            "{cost:?}"
        );
    }

    /// Each request's context tier is decided as it's counted: two requests below Haiku 5.5's
    /// 100k threshold in an hour stay at the base rates however much they come to together,
    /// and one past it is kept apart, at its tier's.
    #[test]
    fn a_long_prompt_is_kept_and_priced_at_its_tier() {
        let store = Store::open_in_memory().unwrap();
        let rates = prices();
        let rec = |key: &str, input: u64, cache_read: u64| Record {
            provider: "claude-code",
            at_ms: NOW - 1,
            model: "claude-haiku-5-5[1m]".into(),
            tokens: t(input, cache_read, 0, 1_000, 0),
            cache_write_1h: 0,
            speed: Speed::Standard,
            reported_usd: None,
            key: key.into(),
        };
        let all = [
            rec("a", 60_000, 0),
            rec("b", 20_000, 40_000),
            rec("c", 150_000, 50_000),
        ];
        assert_eq!(
            keep(&store, "t1", &all, &Cursor::default(), &rates, NOW).unwrap(),
            3
        );
        let v = view(&store, &rates, None, NOW).unwrap();
        let rows: Vec<(u64, u64, f64)> = v
            .hours
            .iter()
            .map(|h| {
                (
                    h.requests,
                    h.tokens.input + h.tokens.cache_read,
                    h.cost.unwrap().total(),
                )
            })
            .collect();
        assert_eq!(rows.len(), 2, "{rows:?}");
        // Base: 80k in at $0.10, 40k from the cache at $0.01, 2k out at $0.50 a million.
        assert_eq!((rows[0].0, rows[0].1), (2, 120_000));
        assert!(
            (rows[0].2 - (80_000. * 0.1e-6 + 40_000. * 0.01e-6 + 2_000. * 0.5e-6)).abs() < 1e-12,
            "{rows:?}"
        );
        // Past 100k: 150k in at $0.50, 50k from the cache at $0.05, 1k out at $2.50.
        assert_eq!((rows[1].0, rows[1].1), (1, 200_000));
        assert!(
            (rows[1].2 - (150_000. * 0.5e-6 + 50_000. * 0.05e-6 + 1_000. * 2.5e-6)).abs() < 1e-12,
            "{rows:?}"
        );
    }

    /// Before there are prices (the first fetch failed), nothing is counted and the cursor
    /// stays: once they're fetched, the same records are read again and a long prompt is at
    /// its tier, not kept for good at the base rates.
    #[test]
    fn nothing_is_counted_before_there_are_prices() {
        if !have(&["bash", "jq", "base64"]) {
            return;
        }
        let vm = Vm::new("no-prices");
        vm.write(
            ".claude/projects/p/long.jsonl",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-11-01T10:00:00Z\",\"requestId\":\"req_l\",\"message\":{\"id\":\"msg_l\",\"model\":\"claude-haiku-5-5\",\"usage\":{\"input_tokens\":200000,\"output_tokens\":1000}}}\n",
        );
        let prices = crate::prices::for_db(&vm.root.join("flotilla.db"));
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{DATA}/prices/litellm.json")).unwrap(),
        )
        .unwrap();
        // A read with the first fetch failing: read, but nothing counted or moved on.
        let c = cursor(&vm.store, "t1", &vm.sandbox);
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(command(&c))
            .env("HOME", vm.home())
            .output()
            .unwrap();
        assert_eq!(
            records(&parse(&String::from_utf8_lossy(&out.stdout)), c, false, NOW)
                .0
                .len(),
            1
        );
        assert!(prices.for_counting_with(NOW, || None).is_none());
        assert_eq!(
            cursor(&vm.store, "t1", &vm.sandbox),
            Cursor {
                sandbox: vm.sandbox.clone(),
                ..Cursor::default()
            }
        );
        // The next read, a minute on, with the prices fetched: counted, at Haiku 5.5's 100k tier.
        let rates = prices
            .for_counting_with(NOW + 60_000, || Some(doc.clone()))
            .unwrap();
        let mut vm = vm;
        vm.rates = (*rates).clone();
        assert_eq!(vm.read(), 1);
        assert!(
            (vm.cost("claude-code") - 0.1025).abs() < 1e-12,
            "{}",
            vm.cost("claude-code")
        );
        // Fetched, the table is used as it is.
        assert!(
            prices
                .for_counting_with(NOW + 120_000, || panic!("fetched again"))
                .is_some()
        );
    }

    #[test]
    fn a_vm_made_again_counts_nothing_twice() {
        if !have(&["bash", "jq", "sqlite3", "base64"]) {
            return;
        }
        let mut vm = Vm::new("again");
        vm.put(
            CLAUDE,
            "meter/claude/2b36a76d-03f9-4adb-aa05-4433cafe85c8.jsonl",
        );
        vm.put(
            ROLLOUT,
            "meter/codex/rollout-2026-11-01T13-05-17-01a19736-dc1a-7c02-9912-459349b8ba1f.jsonl",
        );
        vm.opencode();
        assert_eq!(vm.read(), 13);
        let before = vm.totals();
        // The thread's VM is made again from a copy of its disk: everything is read from the
        // start, and none of it is new.
        vm.sandbox = "sb2".into();
        assert_eq!(vm.read(), 0);
        // A Claude Code session resumed into another file copies the replies before it.
        let copy = std::fs::read_to_string(vm.home().join(CLAUDE)).unwrap();
        std::fs::write(
            vm.home().join(".claude/projects/-home-user/resumed.jsonl"),
            copy,
        )
        .unwrap();
        assert_eq!(vm.read(), 0);
        // A file made again at the same path, with other replies, is read from its start.
        std::fs::write(vm.home().join(CLAUDE), "{\"type\":\"assistant\",\"timestamp\":\"2026-11-01T13:30:00Z\",\"message\":{\"model\":\"claude-haiku-5-5\",\"id\":\"msg_new\",\"usage\":{\"input_tokens\":5,\"output_tokens\":6}}}\n").unwrap();
        assert_eq!(vm.read(), 1);
        assert_eq!(vm.totals()[0].2, before[0].2 + 1);
        // OpenCode: a reply still being written waits until it's finished, then counts once.
        vm.sql("insert into message values ('msg_streaming', 'ses_e68c52bd8ffeBR4g6hLadecv9x', 1793539600000, 1793539600000, '{\"role\":\"assistant\",\"modelID\":\"gpt-6-luna\",\"tokens\":{\"input\":10,\"output\":0,\"reasoning\":0,\"cache\":{\"write\":0,\"read\":0}},\"time\":{\"created\":1793539600000}}')");
        assert_eq!(vm.read(), 0);
        vm.sql("update message set time_updated = 1793539605000, data = '{\"role\":\"assistant\",\"modelID\":\"gpt-6-luna\",\"tokens\":{\"input\":10,\"output\":20,\"reasoning\":5,\"cache\":{\"write\":0,\"read\":0}},\"time\":{\"created\":1793539600000,\"completed\":1793539605000}}' where id = 'msg_streaming'");
        assert_eq!(vm.read(), 1);
        assert_eq!(vm.read(), 0);
        let oc = vm.totals().into_iter().find(|r| r.0 == "opencode").unwrap();
        // Its reasoning is output too.
        assert_eq!((oc.2, oc.3.output, oc.3.reasoning), (6, 279 + 25, 5));
    }

    #[test]
    fn a_small_budget_reads_on_where_it_stopped() {
        if !have(&["bash", "jq", "base64"]) {
            return;
        }
        let vm = Vm::new("budget");
        vm.put(
            CLAUDE,
            "meter/claude/2b36a76d-03f9-4adb-aa05-4433cafe85c8.jsonl",
        );
        vm.put(
            ROLLOUT,
            "meter/codex/rollout-2026-11-01T13-05-17-01a19736-dc1a-7c02-9912-459349b8ba1f.jsonl",
        );
        // 1.5 KB at a time: a few lines a read, the Codex state carried between them.
        let mut reads = 0;
        loop {
            reads += 1;
            let (_, more) = vm.read_with(Some(1500));
            if !more {
                break;
            }
            assert!(reads < 40);
        }
        assert!(reads > 5);
        let totals = vm.totals();
        assert_eq!(totals[0].2, 4);
        assert_eq!(
            totals[1],
            (
                "codex".into(),
                "gpt-6-luna".into(),
                4,
                t(53_979 - 39_680, 39_680, 0, 298, 0)
            )
        );
    }

    /// A Claude Code reply about `size` bytes long, as its own line.
    fn reply(id: &str, size: usize) -> String {
        let line = |pad: usize| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-11-01T10:00:00Z","message":{{"id":"{id}","model":"claude-haiku-5-5","usage":{{"input_tokens":1,"output_tokens":2}},"content":"{}"}}}}"#,
                "x".repeat(pad)
            )
        };
        let bare = line(0).len();
        line(size.saturating_sub(bare + 1)) + "\n"
    }

    /// A line that doesn't fit in what's left of a read's budget waits for the next read,
    /// whole; one longer than the whole budget is passed over, and what follows it counts.
    #[test]
    fn a_line_that_doesnt_fit_waits_and_one_too_long_is_passed_over() {
        if !have(&["bash", "jq", "base64"]) {
            return;
        }
        let vm = Vm::new("fit");
        vm.write(".claude/projects/p/a.jsonl", &reply("a1", 800));
        vm.write(".claude/projects/p/b.jsonl", &reply("b1", 800));
        let mut reads = 0;
        while vm.read_with(Some(1_000)).1 {
            reads += 1;
            assert!(reads < 10);
        }
        assert_eq!(vm.totals()[0].2, 2, "both, a read each");
        // A line longer than the whole budget, then a reply.
        vm.append(
            ".claude/projects/p/a.jsonl",
            &(reply("a2", 3_000) + &reply("a3", 400)),
        );
        let mut reads = 0;
        while vm.read_with(Some(1_000)).1 {
            reads += 1;
            assert!(reads < 10);
        }
        assert_eq!(
            vm.totals()[0].2,
            3,
            "the long one passed over, the next counted"
        );
    }

    /// With more files than the cursor keeps, the ones read to their end are forgotten only once
    /// everything has been read, and a file forgotten is passed over until it changes: every
    /// file is reached, and a read with nothing new reads nothing.
    #[test]
    fn forgetting_files_never_keeps_others_from_being_read() {
        if !have(&["bash", "jq", "base64"]) {
            return;
        }
        let mut vm = Vm::new("forget");
        vm.max_files = 2;
        for i in 0..6 {
            vm.write(
                &format!(".claude/projects/p/s{i}.jsonl"),
                &reply(&format!("m{i}"), 900),
            );
        }
        let mut reads = 0;
        while vm.read_with(Some(1_000)).1 {
            reads += 1;
            assert!(reads < 20, "every file is reached");
        }
        assert_eq!(vm.totals()[0].2, 6);
        let c = cursor(&vm.store, "t1", "sb1");
        assert_eq!(c.files.len(), 2);
        assert!(c.since_s > 0);
        // Later: nothing has changed since that read, so nothing's read, forgotten or not.
        let later = Cursor {
            since_s: c.since_s + 10,
            ..c
        };
        keep(&vm.store, "t1", &[], &later, &vm.rates, NOW).unwrap();
        assert_eq!(vm.read_env(Some(1_000), &[]), ((0, false), 0));
        // A forgotten file that changes is read again (from its start: nothing counted twice).
        vm.append(".claude/projects/p/s0.jsonl", &reply("m6", 300));
        let c = cursor(&vm.store, "t1", "sb1");
        keep(
            &vm.store,
            "t1",
            &[],
            &Cursor { since_s: 0, ..c },
            &vm.rates,
            NOW,
        )
        .unwrap();
        let mut reads = 0;
        while vm.read_with(Some(4_000)).1 {
            reads += 1;
            assert!(reads < 20);
        }
        assert_eq!(vm.totals()[0].2, 7);
    }

    /// OpenCode's replies that changed at the same moment are read on in order of id, however
    /// many there are.
    #[test]
    fn opencode_replies_at_the_same_moment_are_all_read() {
        if !have(&["bash", "jq", "sqlite3", "base64"]) {
            return;
        }
        let vm = Vm::new("oc-ties");
        vm.opencode();
        for i in 0..5 {
            vm.sql(&format!("insert into message values ('msg_tie{i}', 'ses_e68c52bd8ffeBR4g6hLadecv9x', 1793539600000, 1793539700000, '{{\"role\":\"assistant\",\"modelID\":\"gpt-6-luna\",\"tokens\":{{\"input\":1,\"output\":1,\"reasoning\":0,\"cache\":{{\"write\":0,\"read\":0}}}},\"time\":{{\"created\":1793539600000,\"completed\":1793539700000}}}}')"));
        }
        let mut reads = 0;
        while vm.read_env(None, &[("OC_LIMIT", "2")]).0.1 {
            reads += 1;
            assert!(reads < 10);
        }
        assert_eq!(
            vm.totals()
                .into_iter()
                .find(|r| r.0 == "opencode")
                .unwrap()
                .2,
            10
        );
        // A page that ends a pass right after a full one, then a reply that finished late at the
        // same moment with a lower id: the next pass starts a minute back and finds it.
        let late = |id: &str, at: i64| {
            format!(
                "insert into message values ('{id}', 'ses_e68c52bd8ffeBR4g6hLadecv9x', {at}, {at}, '{{\"role\":\"assistant\",\"modelID\":\"gpt-6-luna\",\"tokens\":{{\"input\":1,\"output\":1,\"reasoning\":0,\"cache\":{{\"write\":0,\"read\":0}}}},\"time\":{{\"created\":{at},\"completed\":{at}}}}}')"
            )
        };
        // A pass, two at a time: how many replies were new.
        let pass = || {
            let mut new = 0;
            for _ in 0..10 {
                let ((n, more), _) = vm.read_env(None, &[("OC_LIMIT", "2")]);
                new += n;
                if !more {
                    return new;
                }
            }
            panic!("the pass didn't end");
        };
        vm.sql(&late("msg_tie0a", 1793539700000));
        assert_eq!(pass(), 1);
        // And one a little older than the newest, after a short page.
        vm.sql(&late("msg_late", 1793539690000));
        assert_eq!(pass(), 1);
        assert_eq!(pass(), 0);
    }

    fn count(ts: &str, last: [u64; 3], total: Option<[u64; 3]>) -> Value {
        let u = |[i, c, o]: [u64; 3]| json!({"input_tokens": i, "cached_input_tokens": c, "output_tokens": o, "reasoning_output_tokens": 0, "total_tokens": i + o});
        let mut info = json!({"last_token_usage": u(last)});
        if let Some(t) = total {
            info["total_token_usage"] = u(t);
        }
        json!({"timestamp": ts, "type": "event_msg", "payload": {"type": "token_count", "info": info}})
    }

    fn run(lines: &[Value], fast: bool) -> Vec<Record> {
        let mut st = CodexState::default();
        lines
            .iter()
            .filter_map(|l| codex(l, &mut st, fast))
            .collect()
    }

    /// Codex's counts, as its rollouts write them: one sent again, a resumed session's totals
    /// starting over, the counts after a compaction, a fork's copied history, the service tier.
    #[test]
    fn codex_counts_each_request_once() {
        let meta = |extra: Value| {
            let mut p = json!({"id": "s1", "model_provider": "openai"});
            p.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            json!({"timestamp": "2026-11-01T10:00:00.000Z", "type": "session_meta", "payload": p})
        };
        let model = json!({"timestamp": "2026-11-01T10:00:00.100Z", "type": "turn_context", "payload": {"model": "gpt-6-luna"}});
        let a = count(
            "2026-11-01T10:00:01.000Z",
            [100, 50, 10],
            Some([100, 50, 10]),
        );
        let b = count(
            "2026-11-01T10:00:02.000Z",
            [200, 150, 20],
            Some([300, 200, 30]),
        );
        // The same count sent again; a count of the context's size after compacting.
        let again = count(
            "2026-11-01T10:00:02.500Z",
            [200, 150, 20],
            Some([300, 200, 30]),
        );
        let compacted = count("2026-11-01T10:00:03.000Z", [0, 0, 0], Some([0, 0, 0]));
        // Resumed in a new process: its totals start again at this request.
        let resumed = count(
            "2026-11-01T11:00:00.000Z",
            [400, 300, 40],
            Some([400, 300, 40]),
        );
        let r = run(
            &[
                meta(json!({})),
                model.clone(),
                a.clone(),
                b.clone(),
                again,
                compacted,
                resumed.clone(),
                resumed,
            ],
            false,
        );
        let tokens: Vec<(u64, u64, u64)> = r
            .iter()
            .map(|r| (r.tokens.input, r.tokens.cache_read, r.tokens.output))
            .collect();
        assert_eq!(tokens, [(50, 50, 10), (50, 150, 20), (100, 300, 40)]);
        assert!(r.iter().all(|r| r.model == "gpt-6-luna"
            && r.speed == Speed::Standard
            && r.key.starts_with("codex:s1:")));
        // Older Codex: no totals, so the same count twice in a row is one request.
        let old = |ts: &str| count(ts, [10, 0, 1], None);
        assert_eq!(
            run(
                &[
                    meta(json!({})),
                    model.clone(),
                    old("2026-11-01T10:00:01Z"),
                    old("2026-11-01T10:00:02Z")
                ],
                false
            )
            .len(),
            1
        );
        // A fork copies its parent's counts in a burst: only what it does after them counts.
        let fork = run(
            &[
                meta(json!({"forked_from_id": "s0"})),
                model.clone(),
                count(
                    "2026-11-01T10:00:00.010Z",
                    [100, 50, 10],
                    Some([100, 50, 10]),
                ),
                count(
                    "2026-11-01T10:00:00.030Z",
                    [200, 150, 20],
                    Some([300, 200, 30]),
                ),
                count(
                    "2026-11-01T10:00:09.000Z",
                    [500, 0, 5],
                    Some([800, 200, 35]),
                ),
            ],
            false,
        );
        assert_eq!(
            fork.iter().map(|r| r.tokens.input).collect::<Vec<_>>(),
            [500]
        );
        // The tier from the thread's settings, else the thread's own (it ran Fast).
        let tier = json!({"timestamp": "2026-11-01T10:00:00.200Z", "type": "event_msg", "payload": {"type": "thread_settings_applied", "thread_settings": {"service_tier": "priority"}}});
        assert_eq!(
            run(&[meta(json!({})), model.clone(), tier, a.clone()], false)[0].speed,
            Speed::Fast
        );
        assert_eq!(
            run(&[meta(json!({})), model.clone(), a.clone()], true)[0].speed,
            Speed::Fast
        );
        // No model yet: not a request it can say anything about.
        assert!(run(&[meta(json!({})), a], false).is_empty());
    }

    #[test]
    fn claude_counts_advisors_at_their_model_and_skips_placeholders() {
        let v = json!({"timestamp": "2026-11-01T10:00:00Z", "requestId": "req_1", "sessionId": "s", "message": {"id": "msg_1", "model": "claude-opus-5-5", "usage": {
            "input_tokens": 10, "cache_read_input_tokens": 20, "cache_creation_input_tokens": 30, "output_tokens": 40, "speed": "fast",
            "cache_creation": {"ephemeral_5m_input_tokens": 18, "ephemeral_1h_input_tokens": 12},
            "iterations": [{"type": "advisor_message", "model": "claude-fable-5-1", "input_tokens": 1, "output_tokens": 2, "cache_creation_input_tokens": 3, "cache_creation": {"ephemeral_1h_input_tokens": 9}}, {"type": "other"}]}}});
        let r = claude(&v);
        assert_eq!(r.len(), 2);
        assert_eq!(
            (
                r[0].key.as_str(),
                r[0].speed,
                r[0].tokens,
                r[0].cache_write_1h
            ),
            ("claude:msg_1:req_1", Speed::Fast, t(10, 20, 30, 40, 0), 12)
        );
        // An hour's writes are no more than the writes.
        assert_eq!(
            (
                r[1].model.as_str(),
                r[1].key.as_str(),
                r[1].tokens,
                r[1].cache_write_1h
            ),
            (
                "claude-fable-5-1",
                "claude:msg_1:req_1:advisor:0",
                t(1, 0, 3, 2, 0),
                3
            )
        );
        let mut synthetic = v.clone();
        synthetic["message"]["model"] = json!("<synthetic>");
        assert!(claude(&synthetic).is_empty());
    }

    /// The view prices each hour as T3 does; a model with no known price has tokens and no
    /// cost; what's older than kept is dropped.
    #[test]
    fn the_view_prices_what_it_can() {
        let store = Store::open_in_memory().unwrap();
        let rates = prices();
        let rec = |key: &str, model: &str, at_ms: i64| Record {
            provider: "claude-code",
            at_ms,
            model: model.into(),
            tokens: t(50_000, 0, 0, 50_000, 0),
            cache_write_1h: 0,
            speed: Speed::Standard,
            reported_usd: None,
            key: key.into(),
        };
        let old = NOW - (KEEP_DAYS + 1) * DAY_MS;
        let all = [
            rec("a", "claude-haiku-5-5", NOW - 1),
            rec("b", "mystery-1", NOW - 2),
            rec("a", "claude-haiku-5-5", NOW - 1),
            rec("c", "claude-haiku-5-5", old),
        ];
        assert_eq!(
            keep(&store, "t1", &all, &Cursor::default(), &rates, NOW).unwrap(),
            2
        );
        let v = view(&store, &rates, Some(5), NOW).unwrap();
        assert_eq!(
            (v.read_ms, v.prices_ms, v.hours.len()),
            (Some(NOW), Some(5), 2)
        );
        let haiku = v
            .hours
            .iter()
            .find(|h| h.model == "claude-haiku-5-5")
            .unwrap();
        assert_eq!((haiku.requests, haiku.hour_ms % BUCKET_MS), (1, 0));
        // 50k in at $0.10 and out at $0.50 a million (below its 100k tier).
        assert!((haiku.cost.unwrap().total() - 0.03).abs() < 1e-9);
        assert_eq!(
            v.hours
                .iter()
                .find(|h| h.model == "mystery-1")
                .unwrap()
                .cost,
            None
        );
        // Deleting the thread keeps what it spent.
        store.delete_thread("t1").unwrap();
        assert_eq!(view(&store, &rates, None, NOW).unwrap().hours.len(), 2);
        assert_eq!(
            cursor(&store, "t1", "sb1"),
            Cursor {
                sandbox: "sb1".into(),
                ..Cursor::default()
            }
        );
    }

    /// Where a time zone is half or three quarters of an hour off UTC (India, Nepal, Chatham),
    /// a request just after local midnight is that day's, and one just before is the day
    /// before's, however close: as each device adds them up into its own days.
    #[test]
    fn a_request_counts_on_its_own_local_day_in_every_time_zone() {
        let rates = prices();
        let rec = |key: &str, at_ms: i64| Record {
            provider: "codex",
            at_ms,
            model: "gpt-x".into(),
            tokens: t(1, 0, 0, 0, 0),
            cache_write_1h: 0,
            speed: Speed::Standard,
            reported_usd: None,
            key: key.into(),
        };
        let min = 60_000;
        let day = (NOW / DAY_MS - 3) * DAY_MS;
        for offset in [
            5 * 60 + 30,
            5 * 60 + 45,
            12 * 60 + 45,
            -(3 * 60 + 30),
            -(9 * 60 + 30),
        ] {
            let offset_ms = offset * min;
            // Local midnight, in UTC.
            let midnight = day + DAY_MS - offset_ms;
            let store = Store::open_in_memory().unwrap();
            keep(
                &store,
                "t1",
                &[
                    rec("before", midnight - 5 * min),
                    rec("after", midnight + 5 * min),
                    rec("late", midnight + 14 * min),
                ],
                &Cursor::default(),
                &rates,
                NOW,
            )
            .unwrap();
            let v = view(&store, &rates, None, NOW).unwrap();
            let day_of = |ms: i64| (ms + offset_ms).div_euclid(DAY_MS);
            let today = day_of(NOW);
            let s = flotilla_model::spend::summarize(&v, today, day_of, None);
            let on = |d: i64| s.days.iter().find(|x| x.day == d).map_or(0, |x| x.tokens);
            let local = day_of(midnight);
            assert_eq!(
                (on(local - 1), on(local)),
                (1, 2),
                "UTC{:+}:{:02}",
                offset / 60,
                (offset % 60).abs()
            );
        }
    }
}
