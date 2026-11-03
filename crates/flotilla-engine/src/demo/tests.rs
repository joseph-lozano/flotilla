use super::*;
use flotilla_proto::NewThread;

/// No waiting between steps.
const FAST: Pace = Pace {
    tick: Duration::ZERO,
    chars: 40,
    command: Duration::ZERO,
    pause: Duration::ZERO,
};
/// The seeded thread whose pull request (#19) is open.
const WITH_PR: &str = "99422fe0-da4d-4f86-a77d-f76f7bc32202";

/// Long enough between steps for what's sent next to arrive while a turn plays.
const SLOW: Pace = Pace {
    pause: Duration::from_millis(300),
    ..FAST
};

struct Fixture {
    dir: PathBuf,
    db: PathBuf,
    demo: Arc<dyn Fn(Cmd) + Send + Sync>,
    rx: mpsc::Receiver<Event>,
    /// What it said that no one has taken yet.
    log: Vec<Event>,
    req: u64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fixture(name: &str, pace: Pace) -> Fixture {
    let dir = std::env::temp_dir().join(format!("flotilla-demo-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    reset(&db).unwrap();
    let (tx, rx) = mpsc::channel();
    let demo = start(db.clone(), Events::new(move |e| tx.send(e).is_ok()), pace);
    Fixture {
        dir,
        db,
        demo,
        rx,
        log: Vec::new(),
        req: 0,
    }
}

impl Fixture {
    fn store(&self) -> Store {
        Store::open(&self.db).unwrap()
    }

    fn act(&mut self, action: Action) -> u64 {
        self.req += 1;
        (self.demo)(Cmd::Act {
            action,
            req: Some(self.req),
        });
        self.req
    }

    /// The first event it said (or says next) that `wanted` matches, taken from the log.
    fn take(&mut self, wanted: impl Fn(&Event) -> bool) -> Event {
        loop {
            if let Some(i) = self.log.iter().position(&wanted) {
                return self.log.remove(i);
            }
            let e = self
                .rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the demo went quiet");
            self.log.push(e);
        }
    }

    fn done(&mut self, req: u64) -> Result<Outcome, String> {
        match self.take(|e| matches!(e, Event::Done { req: Some(r), .. } if *r == req)) {
            Event::Done { result, .. } => result,
            _ => unreachable!(),
        }
    }

    fn finished(&mut self, thread: &str) {
        self.take(|e| matches!(e, Event::TurnFinished(t) if t == thread));
    }

    /// A new thread, archived as the host marks it, before the demo takes its start.
    fn launch_archived(&mut self) -> (String, u64) {
        self.launch_as(false, true)
    }

    /// A new thread, written as the host writes it, and started.
    fn launch(&mut self, project: bool) -> (String, u64) {
        self.launch_as(project, false)
    }

    fn launch_as(&mut self, project: bool, archived: bool) -> (String, u64) {
        let id = uuid::Uuid::new_v4().to_string();
        let store = self.store();
        let req = NewThread {
            environment: "base".into(),
            project_id: project.then(|| store.projects().unwrap()[0].id.clone()),
            provider: "claude-code".into(),
            model: "opus-5-5".into(),
            effort: None,
            fast: false,
            title: None,
            machine_type: None,
            prompt: "Add a jitter option to retry".into(),
            attachments: vec![],
            wait_ready: false,
            on_branch: None,
            id: Some(id.clone()),
            parent: None,
            branch_tag: None,
        };
        let catalog = crate::views::catalog(&store).unwrap();
        let (t, local) = flotilla_core::lifecycle::begin_thread(&store, &catalog, &req).unwrap();
        if archived {
            store.set_archived(&t.id, true).unwrap();
        }
        let n = self.act(Action::Launch {
            thread: t.id,
            req: Box::new(req),
            local,
        });
        (id, n)
    }
}

fn turns(store: &Store, thread: &str) -> Vec<(String, bool)> {
    store
        .turns(thread)
        .unwrap()
        .into_iter()
        .map(|t| (t.status, t.settled))
        .collect()
}

/// The seed is the video's threads, as this build reads them, with nothing of the machines
/// and accounts it was recorded on; a reset gives a standing code.
#[test]
fn the_seed_loads_clean() {
    let f = fixture("seed", FAST);
    let store = f.store();
    let threads = store.threads().unwrap();
    assert_eq!(threads.len(), 6);
    assert!(threads.iter().all(|t| t.state == ThreadState::Stopped));
    for t in &threads {
        assert!(!store.messages(&t.id).unwrap().is_empty(), "{}", t.title);
    }
    let pr = &store.pull_requests().unwrap()[WITH_PR].0;
    assert_eq!((pr.number, pr.state.as_str()), (19, "OPEN"));
    assert_eq!(store.projects().unwrap()[0].owner, LOGIN);
    let code = code(&store).unwrap();
    assert!(flotilla_proto::pair::normalize_code(&code).is_some());
    for leak in [
        "ghu_",
        "ghp_",
        "/Users/",
        ".ts.net",
        "box-node-",
        "push_token",
    ] {
        assert!(!SEED.contains(leak), "{leak}");
    }
}

/// A start puts the threads back and keeps who's paired: devices, their push registrations,
/// the standing code and the server's id.
#[test]
fn a_reset_keeps_who_is_paired() {
    let f = fixture("reset", FAST);
    let store = f.store();
    let code = code(&store).unwrap();
    let pairing = store.mint_code(Duration::from_secs(60), "cli", 1).unwrap();
    let paired = store
        .pair_device_noting(
            &pairing.code,
            "iPhone",
            "ios",
            Default::default(),
            None,
            2,
            None,
        )
        .unwrap();
    let token = format!("{}{}", flotilla_core::push::TOKEN_PREFIX, paired.device);
    store.kv_set(&token, "{}").unwrap();
    store.kv_set("server_id", "srv_demo").unwrap();
    store.kv_set("mentions:x", "{}").unwrap();
    let gone = store.threads().unwrap()[0].id.clone();
    store.delete_thread(&gone).unwrap();
    drop(store);
    reset(&f.db).unwrap();
    let store = f.store();
    assert_eq!(store.threads().unwrap().len(), 6);
    assert!(store.thread(&gone).unwrap().is_some());
    assert_eq!(store.devices(3).unwrap().len(), 1);
    assert!(store.kv_get(&token).unwrap().is_some());
    assert_eq!(
        store.kv_get("server_id").unwrap().as_deref(),
        Some("srv_demo")
    );
    assert_eq!(store.kv_get("mentions:x").unwrap(), None);
    assert_eq!(super::code(&store).unwrap(), code);
}

/// Never a database that isn't a demo's: one with threads is refused, as it is.
#[test]
fn a_reset_refuses_a_database_that_isnt_a_demos() {
    let dir = std::env::temp_dir().join(format!("flotilla-demo-mine-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("flotilla.db");
    reset(&db).unwrap();
    let store = Store::open(&db).unwrap();
    store.kv_delete(MARK_KEY).unwrap();
    drop(store);
    assert!(reset(&db).unwrap_err().contains("isn't a demo's"));
    let store = Store::open(&db).unwrap();
    assert_eq!(store.kv_get(MARK_KEY).unwrap(), None);
    assert_eq!(store.threads().unwrap().len(), 6);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A new thread plays its turn: words streamed, two commands shown running then with their
/// output, a reply that says it's the demo; then it's settled and said. A project thread's
/// opens a pull request.
#[test]
fn a_new_thread_plays_its_script() {
    let mut f = fixture("launch", FAST);
    let (thread, n) = f.launch(true);
    assert!(matches!(f.done(n), Ok(Outcome::Created(t)) if t == thread));
    f.finished(&thread);
    f.take(
        |e| matches!(e, Event::Live { partials, .. } if partials.iter().any(RawEvent::is_partial)),
    );
    let store = f.store();
    let messages = store.messages(&thread).unwrap();
    let tools: Vec<_> = messages
        .iter()
        .filter(|m| m.kind == "tool")
        .map(|m| (m.tool_name.as_deref(), m.pending))
        .collect();
    assert_eq!(tools, [(Some("Bash"), false), (Some("Bash"), false)]);
    let reply = messages.iter().rev().find(|m| m.kind == "text").unwrap();
    assert!(reply.text.as_deref().unwrap().contains("demo server"));
    assert_eq!(turns(&store, &thread), [(s("finished"), true)]);
    assert_eq!(
        store.thread(&thread).unwrap().unwrap().state,
        ThreadState::Idle
    );
    let pr = &store.pull_requests().unwrap()[&thread].0;
    assert_eq!((pr.number, pr.state.as_str()), (20, "OPEN"));
    // Not in a project: no pull request.
    let (other, n) = f.launch(false);
    f.done(n).unwrap();
    f.finished(&other);
    assert!(!f.store().pull_requests().unwrap().contains_key(&other));
}

/// A thread archived as it starts (the host marks it before the demo takes the start) stays
/// archived and plays nothing; a follow-up brings one back.
#[test]
fn an_archived_start_stays_archived() {
    let mut f = fixture("archived", SLOW);
    let (thread, n) = f.launch_archived();
    assert!(matches!(f.done(n), Ok(Outcome::Created(t)) if t == thread));
    let store = f.store();
    assert!(store.is_archived(&thread).unwrap());
    assert!(store.turns(&thread).unwrap().is_empty());
    let n = f.act(Action::Send {
        thread: thread.clone(),
        text: "Go on".into(),
        files: vec![],
        local: None,
    });
    f.done(n).unwrap();
    assert!(!store.is_archived(&thread).unwrap());
}

/// A message sent while a turn plays waits for it, then plays its own.
#[test]
fn a_follow_up_waits_for_the_turn_playing() {
    let mut f = fixture("follow", SLOW);
    let (thread, n) = f.launch(false);
    f.done(n).unwrap();
    let send = |text: &str| Action::Send {
        thread: thread.clone(),
        text: text.into(),
        files: vec![],
        local: None,
    };
    let a = f.act(send("And the docs?"));
    let b = f.act(send("Thanks"));
    assert!(matches!(f.done(a), Ok(Outcome::Sent(Sent::Prompt(_)))));
    f.done(b).unwrap();
    f.finished(&thread);
    f.finished(&thread);
    f.finished(&thread);
    let store = f.store();
    assert_eq!(turns(&store, &thread), vec![(s("finished"), true); 3]);
    let prompts: Vec<_> = store
        .messages(&thread)
        .unwrap()
        .into_iter()
        .filter(|m| m.kind == "prompt")
        .filter_map(|m| m.text)
        .collect();
    assert_eq!(
        prompts,
        ["Add a jitter option to retry", "And the docs?", "Thanks"]
    );
}

/// Stop the turn: it ends interrupted, and is said. Stop the VM: stopped, the waiting
/// turns with it, unsaid.
#[test]
fn interrupt_and_stop() {
    let mut f = fixture("stop", SLOW);
    let thread = f.store().threads().unwrap()[0].id.clone();
    let send = Action::Send {
        thread: thread.clone(),
        text: "Go on".into(),
        files: vec![],
        local: None,
    };
    // Sent and stopped at once: nothing has played yet.
    let a = f.act(send.clone());
    let b = f.act(Action::Interrupt(thread.clone()));
    f.done(a).unwrap();
    assert!(matches!(f.done(b), Ok(Outcome::Done)));
    f.finished(&thread);
    let store = f.store();
    let last = turns(&store, &thread).pop().unwrap();
    assert_eq!(last, (s("interrupted"), true));
    let a = f.act(send.clone());
    let b = f.act(send);
    let c = f.act(Action::Stop(thread.clone()));
    f.done(a).unwrap();
    f.done(b).unwrap();
    f.done(c).unwrap();
    assert_eq!(
        store.thread(&thread).unwrap().unwrap().state,
        ThreadState::Stopped
    );
    assert!(store.unsettled_turns(&thread).unwrap().is_empty());
}

/// Merge: the pull request merged, and the thread archived for it (Settings' default).
#[test]
fn merge_merges_and_archives() {
    let mut f = fixture("merge", FAST);
    let n = f.act(Action::MergePr(WITH_PR.into()));
    assert!(matches!(f.done(n), Ok(Outcome::Done)));
    assert!(matches!(
        f.take(|e| matches!(e, Event::PrMerged { .. })),
        Event::PrMerged {
            number: 19,
            archived: true,
            ..
        }
    ));
    let store = f.store();
    assert_eq!(store.pull_requests().unwrap()[WITH_PR].0.state, "MERGED");
    assert!(store.is_archived(WITH_PR).unwrap());
    // Again: it isn't open now.
    let n = f.act(Action::MergePr(WITH_PR.into()));
    assert!(f.done(n).is_err());
}

/// What needs boat or GitHub is refused plainly, and every command a client waits on gets
/// its answer.
#[test]
fn what_it_cant_do_is_refused_and_every_wait_answered() {
    let mut f = fixture("refuse", FAST);
    let thread = f.store().threads().unwrap()[0].id.clone();
    let n = f.act(Action::Preview(thread, 5173));
    assert_eq!(f.done(n).unwrap_err(), "Not in the demo: previews");
    let repo = flotilla_proto::RepoInfo {
        owner: "acme".into(),
        name: "x".into(),
        private: false,
        default_branch: "main".into(),
        pushed_at: None,
        description: None,
    };
    let asks = [
        Cmd::ReadUsage,
        Cmd::ReadAgentUsage { force: true },
        Cmd::TestSummaries,
        Cmd::Rebuild,
        Cmd::ListRepos { req: Some(1) },
        Cmd::Suggest {
            repo: Box::new(repo),
            req: Some(2),
        },
        Cmd::ListBranches {
            project: "p".into(),
            req: Some(3),
        },
        Cmd::CheckBranch {
            project: "p".into(),
            branch: flotilla_proto::OnBranch {
                name: "fix".into(),
                base: "main".into(),
            },
            prompt: None,
            req: Some(4),
        },
    ];
    for cmd in asks {
        // A command left unanswered times out here.
        eprintln!("{cmd:?}");
        (f.demo)(cmd);
        f.take(|e| {
            matches!(
                e,
                Event::Usage(Ok(_))
                    | Event::AgentUsage {
                        checked: Some(Ok(()))
                    }
                    | Event::Summaries(Err(_))
                    | Event::Rebuilt(Err(_))
                    | Event::Repos {
                        result: Err(_),
                        done: true,
                        ..
                    }
                    | Event::Suggested { result: Err(_), .. }
                    | Event::Branches { result: Err(_), .. }
                    | Event::BranchChecked { result: Err(_), .. }
            )
        });
    }
}

/// The standing code pairs any number of devices, each with a code of its own; nothing else
/// is taken for it.
#[test]
fn the_standing_code_pairs_any_number() {
    let f = fixture("code", FAST);
    let store = f.store();
    let code = flotilla_proto::pair::display_code(&code(&store).unwrap()).to_lowercase();
    for n in 0..3 {
        let fresh = standing(&store, &code, 10 + n).unwrap();
        store
            .pair_device_noting(
                &fresh.code,
                "iPhone",
                "ios",
                Default::default(),
                None,
                10 + n,
                None,
            )
            .unwrap();
    }
    assert_eq!(store.devices(20).unwrap().len(), 3);
    assert!(standing(&store, "AAAA-AAAA-AAAA", 30).is_none());
    // As typed, the standing code itself was spent by nothing.
    assert!(
        store
            .pair_device_noting(&code, "x", "ios", Default::default(), None, 40, None)
            .is_err()
    );
}

fn s(v: &str) -> String {
    v.to_string()
}

/// Only the turns playing set when it next wakes: one waiting behind another, due long ago,
/// doesn't keep it spinning.
#[test]
fn only_the_turns_playing_set_the_wait() {
    let turn = |thread: &str, due: Instant| Turn {
        thread: thread.into(),
        task: uuid::Uuid::new_v4().to_string(),
        conversation: String::new(),
        model: String::new(),
        prompt: String::new(),
        files: vec![],
        steps: vec![],
        at: 0,
        shown: 0,
        event: String::new(),
        event_ms: 0,
        due,
        first: false,
    };
    let now = Instant::now();
    let later = now + Duration::from_secs(5);
    let mut demo = Demo {
        db: PathBuf::new(),
        store: None,
        events: Events::new(|_| true),
        pace: FAST,
        turns: vec![turn("a", later), turn("a", now - Duration::from_secs(60))],
        last_ms: 0,
        closed: false,
    };
    assert_eq!(demo.next_due(), Some(later));
    demo.turns.push(turn("b", now));
    assert_eq!(demo.next_due(), Some(now));
}
