//! The tools, and what they answer: plain text, short by default. Lists are a line a thread;
//! a thread reads as its messages with runs of tool calls folded into one line; the whole of a
//! message comes when asked for by its seq. Ids are shown by their first 8 characters, and any
//! unique start of one is taken.

use crate::Mcp;
use flotilla_model::util::{now_ms, one_line};
use flotilla_proto::history::page_of;
use flotilla_proto::{
    Activity, Catalog, Message, NewThread, Project, Reply, Request, Sent, Sidebar, ThreadDetail,
    ThreadState, ThreadSummary,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// How long a request may take to be answered: a start waits for its first prompt to go in.
const START_PATIENCE: Duration = Duration::from_secs(180);
const SEND_PATIENCE: Duration = Duration::from_secs(120);
const ASK_PATIENCE: Duration = Duration::from_secs(60);
/// `wait_for_turn`'s default and longest wait.
const WAIT_DEFAULT: u64 = 45;
const WAIT_MAX: u64 = 600;
/// A turn this server just started that doesn't show as working yet is given this long to.
const GRACE: Duration = Duration::from_secs(20);
/// What's shown of a message's text, a reply in a list, and a question's options, at most.
const TEXT_CAP: usize = 1500;
const LINE_CAP: usize = 160;
/// The most of one message `read_thread seq=` gives, and of a turn's reply `wait_for_turn` does.
const WHOLE_CAP: usize = 40_000;
const REPLY_CAP: usize = 6_000;
/// `read_thread`'s default and most messages; `list_threads`' default and most threads.
const READ_DEFAULT: u64 = 30;
const READ_MAX: u64 = 200;
const LIST_DEFAULT: u64 = 20;
const LIST_MAX: u64 = 100;
/// Each harness's models `list_models` names when it lists them all.
const MODELS_SHOWN: usize = 24;

/// What a tool call can use while it runs.
pub(crate) struct Call<'a> {
    /// Progress for the client (a number that grows, and what's happening).
    pub progress: &'a dyn Fn(u64, &str),
    /// The client gave up on it.
    pub cancelled: &'a dyn Fn() -> bool,
    /// The longest `wait_for_turn` may wait, in seconds, whatever it's asked.
    pub wait_cap: Option<u64>,
}

#[derive(Debug)]
pub(crate) enum Failed {
    Unknown,
    /// Why, for the model.
    Tool(String),
}

impl From<String> for Failed {
    fn from(why: String) -> Failed {
        Failed::Tool(why)
    }
}

impl From<&str> for Failed {
    fn from(why: &str) -> Failed {
        Failed::Tool(why.to_string())
    }
}

type Answer = Result<String, Failed>;

/// The tools, as `tools/list` gives them.
pub(crate) fn definitions() -> Vec<Value> {
    let thread =
        json!({ "type": "string", "description": "The thread's id, or its first 8 characters" });
    let read = json!({ "readOnlyHint": true, "openWorldHint": false });
    vec![
        json!({
            "name": "list_threads",
            "title": "List threads",
            "description": "The threads on the Flotilla server, newest activity first: id, status (starting, working, idle, failed, archived, expired), title, model, project, pull request and the latest reply's first line. With query, finds threads by title and by what was said in them instead (every word, any case), with the matching line.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string", "description": "Words to find in titles and messages" },
                "limit": { "type": "integer", "minimum": 1, "maximum": LIST_MAX, "description": format!("At most this many (default {LIST_DEFAULT})") },
                "include_archived": { "type": "boolean", "description": "Also list archived and expired threads (default false)" }
            }},
            "annotations": read,
        }),
        json!({
            "name": "get_thread",
            "title": "Get a thread's status",
            "description": "One thread's status: what it's doing, its model and project, its branch and pull request (state, checks, link), its preview links, a question its agent is waiting on you to answer, and its last prompt and reply.",
            "inputSchema": { "type": "object", "properties": { "thread": thread }, "required": ["thread"] },
            "annotations": read,
        }),
        json!({
            "name": "read_thread",
            "title": "Read a thread",
            "description": "A thread's messages, newest last: your prompts, the agent's replies (long ones cut) and runs of tool calls folded into one line. Pages back with before. Pass seq for one message whole (a reply's full text, a tool call's input and output). Pass tools=true to see each tool call on a line of its own.",
            "inputSchema": { "type": "object", "properties": {
                "thread": thread,
                "before": { "type": "integer", "minimum": 1, "description": "Read the messages before this seq (the header says where older ones start)" },
                "limit": { "type": "integer", "minimum": 1, "maximum": READ_MAX, "description": format!("At most this many messages (default {READ_DEFAULT})") },
                "seq": { "type": "integer", "minimum": 0, "description": "Just this message, whole" },
                "tools": { "type": "boolean", "description": "A line for each tool call rather than a line for each run of them (default false)" }
            }, "required": ["thread"] },
            "annotations": read,
        }),
        json!({
            "name": "list_projects",
            "title": "List projects",
            "description": "The projects (GitHub repos) a thread can start in, for start_thread's project. A thread with no project starts in a blank VM.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read,
        }),
        json!({
            "name": "list_models",
            "title": "List models",
            "description": "The harnesses (claude-code, codex, opencode…) and the models each runs, for start_thread: every harness's model ids, or with provider, each of its models with its efforts and default.",
            "inputSchema": { "type": "object", "properties": {
                "provider": { "type": "string", "description": "Just this harness's models" }
            }},
            "annotations": read,
        }),
        json!({
            "name": "start_thread",
            "title": "Start a thread",
            "description": "Start a coding agent on a task in a new VM of its own, and send it the prompt. Returns the new thread's id at once: then call wait_for_turn for its answer. Each thread costs a VM start, so start one per task, not one per question.",
            "inputSchema": { "type": "object", "properties": {
                "prompt": { "type": "string", "description": "The task, as you'd write it to the agent" },
                "model": { "type": "string", "description": "A model's id from list_models" },
                "provider": { "type": "string", "description": "Its harness (claude-code, codex, opencode…). Default: the first list_models lists with that model" },
                "effort": { "type": "string", "description": "One of the model's efforts (default: the model's)" },
                "project": { "type": "string", "description": "A project from list_projects (owner/repo, or its id): the agent starts on a new branch of that repo. Leave out for a blank VM" },
                "title": { "type": "string", "description": "A title (default: one written from the prompt)" }
            }, "required": ["prompt", "model"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true },
        }),
        json!({
            "name": "send_message",
            "title": "Send a message",
            "description": "Send a follow-up to a thread's agent. While a turn runs it steers that turn; otherwise it starts the next one (waking the VM if it's stopped). Then call wait_for_turn.",
            "inputSchema": { "type": "object", "properties": {
                "thread": thread,
                "text": { "type": "string", "description": "What to say" }
            }, "required": ["thread", "text"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true },
        }),
        json!({
            "name": "wait_for_turn",
            "title": "Wait for a turn to end",
            "description": format!("Wait until the thread's agent finishes its turn, then give its answer (the turn's last reply, whole), a question it's waiting on, and its pull request. Gives up after timeout_seconds (default {WAIT_DEFAULT}, at most {WAIT_MAX}) and says it's still working: call it again. Set timeout_seconds below your MCP client's tool timeout."),
            "inputSchema": { "type": "object", "properties": {
                "thread": thread,
                "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": WAIT_MAX, "description": format!("How long to wait (default {WAIT_DEFAULT})") }
            }, "required": ["thread"] },
            "annotations": read,
        }),
        json!({
            "name": "answer_question",
            "title": "Answer the agent's question",
            "description": "Answer the questions a thread's agent is waiting on (get_thread and wait_for_turn show them, numbered, with their options). One answer per question, in order: an option's label, your own words, or for a question that takes several, a list. The answer goes to the agent as a message; then call wait_for_turn.",
            "inputSchema": { "type": "object", "properties": {
                "thread": thread,
                "answers": { "type": "array", "description": "One per question, in order", "items": { "anyOf": [ { "type": "string" }, { "type": "array", "items": { "type": "string" } } ] } }
            }, "required": ["thread", "answers"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true },
        }),
        json!({
            "name": "interrupt_turn",
            "title": "Interrupt the turn",
            "description": "Stop the turn a thread's agent is running now. The thread and its VM stay: send_message starts the next turn.",
            "inputSchema": { "type": "object", "properties": { "thread": thread }, "required": ["thread"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
        }),
        json!({
            "name": "archive_thread",
            "title": "Archive a thread",
            "description": "Put a finished thread away: it moves under Archived and its VM stops. Nothing is deleted, and it can be unarchived in the Flotilla app.",
            "inputSchema": { "type": "object", "properties": { "thread": thread }, "required": ["thread"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
        }),
        json!({
            "name": "open_preview",
            "title": "Open a preview link",
            "description": "A link to a port in the thread's VM (a dev server the agent started, say), to open in a browser: private, unless the thread's owner made it public. get_thread lists the links its services already have.",
            "inputSchema": { "type": "object", "properties": {
                "thread": thread,
                "port": { "type": "integer", "minimum": 1, "maximum": 65535 }
            }, "required": ["thread", "port"] },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true },
        }),
    ]
}

/// Run tool `name`.
pub(crate) fn call(mcp: &Mcp, name: &str, args: &Value, call: &Call) -> Answer {
    match name {
        "list_threads" => list_threads(mcp, args),
        "get_thread" => get_thread(mcp, args),
        "read_thread" => read_thread(mcp, args),
        "list_projects" => list_projects(mcp),
        "list_models" => list_models(mcp, args),
        "start_thread" => start_thread(mcp, args),
        "send_message" => send_message(mcp, args),
        "wait_for_turn" => wait_for_turn(mcp, args, call),
        "answer_question" => answer_question(mcp, args),
        "interrupt_turn" => act(mcp, args, Request::Interrupt, "Interrupted the turn"),
        "archive_thread" => act(mcp, args, Request::Archive, "Archived"),
        "open_preview" => open_preview(mcp, args),
        _ => Err(Failed::Unknown),
    }
}

// Arguments.

/// A string argument, trimmed; None if it's missing or empty.
fn text<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, Failed> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim()).filter(|s| !s.is_empty())),
        Some(_) => Err(format!("{key} is a string").into()),
    }
}

fn need<'a>(args: &'a Value, key: &str) -> Result<&'a str, Failed> {
    text(args, key)?.ok_or_else(|| format!("Say {key}").into())
}

/// A whole number argument (a number, or digits in a string), within `lo..=hi`.
fn number(args: &Value, key: &str, lo: u64, hi: u64) -> Result<Option<u64>, Failed> {
    let n = match args.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(n)) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                .map(|f| f as u64)
        }),
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(_) => None,
    };
    match n {
        Some(n) if (lo..=hi).contains(&n) => Ok(Some(n)),
        _ => Err(format!("{key} is a whole number from {lo} to {hi}").into()),
    }
}

fn flag(args: &Value, key: &str) -> Result<bool, Failed> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s == "true" || s == "false" => Ok(s == "true"),
        Some(_) => Err(format!("{key} is true or false").into()),
    }
}

// Threads.

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The thread `q` names: its id, or the only one whose id starts so.
fn find<'a>(sidebar: &'a Sidebar, q: &str) -> Result<&'a ThreadSummary, Failed> {
    let q = q.trim();
    if let Some(t) = sidebar.threads.iter().find(|t| t.thread.id == q) {
        return Ok(t);
    }
    let found: Vec<&ThreadSummary> = if q.len() >= 4 {
        sidebar
            .threads
            .iter()
            .filter(|t| t.thread.id.starts_with(q))
            .collect()
    } else {
        vec![]
    };
    match found[..] {
        [one] => Ok(one),
        [] => Err(format!("No thread {q:?}: list_threads gives their ids").into()),
        _ => Err(format!(
            "{q:?} is the start of {} threads' ids: give more of it",
            found.len()
        )
        .into()),
    }
}

/// What a thread is doing, in a word.
fn status(s: &ThreadSummary) -> &'static str {
    let t = &s.thread;
    if s.start_failed || t.state == ThreadState::Error {
        return "failed";
    }
    match s.activity {
        Activity::Archived => "archived",
        Activity::Expired => "expired",
        _ if matches!(t.state, ThreadState::Creating | ThreadState::Starting) => "starting",
        Activity::Working => "working",
        Activity::Unread | Activity::Idle => "idle",
    }
}

/// Starting, or with a turn running.
fn busy(s: &ThreadSummary) -> bool {
    !s.start_failed
        && (s.activity == Activity::Working
            || matches!(
                s.thread.state,
                ThreadState::Creating | ThreadState::Starting
            ))
}

fn ago(ms: i64) -> String {
    let secs = ((now_ms() - ms) / 1000).max(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86_400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// `s`, cut to `max` characters with how much more there is.
fn cut(s: &str, max: usize, more: &str) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}… [{} more characters{more}]", n - max)
}

fn line(s: &str, max: usize) -> String {
    let one = one_line(s);
    if one.chars().count() <= max {
        one
    } else {
        format!("{}…", one.chars().take(max).collect::<String>())
    }
}

fn project_name(sidebar: &Sidebar, id: Option<&str>) -> Option<String> {
    let id = id?;
    Some(
        sidebar
            .projects
            .iter()
            .find(|p| p.id == id)
            .map(Project::full_name)
            .unwrap_or_else(|| id.to_string()),
    )
}

fn model(s: &ThreadSummary) -> String {
    let t = &s.thread;
    let mut m = format!("{} {}", t.provider, t.model);
    if let Some(e) = t.effort.as_deref().filter(|e| !e.is_empty()) {
        m.push_str(&format!(" ({e})"));
    }
    m
}

fn pr_line(pr: &flotilla_proto::PullRequest) -> String {
    let state = match pr.state.as_str() {
        "OPEN" if pr.is_draft => "draft".to_string(),
        s => s.to_lowercase(),
    };
    let mut out = format!("PR #{} {state}", pr.number);
    let (pass, fail, pending) = (
        pr.checks.iter().filter(|c| c.state == "pass").count(),
        pr.failing(),
        pr.pending(),
    );
    if !pr.checks.is_empty() {
        let mut parts = vec![];
        if pass > 0 {
            parts.push(format!("{pass} passed"));
        }
        if fail > 0 {
            parts.push(format!("{fail} failing"));
        }
        if pending > 0 {
            parts.push(format!("{pending} running"));
        }
        out.push_str(&format!(", checks {}", parts.join(", ")));
    }
    match pr.review.as_deref() {
        Some("APPROVED") => out.push_str(", approved"),
        Some("CHANGES_REQUESTED") => out.push_str(", changes requested"),
        _ => {}
    }
    if pr.mergeable.as_deref() == Some("CONFLICTING") {
        out.push_str(", conflicts");
    }
    out
}

/// A thread's line in a list.
fn row(sidebar: &Sidebar, s: &ThreadSummary) -> String {
    let t = &s.thread;
    let mut out = format!(
        "{}  {:<8}  {}  · {}",
        short(&t.id),
        status(s),
        line(&t.title, 80),
        model(s)
    );
    if let Some(p) = project_name(sidebar, t.repo_project_id.as_deref()) {
        out.push_str(&format!(" · {p}"));
    }
    if let Some((pr, _)) = &s.pr {
        out.push_str(&format!(" · {}", pr_line(pr)));
    }
    out.push_str(&format!(" · {}", ago(s.last_activity_ms)));
    if s.activity == Activity::Unread {
        out.push_str(" · unread");
    }
    if let Some(r) = s.last_reply.as_deref().filter(|r| !r.trim().is_empty()) {
        out.push_str(&format!("\n          {}", line(r, LINE_CAP)));
    }
    out
}

fn list_threads(mcp: &Mcp, args: &Value) -> Answer {
    let limit = number(args, "limit", 1, LIST_MAX)?.unwrap_or(LIST_DEFAULT) as usize;
    let archived = flag(args, "include_archived")?;
    let sidebar = mcp.link().sidebar()?;
    let shown = |s: &ThreadSummary| {
        archived || !matches!(s.activity, Activity::Archived | Activity::Expired)
    };
    if let Some(query) = text(args, "query")? {
        let hits = match mcp.link().ask(
            Request::Search {
                query: query.to_string(),
                limit: Some(limit as u32),
            },
            ASK_PATIENCE,
        )? {
            Reply::Hits(h) => h,
            other => return Err(format!("The server answered {other:?}").into()),
        };
        let mut out = vec![];
        for h in &hits {
            let Some(s) = sidebar.threads.iter().find(|t| t.thread.id == h.thread) else {
                continue;
            };
            if !shown(s) {
                continue;
            }
            let at = h.seq.map(|q| format!(" [seq {q}]")).unwrap_or_default();
            out.push(format!(
                "{}  {:<8}  {}{at}\n          {}",
                short(&h.thread),
                status(s),
                line(&h.title, 80),
                line(&h.snippet, LINE_CAP)
            ));
        }
        if out.is_empty() {
            return Ok(format!(
                "Nothing matches {query:?}{}",
                if archived {
                    ""
                } else {
                    " (archived threads left out)"
                }
            ));
        }
        return Ok(out.join("\n"));
    }
    let all: Vec<&ThreadSummary> = sidebar.threads.iter().filter(|s| shown(s)).collect();
    if all.is_empty() {
        return Ok("No threads yet: start_thread starts one".into());
    }
    let mut out: Vec<String> = all.iter().take(limit).map(|s| row(&sidebar, s)).collect();
    if all.len() > limit {
        out.push(format!(
            "… and {} more (raise limit, or search with query)",
            all.len() - limit
        ));
    }
    Ok(out.join("\n"))
}

/// The ask message a thread's agent waits on an answer to, if one.
fn waiting(detail: &ThreadDetail) -> Option<&Message> {
    detail.messages.iter().rev().find(|m| {
        m.kind == flotilla_proto::ask::KIND
            && m.status.as_deref() == Some(flotilla_proto::ask::WAITING)
    })
}

fn questions_text(m: &Message) -> String {
    let qs = flotilla_proto::ask::questions(m);
    let mut out = vec![];
    for (i, q) in qs.iter().enumerate() {
        let mut l = format!("  {}. {}", i + 1, line(&q.question, 400));
        if !q.options.is_empty() {
            let opts: Vec<String> = q.options.iter().map(|o| line(&o.label, 120)).collect();
            l.push_str(&format!(
                "\n     options: {}{}",
                opts.join(" | "),
                if q.multi {
                    " (several may be picked)"
                } else {
                    ""
                }
            ));
        }
        out.push(l);
    }
    out.join("\n")
}

/// A thread's status card: `get_thread`, and the end of `wait_for_turn`.
fn card(sidebar: &Sidebar, s: &ThreadSummary, detail: &ThreadDetail) -> String {
    let t = &s.thread;
    let mut out = vec![
        format!("{} ({})", t.title, t.id),
        format!(
            "status: {}{}",
            status(s),
            if s.activity == Activity::Unread {
                ", unread"
            } else {
                ""
            }
        ),
    ];
    if let Some(why) = detail.start_error.as_deref() {
        out.push(format!("start failed: {}", line(why, 400)));
    }
    out.push(format!("model: {}", model(s)));
    match (
        &detail.project,
        project_name(sidebar, t.repo_project_id.as_deref()),
    ) {
        (Some(p), _) => out.push(format!(
            "project: {} on branch {} (into {})",
            p.project.full_name(),
            t.branch.as_deref().unwrap_or("?"),
            p.into
        )),
        (None, Some(p)) => out.push(format!("project: {p}")),
        (None, None) => out.push("project: none (a blank VM)".into()),
    }
    if let Some((pr, _)) = &s.pr {
        out.push(format!(
            "{}: {} {}",
            pr_line(pr),
            line(&pr.title, 100),
            pr.url
        ));
    }
    if let Some(services) = &detail.services {
        for r in &services.services {
            let url = r
                .links
                .first()
                .map(|l| l.url.clone())
                .or_else(|| r.preview.clone());
            out.push(format!(
                "preview: {} :{} {}{}",
                r.name,
                r.port,
                r.state,
                url.map(|u| format!(" {u}")).unwrap_or_default()
            ));
        }
    }
    if let Some(m) = waiting(detail) {
        out.push(format!(
            "waiting on your answer (answer_question):\n{}",
            questions_text(m)
        ));
    }
    if let Some(p) = s.last_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        out.push(format!("last prompt: {}", line(p, LINE_CAP)));
    }
    if let Some(r) = s.last_reply.as_deref().filter(|r| !r.trim().is_empty()) {
        out.push(format!("last reply: {}", line(r, LINE_CAP * 2)));
    }
    out.push(format!(
        "messages: {} (read_thread reads them)",
        count(detail)
    ));
    out.join("\n")
}

/// How many messages a thread has.
fn count(d: &ThreadDetail) -> i64 {
    d.messages
        .last()
        .map(|m| m.seq + 1)
        .unwrap_or(d.older)
        .max(d.older)
}

fn get_thread(mcp: &Mcp, args: &Value) -> Answer {
    let sidebar = mcp.link().sidebar()?;
    let s = find(&sidebar, need(args, "thread")?)?;
    let held = mcp.link().hold(&s.thread.id);
    let detail = held.detail()?;
    Ok(card(&sidebar, s, &detail))
}

/// The messages with seqs in `lo..hi`: from the view, and older pages over HTTP.
fn messages(
    mcp: &Mcp,
    id: &str,
    detail: &ThreadDetail,
    lo: i64,
    hi: i64,
) -> Result<Vec<Message>, Failed> {
    let mut out: Vec<Message> = vec![];
    let first_held = detail
        .messages
        .first()
        .map(|m| m.seq)
        .unwrap_or(detail.older);
    if lo < first_held {
        for page in page_of(lo)..=page_of((hi.min(first_held) - 1).max(lo)) {
            out.extend(mcp.link().history(id, page)?.messages);
        }
    }
    out.extend(detail.messages.iter().cloned());
    out.retain(|m| m.seq >= lo && m.seq < hi);
    out.sort_by_key(|m| m.seq);
    out.dedup_by_key(|m| m.seq);
    Ok(out)
}

fn who(m: &Message) -> &'static str {
    match m.kind.as_str() {
        "prompt" | "steer" => "you",
        "text" => "agent",
        "notice" => "notice",
        k if k == flotilla_proto::ask::KIND => "question",
        _ => "tool",
    }
}

/// A tool call on one line.
fn tool_line(m: &Message) -> String {
    let name = m.tool_name.as_deref().unwrap_or("tool");
    let what = m
        .tool_summary
        .as_deref()
        .map(|s| line(s, 120))
        .unwrap_or_default();
    let mark = if m.is_error {
        " (error)"
    } else if m.pending {
        " (running)"
    } else {
        ""
    };
    format!(
        "[{}] tool {name}{}{mark}",
        m.seq,
        if what.is_empty() {
            String::new()
        } else {
            format!(": {what}")
        }
    )
}

/// One message (not a tool call) on its lines.
fn message_text(m: &Message) -> String {
    let body = m.text.as_deref().unwrap_or("").trim();
    match who(m) {
        "question" => {
            let state = m.status.as_deref().unwrap_or("waiting");
            let mut out = format!("[{}] question ({state}):\n{}", m.seq, questions_text(m));
            if let Some(a) = flotilla_proto::ask::answers(m) {
                out.push_str(&format!(
                    "\n  answered: {}",
                    a.iter()
                        .map(|x| x.join(", "))
                        .collect::<Vec<_>>()
                        .join(" / ")
                ));
            }
            out
        }
        "notice" => format!(
            "[{}] notice: {}",
            m.seq,
            line(
                if body.is_empty() {
                    m.status.as_deref().unwrap_or("")
                } else {
                    body
                },
                LINE_CAP
            )
        ),
        w => {
            let files = if m.attachments.is_empty() {
                String::new()
            } else {
                format!(" [files: {}]", m.attachments.join(", "))
            };
            format!(
                "[{}] {w}: {}{files}",
                m.seq,
                cut(
                    body,
                    TEXT_CAP,
                    &format!("; read_thread seq={} for all of it", m.seq)
                )
            )
        }
    }
}

/// Messages as read: a run of tool calls on one line unless `each`.
fn render(ms: &[Message], each: bool) -> Vec<String> {
    let mut out = vec![];
    let mut i = 0;
    while i < ms.len() {
        if who(&ms[i]) != "tool" {
            out.push(message_text(&ms[i]));
            i += 1;
            continue;
        }
        let run_end = ms[i..]
            .iter()
            .position(|m| who(m) != "tool")
            .map_or(ms.len(), |n| i + n);
        let run = &ms[i..run_end];
        if each || run.len() == 1 {
            out.extend(run.iter().map(tool_line));
        } else {
            let mut names: Vec<(String, usize)> = vec![];
            for m in run {
                let n = m.tool_name.clone().unwrap_or_else(|| "tool".into());
                match names.iter_mut().find(|(k, _)| *k == n) {
                    Some((_, c)) => *c += 1,
                    None => names.push((n, 1)),
                }
            }
            let names: Vec<String> = names
                .into_iter()
                .map(|(n, c)| if c > 1 { format!("{n} ×{c}") } else { n })
                .collect();
            let errors = run.iter().filter(|m| m.is_error).count();
            let last = run
                .last()
                .and_then(|m| m.tool_summary.as_deref())
                .map(|s| format!("; last: {}", line(s, 80)))
                .unwrap_or_default();
            out.push(format!(
                "[{}–{}] {} tool calls: {}{}{last}",
                run[0].seq,
                run[run.len() - 1].seq,
                run.len(),
                names.join(", "),
                if errors > 0 {
                    format!(" ({errors} failed)")
                } else {
                    String::new()
                }
            ));
        }
        i = run_end;
    }
    out
}

fn read_thread(mcp: &Mcp, args: &Value) -> Answer {
    let sidebar = mcp.link().sidebar()?;
    let s = find(&sidebar, need(args, "thread")?)?;
    let id = s.thread.id.clone();
    let held = mcp.link().hold(&id);
    let detail = held.detail()?;
    let total = count(&detail);
    if let Some(seq) = number(args, "seq", 0, u32::MAX as u64)? {
        let seq = seq as i64;
        let m = messages(mcp, &id, &detail, seq, seq + 1)?
            .pop()
            .ok_or_else(|| {
                format!(
                    "Thread {} has no message {seq} (it has {total})",
                    short(&id)
                )
            })?;
        return Ok(whole(mcp, &id, m));
    }
    let limit = number(args, "limit", 1, READ_MAX)?.unwrap_or(READ_DEFAULT) as i64;
    let hi = number(args, "before", 1, u32::MAX as u64)?.map_or(total, |b| (b as i64).min(total));
    let lo = (hi - limit).max(0);
    let ms = messages(mcp, &id, &detail, lo, hi)?;
    let mut out = vec![format!(
        "{} ({}) · {} · {}",
        s.thread.title,
        short(&id),
        status(s),
        model(s)
    )];
    out.push(if total == 0 {
        "No messages yet".into()
    } else if lo > 0 {
        format!(
            "messages {lo}–{} of {total}; older: read_thread before={lo}",
            hi - 1
        )
    } else {
        format!("messages 0–{} of {total}", hi - 1)
    });
    out.push(String::new());
    out.extend(render(&ms, flag(args, "tools")?));
    Ok(out.join("\n"))
}

/// One message whole: its text, or a tool call's input and output (fetched whole if the view
/// cut them), each up to `WHOLE_CAP`.
fn whole(mcp: &Mcp, id: &str, mut m: Message) -> String {
    for c in std::mem::take(&mut m.cut) {
        if let Ok(s) = mcp.link().full(id, m.seq, &c) {
            match c.field.as_str() {
                "tool_input_json" => m.tool_input_json = Some(s),
                _ => m.tool_result_json = Some(s),
            }
        }
    }
    let mut out = vec![format!("[{}] {} ({})", m.seq, who(&m), m.kind)];
    if let Some(t) = m.text.as_deref().filter(|t| !t.trim().is_empty()) {
        out.push(cut(t, WHOLE_CAP, ""));
    }
    if who(&m) == "question" {
        out.push(questions_text(&m));
    }
    if let Some(n) = &m.tool_name {
        out.push(format!(
            "tool: {n}{}",
            if m.is_error { " (error)" } else { "" }
        ));
        if let Some(s) = &m.tool_summary {
            out.push(format!("summary: {s}"));
        }
    }
    if let Some(i) = m.tool_input_json.as_deref() {
        out.push(format!("input:\n{}", cut(i, WHOLE_CAP, "")));
    }
    if let Some(r) = m.tool_result_json.as_deref() {
        out.push(format!("output:\n{}", cut(r, WHOLE_CAP, "")));
    }
    out.join("\n")
}

fn list_projects(mcp: &Mcp) -> Answer {
    let sidebar = mcp.link().sidebar()?;
    let env = sidebar
        .environments
        .iter()
        .find(|e| e.is_default)
        .map(|e| e.name.clone());
    let mut out = vec![match env {
        Some(e) => format!("(no project): a blank VM ({e}). Leave project out of start_thread."),
        None => "(no project): unavailable, the server has no default environment".into(),
    }];
    for p in &sidebar.projects {
        out.push(format!(
            "{}  id {} · default branch {}",
            p.full_name(),
            p.id,
            p.default_branch
        ));
    }
    if sidebar.projects.is_empty() {
        out.push("No projects: they're added in the Flotilla app".into());
    }
    Ok(out.join("\n"))
}

fn catalog(mcp: &Mcp) -> Result<Catalog, Failed> {
    mcp.link().catalog()?.ok_or_else(|| {
        "The Flotilla server hasn't read the models yet: try again in a moment".into()
    })
}

fn list_models(mcp: &Mcp, args: &Value) -> Answer {
    let catalog = catalog(mcp)?;
    let only = text(args, "provider")?;
    let mut providers: Vec<&flotilla_proto::Provider> = catalog
        .providers
        .iter()
        .filter(|p| only.is_none_or(|o| provider_is(p, o)))
        .collect();
    providers.sort_by_key(|p| p.order);
    if providers.is_empty() {
        return Err(format!(
            "No harness {:?}: {}",
            only.unwrap_or(""),
            catalog
                .providers
                .iter()
                .map(|p| p.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }
    let mut out = vec![];
    // Every harness: its models' ids, the first few. One harness: each model with its efforts.
    if only.is_none() {
        for p in providers {
            let ids: Vec<&str> = p.models.iter().map(|m| m.id.as_str()).collect();
            let more = if ids.len() > MODELS_SHOWN {
                format!(
                    ", … {} more (list_models provider={})",
                    ids.len() - MODELS_SHOWN,
                    p.key
                )
            } else {
                String::new()
            };
            let default = p
                .default_model
                .as_deref()
                .map(|d| format!(" (default {d})"))
                .unwrap_or_default();
            out.push(format!(
                "{}{default}: {}{more}",
                p.key,
                ids[..ids.len().min(MODELS_SHOWN)].join(", ")
            ));
        }
        out.push("A model several harnesses run starts on the first listed here unless provider says. list_models provider=… gives each model's efforts.".into());
        return Ok(out.join("\n"));
    }
    for p in providers {
        out.push(format!(
            "{}{}",
            p.key,
            p.default_model
                .as_deref()
                .map(|d| format!(" (default {d})"))
                .unwrap_or_default()
        ));
        for m in &p.models {
            let mut l = format!("  {}", m.id);
            if let Some(label) = m.label.as_deref().filter(|l| *l != m.id) {
                l.push_str(&format!("  {label}"));
            }
            if let Some(e) = &m.effort {
                l.push_str(&format!(
                    "  effort: {}{}",
                    e.supported.join("|"),
                    e.default
                        .as_deref()
                        .map(|d| format!(" (default {d})"))
                        .unwrap_or_default()
                ));
            }
            out.push(l);
        }
    }
    Ok(out.join("\n"))
}

fn provider_is(p: &flotilla_proto::Provider, name: &str) -> bool {
    p.key.eq_ignore_ascii_case(name)
        || p.cli_name
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(name))
        || p.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
}

/// The harness and model `model` (and `provider`, if given) name in the catalog.
fn pick<'a>(
    catalog: &'a Catalog,
    provider: Option<&str>,
    model: &str,
) -> Result<(&'a flotilla_proto::Provider, &'a flotilla_proto::Model), Failed> {
    let is = |m: &flotilla_proto::Model| {
        m.id.eq_ignore_ascii_case(model)
            || m.sdk_id
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(model))
    };
    let mut found: Vec<(&flotilla_proto::Provider, &flotilla_proto::Model)> = catalog
        .providers
        .iter()
        .filter(|p| provider.is_none_or(|n| provider_is(p, n)))
        .flat_map(|p| p.models.iter().filter(|m| is(m)).map(move |m| (p, m)))
        .collect();
    // Several harnesses run the same model: the one boat lists first (Claude's models on
    // claude-code, OpenAI's on codex), unless provider says.
    found.sort_by_key(|(p, _)| p.order);
    found.first().copied().ok_or_else(|| {
        format!(
            "No model {model:?}{}: list_models gives their ids",
            provider.map(|p| format!(" for {p}")).unwrap_or_default()
        )
        .into()
    })
}

fn pick_project<'a>(sidebar: &'a Sidebar, q: &str) -> Result<&'a Project, Failed> {
    let q = q
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_end_matches('/');
    let found: Vec<&Project> = sidebar
        .projects
        .iter()
        .filter(|p| p.id == q || p.full_name().eq_ignore_ascii_case(q))
        .collect();
    let found = if found.is_empty() {
        sidebar
            .projects
            .iter()
            .filter(|p| p.repo.eq_ignore_ascii_case(q))
            .collect()
    } else {
        found
    };
    match found[..] {
        [one] => Ok(one),
        [] => Err(format!("No project {q:?}: list_projects gives them").into()),
        _ => Err(format!(
            "{q:?} could be {}: give owner/repo",
            found
                .iter()
                .map(|p| p.full_name())
                .collect::<Vec<_>>()
                .join(" or ")
        )
        .into()),
    }
}

fn start_thread(mcp: &Mcp, args: &Value) -> Answer {
    let prompt = need(args, "prompt")?;
    let catalog = catalog(mcp)?;
    let (provider, model) = pick(&catalog, text(args, "provider")?, need(args, "model")?)?;
    let effort = match (text(args, "effort")?, &model.effort) {
        (None, _) => None,
        (Some(e), Some(o)) if o.supported.iter().any(|s| s.eq_ignore_ascii_case(e)) => {
            Some(e.to_lowercase())
        }
        (Some(e), Some(o)) => {
            return Err(format!(
                "{} takes effort {}, not {e:?}",
                model.id,
                o.supported.join(", ")
            )
            .into());
        }
        (Some(_), None) => {
            return Err(format!("{} has no effort setting: leave effort out", model.id).into());
        }
    };
    let sidebar = mcp.link().sidebar()?;
    let (environment, project) = match text(args, "project")? {
        Some(q) => {
            let p = pick_project(&sidebar, q)?;
            (p.environment_name.clone(), Some(p))
        }
        None => (
            sidebar
                .environments
                .iter()
                .find(|e| e.is_default)
                .map(|e| e.name.clone())
                .ok_or(
                    "The Flotilla server has no default environment for a thread with no project",
                )?,
            None,
        ),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let req = NewThread {
        environment,
        project_id: project.map(|p| p.id.clone()),
        provider: provider.key.clone(),
        model: model.id.clone(),
        effort,
        fast: false,
        title: text(args, "title")?.map(str::to_string),
        machine_type: None,
        prompt: prompt.to_string(),
        attachments: vec![],
        wait_ready: false,
        on_branch: None,
        id: Some(id.clone()),
        parent: None,
        branch_tag: None,
    };
    mcp.sent
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.clone(), (Instant::now(), now_ms()));
    let where_ = project
        .map(|p| p.full_name())
        .unwrap_or_else(|| "no project".into());
    let started = format!(
        "Started thread {} on {} {} ({where_})",
        short(&id),
        provider.key,
        model.id
    );
    match mcp
        .link()
        .ask(Request::NewThread(Box::new(req)), START_PATIENCE)
    {
        Ok(_) => Ok(format!(
            "{started}. Its agent is at work: call wait_for_turn with thread {} for the answer.",
            short(&id)
        )),
        // Still on its way: the thread is there, and its id is ours.
        Err(e)
            if e.contains("in time")
                && mcp
                    .link()
                    .sidebar()
                    .is_ok_and(|s| s.threads.iter().any(|t| t.thread.id == id)) =>
        {
            Ok(format!(
                "{started}; it's still starting. Call wait_for_turn with thread {}.",
                short(&id)
            ))
        }
        Err(e) => Err(format!("Couldn't start the thread: {e}").into()),
    }
}

fn local() -> String {
    format!("local-{}", uuid::Uuid::new_v4())
}

fn send_message(mcp: &Mcp, args: &Value) -> Answer {
    let sidebar = mcp.link().sidebar()?;
    let s = find(&sidebar, need(args, "thread")?)?;
    let text = need(args, "text")?;
    let id = s.thread.id.clone();
    mcp.sent
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.clone(), (Instant::now(), now_ms()));
    match mcp.link().ask(
        Request::Send {
            thread: id.clone(),
            text: text.to_string(),
            files: vec![],
            local: local(),
        },
        SEND_PATIENCE,
    ) {
        Ok(Reply::Sent(Sent::Steer(_))) => Ok(format!(
            "Steered the turn running in {}. Call wait_for_turn for its answer.",
            short(&id)
        )),
        Ok(_) => Ok(format!(
            "Sent to {}: its agent is at work. Call wait_for_turn for the answer.",
            short(&id)
        )),
        Err(e) if e.contains("in time") => Ok(format!(
            "Sending to {} (its VM is waking up). Call wait_for_turn for the answer.",
            short(&id)
        )),
        Err(e) => Err(format!("Couldn't send: {e}").into()),
    }
}

/// The turn's last reply: the agent's last words after the last prompt.
fn last_reply(detail: &ThreadDetail) -> Option<&Message> {
    let from = detail
        .messages
        .iter()
        .rposition(|m| matches!(m.kind.as_str(), "prompt" | "steer"))
        .map_or(0, |i| i + 1);
    detail.messages[from..]
        .iter()
        .rev()
        .find(|m| m.kind == "text" && m.text.as_deref().is_some_and(|t| !t.trim().is_empty()))
}

fn wait_for_turn(mcp: &Mcp, args: &Value, call: &Call) -> Answer {
    let asked = number(args, "timeout_seconds", 1, WAIT_MAX)?.unwrap_or(WAIT_DEFAULT);
    let wait = Duration::from_secs(call.wait_cap.map_or(asked, |cap| asked.min(cap)));
    let started = Instant::now();
    let id = find(&mcp.link().sidebar()?, need(args, "thread")?)?
        .thread
        .id
        .clone();
    let sent = mcp
        .sent
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .copied();
    let mut saw_busy = false;
    let mut told = 0;
    let settled = loop {
        let sidebar = mcp.link().sidebar()?;
        let Some(s) = sidebar.threads.iter().find(|t| t.thread.id == id) else {
            return Err(format!("Thread {} is gone", short(&id)).into());
        };
        let now_busy = busy(s);
        saw_busy |= now_busy;
        // Just sent, not at work yet and nothing since: give it a moment to show.
        let early = sent.is_some_and(|(at, ms)| at.elapsed() < GRACE && s.last_activity_ms < ms)
            && !saw_busy;
        if !now_busy && !early {
            break true;
        }
        if started.elapsed() >= wait || (call.cancelled)() {
            break false;
        }
        let secs = started.elapsed().as_secs();
        if secs / 10 > told {
            told = secs / 10;
            (call.progress)(secs, &format!("{} is {} ({secs}s)", short(&id), status(s)));
        }
        mcp.link().settle(
            Duration::from_millis(1000)
                .min(wait.saturating_sub(started.elapsed()))
                .max(Duration::from_millis(50)),
        );
    };
    let sidebar = mcp.link().sidebar()?;
    let s = find(&sidebar, &id)?;
    let held = mcp.link().hold(&id);
    let detail = held.detail()?;
    let secs = started.elapsed().as_secs();
    let mut out = vec![];
    if !settled {
        let latest = detail
            .messages
            .iter()
            .rev()
            .find(|m| who(m) == "tool")
            .map(|m| format!(" Latest: {}.", tool_line(m)))
            .unwrap_or_default();
        out.push(format!(
            "{} is still {} after {secs}s.{latest} Call wait_for_turn again.",
            short(&id),
            status(s)
        ));
        return Ok(out.join("\n"));
    }
    out.push(format!(
        "{}'s turn has ended: {}{}.",
        short(&id),
        status(s),
        detail
            .start_error
            .as_deref()
            .map(|e| format!(" ({})", line(e, 300)))
            .unwrap_or_default()
    ));
    if let Some(r) = last_reply(&detail) {
        out.push(format!(
            "Reply [seq {}]:\n{}",
            r.seq,
            cut(
                r.text.as_deref().unwrap_or("").trim(),
                REPLY_CAP,
                &format!("; read_thread seq={} for all of it", r.seq)
            )
        ));
    }
    if let Some(m) = waiting(&detail) {
        out.push(format!(
            "Its agent is waiting on your answer (answer_question):\n{}",
            questions_text(m)
        ));
    }
    if let Some((pr, _)) = &s.pr {
        out.push(format!("{}: {}", pr_line(pr), pr.url));
    }
    Ok(out.join("\n"))
}

fn answer_question(mcp: &Mcp, args: &Value) -> Answer {
    let sidebar = mcp.link().sidebar()?;
    let s = find(&sidebar, need(args, "thread")?)?;
    let id = s.thread.id.clone();
    let given: Vec<Vec<String>> = match args.get("answers") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => Ok(vec![s.clone()]),
                Value::Array(xs) => xs
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or("Each answer is a string, or a list of strings")
                    })
                    .collect(),
                _ => Err("Each answer is a string, or a list of strings"),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err("Say answers: one per question, in order".into()),
    };
    let held = mcp.link().hold(&id);
    let detail = held.detail()?;
    let ask = waiting(&detail)
        .ok_or_else(|| format!("{}'s agent isn't waiting on an answer", short(&id)))?;
    let questions = flotilla_proto::ask::questions(ask);
    let answers = flotilla_proto::ask::tidy(&questions, &given).map_err(|e| {
        format!(
            "{e}: it asked {} question{}",
            questions.len(),
            if questions.len() == 1 { "" } else { "s" }
        )
    })?;
    let tool = ask
        .tool_id
        .clone()
        .ok_or("That question can't be answered here")?;
    drop(held);
    mcp.sent
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.clone(), (Instant::now(), now_ms()));
    mcp.link()
        .ask(
            Request::Answer {
                thread: id.clone(),
                ask: tool,
                answers,
                local: local(),
            },
            SEND_PATIENCE,
        )
        .map_err(|e| format!("Couldn't send the answer: {e}"))?;
    Ok(format!(
        "Answered {}'s question. Call wait_for_turn for what it does next.",
        short(&id)
    ))
}

fn act(mcp: &Mcp, args: &Value, req: fn(String) -> Request, done: &str) -> Answer {
    let sidebar = mcp.link().sidebar()?;
    let s = find(&sidebar, need(args, "thread")?)?;
    let id = s.thread.id.clone();
    let r = req(id.clone());
    let verb = r.verb();
    mcp.link()
        .ask(r, SEND_PATIENCE)
        .map_err(|e| format!("Couldn't {verb}: {e}"))?;
    Ok(format!(
        "{done}: {} ({})",
        short(&id),
        line(&s.thread.title, 80)
    ))
}

fn open_preview(mcp: &Mcp, args: &Value) -> Answer {
    let sidebar = mcp.link().sidebar()?;
    let s = find(&sidebar, need(args, "thread")?)?;
    let port = number(args, "port", 1, 65535)?.ok_or("Say port")? as u16;
    match mcp
        .link()
        .ask(
            Request::Preview {
                thread: s.thread.id.clone(),
                port,
            },
            SEND_PATIENCE,
        )
        .map_err(|e| format!("Couldn't open the preview: {e}"))?
    {
        // Its owner may have made it public (only they can, in the app).
        Reply::Url(u) if !u.contains("_token=") => Ok(format!(
            "Port {port} in {}: {u}\nA public link: anyone with it can open it, until its owner makes it private.",
            short(&s.thread.id)
        )),
        Reply::Url(u) => Ok(format!(
            "Port {port} in {}: {u}\nA private link: share it only with whoever should see it.",
            short(&s.thread.id)
        )),
        other => Err(format!("The server answered {other:?}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(seq: i64, kind: &str, text: &str, tool: Option<&str>) -> Message {
        Message {
            seq,
            kind: kind.into(),
            text: (!text.is_empty()).then(|| text.into()),
            tool_name: tool.map(str::to_string),
            tool_summary: tool.map(|t| format!("ran {t}")),
            ..Default::default()
        }
    }

    #[test]
    fn runs_of_tool_calls_fold_into_one_line_unless_asked_for_each() {
        let ms = vec![
            msg(0, "prompt", "Fix the build", None),
            msg(1, "tool", "", Some("Bash")),
            msg(2, "tool", "", Some("Bash")),
            msg(3, "tool", "", Some("Edit")),
            msg(4, "text", "Fixed.", None),
            msg(5, "tool", "", Some("Read")),
        ];
        let folded = render(&ms, false);
        assert_eq!(
            folded,
            [
                "[0] you: Fix the build",
                "[1–3] 3 tool calls: Bash ×2, Edit; last: ran Edit",
                "[4] agent: Fixed.",
                "[5] tool Read: ran Read"
            ]
        );
        assert_eq!(render(&ms, true).len(), 6);
    }

    #[test]
    fn long_text_is_cut_and_says_how_to_read_it_whole() {
        let long = "word ".repeat(1000);
        let shown = message_text(&msg(7, "text", &long, None));
        assert!(shown.len() < TEXT_CAP + 200, "{}", shown.len());
        assert!(shown.contains("read_thread seq=7"), "{shown}");
        assert!(cut("short", 10, "").eq("short"));
    }

    #[test]
    fn arguments_are_checked_with_reasons() {
        let args =
            json!({"limit": "5", "n": 3.0, "bad": -1, "flag": "true", "s": "  x  ", "empty": " "});
        assert_eq!(number(&args, "limit", 1, 10).ok().flatten(), Some(5));
        assert_eq!(number(&args, "n", 1, 10).ok().flatten(), Some(3));
        assert!(number(&args, "bad", 1, 10).is_err());
        assert!(number(&args, "limit", 6, 10).is_err());
        assert!(flag(&args, "flag").unwrap());
        assert_eq!(text(&args, "s").ok().flatten(), Some("x"));
        assert_eq!(text(&args, "empty").ok().flatten(), None);
        assert!(need(&args, "missing").is_err());
        assert!(text(&json!({"s": 1}), "s").is_err());
    }

    #[test]
    fn every_tool_says_what_it_takes() {
        let defs = definitions();
        let names: Vec<&str> = defs.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "list_threads",
                "get_thread",
                "read_thread",
                "list_projects",
                "list_models",
                "start_thread",
                "send_message",
                "wait_for_turn",
                "answer_question",
                "interrupt_turn",
                "archive_thread",
                "open_preview"
            ]
        );
        for d in &defs {
            assert_eq!(d["inputSchema"]["type"], "object", "{d}");
            assert!(d["description"].as_str().unwrap().len() > 40, "{d}");
            for r in d["inputSchema"]["required"]
                .as_array()
                .into_iter()
                .flatten()
            {
                assert!(
                    d["inputSchema"]["properties"]
                        .get(r.as_str().unwrap())
                        .is_some(),
                    "{d}"
                );
            }
        }
    }
}
