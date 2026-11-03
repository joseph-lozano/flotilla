//! Agents handing work to other agents, or asking the user: the request lines the `flotilla`
//! command in a thread's VM prints, and the messages that carry a hand-off's task and answer,
//! written and read back (`flotilla_core::orchestrate` carries the requests out).

use flotilla_proto::Message;
use serde::Deserialize;
use serde_json::Value;

/// Longest child answer passed on to its parent.
const MAX_ANSWER: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    Delegate {
        id: String,
        #[serde(default)]
        agent: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        title: String,
        task: String,
    },
    /// Another agent in the same VM: another conversation there.
    Agent {
        id: String,
        #[serde(default)]
        agent: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        title: String,
        task: String,
    },
    Message {
        id: String,
        to: String,
        text: String,
    },
    Cancel {
        id: String,
        to: String,
    },
    /// A question for the user (`flotilla ask`): its card in the thread, answered as Codex's
    /// own ask tool is (`flotilla_core::asks`).
    Ask {
        id: String,
        questions: Vec<flotilla_proto::ask::Question>,
    },
}

impl Request {
    pub fn id(&self) -> &str {
        match self {
            Request::Delegate { id, .. }
            | Request::Agent { id, .. }
            | Request::Message { id, .. }
            | Request::Cancel { id, .. }
            | Request::Ask { id, .. } => id,
        }
    }
}

/// How long a request line's signature is: 64 lowercase hex digits, a tag that proves its id and
/// a seal over its JSON (see `flotilla_core::orchestrate::check`).
pub const SIG_LEN: usize = 64;

/// A request line seen in a thread's tool output, `FLOTILLA <id> <sig> <json>`: the request,
/// or (cut short on the way) only its id, to read from the VM's copy. Either carries the
/// signature the `flotilla` command in the thread's VM made (`None`: a line from before
/// requests were signed, which nothing carries out). Nothing here says the line is genuine:
/// any text that reaches a tool's output can look like one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// `json`: the request exactly as signed.
    Whole {
        request: Request,
        json: String,
        sig: Option<String>,
    },
    Cut {
        id: String,
        sig: Option<String>,
    },
}

impl Seen {
    pub fn id(&self) -> &str {
        match self {
            Seen::Whole { request, .. } => request.id(),
            Seen::Cut { id, .. } => id,
        }
    }

    pub fn sig(&self) -> Option<&str> {
        match self {
            Seen::Whole { sig, .. } | Seen::Cut { sig, .. } => sig.as_deref(),
        }
    }
}

/// The request lines in a thread's finished tool calls, in order.
pub fn requests(messages: &[Message]) -> Vec<Seen> {
    let mut out = Vec::new();
    for m in messages.iter().filter(|m| m.kind == "tool" && !m.pending) {
        let Some(result) = &m.tool_result_json else {
            continue;
        };
        let mut texts = Vec::new();
        match serde_json::from_str::<Value>(result) {
            Ok(v) => strings(&v, &mut texts),
            Err(_) => texts.push(result.clone()),
        }
        for text in texts {
            for line in text.lines() {
                if let Some(seen) = parse_line(line)
                    && !out.contains(&seen)
                {
                    out.push(seen);
                }
            }
        }
    }
    out
}

fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            // A tool's output is sometimes JSON inside a string.
            match serde_json::from_str::<Value>(s) {
                Ok(inner @ (Value::Object(_) | Value::Array(_))) => strings(&inner, out),
                _ => out.push(s.clone()),
            }
        }
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

fn is_sig(s: &str) -> bool {
    s.len() == SIG_LEN
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One line, as the command prints it or as its copy in the VM holds it.
pub fn parse_line(line: &str) -> Option<Seen> {
    let rest = line.trim_start().strip_prefix("FLOTILLA ")?;
    let (id, rest) = rest.split_once(' ')?;
    if id.len() != 36 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return None;
    }
    let (sig, json) = match rest.split_once(' ') {
        Some((sig, json)) if is_sig(sig) => (Some(sig.to_string()), json.trim()),
        _ => (None, rest.trim()),
    };
    match serde_json::from_str::<Request>(json) {
        Ok(r) if r.id() == id => Some(Seen::Whole {
            request: r,
            json: json.to_string(),
            sig,
        }),
        Ok(_) => None,
        // Only what starts as the command's JSON does is a request cut short.
        Err(_) if json.starts_with("{\"op\":") => Some(Seen::Cut {
            id: id.to_string(),
            sig,
        }),
        Err(_) => None,
    }
}

/// What Flotilla's own messages to an agent start with: a child's answer, a start that failed,
/// the agents' report, a request it couldn't read.
pub const MARK: &str = "[flotilla] ";

/// A prompt or steer is Flotilla's own, not yours: it starts with [`MARK`].
pub fn by_flotilla(text: &str) -> bool {
    text.starts_with(MARK)
}

/// What a child's first prompt ends with, after the task.
pub const CHILD_NOTE: &str =
    "\n\n(Another agent handed you this task; your final message goes back to it.)";

/// A child's first prompt: the task, and who its answer is for.
pub fn child_prompt(task: &str) -> String {
    format!("{}{CHILD_NOTE}", task.trim_end())
}

/// What a parent is sent when its child's turn ends (`how`: finished, was stopped, failed):
/// `answer` is the child's last reply, or `None` if the turn ended without one.
pub fn wake_text(
    child_id: &str,
    title: &str,
    branch: Option<&str>,
    answer: Option<&str>,
    how: &str,
) -> String {
    let on = branch.map(|b| format!(", branch {b}")).unwrap_or_default();
    match answer.map(str::trim).filter(|a| !a.is_empty()) {
        Some(a) => {
            let a = if a.len() > MAX_ANSWER {
                let cut = (0..=MAX_ANSWER)
                    .rev()
                    .find(|i| a.is_char_boundary(*i))
                    .unwrap_or(0);
                format!("{}\n\n[…cut short]", &a[..cut])
            } else {
                a.to_string()
            };
            format!("[flotilla] \"{title}\" ({child_id}{on}) {how}:\n\n{a}")
        }
        None => format!("[flotilla] \"{title}\" ({child_id}{on}) {how} without an answer."),
    }
}

/// A child's answer as its parent got it (`wake_text`), on one line or several: the child's
/// title, its id, how its turn ended, and the answer.
pub fn parse_answer(text: &str) -> Option<(&str, &str, &str, &str)> {
    let rest = text.strip_prefix("[flotilla] \"")?;
    let (title, rest) = rest.split_once("\" (")?;
    let (inside, rest) = rest.split_once(") ")?;
    let id = inside.split(',').next()?.trim();
    let (how, answer) = match rest.split_once(':') {
        Some((how, answer)) => (how, answer),
        None => (rest.trim_end_matches('.'), ""),
    };
    Some((title, id, how.trim(), answer.trim()))
}

/// What a thread is told when a thread it asked for couldn't start: the child's title, the id
/// it asked with, and why.
pub fn couldnt_start_text(title: &str, id: &str, error: &str) -> String {
    format!("[flotilla] Couldn't start \"{title}\" ({id}): {error}")
}

/// A [`couldnt_start_text`]: the title, the id and why.
pub fn parse_couldnt_start(text: &str) -> Option<(&str, &str, &str)> {
    let rest = text.strip_prefix("[flotilla] Couldn't start \"")?;
    let (title, rest) = rest.split_once("\" (")?;
    let (id, error) = rest.split_once("): ")?;
    Some((title, id, error.trim()))
}

/// What a thread is told when a request of its was cut short in boat's events and its copy in
/// the VM couldn't be read either.
pub fn unreadable_text(id: &str) -> String {
    format!(
        "[flotilla] Couldn't read your request {id}: it was cut short, and its copy in the VM couldn't be read. Make it again."
    )
}

/// An [`unreadable_text`]: the request's id.
pub fn parse_unreadable(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("[flotilla] Couldn't read your request ")?;
    let (id, _) = rest.split_once(": it was cut short")?;
    Some(id)
}

/// A child's task as its parent handed it, without the note for the agent.
pub fn task_of(text: &str) -> Option<&str> {
    text.strip_suffix(CHILD_NOTE).or_else(|| {
        text.strip_suffix(CHILD_NOTE.trim_start())
            .map(str::trim_end)
    })
}

/// The text a delegate's child thread is called: its title, or the task's first words.
pub fn child_title(title: &str, task: &str) -> String {
    let t = title.trim();
    if !t.is_empty() {
        return crate::util::ellipsize(t, 80);
    }
    let first = task
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Delegated task")
        .trim();
    crate::util::ellipsize(first, 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(result: &str) -> Message {
        Message {
            kind: "tool".into(),
            tool_result_json: Some(result.into()),
            ..Default::default()
        }
    }

    const ID: &str = "7c11d703-6d90-4aba-a93a-a86fd03a90f7";

    const SIG: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn finds_requests_in_any_harness_output() {
        let json = format!(
            r#"{{"op":"delegate","id":"{ID}","agent":"codex","model":"","title":"Add tests","task":"Write tests.\nReport."}}"#
        );
        let line = format!("FLOTILLA {ID} {SIG} {json}");
        // Claude Code: the output as a string; Codex: inside an object; OpenCode: JSON in a string.
        let claude =
            serde_json::to_string(&Value::String(format!("{line}\nDelegated as {ID}"))).unwrap();
        let codex = serde_json::json!({"output": format!("{line}\n"), "exit_code": 0}).to_string();
        let opencode = serde_json::to_string(&Value::String(
            serde_json::json!({"output": line}).to_string(),
        ))
        .unwrap();
        for r in [claude, codex, opencode] {
            let found = requests(&[tool(&r)]);
            assert_eq!(found.len(), 1, "{r}");
            let Seen::Whole {
                request:
                    Request::Delegate {
                        agent, title, task, ..
                    },
                json: signed,
                sig,
            } = &found[0]
            else {
                panic!("{found:?}")
            };
            assert_eq!(
                (agent.as_str(), title.as_str(), task.as_str()),
                ("codex", "Add tests", "Write tests.\nReport.")
            );
            // What's checked is the JSON exactly as printed, and the signature beside it.
            assert_eq!(
                (signed.as_str(), sig.as_deref()),
                (json.as_str(), Some(SIG))
            );
        }
        // The same request twice (the result and a later read of it) counts once.
        let both = requests(&[
            tool(&serde_json::to_string(&Value::String(line.clone())).unwrap()),
            tool(&serde_json::to_string(&Value::String(line)).unwrap()),
        ]);
        assert_eq!(both.len(), 1);
        // A line from before requests were signed still reads (for its card), unsigned.
        let old = format!("FLOTILLA {ID} {json}");
        assert!(matches!(
            &requests(&[tool(&serde_json::to_string(&Value::String(old)).unwrap())])[..],
            [Seen::Whole { sig: None, .. }]
        ));
    }

    #[test]
    fn cut_lines_and_lookalikes() {
        let cut = format!(
            r#""FLOTILLA {ID} {SIG} {{\"op\":\"delegate\",\"id\":\"{ID}\",\"task\":\"Write te""#
        );
        assert_eq!(
            requests(&[tool(&cut)]),
            [Seen::Cut {
                id: ID.into(),
                sig: Some(SIG.into())
            }]
        );
        let old_cut =
            format!(r#""FLOTILLA {ID} {{\"op\":\"delegate\",\"id\":\"{ID}\",\"task\":\"Write te""#);
        assert_eq!(
            requests(&[tool(&old_cut)]),
            [Seen::Cut {
                id: ID.into(),
                sig: None
            }]
        );
        // Not a request: no id, an id that doesn't match, a pending call, a tail that isn't
        // the command's JSON (a log line), a signature cut short.
        assert!(requests(&[tool(r#""FLOTILLA hello world""#)]).is_empty());
        let other = format!(
            r#""FLOTILLA {ID} {SIG} {{\"op\":\"cancel\",\"id\":\"00000000-0000-0000-0000-000000000000\",\"to\":\"x\"}}""#
        );
        assert!(requests(&[tool(&other)]).is_empty());
        let mut pending = tool(&format!(
            r#""FLOTILLA {ID} {SIG} {{\"op\":\"cancel\",\"id\":\"{ID}\",\"to\":\"x\"}}""#
        ));
        pending.pending = true;
        assert!(requests(&[pending]).is_empty());
        assert!(
            requests(&[tool(&format!(r#""FLOTILLA {ID} {SIG} started the build""#))]).is_empty()
        );
        assert!(requests(&[tool(&format!(r#""FLOTILLA {ID} {}""#, &SIG[..20]))]).is_empty());
    }

    #[test]
    fn answers_read_back() {
        let w = wake_text(
            "c1",
            "Add tests",
            Some("b"),
            Some("Added 4 tests.\nAll pass."),
            "finished",
        );
        assert_eq!(
            parse_answer(&w),
            Some(("Add tests", "c1", "finished", "Added 4 tests.\nAll pass."))
        );
        let one_line = crate::util::one_line(&w);
        assert_eq!(
            parse_answer(&one_line).map(|a| (a.0, a.2)),
            Some(("Add tests", "finished"))
        );
        assert_eq!(
            parse_answer("[flotilla] \"X\" (c1) was stopped without an answer."),
            Some(("X", "c1", "was stopped without an answer", ""))
        );
        assert_eq!(task_of(&child_prompt("Fix it")), Some("Fix it"));
        assert_eq!(
            task_of(&crate::util::one_line(&child_prompt("Fix it"))),
            Some("Fix it")
        );
        assert_eq!(task_of("Fix it"), None);
    }

    #[test]
    fn a_hand_off_that_couldnt_start_reads_back() {
        let text = couldnt_start_text("Add tests", "c1", "boat said 402");
        assert_eq!(
            text,
            "[flotilla] Couldn't start \"Add tests\" (c1): boat said 402"
        );
        assert_eq!(
            parse_couldnt_start(&text),
            Some(("Add tests", "c1", "boat said 402"))
        );
        assert_eq!(
            parse_couldnt_start("[flotilla] \"Add tests\" (c1) finished."),
            None
        );
        assert_eq!(parse_answer(&text), None);
    }

    /// Each of Flotilla's own messages is marked as its, and a request it couldn't read reads
    /// back; what you write isn't, even quoting one.
    #[test]
    fn flotillas_own_messages_are_marked() {
        let unreadable = unreadable_text("c1");
        assert_eq!(parse_unreadable(&unreadable), Some("c1"));
        assert_eq!(
            (parse_answer(&unreadable), parse_couldnt_start(&unreadable)),
            (None, None)
        );
        for text in [
            wake_text("c1", "X", None, Some("Done."), "finished"),
            couldnt_start_text("X", "c1", "no"),
            unreadable,
        ] {
            assert!(by_flotilla(&text), "{text}");
        }
        for text in [
            "Fix it",
            "Why did it say [flotilla] \"X\" (c1) finished?",
            "[flotilla]",
        ] {
            assert!(!by_flotilla(text), "{text}");
        }
    }

    #[test]
    fn wake_and_titles() {
        let w = wake_text(
            "c1",
            "Add tests",
            Some("flotilla/add-tests"),
            Some("Added 4 tests.\n"),
            "finished",
        );
        assert_eq!(
            w,
            "[flotilla] \"Add tests\" (c1, branch flotilla/add-tests) finished:\n\nAdded 4 tests."
        );
        assert_eq!(
            wake_text("c1", "X", None, None, "was stopped"),
            "[flotilla] \"X\" (c1) was stopped without an answer."
        );
        assert_eq!(
            wake_text("c1", "X", None, Some("Halfway."), "was stopped"),
            "[flotilla] \"X\" (c1) was stopped:\n\nHalfway."
        );
        assert!(
            wake_text("c1", "X", None, Some(&"é".repeat(MAX_ANSWER)), "finished")
                .ends_with("[…cut short]")
        );
        assert_eq!(
            child_title("", "\n  Fix the login bug in auth.rs\nthen test"),
            "Fix the login bug in auth.rs"
        );
        assert_eq!(child_title(" Tests ", "x"), "Tests");
    }
}
