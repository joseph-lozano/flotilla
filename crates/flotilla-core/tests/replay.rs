//! Offline tests over the M0 captures in `m0-samples/`.
//!
//! - Normalizer fixtures from the `*-readback.jsonl` files (CLI-wrapped: unwrap `.data`).
//! - Reconcile tests: rebuild boat's per-poll state from `*-live-poll.jsonl` (apply
//!   new/updated/gone in order), serve it through a fake boat with boat's cursor semantics,
//!   run the real `poll_thread`, and check the DB matches the server after every poll and
//!   the read-back at the end, with no partials and no ghosts.
//! - `rest-turn.jsonl`: `done` arrives before the prompt event goes terminal.

use flotilla_core::boat::{EventsPage, PromptRun};
use flotilla_core::event::{RawEvent, decode_cursor, encode_cursor, is_terminal_status};
use flotilla_core::normalize::{self, Message, PROMPT_NOT_KEPT};
use flotilla_core::store::{SentPrompt, Store, Thread, ThreadState};
use flotilla_core::sync::{LiveState, PollApi, poll_thread};
use flotilla_core::{ApiError, BoatError};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

const THREAD: &str = "11111111-2222-3333-4444-555555555555";

fn sample(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../m0-samples/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn lines(name: &str) -> Vec<Value> {
    sample(name)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn readback(name: &str) -> Vec<RawEvent> {
    lines(name)
        .into_iter()
        .map(|l| RawEvent::from_value(l["data"].clone()))
        .collect()
}

// ---- fake boat ----

#[derive(Default)]
struct FakeBoat {
    /// boat's current events, by id.
    events: RefCell<BTreeMap<String, Value>>,
    /// promptId → (status, done).
    runs: RefCell<HashMap<String, (String, bool)>>,
    page_limit: Option<u32>,
    prompt_status_calls: RefCell<usize>,
    after_turns: RefCell<Vec<String>>,
}

impl FakeBoat {
    fn sorted(&self) -> Vec<RawEvent> {
        let mut v: Vec<RawEvent> = self
            .events
            .borrow()
            .values()
            .cloned()
            .map(RawEvent::from_value)
            .collect();
        v.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
        v
    }

    /// Prompt runs follow the prompt events (done when terminal) unless set explicitly.
    fn runs_follow_events(&self) {
        let mut runs = self.runs.borrow_mut();
        for e in self.sorted().into_iter().filter(|e| e.is_prompt()) {
            let s = e.prompt_status().unwrap_or("running").to_string();
            runs.insert(
                e.task_id.clone().unwrap(),
                (s.clone(), is_terminal_status(&s)),
            );
        }
    }
}

impl PollApi for FakeBoat {
    fn events_page(
        &self,
        _: &str,
        _: &str,
        cursor: Option<&str>,
        limit: u32,
        inm: Option<&str>,
    ) -> Result<EventsPage, BoatError> {
        let all = self.sorted();
        // boat's cursor is an id lookup; an id it doesn't have is silently ignored.
        let start = cursor
            .and_then(decode_cursor)
            .and_then(|(_, id)| all.iter().position(|e| e.id == id))
            .map(|i| i + 1)
            .unwrap_or(0);
        let limit = self.page_limit.unwrap_or(limit) as usize;
        let page: Vec<RawEvent> = all[start..].iter().take(limit).cloned().collect();
        let has_more = start + page.len() < all.len();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for e in &page {
            e.raw.to_string().hash(&mut h);
        }
        (cursor, has_more).hash(&mut h);
        let etag = format!("W/\"{:x}\"", h.finish());
        if inm == Some(etag.as_str()) {
            return Ok(EventsPage::NotModified);
        }
        let next_cursor = if has_more {
            page.last().map(|e| encode_cursor(e.timestamp, &e.id))
        } else {
            None
        };
        Ok(EventsPage::Page {
            events: page,
            next_cursor,
            has_more,
            etag: Some(etag),
        })
    }

    fn prompt_status(&self, _: &str, prompt_id: &str) -> Result<PromptRun, BoatError> {
        *self.prompt_status_calls.borrow_mut() += 1;
        match self.runs.borrow().get(prompt_id) {
            Some((status, done)) => Ok(PromptRun {
                status: status.clone(),
                done: *done,
                conversation_id: None,
                error: None,
            }),
            None => Err(BoatError::Api(Box::new(ApiError {
                status: 404,
                code: "not_found".into(),
                message: "no such prompt".into(),
                details: Value::Null,
                request_id: None,
                retry_after_seconds: None,
            }))),
        }
    }

    fn after_turn(&self, _: &Store, thread_id: &str) -> flotilla_core::Result<()> {
        self.after_turns.borrow_mut().push(thread_id.to_string());
        Ok(())
    }
}

fn store_with_thread() -> Store {
    let store = Store::open_in_memory().unwrap();
    let now = flotilla_core::util::now_iso();
    store
        .insert_thread(&Thread {
            id: THREAD.into(),
            environment_id: None,
            environment_name: Some("base".into()),
            title: "test".into(),
            sandbox_id: Some("bx_test".into()),
            conversation_id: Some("conv".into()),
            provider: "claude-code".into(),
            model: "sonnet".into(),
            effort: Some("low".into()),
            fast: false,
            state: ThreadState::Running,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: now.clone(),
            updated_at: now,
            repo_project_id: None,
            branch: None,
            base_sha: None,
        })
        .unwrap();
    store
}

/// key → raw JSON of what the DB holds.
fn db_events(store: &Store) -> BTreeMap<String, Value> {
    store
        .events(THREAD)
        .unwrap()
        .into_iter()
        .map(|e| (e.key, e.event.raw))
        .collect()
}

/// key → raw JSON of what boat holds, minus partials (as the DB should see it).
fn server_events(fake: &FakeBoat) -> BTreeMap<String, Value> {
    fake.sorted()
        .into_iter()
        .filter(|e| !e.is_partial())
        .map(|e| (e.key(), e.raw))
        .collect()
}

/// Replay a live-poll capture, polling after each of the watcher's polls.
fn replay_live_poll(name: &str, page_limit: Option<u32>) -> (FakeBoat, Store, LiveState) {
    let fake = FakeBoat {
        page_limit,
        ..Default::default()
    };
    let store = store_with_thread();
    let mut live = LiveState::default();
    let records = lines(name);
    let mut i = 0;
    let mut saw_partial = false;
    while i < records.len() {
        let poll = records[i]["poll"].as_i64();
        while i < records.len() && records[i]["poll"].as_i64() == poll {
            let r = &records[i];
            let id = r["event"]["data"]["id"]
                .as_str()
                .or(r["id"].as_str())
                .unwrap()
                .to_string();
            match r["change"].as_str().unwrap() {
                "new" | "updated" => {
                    fake.events
                        .borrow_mut()
                        .insert(id, r["event"]["data"].clone());
                }
                "gone" => {
                    fake.events.borrow_mut().remove(&id);
                }
                other => panic!("unknown change {other}"),
            }
            i += 1;
        }
        fake.runs_follow_events();
        poll_thread(&fake, &store, THREAD, &mut live).unwrap();
        saw_partial |= !live.partials.is_empty();
        // After every poll the DB mirrors boat exactly: nothing stale, no ghosts, no partials.
        assert_eq!(
            db_events(&store),
            server_events(&fake),
            "{name}: DB differs from boat after poll {poll:?}"
        );
    }
    if name.starts_with("claude") {
        assert!(
            saw_partial,
            "the Claude capture streams partials; live state should have shown them"
        );
    }
    (fake, store, live)
}

fn by_key(events: &[RawEvent]) -> BTreeMap<String, Value> {
    events.iter().map(|e| (e.key(), e.raw.clone())).collect()
}

fn assert_settled(store: &Store) {
    let turns = store.turns(THREAD).unwrap();
    assert!(!turns.is_empty());
    assert!(
        turns.iter().all(|t| t.settled && t.done),
        "unsettled turns: {turns:?}"
    );
    assert!(store.events(THREAD).unwrap().iter().all(|e| e.settled));
    let (ts, id) = store.cursor_event(THREAD).unwrap().unwrap();
    assert!(!id.starts_with("qp-"));
    assert_eq!(
        store.sync_state(THREAD).unwrap().cursor,
        Some(encode_cursor(ts, &id))
    );
    let msgs = store.messages(THREAD).unwrap();
    assert!(msgs.iter().all(|m| !m.pending), "pending messages remain");
}

// ---- reconcile tests ----

#[test]
fn claude_turn_reconciles_to_readback() {
    for limit in [None, Some(3)] {
        let (fake, store, _) = replay_live_poll("claude-turn-live-poll.jsonl", limit);
        assert_eq!(
            db_events(&store),
            by_key(&readback("claude-turn-readback.jsonl"))
        );
        assert_settled(&store);
        assert_eq!(
            fake.after_turns.borrow().len(),
            1,
            "after-turn hook runs once for a turn seen running"
        );
    }
}

#[test]
fn codex_turn_reconciles_to_readback() {
    for limit in [None, Some(2)] {
        let (_, store, _) = replay_live_poll("codex-turn-live-poll.jsonl", limit);
        assert_eq!(
            db_events(&store),
            by_key(&readback("codex-turn-readback.jsonl"))
        );
        assert_settled(&store);
    }
}

#[test]
fn steer_turn_reconciles_to_readback() {
    for limit in [None, Some(4)] {
        let (fake, store, _) = replay_live_poll("steer-turn-live-poll.jsonl", limit);
        // The capture's first poll holds the earlier Claude turn, then the steered turn.
        let mut want = by_key(&readback("claude-turn-readback.jsonl"));
        want.extend(by_key(&readback("steer-turn-readback.jsonl")));
        assert_eq!(db_events(&store), want);
        assert_settled(&store);
        // The earlier turn was first seen finished: settled without asking boat, no hook.
        assert_eq!(fake.after_turns.borrow().len(), 1);
    }
}

#[test]
fn cursor_moves_only_when_settled_and_304s_after() {
    let fake = FakeBoat::default();
    let store = store_with_thread();
    let mut live = LiveState::default();
    let records = lines("claude-turn-live-poll.jsonl");
    // Up to the first response: the turn is running, so the cursor must not move.
    for r in records.iter().take(2) {
        fake.events.borrow_mut().insert(
            r["event"]["data"]["id"].as_str().unwrap().into(),
            r["event"]["data"].clone(),
        );
    }
    fake.runs_follow_events();
    let o = poll_thread(&fake, &store, THREAD, &mut live).unwrap();
    assert!(o.active && o.changed);
    assert_eq!(store.sync_state(THREAD).unwrap().cursor, None);
    // Nothing changed: 304.
    let o = poll_thread(&fake, &store, THREAD, &mut live).unwrap();
    assert!(o.not_modified && !o.changed);

    // Finish the turn from the read-back.
    fake.events.borrow_mut().clear();
    for e in readback("claude-turn-readback.jsonl") {
        fake.events.borrow_mut().insert(e.id.clone(), e.raw.clone());
    }
    fake.runs_follow_events();
    let o = poll_thread(&fake, &store, THREAD, &mut live).unwrap();
    assert!(!o.active && o.settled_turns.len() == 1);
    assert!(store.sync_state(THREAD).unwrap().cursor.is_some());
    // An empty page after the cursor, then 304s from then on.
    let o = poll_thread(&fake, &store, THREAD, &mut live).unwrap();
    assert!(!o.not_modified && !o.changed);
    let o = poll_thread(&fake, &store, THREAD, &mut live).unwrap();
    assert!(o.not_modified);
    assert_eq!(fake.after_turns.borrow().len(), 1);
}

#[test]
fn rebuild_from_boat_needs_no_prompt_status_calls() {
    let fake = FakeBoat::default();
    let mut all = readback("claude-turn-readback.jsonl");
    all.extend(readback("steer-turn-readback.jsonl"));
    all.extend(readback("interrupt-turn-readback.jsonl"));
    for e in &all {
        fake.events.borrow_mut().insert(e.id.clone(), e.raw.clone());
    }
    let store = store_with_thread();
    let mut live = LiveState::default();
    let o = poll_thread(&fake, &store, THREAD, &mut live).unwrap();
    assert_eq!(o.settled_turns.len(), 3);
    assert_eq!(*fake.prompt_status_calls.borrow(), 0);
    assert!(
        fake.after_turns.borrow().is_empty(),
        "no hook for turns this process never saw running"
    );
    assert_settled(&store);
    // Rebuilt without sent_prompts: the steered turn's prompt shows "not kept".
    let msgs = store.messages(THREAD).unwrap();
    assert_eq!(
        msgs.iter()
            .filter(|m| m.kind == "prompt" && m.text.as_deref() == Some(PROMPT_NOT_KEPT))
            .count(),
        1
    );
}

/// `rest-turn.jsonl` (B = full re-read, C = prompt status) as synthetic events: `done`
/// arrives before the prompt event is terminal, so the turn mustn't settle on `done` alone.
#[test]
fn rest_turn_waits_for_terminal_prompt_event() {
    const TASK: &str = "af9da954-7e3e-483e-bcdb-eadcadd9e09c";
    let fake = FakeBoat::default();
    let store = store_with_thread();
    let mut live = LiveState::default();
    let records = lines("rest-turn.jsonl");
    let turn_start = 1793287836300i64;
    let synth = |r: &Value| -> Value {
        let ts = r["ts"].as_i64().unwrap();
        let id = r["id"].as_str().unwrap();
        let kind = r["type"].as_str().unwrap();
        let mut data = json!({"is_reverted": false});
        if let Some(s) = r["status"].as_str() {
            data["status"] = json!(s);
        }
        if r["streaming"].as_bool() == Some(true) {
            data["is_streaming"] = json!(true);
        }
        if let Some(p) = r["pend"].as_array().filter(|p| !p.is_empty()) {
            data["tools"] = Value::Array(
                p.iter()
                    .enumerate()
                    .map(|(n, pending)| {
                        let content = json!({"type": "bash_output", "pending": pending, "command": format!("cmd{n}")}).to_string();
                        json!({"use": {"id": format!("{id}-{n}"), "name": "Bash", "input": {}}, "result": {"content": content, "is_error": false}})
                    })
                    .collect(),
            );
        }
        let task = if ts >= turn_start {
            TASK.to_string()
        } else if kind == "prompt" {
            id.to_string()
        } else {
            "older".into()
        };
        json!({"id": id, "type": kind, "timestamp": ts, "taskId": task, "conversationId": "conv", "data": data})
    };
    // boat has the prompt run from the moment POST /prompt returns.
    fake.runs
        .borrow_mut()
        .insert(TASK.into(), ("queued".into(), false));
    let mut i = 0;
    let mut settled_at_done_only = false;
    while i < records.len() {
        let r = &records[i];
        match r["kind"].as_str().unwrap() {
            "B" => {
                // One re-read's records are logged within a few ms of each other.
                let t = r["t"].as_f64().unwrap();
                while i < records.len()
                    && records[i]["kind"] == "B"
                    && records[i]["t"].as_f64().unwrap() - t < 0.05
                {
                    let r = &records[i];
                    let id = r["id"].as_str().unwrap().to_string();
                    if r["change"] == "gone" {
                        fake.events.borrow_mut().remove(&id);
                    } else {
                        fake.events.borrow_mut().insert(id, synth(r));
                    }
                    i += 1;
                }
            }
            "C" => {
                fake.runs.borrow_mut().insert(
                    TASK.into(),
                    (
                        r["status"].as_str().unwrap().into(),
                        r["done"].as_bool().unwrap(),
                    ),
                );
                i += 1;
            }
            _ => {
                i += 1;
                continue;
            }
        }
        // Older turns are already over.
        for e in fake
            .sorted()
            .into_iter()
            .filter(|e| e.is_prompt() && e.task_id.as_deref() != Some(TASK))
        {
            fake.runs
                .borrow_mut()
                .insert(e.task_id.clone().unwrap(), ("finished".into(), true));
        }
        let o = poll_thread(&fake, &store, THREAD, &mut live).unwrap();
        let run_done = fake.runs.borrow().get(TASK).is_some_and(|r| r.1);
        let event_terminal = store
            .event(THREAD, TASK)
            .unwrap()
            .and_then(|e| e.prompt_status().map(is_terminal_status))
            == Some(true);
        if run_done && !event_terminal && o.settled_turns.iter().any(|t| t == TASK) {
            settled_at_done_only = true;
        }
        assert_eq!(db_events(&store), server_events(&fake));
    }
    assert!(
        !settled_at_done_only,
        "the turn settled on `done` before its prompt event was terminal"
    );
    let db = db_events(&store);
    assert!(
        !db.values()
            .any(|e| e["id"].as_str().unwrap().starts_with("qp-")),
        "qp- ghost"
    );
    assert!(
        !db.values()
            .any(|e| e["id"] == "19c7c36b-21d6-419c-8d94-792f49de5873"),
        "partial ghost"
    );
    assert_eq!(db[TASK]["data"]["status"], "finished");
    assert_settled(&store);
    assert_eq!(fake.after_turns.borrow().len(), 1);
}

// ---- normalizer fixtures ----

fn kinds(msgs: &[Message]) -> Vec<&str> {
    msgs.iter().map(|m| m.kind.as_str()).collect()
}

#[test]
fn normalizes_claude_tool_turn() {
    let msgs = normalize::normalize(THREAD, &readback("claude-turn-readback.jsonl"), &[]);
    assert_eq!(msgs[0].kind, "prompt");
    assert!(
        msgs[0]
            .text
            .as_deref()
            .unwrap()
            .starts_with("This is a test of an event format.")
    );
    assert_eq!(msgs[0].status.as_deref(), Some("finished"));
    let tools: Vec<&Message> = msgs.iter().filter(|m| m.kind == "tool").collect();
    assert_eq!(tools.len(), 11);
    assert!(tools.iter().all(|t| !t.pending));
    let by_name = |n: &str| {
        tools
            .iter()
            .filter(|t| t.tool_name.as_deref() == Some(n))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        by_name("Write")[0].tool_summary.as_deref(),
        Some("/home/user/flotilla-m0/notes.txt")
    );
    assert_eq!(
        by_name("Read")[0].tool_result_type.as_deref(),
        Some("file_content")
    );
    assert_eq!(
        by_name("Edit")[0].tool_result_type.as_deref(),
        Some("file_edit")
    );
    let failed: Vec<_> = by_name("Bash").into_iter().filter(|t| t.is_error).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].tool_summary.as_deref(),
        Some("ls -la ~/flotilla-m0 && false")
    );
    // Text then its `-tools` sibling, final summary last.
    assert_eq!(msgs.last().unwrap().kind, "text");
    assert!(
        msgs.last()
            .unwrap()
            .text
            .as_deref()
            .unwrap()
            .starts_with("All steps completed.")
    );
    assert_eq!(
        msgs.last().unwrap().model.as_deref(),
        Some("claude-sonnet-5")
    );
    assert!(msgs.windows(2).all(|w| w[0].seq + 1 == w[1].seq));
}

#[test]
fn normalizes_steered_turn() {
    let events = readback("steer-turn-readback.jsonl");
    let msgs = normalize::normalize(THREAD, &events, &[]);
    assert_eq!(
        kinds(&msgs),
        ["prompt", "tool", "steer", "tool", "tool", "text"]
    );
    assert_eq!(msgs[0].text.as_deref(), Some(PROMPT_NOT_KEPT));
    assert_eq!(msgs[2].status.as_deref(), Some("native-continued"));
    assert!(msgs[2].text.as_deref().unwrap().contains("PINEAPPLE"));

    // On the machine that sent it, the real prompt text is shown.
    let sent = vec![
        SentPrompt {
            id: "5e41194e-c067-4099-a744-6f87da024037".into(),
            kind: "prompt".into(),
            text: "Run these with Bash as three separate tool calls".into(),
            sent_at: "2026-10-29T15:12:44.700Z".into(),
            ..Default::default()
        },
        SentPrompt {
            id: "e998c111-64c1-4bc3-aca0-0cc60a49e73d".into(),
            kind: "steer".into(),
            text: "typed steer".into(),
            sent_at: "2026-10-29T15:12:56.000Z".into(),
            ..Default::default()
        },
    ];
    let msgs = normalize::normalize(THREAD, &events, &sent);
    assert_eq!(
        msgs[0].text.as_deref(),
        Some("Run these with Bash as three separate tool calls")
    );
    assert_eq!(msgs[2].text.as_deref(), Some("typed steer"));
    assert_eq!(msgs.len(), 6, "local rows don't duplicate boat's");
}

#[test]
fn steer_before_any_response_stays_with_its_turn() {
    // A steer rewrite moves the prompt event after the steer; with no earlier responses the
    // turn would otherwise start after the steer.
    let mut events = readback("claude-turn-readback.jsonl");
    let steer_turn: Vec<RawEvent> = readback("steer-turn-readback.jsonl")
        .into_iter()
        .filter(|e| e.id != "b7b6926a-7a17-44d8-824b-02ba4a60b794")
        .collect();
    events.extend(steer_turn);
    let msgs = normalize::normalize(THREAD, &events, &[]);
    let steer = msgs.iter().position(|m| m.kind == "steer").unwrap();
    let not_kept = msgs
        .iter()
        .position(|m| m.text.as_deref() == Some(PROMPT_NOT_KEPT))
        .unwrap();
    assert!(
        steer > not_kept,
        "steer should follow its own turn's prompt: {:?}",
        kinds(&msgs)
    );
}

#[test]
fn normalizes_interrupt_and_codex() {
    let msgs = normalize::normalize(THREAD, &readback("interrupt-turn-readback.jsonl"), &[]);
    assert_eq!(kinds(&msgs), ["prompt"]);
    assert_eq!(msgs[0].status.as_deref(), Some("interrupted"));

    let msgs = normalize::normalize(THREAD, &readback("codex-turn-readback.jsonl"), &[]);
    assert_eq!(
        kinds(&msgs),
        ["prompt", "tool", "tool", "tool", "tool", "tool", "text"]
    );
    assert!(
        msgs.iter()
            .filter(|m| m.kind == "tool")
            .all(|m| m.tool_name.as_deref() == Some("Bash"))
    );
    assert_eq!(msgs.iter().filter(|m| m.is_error).count(), 1);
}

#[test]
fn tools_left_pending_by_an_interrupt_are_not_pending() {
    // Live M1 run: the interrupted turn kept its in-flight Bash call with pending:true.
    let content =
        json!({"type": "bash_output", "pending": true, "command": "sleep 60"}).to_string();
    let events = vec![
        RawEvent::from_value(
            json!({"id": "p", "type": "prompt", "timestamp": 1, "taskId": "t", "data": {"prompt": "x", "status": "interrupted"}}),
        ),
        RawEvent::from_value(
            json!({"id": "r", "type": "response", "timestamp": 2, "taskId": "t", "data": {"content": "", "tools": [
                {"use": {"id": "u", "name": "Bash", "input": {"command": "sleep 60"}}, "result": {"content": content, "is_error": false}}
            ]}}),
        ),
    ];
    let msgs = normalize::normalize(THREAD, &events, &[]);
    assert_eq!(kinds(&msgs), ["prompt", "tool"]);
    assert!(!msgs[1].pending);
    assert_eq!(msgs[1].status.as_deref(), Some("interrupted"));
}

#[test]
fn cursor_waits_for_a_pending_tool_boat_fills_in_later() {
    // Live M1 run: an interrupted turn kept its Bash call pending, and boat filled in the
    // result during the next turn, after the interrupted turn had settled.
    let fake = FakeBoat::default();
    let store = store_with_thread();
    let mut live = LiveState::default();
    let pending =
        json!({"type": "bash_output", "pending": true, "command": "sleep 60"}).to_string();
    let resolved =
        json!({"type": "bash_output", "pending": false, "command": "sleep 60", "error": true})
            .to_string();
    let tool = |content: &str, is_error: bool| {
        json!({"id": "r1", "type": "response", "timestamp": 20, "taskId": "t1", "conversationId": "conv", "data": {"content": "", "tools": [
            {"use": {"id": "u", "name": "Bash", "input": {"command": "sleep 60"}}, "result": {"content": content, "is_error": is_error}}
        ]}})
    };
    let insert = |v: Value| {
        fake.events
            .borrow_mut()
            .insert(v["id"].as_str().unwrap().into(), v);
    };
    insert(
        json!({"id": "p0", "type": "prompt", "timestamp": 1, "taskId": "t0", "conversationId": "conv", "data": {"prompt": "a", "status": "finished"}}),
    );
    insert(
        json!({"id": "p1", "type": "prompt", "timestamp": 10, "taskId": "t1", "conversationId": "conv", "data": {"prompt": "b", "status": "interrupted"}}),
    );
    insert(tool(&pending, false));
    fake.runs_follow_events();
    let o = poll_thread(&fake, &store, THREAD, &mut live).unwrap();
    assert!(!o.active);
    // Settled, but the cursor sits before the pending call, on the interrupted turn's prompt.
    assert_eq!(
        store.sync_state(THREAD).unwrap().cursor,
        Some(encode_cursor(10, "p1"))
    );

    // The next turn: boat resolves the old call.
    insert(tool(&resolved, true));
    insert(
        json!({"id": "p2", "type": "prompt", "timestamp": 30, "taskId": "t2", "conversationId": "conv", "data": {"prompt": "c", "status": "finished"}}),
    );
    fake.runs_follow_events();
    poll_thread(&fake, &store, THREAD, &mut live).unwrap();
    assert_eq!(db_events(&store), server_events(&fake));
    assert_eq!(
        store.sync_state(THREAD).unwrap().cursor,
        Some(encode_cursor(30, "p2"))
    );
    let tool_msg = store
        .messages(THREAD)
        .unwrap()
        .into_iter()
        .find(|m| m.kind == "tool")
        .unwrap();
    assert!(tool_msg.is_error && !tool_msg.pending);
}

#[test]
fn unknown_events_become_notices_and_local_sends_show_immediately() {
    let events = vec![RawEvent::from_value(json!({
        "id": "u1", "type": "usage_limit", "timestamp": 5, "taskId": "t1", "data": {"message": "limit hit"}
    }))];
    let sent = vec![SentPrompt {
        id: "p2".into(),
        kind: "prompt".into(),
        text: "hello".into(),
        sent_at: "2026-10-29T15:00:00.000Z".into(),
        ..Default::default()
    }];
    let msgs = normalize::normalize(THREAD, &events, &sent);
    assert_eq!(kinds(&msgs), ["notice", "prompt"]);
    assert_eq!(msgs[0].text.as_deref(), Some("usage_limit: limit hit"));
    assert_eq!(msgs[1].status.as_deref(), Some("sending"));
}

#[test]
fn prompts_show_their_files_while_sending_and_from_boat() {
    let file = |name: &str| flotilla_core::attach::Attached {
        path: format!("/tmp/{name}").into(),
        name: name.into(),
        size: 1,
        kind: flotilla_core::attach::Kind::of_name(name),
        blob: String::new(),
        from: None,
    };
    // While sending: the names come from what this Mac sent.
    let sent = vec![SentPrompt {
        id: "local-1".into(),
        kind: "prompt".into(),
        text: "look".into(),
        sent_at: "2026-10-29T15:00:00.000Z".into(),
        attachments: vec![file("red.png"), file("note.txt")],
        ..Default::default()
    }];
    let msgs = normalize::normalize(THREAD, &[], &sent);
    assert_eq!(msgs[0].attachments, ["red.png", "note.txt"]);

    // From boat's event (on a rebuilt database too), without the files' data.
    let events = vec![RawEvent::from_value(json!({
        "id": "e1", "type": "prompt", "timestamp": 5, "taskId": "t1",
        "data": {"prompt": "look", "status": "finished", "attachments": [
            {"filename": "red.png", "mimeType": "image/png", "base64Data": "iVBORw0K"}]}
    }))];
    let msgs = normalize::normalize(THREAD, &events, &[]);
    assert_eq!(kinds(&msgs), ["prompt"]);
    assert_eq!(msgs[0].attachments, ["red.png"]);

    // Round trip through the store, where the data never lands.
    let store = store_with_thread();
    store.apply_events(THREAD, &events, true).unwrap();
    store
        .insert_sent_prompt(THREAD, "t1", "prompt", "look")
        .unwrap();
    store
        .set_sent_attachments(THREAD, "t1", &[file("red.png")])
        .unwrap();
    store.rebuild_messages(THREAD).unwrap();
    assert_eq!(store.messages(THREAD).unwrap()[0].attachments, ["red.png"]);
    assert_eq!(
        store.sent_prompts(THREAD).unwrap()[0].attachments,
        [file("red.png")]
    );
    let raw: String = store
        .conn()
        .query_row("SELECT raw_json FROM events", [], |r| r.get(0))
        .unwrap();
    assert!(!raw.contains("base64Data"));
}

// ---- DB ----

#[test]
fn migrations_are_idempotent_and_settings_are_immutable() {
    let dir = std::env::temp_dir().join(format!("flotilla-test-{}", uuid::Uuid::new_v4()));
    let path = dir.join("t.db");
    {
        let s = Store::open(&path).unwrap();
        drop(s);
    }
    let s = Store::open(&path).unwrap();
    let v: i64 = s
        .conn()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(v, 16);
    drop(s);

    let store = store_with_thread();
    for sql in [
        "UPDATE threads SET model='opus' WHERE id=?1",
        "UPDATE threads SET provider='codex' WHERE id=?1",
        "UPDATE threads SET effort=NULL WHERE id=?1",
        "UPDATE threads SET fast=1 WHERE id=?1",
    ] {
        let err = store.conn().execute(sql, [THREAD]).unwrap_err();
        assert!(
            err.to_string().contains("fixed at thread creation"),
            "{sql}: {err}"
        );
    }
    // Same values and other columns are fine.
    store
        .conn()
        .execute(
            "UPDATE threads SET model='sonnet', title='x' WHERE id=?1",
            [THREAD],
        )
        .unwrap();
    let mut t = store.thread(THREAD).unwrap().unwrap();
    t.title = "renamed".into();
    store.update_thread(&t).unwrap();
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn threads_group_by_activity() {
    use flotilla_core::store::Activity;
    let store = store_with_thread();
    let activity = |store: &Store| store.thread_summaries().unwrap()[0].activity;
    // Synced from boat, never shown here, no turns: idle.
    assert_eq!(activity(&store), Activity::Idle);
    // A turn starts: working.
    store.insert_turn(THREAD, "t1", "queued").unwrap();
    assert_eq!(activity(&store), Activity::Working);
    // Viewed, then the turn finishes with events newer than the view: waiting on you.
    store.mark_viewed(THREAD).unwrap();
    let mut t = store.turns(THREAD).unwrap().remove(0);
    t.settled = true;
    t.done = true;
    store.update_turn(&t).unwrap();
    let future = flotilla_core::util::now_ms() + 60_000;
    let e = RawEvent::from_value(
        json!({"id": "r", "type": "response", "timestamp": future, "taskId": "t1", "data": {"content": "done"}}),
    );
    store.apply_events(THREAD, &[e], true).unwrap();
    assert_eq!(activity(&store), Activity::Unread);
    // The sidebar previews the latest reply's first line.
    store.rebuild_messages(THREAD).unwrap();
    assert_eq!(
        store.thread_summaries().unwrap()[0].last_reply.as_deref(),
        Some("done")
    );
    // A summary of that reply replaces it; one of an older reply doesn't.
    let (key, _) = store.latest_reply(THREAD).unwrap().unwrap();
    store
        .set_summary(THREAD, "reply", &key, "Finished the task")
        .unwrap();
    assert_eq!(
        store.thread_summaries().unwrap()[0].last_reply.as_deref(),
        Some("Finished the task")
    );
    store
        .set_summary(THREAD, "reply", "an-older-reply", "Stale")
        .unwrap();
    assert_eq!(
        store.thread_summaries().unwrap()[0].last_reply.as_deref(),
        Some("done")
    );
    // Opening it reads it; "mark as unread" brings it back.
    store.mark_viewed(THREAD).unwrap();
    assert_eq!(
        activity(&store),
        Activity::Unread,
        "events newer than now stay unread"
    );
    store.mark_unread(THREAD).unwrap();
    assert_eq!(activity(&store), Activity::Unread);
    // Archived wins over unread.
    store.set_archived(THREAD, true).unwrap();
    assert_eq!(activity(&store), Activity::Archived);
    store.set_archived(THREAD, false).unwrap();
    assert_eq!(activity(&store), Activity::Unread);
    // Expired wins over everything.
    let mut th = store.thread(THREAD).unwrap().unwrap();
    th.state = ThreadState::Expired;
    store.update_thread(&th).unwrap();
    assert_eq!(activity(&store), Activity::Expired);
}

#[test]
fn messages_table_rebuilds_when_version_changes() {
    let dir = std::env::temp_dir().join(format!("flotilla-test-{}", uuid::Uuid::new_v4()));
    let path = dir.join("t.db");
    {
        let store = Store::open(&path).unwrap();
        let now = flotilla_core::util::now_iso();
        store
            .insert_thread(&Thread {
                id: THREAD.into(),
                environment_id: None,
                environment_name: None,
                title: "t".into(),
                sandbox_id: None,
                conversation_id: None,
                provider: "codex".into(),
                model: "gpt-5.6-terra".into(),
                effort: None,
                fast: false,
                state: ThreadState::Idle,
                keep_running: false,
                archive_after: None,
                last_stopped_at: None,
                data_expired_at: None,
                created_at: now.clone(),
                updated_at: now,
                repo_project_id: None,
                branch: None,
                base_sha: None,
            })
            .unwrap();
        store
            .apply_events(THREAD, &readback("codex-turn-readback.jsonl"), true)
            .unwrap();
        store.conn().execute("DROP TABLE messages", []).unwrap();
        store
            .conn()
            .execute_batch("CREATE TABLE messages (thread_id TEXT, seq INTEGER)")
            .unwrap();
        store.kv_set("messages_version", "0").unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.messages(THREAD).unwrap().len(), 7);
    std::fs::remove_dir_all(dir).ok();
}

/// A row read before another thread saved the conversation id (as the VM state refresh did
/// while the first prompt was sent) must not erase it when written back. One-field setters
/// leave the rest of the row alone.
#[test]
fn stale_thread_writes_keep_ids_and_setters_touch_one_field() {
    let store = store_with_thread();
    let mut stale = store.thread(THREAD).unwrap().unwrap();
    stale.sandbox_id = None;
    stale.conversation_id = None;
    stale.state = ThreadState::Idle;
    store.update_thread(&stale).unwrap();
    let t = store.thread(THREAD).unwrap().unwrap();
    assert_eq!(
        (
            t.sandbox_id.as_deref(),
            t.conversation_id.as_deref(),
            t.state
        ),
        (Some("bx_test"), Some("conv"), ThreadState::Idle)
    );

    store.set_thread_title(THREAD, "renamed").unwrap();
    store.set_keep_running(THREAD, true).unwrap();
    store
        .set_thread_state(THREAD, ThreadState::Running)
        .unwrap();
    store
        .set_archive_after(THREAD, Some("2026-10-29T21:00:00Z"))
        .unwrap();
    // Already set: a thread keeps its first conversation.
    store.set_conversation_id(THREAD, "other").unwrap();
    let t = store.thread(THREAD).unwrap().unwrap();
    assert_eq!(t.title, "renamed");
    assert!(t.keep_running);
    assert_eq!(t.state, ThreadState::Running);
    assert_eq!(t.archive_after.as_deref(), Some("2026-10-29T21:00:00Z"));
    assert_eq!(t.conversation_id.as_deref(), Some("conv"));
    assert_eq!(t.effort.as_deref(), Some("low"));
}

/// A message shows from its placeholder the moment Send is pressed (before the VM resumes),
/// then the placeholder takes boat's prompt id; a failed send's placeholder is dropped.
#[test]
fn a_placeholder_shows_a_send_then_takes_its_id_or_goes() {
    let store = store_with_thread();
    store
        .insert_sent_prompt(THREAD, "local-1", "prompt", "run the tests")
        .unwrap();
    store.rebuild_messages(THREAD).unwrap();
    let msgs = store.messages(THREAD).unwrap();
    assert_eq!(
        (kinds(&msgs), msgs[0].status.as_deref()),
        (vec!["prompt"], Some("sending"))
    );
    assert_eq!(msgs[0].text.as_deref(), Some("run the tests"));

    // boat took it as a prompt (the placeholder guessed a steer here, and is corrected).
    store
        .insert_sent_prompt(THREAD, "local-2", "steer", "and then lint")
        .unwrap();
    store
        .record_sent_prompt(THREAD, Some("local-1"), "p1", "prompt", "run the tests")
        .unwrap();
    store
        .record_sent_prompt(THREAD, Some("local-2"), "p2", "prompt", "and then lint")
        .unwrap();
    let sent = store.sent_prompts(THREAD).unwrap();
    assert_eq!(
        sent.iter()
            .map(|s| (s.id.as_str(), s.kind.as_str()))
            .collect::<Vec<_>>(),
        [("p1", "prompt"), ("p2", "prompt")]
    );
    // Without a placeholder it's a plain insert.
    store
        .record_sent_prompt(THREAD, None, "p3", "steer", "faster")
        .unwrap();
    assert_eq!(store.sent_prompts(THREAD).unwrap().len(), 3);

    store
        .insert_sent_prompt(THREAD, "local-4", "prompt", "this one fails")
        .unwrap();
    store.delete_sent_prompt(THREAD, "local-4").unwrap();
    store.rebuild_messages(THREAD).unwrap();
    assert!(
        store
            .messages(THREAD)
            .unwrap()
            .iter()
            .all(|m| m.text.as_deref() != Some("this one fails"))
    );
}

/// A start that failed before boat made a VM keeps the thread, its prompt (unsent) and the
/// reason; trying again rebuilds the request from what was stored.
#[test]
fn a_failed_start_keeps_what_a_retry_needs() {
    let store = Store::open_in_memory().unwrap();
    let now = flotilla_core::util::now_iso();
    store
        .insert_thread(&Thread {
            id: THREAD.into(),
            environment_id: Some("env_1".into()),
            environment_name: Some("base".into()),
            title: "Summarise the README".into(),
            sandbox_id: None,
            conversation_id: None,
            provider: "claude-code".into(),
            model: "opus-5-5".into(),
            effort: Some("high".into()),
            fast: true,
            state: ThreadState::Error,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: now.clone(),
            updated_at: now,
            repo_project_id: None,
            branch: None,
            base_sha: None,
        })
        .unwrap();
    store
        .insert_sent_prompt(
            THREAD,
            "local-1",
            "prompt",
            "Summarise the README in one line",
        )
        .unwrap();
    store
        .set_start_error(THREAD, Some("boat 401 unauthorized"))
        .unwrap();
    assert_eq!(
        store.start_error(THREAD).unwrap().as_deref(),
        Some("boat 401 unauthorized")
    );

    let (req, local) = flotilla_core::lifecycle::retry_request(&store, THREAD).unwrap();
    assert_eq!(local, "local-1");
    assert_eq!(
        (
            req.environment.as_str(),
            req.provider.as_str(),
            req.model.as_str()
        ),
        ("base", "claude-code", "opus-5-5")
    );
    assert_eq!(
        (req.effort.as_deref(), req.fast, req.title.as_deref()),
        (Some("high"), true, Some("Summarise the README"))
    );
    assert_eq!(req.prompt, "Summarise the README in one line");

    // Discarding removes the reason with the thread.
    store.delete_thread(THREAD).unwrap();
    assert_eq!(store.start_error(THREAD).unwrap(), None);
}

// ---- what a quit left behind (Core::reconcile_after_quit) ----

/// "This run started": everything in the tests is from before it.
const LATER: &str = "2999-01-01T00:00:00.000Z";

fn reconcile(
    store: Store,
    synced: &[&str],
) -> (
    flotilla_core::lifecycle::Core,
    flotilla_core::lifecycle::Reconciled,
) {
    let mut core = flotilla_core::lifecycle::Core::new(
        flotilla_core::boat::BoatClient::new("test".into()).unwrap(),
        store,
    );
    let synced = synced.iter().map(|s| s.to_string()).collect();
    let r = core
        .reconcile_after_quit(LATER, &synced, &Default::default())
        .unwrap();
    (core, r)
}

fn prompt_event(id: &str, task: &str, text: &str) -> RawEvent {
    let now = flotilla_core::util::iso_to_ms(&flotilla_core::util::now_iso()).unwrap();
    RawEvent::from_value(
        json!({"id": id, "type": "prompt", "timestamp": now, "taskId": task, "data": {"prompt": text, "status": "finished"}}),
    )
}

fn ids(store: &Store) -> Vec<String> {
    store
        .sent_prompts(THREAD)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect()
}

#[test]
fn a_start_cut_short_before_its_vm_gets_try_again_with_its_prompt() {
    // A thread whose VM was never made (update_thread can't clear a VM id: a fresh store).
    let mut t = store_with_thread().thread(THREAD).unwrap().unwrap();
    (t.sandbox_id, t.conversation_id, t.state) = (None, None, ThreadState::Creating);
    let store = Store::open_in_memory().unwrap();
    store.insert_thread(&t).unwrap();
    store
        .insert_sent_prompt(THREAD, "local-1", "prompt", "hello")
        .unwrap();
    let (core, r) = reconcile(store, &[]);
    assert_eq!(r.failed_starts, [THREAD]);
    assert!(core.store.start_error(THREAD).unwrap().is_some());
    assert_eq!(
        core.store.thread(THREAD).unwrap().unwrap().state,
        ThreadState::Error
    );
    // Kept for Try again, under its placeholder id.
    assert_eq!(ids(&core.store), ["local-1"]);
}

#[test]
fn a_start_cut_short_after_boat_got_the_prompt_is_finished() {
    let store = store_with_thread();
    store.set_start_stages(THREAD, Some("{}")).unwrap();
    store
        .insert_sent_prompt(THREAD, "local-1", "prompt", "fix the bug")
        .unwrap();
    store
        .apply_events(
            THREAD,
            &[prompt_event("ev-1", "task-1", "fix the bug")],
            true,
        )
        .unwrap();
    let (core, r) = reconcile(store, &[THREAD]);
    assert!(r.failed_starts.is_empty() && r.not_sent.is_empty());
    assert!(core.store.start_stages(THREAD).unwrap().is_none());
    assert!(core.store.start_error(THREAD).unwrap().is_none());
    // The placeholder took boat's id, so Try again (never offered) couldn't send it twice.
    assert_eq!(ids(&core.store), ["task-1"]);
}

#[test]
fn a_thread_whose_history_this_sync_didnt_read_is_left_for_later() {
    let store = store_with_thread();
    store.set_start_stages(THREAD, Some("{}")).unwrap();
    store
        .insert_sent_prompt(THREAD, "local-1", "prompt", "hello")
        .unwrap();
    let (core, r) = reconcile(store, &[]);
    assert!(r.looked_at.is_empty() && r.failed_starts.is_empty() && r.not_sent.is_empty());
    assert_eq!(ids(&core.store), ["local-1"]);
    assert!(core.store.start_stages(THREAD).unwrap().is_some());
}

#[test]
fn a_repeated_message_isnt_taken_for_the_one_before_it() {
    let store = store_with_thread();
    // "continue" went through earlier (boat's id task-1); a second "continue" was sending when
    // Flotilla quit, and boat never got it.
    store
        .record_sent_prompt(THREAD, None, "task-1", "prompt", "continue")
        .unwrap();
    store
        .apply_events(THREAD, &[prompt_event("ev-1", "task-1", "continue")], true)
        .unwrap();
    store
        .insert_sent_prompt(THREAD, "local-2", "prompt", "continue")
        .unwrap();
    let (core, r) = reconcile(store, &[THREAD]);
    assert_eq!(r.not_sent, [THREAD]);
    assert_eq!(ids(&core.store), ["task-1", "unsent-2"]);
}

#[test]
fn a_message_boat_never_got_stays_in_place_marked_not_sent() {
    let store = store_with_thread();
    store
        .record_sent_prompt(THREAD, None, "task-1", "prompt", "start")
        .unwrap();
    store
        .apply_events(THREAD, &[prompt_event("ev-1", "task-1", "start")], true)
        .unwrap();
    store
        .insert_sent_prompt(THREAD, "local-2", "prompt", "check the logs")
        .unwrap();
    // A resume script Flotilla was waiting on.
    let run = json!({"sandbox": "bx_test", "pid": 7, "started_at": flotilla_core::util::now_iso(), "state": "running", "exit_code": null, "output": "hi"});
    store
        .kv_set(&format!("resume:{THREAD}"), &run.to_string())
        .unwrap();
    let (core, r) = reconcile(store, &[THREAD]);
    assert_eq!(r.not_sent, [THREAD]);
    // Not moved to the composer, not deleted: shown where it was, "not sent".
    assert_eq!(
        core.store
            .kv_get(&flotilla_core::lifecycle::unsent_key(THREAD))
            .unwrap(),
        None
    );
    let shown = core.store.messages(THREAD).unwrap();
    let last = shown.iter().rfind(|m| m.kind == "prompt").unwrap();
    assert_eq!(
        (last.text.as_deref(), last.status.as_deref()),
        (Some("check the logs"), Some(normalize::NOT_SENT))
    );
    let hook =
        flotilla_core::agents::hook_run(&core.store, flotilla_core::agents::Hook::Resume, THREAD)
            .unwrap()
            .unwrap();
    assert_eq!(hook.state, "failed");
}

#[test]
fn a_message_isnt_taken_for_a_longer_one_that_starts_the_same() {
    let store = store_with_thread();
    store
        .record_sent_prompt(THREAD, None, "task-0", "prompt", "start")
        .unwrap();
    store
        .apply_events(THREAD, &[prompt_event("ev-0", "task-0", "start")], true)
        .unwrap();
    // Boat has "continue with tests" from another send it never told us the id of; the local
    // "continue" is a different message.
    store
        .apply_events(
            THREAD,
            &[prompt_event("ev-1", "task-1", "continue with tests")],
            true,
        )
        .unwrap();
    store
        .insert_sent_prompt(THREAD, "local-2", "prompt", "continue")
        .unwrap();
    let (core, r) = reconcile(store, &[THREAD]);
    assert_eq!(r.not_sent, [THREAD]);
    assert_eq!(ids(&core.store), ["task-0", "unsent-2"]);
}

#[test]
fn a_thread_boat_already_has_prompts_for_isnt_a_failed_start() {
    // Imported from boat: its prompts are in boat's history but not in this Mac's records.
    let store = store_with_thread();
    store
        .apply_events(
            THREAD,
            &[prompt_event("ev-0", "task-0", "set up the repo")],
            true,
        )
        .unwrap();
    store
        .insert_sent_prompt(THREAD, "local-2", "prompt", "now add tests")
        .unwrap();
    let (core, r) = reconcile(store, &[THREAD]);
    assert!(r.failed_starts.is_empty());
    assert!(core.store.start_error(THREAD).unwrap().is_none());
    assert_eq!(r.not_sent, [THREAD]);
    assert_eq!(ids(&core.store), ["unsent-2"]);
}

#[test]
fn a_steer_needs_its_exact_text_even_next_to_wrapper_words() {
    let store = store_with_thread();
    store
        .record_sent_prompt(THREAD, None, "task-0", "prompt", "start")
        .unwrap();
    store
        .apply_events(THREAD, &[prompt_event("ev-0", "task-0", "start")], true)
        .unwrap();
    // An unowned steer that merely contains boat's wrapper words isn't the pending "continue".
    let now = flotilla_core::util::iso_to_ms(&flotilla_core::util::now_iso()).unwrap();
    let steer = RawEvent::from_value(
        json!({"id": "ev-1", "type": "steer", "timestamp": now, "taskId": "task-1", "data": {"message": "Explain The user's instruction: continue"}}),
    );
    store.apply_events(THREAD, &[steer], true).unwrap();
    store
        .insert_sent_prompt(THREAD, "local-2", "steer", "continue")
        .unwrap();
    let (core, r) = reconcile(store, &[THREAD]);
    assert_eq!(r.not_sent, [THREAD]);
    assert!(ids(&core.store).contains(&"unsent-2".to_string()));
}

#[test]
fn an_answer_whose_message_never_went_lets_its_question_wait_again() {
    use flotilla_core::asks::{self, Answer, Asked};
    use flotilla_proto::ask::{Choice, Question};
    let store = store_with_thread();
    let now = flotilla_core::util::now_ms();
    store
        .record_sent_prompt(THREAD, None, "task-1", "prompt", "ask me")
        .unwrap();
    store.apply_events(THREAD, &[RawEvent::from_value(json!({"id": "ev-1", "type": "prompt", "timestamp": now - 60_000, "taskId": "task-1", "data": {"prompt": "ask me", "status": "finished"}}))], true).unwrap();
    let q = Question {
        header: None,
        question: "Which one?".into(),
        options: vec![Choice {
            label: "a".into(),
            description: None,
        }],
        multi: false,
    };
    asks::store(
        &store,
        THREAD,
        &[Asked {
            id: "call_1".into(),
            at_ms: now - 50_000,
            text: String::new(),
            tool: asks::CODEX_TOOL.into(),
            questions: vec![q],
            prompt: Some("ask me".into()),
            followed: false,
            call: None,
            turn: None,
        }],
    )
    .unwrap();
    // Answered, and Flotilla quit before boat took the message.
    asks::set_answer(
        &store,
        THREAD,
        "call_1",
        Answer {
            answers: vec![vec!["a".into()]],
            local: "local-2".into(),
        },
    )
    .unwrap();
    store
        .insert_sent_prompt(THREAD, "local-2", "prompt", "Which one?\na")
        .unwrap();
    let (core, r) = reconcile(store, &[THREAD]);
    // Not a "not sent" message: the question waits for an answer again.
    assert!(r.not_sent.is_empty());
    assert_eq!(ids(&core.store), ["task-1"]);
    assert!(asks::answers(&core.store, THREAD).is_empty());
    let ask = core
        .store
        .messages(THREAD)
        .unwrap()
        .into_iter()
        .find(|m| m.kind == asks::KIND)
        .unwrap();
    assert_eq!(ask.status.as_deref(), Some(asks::WAITING));
}
