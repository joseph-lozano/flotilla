//! Agents started in a thread's own VM (`flotilla agent`): each another boat conversation in
//! the same sandbox, on any harness and model, sharing the thread's files. Flotilla follows each
//! one's events and, once every agent started in one turn of the thread has answered, sends the
//! thread all their answers in one message.

use crate::boat::EventsPage;
use crate::error::{BoatError, Result};
use crate::event::{RawEvent, encode_cursor, is_terminal_status};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Longest answer passed on per agent.
const MAX_ANSWER: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Side {
    /// The request id (`flotilla agent` printed it).
    pub id: String,
    pub title: String,
    pub provider: String,
    pub model: String,
    pub task: String,
    /// The thread's turn that asked for it: the agents asked for in one turn answer together.
    pub batch: String,
    /// Its conversation in the VM, once boat took the prompt.
    #[serde(default)]
    pub conversation: Option<String>,
    /// The prompt boat took for its latest task or follow-up (its turn's `taskId`): what it
    /// answers is that turn's.
    #[serde(default)]
    pub turn: Option<String>,
    /// `starting`, `running`, `finished`, `failed`, `interrupted`.
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub calls: usize,
    #[serde(default)]
    pub latest: Option<(String, String)>,
    #[serde(default)]
    pub answer: Option<String>,
    pub started_ms: i64,
    #[serde(default)]
    pub ended_ms: Option<i64>,
    /// Its answer has gone to the thread.
    #[serde(default)]
    pub reported: bool,
}

impl Side {
    pub fn running(&self) -> bool {
        matches!(self.status.as_str(), "starting" | "running")
    }

    /// Take in what its conversation's events say now: its latest turn's calls, the latest of
    /// them while it runs, and how it ended and what it answered.
    pub fn follow(&mut self, events: &[RawEvent], now_ms: i64) {
        let messages = crate::normalize::normalize(&self.id, events, &[]);
        // Its own turn (one kept from before turns were: the latest). Boat shows a prompt it
        // took only as a placeholder at first, which isn't read: until it shows it, the turn
        // before isn't this one, and it's still to run.
        let from = match &self.turn {
            Some(turn) => messages
                .iter()
                .rposition(|m| m.kind == "prompt" && m.task_id.as_ref() == Some(turn)),
            None => messages.iter().rposition(|m| m.kind == "prompt"),
        };
        if self.turn.is_some() && from.is_none() {
            (self.status, self.calls, self.latest) = ("running".into(), 0, None);
            return;
        }
        let status = from
            .and_then(|i| messages[i].status.clone())
            .unwrap_or_else(|| "running".into());
        let turn = from.map_or(&messages[..], |i| &messages[i + 1..]);
        let turn = &turn[..turn
            .iter()
            .position(|m| m.kind == "prompt")
            .unwrap_or(turn.len())];
        let calls: Vec<_> = turn.iter().filter(|m| m.kind == "tool").collect();
        self.calls = calls.len();
        if is_terminal_status(&status) {
            self.status = status;
            self.latest = None;
            self.answer = turn
                .iter()
                .rev()
                .find(|m| m.kind == "text")
                .and_then(|m| m.text.clone());
            self.ended_ms.get_or_insert(now_ms);
        } else {
            self.status = "running".into();
            self.latest = calls.last().map(|m| {
                (
                    m.tool_name.clone().unwrap_or_default(),
                    m.tool_summary.clone().unwrap_or_default(),
                )
            });
        }
    }

    fn how(&self) -> &str {
        match self.status.as_str() {
            "interrupted" => "was stopped",
            "failed" => "failed",
            _ => "finished",
        }
    }
}

/// An agent's events read so far, when they run past one read's pages.
#[derive(Default)]
pub struct Reading {
    cursor: Option<String>,
    events: HashMap<String, RawEvent>,
}

/// Read on through an agent's events, `max` pages at most: all of them, oldest first, once the
/// end is reached (and `reading` starts over), else `None`, `reading` kept to go on from (after
/// an error too, which is passed on).
pub fn read_events(
    reading: &mut Reading,
    max: usize,
    mut page: impl FnMut(Option<&str>) -> std::result::Result<EventsPage, BoatError>,
) -> std::result::Result<Option<Vec<RawEvent>>, BoatError> {
    for _ in 0..max {
        let EventsPage::Page {
            events,
            next_cursor,
            has_more,
            ..
        } = page(reading.cursor.as_deref())?
        else {
            return Ok(None);
        };
        // On from the last event that can't vanish: boat starts over for a cursor naming a gone
        // one. Streaming text and queued placeholders aren't its record.
        let next = events
            .iter()
            .rev()
            .find(|e| !e.is_partial() && !e.is_queued_placeholder())
            .map(|e| encode_cursor(e.timestamp, &e.id))
            .or(next_cursor);
        for e in events
            .into_iter()
            .filter(|e| !e.is_partial() && !e.is_queued_placeholder())
        {
            reading.events.insert(e.key(), e);
        }
        match next.filter(|c| has_more && reading.cursor.as_ref() != Some(c)) {
            Some(c) => reading.cursor = Some(c),
            None => {
                let mut all: Vec<RawEvent> = std::mem::take(reading).events.into_values().collect();
                all.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
                return Ok(Some(all));
            }
        }
    }
    Ok(None)
}

/// What a started agent is told around its task.
pub fn prompt(task: &str) -> String {
    format!(
        "{}\n\n(Another agent in this VM asked you this; your final message goes back to it. Do it yourself: don't hand it on.)",
        task.trim_end()
    )
}

fn key(thread: &str, id: &str) -> String {
    format!("side:{thread}:{id}")
}

pub fn save(store: &Store, thread: &str, side: &Side) -> Result<()> {
    let (k, v) = row(thread, side)?;
    store.kv_set(&k, &v)
}

/// The `kv` entry `save` writes, to write with others at once.
pub fn row(thread: &str, side: &Side) -> Result<(String, String)> {
    Ok((key(thread, &side.id), serde_json::to_string(side)?))
}

/// A thread's agents, oldest first.
pub fn of(store: &Store, thread: &str) -> Vec<Side> {
    let mut out: Vec<Side> = store
        .kv_with_prefix(&key(thread, ""))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_str(&v).ok())
        .collect();
    out.sort_by_key(|s| s.started_ms);
    out
}

/// Every thread's agents still at work, with their thread.
pub fn running(store: &Store) -> Vec<(String, Side)> {
    store
        .kv_with_prefix("side:")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| {
            let thread = k.strip_prefix("side:")?.split(':').next()?.to_string();
            let side: Side = serde_json::from_str(&v).ok()?;
            side.running().then_some((thread, side))
        })
        .collect()
}

/// Batches (thread, batch) with an agent not yet reported and none still at work.
pub fn unreported(store: &Store) -> Vec<(String, String)> {
    let mut all: std::collections::BTreeMap<(String, String), Vec<Side>> =
        std::collections::BTreeMap::new();
    for (k, v) in store.kv_with_prefix("side:").unwrap_or_default() {
        let Some(thread) = k.strip_prefix("side:").and_then(|r| r.split(':').next()) else {
            continue;
        };
        let Ok(side) = serde_json::from_str::<Side>(&v) else {
            continue;
        };
        all.entry((thread.to_string(), side.batch.clone()))
            .or_default()
            .push(side);
    }
    all.into_iter()
        .filter(|(_, sides)| !sides.iter().any(Side::running) && sides.iter().any(|s| !s.reported))
        .map(|(k, _)| k)
        .collect()
}

/// A batch whose agents have all answered, and not yet been reported: the message that tells
/// the thread, and the agents to mark reported. `None` while any is still at work.
pub fn report(sides: &[Side], batch: &str) -> Option<(String, Vec<String>)> {
    let members: Vec<&Side> = sides.iter().filter(|s| s.batch == batch).collect();
    if members.is_empty()
        || members.iter().any(|s| s.running())
        || members.iter().all(|s| s.reported)
    {
        return None;
    }
    let mut text = format!("[flotilla] Agents answered ({}):", members.len());
    for s in &members {
        match s.answer.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
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
                text.push_str(&format!(
                    "\n\n### \"{}\" ({}) {}:\n\n{a}",
                    s.title,
                    s.id,
                    s.how()
                ));
            }
            None => {
                let why = s
                    .error
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "\n\n### \"{}\" ({}) {} without an answer{why}.",
                    s.title,
                    s.id,
                    s.how()
                ));
            }
        }
    }
    Some((text, members.iter().map(|s| s.id.clone()).collect()))
}

pub use flotilla_model::side::parse_report;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn side(id: &str, batch: &str, status: &str) -> Side {
        Side {
            id: id.into(),
            title: format!("Review {id}"),
            provider: "codex".into(),
            model: "gpt-6.1-sol".into(),
            task: "Review the diff".into(),
            batch: batch.into(),
            conversation: Some(format!("conv-{id}")),
            turn: None,
            status: status.into(),
            error: None,
            calls: 0,
            latest: None,
            answer: None,
            started_ms: 1,
            ended_ms: None,
            reported: false,
        }
    }

    fn ev(id: &str, ts: i64, kind: &str, data: serde_json::Value) -> RawEvent {
        RawEvent::from_value(
            json!({"id": id, "type": kind, "timestamp": ts, "taskId": "t1", "conversationId": "c", "data": data}),
        )
    }

    #[test]
    fn follows_a_conversation() {
        let mut s = side("a", "b1", "starting");
        let mut events = vec![
            ev(
                "p",
                1,
                "prompt",
                json!({"prompt": "Review", "status": "running"}),
            ),
            // As boat sends a call: a response carrying it, its result pending.
            ev(
                "u",
                2,
                "response",
                json!({"content": "", "tools": [{
                    "use": {"id": "x", "name": "Bash", "type": "tool_use", "input": {"command": "git diff"}},
                    "result": {"tool_use_id": "x", "type": "tool_result", "content": "{\"pending\":true}"}
                }]}),
            ),
        ];
        s.follow(&events, 10);
        assert_eq!((s.status.as_str(), s.calls), ("running", 1));
        assert!(s.latest.is_some());
        events[0] = ev(
            "p",
            1,
            "prompt",
            json!({"prompt": "Review", "status": "finished"}),
        );
        events.push(ev(
            "r",
            3,
            "response",
            json!({"content": "Looks right. 8/10."}),
        ));
        s.follow(&events, 20);
        assert_eq!(s.status, "finished");
        assert_eq!(s.answer.as_deref(), Some("Looks right. 8/10."));
        assert_eq!((s.latest.clone(), s.ended_ms), (None, Some(20)));
    }

    /// A follow-up's turn is the prompt boat took for it. While boat shows that prompt only as
    /// its queued placeholder (which isn't read), the turn before's answer isn't its answer.
    #[test]
    fn a_follow_up_is_answered_by_its_own_turn() {
        let at = |id: &str, ts: i64, kind: &str, task: &str, data: serde_json::Value| {
            RawEvent::from_value(
                json!({"id": id, "type": kind, "timestamp": ts, "taskId": task, "conversationId": "c", "data": data}),
            )
        };
        let mut events = vec![
            at(
                "p1",
                1,
                "prompt",
                "t1",
                json!({"prompt": "Review", "status": "finished"}),
            ),
            at("r1", 2, "response", "t1", json!({"content": "OLD ANSWER"})),
            at(
                "qp-t2",
                3,
                "prompt",
                "t2",
                json!({"prompt": "And the docs", "status": "queued"}),
            ),
        ];
        let read = |events: &[RawEvent]| {
            let page = |_: Option<&str>| {
                Ok(EventsPage::Page {
                    events: events.to_vec(),
                    next_cursor: None,
                    has_more: false,
                    etag: None,
                })
            };
            read_events(&mut Reading::default(), 1, page)
                .unwrap()
                .unwrap()
        };
        // As `side_follow_up` leaves it: running again, in a new batch, on boat's prompt t2.
        let mut s = side("a", "b2", "running");
        s.turn = Some("t2".into());
        // Kept, and read back (a restart): it still knows its turn.
        let store = Store::open_in_memory().unwrap();
        save(&store, "t", &s).unwrap();
        let mut s = of(&store, "t").remove(0);
        s.follow(&read(&events), 10);
        assert_eq!((s.status.as_str(), s.answer.as_deref()), ("running", None));
        assert_eq!(report(std::slice::from_ref(&s), "b2"), None);
        // Its turn shows, and runs.
        events[2] = at(
            "p2",
            4,
            "prompt",
            "t2",
            json!({"prompt": "And the docs", "status": "running"}),
        );
        s.follow(&read(&events), 20);
        assert_eq!((s.status.as_str(), s.answer.as_deref()), ("running", None));
        // And ends: its own answer.
        events[2] = at(
            "p2",
            4,
            "prompt",
            "t2",
            json!({"prompt": "And the docs", "status": "finished"}),
        );
        events.push(at(
            "r2",
            5,
            "response",
            "t2",
            json!({"content": "NEW ANSWER"}),
        ));
        s.follow(&read(&events), 30);
        assert_eq!(
            (s.status.as_str(), s.answer.as_deref(), s.ended_ms),
            ("finished", Some("NEW ANSWER"), Some(30))
        );
        // One kept from before turns were (none): its latest prompt, as then.
        let mut old = side("o", "b1", "running");
        old.follow(&read(&events[..2]), 40);
        assert_eq!(
            (old.status.as_str(), old.answer.as_deref()),
            ("finished", Some("OLD ANSWER"))
        );
    }

    #[test]
    fn a_batch_reports_once_all_have_answered() {
        let mut a = side("a", "b1", "finished");
        a.answer = Some("Fine. 9/10".into());
        let b = side("b", "b1", "running");
        let other = side("c", "b2", "finished");
        assert_eq!(report(&[a.clone(), b.clone(), other.clone()], "b1"), None);
        let mut b = b;
        b.status = "failed".into();
        b.error = Some("no key for that model".into());
        let (text, ids) = report(&[a.clone(), b.clone(), other], "b1").unwrap();
        assert_eq!(ids, ["a", "b"]);
        assert!(text.starts_with(
            "[flotilla] Agents answered (2):\n\n### \"Review a\" (a) finished:\n\nFine. 9/10"
        ));
        assert!(
            text.ends_with("### \"Review b\" (b) failed without an answer: no key for that model.")
        );
        assert_eq!(
            parse_report(&text),
            Some(vec![("Review a", "a"), ("Review b", "b")])
        );
        assert_eq!(
            parse_report(&crate::util::one_line(&text)),
            Some(vec![("Review a", "a"), ("Review b", "b")])
        );
        a.reported = true;
        b.reported = true;
        assert_eq!(report(&[a, b], "b1"), None);
    }

    #[test]
    fn a_long_conversation_is_read_over_several_reads() {
        // 25 pages of 2 events; 10 pages a read.
        let events: Vec<RawEvent> = (0..50)
            .map(|i| ev(&format!("e{i:02}"), i, "response", json!({"content": "x"})))
            .collect();
        let page = |cursor: Option<&str>| {
            let from = cursor.map_or(0, |c| {
                events
                    .iter()
                    .position(|e| encode_cursor(e.timestamp, &e.id) == c)
                    .unwrap()
                    + 1
            });
            let to = (from + 2).min(events.len());
            Ok(EventsPage::Page {
                events: events[from..to].to_vec(),
                next_cursor: None,
                has_more: to < events.len(),
                etag: None,
            })
        };
        let mut reading = Reading::default();
        assert!(read_events(&mut reading, 10, page).unwrap().is_none());
        assert!(read_events(&mut reading, 10, page).unwrap().is_none());
        let all = read_events(&mut reading, 10, page).unwrap().unwrap();
        assert_eq!(all.len(), 50);
        assert_eq!((all[0].id.as_str(), all[49].id.as_str()), ("e00", "e49"));
        // And the next read starts from the top again.
        assert!(reading.cursor.is_none() && reading.events.is_empty());
    }

    #[test]
    fn kept_per_thread() {
        let store = Store::open_in_memory().unwrap();
        save(&store, "t1", &side("a", "b1", "running")).unwrap();
        save(&store, "t1", &side("b", "b1", "finished")).unwrap();
        save(&store, "t2", &side("c", "b9", "running")).unwrap();
        assert_eq!(of(&store, "t1").len(), 2);
        let mut run: Vec<_> = running(&store)
            .into_iter()
            .map(|(t, s)| (t, s.id))
            .collect();
        run.sort();
        assert_eq!(
            run,
            [
                ("t1".to_string(), "a".to_string()),
                ("t2".to_string(), "c".to_string())
            ]
        );
        assert_eq!(unreported(&store), []);
        let mut c = side("c", "b9", "finished");
        save(&store, "t2", &c).unwrap();
        assert_eq!(unreported(&store), [("t2".to_string(), "b9".to_string())]);
        c.reported = true;
        save(&store, "t2", &c).unwrap();
        assert_eq!(unreported(&store), []);
    }
}
