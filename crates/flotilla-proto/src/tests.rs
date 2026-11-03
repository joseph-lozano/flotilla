//! Every message through JSON and back. Each `*_variant` match has no wildcard, so a new
//! variant doesn't compile until it has a sample here too.

use crate::*;
use serde_json::json;
use std::collections::BTreeMap;

fn s(v: &str) -> String {
    v.to_string()
}

fn thread() -> Thread {
    Thread {
        id: s("t1"),
        environment_id: Some(s("e1")),
        environment_name: Some(s("base")),
        title: s("Fix the webhook"),
        sandbox_id: Some(s("bx_1")),
        conversation_id: Some(s("c1")),
        provider: s("claude-code"),
        model: s("haiku"),
        effort: Some(s("low")),
        fast: false,
        state: ThreadState::Idle,
        keep_running: false,
        archive_after: Some(s("2026-10-31T12:00:00.000Z")),
        last_stopped_at: None,
        data_expired_at: None,
        created_at: s("2026-10-31T11:00:00.000Z"),
        updated_at: s("2026-10-31T11:00:00.000Z"),
        repo_project_id: Some(s("p1")),
        branch: Some(s("fix-webhook")),
        base_sha: Some(s("abc")),
    }
}

fn project() -> Project {
    Project {
        id: s("p1"),
        owner: s("acme"),
        repo: s("web"),
        default_branch: s("main"),
        environment_name: s("flotilla-web"),
        created_at: s("2026-10-31T10:00:00.000Z"),
        template_sandbox_id: Some(s("bx_t")),
        template_sha: Some(s("abc")),
        template_built_at: None,
        prev_template_sandbox_id: None,
        head_sha: Some(s("def")),
        head_etag: None,
        checked_at: None,
        machine_type: None,
    }
}

fn run() -> SetupRun {
    SetupRun {
        id: s("r1"),
        project_id: s("p1"),
        kind: s("update"),
        trigger: s("main"),
        sha: s("def"),
        sandbox_id: None,
        state: s("failed"),
        step: Some(s("npm install")),
        error: Some(s("exit 1")),
        log: s("…"),
        started_at: s("2026-10-31T10:00:00.000Z"),
        finished_at: None,
    }
}

fn pr() -> PullRequest {
    PullRequest {
        number: 7,
        url: s("https://github.com/acme/web/pull/7"),
        title: s("Fix it"),
        head: s("fix-webhook"),
        base: s("main"),
        state: s("OPEN"),
        is_draft: false,
        mergeable: Some(s("MERGEABLE")),
        review: None,
        additions: 3,
        deletions: 1,
        checks: vec![Check {
            name: s("ci"),
            state: s("pass"),
            url: None,
            description: Some(s("ok")),
        }],
        updated_at: None,
        merged_at: None,
    }
}

fn summary() -> ThreadSummary {
    ThreadSummary {
        thread: thread(),
        activity: Activity::Unread,
        last_activity_ms: 5,
        turn_started_ms: None,
        last_reply: Some(s("Done")),
        last_prompt: None,
        pr: Some((pr(), false)),
        start_failed: false,
        prompted_ms: Some(4),
        opens: Some(crate::search::Opens {
            at: 3,
            score: 1.5,
            picks: vec![crate::search::SearchPick {
                query: s("retry"),
                at: 3,
            }],
        }),
    }
}

fn file() -> Attached {
    Attached {
        path: "/tmp/a.png".into(),
        name: s("a.png"),
        size: 10,
        kind: Kind::Image,
        blob: format!("b_{}", "ab".repeat(32)),
        from: Some(s("0123456789abcdef")),
    }
}

fn new_thread() -> NewThread {
    NewThread {
        environment: s("base"),
        project_id: None,
        provider: s("claude-code"),
        model: s("haiku"),
        effort: None,
        fast: false,
        title: None,
        machine_type: None,
        prompt: s("Hello"),
        attachments: vec![file()],
        wait_ready: false,
        on_branch: Some(OnBranch {
            name: s("b"),
            base: s("main"),
        }),
        id: Some(s("7f3e0000-0000-4000-8000-000000000000")),
        parent: None,
        branch_tag: None,
    }
}

fn repo() -> RepoInfo {
    RepoInfo {
        owner: s("acme"),
        name: s("web"),
        private: true,
        default_branch: s("main"),
        pushed_at: None,
        description: None,
    }
}

fn mentions() -> Mentions {
    Mentions {
        entries: vec![Entry {
            kind: s("skill"),
            name: s("council"),
            description: s("ask"),
            harness: s("claude-code"),
            insert: s("/council"),
            shadowed: false,
        }],
        root: s("/home/user/web"),
        files: vec![s("src/main.rs")],
    }
}

fn catalog() -> Catalog {
    Catalog::from_value(&json!({
        "claude-code": {
            "cli": {"name": "claude", "aliases": ["claude-code"], "order": 1},
            "default": "haiku",
            "models": [{"id": "haiku", "label": "Haiku 4.5", "reasoningEffort": {"supported": ["low", "high"], "default": "low"}}]
        }
    }))
    .unwrap()
}

fn requests() -> Vec<Request> {
    let t = || s("t1");
    vec![
        Request::NewThread(Box::new(new_thread())),
        Request::Retry(t()),
        Request::Send {
            thread: t(),
            text: s("also tests"),
            files: vec![file()],
            local: s("local-1"),
        },
        Request::Answer {
            thread: t(),
            ask: s("call_4e64"),
            answers: vec![vec![s("beta.txt")], vec![s("lint"), s("test")]],
            local: s("local-2"),
        },
        Request::DiscardUnsent {
            thread: t(),
            id: s("unsent-1"),
        },
        Request::Interrupt(t()),
        Request::Stop(t()),
        Request::Resume(t()),
        Request::Rename {
            thread: t(),
            title: s("New name"),
        },
        Request::KeepRunning {
            thread: t(),
            on: true,
        },
        Request::Preview {
            thread: t(),
            port: 3000,
        },
        Request::MakePublic {
            thread: t(),
            port: 3000,
            for_: crate::PublicFor::Hour,
        },
        Request::MakePublic {
            thread: t(),
            port: 4100,
            for_: crate::PublicFor::UntilStopped,
        },
        Request::MakePrivate {
            thread: t(),
            port: 3000,
        },
        Request::Archive(t()),
        Request::Unarchive(t()),
        Request::Delete(t()),
        Request::Forget(t()),
        Request::MarkViewed(t()),
        Request::Opened {
            thread: t(),
            query: Some(s("retry")),
        },
        Request::Opened {
            thread: t(),
            query: None,
        },
        Request::MarkUnread(t()),
        Request::SuggestName(t()),
        Request::OnBranch {
            thread: t(),
            prompt: Some(s("fix the checks")),
        },
        Request::ReadMentions(t()),
        Request::MergePr(t()),
        Request::MarkReady(t()),
        Request::LinkPr {
            thread: t(),
            number: 7,
        },
        Request::ReviewPrompt(t()),
        Request::AddProject {
            repo: Box::new(repo()),
            scripts: vec![SetupScript::every("npm ci")],
        },
        Request::RemoveProject(s("p1")),
        Request::SetScripts {
            project: s("p1"),
            scripts: vec![SetupScript {
                command: s("make"),
                fresh_only: true,
            }],
            run: true,
        },
        Request::SetMachineType {
            project: s("p1"),
            machine_type: Some(s("large")),
        },
        Request::ProjectMentions(s("p1")),
        Request::RunSetup {
            project: s("p1"),
            fresh: true,
        },
        Request::CancelSetup(s("p1")),
        Request::SetupFixPrompt {
            project: s("p1"),
            run: s("r1"),
        },
        Request::Services(ServiceCmd::Ensure(t())),
        Request::Services(ServiceCmd::Restart {
            thread: t(),
            name: s("web"),
        }),
        Request::Services(ServiceCmd::Stop {
            thread: t(),
            name: s("web"),
        }),
        Request::CheckGitHub,
        Request::ListRepos,
        Request::CachedRepos,
        Request::Suggest(Box::new(repo())),
        Request::ListBranches(s("p1")),
        Request::SyncNow,
        Request::Rebuild,
        Request::TestSummaries,
        Request::Set(Setting::Defaults(ThreadSettings {
            provider: s("codex"),
            model: s("gpt"),
            effort: None,
            fast: true,
        })),
        Request::Set(Setting::IdleMinutes(30)),
        Request::Set(Setting::BranchPrefix(s("flotilla/"))),
        Request::Set(Setting::SummaryModel(String::new())),
        Request::SetBoatKey(Secret(s("boat_test"))),
        Request::SetInstructions(s("Use pnpm.")),
        Request::PutSkill {
            skill: Skill {
                name: s("council"),
                description: s("ask"),
                folder: "/Users/me/.claude/skills/council".into(),
            },
            blob: format!("b_{}", "cd".repeat(32)),
        },
        Request::RemoveSkill(s("council")),
        Request::RunDoctor,
        Request::ReadUsage,
        Request::ReadAgentUsage,
        Request::PairCode {
            ttl_secs: Some(600),
        },
        Request::PairCode { ttl_secs: None },
        Request::RevokeDevice(s("0123456789abcdef")),
        Request::RenameDevice {
            device: s("0123456789abcdef"),
            name: s("Work phone"),
        },
        Request::RegisterPush(PushToken {
            token: Secret("ab".repeat(32)),
            env: s("sandbox"),
            topic: s("com.useflotilla.flotilla"),
            relay: None,
        }),
        Request::RegisterPush(PushToken {
            token: Secret("ab".repeat(32)),
            env: s("production"),
            topic: s("com.useflotilla.flotilla"),
            relay: Some(crate::PushRelay {
                url: s("https://push.useflotilla.com"),
                handle: s("giLjuhbbFalUBROEtNqwBg"),
                secret: Secret("s".repeat(43)),
                key: Secret(s("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=")),
            }),
        }),
        Request::UnregisterPush,
        Request::Search {
            query: s("retry loop"),
            limit: Some(20),
        },
        Request::Search {
            query: s("café"),
            limit: None,
        },
        Request::PutSchedule(Box::new(schedule().spec())),
        Request::PutSchedule(Box::new(crate::ScheduleSpec {
            target: crate::Target::New(fresh()),
            when: crate::When::Once {
                at_ms: 1_800_000_000_000,
            },
            ..schedule().spec()
        })),
        Request::PauseSchedule {
            schedule: s("s1"),
            paused: true,
        },
        Request::DeleteSchedule(s("s1")),
        Request::RunSchedule(s("s1")),
    ]
}

/// An agent (`Scope::Agent`) may read, start, send, answer, interrupt, archive, open a preview
/// and sign itself out: every other request is refused, files and hand-offs too, and making a
/// link public or private, for any time.
#[test]
fn an_agent_may_send_only_its_few_requests() {
    let me = "0123456789abcdef";
    for r in requests() {
        let may = match &r {
            Request::Answer { .. }
            | Request::Interrupt(_)
            | Request::Archive(_)
            | Request::Preview { .. }
            | Request::Search { .. } => true,
            Request::RevokeDevice(d) => d == me,
            // The samples carry files.
            _ => false,
        };
        let why = r.agent_refusal(Some(me));
        assert_eq!(why.is_none(), may, "{r:?}: {why:?}");
        if let Some(why) = why {
            assert!(why.starts_with("Agents can't"), "{why}");
        }
    }
    let plain = NewThread {
        attachments: vec![],
        on_branch: None,
        ..new_thread()
    };
    assert_eq!(
        Request::NewThread(Box::new(plain.clone())).agent_refusal(Some(me)),
        None
    );
    assert_eq!(
        Request::NewThread(Box::new(plain.clone())).agent_refusal(None),
        None
    );
    assert!(
        Request::NewThread(Box::new(NewThread {
            parent: Some(s("t0")),
            ..plain
        }))
        .agent_refusal(Some(me))
        .is_some()
    );
    assert!(
        Request::NewThread(Box::new(new_thread()))
            .agent_refusal(Some(me))
            .unwrap()
            .contains("files")
    );
    assert_eq!(
        Request::Send {
            thread: s("t1"),
            text: s("go on"),
            files: vec![],
            local: s("local-1")
        }
        .agent_refusal(Some(me)),
        None
    );
    // Signing out another device, or this machine (no device) signing out anyone.
    assert!(
        Request::RevokeDevice(s("fedcba9876543210"))
            .agent_refusal(Some(me))
            .is_some()
    );
    assert!(Request::RevokeDevice(s(me)).agent_refusal(None).is_some());
    assert_eq!(
        Request::RevokeDevice(format!(" {me} ")).agent_refusal(Some(me)),
        None
    );
    for for_ in crate::PublicFor::ALL {
        let why = Request::MakePublic {
            thread: s("t1"),
            port: 3000,
            for_,
        }
        .agent_refusal(Some(me));
        assert_eq!(
            why.as_deref(),
            Some("Agents can't make the link public"),
            "{for_:?}"
        );
    }
    assert_eq!(
        Request::MakePrivate {
            thread: s("t1"),
            port: 3000
        }
        .agent_refusal(None)
        .as_deref(),
        Some("Agents can't make the link private")
    );
}

#[test]
fn an_agent_may_see_the_threads_and_models_only() {
    for (view, may) in [
        (ViewKey::Sidebar, true),
        (ViewKey::Catalog, true),
        (ViewKey::Thread(s("t1")), true),
        (ViewKey::Settings, false),
        (ViewKey::Devices, false),
        (ViewKey::Project(s("p1")), false),
        (ViewKey::Mentions(s("t1")), false),
        (ViewKey::AgentUsage, false),
        (ViewKey::Schedules, false),
    ] {
        assert_eq!(view.agent_may(), may, "{view:?}");
    }
}

fn fresh() -> crate::Fresh {
    crate::Fresh {
        environment: s("flotilla-web"),
        project_id: Some(s("p1")),
        provider: s("claude-code"),
        model: s("claude-haiku-5-5"),
        effort: Some(s("low")),
        fast: false,
        machine_type: None,
        on_branch: Some(OnBranch {
            name: s("fix"),
            base: s("main"),
        }),
    }
}

/// A schedule of each timing and target, with each run's outcome.
fn schedules() -> Vec<crate::Schedule> {
    use crate::{Run, RunOutcome, When};
    let whens = [
        When::Once {
            at_ms: 1_800_000_000_000,
        },
        // As made before runs had to be a day apart: still read.
        When::Hours {
            every: 3,
            from_ms: 1_800_000_000_000,
        },
        When::Daily { at: 540 },
        When::Weekdays { at: 510 },
        When::Weekly {
            days: vec![1, 3, 7],
            at: 1_200,
        },
        When::Cron {
            expr: s("30 9 * * 1-5"),
        },
    ];
    whens
        .into_iter()
        .enumerate()
        .map(|(i, when)| crate::Schedule {
            id: format!("s{i}"),
            target: if i % 2 == 0 {
                crate::Target::Thread(s("t1"))
            } else {
                crate::Target::New(fresh())
            },
            when,
            paused: i == 3,
            waiting_ms: (i == 2).then_some(1_800_000_000_000),
            last: match i % 3 {
                0 => Some(Run {
                    at_ms: 1,
                    outcome: RunOutcome::Sent,
                    thread: Some(s("t1")),
                    why: None,
                }),
                1 => Some(Run {
                    at_ms: 2,
                    outcome: RunOutcome::Failed,
                    thread: None,
                    why: Some(s("boat has no starts to spare")),
                }),
                _ => Some(Run {
                    at_ms: 3,
                    outcome: RunOutcome::Missed,
                    thread: None,
                    why: None,
                }),
            },
            ..schedule()
        })
        .collect()
}

fn schedule() -> crate::Schedule {
    crate::Schedule {
        id: s("s1"),
        prompt: s("Check the overnight CI runs"),
        target: crate::Target::Thread(s("t1")),
        when: crate::When::Daily { at: 540 },
        zone: s("Europe/London"),
        paused: false,
        next_ms: Some(1_800_000_000_000),
        waiting_ms: None,
        last: None,
        runs: 4,
        from: s("MacBook"),
        created_ms: 1,
        updated_ms: 2,
    }
}

fn request_variant(r: &Request) -> usize {
    match r {
        Request::NewThread(_) => 0,
        Request::Retry(_) => 1,
        Request::Send { .. } => 2,
        Request::DiscardUnsent { .. } => 3,
        Request::Interrupt(_) => 4,
        Request::Stop(_) => 5,
        Request::Resume(_) => 6,
        Request::Rename { .. } => 7,
        Request::KeepRunning { .. } => 8,
        Request::Preview { .. } => 9,
        Request::Archive(_) => 10,
        Request::Unarchive(_) => 11,
        Request::Delete(_) => 12,
        Request::Forget(_) => 13,
        Request::MarkViewed(_) => 14,
        Request::MarkUnread(_) => 15,
        Request::SuggestName(_) => 16,
        Request::OnBranch { .. } => 17,
        Request::ReadMentions(_) => 18,
        Request::MergePr(_) => 19,
        Request::MarkReady(_) => 20,
        Request::LinkPr { .. } => 21,
        Request::ReviewPrompt(_) => 22,
        Request::AddProject { .. } => 23,
        Request::RemoveProject(_) => 24,
        Request::SetScripts { .. } => 25,
        Request::ProjectMentions(_) => 26,
        Request::RunSetup { .. } => 27,
        Request::CancelSetup(_) => 28,
        Request::SetupFixPrompt { .. } => 29,
        Request::Services(_) => 30,
        Request::CheckGitHub => 31,
        Request::ListRepos => 32,
        Request::CachedRepos => 33,
        Request::Suggest(_) => 34,
        Request::ListBranches(_) => 35,
        Request::SyncNow => 36,
        Request::Rebuild => 37,
        Request::TestSummaries => 38,
        Request::Set(_) => 39,
        Request::SetBoatKey(_) => 40,
        Request::SetInstructions(_) => 41,
        Request::PutSkill { .. } => 42,
        Request::PairCode { .. } => 43,
        Request::RevokeDevice(_) => 44,
        Request::RemoveSkill(_) => 45,
        Request::RunDoctor => 46,
        Request::RegisterPush(_) => 47,
        Request::UnregisterPush => 48,
        Request::RenameDevice { .. } => 49,
        Request::ReadUsage => 50,
        Request::Search { .. } => 51,
        Request::Opened { .. } => 52,
        Request::ReadAgentUsage => 53,
        Request::PutSchedule(_) => 54,
        Request::PauseSchedule { .. } => 55,
        Request::DeleteSchedule(_) => 56,
        Request::RunSchedule(_) => 57,
        Request::Answer { .. } => 58,
        Request::MakePublic { .. } => 59,
        Request::MakePrivate { .. } => 60,
        Request::SetMachineType { .. } => 61,
    }
}

fn replies() -> Vec<Reply> {
    vec![
        Reply::Done,
        Reply::Already,
        Reply::Created(s("t2")),
        Reply::Sent(Sent::Prompt(s("p1"))),
        Reply::Sent(Sent::Steer(s("s1"))),
        Reply::Url(s("https://x.boat.dev/?_token=…")),
        Reply::Prompt(s("Address the review")),
        Reply::Name(s("Webhook backoff")),
        Reply::Mentions(Some(mentions())),
        Reply::Mentions(None),
        Reply::Repos(vec![repo()]),
        Reply::Code(crate::PairingCode {
            code: s("ABCDEFGHJKMN"),
            expires_ms: 1_800_000_000_000,
            link: s("flotilla://pair?u=http%3A%2F%2F127.0.0.1%3A7777#ABCDEFGHJKMN"),
        }),
        Reply::Hits(vec![
            SearchHit {
                thread: s("t1"),
                title: s("Fix the webhook"),
                kind: HitKind::Title,
                seq: None,
                snippet: s("Fix the webhook"),
                marks: vec![[8, 15]],
                at: Some(1_800_000_000_000),
                from: None,
                hits: 0,
            },
            SearchHit {
                thread: s("t1"),
                title: s("Fix the webhook"),
                kind: HitKind::Tool,
                seq: Some(51),
                snippet: s("…curl the webhook"),
                marks: vec![],
                at: None,
                from: Some(s("From Webhook review")),
                hits: 2,
            },
        ]),
        Reply::Hits(vec![]),
        Reply::Push(crate::PushMode {
            relay: Some(s("https://push.useflotilla.com")),
        }),
        Reply::Push(crate::PushMode::default()),
    ]
}

fn reply_variant(r: &Reply) -> usize {
    match r {
        Reply::Done => 0,
        Reply::Already => 1,
        Reply::Created(_) => 2,
        Reply::Sent(_) => 3,
        Reply::Url(_) => 4,
        Reply::Prompt(_) => 5,
        Reply::Name(_) => 6,
        Reply::Mentions(_) => 7,
        Reply::Repos(_) => 8,
        Reply::Code(_) => 9,
        Reply::Hits(_) => 10,
        Reply::Push(_) => 11,
    }
}

fn detail() -> ThreadDetail {
    ThreadDetail {
        id: s("t1"),
        thread: Some(thread()),
        messages: vec![
            Message {
                seq: 50,
                kind: s("prompt"),
                text: Some(s("Hello")),
                attachments: vec![s("a.png")],
                ..Default::default()
            },
            Message {
                seq: 51,
                kind: s("tool"),
                tool_result_json: Some(s(r#"{"content":"zz…"}"#)),
                cut: vec![crate::Cut {
                    field: s("tool_result_json"),
                    bytes: 90_000,
                    mark: s("00ff00ff00ff00ff"),
                }],
                ..Default::default()
            },
        ],
        older: 50,
        pages: vec![
            crate::PageMark {
                page: 0,
                mark: s("0123456789abcdef"),
            },
            crate::PageMark {
                page: 1,
                mark: s("fedcba9876543210"),
            },
        ],
        sent: vec![
            SentPrompt {
                id: s("local-1"),
                kind: s("prompt"),
                text: s("Hello"),
                sent_at: s("now"),
                attachments: vec![file()],
                schedule: None,
            },
            SentPrompt {
                id: s("p2"),
                kind: s("prompt"),
                text: s("Check CI"),
                sent_at: s("later"),
                attachments: vec![],
                schedule: Some(s("s1")),
            },
        ],
        start_error: None,
        stages: Some(StartStages {
            done: vec![s("vm")],
            current: Some(s("setup")),
        }),
        project: Some(ThreadProject {
            project: project(),
            behind: Some(2),
            into: s("main"),
        }),
        resume: Some(HookRun {
            sandbox: s("bx_1"),
            pid: 3,
            started_at: s("now"),
            state: s("done"),
            exit_code: Some(0),
            output: s("ok"),
            out_at: 2,
            err_at: 0,
        }),
        archive: None,
        setup_failure: Some(ScriptFailure {
            label: s("npm ci"),
            exit_code: Some(1),
            timed_out: false,
            output: s("boom"),
        }),
        services: Some(ThreadServices {
            sandbox: s("bx_1"),
            file: Some(s(".agents/services.yml")),
            error: None,
            services: vec![ServiceRun {
                name: s("web"),
                port: 3000,
                state: s("ready"),
                links: vec![Link {
                    title: s("Home"),
                    url: s("/"),
                }],
                ..Default::default()
            }],
        }),
        public: vec![crate::PublicLink {
            port: 3000,
            url: s("https://bx1-3000.example.com"),
            since_ms: 1,
            until_ms: Some(3_600_001),
        }],
        lineage: Some(Lineage {
            subagents: vec![Subagent {
                id: s("a"),
                latest: Some((s("Bash"), s("ls"))),
                ..Default::default()
            }],
        }),
        parent: Some((s("t0"), s("Parent"))),
        children: vec![ChildCard {
            id: s("t2"),
            title: s("Review"),
            provider: s("codex"),
            model: s("gpt"),
            state: ChildState::Working,
            task: s("Review it"),
            calls: 2,
            latest: Some((s("Bash"), s("cargo test"))),
            answer: None,
            started_ms: Some(1),
            duration_ms: None,
            thread: false,
        }],
        context: Some(crate::ContextUse {
            used: 23_767,
            window: Some(1_000_000),
            model: Some(s("claude-haiku-5-5")),
            account: Some(s("claude")),
            at_ms: Some(7),
        }),
    }
}

pub(crate) fn sample_views() -> Vec<View> {
    vec![
        View::Sidebar(Sidebar {
            threads: vec![summary()],
            parents: BTreeMap::from([(s("t2"), s("t1"))]),
            waiting: vec![s("t1")],
            projects: vec![project()],
            environments: vec![Environment {
                id: s("e1"),
                name: s("base"),
                is_default: true,
                updated_at: s("now"),
            }],
            status: SyncStatus {
                error: Some(s("offline")),
                syncing: true,
            },
            github: GhStatus::SignedIn {
                login: s("me"),
                version: s("2.81"),
                scopes: vec![s("repo")],
            },
            limits: vec![crate::AccountLimits {
                account: s("claude"),
                plan: Some(s("max")),
                windows: vec![crate::LimitWindow {
                    id: s("seven_day"),
                    label: s("Weekly"),
                    used: 92,
                    resets_at: Some(s("2026-11-01T15:00:00Z")),
                    minutes: Some(10080),
                    scope: None,
                    earlier: None,
                }],
                read_ms: 6,
            }],
            usage: Some(Usage {
                vms: Some((1, 2)),
                renews: Some(s("soon")),
                starts_free: Some(s("later")),
                extra_starts: Some(ExtraStarts {
                    on: true,
                    used: 3,
                    spend_cents: Some((15, 8000)),
                    cap_reached: false,
                }),
                read_ms: Some(5),
                ..Default::default()
            }),
            engine: EngineState::Stopped(s("no key")),
        }),
        View::Catalog(Some(catalog())),
        View::Catalog(None),
        View::Settings(Settings {
            defaults: Some(ThreadSettings {
                provider: s("claude-code"),
                model: s("haiku"),
                effort: Some(s("low")),
                fast: false,
            }),
            idle_minutes: 30,
            branch_prefix: s("flotilla/"),
            summary_model: s("claude-haiku-5-5"),
            archive_on_merge: true,
            archive_idle_days: 7,
            summaries: SummaryStatus::Failed(s("no claude")),
            instructions: s("Use pnpm."),
            skills: vec![UserSkill {
                name: s("council"),
                description: s("ask"),
                blob: format!("b_{}", "cd".repeat(32)),
                size: 2048,
                folder: "/tmp/council".into(),
                from: Some(s("0123456789abcdef")),
                from_name: s("MacBook"),
                added_ms: 1_800_000_000_000,
            }],
            skills_error: None,
            projects: vec![ProjectRow {
                project: project(),
                last_run: Some(run()),
            }],
            database: s("/tmp/db"),
            boat_key: Some(s("ab12")),
            doctor: Some(Doctor {
                checks: vec![
                    Diagnosis {
                        check: CheckOf::BoatKey,
                        health: Health::Ok,
                        says: s("Works, ends …ab12"),
                        fix: vec![],
                    },
                    Diagnosis {
                        check: CheckOf::GitHub,
                        health: Health::Fail,
                        says: s("gh isn't signed in"),
                        fix: vec![s("gh auth login --hostname github.com")],
                    },
                    Diagnosis {
                        check: CheckOf::Claude,
                        health: Health::Warn,
                        says: s("slow"),
                        fix: vec![],
                    },
                    Diagnosis {
                        check: CheckOf::Tailscale,
                        health: Health::Off,
                        says: s("Not set up"),
                        fix: vec![s("flotilla setup --tailscale-serve")],
                    },
                    Diagnosis {
                        check: CheckOf::Database,
                        health: Health::Ok,
                        says: s("Writable"),
                        fix: vec![],
                    },
                ],
                at_ms: 1_800_000_000_000,
            }),
        }),
        View::Settings(Settings {
            defaults: None,
            idle_minutes: 5,
            branch_prefix: String::new(),
            summary_model: String::new(),
            archive_on_merge: false,
            archive_idle_days: 0,
            summaries: SummaryStatus::Unknown,
            instructions: String::new(),
            skills: vec![],
            skills_error: Some(s("too big")),
            projects: vec![],
            database: s("/tmp/db"),
            boat_key: None,
            doctor: None,
        }),
        View::Thread(Box::new(detail())),
        View::Mentions {
            thread: s("t1"),
            mentions: Some(mentions()),
        },
        View::Project(ProjectDetail {
            id: s("p1"),
            project: Some(project()),
            runs: vec![run()],
            scripts: vec![SetupScript::every("npm ci")],
            github_error: Some(s("403")),
        }),
        View::Devices(crate::Devices {
            devices: vec![
                crate::DeviceRow {
                    id: s("0123456789abcdef"),
                    name: s("MacBook"),
                    platform: s("macos"),
                    paired_ms: 1,
                    last_seen_ms: 2,
                    connected: true,
                    scope: crate::Scope::Full,
                },
                crate::DeviceRow {
                    id: s("fedcba9876543210"),
                    name: s("Phone"),
                    platform: s("ios"),
                    paired_ms: 3,
                    last_seen_ms: 4,
                    connected: false,
                    scope: crate::Scope::Agent,
                },
            ],
        }),
        View::Devices(crate::Devices::default()),
        View::AgentUsage(crate::AgentUsage {
            hours: vec![
                crate::UsageHour {
                    hour_ms: 1_793_538_000_000,
                    provider: s("claude-code"),
                    model: s("claude-haiku-5-5"),
                    speed: crate::Speed::Fast,
                    tokens: crate::TokenTotals {
                        input: 2,
                        cache_read: 15_615,
                        cache_write: 4_792,
                        output: 91,
                        reasoning: 0,
                    },
                    requests: 1,
                    cost: Some(crate::UsageCost {
                        input: 0.25,
                        cache_read: 0.125,
                        cache_write: 0.5,
                        output: 1.5,
                        other: 0.,
                        premium: Some(1.25),
                        saved: 0.0625,
                    }),
                },
                crate::UsageHour {
                    hour_ms: 1_793_541_600_000,
                    provider: s("codex"),
                    model: s("gpt-x"),
                    speed: crate::Speed::Standard,
                    tokens: crate::TokenTotals::default(),
                    requests: 2,
                    cost: None,
                },
            ],
            read_ms: Some(1_793_540_000_000),
            prices_ms: None,
        }),
        View::AgentUsage(crate::AgentUsage::default()),
        View::Schedules(crate::Schedules {
            schedules: schedules(),
        }),
        View::Schedules(crate::Schedules::default()),
    ]
}

fn view_variant(v: &View) -> usize {
    match v {
        View::Sidebar(_) => 0,
        View::Catalog(_) => 1,
        View::Settings(_) => 2,
        View::Thread(_) => 3,
        View::Mentions { .. } => 4,
        View::Project(_) => 5,
        View::Devices(_) => 6,
        View::AgentUsage(_) => 7,
        View::Schedules(_) => 8,
    }
}

fn raw_event() -> RawEvent {
    RawEvent::from_value(
        json!({"id": "e1", "type": "response", "timestamp": 5, "data": {"content": "Hel", "is_streaming": true}}),
    )
}

fn notes() -> Vec<Note> {
    vec![
        Note::Done {
            request: Request::Send {
                thread: s("t1"),
                text: s("[flotilla] done"),
                files: vec![],
                local: String::new(),
            },
            result: Err(s("409")),
        },
        Note::TurnFinished(s("t1")),
        Note::Asked(s("t1")),
        Note::Summaries(Ok(s("2026-10-31T12:00:00Z"))),
        Note::Summaries(Err(s("no claude"))),
        Note::GitHub(GhStatus::NotSignedIn { version: s("2.81") }),
        Note::Unsent(vec![s("t1")]),
        Note::PrMerged {
            thread: s("t1"),
            number: 7,
            title: s("Fix"),
            branch: s("main"),
            archived: true,
        },
        Note::IdleArchived(vec![s("t1"), s("t2")]),
        Note::SetupDone {
            project: s("p1"),
            name: s("acme/web"),
            ok: false,
            error: Some(s("exit 1")),
            recovered: false,
            fixable: Some(s("r1")),
        },
        Note::Repos {
            result: Ok(vec![repo()]),
            done: false,
            login: None,
        },
        Note::Repos {
            result: Err(s("gh")),
            done: true,
            login: Some(s("me")),
        },
        Note::Suggested {
            repo: s("acme/web"),
            result: Ok(Suggestions {
                scripts: vec![SetupScript::every("npm ci")],
                agents_setup: false,
            }),
        },
        Note::Branches {
            project: s("p1"),
            result: Ok(BranchChoices {
                prs: vec![OpenPr {
                    number: 7,
                    title: s("x"),
                    url: s("u"),
                    head: s("h"),
                    base: s("b"),
                    draft: true,
                    from_fork: false,
                }],
                branches: vec![s("main")],
            }),
        },
        Note::BranchChecked {
            project: s("p1"),
            branch: OnBranch {
                name: s("b"),
                base: s("main"),
            },
            prompt: None,
            result: Ok(()),
        },
        Note::Rebuilt(Ok(s("/tmp/db.bak"))),
        Note::Alert {
            id: s("e1-1"),
            alert: Alert {
                kind: AlertKind::SetupFailed,
                title: s("acme/web"),
                body: s("Setup failed."),
                thread: None,
                about: s("p1"),
            },
        },
        Note::ScheduleFailed {
            schedule: s("s1"),
            prompt: s("Check CI"),
            thread: Some(s("t1")),
            why: s("The thread is archived."),
        },
        Note::Alert {
            id: s("e1-2"),
            alert: Alert {
                kind: AlertKind::ScheduleFailed,
                title: s("Check CI"),
                body: s("Didn't run: the thread is archived."),
                thread: Some(s("t1")),
                about: s("s1"),
            },
        },
        Note::Alert {
            id: s("e1-3"),
            alert: Alert {
                kind: AlertKind::Asked,
                title: s("Fix the webhook"),
                body: s("Asks: Which file name?"),
                thread: Some(s("t1")),
                about: s("t1"),
            },
        },
    ]
}

fn note_variant(n: &Note) -> usize {
    match n {
        Note::Done { .. } => 0,
        Note::TurnFinished(_) => 1,
        Note::Summaries(_) => 2,
        Note::GitHub(_) => 3,
        Note::Unsent(_) => 4,
        Note::PrMerged { .. } => 5,
        Note::SetupDone { .. } => 6,
        Note::Repos { .. } => 7,
        Note::Suggested { .. } => 8,
        Note::Branches { .. } => 9,
        Note::BranchChecked { .. } => 10,
        Note::Rebuilt(_) => 11,
        Note::IdleArchived(_) => 12,
        Note::Alert { .. } => 13,
        Note::ScheduleFailed { .. } => 14,
        Note::Asked(_) => 15,
    }
}

fn view_keys() -> Vec<ViewKey> {
    vec![
        ViewKey::Sidebar,
        ViewKey::Catalog,
        ViewKey::Settings,
        ViewKey::Thread(s("t1")),
        ViewKey::Mentions(s("t1")),
        ViewKey::Project(s("p1")),
        ViewKey::Devices,
        ViewKey::AgentUsage,
        ViewKey::Schedules,
    ]
}

fn covers<T>(items: &[T], variant: impl Fn(&T) -> usize, count: usize) {
    let seen: std::collections::BTreeSet<usize> = items.iter().map(variant).collect();
    assert_eq!(seen, (0..count).collect(), "a variant has no sample");
}

fn back<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(x: T) {
    let y = through_json(&x).unwrap_or_else(|e| panic!("{x:?}: {e}"));
    assert_eq!(x, y);
}

#[test]
fn every_message_round_trips_through_json() {
    covers(&requests(), request_variant, 62);
    covers(&replies(), reply_variant, 12);
    covers(&sample_views(), view_variant, 9);
    covers(&notes(), note_variant, 16);
    for (id, req) in (1..).zip(requests()) {
        back(ClientMsg::Req {
            id,
            req: req.clone(),
        });
        back(ServerMsg::Res {
            id,
            result: Ok(Reply::Done),
        });
        back(ServerMsg::Res {
            id,
            result: Err(s("Couldn't")),
        });
        back(ServerMsg::Note(Note::Done {
            request: req,
            result: Ok(Reply::Created(s("x"))),
        }));
    }
    for reply in replies() {
        back(ServerMsg::Res {
            id: 1,
            result: Ok(reply),
        });
    }
    for view in sample_views() {
        back(ServerMsg::Snap {
            view: Box::new(view),
        });
    }
    for note in notes() {
        back(ServerMsg::Note(note));
    }
    for view in view_keys() {
        back(ClientMsg::Sub { view: view.clone() });
        back(ClientMsg::Unsub { view });
    }
    back(ServerMsg::Live {
        thread: s("t1"),
        partials: vec![raw_event()],
    });
    back(ServerMsg::Progress {
        id: 3,
        note: notes().remove(8),
    });
    for c in [
        Connection::Online,
        Connection::Offline {
            error: s("refused"),
        },
        Connection::Refused {
            reason: s("protocol 2"),
        },
    ] {
        back(ServerMsg::Connection(c));
    }
}

#[test]
fn every_wire_message_round_trips() {
    use crate::wire::*;
    let subs = view_keys()
        .into_iter()
        .map(|view| Held { view, rev: Some(3) })
        .collect();
    back(Up::Hello(Hello {
        proto: PROTOCOL,
        app: s("flotilla-desktop"),
        version: s("0.1.0"),
        platform: s("macos"),
        session: s("s1"),
        epoch: Some(s("e1")),
        subs,
        got: Some(7),
        scope: None,
    }));
    for (id, req) in (1..).zip(requests()) {
        back(Up::Req { id, req });
    }
    for view in view_keys() {
        back(Up::Sub { view: view.clone() });
        back(Up::Unsub { view: view.clone() });
        back(Up::Resync { view });
    }
    let server = ServerInfo {
        id: s("srv_1"),
        version: s("0.1.0"),
    };
    back(Down::Welcome(Welcome {
        proto: PROTOCOL,
        server: server.clone(),
        epoch: s("e1"),
        resumed: true,
        seen: Some(4),
        caps: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
        device: Some(s("0123456789abcdef")),
        adopted: vec![s("srv_mac")],
    }));
    back(Down::Res {
        id: 1,
        result: Ok(Reply::Sent(Sent::Steer(s("p1")))),
        n: Some(3),
    });
    back(Down::Res {
        id: 2,
        result: Err(s("unknown")),
        n: None,
    });
    back(Down::Live {
        thread: s("t1"),
        partials: vec![raw_event()],
    });
    back(Down::Synced);
    back(Down::Error {
        msg: s("protocol 2"),
    });
    for note in notes() {
        back(Down::Note {
            note: note.clone(),
            n: Some(5),
        });
        back(Down::Progress {
            id: 4,
            note,
            n: Some(6),
        });
    }
    for view in sample_views() {
        back(Down::Snap {
            view: view.key(),
            rev: 9,
            data: view.data(),
        });
    }
    back(Descriptor {
        server: server.clone(),
        protocol: PROTOCOL,
        capabilities: vec![s("deltas")],
    });
    back(Blob {
        id: s("b_ab"),
        size: 3,
    });
    back(PairRequest {
        code: crate::Secret(s("ABCD-EFGH-JKMN")),
        device_name: s("MacBook"),
        platform: s("macos"),
        scope: Some(crate::Scope::Agent),
    });
    back(Paired {
        device: s("0123456789abcdef"),
        token: crate::Secret(s("fdt_0123456789abcdef_x")),
        server: server.clone(),
    });
    // The shapes the plan sketches.
    let hello = serde_json::to_value(Up::Hello(Hello {
        proto: 1,
        app: s("a"),
        version: s("v"),
        platform: s("macos"),
        session: s("s"),
        epoch: None,
        subs: vec![Held {
            view: ViewKey::Thread(s("t1")),
            rev: Some(92),
        }],
        got: None,
        scope: None,
    }))
    .unwrap();
    assert_eq!(
        hello["subs"],
        json!([{"view": "thread", "key": "t1", "rev": 92}])
    );
    let delta = Down::Delta {
        view: ViewKey::Thread(s("t1")),
        base: 93,
        rev: 94,
        ops: vec![Op::Set {
            field: s("start_error"),
            v: json!("boom"),
        }],
    };
    assert_eq!(
        serde_json::to_value(delta).unwrap(),
        json!({"t": "delta", "view": "thread", "key": "t1", "base": 93, "rev": 94, "ops": [{"op": "set", "field": "start_error", "v": "boom"}]})
    );
    assert_eq!(
        serde_json::to_value(Down::Synced).unwrap(),
        json!({"t": "synced"})
    );
}

/// Every view through its data and back, and from each sample of a view to every other by
/// delta.
#[test]
fn every_view_survives_snapshots_and_deltas() {
    use crate::delta::{apply, diff};
    use crate::wire::{Down, Op};
    let mut all = sample_views();
    // A thread a turn later: a message changed, one more, a field set, its children gone.
    let mut later = detail();
    later.messages[0].text = Some(s("Changed"));
    later.messages.push(Message {
        seq: later.messages.last().map_or(0, |m| m.seq + 1),
        kind: s("text"),
        text: Some(s("Done.")),
        ..Default::default()
    });
    later.start_error = Some(s("boom"));
    later.children.clear();
    all.push(View::Thread(Box::new(later)));
    // A sidebar with its rows moved and one gone.
    if let View::Sidebar(mut sb) = sample_views().remove(0) {
        let mut other = summary();
        other.thread.id = s("t9");
        sb.threads.insert(0, other);
        all.push(View::Sidebar(sb.clone()));
        sb.threads.reverse();
        sb.threads.pop();
        all.push(View::Sidebar(sb));
    }
    for v in &all {
        assert_eq!(View::from_data(&v.key(), v.data()).as_ref(), Ok(v));
    }
    let mut diffs = 0;
    for a in &all {
        for b in all.iter().filter(|b| b.key() == a.key()) {
            let Some(ops) = diff(&a.key(), &a.data(), &b.data()) else {
                continue;
            };
            let ops: Vec<Op> = match through_json(&Down::Delta {
                view: a.key(),
                base: 1,
                rev: 2,
                ops,
            }) {
                Ok(Down::Delta { ops, .. }) => ops,
                other => panic!("{other:?}"),
            };
            let mut data = a.data();
            apply(&a.key(), &mut data, &ops).unwrap();
            assert_eq!(View::from_data(&a.key(), data).as_ref(), Ok(b));
            diffs += 1;
        }
    }
    assert!(diffs > all.len());
}

#[test]
fn the_envelope_reads_as_the_plan_sketches_it() {
    let req = ClientMsg::Req {
        id: 17,
        req: Request::Send {
            thread: s("t1"),
            text: s("hi"),
            files: vec![],
            local: s("local-1"),
        },
    };
    assert_eq!(
        serde_json::to_value(&req).unwrap(),
        json!({"t": "req", "id": 17, "m": "thread.send", "p": {"thread": "t1", "text": "hi", "files": [], "local": "local-1"}})
    );
    assert_eq!(
        serde_json::to_value(ClientMsg::Req {
            id: 1,
            req: Request::SyncNow
        })
        .unwrap(),
        json!({"t": "req", "id": 1, "m": "sync.now"})
    );
    // As the phone writes it (FlotillaKit's `Request.renameDevice`).
    let rename = ClientMsg::Req {
        id: 2,
        req: Request::RenameDevice {
            device: s("0123456789abcdef"),
            name: s("Work phone"),
        },
    };
    assert_eq!(
        serde_json::to_value(rename).unwrap(),
        json!({"t": "req", "id": 2, "m": "devices.rename", "p": {"device": "0123456789abcdef", "name": "Work phone"}})
    );
    assert_eq!(
        serde_json::to_value(ClientMsg::Sub {
            view: ViewKey::Thread(s("t1"))
        })
        .unwrap(),
        json!({"t": "sub", "view": "thread", "key": "t1"})
    );
    assert_eq!(
        serde_json::to_value(ClientMsg::Sub {
            view: ViewKey::Sidebar
        })
        .unwrap(),
        json!({"t": "sub", "view": "sidebar"})
    );
    let snap = serde_json::to_value(ServerMsg::Snap {
        view: Box::new(View::Catalog(None)),
    })
    .unwrap();
    assert_eq!(snap, json!({"t": "snap", "view": "catalog", "data": null}));
    // Unknown fields from a newer peer are ignored.
    let newer = json!({"t": "req", "id": 2, "m": "thread.stop", "p": "t1", "trace": "x"});
    assert_eq!(
        serde_json::from_value::<ClientMsg>(newer).unwrap(),
        ClientMsg::Req {
            id: 2,
            req: Request::Stop(s("t1"))
        }
    );
}

#[test]
fn a_secret_never_shows_in_debug_output() {
    let req = Request::SetBoatKey(Secret(s("boat_very_secret")));
    assert!(!format!("{req:?}").contains("very_secret"));
    assert!(!format!("{:?}", ClientMsg::Req { id: 1, req }).contains("very_secret"));
    // A pairing code, and a device's token on its way out of pairing.
    let code = Reply::Code(crate::PairingCode {
        code: s("ABCDEFGHJKMN"),
        expires_ms: 1,
        link: s("flotilla://pair?u=x#ABCDEFGHJKMN"),
    });
    assert!(!format!("{code:?}").contains("ABCDEFGHJKMN"));
    let paired = crate::wire::Paired {
        device: s("d"),
        token: Secret(s("fdt_secret")),
        server: crate::wire::ServerInfo {
            id: s("srv"),
            version: s("0"),
        },
    };
    assert!(!format!("{paired:?}").contains("fdt_secret"));
    let req = crate::wire::PairRequest {
        code: Secret(s("ABCDEFGHJKMN")),
        device_name: s("Mac"),
        platform: s("macos"),
        scope: None,
    };
    assert!(!format!("{req:?}").contains("ABCDEFGHJKMN"));
    assert!(
        !format!(
            "{:?}",
            crate::pair::read("flotilla://pair?u=x.ts.net#ABCDEFGHJKMN").unwrap()
        )
        .contains("ABCDEFGHJKMN")
    );
}

#[test]
fn a_view_knows_its_key() {
    let keys: Vec<ViewKey> = sample_views().iter().map(View::key).collect();
    assert!(keys.contains(&ViewKey::Thread(s("t1"))));
    assert!(keys.contains(&ViewKey::Project(s("p1"))));
    assert!(keys.contains(&ViewKey::Mentions(s("t1"))));
}

#[test]
fn push_requests_on_the_wire() {
    // What the iPhone app writes.
    let req = Request::RegisterPush(PushToken {
        token: Secret(s("00ff")),
        env: s("sandbox"),
        topic: s("com.useflotilla.flotilla"),
        relay: None,
    });
    assert_eq!(
        serde_json::to_value(crate::wire::Up::Req { id: 3, req }).unwrap(),
        json!({"t": "req", "id": 3, "m": "push.register", "p": {"token": "00ff", "env": "sandbox", "topic": "com.useflotilla.flotilla"}})
    );
    // With the relay the phone registered with (FlotillaKit's `push register and unregister`);
    // an older app's, without, reads as before.
    let relay = crate::PushRelay {
        url: s("https://push.useflotilla.com"),
        handle: s("giLjuhbbFalUBROEtNqwBg"),
        secret: Secret(s("c2VjcmV0")),
        key: Secret(s("a2V5")),
    };
    let req = Request::RegisterPush(PushToken {
        token: Secret(s("00ff")),
        env: s("sandbox"),
        topic: s("com.useflotilla.flotilla"),
        relay: Some(relay.clone()),
    });
    let wire = json!({"t": "req", "id": 5, "m": "push.register", "p": {"token": "00ff", "env": "sandbox", "topic": "com.useflotilla.flotilla", "relay": {"url": "https://push.useflotilla.com", "handle": "giLjuhbbFalUBROEtNqwBg", "secret": "c2VjcmV0", "key": "a2V5"}}});
    assert_eq!(
        serde_json::to_value(crate::wire::Up::Req { id: 5, req }).unwrap(),
        wire
    );
    assert!(!format!("{relay:?}").contains("c2VjcmV0") && !format!("{relay:?}").contains("a2V5"));
    // Its answer: the relay the server sends through, or none (FlotillaKit reads both).
    let answer = |mode: crate::PushMode| {
        serde_json::to_value(Ok::<Reply, String>(Reply::Push(mode))).unwrap()
    };
    assert_eq!(
        answer(crate::PushMode {
            relay: Some(s("https://push.useflotilla.com"))
        }),
        json!({"Ok": {"push": {"relay": "https://push.useflotilla.com"}}})
    );
    assert_eq!(
        answer(crate::PushMode::default()),
        json!({"Ok": {"push": {}}})
    );
    assert_eq!(
        serde_json::to_value(crate::wire::Up::Req {
            id: 4,
            req: Request::UnregisterPush
        })
        .unwrap(),
        json!({"t": "req", "id": 4, "m": "push.unregister"})
    );
    // The token never shows in a log.
    let t = PushToken {
        token: Secret(s("00ff")),
        env: s("sandbox"),
        topic: s("t"),
        relay: None,
    };
    assert!(!format!("{t:?}").contains("00ff"));
}

#[test]
fn phone_preview_and_services_on_the_wire() {
    // What the iPhone app writes (FlotillaKit's `ServicesTests`).
    let up = |id, req| serde_json::to_value(crate::wire::Up::Req { id, req }).unwrap();
    assert_eq!(
        up(
            7,
            Request::Preview {
                thread: s("t1"),
                port: 3000
            }
        ),
        json!({"t": "req", "id": 7, "m": "thread.preview", "p": {"thread": "t1", "port": 3000}})
    );
    let restart = Request::Services(ServiceCmd::Restart {
        thread: s("t1"),
        name: s("web"),
    });
    assert_eq!(
        up(8, restart),
        json!({"t": "req", "id": 8, "m": "services", "p": {"restart": {"thread": "t1", "name": "web"}}})
    );
    let stop = Request::Services(ServiceCmd::Stop {
        thread: s("t1"),
        name: s("web"),
    });
    assert_eq!(
        up(9, stop),
        json!({"t": "req", "id": 9, "m": "services", "p": {"stop": {"thread": "t1", "name": "web"}}})
    );
    assert_eq!(
        up(10, Request::Services(ServiceCmd::Ensure(s("t1")))),
        json!({"t": "req", "id": 10, "m": "services", "p": {"ensure": "t1"}})
    );
    // Make public… and Make private (FlotillaKit's `PublicLinksTests`).
    for (i, (for_, word)) in crate::PublicFor::ALL
        .into_iter()
        .zip(["hour", "day", "week", "until_stopped"])
        .enumerate()
    {
        let id = 11 + i as u64;
        assert_eq!(
            up(
                id,
                Request::MakePublic {
                    thread: s("t1"),
                    port: 3000,
                    for_
                }
            ),
            json!({"t": "req", "id": id, "m": "thread.make_public", "p": {"thread": "t1", "port": 3000, "for": word}})
        );
    }
    assert_eq!(
        up(
            15,
            Request::MakePrivate {
                thread: s("t1"),
                port: 3000
            }
        ),
        json!({"t": "req", "id": 15, "m": "thread.make_private", "p": {"thread": "t1", "port": 3000}})
    );
    assert_eq!(
        serde_json::to_value(Reply::Url(s("https://3000-bx1.example.com/?_token=t"))).unwrap(),
        json!({"url": "https://3000-bx1.example.com/?_token=t"})
    );
    // A thread view's services, as the phone reads them (`ServicesTests.wire`).
    let preview = |port: u16| Some(format!("https://{port}-bx1.example.com/?_token=t"));
    let services = ThreadServices {
        sandbox: s("bx_1"),
        file: Some(s(".agents/services.yml")),
        error: None,
        services: vec![
            ServiceRun {
                name: s("web"),
                port: 4100,
                pid: Some(12),
                state: s("ready"),
                output: s("  VITE ready in 312 ms\n\n  Local: http://localhost:4100/  \n"),
                preview: preview(4100),
                links: vec![
                    Link {
                        title: s("web"),
                        url: s("https://4100-bx1.example.com/?_token=t"),
                    },
                    Link {
                        title: s("Admin"),
                        url: s("https://4100-bx1.example.com/admin?_token=t"),
                    },
                ],
                started_at: Some(s("2026-11-01T12:00:00Z")),
                out_at: 40,
                ..Default::default()
            },
            ServiceRun {
                name: s("worker"),
                port: 4101,
                state: s("stopped"),
                ..Default::default()
            },
            ServiceRun {
                name: s("api"),
                port: 4102,
                state: s("exited"),
                exit_code: Some(1),
                output: s("Error: listen EADDRINUSE\n"),
                preview: preview(4102),
                started_at: Some(s("2026-11-01T12:00:00Z")),
                out_at: 9,
                err_at: 24,
                ..Default::default()
            },
        ],
    };
    let phone = r#"{"sandbox":"bx_1","file":".agents/services.yml","error":null,"services":[{"name":"web","port":4100,"pid":12,"state":"ready","exit_code":null,"error":null,"output":"  VITE ready in 312 ms\n\n  Local: http://localhost:4100/  \n","preview":"https://4100-bx1.example.com/?_token=t","links":[{"title":"web","url":"https://4100-bx1.example.com/?_token=t"},{"title":"Admin","url":"https://4100-bx1.example.com/admin?_token=t"}],"started_at":"2026-11-01T12:00:00Z","out_at":40,"err_at":0},{"name":"worker","port":4101,"pid":null,"state":"stopped","exit_code":null,"error":null,"output":"","preview":null,"links":[],"started_at":null,"out_at":0,"err_at":0},{"name":"api","port":4102,"pid":null,"state":"exited","exit_code":1,"error":null,"output":"Error: listen EADDRINUSE\n","preview":"https://4102-bx1.example.com/?_token=t","links":[],"started_at":"2026-11-01T12:00:00Z","out_at":9,"err_at":24}]}"#;
    assert_eq!(
        serde_json::to_value(&services).unwrap(),
        serde_json::from_str::<serde_json::Value>(phone).unwrap()
    );
    // A thread view's public links, as the phone reads them (`PublicLinksTests.wire`).
    let public = vec![
        crate::PublicLink {
            port: 3000,
            url: s("https://bx1-3000.example.com"),
            since_ms: 1_800_000_000_000,
            until_ms: Some(1_800_003_600_000),
        },
        crate::PublicLink {
            port: 4100,
            url: s("https://bx1-4100.example.com"),
            since_ms: 1_800_000_000_000,
            until_ms: None,
        },
    ];
    let phone = r#"[{"port":3000,"url":"https://bx1-3000.example.com","since_ms":1800000000000,"until_ms":1800003600000},{"port":4100,"url":"https://bx1-4100.example.com","since_ms":1800000000000,"until_ms":null}]"#;
    assert_eq!(
        serde_json::to_value(&public).unwrap(),
        serde_json::from_str::<serde_json::Value>(phone).unwrap()
    );
    // A server from before public links sends none.
    let old: ThreadDetail = serde_json::from_value(json!({"id": "t1", "thread": null, "messages": [], "sent": [], "start_error": null, "stages": null, "project": null, "resume": null, "archive": null, "setup_failure": null, "services": null, "lineage": null, "parent": null, "children": []})).unwrap();
    assert!(old.public.is_empty());
}

#[test]
fn usage_from_a_server_before_the_full_usage() {
    // What a server before extra starts and read times sent: still read, the new fields empty.
    let old = json!({"vms": [3, 100], "minute": [0, 12], "hour": [7, 60], "day": [120, 200], "vm_hours": [549, 556], "renews": "2026-11-30T01:01:38.000Z", "blocked": null});
    let u: Usage = serde_json::from_value(old).unwrap();
    assert_eq!(
        (u.vms, u.hour, u.starts_free, u.extra_starts, u.read_ms),
        (Some((3, 100)), Some((7, 60)), None, None, None)
    );
    assert_eq!(
        serde_json::to_value(crate::wire::Up::Req {
            id: 5,
            req: Request::ReadUsage
        })
        .unwrap(),
        json!({"t": "req", "id": 5, "m": "usage.read"})
    );
}

/// Only a failing check threads need takes New thread's place: the boat key or the database.
/// GitHub, titles, Tailscale and push failing, a warning, or an optional part off leave it.
#[test]
fn which_failing_checks_stop_threads() {
    let check = |check: CheckOf, health: Health| Diagnosis {
        check,
        health,
        says: s(""),
        fix: vec![],
    };
    let doctor = |checks: Vec<Diagnosis>| Doctor { checks, at_ms: 0 };
    let others = [
        CheckOf::GitHub,
        CheckOf::Claude,
        CheckOf::Tailscale,
        CheckOf::Others,
        CheckOf::Push,
        CheckOf::Mcp,
    ];
    assert!(!doctor(others.iter().map(|&c| check(c, Health::Fail)).collect()).stops_threads());
    for needed in [CheckOf::BoatKey, CheckOf::Database] {
        assert!(
            doctor(vec![
                check(CheckOf::Push, Health::Off),
                check(needed, Health::Fail)
            ])
            .stops_threads(),
            "{needed:?}"
        );
        for health in [Health::Ok, Health::Warn, Health::Off] {
            assert!(
                !doctor(vec![check(needed, health)]).stops_threads(),
                "{needed:?} {health:?}"
            );
        }
    }
    assert!(!doctor(vec![]).stops_threads());
}

/// The setup's count is of the checks that are on: optional parts that are off (remote MCP,
/// push, Tailscale) don't make a working setup look unfinished; a warning or a failure does.
#[test]
fn optional_parts_that_are_off_leave_the_count_whole() {
    let check = |check: CheckOf, health: Health| Diagnosis {
        check,
        health,
        says: s(""),
        fix: vec![],
    };
    let doctor = |checks: Vec<Diagnosis>| Doctor { checks, at_ms: 0 };
    let mut checks = vec![
        check(CheckOf::BoatKey, Health::Ok),
        check(CheckOf::GitHub, Health::Ok),
        check(CheckOf::Database, Health::Ok),
        check(CheckOf::Push, Health::Off),
        check(CheckOf::Mcp, Health::Off),
    ];
    assert_eq!(doctor(checks.clone()).tally(), (3, 3));
    assert!(doctor(checks.clone()).ok());
    checks.push(check(CheckOf::Claude, Health::Warn));
    assert_eq!(doctor(checks.clone()).tally(), (3, 4));
    checks[4] = check(CheckOf::Mcp, Health::Fail);
    assert_eq!(doctor(checks).tally(), (3, 5));
    assert_eq!(CheckOf::Mcp.label(), "Remote MCP");
    assert_eq!(serde_json::to_value(CheckOf::Mcp).unwrap(), json!("mcp"));
}
