//! Scheduled prompts' runs (`flotilla_proto::schedule`, `design/scheduled-prompts.md`): the
//! worker looks every few seconds for those due, and sends each one's prompt to its thread as a
//! follow-up, or starts a new thread with it.
//!
//! - A thread with a turn running (or starting, or an action under way) is waited for: the run
//!   goes once it's free. A run waiting takes up times that come round meanwhile.
//! - A run never brings back an archived thread or one whose VM is gone: it pauses the
//!   schedule instead, and says so once.
//! - A run that would start a VM (a new thread, or waking a stopped one) goes only while boat
//!   has starts to spare (`Limits::spares_starts`, the idle archiver's reserve). One that
//!   can't is noted and said once, not tried again: a recurring one waits for its next time.
//! - A daily, weekly or cron time that passed while the server was off is skipped to the next
//!   (noted as missed); a one-off or an every-N-hours run goes late, once.
//! - One that runs more often than once a day (set before that was the rule) pauses, says
//!   why once, and doesn't run.

use super::{Action, Actions, After, Changes, Event, Events};
use flotilla_core::lifecycle::{NO_STARTS, SCHEDULED_ARCHIVED, SCHEDULED_BUSY};
use flotilla_core::store::{Store, ThreadState};
use flotilla_model::schedule as when;
use flotilla_proto::{NewThread, Run, RunOutcome, Schedule, Target};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// How often due schedules are looked for, at most; sooner while a run waits on a thread.
const LOOK_EVERY: Duration = Duration::from_secs(15);
const WAITING_EVERY: Duration = Duration::from_secs(3);
/// One-offs that ran this long ago leave the list.
const DONE_KEPT_MS: i64 = 7 * 24 * 3600 * 1000;

/// What the worker is asked.
#[derive(Debug, Clone)]
pub(crate) enum Job {
    /// A schedule was set, or a turn ended: look now.
    Look,
    /// Run this one now, leaving its next time as it is.
    RunNow(String),
}

/// How a run's thread is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ready {
    /// Up: send now.
    Up,
    /// Its VM is stopped: sending wakes it (a start).
    Stopped,
    /// A turn is running, it's starting or stopping, or an action is under way: wait.
    Busy,
    /// It won't take a prompt as it is: the schedule pauses (`why`).
    Never(&'static str),
}

pub(crate) struct Worker {
    pub(crate) db: PathBuf,
    pub(crate) events: Events,
    pub(crate) actions: Actions,
    /// The VM starts this look may still spend, beyond boat's reserve (None: not asked yet).
    /// Each run that starts a VM spends one, so several due at once can't spend the reserve on
    /// one reading; the start itself asks boat again (`Core::scheduled`).
    pub(crate) room: Option<i64>,
    /// Asks boat how many starts there are to spare; a test stands in for it.
    pub(crate) spare: Box<dyn Fn() -> i64 + Send>,
}

impl Worker {
    pub(crate) fn new(db: PathBuf, events: Events, actions: Actions) -> Worker {
        let boat = actions.boat.clone();
        let spare = move || match boat.limits() {
            Ok(l) if l.can_start != Some(false) => l
                .starts_left()
                .map_or(0, |left| left - flotilla_core::boat::START_RESERVE + 1)
                .max(0),
            _ => 0,
        };
        Worker {
            db,
            events,
            actions,
            room: None,
            spare: Box::new(spare),
        }
    }

    pub(crate) fn run(mut self, jobs: mpsc::Receiver<Job>) {
        loop {
            let wait = self.look(flotilla_core::util::now_ms());
            match jobs.recv_timeout(wait) {
                Ok(Job::Look) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(Job::RunNow(id)) => self.run_now(&id, flotilla_core::util::now_ms()),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send_blocking(e);
    }

    fn changed(&self, thread: Option<&str>) {
        let mut c = Changes {
            schedules: true,
            ..Changes::default()
        };
        if let Some(t) = thread {
            c.add_thread(t);
            c.sidebar = true;
        }
        self.emit(Event::Changed(c));
    }

    /// Run what's due at `now`, and how long until the next look.
    pub(crate) fn look(&mut self, now: i64) -> Duration {
        self.room = None;
        let Ok(store) = Store::open(&self.db) else {
            return LOOK_EVERY;
        };
        if store.delete_done_schedules(now - DONE_KEPT_MS).unwrap_or(0) > 0 {
            self.changed(None);
        }
        let mut wait = LOOK_EVERY;
        for s in store.schedules().unwrap_or_default() {
            if !s.paused
                && let Err(why) = when::often_enough(&s.when)
            {
                self.too_often(&store, &s, &why);
                continue;
            }
            match (s.waiting_ms, s.next_ms) {
                // A run waiting on its thread goes once it's free, even while paused: only Run
                // now makes one wait on a paused schedule (pausing drops a waiting run).
                (Some(_), _) => self.fire(&store, &s, now, false),
                (None, _) if s.paused => continue,
                (None, Some(due)) if due <= now => {
                    if when::still_due(&s.when, due, now) {
                        self.fire(&store, &s, now, false);
                    } else {
                        self.missed(&store, &s, due, now);
                    }
                }
                (None, Some(due)) => wait = wait.min(Duration::from_millis((due - now) as u64)),
                (None, None) => {}
            }
            if store
                .schedule(&s.id)
                .ok()
                .flatten()
                .is_some_and(|s| s.waiting_ms.is_some())
            {
                wait = wait.min(WAITING_EVERY);
            }
        }
        wait.max(Duration::from_millis(200))
    }

    /// Run now: as when due, but its next time stays.
    pub(crate) fn run_now(&mut self, id: &str, now: i64) {
        self.room = None;
        let Ok(store) = Store::open(&self.db) else {
            return;
        };
        if let Some(s) = store.schedule(id).ok().flatten() {
            self.fire(&store, &s, now, true);
        }
    }

    /// It runs more often than once a day (set before that was the rule): paused, and said
    /// once, as it was read. Claimed first at that revision, so a retime since (to daily, say)
    /// is neither paused nor noted.
    fn too_often(&self, store: &Store, s: &Schedule, why: &str) {
        let plan = store
            .schedule_plan(&s.id)
            .ok()
            .flatten()
            .unwrap_or_default();
        if store
            .set_schedule_next(&s.id, None, None, s.updated_ms)
            .unwrap_or(false)
        {
            self.fail(store, s, plan, s.target.thread(), why, true);
        }
    }

    /// Its time passed while the server was off: on to the next, noted as missed.
    fn missed(&self, store: &Store, s: &Schedule, due: i64, now: i64) {
        let next = when::next_after(&s.when, &s.zone, now).ok().flatten();
        if store
            .set_schedule_next(&s.id, next, None, s.updated_ms)
            .unwrap_or(false)
        {
            let run = Run {
                at_ms: due,
                outcome: RunOutcome::Missed,
                thread: s.target.thread().map(str::to_string),
                why: Some("The server was off.".into()),
            };
            let _ = store.record_run(&s.id, &run, false, None);
            eprintln!(
                "[schedules] {} missed its time while the server was off",
                s.id
            );
            self.changed(None);
        }
    }

    /// Spend one of this look's VM starts, if there's one to spare.
    fn take_start(&mut self) -> bool {
        let room = self.room.get_or_insert_with(|| (self.spare)());
        if *room > 0 {
            *room -= 1;
            true
        } else {
            false
        }
    }

    /// Its thread, as a run sees it.
    fn ready(&self, store: &Store, thread: &str) -> Ready {
        let Some(t) = store.thread(thread).ok().flatten() else {
            return Ready::Never("The thread is gone.");
        };
        if store.is_archived(thread).unwrap_or(false) {
            return Ready::Never(SCHEDULED_ARCHIVED);
        }
        if t.vm_gone() {
            return Ready::Never("The thread's VM is gone.");
        }
        if store.start_error(thread).ok().flatten().is_some() {
            return Ready::Never("The thread didn't start.");
        }
        let starting = store.start_stages(thread).ok().flatten().is_some();
        let turn = !store.unsettled_turns(thread).unwrap_or_default().is_empty();
        if starting || turn || self.actions.busy_threads().contains(thread) {
            return Ready::Busy;
        }
        match t.state {
            ThreadState::Idle | ThreadState::Running | ThreadState::Error => Ready::Up,
            ThreadState::Stopped => Ready::Stopped,
            ThreadState::Creating | ThreadState::Starting | ThreadState::Stopping => Ready::Busy,
            ThreadState::Expired => Ready::Never("The thread's VM is gone."),
        }
    }

    /// Run `s` (due, or `now_asked` by Run now): claim it (its next time written), then send
    /// or start, or wait, or say why not.
    fn fire(&mut self, store: &Store, s: &Schedule, now: i64, now_asked: bool) {
        // Run now, or a run of a paused schedule, leaves the next time alone; a due run moves it
        // on first, so a restart in between never runs it twice (as T3, a run cut off isn't run
        // again).
        let next = if now_asked || s.paused {
            s.next_ms
        } else {
            when::next_after(&s.when, &s.zone, now).ok().flatten()
        };
        // Its plan as read with it (read before the claim: a change in between fails the claim).
        let plan = store
            .schedule_plan(&s.id)
            .ok()
            .flatten()
            .unwrap_or_default();
        // When the run was due: what a run that finds its thread busy waits from.
        let due = s
            .waiting_ms
            .or(if now_asked { None } else { s.next_ms })
            .unwrap_or(now);
        match &s.target {
            Target::Thread(thread) => match self.ready(store, thread) {
                Ready::Busy => {
                    if (s.next_ms, s.waiting_ms) != (next, Some(due))
                        && store
                            .set_schedule_next(&s.id, next, Some(due), s.updated_ms)
                            .unwrap_or(false)
                    {
                        self.changed(None);
                    }
                }
                Ready::Never(why) => {
                    if store
                        .set_schedule_next(&s.id, next, None, s.updated_ms)
                        .unwrap_or(false)
                    {
                        self.fail(store, s, plan, Some(thread), why, true);
                    }
                }
                Ready::Stopped if !self.take_start() => {
                    if store
                        .set_schedule_next(&s.id, next, None, s.updated_ms)
                        .unwrap_or(false)
                    {
                        self.fail(store, s, plan, Some(thread), NO_STARTS, false);
                    }
                }
                Ready::Up | Ready::Stopped => {
                    if store
                        .set_schedule_next(&s.id, next, None, s.updated_ms)
                        .unwrap_or(false)
                    {
                        self.send(store, s, plan, thread, due, now);
                    }
                }
            },
            Target::New(_) => {
                let removed = s
                    .target_project()
                    .is_some_and(|p| store.project(p).ok().flatten().is_none());
                // Nothing to check the model against yet (boat's catalog is read at the first
                // sync): left due for the next look.
                let Some(catalog) = crate::views::catalog(store) else {
                    return;
                };
                if !store
                    .set_schedule_next(&s.id, next, None, s.updated_ms)
                    .unwrap_or(false)
                {
                    return;
                }
                if removed {
                    self.fail(store, s, plan, None, "The project was removed.", true);
                } else if !self.take_start() {
                    self.fail(store, s, plan, None, NO_STARTS, false);
                } else {
                    self.start(store, s, plan, &catalog, now);
                }
            }
        }
    }

    /// Send the prompt to `thread` as a follow-up, marked as this schedule's. Noted as sent at
    /// once (how its runs fail stays until it's sent); `due` is what it waits from if the
    /// thread turns out busy.
    fn send(&self, store: &Store, s: &Schedule, plan: i64, thread: &str, due: i64, now: i64) {
        let local = format!(
            "{}{}",
            flotilla_core::lifecycle::LOCAL_PREFIX,
            uuid::Uuid::new_v4()
        );
        let _ = store.insert_sent_prompt(thread, &local, "prompt", &s.prompt);
        let _ = store.set_sent_schedule(thread, &local, &s.id);
        let _ = store.rebuild_messages(thread);
        let run = Run {
            at_ms: now,
            outcome: RunOutcome::Sent,
            thread: Some(thread.to_string()),
            why: None,
        };
        let _ = store.record_run(&s.id, &run, true, None);
        self.changed(Some(thread));
        let after = self.after(s, plan, &run, due, true);
        self.actions.spawn_after(
            Action::SendLater {
                thread: thread.to_string(),
                text: s.prompt.clone(),
                local,
            },
            after,
            None,
        );
    }

    /// Start a new thread with the prompt, its first prompt marked as this schedule's.
    fn start(
        &self,
        store: &Store,
        s: &Schedule,
        plan: i64,
        catalog: &flotilla_core::catalog::Catalog,
        now: i64,
    ) {
        let Target::New(f) = &s.target else { return };
        let req = NewThread {
            environment: f.environment.clone(),
            project_id: f.project_id.clone(),
            provider: f.provider.clone(),
            model: f.model.clone(),
            effort: f.effort.clone(),
            fast: f.fast,
            title: None,
            machine_type: f.machine_type.clone(),
            prompt: s.prompt.clone(),
            attachments: vec![],
            wait_ready: false,
            on_branch: f.on_branch.clone(),
            id: Some(uuid::Uuid::new_v4().to_string()),
            parent: None,
            branch_tag: None,
        };
        match flotilla_core::lifecycle::begin_thread(store, catalog, &req) {
            Ok((t, local)) => {
                let _ = store.set_sent_schedule(&t.id, &local, &s.id);
                let _ = store.rebuild_messages(&t.id);
                let run = Run {
                    at_ms: now,
                    outcome: RunOutcome::Sent,
                    thread: Some(t.id.clone()),
                    why: None,
                };
                let _ = store.record_run(&s.id, &run, true, None);
                let mut c = Changes {
                    schedules: true,
                    ..Changes::thread(&t.id).with_sidebar()
                };
                if let Some(p) = &req.project_id {
                    c.add_project(p);
                }
                self.emit(Event::Changed(c));
                // A start that fails is said here, once for failures alike (the host leaves out
                // the start-failed alert a thread of yours would make).
                let after = self.after(s, plan, &run, now, true);
                self.actions.spawn_after(
                    Action::StartLater {
                        thread: t.id,
                        req: Box::new(req),
                        local,
                    },
                    after,
                    None,
                );
            }
            // Refused before anything started (a model gone from the catalog, say): it can't
            // run as it is.
            Err(e) => self.fail(
                store,
                s,
                plan,
                None,
                &format!("{}.", e.plain().trim_end_matches('.')),
                true,
            ),
        }
    }

    /// As the send or start ends. Sent: its runs work. Its thread busy after all (a turn
    /// started since): the run (`sent`, as noted) is taken back and waits, unless the schedule
    /// was changed, paused or run again since. Archived since: noted, the schedule paused (as it
    /// was then) and said once. Otherwise noted, and said (`say`) unless it was the last time
    /// too.
    fn after(&self, s: &Schedule, plan: i64, sent: &Run, due: i64, say: bool) -> After {
        let (id, prompt, before, target, events) = (
            s.id.clone(),
            s.prompt.clone(),
            s.last.clone(),
            s.target.clone(),
            self.events.clone(),
        );
        let (thread, sent) = (sent.thread.clone(), sent.clone());
        Box::new(move |db: &std::path::Path, result: Result<(), &str>| {
            let Ok(store) = Store::open(db) else { return };
            match result {
                Ok(()) => {
                    let _ = store.note_failing(&id, None);
                }
                Err(why) if why == SCHEDULED_BUSY => {
                    let _ = store.unsend_run(&id, before.as_ref(), due, plan, &sent);
                }
                Err(why) => {
                    let why = flotilla_model::util::redact(why.trim());
                    let pause = why == SCHEDULED_ARCHIVED;
                    let run = Run {
                        at_ms: flotilla_core::util::now_ms(),
                        outcome: RunOutcome::Failed,
                        thread: thread.clone(),
                        why: Some(why.clone()),
                    };
                    let _ = store.record_run(&id, &run, false, pause.then_some((plan, &target)));
                    let news = store.note_failing(&id, Some(&why)).unwrap_or(false);
                    if (say || pause) && news {
                        let _ = events.send_blocking(Event::ScheduleFailed {
                            schedule: id.clone(),
                            prompt,
                            thread,
                            why,
                        });
                    }
                }
            }
            let _ = events.send_blocking(Event::Changed(Changes {
                schedules: true,
                ..Changes::default()
            }));
        })
    }

    /// It didn't run (`why`): noted, paused if it can't run as it is (as its plan and target
    /// were when read), and said once for a run of failures with the same reason.
    fn fail(
        &self,
        store: &Store,
        s: &Schedule,
        plan: i64,
        thread: Option<&str>,
        why: &str,
        pause: bool,
    ) {
        let run = Run {
            at_ms: flotilla_core::util::now_ms(),
            outcome: RunOutcome::Failed,
            thread: thread.map(str::to_string),
            why: Some(why.to_string()),
        };
        let _ = store.record_run(&s.id, &run, false, pause.then_some((plan, &s.target)));
        let news = store.note_failing(&s.id, Some(why)).unwrap_or(false);
        eprintln!("[schedules] {} didn't run: {why}", s.id);
        self.changed(None);
        if news {
            self.emit(Event::ScheduleFailed {
                schedule: s.id.clone(),
                prompt: s.prompt.clone(),
                thread: thread.map(str::to_string),
                why: why.to_string(),
            });
        }
    }
}

/// Runs against a fresh database, a boat that can't be reached (so each send or start fails at
/// once, after the run is claimed) and boat's starts to spare stood in for.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Actions};
    use flotilla_core::boat::BoatClient;
    use flotilla_core::github::{Gh, GitHub};
    use flotilla_core::store::Thread;
    use flotilla_proto::{Fresh, When};
    use std::sync::Arc;
    use std::time::Instant;

    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;

    /// 09:00 on Monday 2 November 2026 in London, when `daily()` runs.
    fn nine() -> i64 {
        flotilla_model::schedule::at_local("Europe/London", jiff::civil::date(2026, 11, 2), 9 * 60)
            .unwrap()
    }

    fn daily() -> When {
        When::Daily { at: 9 * 60 }
    }

    fn thread(id: &str, state: ThreadState) -> Thread {
        Thread {
            id: id.into(),
            environment_id: Some("e1".into()),
            environment_name: Some("base".into()),
            title: format!("Thread {id}"),
            sandbox_id: Some(format!("bx_{id}")),
            conversation_id: Some(format!("conv_{id}")),
            provider: "claude-code".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: flotilla_core::util::now_iso(),
            updated_at: flotilla_core::util::now_iso(),
            branch: None,
            repo_project_id: None,
            base_sha: None,
        }
    }

    struct Fixture {
        worker: Worker,
        store: Store,
        rx: mpsc::Receiver<Event>,
        dir: PathBuf,
        spare: Arc<std::sync::atomic::AtomicI64>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!(
            "flotilla-schedules-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        let store = Store::open(&db).unwrap();
        let catalog = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../m0-samples/provider-models.json"
        ))
        .unwrap();
        store.kv_set("catalog", &catalog).unwrap();
        store
            .replace_environments(&[flotilla_core::boat::Environment {
                id: "e1".into(),
                name: "base".into(),
                is_default: true,
            }])
            .unwrap();
        let boat = BoatClient::with_base(
            "boat_test".into(),
            "http://127.0.0.1:9/api/v1",
            "http://127.0.0.1:9/api/v1/provider-models",
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        let events = Events::new(move |e| tx.send(e).is_ok());
        let actions = Actions {
            db: db.clone(),
            boat,
            github: GitHub::new(Arc::new(Gh::with_path(dir.join("no-gh")))),
            events: events.clone(),
            poke: mpsc::channel().0,
            summaries: mpsc::channel().0,
            services: mpsc::channel().0,
            usage: mpsc::channel().0,
            busy: Arc::default(),
            starts: Arc::default(),
            locks: Arc::default(),
        };
        let spare = Arc::new(std::sync::atomic::AtomicI64::new(5));
        let mut worker = Worker::new(db, events, actions);
        let room = spare.clone();
        worker.spare = Box::new(move || room.load(std::sync::atomic::Ordering::SeqCst));
        Fixture {
            worker,
            store,
            rx,
            dir,
            spare,
        }
    }

    fn schedule(id: &str, target: Target, when: When, next_ms: Option<i64>) -> Schedule {
        Schedule {
            id: id.into(),
            prompt: "Check the nightly CI".into(),
            target,
            when,
            zone: "Europe/London".into(),
            paused: false,
            next_ms,
            waiting_ms: None,
            last: None,
            runs: 0,
            from: "MacBook".into(),
            created_ms: 1,
            updated_ms: 1,
        }
    }

    fn fresh() -> Target {
        Target::New(Fresh {
            environment: "base".into(),
            project_id: None,
            provider: "claude-code".into(),
            model: "claude-sonnet-5-5".into(),
            effort: None,
            fast: false,
            machine_type: None,
            on_branch: None,
        })
    }

    /// What the worker set off within `wait`: each action that ended, and each failure said.
    fn seen(rx: &mpsc::Receiver<Event>, wait: Duration) -> (Vec<Action>, Vec<String>) {
        let (mut acts, mut said) = (vec![], vec![]);
        let until = Instant::now() + wait;
        while let Ok(e) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            match e {
                Event::Done { action, .. } => acts.push(action),
                Event::ScheduleFailed { why, .. } => said.push(why),
                _ => {}
            }
        }
        (acts, said)
    }

    fn get(f: &Fixture, id: &str) -> Schedule {
        f.store.schedule(id).unwrap().unwrap()
    }

    /// Due, to a new thread: the thread is written with the prompt, marked as the schedule's,
    /// and its start goes to the engine. Its start failing is noted on the schedule (the alert
    /// rule says it, as for any start), and the next time is tomorrow's.
    #[test]
    fn a_run_starts_a_new_thread_marked_as_the_schedules() {
        let mut f = fixture("new");
        let now = flotilla_model::schedule::at_local(
            "Europe/London",
            jiff::civil::date(2026, 11, 2),
            9 * 60,
        )
        .unwrap();
        f.store
            .put_schedule(
                &schedule("s1", fresh(), When::Daily { at: 9 * 60 }, Some(now)),
                true,
            )
            .unwrap();
        f.worker.look(now + 1000);
        let (acts, said) = seen(&f.rx, Duration::from_secs(5));
        let started: Vec<&String> = acts
            .iter()
            .filter_map(|a| {
                if let Action::StartLater { thread, .. } = a {
                    Some(thread)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(started.len(), 1, "{acts:?}");
        // Its start failing (boat can't be reached) is the schedule's to say, once.
        assert_eq!(said.len(), 1, "{said:?}");
        let t = started[0];
        let sent = f.store.sent_prompts(t).unwrap();
        assert_eq!(
            (sent[0].text.as_str(), sent[0].schedule.as_deref()),
            ("Check the nightly CI", Some("s1"))
        );
        let s = get(&f, "s1");
        assert_eq!(s.next_ms, Some(now + 24 * HOUR));
        assert_eq!(s.runs, 1);
        let last = s.last.unwrap();
        assert_eq!(
            (last.outcome, last.thread.as_deref()),
            (RunOutcome::Failed, Some(t.as_str()))
        );
    }

    /// No starts to spare: noted and said once, not tried again until its next time; a run
    /// that works ends the streak.
    #[test]
    fn a_run_without_starts_to_spare_is_said_once_and_waits_for_its_next_time() {
        let mut f = fixture("starts");
        f.spare.store(0, std::sync::atomic::Ordering::SeqCst);
        let now = nine();
        f.store
            .put_schedule(&schedule("s1", fresh(), daily(), Some(now)), true)
            .unwrap();
        f.worker.look(now);
        let s = get(&f, "s1");
        assert_eq!(
            (s.next_ms, s.paused, s.last.as_ref().map(|l| l.outcome)),
            (Some(now + DAY), false, Some(RunOutcome::Failed))
        );
        assert_eq!(s.last.unwrap().why.as_deref(), Some(NO_STARTS));
        // Looked at again before its next time: nothing.
        f.worker.look(now + HOUR);
        // Its next time, still no starts: noted again, said no more.
        f.worker.look(now + DAY);
        let (acts, said) = seen(&f.rx, Duration::from_millis(300));
        assert!(acts.is_empty(), "{acts:?}");
        assert_eq!(said, [NO_STARTS]);
        assert_eq!(get(&f, "s1").next_ms, Some(now + 2 * DAY));
        assert!(f.store.thread_summaries().unwrap().is_empty());
    }

    /// A thread with a turn running: the run waits for it, taking up a time that comes round
    /// meanwhile, then goes once the turn ends, marked as the schedule's.
    #[test]
    fn a_run_to_a_busy_thread_waits_for_its_turn_to_end() {
        let mut f = fixture("busy");
        f.store
            .insert_thread(&thread("t1", ThreadState::Running))
            .unwrap();
        f.store.insert_turn("t1", "p1", "running").unwrap();
        let now = nine();
        f.store
            .put_schedule(
                &schedule("s1", Target::Thread("t1".into()), daily(), Some(now)),
                true,
            )
            .unwrap();
        f.worker.look(now);
        let s = get(&f, "s1");
        assert_eq!(
            (s.waiting_ms, s.next_ms, s.runs),
            (Some(now), Some(now + DAY), 0)
        );
        // A day on, still running: one run waits, the next time moves on.
        f.worker.look(now + DAY + 1);
        let s = get(&f, "s1");
        assert_eq!((s.waiting_ms, s.next_ms), (Some(now), Some(now + 2 * DAY)));
        assert!(seen(&f.rx, Duration::from_millis(200)).0.is_empty());
        // The turn ends: it goes.
        let mut turn = f.store.unsettled_turns("t1").unwrap().remove(0);
        turn.settled = true;
        turn.done = true;
        f.store.update_turn(&turn).unwrap();
        f.store.set_thread_state("t1", ThreadState::Idle).unwrap();
        f.worker.look(now + DAY + 2);
        let (acts, _) = seen(&f.rx, Duration::from_secs(5));
        let sends: Vec<&String> = acts
            .iter()
            .filter_map(|a| {
                if let Action::SendLater { thread, text, .. } = a {
                    (thread == "t1").then_some(text)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sends, ["Check the nightly CI"]);
        let s = get(&f, "s1");
        assert_eq!(
            (s.waiting_ms, s.next_ms, s.runs),
            (None, Some(now + 2 * DAY), 1)
        );
    }

    /// An archived thread isn't brought back: the schedule pauses and says why, once.
    #[test]
    fn a_run_to_an_archived_thread_pauses_the_schedule() {
        let mut f = fixture("archived");
        f.store
            .insert_thread(&thread("t1", ThreadState::Stopped))
            .unwrap();
        f.store.set_archived("t1", true).unwrap();
        let now = nine();
        f.store
            .put_schedule(
                &schedule("s1", Target::Thread("t1".into()), daily(), Some(now)),
                true,
            )
            .unwrap();
        f.worker.look(now);
        let (acts, said) = seen(&f.rx, Duration::from_millis(300));
        assert!(acts.is_empty(), "{acts:?}");
        assert_eq!(said, ["The thread is archived."]);
        let s = get(&f, "s1");
        assert!(s.paused && s.next_ms.is_none());
        assert!(f.store.is_archived("t1").unwrap());
        // Paused: never again on its own.
        f.worker.look(now + DAY);
        let (acts, said) = seen(&f.rx, Duration::from_millis(200));
        assert!(acts.is_empty() && said.is_empty(), "{acts:?} {said:?}");
    }

    /// A stopped VM is woken to send only while boat has starts to spare.
    #[test]
    fn a_run_to_a_stopped_thread_wakes_it_only_with_starts_to_spare() {
        let mut f = fixture("stopped");
        f.store
            .insert_thread(&thread("t1", ThreadState::Stopped))
            .unwrap();
        let now = nine();
        f.store
            .put_schedule(
                &schedule("s1", Target::Thread("t1".into()), daily(), Some(now)),
                true,
            )
            .unwrap();
        f.spare.store(0, std::sync::atomic::Ordering::SeqCst);
        f.worker.look(now);
        let (acts, said) = seen(&f.rx, Duration::from_millis(300));
        assert!(acts.is_empty());
        assert_eq!(said, [NO_STARTS]);
        assert!(!get(&f, "s1").paused);
        f.spare.store(5, std::sync::atomic::Ordering::SeqCst);
        f.worker.look(now + DAY);
        let (acts, said) = seen(&f.rx, Duration::from_secs(5));
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::SendLater { thread, .. } if thread == "t1")),
            "{acts:?}"
        );
        // The send failed (boat can't be reached): said, since nothing else would.
        assert_eq!(said.len(), 1, "{said:?}");
    }

    /// The server was off: a daily time missed by hours is skipped to the next and noted; a
    /// one-off goes late.
    #[test]
    fn missed_times_are_skipped_and_one_offs_go_late() {
        let mut f = fixture("missed");
        f.store
            .insert_thread(&thread("t1", ThreadState::Idle))
            .unwrap();
        let nine = flotilla_model::schedule::at_local(
            "Europe/London",
            jiff::civil::date(2026, 11, 2),
            9 * 60,
        )
        .unwrap();
        f.store
            .put_schedule(
                &schedule(
                    "daily",
                    Target::Thread("t1".into()),
                    When::Daily { at: 9 * 60 },
                    Some(nine),
                ),
                true,
            )
            .unwrap();
        f.store
            .put_schedule(
                &schedule(
                    "once",
                    Target::Thread("t1".into()),
                    When::Once { at_ms: nine },
                    Some(nine),
                ),
                true,
            )
            .unwrap();
        f.worker.look(nine + 3 * HOUR);
        let (acts, _) = seen(&f.rx, Duration::from_secs(5));
        assert_eq!(
            acts.iter()
                .filter(|a| matches!(a, Action::SendLater { .. }))
                .count(),
            1,
            "{acts:?}"
        );
        let daily = get(&f, "daily");
        assert_eq!(
            (
                daily.next_ms,
                daily.last.as_ref().map(|l| (l.outcome, l.at_ms))
            ),
            (Some(nine + 24 * HOUR), Some((RunOutcome::Missed, nine)))
        );
        let once = get(&f, "once");
        assert_eq!((once.next_ms, once.runs), (None, 1));
        assert!(once.done());
    }

    /// Set before runs had to be a day apart, one that runs more often (every 3 hours, a cron
    /// every 4 hours) pauses at the next look, due or not, and says why once; it never runs.
    /// A daily one is left alone. Resumed by hand it would only pause again, so the host
    /// refuses that until it's edited.
    #[test]
    fn a_schedule_more_often_than_daily_pauses_and_says_why() {
        let mut f = fixture("often");
        f.store
            .insert_thread(&thread("t1", ThreadState::Idle))
            .unwrap();
        let now = nine();
        f.store
            .put_schedule(
                &schedule(
                    "hours",
                    Target::Thread("t1".into()),
                    When::Hours {
                        every: 3,
                        from_ms: now + HOUR,
                    },
                    Some(now + HOUR),
                ),
                true,
            )
            .unwrap();
        f.store
            .put_schedule(
                &schedule(
                    "cron",
                    fresh(),
                    When::Cron {
                        expr: "0 */4 * * 1-5".into(),
                    },
                    Some(now),
                ),
                true,
            )
            .unwrap();
        f.store
            .put_schedule(
                &schedule(
                    "daily",
                    Target::Thread("t1".into()),
                    daily(),
                    Some(now + DAY),
                ),
                true,
            )
            .unwrap();
        f.worker.look(now);
        let (acts, mut said) = seen(&f.rx, Duration::from_millis(300));
        assert!(acts.is_empty(), "{acts:?}");
        said.sort();
        assert_eq!(said, [when::TOO_OFTEN, when::TOO_OFTEN]);
        for id in ["hours", "cron"] {
            let s = get(&f, id);
            let last = s.last.as_ref().unwrap();
            assert!(
                s.paused && s.next_ms.is_none() && s.waiting_ms.is_none(),
                "{s:?}"
            );
            assert_eq!(
                (last.outcome, last.why.as_deref(), s.runs),
                (RunOutcome::Failed, Some(when::TOO_OFTEN), 0)
            );
        }
        assert_eq!(
            when::summary(&get(&f, "hours"), now, "Europe/London"),
            "Every 3 hours · paused"
        );
        assert!(
            when::last_words(&get(&f, "cron"), now, "Europe/London")
                .is_some_and(|w| w.ends_with(": runs must be at least a day apart."))
        );
        let d = get(&f, "daily");
        assert!(!d.paused && d.next_ms == Some(now + DAY) && d.last.is_none());
        // Looked at again: nothing more, and no new thread started.
        f.worker.look(now + 4 * HOUR);
        let (acts, said) = seen(&f.rx, Duration::from_millis(300));
        assert!(acts.is_empty() && said.is_empty(), "{acts:?} {said:?}");
        assert!(
            f.store
                .thread_summaries()
                .unwrap()
                .iter()
                .all(|t| t.thread.id == "t1")
        );
    }

    /// Retimed to daily between the look that read it every 3 hours and its pause: the new
    /// timing stands, not paused, nothing noted or said.
    #[test]
    fn a_retime_while_it_pauses_for_too_often_stands() {
        let f = fixture("often-race");
        f.store
            .insert_thread(&thread("t1", ThreadState::Idle))
            .unwrap();
        let now = nine();
        f.store
            .put_schedule(
                &schedule(
                    "s1",
                    Target::Thread("t1".into()),
                    When::Hours {
                        every: 3,
                        from_ms: now,
                    },
                    Some(now),
                ),
                true,
            )
            .unwrap();
        let read = get(&f, "s1");
        // The host's edit, as `put_schedule` makes it: a new revision, retimed.
        f.store
            .put_schedule(
                &Schedule {
                    when: daily(),
                    next_ms: Some(now + DAY),
                    updated_ms: read.updated_ms + 1,
                    ..read.clone()
                },
                true,
            )
            .unwrap();
        f.worker.too_often(&f.store, &read, when::TOO_OFTEN);
        let s = get(&f, "s1");
        assert!(
            !s.paused && s.when == daily() && s.next_ms == Some(now + DAY) && s.last.is_none(),
            "{s:?}"
        );
        let (_, said) = seen(&f.rx, Duration::from_millis(200));
        assert!(said.is_empty(), "{said:?}");
    }

    /// Several due at once can't spend the reserve on one reading of boat's starts: each run
    /// that starts a VM spends one of those this look may.
    #[test]
    fn runs_due_together_spend_one_start_each() {
        let mut f = fixture("budget");
        f.spare.store(1, std::sync::atomic::Ordering::SeqCst);
        let now = nine();
        for id in ["s1", "s2"] {
            f.store
                .put_schedule(&schedule(id, fresh(), daily(), Some(now)), true)
                .unwrap();
        }
        f.worker.look(now);
        let (acts, said) = seen(&f.rx, Duration::from_secs(5));
        assert_eq!(
            acts.iter()
                .filter(|a| matches!(a, Action::StartLater { .. }))
                .count(),
            1,
            "{acts:?}"
        );
        // The other: no starts to spare. (The one started fails on: boat can't be reached.)
        assert!(
            said.iter().filter(|w| *w == NO_STARTS).count() == 1 && said.len() == 2,
            "{said:?}"
        );
    }

    /// Between the look that found the thread free and the send, a turn started: the run is
    /// taken back and waits; the thread was archived: it stays archived, the schedule pauses,
    /// said once. A send boat refuses again and again for one reason is said once.
    #[test]
    fn what_happens_between_the_look_and_the_send() {
        let f = fixture("after");
        f.store
            .insert_thread(&thread("t1", ThreadState::Idle))
            .unwrap();
        let now = 1_800_000_000_000;
        let earlier = Run {
            at_ms: now - HOUR,
            outcome: RunOutcome::Sent,
            thread: Some("t1".into()),
            why: None,
        };
        let s1 = Schedule {
            last: Some(earlier.clone()),
            runs: 3,
            ..schedule("s1", Target::Thread("t1".into()), daily(), Some(now + HOUR))
        };
        f.store.put_schedule(&s1, true).unwrap();
        let sent = Run {
            at_ms: now,
            outcome: RunOutcome::Sent,
            thread: Some("t1".into()),
            why: None,
        };
        f.store.record_run("s1", &sent, true, None).unwrap();
        let db = f.dir.join("flotilla.db");
        // Busy after all: the run before it back, waiting from when it was due.
        (f.worker.after(&s1, 1, &sent, now, true))(&db, Err(SCHEDULED_BUSY));
        let s = get(&f, "s1");
        assert_eq!(
            (s.last, s.runs, s.waiting_ms, s.paused),
            (Some(earlier), 3, Some(now), false)
        );
        // boat refusing twice: said once.
        for _ in 0..2 {
            f.store.record_run("s1", &sent, true, None).unwrap();
            (f.worker.after(&s1, 1, &sent, now, true))(&db, Err("boat said 500"));
        }
        // Archived meanwhile: paused, said.
        (f.worker.after(&s1, 1, &sent, now, true))(&db, Err(SCHEDULED_ARCHIVED));
        let (_, said) = seen(&f.rx, Duration::from_millis(300));
        assert_eq!(said, ["boat said 500", SCHEDULED_ARCHIVED]);
        assert!(get(&f, "s1").paused);
        // A send that works ends the run of failures.
        (f.worker.after(&s1, 1, &sent, now, true))(&db, Ok(()));
        assert_eq!(f.store.schedule_failing("s1").unwrap(), None);
        // Paused while a send was under way that then found the thread busy: it stays paused,
        // with nothing waiting (it would run on a paused schedule).
        f.store
            .set_schedule_paused("s1", false, Some(now + HOUR), 2)
            .unwrap();
        let s1 = get(&f, "s1");
        f.store.record_run("s1", &sent, true, None).unwrap();
        f.store.set_schedule_paused("s1", true, None, 3).unwrap();
        (f.worker.after(&s1, 1, &sent, now, true))(&db, Err(SCHEDULED_BUSY));
        let s = get(&f, "s1");
        assert!(s.paused && s.waiting_ms.is_none(), "{s:?}");
    }

    /// The scheduled send itself: never brings back an archived thread, and never steers a
    /// turn that started since it was due (it waits instead); its placeholder goes either way.
    #[test]
    fn a_scheduled_send_refuses_an_archived_or_busy_thread() {
        let f = fixture("send");
        f.store
            .insert_thread(&thread("t1", ThreadState::Idle))
            .unwrap();
        f.store.set_archived("t1", true).unwrap();
        f.store
            .insert_thread(&thread("t2", ThreadState::Running))
            .unwrap();
        f.store.insert_turn("t2", "p1", "running").unwrap();
        for (t, local) in [("t1", "local-a"), ("t2", "local-b")] {
            f.store
                .insert_sent_prompt(t, local, "prompt", "Check CI")
                .unwrap();
            f.worker.actions.spawn(Action::SendLater {
                thread: t.into(),
                text: "Check CI".into(),
                local: local.into(),
            });
        }
        let mut results = std::collections::HashMap::new();
        let until = Instant::now() + Duration::from_secs(10);
        while results.len() < 2 && Instant::now() < until {
            if let Ok(Event::Done {
                action: Action::SendLater { thread, .. },
                result,
                ..
            }) = f.rx.recv_timeout(Duration::from_millis(100))
            {
                results.insert(thread, result.err().unwrap_or_default());
            }
        }
        assert_eq!(
            results.get("t1").map(String::as_str),
            Some(SCHEDULED_ARCHIVED)
        );
        assert_eq!(results.get("t2").map(String::as_str), Some(SCHEDULED_BUSY));
        assert!(f.store.is_archived("t1").unwrap());
        assert!(
            f.store.sent_prompts("t1").unwrap().is_empty()
                && f.store.sent_prompts("t2").unwrap().is_empty()
        );
    }

    /// A scheduled send or start takes turns with a send of yours (and archiving, stopping,
    /// deleting) on the same thread: while one holds the thread, the other waits. (A start's
    /// first prompt goes before a send of yours made while its VM came up.)
    #[test]
    fn a_scheduled_send_takes_turns_with_yours() {
        let f = fixture("turns");
        f.store
            .insert_thread(&thread("t1", ThreadState::Idle))
            .unwrap();
        let Target::New(fr) = fresh() else {
            unreachable!()
        };
        let req = NewThread {
            environment: fr.environment,
            project_id: None,
            provider: fr.provider,
            model: fr.model,
            effort: None,
            fast: false,
            title: None,
            machine_type: None,
            prompt: "Check CI".into(),
            attachments: vec![],
            wait_ready: false,
            on_branch: None,
            id: Some("t1".into()),
            parent: None,
            branch_tag: None,
        };
        for action in [
            Action::SendLater {
                thread: "t1".into(),
                text: "Check CI".into(),
                local: "local-a".into(),
            },
            Action::StartLater {
                thread: "t1".into(),
                req: Box::new(req),
                local: "local-b".into(),
            },
            Action::Send {
                thread: "t1".into(),
                text: "and docs".into(),
                files: vec![],
                local: None,
            },
        ] {
            let lock = f.worker.actions.lock_for("t1");
            let held = lock.lock().unwrap();
            f.worker.actions.spawn(action);
            // Anything it says (a start's progress too) means it ran.
            assert!(
                f.rx.recv_timeout(Duration::from_millis(300)).is_err(),
                "ran while the thread was held"
            );
            drop(held);
            let until = Instant::now() + Duration::from_secs(10);
            while !matches!(
                f.rx.recv_timeout(until.saturating_duration_since(Instant::now())),
                Ok(Event::Done { .. })
            ) {
                assert!(Instant::now() < until, "never ran");
            }
        }
    }

    /// Run now on a paused schedule whose thread is busy: it waits, and goes when the turn
    /// ends; the schedule stays paused.
    #[test]
    fn run_now_on_a_paused_schedule_waits_for_a_busy_thread() {
        let mut f = fixture("paused");
        f.store
            .insert_thread(&thread("t1", ThreadState::Running))
            .unwrap();
        f.store.insert_turn("t1", "p1", "running").unwrap();
        let now = 1_800_000_000_000;
        f.store
            .put_schedule(
                &Schedule {
                    paused: true,
                    ..schedule(
                        "s1",
                        Target::Thread("t1".into()),
                        When::Daily { at: 540 },
                        None,
                    )
                },
                true,
            )
            .unwrap();
        f.worker.run_now("s1", now);
        assert_eq!(get(&f, "s1").waiting_ms, Some(now));
        let mut turn = f.store.unsettled_turns("t1").unwrap().remove(0);
        turn.settled = true;
        turn.done = true;
        f.store.update_turn(&turn).unwrap();
        f.store.set_thread_state("t1", ThreadState::Idle).unwrap();
        f.worker.look(now + 1000);
        let (acts, _) = seen(&f.rx, Duration::from_secs(5));
        assert!(
            acts.iter().any(|a| matches!(a, Action::SendLater { .. })),
            "{acts:?}"
        );
        let s = get(&f, "s1");
        assert!(s.paused && s.next_ms.is_none() && s.waiting_ms.is_none());
    }

    /// A boat whose every answer is its limits, with `left` starts this hour.
    fn boat_with_starts(left: i64) -> BoatClient {
        boat_with_limits(
            serde_json::json!({"canStart": true, "starts": {"hour": {"limit": 60, "remaining": left}}}),
        )
    }

    /// A boat whose every answer is `limits`.
    fn boat_with_limits(limits: serde_json::Value) -> BoatClient {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for mut conn in listener.incoming().flatten() {
                let mut buf = [0u8; 8192];
                let _ = conn.read(&mut buf);
                let body = limits.to_string();
                let _ = write!(
                    conn,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        BoatClient::with_base(
            "boat_test".into(),
            &base,
            &format!("{base}/provider-models"),
        )
        .unwrap()
    }

    /// How a start against `boat` ended: a scheduled thread's (`StartLater`), or a `Launch` of
    /// a thread delegated from `parent` (or the user's own, with none).
    fn start_ended(
        boat: BoatClient,
        scheduled: bool,
        parent: Option<&str>,
    ) -> Result<String, String> {
        let catalog = flotilla_core::catalog::Catalog::from_value(
            &serde_json::from_str(
                &std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../m0-samples/provider-models.json"
                ))
                .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let mut f = fixture("reserve");
        f.worker.actions.boat = boat;
        let Target::New(fr) = fresh() else {
            unreachable!()
        };
        let req = NewThread {
            environment: fr.environment,
            project_id: None,
            provider: fr.provider,
            model: fr.model,
            effort: None,
            fast: false,
            title: None,
            machine_type: None,
            prompt: "Check CI".into(),
            attachments: vec![],
            wait_ready: false,
            on_branch: None,
            id: Some(uuid::Uuid::new_v4().to_string()),
            parent: parent.map(str::to_string),
            branch_tag: None,
        };
        let (t, local) = flotilla_core::lifecycle::begin_thread(&f.store, &catalog, &req).unwrap();
        let req = Box::new(req);
        f.worker.actions.spawn(if scheduled {
            Action::StartLater {
                thread: t.id,
                req,
                local,
            }
        } else {
            Action::Launch {
                thread: t.id,
                req,
                local,
            }
        });
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match f
                .rx
                .recv_timeout(until.saturating_duration_since(Instant::now()))
            {
                Ok(Event::Done {
                    action: Action::StartLater { .. } | Action::Launch { .. },
                    result,
                    ..
                }) => break result.map(|o| format!("{o:?}")),
                Ok(_) => continue,
                Err(_) => panic!("the start never ended"),
            }
        }
    }

    /// A scheduled thread's start asks boat for starts to spare as it starts: with only the
    /// reserve left it doesn't make a VM (a start of yours would).
    #[test]
    fn a_scheduled_start_keeps_the_reserve_as_it_starts() {
        // A plan whose starts aren't counted: boat gives no `remaining`.
        let unlimited = serde_json::json!({"canStart": true, "starts": {"unlimited": true, "hour": {"used": 1, "limit": 60}}});
        for (left, refused) in [
            (Some(flotilla_core::boat::START_RESERVE - 1), true),
            (Some(59), false),
            (None, false),
        ] {
            let boat = left.map_or_else(|| boat_with_limits(unlimited.clone()), boat_with_starts);
            let result = start_ended(boat, true, None);
            assert_eq!(
                result.as_ref().err().map(String::as_str) == Some(NO_STARTS),
                refused,
                "{left:?} left: {result:?}"
            );
        }
    }

    /// A delegated thread's start keeps the reserve too (nobody pressed anything for it); a
    /// start of yours doesn't.
    #[test]
    fn a_delegates_start_keeps_the_reserve() {
        let few = flotilla_core::boat::START_RESERVE - 1;
        let result = start_ended(boat_with_starts(few), false, Some("parent"));
        assert_eq!(
            result.as_ref().err().map(String::as_str),
            Some(NO_STARTS),
            "{result:?}"
        );
        let result = start_ended(boat_with_starts(59), false, Some("parent"));
        assert_ne!(
            result.as_ref().err().map(String::as_str),
            Some(NO_STARTS),
            "{result:?}"
        );
        let result = start_ended(boat_with_starts(few), false, None);
        assert_ne!(
            result.as_ref().err().map(String::as_str),
            Some(NO_STARTS),
            "yours: {result:?}"
        );
    }

    /// Run now sends without moving the next time; an edit made since the worker read the
    /// schedule isn't undone by its run.
    #[test]
    fn run_now_leaves_the_next_time() {
        let mut f = fixture("now");
        f.store
            .insert_thread(&thread("t1", ThreadState::Idle))
            .unwrap();
        let later = 1_900_000_000_000;
        f.store
            .put_schedule(
                &schedule(
                    "s1",
                    Target::Thread("t1".into()),
                    When::Daily { at: 9 * 60 },
                    Some(later),
                ),
                true,
            )
            .unwrap();
        f.worker.run_now("s1", 1_800_000_000_000);
        let (acts, _) = seen(&f.rx, Duration::from_secs(5));
        assert!(acts.iter().any(|a| matches!(a, Action::SendLater { .. })));
        let s = get(&f, "s1");
        assert_eq!((s.next_ms, s.runs), (Some(later), 1));
    }
}
