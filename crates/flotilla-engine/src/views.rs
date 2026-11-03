//! Views computed from the store: what each client screen shows (see `flotilla_proto::views`).
//! Pure reads; the host decides when to compute them.

use flotilla_core::agents::{self, Hook};
use flotilla_core::store::{Activity, Store, ThreadState, ThreadSummary};
use flotilla_proto::{
    Catalog, ChildCard, ChildState, EngineState, GhStatus, HistoryPage, Mentions, ProjectDetail,
    ProjectRow, Settings, Sidebar, SummaryStatus, SyncStatus, ThreadDetail, ThreadProject,
    ThreadSettings, Usage,
};
use std::collections::BTreeMap;

/// How many of a project's setup runs its page lists.
pub const PROJECT_RUNS: usize = 20;

/// What the sidebar needs that isn't in the store: the engine's own state.
#[derive(Debug, Clone)]
pub struct Live {
    pub status: SyncStatus,
    pub github: GhStatus,
    pub usage: Option<Usage>,
    pub engine: EngineState,
}

/// Every thread's row, newest activity first, and who handed whom work.
pub fn sidebar(store: &Store, live: &Live) -> flotilla_core::Result<Sidebar> {
    let threads = store.thread_summaries()?;
    let parents = flotilla_core::orchestrate::parents(store);
    let mut waiting: Vec<String> = flotilla_core::orchestrate::waiting_parents(store, &parents)
        .into_iter()
        .collect();
    waiting.sort();
    Ok(Sidebar {
        threads,
        parents: parents.into_iter().collect(),
        waiting,
        projects: store.projects()?,
        environments: store.user_environments()?,
        status: live.status.clone(),
        github: live.github.clone(),
        usage: live.usage.clone(),
        engine: live.engine.clone(),
        limits: flotilla_core::meter::all_limits(store),
    })
}

/// One thread's page. `summaries` and `parents` are the sidebar's (for its family's cards).
pub fn thread(
    store: &Store,
    id: &str,
    summaries: &[ThreadSummary],
    parents: &BTreeMap<String, String>,
) -> flotilla_core::Result<ThreadDetail> {
    let thread = store.thread(id)?;
    let Some(t) = thread.as_ref() else {
        return Ok(ThreadDetail {
            id: id.to_string(),
            thread: None,
            messages: vec![],
            older: 0,
            pages: vec![],
            sent: vec![],
            start_error: None,
            stages: None,
            project: None,
            resume: None,
            archive: None,
            setup_failure: None,
            services: None,
            public: vec![],
            lineage: None,
            parent: None,
            children: vec![],
            context: None,
        });
    };
    let project = match &t.repo_project_id {
        Some(pid) => store.project(pid)?.map(|p| {
            // Only a count for the base the thread has now.
            let behind = store
                .behind(id)
                .ok()
                .flatten()
                .filter(|(b, _, _)| Some(b) == t.base_sha.as_ref())
                .map(|(_, _, n)| n);
            let into = flotilla_core::lifecycle::base_branch(store, id, &p.default_branch);
            ThreadProject {
                project: p,
                behind,
                into,
            }
        }),
        None => None,
    };
    let title_of = |id: &str| {
        summaries
            .iter()
            .find(|s| s.thread.id == id)
            .map(|s| s.thread.title.clone())
    };
    let parent = parents
        .get(id)
        .and_then(|p| title_of(p).map(|title| (p.clone(), title)));
    let mut children: Vec<ChildCard> = summaries
        .iter()
        .filter(|s| parents.get(&s.thread.id).map(String::as_str) == Some(id))
        .map(|s| child_card(store, s))
        .collect();
    children.extend(flotilla_core::side::of(store, id).iter().map(side_card));
    // Only its newest whole pages, cut to size, and every page's mark (see
    // `flotilla_proto::history`): the marks come from the store without reading the messages.
    let marks = store.message_marks(id)?;
    let older = flotilla_proto::history::window_start(marks.len() as i64);
    let messages = store
        .messages_between(id, older, i64::MAX)?
        .iter()
        .map(flotilla_proto::history::cap)
        .collect();
    Ok(ThreadDetail {
        id: id.to_string(),
        messages,
        older,
        pages: flotilla_proto::history::page_marks(&marks),
        sent: store.sent_prompts(id)?,
        start_error: store.start_error(id)?,
        stages: store
            .start_stages(id)?
            .and_then(|j| serde_json::from_str(&j).ok()),
        project,
        resume: agents::hook_run(store, Hook::Resume, id)?,
        archive: agents::hook_run(store, Hook::Archive, id)?,
        setup_failure: flotilla_core::setup::thread_setup_failure(store, id)?,
        services: flotilla_core::services::state(store, id)?,
        public: flotilla_core::public::shown(store, t),
        lineage: flotilla_core::lineage::stored(store, id),
        context: flotilla_core::meter::stored_context(store, id),
        parent,
        children,
        thread,
    })
}

/// A thread another handed work to, as that one's card shows it.
fn child_card(store: &Store, s: &ThreadSummary) -> ChildCard {
    let t = &s.thread;
    let state = match (s.activity, t.state) {
        _ if s.start_failed => ChildState::DidntStart,
        (Activity::Expired, _) => ChildState::Gone,
        (_, ThreadState::Creating | ThreadState::Starting) => ChildState::Starting,
        (Activity::Working, _) => ChildState::Working,
        (_, ThreadState::Error) => ChildState::Error,
        _ => ChildState::Done,
    };
    let running = state.running();
    // Its own messages: the task it was handed (its first prompt), and its latest turn's calls,
    // the latest of them, its answer and how long it took.
    let messages = store.messages(&t.id).unwrap_or_default();
    let task = messages
        .iter()
        .find(|m| m.kind == "prompt")
        .and_then(|m| m.text.as_deref())
        .map(|text| {
            flotilla_core::orchestrate::task_of(text)
                .unwrap_or(text)
                .to_string()
        })
        .unwrap_or_default();
    let from = messages
        .iter()
        .rposition(|m| m.kind == "prompt")
        .map_or(0, |i| i + 1);
    let turn = &messages[from..];
    let calls: Vec<_> = turn.iter().filter(|m| m.kind == "tool").collect();
    let latest = calls.last().filter(|_| running).map(|m| {
        (
            m.tool_name.clone().unwrap_or_default(),
            m.tool_summary.clone().unwrap_or_default(),
        )
    });
    let answer = turn
        .iter()
        .rev()
        .find(|m| m.kind == "text")
        .and_then(|m| m.text.clone())
        .filter(|_| !running);
    // Timed as its live timer is (the summary's `turn_started_ms`): from when its latest prompt
    // was sent, before any VM boots for it, to when its turn was seen to end, both by this
    // server's clock. A turn not sent from here (a rebuilt database) goes by its messages' times.
    let (started, ended) = match sent_turn(store, &t.id) {
        Some((a, b)) => (Some(a), b),
        None => (
            messages
                .get(from.saturating_sub(1))
                .and_then(|m| m.timestamp),
            turn.iter().filter_map(|m| m.timestamp).max(),
        ),
    };
    let duration_ms = match (started, ended) {
        (Some(a), Some(b)) if !running && b > a => Some((b - a) as u64),
        _ => None,
    };
    ChildCard {
        id: t.id.clone(),
        title: t.title.clone(),
        provider: t.provider.clone(),
        model: t.model.clone(),
        state,
        task,
        calls: calls.len(),
        latest,
        answer,
        started_ms: s.turn_started_ms.or(started),
        duration_ms,
        thread: true,
    }
}

/// The thread's latest turn, if its prompt was sent from here: when it began (sent, or first
/// seen, whichever came first) and when it was seen to end (none while it runs), in ms.
fn sent_turn(store: &Store, thread: &str) -> Option<(i64, Option<i64>)> {
    let turn = store.turns(thread).ok()?.pop()?;
    let sent = store
        .sent_prompts(thread)
        .ok()?
        .into_iter()
        .find(|p| p.id == turn.prompt_id)?;
    let ms = flotilla_core::util::iso_to_ms;
    let began = ms(&sent.sent_at)?.min(ms(&turn.created_at).unwrap_or(i64::MAX));
    Some((began, turn.done_at.as_deref().and_then(ms)))
}

/// An agent a thread started in its own VM, as its card shows it.
fn side_card(s: &flotilla_core::side::Side) -> ChildCard {
    let state = match s.status.as_str() {
        "starting" => ChildState::Starting,
        "running" => ChildState::Working,
        "failed" => ChildState::Failed,
        "interrupted" => ChildState::Stopped,
        _ => ChildState::Done,
    };
    ChildCard {
        id: s.id.clone(),
        title: s.title.clone(),
        provider: s.provider.clone(),
        model: s.model.clone(),
        state,
        task: s.task.clone(),
        calls: s.calls,
        latest: s.latest.clone(),
        answer: s.answer.clone().or_else(|| s.error.clone()),
        started_ms: Some(s.started_ms),
        duration_ms: s.ended_ms.map(|e| (e - s.started_ms).max(0) as u64),
        thread: false,
    }
}

/// One page of a thread's older messages, cut as its view's are, with its mark. None if the
/// thread has no such page.
pub fn history(store: &Store, id: &str, page: i64) -> flotilla_core::Result<Option<HistoryPage>> {
    use flotilla_proto::history::{PAGE, message_mark, page_mark};
    if page < 0 || store.thread(id)?.is_none() {
        return Ok(None);
    }
    let messages = store.messages_between(id, page * PAGE, (page + 1) * PAGE)?;
    if messages.is_empty() {
        return Ok(None);
    }
    let mark = page_mark(page, messages.iter().map(|m| (m.seq, message_mark(m))));
    Ok(Some(HistoryPage {
        thread: id.to_string(),
        page,
        mark,
        messages: messages.iter().map(flotilla_proto::history::cap).collect(),
    }))
}

/// A cut string whole: the message at `seq`'s `field` if it's still the one with `mark`, else
/// a page either side of it (a message inserted or gone before it moves it a little). Never the
/// whole thread: this runs on the host's thread.
pub fn uncut(
    store: &Store,
    id: &str,
    seq: i64,
    field: &str,
    mark: &str,
) -> flotilla_core::Result<Option<String>> {
    use flotilla_proto::history::{PAGE, uncut};
    if let Some(m) = store
        .messages_between(id, seq, seq.saturating_add(1))?
        .first()
        && let Some(text) = uncut(m, field, mark)
    {
        return Ok(Some(text.to_string()));
    }
    let near =
        store.messages_between(id, seq.saturating_sub(PAGE), seq.saturating_add(PAGE + 1))?;
    Ok(near
        .iter()
        .find_map(|m| uncut(m, field, mark).map(str::to_string)))
}

/// A project's page.
pub fn project(store: &Store, id: &str) -> flotilla_core::Result<ProjectDetail> {
    Ok(ProjectDetail {
        id: id.to_string(),
        project: store.project(id)?,
        runs: store.setup_runs(id, PROJECT_RUNS)?,
        scripts: store.scripts(id)?,
        github_error: github_error(store, id),
    })
}

/// Why GitHub couldn't be read for a project last time.
pub fn github_error(store: &Store, project: &str) -> Option<String> {
    store
        .kv_get(&format!("github_error:{project}"))
        .ok()
        .flatten()
        .filter(|e| !e.is_empty())
}

/// `kv` keys of the defaults for new threads.
pub const DEFAULTS_KEYS: [&str; 4] = [
    "default_provider",
    "default_model",
    "default_effort",
    "default_fast",
];

/// Your settings. `boat_key`: the saved key's last four characters; `doctor`: the setup
/// checks as last run.
pub fn settings(
    store: &Store,
    boat_key: Option<String>,
    doctor: Option<flotilla_proto::Doctor>,
) -> flotilla_core::Result<Settings> {
    let get = |k: &str| store.kv_get(k).ok().flatten();
    let defaults = (|| {
        Some(ThreadSettings {
            provider: get(DEFAULTS_KEYS[0])?,
            model: get(DEFAULTS_KEYS[1])?,
            effort: get(DEFAULTS_KEYS[2]).filter(|e| !e.is_empty()),
            fast: get(DEFAULTS_KEYS[3]).as_deref() == Some("true"),
        })
    })();
    let summaries = match get(crate::SUMMARY_STATUS_KEY)
        .as_deref()
        .and_then(|v| v.split_once(':'))
    {
        Some(("ok", at)) => SummaryStatus::Working(at.to_string()),
        Some(("error", e)) => SummaryStatus::Failed(e.to_string()),
        _ => SummaryStatus::Unknown,
    };
    let projects = store
        .projects()?
        .into_iter()
        .map(|p| ProjectRow {
            last_run: store
                .setup_runs(&p.id, 1)
                .ok()
                .and_then(|r| r.into_iter().next()),
            project: p,
        })
        .collect();
    Ok(Settings {
        defaults,
        idle_minutes: flotilla_core::lifecycle::idle_minutes(store),
        branch_prefix: flotilla_core::lifecycle::branch_prefix(store),
        summary_model: get(crate::SUMMARY_MODEL_KEY)
            .unwrap_or_else(|| flotilla_core::summarize::DEFAULT_MODEL.to_string()),
        archive_on_merge: flotilla_core::tidy::on_merge(store),
        archive_idle_days: flotilla_core::tidy::idle_days(store),
        summaries,
        instructions: flotilla_core::skills::instructions(store),
        skills: flotilla_core::skills::chosen(store),
        skills_error: flotilla_core::skills::error(store),
        projects,
        database: store
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        boat_key,
        doctor,
    })
}

/// The catalog as last read from boat.
pub fn catalog(store: &Store) -> Option<Catalog> {
    let json = store.kv_get("catalog").ok().flatten()?;
    Catalog::from_value(&serde_json::from_str(&json).ok()?).ok()
}

/// Every scheduled prompt, due soonest first.
pub fn schedules(store: &Store) -> flotilla_core::Result<flotilla_proto::Schedules> {
    Ok(flotilla_proto::Schedules {
        schedules: store.schedules()?,
    })
}

/// A thread's skills and files, as last read in its VM.
pub fn mentions(store: &Store, thread: &str) -> Option<Mentions> {
    flotilla_core::mentions::stored(store, thread)
}

/// What the agents spent (`flotilla_core::spend`), priced with the model prices kept beside the
/// database (none for one in memory).
pub fn agent_usage(store: &Store) -> flotilla_core::Result<flotilla_proto::AgentUsage> {
    let (rates, fetched) = match store.path() {
        Some(db) => flotilla_core::prices::for_db(db).table(),
        None => Default::default(),
    };
    flotilla_core::spend::view(store, &rates, fetched, flotilla_core::util::now_ms())
}

#[cfg(test)]
mod tests {
    use flotilla_core::store::{Store, Thread, ThreadState};
    use flotilla_proto::RawEvent;
    use std::collections::BTreeMap;

    fn thread(id: &str) -> Thread {
        Thread {
            id: id.into(),
            environment_id: Some("e1".into()),
            environment_name: Some("base".into()),
            title: format!("Thread {id}"),
            sandbox_id: Some(format!("bx_{id}")),
            conversation_id: Some(format!("c_{id}")),
            provider: "codex".into(),
            model: "gpt-6-luna".into(),
            effort: None,
            fast: false,
            state: ThreadState::Idle,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: "2026-11-01T11:51:07.000Z".into(),
            updated_at: "2026-11-01T11:51:07.000Z".into(),
            repo_project_id: None,
            branch: None,
            base_sha: None,
        }
    }

    /// The card's live timer counts from the hand-off (the prompt sent, before the child's VM
    /// booted); once the child is done, its time is from that same start to the turn's end, not
    /// from the prompt as boat stamped it to the answer's first word.
    #[test]
    fn a_hand_offs_time_runs_from_where_its_timer_did() {
        let store = Store::open_in_memory().unwrap();
        store.insert_thread(&thread("parent")).unwrap();
        store.insert_thread(&thread("child")).unwrap();
        flotilla_core::orchestrate::set_parent(&store, "child", "parent").unwrap();
        store
            .insert_sent_prompt("child", "p1", "prompt", "Review it")
            .unwrap();
        let sent = flotilla_core::util::iso_to_ms(&store.sent_prompts("child").unwrap()[0].sent_at)
            .unwrap();
        store.insert_turn("child", "p1", "running").unwrap();
        // boat's stamps: the prompt 2.4 s after the hand-off (the VM booted), the answer 2.2 s
        // after that.
        let event = |id: &str, kind: &str, ts: i64, data: serde_json::Value| {
            RawEvent::from_value(
                serde_json::json!({"id": id, "type": kind, "timestamp": ts, "taskId": "p1", "conversationId": "c_child", "data": data}),
            )
        };
        let events = [
            event(
                "e1",
                "prompt",
                sent + 2_400,
                serde_json::json!({"prompt": "Review it", "status": "running"}),
            ),
            event(
                "e2",
                "response",
                sent + 4_600,
                serde_json::json!({"content": "Looks right.", "model": "gpt-6-luna"}),
            ),
        ];
        store.apply_events("child", &events, true).unwrap();
        store.rebuild_messages("child").unwrap();
        let card = |store: &Store| {
            let summaries = store.thread_summaries().unwrap();
            let parents: BTreeMap<String, String> = flotilla_core::orchestrate::parents(store)
                .into_iter()
                .collect();
            super::thread(store, "parent", &summaries, &parents)
                .unwrap()
                .children
                .remove(0)
        };
        // Working: the timer starts at the hand-off.
        let working = card(&store);
        assert_eq!(
            (working.started_ms, working.duration_ms),
            (Some(sent), None)
        );

        // Seen to end 9.1 s after the hand-off.
        let mut turn = store.turns("child").unwrap().remove(0);
        turn.status = "finished".into();
        (turn.done, turn.settled) = (true, true);
        turn.done_at = Some(
            chrono::DateTime::from_timestamp_millis(sent + 9_100)
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
        );
        store.update_turn(&turn).unwrap();
        let done = card(&store);
        assert_eq!(
            (done.started_ms, done.duration_ms, done.answer.as_deref()),
            (Some(sent), Some(9_100), Some("Looks right."))
        );
    }
}
