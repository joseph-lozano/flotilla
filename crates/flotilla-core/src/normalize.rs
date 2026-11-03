//! Events → messages. Pure: the same events and sent prompts always give the same messages.

use crate::asks::Ask;
use crate::event::{RawEvent, is_terminal_status};
use crate::store::SentPrompt;
use crate::util::{ellipsize, iso_to_ms, one_line};
use serde_json::Value;
use std::collections::HashMap;

/// Bump to make every database drop and rebuild its `messages` table on next open.
pub const MESSAGES_VERSION: u32 = 9;

pub use flotilla_model::normalize::{NOT_SENT, PROMPT_NOT_KEPT};

/// How far a VM's clock may be from boat's, for a question whose turn only its time tells: one
/// placed by when it was asked counts boat's turns and steers from this long after.
const ASK_SKEW_MS: i64 = 60_000;

const STEER_WRAPPER_START: &str =
    "This is a message from the user, the same person who gave you the original task.";
const STEER_WRAPPER_MARK: &str = "The user's instruction: ";

pub use flotilla_proto::Message;

/// If `text` is boat's steer wrapper, the steer instruction it ends with.
pub fn steer_wrapper_instruction(text: &str) -> Option<&str> {
    if !text.starts_with(STEER_WRAPPER_START) {
        return None;
    }
    text.rfind(STEER_WRAPPER_MARK)
        .map(|i| text[i + STEER_WRAPPER_MARK.len()..].trim())
}

struct Group<'a> {
    task_id: Option<String>,
    prompt: Option<&'a RawEvent>,
    /// A prompt this machine sent that boat hasn't shown an event for yet.
    local_prompt: Option<&'a SentPrompt>,
    items: Vec<Item<'a>>,
    start: i64,
}

enum Item<'a> {
    Event(&'a RawEvent),
    /// A steer this machine sent that boat hasn't shown an event for yet.
    LocalSteer(&'a SentPrompt, i64),
    /// A question asked whose text no reply shows: when it was asked, and if its turn was found
    /// by time alone, from when boat's steers count as after it.
    Ask(&'a Ask, i64, Option<i64>),
}

impl Item<'_> {
    fn sort_key(&self) -> (i64, &str) {
        match self {
            Item::Event(e) => (e.timestamp, e.id.as_str()),
            Item::LocalSteer(s, ts) => (*ts, s.id.as_str()),
            Item::Ask(a, ts, _) => (*ts, a.asked.id.as_str()),
        }
    }
}

impl Group<'_> {
    fn is_turn(&self) -> bool {
        self.prompt.is_some() || self.local_prompt.is_some()
    }
}

pub fn normalize(thread_id: &str, events: &[RawEvent], sent: &[SentPrompt]) -> Vec<Message> {
    normalize_with(thread_id, events, sent, &[])
}

/// [`normalize`], with the questions the agent asked (`crate::asks`): each an ask message in
/// place of the text a reply shows for it, else in the turn it was asked in.
pub fn normalize_with(
    _thread_id: &str,
    events: &[RawEvent],
    sent: &[SentPrompt],
    asks: &[Ask],
) -> Vec<Message> {
    let sent_by_id: HashMap<&str, &SentPrompt> = sent.iter().map(|s| (s.id.as_str(), s)).collect();
    let mut groups: Vec<Group> = Vec::new();
    let mut by_task: HashMap<Option<String>, usize> = HashMap::new();
    let mut steers: Vec<&RawEvent> = Vec::new();

    let events: Vec<&RawEvent> = events.iter().filter(|e| !e.is_partial()).collect();

    // Each question goes in place of the call that asked (`flotilla ask`), or where a reply
    // shows its text (the reply nearest when it was asked, each showing of the text taken
    // once: the same question asked again goes by its own), else on its own in the turn it was
    // asked in.
    let calls: HashMap<&str, &str> = events
        .iter()
        .filter(|e| e.kind == "response")
        .flat_map(|e| {
            e.data
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(move |t| {
                    t.pointer("/use/id")
                        .and_then(Value::as_str)
                        .map(|c| (c, e.id.as_str()))
                })
        })
        .collect();
    let mut in_call: HashMap<&str, &Ask> = HashMap::new();
    let mut in_reply: HashMap<&str, Vec<&Ask>> = HashMap::new();
    let mut loose: Vec<&Ask> = Vec::new();
    let replies: Vec<(&RawEvent, &str)> = events
        .iter()
        .filter(|e| e.kind == "response")
        .filter_map(|e| {
            e.data
                .get("content")
                .and_then(Value::as_str)
                .map(|c| (*e, c))
        })
        .collect();
    let mut taken: HashMap<(&str, &str), usize> = HashMap::new();
    let mut by_time: Vec<&Ask> = asks.iter().collect();
    by_time.sort_by(|a, b| {
        a.asked
            .at_ms
            .cmp(&b.asked.at_ms)
            .then_with(|| a.asked.id.cmp(&b.asked.id))
    });
    for a in by_time {
        if let Some(call) = a
            .asked
            .call
            .as_deref()
            .filter(|c| calls.contains_key(c) && !in_call.contains_key(c))
        {
            in_call.insert(call, a);
            continue;
        }
        let text = a.asked.text.trim();
        let hit = (!text.is_empty())
            .then(|| {
                replies
                    .iter()
                    .filter(|(e, c)| {
                        c.matches(text).count()
                            > taken.get(&(e.id.as_str(), text)).copied().unwrap_or(0)
                    })
                    .min_by_key(|(e, _)| ((e.timestamp - a.asked.at_ms).abs(), e.id.as_str()))
            })
            .flatten();
        match hit {
            Some((e, _)) => {
                *taken.entry((e.id.as_str(), text)).or_default() += 1;
                in_reply.entry(e.id.as_str()).or_default().push(a);
            }
            None => loose.push(a),
        }
    }

    // A turn = all events sharing a prompt event's taskId.
    for p in events.iter().filter(|e| e.is_prompt()) {
        let task = Some(p.task_id.clone().unwrap_or_else(|| p.id.clone()));
        by_task.insert(task.clone(), groups.len());
        groups.push(Group {
            task_id: task,
            prompt: Some(p),
            local_prompt: None,
            items: vec![],
            start: p.timestamp,
        });
    }
    for e in events.iter().filter(|e| !e.is_prompt()) {
        if e.kind == "steer" {
            steers.push(e);
            continue;
        }
        // Orphans (no matching prompt) are grouped by taskId.
        let idx = *by_task.entry(e.task_id.clone()).or_insert_with(|| {
            groups.push(Group {
                task_id: e.task_id.clone(),
                prompt: None,
                local_prompt: None,
                items: vec![],
                start: e.timestamp,
            });
            groups.len() - 1
        });
        groups[idx].items.push(Item::Event(e));
    }
    for s in sent.iter().filter(|s| s.kind == "prompt") {
        if let std::collections::hash_map::Entry::Vacant(e) = by_task.entry(Some(s.id.clone())) {
            let ts = iso_to_ms(&s.sent_at).unwrap_or(i64::MAX);
            e.insert(groups.len());
            groups.push(Group {
                task_id: Some(s.id.clone()),
                prompt: None,
                local_prompt: Some(s),
                items: vec![],
                start: ts,
            });
        }
    }

    // A turn starts at the minimum timestamp of its events: after a steer rewrite, the prompt
    // event's own timestamp is later than the turn's first responses.
    for g in &mut groups {
        for it in &g.items {
            g.start = g.start.min(it.sort_key().0);
        }
    }

    // Steers belong to the turn whose rewritten prompt quotes them, else to the latest turn
    // that started at or before the steer.
    let steer_items: Vec<Item> = {
        let steer_ids: std::collections::HashSet<&str> =
            steers.iter().filter_map(|s| s.task_id.as_deref()).collect();
        let mut v: Vec<Item> = steers.iter().map(|s| Item::Event(s)).collect();
        for s in sent
            .iter()
            .filter(|s| s.kind == "steer" && !steer_ids.contains(s.id.as_str()))
        {
            v.push(Item::LocalSteer(
                s,
                iso_to_ms(&s.sent_at).unwrap_or(i64::MAX),
            ));
        }
        v.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        v
    };
    for item in steer_items {
        let (ts, message) = match &item {
            Item::Event(e) => (
                e.timestamp,
                e.data.get("message").and_then(Value::as_str).unwrap_or(""),
            ),
            Item::LocalSteer(s, ts) => (*ts, s.text.as_str()),
            Item::Ask(_, ts, _) => (*ts, ""),
        };
        let quoted = groups.iter().position(|g| {
            g.prompt
                .and_then(|p| p.data.get("prompt").and_then(Value::as_str))
                .and_then(steer_wrapper_instruction)
                .is_some_and(|ins| !message.is_empty() && ins == message.trim())
        });
        let latest = groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.is_turn() && g.start <= ts)
            .max_by_key(|(_, g)| g.start)
            .map(|(i, _)| i);
        let first_turn = groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.is_turn())
            .min_by_key(|(_, g)| g.start)
            .map(|(i, _)| i);
        match quoted.or(latest).or(first_turn) {
            Some(i) => groups[i].items.push(item),
            None => groups.push(Group {
                task_id: None,
                prompt: None,
                local_prompt: None,
                items: vec![item],
                start: ts,
            }),
        }
    }

    // A question no reply shows (yet: it streams) goes in its turn: the one whose prompt is the
    // one the harness got, else the latest that started before it was asked, give or take the
    // VM's clock against boat's.
    for a in loose {
        let at = a.asked.at_ms;
        // Asked in a turn boat names (`flotilla ask`, its call not found): that one.
        if let Some(i) = a.asked.turn.as_deref().and_then(|t| {
            groups
                .iter()
                .position(|g| g.is_turn() && g.task_id.as_deref() == Some(t))
        }) {
            groups[i].items.push(Item::Ask(a, at, Some(at)));
            continue;
        }
        let by_prompt = a.asked.prompt.as_deref().map(plain).and_then(|want| {
            groups
                .iter()
                .enumerate()
                .filter(|(_, g)| {
                    g.is_turn()
                        && turn_prompts(g, &sent_by_id)
                            .iter()
                            .any(|p| plain(p) == want)
                })
                .min_by_key(|(_, g)| (g.start - at).abs())
                .map(|(i, _)| i)
        });
        let by_time = || {
            groups
                .iter()
                .enumerate()
                .filter(|(_, g)| g.is_turn() && g.start <= at + ASK_SKEW_MS)
                .max_by_key(|(_, g)| g.start)
                .map(|(i, _)| i)
        };
        let first_turn = groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.is_turn())
            .min_by_key(|(_, g)| g.start)
            .map(|(i, _)| i);
        let (place, steers_from) = match by_prompt {
            Some(i) => (Some(i), None),
            None => (by_time().or(first_turn), Some(at + ASK_SKEW_MS)),
        };
        match place {
            Some(i) => groups[i].items.push(Item::Ask(a, at, steers_from)),
            None => groups.push(Group {
                task_id: None,
                prompt: None,
                local_prompt: None,
                items: vec![Item::Ask(a, at, steers_from)],
                start: at,
            }),
        }
    }

    // Turns by start time; within a turn the prompt first, then responses and steers by (timestamp, id).
    groups.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.task_id.cmp(&b.task_id))
    });

    // A question waits until answered, unless the thread moved on: you wrote to the agent after
    // it (its own record says), a turn after its own, or a steer in its turn after the reply
    // that shows it (boat's clock against boat's: the VM's can differ).
    let mut states: HashMap<&str, &'static str> = HashMap::new();
    for (gi, g) in groups.iter().enumerate() {
        let later_turn = groups[gi + 1..].iter().any(Group::is_turn);
        let steered_after = |at: i64| {
            g.items.iter().any(|it| match it {
                Item::Event(e) => e.kind == "steer" && e.timestamp > at,
                Item::LocalSteer(_, ts) => *ts > at,
                Item::Ask(..) => false,
            })
        };
        for it in &g.items {
            let placed: Vec<(&Ask, Option<i64>)> = match it {
                Item::Event(e) => in_reply
                    .get(e.id.as_str())
                    .into_iter()
                    .flatten()
                    .copied()
                    .chain(
                        in_call
                            .iter()
                            .filter(|(c, _)| calls.get(*c) == Some(&e.id.as_str()))
                            .map(|(_, a)| *a),
                    )
                    .map(|a| (a, Some(e.timestamp)))
                    .collect(),
                Item::Ask(a, _, steers_from) => vec![(*a, *steers_from)],
                Item::LocalSteer(..) => vec![],
            };
            for (a, steers_from) in placed {
                let state = if a.answer.is_some() {
                    crate::asks::ANSWERED
                } else if a.asked.followed || later_turn || steers_from.is_some_and(steered_after) {
                    crate::asks::PASSED
                } else {
                    crate::asks::WAITING
                };
                states.insert(a.asked.id.as_str(), state);
            }
        }
    }
    let state = |a: &Ask| -> &'static str {
        if a.answer.is_some() {
            crate::asks::ANSWERED
        } else {
            states
                .get(a.asked.id.as_str())
                .copied()
                .unwrap_or(crate::asks::WAITING)
        }
    };

    let mut out = Vec::new();
    for g in &mut groups {
        g.items.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        if let Some(p) = g.prompt {
            prompt_messages(p, g.start, &sent_by_id, &mut out);
        } else if let Some(s) = g.local_prompt {
            out.push(Message {
                kind: "prompt".into(),
                task_id: Some(s.id.clone()),
                text: Some(s.text.clone()),
                status: Some(placeholder_status(s).into()),
                event_key: Some(s.id.clone()),
                timestamp: Some(g.start),
                attachments: s.attachments.iter().map(|a| a.name.clone()).collect(),
                ..Default::default()
            });
        }
        let ended = g
            .prompt
            .and_then(|p| p.prompt_status())
            .filter(|s| is_terminal_status(s));
        for it in &g.items {
            match it {
                Item::Event(e) => {
                    let here: Vec<(&Ask, &'static str)> = in_reply
                        .get(e.id.as_str())
                        .map(|v| v.iter().map(|a| (*a, state(a))).collect())
                        .unwrap_or_default();
                    let calling: HashMap<&str, (&Ask, &'static str)> = in_call
                        .iter()
                        .filter(|(c, _)| calls.get(*c) == Some(&e.id.as_str()))
                        .map(|(c, a)| (*c, (*a, state(a))))
                        .collect();
                    event_messages(e, &sent_by_id, ended, &here, &calling, &mut out)
                }
                Item::Ask(a, ..) => out.push(ask_message(
                    a,
                    state(a),
                    g.task_id
                        .clone()
                        .or_else(|| g.prompt.and_then(|p| p.task_id.clone())),
                )),
                Item::LocalSteer(s, ts) => out.push(Message {
                    kind: "steer".into(),
                    task_id: Some(s.id.clone()),
                    text: Some(s.text.clone()),
                    status: Some(placeholder_status(s).into()),
                    event_key: Some(format!("steer-{}", s.id)),
                    timestamp: Some(*ts),
                    ..Default::default()
                }),
            }
        }
    }
    mark_answers(&mut out, asks);
    for (i, m) in out.iter_mut().enumerate() {
        m.seq = i as i64;
        // Flotilla's own, however it went: as a prompt, or as a steer into a running turn.
        m.from_flotilla = matches!(m.kind.as_str(), "prompt" | "steer")
            && m.text
                .as_deref()
                .is_some_and(flotilla_model::orchestrate::by_flotilla);
    }
    out
}

/// Each answered question's answer: the prompt or steer that took it says so
/// ([`Message::answer_to`]). It's the message the answer names (`Answer::local`: its placeholder,
/// or the id boat gave it), wherever it's placed (one still sending is placed by this machine's
/// clock, which can be behind boat's). An answer kept before it was named that way: the first
/// after its card that says it, as it went (`crate::asks::reply_text`).
fn mark_answers(out: &mut [Message], asks: &[Ask]) {
    let says = |m: &Message| matches!(m.kind.as_str(), "prompt" | "steer") && m.answer_to.is_none();
    let named: std::collections::HashSet<&str> = asks
        .iter()
        .filter_map(|a| a.answer.as_ref())
        .map(|x| x.local.as_str())
        .collect();
    for i in 0..out.len() {
        if out[i].kind != crate::asks::KIND
            || out[i].status.as_deref() != Some(crate::asks::ANSWERED)
        {
            continue;
        }
        let Some(id) = out[i].tool_id.clone() else {
            continue;
        };
        let Some(Ask {
            asked,
            answer: Some(answer),
        }) = asks.iter().find(|a| a.asked.id == id)
        else {
            continue;
        };
        let by_id = out
            .iter()
            .position(|m| says(m) && m.task_id.as_deref() == Some(answer.local.as_str()));
        let by_text = || {
            let text = crate::asks::reply_text(&asked.questions, &answer.answers);
            (i + 1..out.len()).find(|&j| {
                says(&out[j])
                    && !out[j].task_id.as_deref().is_some_and(|t| named.contains(t))
                    && out[j].text.as_deref().map(str::trim) == Some(text.trim())
            })
        };
        if let Some(j) = by_id.or_else(by_text) {
            out[j].answer_to = Some(id);
        }
    }
}

/// A message this Mac sent that boat hasn't shown yet: "sending", or "not sent" once a restart
/// found boat never got it.
fn placeholder_status(s: &SentPrompt) -> &'static str {
    if s.id.starts_with(crate::lifecycle::UNSENT_PREFIX) {
        NOT_SENT
    } else {
        "sending"
    }
}

fn prompt_messages(
    p: &RawEvent,
    start: i64,
    sent: &HashMap<&str, &SentPrompt>,
    out: &mut Vec<Message>,
) {
    let task = p.task_id.clone().unwrap_or_else(|| p.id.clone());
    let boat_text = p.data.get("prompt").and_then(Value::as_str).unwrap_or("");
    let text = match sent.get(task.as_str()) {
        Some(s) => s.text.clone(),
        None if steer_wrapper_instruction(boat_text).is_some() => PROMPT_NOT_KEPT.to_string(),
        None => boat_text.to_string(),
    };
    let mut attachments = p.attachment_names();
    if attachments.is_empty()
        && let Some(s) = sent.get(task.as_str())
    {
        attachments = s.attachments.iter().map(|a| a.name.clone()).collect();
    }
    out.push(Message {
        kind: "prompt".into(),
        task_id: Some(task.clone()),
        text: Some(text),
        status: p.prompt_status().map(str::to_string),
        event_key: Some(task.clone()),
        timestamp: Some(start),
        attachments,
        ..Default::default()
    });
    if let Some(err) = p.data.get("error").filter(|e| !e.is_null()) {
        let msg = err.as_str().map(str::to_string).or_else(|| {
            err.get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        out.push(Message {
            kind: "notice".into(),
            task_id: Some(task.clone()),
            text: Some(format!(
                "prompt failed: {}",
                msg.unwrap_or_else(|| err.to_string())
            )),
            status: Some("error".into()),
            is_error: true,
            event_key: Some(task),
            timestamp: Some(p.timestamp),
            ..Default::default()
        });
    }
}

/// A turn's prompt as boat has it and as this machine sent it.
fn turn_prompts<'a>(g: &Group<'a>, sent: &HashMap<&str, &'a SentPrompt>) -> Vec<&'a str> {
    let mut out = Vec::new();
    if let Some(p) = g.prompt {
        out.extend(p.data.get("prompt").and_then(Value::as_str));
    }
    if let Some(s) = g.local_prompt {
        out.push(s.text.as_str());
    }
    if let Some(s) = g.task_id.as_deref().and_then(|t| sent.get(t)) {
        out.push(s.text.as_str());
    }
    out
}

/// A prompt as compared: trimmed, and without the mark that keeps a leading `/` plain text.
fn plain(p: &str) -> &str {
    let p = p.trim();
    p.strip_prefix('\u{200B}').unwrap_or(p)
}

/// An ask message for a question (`crate::asks`), in `state`.
fn ask_message(a: &Ask, state: &str, task_id: Option<String>) -> Message {
    Message {
        kind: crate::asks::KIND.into(),
        task_id,
        text: Some(
            a.asked
                .questions
                .iter()
                .map(|q| q.question.trim())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        status: Some(state.into()),
        tool_name: Some(a.asked.tool.clone()),
        tool_id: Some(a.asked.id.clone()),
        tool_input_json: serde_json::to_string(&a.asked.questions).ok(),
        tool_result_json: a
            .answer
            .as_ref()
            .and_then(|x| serde_json::to_string(&x.answers).ok()),
        event_key: Some(format!("ask-{}", a.asked.id)),
        timestamp: Some(a.asked.at_ms),
        ..Default::default()
    }
}

/// A reply's text with each question's (`asks`, as boat shows them) taken out: the text before
/// and after each, and the questions, in order.
fn split_reply<'a>(
    content: &str,
    asks: &[(&'a Ask, &'static str)],
) -> Vec<Result<String, (&'a Ask, &'static str)>> {
    let mut pieces: Vec<Result<String, (&Ask, &str)>> = vec![Ok(content.to_string())];
    for (a, state) in asks {
        let text = a.asked.text.trim();
        // Not there (any more): after the reply, never left out.
        let Some(at) = pieces
            .iter()
            .position(|p| p.as_ref().is_ok_and(|t| t.contains(text)))
        else {
            pieces.push(Err((*a, *state)));
            continue;
        };
        let Ok(whole) = pieces.remove(at) else {
            continue;
        };
        let i = whole.find(text).unwrap_or(0);
        let (before, after) = (whole[..i].to_string(), whole[i + text.len()..].to_string());
        pieces.splice(at..at, [Ok(before), Err((*a, *state)), Ok(after)]);
    }
    pieces
}

/// `calling`: the questions asked by its calls (`flotilla ask`), by call id, each in place of
/// its call.
fn event_messages(
    e: &RawEvent,
    sent: &HashMap<&str, &SentPrompt>,
    turn_ended: Option<&str>,
    asks: &[(&Ask, &'static str)],
    calling: &HashMap<&str, (&Ask, &'static str)>,
    out: &mut Vec<Message>,
) {
    let base = Message {
        task_id: e.task_id.clone(),
        event_key: Some(e.key()),
        timestamp: Some(e.timestamp),
        ..Default::default()
    };
    match e.kind.as_str() {
        "steer" => {
            let boat_text = e.data.get("message").and_then(Value::as_str).unwrap_or("");
            let text = e
                .task_id
                .as_deref()
                .and_then(|t| sent.get(t))
                .map(|s| s.text.clone())
                .unwrap_or_else(|| boat_text.to_string());
            out.push(Message {
                kind: "steer".into(),
                text: Some(text),
                status: e
                    .data
                    .get("mode")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                ..base
            });
        }
        "response" => {
            let model = e
                .data
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(c) = e
                .data
                .get("content")
                .and_then(Value::as_str)
                .filter(|c| !c.trim().is_empty())
            {
                if asks.is_empty() {
                    out.push(Message {
                        kind: "text".into(),
                        text: Some(c.to_string()),
                        model: model.clone(),
                        ..base.clone()
                    });
                } else {
                    for piece in split_reply(c, asks) {
                        match piece {
                            Ok(t) if !t.trim().is_empty() => {
                                let t = t.trim_matches(|ch| ch == '\n' || ch == '\r');
                                out.push(Message {
                                    kind: "text".into(),
                                    text: Some(t.to_string()),
                                    model: model.clone(),
                                    ..base.clone()
                                });
                            }
                            Ok(_) => {}
                            Err((a, state)) => out.push(ask_message(a, state, e.task_id.clone())),
                        }
                    }
                }
            }
            if let Some(tools) = e.data.get("tools").and_then(Value::as_array) {
                for t in tools {
                    let mut m = tool_message(t, model.clone(), base.clone());
                    // An interrupted turn can keep its in-flight tool call, pending forever.
                    if m.pending
                        && let Some(how) = turn_ended
                    {
                        m.pending = false;
                        m.status = Some(if how == "finished" {
                            "unfinished".into()
                        } else {
                            how.to_string()
                        });
                    }
                    // A question it asked: in its place, unless it printed another request too,
                    // which is still read from here (one cut short, read back on a later poll).
                    if let Some((a, state)) = t
                        .pointer("/use/id")
                        .and_then(Value::as_str)
                        .and_then(|c| calling.get(c))
                    {
                        if crate::orchestrate::requests(std::slice::from_ref(&m))
                            .iter()
                            .any(|r| r.id() != a.asked.id)
                        {
                            out.push(m);
                        }
                        out.push(ask_message(a, state, e.task_id.clone()));
                        continue;
                    }
                    out.push(m);
                }
            }
        }
        kind => {
            let d = &e.data;
            let detail = ["message", "text", "content"]
                .iter()
                .find_map(|k| d.get(*k).and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| ellipsize(&d.to_string(), 200));
            out.push(Message {
                kind: "notice".into(),
                text: Some(format!("{kind}: {detail}")),
                status: Some(kind.to_string()),
                ..base
            });
        }
    }
}

fn tool_message(t: &Value, model: Option<String>, base: Message) -> Message {
    let use_ = t.get("use");
    let result = t.get("result").filter(|r| !r.is_null());
    let name = use_
        .and_then(|u| u.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let id = use_
        .and_then(|u| u.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let input = use_
        .and_then(|u| u.get("input"))
        .cloned()
        .unwrap_or(Value::Null);
    // `result.content` is a JSON string with its own `type`.
    let parsed: Option<Value> = result.and_then(|r| r.get("content")).map(|c| match c {
        Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
        other => other.clone(),
    });
    let rtype = parsed
        .as_ref()
        .and_then(|p| p.get("type"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let pending = result.is_none()
        || parsed
            .as_ref()
            .and_then(|p| p.get("pending"))
            .and_then(Value::as_bool)
            == Some(true);
    let is_error = result
        .and_then(|r| r.get("is_error"))
        .and_then(Value::as_bool)
        == Some(true)
        || parsed
            .as_ref()
            .and_then(|p| p.get("error"))
            .and_then(Value::as_bool)
            == Some(true);
    let field = |v: Option<&Value>, k: &str| {
        v.and_then(|v| v.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let summary = match rtype.as_deref() {
        Some("bash_output") => {
            field(parsed.as_ref(), "command").or_else(|| field(Some(&input), "command"))
        }
        Some("file_write" | "file_content" | "file_edit") => {
            field(parsed.as_ref(), "filePath").or_else(|| field(Some(&input), "file_path"))
        }
        _ => None,
    }
    .or_else(|| {
        ["command", "file_path", "path", "pattern", "query", "url"]
            .iter()
            .find_map(|k| field(Some(&input), k))
    })
    .unwrap_or_else(|| {
        if input.is_null() {
            String::new()
        } else {
            input.to_string()
        }
    });
    Message {
        kind: "tool".into(),
        model,
        tool_name: Some(name),
        tool_id: id,
        tool_summary: Some(ellipsize(&one_line(&summary), 120)),
        tool_input_json: Some(input.to_string()),
        tool_result_json: parsed.map(|p| p.to_string()),
        tool_result_type: rtype,
        is_error,
        pending,
        ..base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asks::{Answer, Asked};
    use flotilla_proto::ask::{Choice, Question};
    use serde_json::json;

    /// A live Codex turn's events (2026-11-01), as boat showed them: the question only as
    /// text, at the start of the reply, with the agent's last line after it.
    fn codex_turn() -> Vec<RawEvent> {
        vec![
            RawEvent::from_value(
                json!({"id": "a093", "type": "prompt", "timestamp": 1793571137797_i64, "taskId": "f55b", "data": {"prompt": "Ask me which file name, then make it.", "status": "finished"}}),
            ),
            RawEvent::from_value(
                json!({"id": "0b94", "type": "response", "timestamp": 1793571145446_i64, "taskId": "f55b", "data": {"content": "Which file name should I create?\n- alpha.txt (recommended)\n- beta.txt\nWhich file name would you like: **alpha.txt** (recommended) or **beta.txt**?", "model": "gpt-6-luna"}}),
            ),
        ]
    }

    fn asked(text: &str) -> Asked {
        Asked {
            id: "call_4e64".into(),
            at_ms: 1793571145443,
            text: text.into(),
            tool: crate::asks::CODEX_TOOL.into(),
            prompt: None,
            followed: false,
            call: None,
            turn: None,
            questions: vec![Question {
                header: None,
                question: "Which file name should I create?".into(),
                options: vec![
                    Choice {
                        label: "alpha.txt (recommended)".into(),
                        description: None,
                    },
                    Choice {
                        label: "beta.txt".into(),
                        description: None,
                    },
                ],
                multi: false,
            }],
        }
    }

    const SHOWN: &str = "Which file name should I create?\n- alpha.txt (recommended)\n- beta.txt";

    #[test]
    fn a_question_stands_in_for_its_text_in_the_reply() {
        let ask = Ask {
            asked: asked(SHOWN),
            answer: None,
        };
        let m = normalize_with("t", &codex_turn(), &[], std::slice::from_ref(&ask));
        let kinds: Vec<&str> = m.iter().map(|m| m.kind.as_str()).collect();
        assert_eq!(kinds, ["prompt", "ask", "text"]);
        assert_eq!(m[1].status.as_deref(), Some(crate::asks::WAITING));
        assert_eq!(m[1].task_id.as_deref(), Some("f55b"));
        assert_eq!(m[1].tool_id.as_deref(), Some("call_4e64"));
        assert_eq!(flotilla_proto::ask::questions(&m[1]), ask.asked.questions);
        assert_eq!(
            m[1].text.as_deref(),
            Some("Which file name should I create?")
        );
        assert_eq!(
            m[2].text.as_deref(),
            Some("Which file name would you like: **alpha.txt** (recommended) or **beta.txt**?")
        );
        // Without it, the reply is as boat has it.
        let plain = normalize("t", &codex_turn(), &[]);
        assert_eq!(
            plain.iter().map(|m| m.kind.as_str()).collect::<Vec<_>>(),
            ["prompt", "text"]
        );
    }

    #[test]
    fn answered_passed_or_waiting() {
        let mut events = codex_turn();
        // Answered: what was picked comes with it, however the thread went on.
        let answered = Ask {
            asked: asked(SHOWN),
            answer: Some(Answer {
                answers: vec![vec!["beta.txt".into()]],
                local: "local-1".into(),
            }),
        };
        events.push(RawEvent::from_value(json!({"id": "d825", "type": "prompt", "timestamp": 1793571422000_i64, "taskId": "d825", "data": {"prompt": "Which file name should I create?\nbeta.txt", "status": "finished"}})));
        let m = normalize_with("t", &events, &[], std::slice::from_ref(&answered));
        let a = m.iter().find(|m| m.kind == "ask").unwrap();
        assert_eq!(a.status.as_deref(), Some(crate::asks::ANSWERED));
        assert_eq!(
            flotilla_proto::ask::answers(a),
            Some(vec![vec!["beta.txt".to_string()]])
        );
        // Not answered, and something else was sent after it: passed.
        let unanswered = Ask {
            answer: None,
            ..answered.clone()
        };
        let m = normalize_with("t", &events, &[], std::slice::from_ref(&unanswered));
        assert_eq!(
            m.iter()
                .find(|m| m.kind == "ask")
                .unwrap()
                .status
                .as_deref(),
            Some(crate::asks::PASSED)
        );
        // A message on its way counts too.
        let sending = SentPrompt {
            id: "local-9".into(),
            kind: "prompt".into(),
            text: "no".into(),
            sent_at: "2026-11-01T22:20:00Z".into(),
            ..Default::default()
        };
        let m = normalize_with(
            "t",
            &codex_turn(),
            &[sending],
            std::slice::from_ref(&unanswered),
        );
        assert_eq!(
            m.iter()
                .find(|m| m.kind == "ask")
                .unwrap()
                .status
                .as_deref(),
            Some(crate::asks::PASSED)
        );
    }

    #[test]
    fn the_same_question_asked_again_has_its_own_card() {
        // Asked in one turn, answered in prose (a later turn), asked again in that turn.
        let mut events = codex_turn();
        events.push(RawEvent::from_value(json!({"id": "p2", "type": "prompt", "timestamp": 1793571300000_i64, "taskId": "p2", "data": {"prompt": "Not sure, ask again.", "status": "finished"}})));
        events.push(RawEvent::from_value(json!({"id": "r2", "type": "response", "timestamp": 1793571310000_i64, "taskId": "p2", "data": {"content": format!("{SHOWN}\nWhich one?")}})));
        let first = Ask {
            asked: asked(SHOWN),
            answer: None,
        };
        let again = Ask {
            asked: Asked {
                id: "call_again".into(),
                at_ms: 1793571309990,
                ..asked(SHOWN)
            },
            answer: None,
        };
        let m = normalize_with("t", &events, &[], &[first, again]);
        let asks: Vec<(Option<&str>, Option<&str>, Option<&str>)> = m
            .iter()
            .filter(|m| m.kind == "ask")
            .map(|m| {
                (
                    m.tool_id.as_deref(),
                    m.task_id.as_deref(),
                    m.status.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            asks,
            [
                (Some("call_4e64"), Some("f55b"), Some("passed")),
                (Some("call_again"), Some("p2"), Some("waiting"))
            ]
        );
        // Neither reply shows the question's text any more.
        assert!(
            m.iter().filter(|m| m.kind == "text").all(|m| !m
                .text
                .as_deref()
                .unwrap_or("")
                .contains("- beta.txt"))
        );
    }

    #[test]
    fn a_vm_clock_behind_boats_doesnt_pass_a_new_question() {
        // The VM says it was asked before boat says the turn's prompt came: still its turn's.
        let ask = Ask {
            asked: Asked {
                at_ms: 1793571130000,
                ..asked(SHOWN)
            },
            answer: None,
        };
        let m = normalize_with("t", &codex_turn(), &[], &[ask]);
        assert_eq!(
            m.iter()
                .find(|m| m.kind == "ask")
                .unwrap()
                .status
                .as_deref(),
            Some("waiting")
        );
        // A steer after the reply that shows it, in the same turn, moves the thread on.
        let mut events = codex_turn();
        events.push(RawEvent::from_value(json!({"id": "s1", "type": "steer", "timestamp": 1793571146000_i64, "taskId": "s1", "data": {"message": "Use gamma.txt", "mode": "native"}})));
        let m = normalize_with(
            "t",
            &events,
            &[],
            &[Ask {
                asked: asked(SHOWN),
                answer: None,
            }],
        );
        assert_eq!(
            m.iter()
                .find(|m| m.kind == "ask")
                .unwrap()
                .status
                .as_deref(),
            Some("passed")
        );
    }

    #[test]
    fn a_question_still_streaming_goes_in_the_turn_its_vm_says_even_behind_boats_clock() {
        // Two turns; the second's reply isn't in yet, and the VM's clock is 5 s behind boat's.
        let mut events = codex_turn();
        events.push(RawEvent::from_value(json!({"id": "p2", "type": "prompt", "timestamp": 1793571300000_i64, "taskId": "p2", "data": {"prompt": "Now ask again.", "status": "running"}})));
        let ask = Ask {
            asked: Asked {
                id: "call_2".into(),
                at_ms: 1793571295000,
                ..asked(SHOWN)
            },
            answer: None,
        };
        let m = normalize_with(
            "t",
            &events,
            &[],
            &[
                Ask {
                    asked: asked(SHOWN),
                    answer: Some(Answer {
                        answers: vec![vec!["beta.txt".into()]],
                        local: "local-1".into(),
                    }),
                },
                ask,
            ],
        );
        let second = m
            .iter()
            .find(|m| m.tool_id.as_deref() == Some("call_2"))
            .unwrap();
        assert_eq!(
            (second.task_id.as_deref(), second.status.as_deref()),
            (Some("p2"), Some("waiting"))
        );
    }

    #[test]
    fn a_question_still_streaming_goes_in_the_turn_of_its_prompt() {
        // The reply isn't in yet; 20 s after it was asked you sent another prompt. By its
        // turn's prompt it's the first turn's, and the second passed it.
        let mut events = vec![codex_turn().remove(0)];
        events.push(RawEvent::from_value(json!({"id": "p2", "type": "prompt", "timestamp": 1793571165443_i64, "taskId": "p2", "data": {"prompt": "Never mind, use gamma.txt.", "status": "running"}})));
        let ask = Ask {
            asked: Asked {
                prompt: Some("Ask me which file name, then make it.".into()),
                ..asked(SHOWN)
            },
            answer: None,
        };
        let m = normalize_with("t", &events, &[], std::slice::from_ref(&ask));
        let a = m.iter().find(|m| m.kind == "ask").unwrap();
        assert_eq!(
            (a.task_id.as_deref(), a.status.as_deref()),
            (Some("f55b"), Some("passed"))
        );
        // Its own record says you wrote again (a steer that reached it): passed, whatever the
        // clocks say.
        let steered = Ask {
            asked: Asked {
                followed: true,
                ..asked(SHOWN)
            },
            answer: None,
        };
        let m = normalize_with("t", &codex_turn(), &[], &[steered]);
        assert_eq!(
            m.iter()
                .find(|m| m.kind == "ask")
                .unwrap()
                .status
                .as_deref(),
            Some("passed")
        );
    }

    /// A Claude Code turn that asked with `flotilla ask` (the call's output is its request
    /// line), then said one line and ended.
    fn claude_turn() -> Vec<RawEvent> {
        let line = "FLOTILLA 7c11d703-6d90-4aba-a93a-a86fd03a90f7 0123 {\"op\":\"ask\"}";
        vec![
            RawEvent::from_value(
                json!({"id": "p1", "type": "prompt", "timestamp": 1_000_000, "taskId": "p1", "data": {"prompt": "Ask me which file name, then make it.", "status": "finished"}}),
            ),
            RawEvent::from_value(
                json!({"id": "r1", "type": "response", "timestamp": 1_001_000, "taskId": "p1", "data": {"content": "", "tools": [{"use": {"id": "toolu_1", "name": "Bash", "input": {"command": "flotilla ask \"Which file name should I create?\" alpha.txt beta.txt"}}, "result": {"content": json!({"type": "bash_output", "content": line}).to_string(), "is_error": false}}]}}),
            ),
            RawEvent::from_value(
                json!({"id": "r2", "type": "response", "timestamp": 1_002_000, "taskId": "p1", "data": {"content": "I've asked which name to use."}}),
            ),
        ]
    }

    fn flotilla_asked() -> Asked {
        Asked {
            id: "7c11d703".into(),
            at_ms: 1_001_000,
            text: String::new(),
            tool: crate::asks::FLOTILLA_TOOL.into(),
            call: Some("toolu_1".into()),
            turn: Some("p1".into()),
            ..asked("")
        }
    }

    #[test]
    fn a_flotilla_ask_stands_in_for_the_call_that_asked() {
        let ask = Ask {
            asked: flotilla_asked(),
            answer: None,
        };
        let m = normalize_with("t", &claude_turn(), &[], std::slice::from_ref(&ask));
        assert_eq!(
            m.iter().map(|m| m.kind.as_str()).collect::<Vec<_>>(),
            ["prompt", "ask", "text"]
        );
        assert_eq!(
            (
                m[1].tool_id.as_deref(),
                m[1].task_id.as_deref(),
                m[1].status.as_deref()
            ),
            (Some("7c11d703"), Some("p1"), Some(crate::asks::WAITING))
        );
        // A steer after it passes it; one before it doesn't.
        for (at, state) in [
            (1_000_500, crate::asks::WAITING),
            (1_003_000, crate::asks::PASSED),
        ] {
            let mut events = claude_turn();
            events.push(RawEvent::from_value(json!({"id": "s1", "type": "steer", "timestamp": at, "taskId": "s1", "data": {"message": "use gamma.txt", "mode": "native"}})));
            let m = normalize_with("t", &events, &[], std::slice::from_ref(&ask));
            assert_eq!(
                m.iter()
                    .find(|m| m.kind == "ask")
                    .unwrap()
                    .status
                    .as_deref(),
                Some(state),
                "steer at {at}"
            );
        }
        // Its call not among the events: in the turn it names, even with a later turn begun
        // within a minute (which a time alone would put it in).
        let mut events = claude_turn();
        events.remove(1);
        events.push(RawEvent::from_value(json!({"id": "p2", "type": "prompt", "timestamp": 1_010_000, "taskId": "p2", "data": {"prompt": "Never mind.", "status": "running"}})));
        let m = normalize_with("t", &events, &[], std::slice::from_ref(&ask));
        let a = m.iter().find(|m| m.kind == "ask").unwrap();
        assert_eq!(
            (a.task_id.as_deref(), a.status.as_deref()),
            (Some("p1"), Some(crate::asks::PASSED))
        );
    }

    /// One call that asked and printed another request (a delegate, say, cut short in boat's
    /// events and read back on a later poll): the call stays, so that request is still found,
    /// with the question after it.
    #[test]
    fn a_call_that_printed_another_request_too_stays() {
        let other = "FLOTILLA 0b5c0f6e-0000-4000-8000-000000000001 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef {\"op\":\"delegate\",\"id\":\"0b5c0f6e-0000-4000-8000-000000000001\",\"task\":\"Write te";
        let mut events = claude_turn();
        let both = format!("FLOTILLA 7c11d703 x\n{other}");
        events[1] = RawEvent::from_value(
            json!({"id": "r1", "type": "response", "timestamp": 1_001_000, "taskId": "p1", "data": {"content": "", "tools": [{"use": {"id": "toolu_1", "name": "Bash", "input": {"command": "flotilla ask …; flotilla delegate …"}}, "result": {"content": json!({"type": "bash_output", "content": both}).to_string(), "is_error": false}}]}}),
        );
        let m = normalize_with(
            "t",
            &events,
            &[],
            &[Ask {
                asked: flotilla_asked(),
                answer: None,
            }],
        );
        assert_eq!(
            m.iter().map(|m| m.kind.as_str()).collect::<Vec<_>>(),
            ["prompt", "tool", "ask", "text"]
        );
        assert_eq!(
            crate::orchestrate::requests(&m)
                .iter()
                .map(|r| r.id().to_string())
                .collect::<Vec<_>>(),
            ["0b5c0f6e-0000-4000-8000-000000000001"]
        );
    }

    /// Your answer goes as the next prompt (or a steer, while the turn runs): it says it took
    /// it, so the apps show the card and not the message again, and it still heads its turn.
    #[test]
    fn the_message_that_took_an_answer_says_so() {
        let answer = |local: &str| {
            Some(Answer {
                answers: vec![vec!["beta.txt".into()]],
                local: local.into(),
            })
        };
        let ask = Ask {
            asked: flotilla_asked(),
            answer: answer("local-1"),
        };
        let delivered = |to: &str| Ask {
            answer: answer(to),
            ..ask.clone()
        };
        let text =
            crate::asks::reply_text(&ask.asked.questions, &ask.answer.as_ref().unwrap().answers);
        // Sending: its placeholder.
        let sending = SentPrompt {
            id: "local-1".into(),
            kind: "prompt".into(),
            text: text.clone(),
            sent_at: "2026-11-01T22:20:00Z".into(),
            ..Default::default()
        };
        let m = normalize_with(
            "t",
            &claude_turn(),
            std::slice::from_ref(&sending),
            std::slice::from_ref(&ask),
        );
        let p = m
            .iter()
            .find(|m| m.kind == "prompt" && m.status.as_deref() == Some("sending"))
            .unwrap();
        assert_eq!(p.answer_to.as_deref(), Some("7c11d703"));
        // Taken by boat: its turn, with the reply under it.
        let mut events = claude_turn();
        events.push(RawEvent::from_value(json!({"id": "p2", "type": "prompt", "timestamp": 1_100_000, "taskId": "pr-2", "data": {"prompt": text, "status": "finished"}})));
        events.push(RawEvent::from_value(json!({"id": "r3", "type": "response", "timestamp": 1_101_000, "taskId": "pr-2", "data": {"content": "Made beta.txt."}})));
        let sent = SentPrompt {
            id: "pr-2".into(),
            ..sending.clone()
        };
        let m = normalize_with(
            "t",
            &events,
            std::slice::from_ref(&sent),
            &[delivered("pr-2")],
        );
        let shown: Vec<(&str, Option<&str>, Option<&str>)> = m
            .iter()
            .map(|m| {
                (
                    m.kind.as_str(),
                    m.task_id.as_deref(),
                    m.answer_to.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("prompt", Some("p1"), None),
                ("ask", Some("p1"), None),
                ("text", Some("p1"), None),
                ("prompt", Some("pr-2"), Some("7c11d703")),
                ("text", Some("pr-2"), None)
            ]
        );
        assert_eq!(m[1].status.as_deref(), Some(crate::asks::ANSWERED));
        // Not answered: the same words typed are just a prompt.
        let m = normalize_with(
            "t",
            &events,
            std::slice::from_ref(&sent),
            &[Ask {
                answer: None,
                ..ask.clone()
            }],
        );
        assert!(m.iter().all(|m| m.answer_to.is_none()));
        // An answer kept before answers named their message: the first after its card that says it.
        let m = normalize_with(
            "t",
            &events,
            std::slice::from_ref(&sent),
            std::slice::from_ref(&ask),
        );
        assert_eq!(
            m.iter()
                .find(|m| m.task_id.as_deref() == Some("pr-2") && m.kind == "prompt")
                .unwrap()
                .answer_to
                .as_deref(),
            Some("7c11d703")
        );
        // A steer that took it (the turn still ran).
        let mut events = claude_turn();
        events.push(RawEvent::from_value(json!({"id": "s1", "type": "steer", "timestamp": 1_001_500, "taskId": "st-1", "data": {"message": text, "mode": "native"}})));
        let m = normalize_with("t", &events, &[], &[delivered("st-1")]);
        let s = m.iter().find(|m| m.kind == "steer").unwrap();
        assert_eq!(s.answer_to.as_deref(), Some("7c11d703"));
        // Codex's answered the same way.
        let mut events = codex_turn();
        events.push(RawEvent::from_value(json!({"id": "d825", "type": "prompt", "timestamp": 1793571422000_i64, "taskId": "d825", "data": {"prompt": "Which file name should I create?\nbeta.txt", "status": "finished"}})));
        let m = normalize_with(
            "t",
            &events,
            &[],
            &[Ask {
                asked: asked(SHOWN),
                answer: answer("d825"),
            }],
        );
        assert_eq!(
            m.iter()
                .find(|m| m.task_id.as_deref() == Some("d825"))
                .unwrap()
                .answer_to
                .as_deref(),
            Some("call_4e64")
        );
    }

    /// Flotilla's own messages are marked as its, whichever way they went: a prompt, a steer
    /// into a running turn (boat's, or still sending); yours aren't, nor is the agent's reply
    /// quoting one.
    #[test]
    fn flotillas_own_are_marked_as_a_prompt_or_a_steer() {
        let report = "[flotilla] Agents answered (1):\n\n### \"Sleep then ready\" (f72a1419-dbb3-47cd-a19c-d30eb2255730) finished:\n\nready";
        let answer = flotilla_model::orchestrate::wake_text(
            "c1",
            "Add tests",
            None,
            Some("Done."),
            "finished",
        );
        let mut events = claude_turn();
        events.push(RawEvent::from_value(json!({"id": "s1", "type": "steer", "timestamp": 1_001_500, "taskId": "st-1", "data": {"message": report, "mode": "native"}})));
        events.push(RawEvent::from_value(json!({"id": "s2", "type": "steer", "timestamp": 1_001_600, "taskId": "st-2", "data": {"message": "Use gamma.txt", "mode": "native"}})));
        events.push(RawEvent::from_value(json!({"id": "p2", "type": "prompt", "timestamp": 1_100_000, "taskId": "p2", "data": {"prompt": answer, "status": "running"}})));
        events.push(RawEvent::from_value(json!({"id": "r3", "type": "response", "timestamp": 1_100_500, "taskId": "p2", "data": {"content": report}})));
        let sending = SentPrompt {
            id: "local-1".into(),
            kind: "steer".into(),
            text: flotilla_model::orchestrate::couldnt_start_text("X", "c2", "no"),
            sent_at: "1970-01-01T00:18:21Z".into(),
            ..Default::default()
        };
        let m = normalize("t", &events, std::slice::from_ref(&sending));
        let ours: Vec<(&str, Option<&str>)> = m
            .iter()
            .filter(|m| m.from_flotilla)
            .map(|m| (m.kind.as_str(), m.task_id.as_deref()))
            .collect();
        assert_eq!(
            ours,
            [
                ("steer", Some("st-1")),
                ("prompt", Some("p2")),
                ("steer", Some("local-1"))
            ]
        );
        assert_eq!(
            m.iter()
                .filter(|m| !m.from_flotilla && matches!(m.kind.as_str(), "prompt" | "steer"))
                .count(),
            2,
            "{m:?}"
        );
    }

    /// The same words typed earlier, and the answer still sending, placed before its card (this
    /// machine's clock behind boat's): the answer's own message takes it, not the earlier one.
    #[test]
    fn an_earlier_message_saying_the_same_isnt_the_answer() {
        let ask = Ask {
            asked: flotilla_asked(),
            answer: Some(Answer {
                answers: vec![vec!["beta.txt".into()]],
                local: "local-1".into(),
            }),
        };
        let text =
            crate::asks::reply_text(&ask.asked.questions, &ask.answer.as_ref().unwrap().answers);
        let mut events = vec![RawEvent::from_value(
            json!({"id": "p0", "type": "prompt", "timestamp": 900_000, "taskId": "p0", "data": {"prompt": text, "status": "finished"}}),
        )];
        events.extend(claude_turn());
        let sending = SentPrompt {
            id: "local-1".into(),
            kind: "prompt".into(),
            text: text.clone(),
            sent_at: "1970-01-01T00:15:50Z".into(),
            ..Default::default()
        };
        let m = normalize_with(
            "t",
            &events,
            std::slice::from_ref(&sending),
            std::slice::from_ref(&ask),
        );
        let marked: Vec<(Option<&str>, i64)> = m
            .iter()
            .filter(|m| m.answer_to.is_some())
            .map(|m| (m.task_id.as_deref(), m.seq))
            .collect();
        let card = m.iter().find(|m| m.kind == "ask").unwrap().seq;
        assert_eq!(
            marked.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            [Some("local-1")]
        );
        let earlier = m
            .iter()
            .find(|m| m.task_id.as_deref() == Some("p0"))
            .unwrap()
            .seq;
        assert!(
            earlier < marked[0].1 && marked[0].1 < card,
            "the earlier one, then the answer, then its card"
        );
    }

    #[test]
    fn a_question_no_reply_shows_goes_in_its_turn() {
        // The reply isn't in yet (or shows it otherwise): the question still shows, in the
        // turn it was asked in, by when.
        let events = vec![
            codex_turn().remove(0),
            RawEvent::from_value(
                json!({"id": "x", "type": "response", "timestamp": 1793571146000_i64, "taskId": "f55b", "data": {"content": "Done asking."}}),
            ),
        ];
        let ask = Ask {
            asked: asked("text no reply has"),
            answer: None,
        };
        let m = normalize_with("t", &events, &[], std::slice::from_ref(&ask));
        assert_eq!(
            m.iter().map(|m| m.kind.as_str()).collect::<Vec<_>>(),
            ["prompt", "ask", "text"]
        );
        assert_eq!(m[1].task_id.as_deref(), Some("f55b"));
        assert_eq!(m.iter().map(|m| m.seq).collect::<Vec<_>>(), [0, 1, 2]);
        // In the middle of a reply: before, the question, after.
        let mid = vec![RawEvent::from_value(
            json!({"id": "r", "type": "response", "timestamp": 1793571145446_i64, "taskId": "f55b", "data": {"content": format!("Looking.\n{SHOWN}\nWaiting.")}}),
        )];
        let m = normalize_with(
            "t",
            &mid,
            &[],
            &[Ask {
                asked: asked(SHOWN),
                answer: None,
            }],
        );
        assert_eq!(
            m.iter()
                .map(|m| (m.kind.as_str(), m.text.clone().unwrap_or_default()))
                .collect::<Vec<_>>(),
            [
                ("text", "Looking.".into()),
                ("ask", "Which file name should I create?".into()),
                ("text", "Waiting.".into())
            ]
        );
    }
}
