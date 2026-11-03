//! `flotilla serve --demo`: a server with no boat, GitHub, model or VMs, for anyone to pair with
//! and try (Apple's reviewer, say), at no cost and the same every time.
//!
//! - **In place of the engine** ([`start`]): the host sends it the engine's commands, and it
//!   answers with the engine's events, writing what a turn would through the same `Store` calls,
//!   so the views, alerts and push are the real ones. A new thread or a message plays a scripted
//!   turn: words streamed, two commands and their output, then a reply that says it's the demo.
//!   A project thread's first turn opens a pull request, and Merge merges it (archiving the
//!   thread, as Settings says). Stop, Resume, Rename, Archive and Delete do what they say. What
//!   needs boat or GitHub is refused ("Not in the demo: …"), and every command a client waits on
//!   is answered.
//! - **Its threads** ([`reset`]): each start puts the database back to the demo's
//!   (`demo/seed.sql`: the video's threads, scrubbed), keeping who's paired.
//! - **The standing code** ([`standing`]): one pairing code that pairs any number of devices and
//!   never lapses, for a review that takes days. Only a demo honours it.

use crate::{Action, Changes, Cmd, Event, Events, Outcome};
use flotilla_core::store::{Store, ThreadState};
use flotilla_core::util::{now_iso, now_ms};
use flotilla_proto::{Check, GhStatus, PullRequest, RawEvent, Sent, Usage};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// What a refusal starts with.
pub const NOT_IN_DEMO: &str = "Not in the demo";
/// `kv`: this database is a demo's (so a start may reset it), and its standing code.
pub const MARK_KEY: &str = "demo";
pub const CODE_KEY: &str = "demo:code";
const SEED: &str = include_str!("demo/seed.sql");
/// What a reset keeps of `kv`: who's paired and how they're notified, and the store's own.
const KEPT: &[&str] = &[
    MARK_KEY,
    CODE_KEY,
    "server_id",
    "server_url",
    "messages_version",
    flotilla_core::push::RELAY_KEY,
    flotilla_core::push::KEY_ID_KEY,
    flotilla_core::push::TEAM_ID_KEY,
];
/// The GitHub account the demo's projects are under.
const LOGIN: &str = "acme";

/// `flotilla_core`'s errors, as said to a person.
trait Plain<T> {
    fn plain(self) -> Result<T, String>;
}

impl<T> Plain<T> for flotilla_core::Result<T> {
    fn plain(self) -> Result<T, String> {
        self.map_err(|e| e.plain())
    }
}

pub fn refused(what: &str) -> String {
    format!("{NOT_IN_DEMO}: {what}")
}

/// How fast a turn plays: words streamed `chars` at a time every `tick`, a command running for
/// `command`, and a `pause` between steps.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub tick: Duration,
    pub chars: usize,
    pub command: Duration,
    pub pause: Duration,
}

impl Pace {
    /// About eight seconds a first turn.
    pub const SHOWN: Pace = Pace {
        tick: Duration::from_millis(60),
        chars: 9,
        command: Duration::from_millis(1200),
        pause: Duration::from_millis(400),
    };
}

/// Start the demo's engine on `db`; what it says goes to `events`. Its commands go to the
/// function returned; it stops when that's dropped.
pub fn start(db: PathBuf, events: Events, pace: Pace) -> Arc<dyn Fn(Cmd) + Send + Sync> {
    let (tx, rx) = mpsc::channel();
    let demo = Demo {
        store: Store::open(&db).ok(),
        db,
        events,
        pace,
        turns: Vec::new(),
        last_ms: 0,
        closed: false,
    };
    let _ = std::thread::Builder::new()
        .name("flotilla-demo".into())
        .spawn(move || demo.run(rx));
    Arc::new(move |c| {
        let _ = tx.send(c);
    })
}

/// One step of a scripted turn.
#[derive(Debug, Clone)]
enum Step {
    /// The agent's words, streamed.
    Say(String),
    /// A command: shown running, then with its output.
    Run {
        command: &'static str,
        description: &'static str,
        output: &'static str,
    },
}

/// A turn playing (the first of its thread's) or waiting behind one.
struct Turn {
    thread: String,
    task: String,
    conversation: String,
    model: String,
    prompt: String,
    files: Vec<flotilla_proto::Attached>,
    steps: Vec<Step>,
    /// The step under way, how far into it (words shown, or the command shown running), and
    /// its event's id and time (a partial keeps its time as it grows, and its event has it too).
    at: usize,
    shown: usize,
    event: String,
    event_ms: i64,
    due: Instant,
    /// The thread's first: a project thread's opens a pull request.
    first: bool,
}

struct Demo {
    db: PathBuf,
    store: Option<Store>,
    events: Events,
    pace: Pace,
    turns: Vec<Turn>,
    last_ms: i64,
    closed: bool,
}

impl Demo {
    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        self.emit(Event::Usage(Ok(usage())));
        self.emit(Event::GitHub(GhStatus::SignedIn {
            login: LOGIN.into(),
            version: "2.81.0".into(),
            scopes: vec!["repo".into()],
        }));
        while !self.closed {
            let wait = self.next_due().map_or(Duration::from_secs(3600), |d| {
                d.saturating_duration_since(Instant::now())
            });
            match rx.recv_timeout(wait) {
                Ok(Cmd::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Ok(cmd) => self.command(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            self.play();
        }
    }

    fn emit(&mut self, e: Event) {
        self.closed |= self.events.send_blocking(e).is_err();
    }

    fn store(&mut self) -> Result<&Store, String> {
        if self.store.is_none() {
            self.store = Some(Store::open(&self.db).plain()?);
        }
        Ok(self.store.as_ref().expect("opened"))
    }

    /// An event time after the last, as boat's are.
    fn clock(&mut self) -> i64 {
        self.last_ms = now_ms().max(self.last_ms + 1);
        self.last_ms
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Act { action, req } => {
                let mut after = None;
                let result = self.act(&action, &mut after);
                self.emit(Event::Changed(action.changes()));
                self.emit(Event::Done {
                    action,
                    result,
                    req,
                });
                if let Some(e) = after {
                    self.emit(e);
                }
            }
            Cmd::ReadUsage => self.emit(Event::Usage(Ok(usage()))),
            Cmd::ReadAgentUsage { .. } => self.emit(Event::AgentUsage {
                checked: Some(Ok(())),
            }),
            Cmd::TestSummaries => self.emit(Event::Summaries(Err(refused("summaries")))),
            Cmd::Rebuild => self.emit(Event::Rebuilt(Err(refused("rebuilding")))),
            Cmd::ListRepos { req } => self.emit(Event::Repos {
                result: Err(refused("adding projects")),
                done: true,
                login: None,
                req,
            }),
            Cmd::Suggest { repo, req } => self.emit(Event::Suggested {
                repo: format!("{}/{}", repo.owner, repo.name),
                result: Err(refused("adding projects")),
                req,
            }),
            Cmd::ListBranches { project, req } => self.emit(Event::Branches {
                project,
                result: Err(refused("branches")),
                req,
            }),
            Cmd::CheckBranch {
                project,
                branch,
                prompt,
                req,
            } => self.emit(Event::BranchChecked {
                project,
                branch,
                prompt,
                result: Err(refused("branches")),
                req,
            }),
            // Nothing to look at, and no one waits on them.
            Cmd::Open(_)
            | Cmd::SyncNow
            | Cmd::PollNow(_)
            | Cmd::CheckGitHub
            | Cmd::CheckProject(_)
            | Cmd::RunSetup { .. }
            | Cmd::CancelSetup(_)
            | Cmd::CheckPrs
            | Cmd::CheckPr(_)
            | Cmd::Services(_)
            | Cmd::ReadMentions(_)
            | Cmd::LookSchedules
            | Cmd::RunSchedule(_)
            | Cmd::Shutdown => {}
        }
    }

    fn act(&mut self, action: &Action, after: &mut Option<Event>) -> Result<Outcome, String> {
        match action {
            // Archived as it started: it stays so and plays nothing, as a real start.
            Action::Launch { thread, req, local } => {
                if !self.store()?.is_archived(thread).plain()? {
                    self.begin(thread, Some(local), &req.prompt, &req.attachments, true)?;
                }
                Ok(Outcome::Created(thread.clone()))
            }
            Action::Send {
                thread,
                text,
                files,
                local,
            } => self
                .begin(thread, local.as_deref(), text, files, false)
                .map(|task| Outcome::Sent(Sent::Prompt(task))),
            Action::Retry(thread) => Ok(Outcome::Created(thread.clone())),
            Action::Interrupt(thread) => {
                self.cut(thread, false);
                Ok(Outcome::Done)
            }
            Action::Stop(thread) | Action::Archive(thread) | Action::ArchiveIdle { thread, .. } => {
                self.cut(thread, true);
                self.store()?
                    .set_thread_state(thread, ThreadState::Stopped)
                    .plain()?;
                Ok(Outcome::Done)
            }
            Action::Resume(thread) => {
                self.store()?
                    .set_thread_state(thread, ThreadState::Idle)
                    .plain()?;
                Ok(Outcome::Done)
            }
            Action::Rename(thread, title) => {
                let title = flotilla_core::util::truncate_chars(title.trim(), 120);
                if title.is_empty() {
                    return Err("the title is empty".into());
                }
                let store = self.store()?;
                store.set_thread_title(thread, &title).plain()?;
                Ok(Outcome::Done)
            }
            Action::KeepRunning(thread, on) => {
                let store = self.store()?;
                store.set_keep_running(thread, *on).plain()?;
                Ok(Outcome::Done)
            }
            Action::Delete(thread) => {
                self.turns.retain(|t| &t.thread != thread);
                let store = self.store()?;
                store.delete_thread(thread).plain()?;
                Ok(Outcome::Done)
            }
            Action::MergePr(thread) => {
                *after = Some(self.merge(thread)?);
                Ok(Outcome::Done)
            }
            Action::Preview(..) | Action::MakePublic { .. } | Action::MakePrivate { .. } => {
                Err(refused("previews"))
            }
            Action::AddProject { .. } => Err(refused("adding projects")),
            Action::RemoveProject(_) => Err(refused("removing projects")),
            Action::MarkReady(_)
            | Action::LinkPr { .. }
            | Action::ReviewPrompt(_)
            | Action::SetupFixPrompt { .. } => Err(refused("GitHub")),
            Action::SendLater { .. } | Action::StartLater { .. } => Err(refused("schedules")),
            Action::New(_) | Action::Tell { .. } => Err(NOT_IN_DEMO.into()),
        }
    }

    /// The thread's VM "up", the message kept as sent, and its turn set to play (after the one
    /// playing, if there is one). Its id.
    fn begin(
        &mut self,
        thread: &str,
        local: Option<&str>,
        text: &str,
        files: &[flotilla_proto::Attached],
        first: bool,
    ) -> Result<String, String> {
        let waiting = self.turns.iter().any(|t| t.thread == thread);
        let ms = self.clock();
        let store = self.store()?;
        let mut t = store
            .thread(thread)
            .plain()?
            .ok_or("That thread was deleted")?;
        t.sandbox_id
            .get_or_insert_with(|| format!("bx_demo{}", &thread[..8.min(thread.len())]));
        let conversation = t
            .conversation_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
        t.state = ThreadState::Running;
        store.update_thread(&t).plain()?;
        // Your follow-up brings the thread back from the archive.
        if !first {
            store.set_archived(thread, false).plain()?;
        }
        let task = uuid::Uuid::new_v4().to_string();
        store
            .record_sent_prompt(thread, local, &task, "prompt", text)
            .plain()?;
        store.set_sent_attachments(thread, &task, files).plain()?;
        let status = if waiting { "queued" } else { "running" };
        let prompt = prompt_event(&task, &conversation, text, files, status, ms);
        store.apply_events(thread, &[prompt], false).plain()?;
        store.rebuild_messages(thread).plain()?;
        let project = t.repo_project_id.is_some();
        self.turns.push(Turn {
            thread: thread.into(),
            task: task.clone(),
            conversation,
            model: t.model.clone(),
            prompt: text.into(),
            files: files.to_vec(),
            steps: script(first, project),
            at: 0,
            shown: 0,
            event: String::new(),
            event_ms: 0,
            due: Instant::now() + self.pace.pause,
            first,
        });
        Ok(task)
    }

    /// The turns playing: the first of each thread's (the rest wait behind it).
    fn playing(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.turns.len()).filter(|&i| {
            !self.turns[..i]
                .iter()
                .any(|t| t.thread == self.turns[i].thread)
        })
    }

    /// When the next step of a turn playing is due.
    fn next_due(&self) -> Option<Instant> {
        self.playing().map(|i| self.turns[i].due).min()
    }

    /// Play every step that's due.
    fn play(&mut self) {
        loop {
            let now = Instant::now();
            let Some(i) = self.playing().find(|&i| self.turns[i].due <= now) else {
                return;
            };
            let task = self.turns[i].task.clone();
            if let Err(e) = self.step(i) {
                eprintln!("[demo] {e}");
                // Unless its end took it already.
                if self.turns.get(i).is_some_and(|t| t.task == task) {
                    self.turns.remove(i);
                }
            }
            if self.closed {
                return;
            }
        }
    }

    /// One step of turn `i`, or its end.
    fn step(&mut self, i: usize) -> Result<(), String> {
        let ms = self.clock();
        let pace = self.pace;
        let store = self.store.as_ref().ok_or("no database")?;
        let t = &mut self.turns[i];
        // Deleted or forgotten meanwhile: nothing more is written for it.
        if store.thread(&t.thread).ok().flatten().is_none() {
            self.turns.remove(i);
            return Ok(());
        }
        let Some(step) = t.steps.get(t.at).cloned() else {
            return self.finish(i, "finished", true);
        };
        if t.shown == 0 {
            t.event = uuid::Uuid::new_v4().to_string();
            t.event_ms = ms;
        }
        let thread = t.thread.clone();
        let mut live = None;
        match step {
            Step::Say(words) => {
                let shown = words
                    .char_indices()
                    .nth(t.shown + pace.chars)
                    .map_or(words.len(), |(at, _)| at);
                if shown < words.len() {
                    t.shown += pace.chars;
                    t.due = Instant::now() + pace.tick;
                    let mut e = response(t, &words[..shown], None);
                    e["data"]["is_streaming"] = json!(true);
                    e["id"] = json!(format!("{}-streaming", t.event));
                    live = Some(vec![RawEvent::from_value(e)]);
                } else {
                    let e = response(t, &words, None);
                    store
                        .apply_events(&thread, &[RawEvent::from_value(e)], false)
                        .plain()?;
                    (t.at, t.shown, t.due) = (t.at + 1, 0, Instant::now() + pace.pause);
                    live = Some(Vec::new());
                }
            }
            Step::Run { .. } => {
                let done = t.shown > 0;
                let e = response(t, "", Some((&step, done)));
                store
                    .apply_events(&thread, &[RawEvent::from_value(e)], false)
                    .plain()?;
                if done {
                    (t.at, t.shown, t.due) = (t.at + 1, 0, Instant::now() + pace.pause);
                } else {
                    (t.shown, t.due) = (1, Instant::now() + pace.command);
                }
            }
        }
        if let Some(partials) = live {
            if partials.is_empty() {
                store.rebuild_messages(&thread).plain()?;
                self.emit(Event::Changed(Changes::thread(&thread).with_sidebar()));
            }
            self.emit(Event::Live { thread, partials });
        } else {
            store.rebuild_messages(&thread).plain()?;
            self.emit(Event::Changed(Changes::thread(&thread).with_sidebar()));
        }
        Ok(())
    }

    /// Turn `i` ends as `status` (`finished`, `interrupted`): settled, said, and the next of its
    /// thread's starts. A project thread's first opens a pull request.
    fn finish(&mut self, i: usize, status: &str, said: bool) -> Result<(), String> {
        let ms = self.clock();
        let t = self.turns.remove(i);
        let next = self.turns.iter().position(|n| n.thread == t.thread);
        let store = self.store.as_ref().ok_or("no database")?;
        // A command cut short says so.
        if let Some(step @ Step::Run { .. }) = t.steps.get(t.at)
            && t.shown > 0
        {
            let mut e = response(&t, "", Some((step, true)));
            e["data"]["tools"][0]["result"]["content"] = json!(
                json!({"type": "bash_output", "pending": false, "content": "Interrupted", "error": true})
                    .to_string()
            );
            store
                .apply_events(&t.thread, &[RawEvent::from_value(e)], false)
                .plain()?;
        }
        let prompt = prompt_event(&t.task, &t.conversation, &t.prompt, &t.files, status, ms);
        store.apply_events(&t.thread, &[prompt], false).plain()?;
        if let Some(mut turn) = store
            .turns(&t.thread)
            .plain()?
            .into_iter()
            .find(|u| u.prompt_id == t.task)
        {
            (turn.done, turn.settled) = (true, true);
            turn.done_at = Some(now_iso());
            store.update_turn(&turn).plain()?;
        }
        store.settle_all_events(&t.thread).plain()?;
        if t.first && status == "finished" {
            open_pr(store, &t.thread).plain()?;
        }
        match next {
            Some(n) => {
                let n = &mut self.turns[n];
                let e = prompt_event(&n.task, &n.conversation, &n.prompt, &n.files, "running", ms);
                store.apply_events(&n.thread, &[e], false).plain()?;
                n.due = Instant::now() + self.pace.pause;
            }
            None => {
                if store.thread(&t.thread).ok().flatten().map(|x| x.state)
                    == Some(ThreadState::Running)
                {
                    store
                        .set_thread_state(&t.thread, ThreadState::Idle)
                        .plain()?;
                }
            }
        }
        store.rebuild_messages(&t.thread).plain()?;
        self.emit(Event::Live {
            thread: t.thread.clone(),
            partials: Vec::new(),
        });
        self.emit(Event::Changed(Changes::thread(&t.thread).with_sidebar()));
        if said {
            self.emit(Event::TurnFinished(t.thread));
        }
        Ok(())
    }

    /// Interrupt the thread's turn; `all`: the ones waiting too, unsaid (its VM stops).
    fn cut(&mut self, thread: &str, all: bool) {
        while let Some(i) = self.turns.iter().position(|t| t.thread == thread) {
            if let Err(e) = self.finish(i, "interrupted", !all) {
                eprintln!("[demo] {e}");
            }
            if !all {
                return;
            }
        }
    }

    /// Merge the thread's pull request: archived for it, as Settings says.
    fn merge(&mut self, thread: &str) -> Result<Event, String> {
        let store = self.store()?;
        let (mut pr, by_user) = store
            .pull_requests()
            .plain()?
            .remove(thread)
            .ok_or("This thread has no pull request")?;
        if pr.state != "OPEN" {
            return Err("The pull request isn't open".into());
        }
        pr.state = "MERGED".into();
        pr.merged_at = Some(now_iso());
        pr.updated_at = Some(now_iso());
        store.set_pull_request(thread, &pr, by_user).plain()?;
        let archived = flotilla_core::tidy::on_merge(store);
        if archived {
            store.set_archived(thread, true).plain()?;
            self.cut(thread, true);
            self.store()?
                .set_thread_state(thread, ThreadState::Stopped)
                .plain()?;
        }
        Ok(Event::PrMerged {
            thread: thread.into(),
            number: pr.number,
            title: pr.title,
            branch: pr.base,
            archived,
        })
    }
}

/// The turn's script: the first a thread plays, or a follow-up.
fn script(first: bool, project: bool) -> Vec<Step> {
    if !first {
        return vec![
            Step::Run {
                command: "git status",
                description: "Check the working tree",
                output: "On branch main\nnothing to commit, working tree clean",
            },
            Step::Say(
                "Got it. This is the demo server, so this reply is scripted too: on your own \
                 server, your message goes to the same agent, in the same VM, with everything it \
                 has done so far."
                    .into(),
            ),
        ];
    }
    let pr = if project {
        "\n\nI opened a pull request for it, with its checks passing."
    } else {
        ""
    };
    vec![
        Step::Say("I'll look around the project first.".into()),
        Step::Run {
            command: "git log --oneline -3 && ls",
            description: "Look around the project",
            output: "4f2c1a9 Add onRetry callback to retry\n9b07e3d docs: add retry usage \
                     example\n1c55d20 Retry with backoff\nCHANGELOG.md\nREADME.md\n\
                     package.json\nsrc",
        },
        Step::Say(
            "A small TypeScript library. I'll run its tests before changing anything.".into(),
        ),
        Step::Run {
            command: "npm test",
            description: "Run the tests",
            output: "> retry@0.1.0 test\n> node --test\n\n✔ retries until it succeeds \
                     (3.1ms)\n✔ gives up after the last try (1.2ms)\n✔ calls onRetry before \
                     each retry (0.9ms)\nℹ tests 3\nℹ pass 3\nℹ fail 0",
        },
        Step::Say(format!(
            "The tests pass. This is Flotilla's demo server, so no agent ran and nothing \
             changed: this reply is scripted.\n\nOn your own server, the agent works in a VM of \
             its own, and you'd see its edits, commands and test runs here as they happen. You \
             can send a follow-up, stop a turn, or start more threads side by side.{pr}"
        )),
    ]
}

fn prompt_event(
    task: &str,
    conversation: &str,
    text: &str,
    files: &[flotilla_proto::Attached],
    status: &str,
    ms: i64,
) -> RawEvent {
    let files: Vec<Value> = files
        .iter()
        .map(|f| json!({"filename": f.name, "mimeType": "application/octet-stream"}))
        .collect();
    RawEvent::from_value(json!({
        "id": format!("p-{task}"), "type": "prompt", "timestamp": ms, "taskId": task,
        "conversationId": conversation,
        "data": {"prompt": text, "status": status, "is_reverted": false, "attachments": files},
    }))
}

/// The agent's words, or a command (`done`: with its output), as boat sends them.
fn response(t: &Turn, words: &str, run: Option<(&Step, bool)>) -> Value {
    let mut data = json!({"content": words, "model": t.model, "is_reverted": false});
    if let Some((
        Step::Run {
            command,
            description,
            output,
        },
        done,
    )) = run
    {
        let call = format!("call_{}", &t.event[..8]);
        let result = done.then(|| {
            let content = json!({"type": "bash_output", "pending": false, "command": command,
                "description": description, "content": output, "error": false});
            json!({"type": "tool_result", "content": content.to_string(), "is_error": false,
                "tool_use_id": call})
        });
        data["tools"] = json!([{
            "use": {"id": call, "name": "Bash", "type": "tool_use",
                "input": {"command": command, "description": description}},
            "result": result,
        }]);
    }
    json!({
        "id": t.event, "type": "response", "timestamp": t.event_ms, "taskId": t.task,
        "conversationId": t.conversation, "data": data,
    })
}

/// A project thread's pull request, open and ready to merge, unless it has one.
fn open_pr(store: &Store, thread: &str) -> flotilla_core::Result<()> {
    let Some(t) = store.thread(thread)? else {
        return Ok(());
    };
    let (Some(project), Some(branch)) = (&t.repo_project_id, &t.branch) else {
        return Ok(());
    };
    let Some(p) = store.project(project)? else {
        return Ok(());
    };
    let prs = store.pull_requests()?;
    if prs.contains_key(thread) {
        return Ok(());
    }
    let number = prs
        .values()
        .map(|(pr, _)| pr.number)
        .max()
        .unwrap_or(0)
        .max(19)
        + 1;
    let pass = |name: &str| Check {
        name: name.into(),
        state: "pass".into(),
        url: None,
        description: None,
    };
    let pr = PullRequest {
        number,
        url: format!("https://github.com/{}/{}/pull/{number}", p.owner, p.repo),
        title: t.title.clone(),
        head: branch.clone(),
        base: p.default_branch.clone(),
        state: "OPEN".into(),
        is_draft: false,
        mergeable: Some("MERGEABLE".into()),
        review: Some("APPROVED".into()),
        additions: 24,
        deletions: 3,
        checks: vec![pass("build"), pass("test")],
        updated_at: Some(now_iso()),
        merged_at: None,
    };
    store.set_pull_request(thread, &pr, false)?;
    Ok(())
}

/// The account's usage as the sidebar shows it: comfortably within the plan.
fn usage() -> Usage {
    Usage {
        vms: Some((1, 100)),
        minute: Some((0, 12)),
        hour: Some((3, 60)),
        day: Some((40, 200)),
        vm_hours: Some((520, 556)),
        renews: None,
        blocked: None,
        starts_free: None,
        extra_starts: None,
        read_ms: Some(now_ms()),
    }
}

/// Put the database at `db` back to the demo's threads, keeping who's paired: the devices, their
/// push registrations, the standing code (made the first time), and the server's id and address.
/// A database that isn't a demo's and has threads is refused, never reset.
pub fn reset(db: &Path) -> Result<(), String> {
    let store = Store::open(db).plain()?;
    let ours = store.kv_get(MARK_KEY).plain()?.is_some();
    if !ours && !store.threads().plain()?.is_empty() {
        return Err(format!(
            "{} has threads and isn't a demo's: give --demo a data directory of its own",
            db.display()
        ));
    }
    // Every table but the devices and kv, and the search indexes (kept by their tables'
    // triggers).
    let conn = store.conn();
    let tables: Vec<(String, String)> = {
        let mut st = conn
            .prepare("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'table'")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    let indexes: Vec<&str> = tables
        .iter()
        .filter(|(_, sql)| sql.starts_with("CREATE VIRTUAL TABLE"))
        .map(|(name, _)| name.as_str())
        .collect();
    let mut sql = String::from("BEGIN;\n");
    for (name, _) in &tables {
        let index = indexes
            .iter()
            .any(|v| name == v || name.starts_with(&format!("{v}_")));
        if !(index || name.starts_with("sqlite_") || name == "devices" || name == "kv") {
            sql.push_str(&format!("DELETE FROM \"{name}\";\n"));
        }
    }
    let kept: Vec<String> = KEPT.iter().map(|k| format!("'{k}'")).collect();
    sql.push_str(&format!(
        "DELETE FROM kv WHERE key NOT IN ({}) AND key NOT LIKE '{}%';\n{SEED}\nCOMMIT;",
        kept.join(", "),
        flotilla_core::push::TOKEN_PREFIX
    ));
    conn.execute_batch(&sql).map_err(|e| {
        let _ = conn.execute_batch("ROLLBACK");
        format!("couldn't reset the demo: {e}")
    })?;
    store.kv_set(MARK_KEY, "1").plain()?;
    if store.kv_get(CODE_KEY).plain()?.is_none() {
        let code = store
            .mint_code(flotilla_core::devices::CODE_TTL, "demo", now_ms())
            .plain()?;
        store.revoke_code(&code.id, now_ms()).plain()?;
        store.kv_set(CODE_KEY, &code.code).plain()?;
    }
    for t in store.threads().plain()? {
        store.rebuild_messages(&t.id).plain()?;
    }
    Ok(())
}

/// The demo's standing code, to show.
pub fn code(store: &Store) -> Option<String> {
    store.kv_get(CODE_KEY).ok().flatten()
}

/// If `given` is the demo's standing code: a new one-time code to pair with in its place (made
/// now), as a code `flotilla pair` makes.
pub fn standing(store: &Store, given: &str, now_ms: i64) -> Option<flotilla_core::devices::Code> {
    let ours = code(store)?;
    let given = flotilla_proto::pair::normalize_code(given)?;
    (given == ours)
        .then(|| {
            store
                .mint_code(flotilla_core::devices::CODE_TTL, "demo", now_ms)
                .ok()
        })
        .flatten()
}

#[cfg(test)]
mod tests;
