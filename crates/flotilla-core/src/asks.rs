//! Questions an agent asked you, and your answers.
//!
//! Under boat only Codex's own ask tool, `request_user_input_async`, gets through (boat turns
//! Claude Code's `AskUserQuestion` off, and OpenCode's `question` tool isn't there in the mode
//! boat runs it in). Codex takes the question at once and the turn goes on, usually to its end;
//! boat's events show the question only as text in the reply. Codex's own record ("rollout")
//! keeps it whole, so `lineage.sh` reads it there with the thread's subagents ([`parse`]), and
//! [`crate::normalize`] puts it in the thread as an ask message in place of that text.
//!
//! So every agent is told to ask with `flotilla ask`, the `flotilla` command in the VM (Codex
//! too: it seldom uses its own tool unasked, and that question shows only once its record is
//! read; it's still read when Codex does): a request line its VM signed, like a hand-off's
//! (`crate::orchestrate`), checked here ([`checked`]) and kept with Codex's. Its ask message
//! stands in for the call that asked.
//!
//! An answer goes to the agent as your next message (a steer while a turn runs), which is how
//! the tool expects it back. What was read is kept per thread ([`store`]); answers are kept
//! apart ([`set_answer`]): the server writes those (and a restart's clean-up drops one whose
//! message never went), the reader the others, each change under the write lock.

use crate::error::Result;
use crate::store::Store;
use flotilla_proto::ask::{Choice, Question};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub use flotilla_proto::ask::{ANSWERED, KIND, PASSED, WAITING, reply_text, tidy};

/// Codex's tool, as the thread names it.
pub const CODEX_TOOL: &str = "request_user_input_async";
/// The `flotilla` command's, as the thread names it.
pub const FLOTILLA_TOOL: &str = "flotilla ask";

// What one `flotilla ask` may hold (`flotilla.sh` checks the same, to say so at once).
pub const MAX_QUESTIONS: usize = 3;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
/// Characters, after trimming.
pub const MAX_QUESTION: usize = 1000;
pub const MAX_HEADER: usize = 40;
pub const MAX_LABEL: usize = 120;
pub const MAX_DESCRIPTION: usize = 300;

/// A question as the harness recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    /// The harness's id for it (Codex's call id).
    pub id: String,
    /// When it was asked (ms).
    pub at_ms: i64,
    /// The text boat's events show for it, which the ask message stands in for.
    #[serde(default)]
    pub text: String,
    pub tool: String,
    pub questions: Vec<Question>,
    /// Its turn's prompt as the harness got it (your first message in that turn), which finds
    /// the turn among boat's when no reply shows its text yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// You wrote to the agent after it was asked (a steer, or a later turn), as its own record
    /// orders them: it was passed by, unless that was its answer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub followed: bool,
    /// The tool call that asked (`flotilla ask`'s), which its ask message stands in for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// The turn it was asked in (boat's task id), where no call is found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
}

/// Your answer to one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// One per question, as sent.
    pub answers: Vec<Vec<String>>,
    /// The message that took it: its `local-` placeholder's id, then the id boat gave it once
    /// boat has it ([`moved`]), which the message is found by.
    pub local: String,
}

/// One with its answer, for [`crate::normalize`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub asked: Asked,
    pub answer: Option<Answer>,
}

/// The questions in what `lineage.sh` printed (`asks`, beside the subagents): Codex's
/// `{id, at, text, questions: [{title, options}]}`. Ones that don't read are left out.
pub fn parse(out: &str) -> Vec<Asked> {
    let Ok(v) = serde_json::from_str::<Value>(out.trim()) else {
        return Vec::new();
    };
    let Some(list) = v.get("asks").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter().filter_map(codex_ask).collect()
}

fn codex_ask(a: &Value) -> Option<Asked> {
    let id = a
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_string();
    let at_ms = a.get("at").and_then(Value::as_i64)?;
    let questions: Vec<Question> = a
        .get("questions")?
        .as_array()?
        .iter()
        .filter_map(|q| {
            let question = q
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())?
                .to_string();
            let options = q
                .get("options")
                .and_then(Value::as_array)
                .map(|o| {
                    o.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(|l| Choice {
                            label: l.to_string(),
                            description: None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Question {
                header: None,
                question,
                options,
                multi: false,
            })
        })
        .collect();
    if questions.is_empty() {
        return None;
    }
    Some(Asked {
        id,
        at_ms,
        text: a
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tool: CODEX_TOOL.into(),
        questions,
        prompt: a
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string),
        followed: a.get("followed").and_then(Value::as_bool).unwrap_or(false),
        call: None,
        turn: None,
    })
}

/// A short line: trimmed, each run of white space (a newline too) one space.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A `flotilla ask`'s questions as they're kept, or why they can't be: 1 to [`MAX_QUESTIONS`],
/// each with its text and [`MIN_OPTIONS`] to [`MAX_OPTIONS`] options of different labels,
/// nothing too long. A header, label or description is one line. The command checks the same
/// before it prints the request; this is what holds, whatever reached the thread.
pub fn checked(questions: Vec<Question>) -> std::result::Result<Vec<Question>, String> {
    let too_long = |what: &str, s: &str, max: usize| {
        (s.chars().count() > max).then(|| format!("{what} is over {max} characters"))
    };
    if questions.is_empty() || questions.len() > MAX_QUESTIONS {
        return Err(format!(
            "ask 1 to {MAX_QUESTIONS} questions at once, not {}",
            questions.len()
        ));
    }
    questions
        .into_iter()
        .enumerate()
        .map(|(i, q)| {
            let n = i + 1;
            let question = q.question.trim().to_string();
            if question.is_empty() {
                return Err(format!("question {n} has no text"));
            }
            let header = q.header.as_deref().map(one_line).filter(|h| !h.is_empty());
            if let Some(e) =
                too_long(&format!("question {n}"), &question, MAX_QUESTION).or_else(|| {
                    header.as_deref().and_then(|h| {
                        too_long(&format!("the header of question {n}"), h, MAX_HEADER)
                    })
                })
            {
                return Err(e);
            }
            if q.options.len() < MIN_OPTIONS || q.options.len() > MAX_OPTIONS {
                return Err(format!(
                    "question {n} needs {MIN_OPTIONS} to {MAX_OPTIONS} options, not {}",
                    q.options.len()
                ));
            }
            let mut options: Vec<Choice> = Vec::new();
            for o in q.options {
                let label = one_line(&o.label);
                let description = o
                    .description
                    .as_deref()
                    .map(one_line)
                    .filter(|d| !d.is_empty());
                if label.is_empty() {
                    return Err(format!("an option of question {n} has no label"));
                }
                if let Some(e) = too_long(&format!("option \"{label}\""), &label, MAX_LABEL)
                    .or_else(|| {
                        description.as_deref().and_then(|d| {
                            too_long(
                                &format!("the description of option \"{label}\""),
                                d,
                                MAX_DESCRIPTION,
                            )
                        })
                    })
                {
                    return Err(e);
                }
                if options.iter().any(|x| x.label == label) {
                    return Err(format!("question {n} has the option \"{label}\" twice"));
                }
                options.push(Choice { label, description });
            }
            Ok(Question {
                header,
                question,
                options,
                multi: q.multi,
            })
        })
        .collect()
}

/// A `flotilla ask` request as it's kept: asked at `at_ms` (boat's time for the call), in turn
/// `turn`, by tool call `call`. Why not, if its questions don't check.
pub fn from_request(
    id: &str,
    questions: Vec<Question>,
    at_ms: i64,
    call: Option<String>,
    turn: Option<String>,
) -> std::result::Result<Asked, String> {
    Ok(Asked {
        id: id.to_string(),
        at_ms,
        text: String::new(),
        tool: FLOTILLA_TOOL.into(),
        questions: checked(questions)?,
        prompt: None,
        followed: false,
        call,
        turn,
    })
}

fn asked_key(thread: &str) -> String {
    format!("asks:{thread}")
}

fn answers_key(thread: &str) -> String {
    format!("answers:{thread}")
}

/// What was last read for a thread.
pub fn stored(store: &Store, thread: &str) -> Vec<Asked> {
    store
        .kv_get(&asked_key(thread))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

/// Keep what was read, adding to what was kept (a harness's record can go: a VM forked
/// afresh). The ids that are new, if any changed; None if nothing did.
pub fn store(store: &Store, thread: &str, read: &[Asked]) -> Result<Option<Vec<String>>> {
    let tx = store.write()?;
    let mut kept = stored(store, thread);
    let mut new = Vec::new();
    let mut changed = false;
    for a in read {
        match kept.iter_mut().find(|k| k.id == a.id) {
            Some(k) if k == a => {}
            Some(k) => {
                *k = a.clone();
                changed = true;
            }
            None => {
                kept.push(a.clone());
                new.push(a.id.clone());
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(None);
    }
    kept.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.id.cmp(&b.id)));
    store.kv_set(&asked_key(thread), &serde_json::to_string(&kept)?)?;
    tx.commit()?;
    Ok(Some(new))
}

/// Whether the thread's agent waits on the user's answer to a question. Nothing the user didn't
/// send goes to it meanwhile: a message would give it a turn, which passes the question.
pub fn waiting(store: &Store, thread: &str) -> bool {
    store.has_message(thread, KIND, WAITING).unwrap_or(false)
}

/// Your answers in a thread, by question id.
pub fn answers(store: &Store, thread: &str) -> BTreeMap<String, Answer> {
    store
        .kv_get(&answers_key(thread))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

/// Change a thread's answers under the write lock (read, change, write: another writer can't
/// land between). Whether `change` changed anything.
fn change_answers(
    store: &Store,
    thread: &str,
    change: impl FnOnce(&mut BTreeMap<String, Answer>) -> bool,
) -> Result<bool> {
    let tx = store.write()?;
    let mut all = answers(store, thread);
    if !change(&mut all) {
        return Ok(false);
    }
    store.kv_set(&answers_key(thread), &serde_json::to_string(&all)?)?;
    tx.commit()?;
    Ok(true)
}

/// Keep an answer and show the message that takes it (its `local-` placeholder, `kind` prompt
/// or steer) together: a quit can't leave one without the other.
pub fn answer_with_message(
    store: &Store,
    thread: &str,
    ask: &str,
    answer: Answer,
    kind: &str,
    text: &str,
) -> Result<()> {
    let tx = store.write()?;
    let mut all = answers(store, thread);
    store.insert_sent_prompt(thread, &answer.local, kind, text)?;
    all.insert(ask.to_string(), answer);
    store.kv_set(&answers_key(thread), &serde_json::to_string(&all)?)?;
    tx.commit()?;
    Ok(())
}

/// A message that didn't go (its `local-` or `unsent-` id): it goes, with any answer it took,
/// together. Whether it took one.
pub fn drop_with_message(store: &Store, thread: &str, prompt: &str) -> Result<bool> {
    let tx = store.write()?;
    let mut all = answers(store, thread);
    let before = all.len();
    all.retain(|_, a| same_message(&a.local, prompt));
    store.delete_sent_prompt(thread, prompt)?;
    let took = all.len() != before;
    if took {
        store.kv_set(&answers_key(thread), &serde_json::to_string(&all)?)?;
    }
    tx.commit()?;
    Ok(took)
}

/// Whether two message ids aren't the same message (a `local-` id is `unsent-` after a
/// restart).
fn same_message(a: &str, b: &str) -> bool {
    let uuid = |id: &str| {
        id.strip_prefix(crate::lifecycle::LOCAL_PREFIX)
            .or_else(|| id.strip_prefix(crate::lifecycle::UNSENT_PREFIX))
            .unwrap_or(id)
            .to_string()
    };
    uuid(a) != uuid(b)
}

/// The message `from` (a placeholder) that took an answer reached boat as `to`: the answer goes
/// with it. Under the caller's write lock (`Store::record_sent_prompt`).
pub(crate) fn moved(store: &Store, thread: &str, from: &str, to: &str) -> Result<()> {
    let mut all = answers(store, thread);
    let mut changed = false;
    for a in all.values_mut().filter(|a| a.local == from) {
        a.local = to.to_string();
        changed = true;
    }
    if changed {
        store.kv_set(&answers_key(thread), &serde_json::to_string(&all)?)?;
    }
    Ok(())
}

pub fn set_answer(store: &Store, thread: &str, ask: &str, answer: Answer) -> Result<()> {
    change_answers(store, thread, |all| {
        all.insert(ask.to_string(), answer);
        true
    })
    .map(|_| ())
}

/// Forget the answer the message `prompt` took (its `local-` id, or the `unsent-` one a
/// restart gave it), when that message didn't go. Whether there was one.
pub fn drop_answer_of(store: &Store, thread: &str, prompt: &str) -> Result<bool> {
    change_answers(store, thread, |all| {
        let before = all.len();
        all.retain(|_, a| same_message(&a.local, prompt));
        all.len() != before
    })
}

/// A thread's questions with their answers.
pub fn of_thread(store: &Store, thread: &str) -> Vec<Ask> {
    let asked = stored(store, thread);
    if asked.is_empty() {
        return Vec::new();
    }
    let mut answers = answers(store, thread);
    asked
        .into_iter()
        .map(|a| Ask {
            answer: answers.remove(&a.id),
            asked: a,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As `lineage.sh` printed it for a live Codex turn (2026-11-01), the subagents left out.
    const READ: &str = r#"{"subagents":[],"asks":[{"id":"call_4e640a800d854e80a3eff9c38bdbf4bc","turn":"01a1992b-b765-7fe0-b521-ac6a48610d5d","at":1793571145443,"text":"Which file name should I create?\n- alpha.txt (recommended)\n- beta.txt","questions":[{"title":"Which file name should I create?","options":["alpha.txt (recommended)","beta.txt"]}],"prompt":"Ask me which file name, then make it.","followed":false}]}"#;

    #[test]
    fn reads_codex_questions() {
        let a = parse(READ);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].id, "call_4e640a800d854e80a3eff9c38bdbf4bc");
        assert_eq!(a[0].at_ms, 1793571145443);
        assert_eq!(
            a[0].questions[0].question,
            "Which file name should I create?"
        );
        assert_eq!(
            a[0].questions[0]
                .options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            ["alpha.txt (recommended)", "beta.txt"]
        );
        assert!(!a[0].questions[0].multi);
        assert_eq!(
            (a[0].prompt.as_deref(), a[0].followed),
            (Some("Ask me which file name, then make it."), false)
        );
        // No asks, an old script's answer, or one without a question: nothing.
        assert!(parse(r#"{"subagents":[]}"#).is_empty());
        assert!(parse("not json").is_empty());
        assert!(parse(r#"{"asks":[{"id":"x","at":1,"questions":[{"title":" "}]}]}"#).is_empty());
        // A question with no options is answered by typing.
        assert_eq!(
            parse(r#"{"asks":[{"id":"x","at":1,"questions":[{"title":"Why?"}]}]}"#)[0].questions[0]
                .options,
            []
        );
    }

    #[test]
    fn keeps_what_was_read_and_says_what_is_new() {
        let store = Store::open_in_memory().unwrap();
        let a = parse(READ);
        assert_eq!(store_new(&store, &a), Some(vec![a[0].id.clone()]));
        assert_eq!(store_new(&store, &a), None);
        // Read again without it (a forked VM): kept.
        assert_eq!(store_new(&store, &[]), None);
        assert_eq!(stored(&store, "t1"), a);
        // An answer, then the message that took it didn't go: it's forgotten.
        set_answer(
            &store,
            "t1",
            &a[0].id,
            Answer {
                answers: vec![vec!["beta.txt".into()]],
                local: "local-1".into(),
            },
        )
        .unwrap();
        assert_eq!(
            of_thread(&store, "t1")[0].answer.as_ref().unwrap().answers,
            [vec!["beta.txt"]]
        );
        assert!(!drop_answer_of(&store, "t1", "local-2").unwrap());
        assert!(drop_answer_of(&store, "t1", "unsent-1").unwrap());
        assert_eq!(of_thread(&store, "t1")[0].answer, None);
        // An answer and its message go together, and leave together.
        let answer = Answer {
            answers: vec![vec!["beta.txt".into()]],
            local: "local-3".into(),
        };
        answer_with_message(
            &store,
            "t1",
            &a[0].id,
            answer,
            "prompt",
            "Which file name should I create?\nbeta.txt",
        )
        .unwrap();
        assert_eq!(
            store
                .sent_prompts("t1")
                .unwrap()
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["local-3"]
        );
        assert!(of_thread(&store, "t1")[0].answer.is_some());
        assert!(drop_with_message(&store, "t1", "local-3").unwrap());
        assert!(
            store.sent_prompts("t1").unwrap().is_empty()
                && of_thread(&store, "t1")[0].answer.is_none()
        );
        assert!(!drop_with_message(&store, "t1", "local-3").unwrap());
        // Its message reaches boat: the answer goes with it, to boat's id.
        let answer = Answer {
            answers: vec![vec!["beta.txt".into()]],
            local: "local-4".into(),
        };
        answer_with_message(
            &store,
            "t1",
            &a[0].id,
            answer,
            "prompt",
            "Which file name should I create?\nbeta.txt",
        )
        .unwrap();
        store
            .record_sent_prompt(
                "t1",
                Some("local-4"),
                "pr-4",
                "prompt",
                "Which file name should I create?\nbeta.txt",
            )
            .unwrap();
        assert_eq!(
            of_thread(&store, "t1")[0]
                .answer
                .as_ref()
                .map(|a| a.local.as_str()),
            Some("pr-4")
        );
    }

    /// What a `flotilla ask` request may hold, whatever reached the thread (a line the agent
    /// signed itself, not the command's): the counts and lengths, one-line labels, no option
    /// twice.
    #[test]
    fn a_flotilla_ask_is_checked_before_it_is_kept() {
        let q = |question: &str, options: &[&str]| Question {
            header: None,
            question: question.into(),
            options: options
                .iter()
                .map(|l| Choice {
                    label: l.to_string(),
                    description: None,
                })
                .collect(),
            multi: false,
        };
        let ok = checked(vec![Question {
            header: Some(" Pick\n one ".into()),
            ..q(" Which?\nReally. ", &["a  (recommended)", "b"])
        }])
        .unwrap();
        assert_eq!(
            (
                ok[0].header.as_deref(),
                ok[0].question.as_str(),
                ok[0].options[0].label.as_str()
            ),
            (Some("Pick one"), "Which?\nReally.", "a (recommended)")
        );
        let refused = |qs: Vec<Question>| checked(qs).unwrap_err();
        assert!(refused(vec![]).contains("not 0"));
        assert!(refused(vec![q("A?", &["a", "b"]); MAX_QUESTIONS + 1]).contains("not 4"));
        assert!(refused(vec![q("A?", &["a"])]).contains("needs 2 to 4 options"));
        assert!(refused(vec![q("A?", &["a", "b", "c", "d", "e"])]).contains("not 5"));
        assert!(refused(vec![q(" ", &["a", "b"])]).contains("no text"));
        assert!(refused(vec![q(&"x".repeat(MAX_QUESTION + 1), &["a", "b"])]).contains("over 1000"));
        assert!(refused(vec![q("A?", &["a", " a"])]).contains("twice"));
        assert!(refused(vec![q("A?", &["", "b"])]).contains("no label"));
        assert!(refused(vec![q("A?", &[&"l".repeat(MAX_LABEL + 1), "b"])]).contains("over 120"));
        assert!(
            refused(vec![Question {
                header: Some("h".repeat(MAX_HEADER + 1)),
                ..q("A?", &["a", "b"])
            }])
            .contains("header")
        );
        let long = Question {
            options: vec![
                Choice {
                    label: "a".into(),
                    description: Some("d".repeat(MAX_DESCRIPTION + 1)),
                },
                Choice {
                    label: "b".into(),
                    description: None,
                },
            ],
            ..q("A?", &[])
        };
        assert!(refused(vec![long]).contains("description"));
        // Kept as asked by the call, in its turn.
        let a = from_request(
            "r1",
            vec![q("A?", &["a", "b"])],
            5,
            Some("toolu_1".into()),
            Some("task-1".into()),
        )
        .unwrap();
        assert_eq!(
            (
                a.tool.as_str(),
                a.call.as_deref(),
                a.turn.as_deref(),
                a.text.as_str()
            ),
            (FLOTILLA_TOOL, Some("toolu_1"), Some("task-1"), "")
        );
    }

    fn store_new(store: &Store, read: &[Asked]) -> Option<Vec<String>> {
        super::store(store, "t1", read).unwrap()
    }
}
