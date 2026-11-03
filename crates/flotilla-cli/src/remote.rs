//! The CLI's commands when a server runs on the database (`flotilla serve`, or the app on This
//! Mac, which runs one inside itself): the same commands, as requests and views over its Unix
//! socket, so nothing here writes the database behind its back.

use crate::{Cmd, OnOff, print_catalog, print_message, thread_line};
use flotilla_client::{Client, Info, Target};
use flotilla_proto::{
    ClientMsg, Connection, Message, Note, RawEvent, Reply, ReqId, Request, Secret, Sent, ServerMsg,
    ServiceCmd, Sidebar, ThreadDetail, ThreadState, ThreadSummary, View, ViewKey,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long to wait for the server to answer or send a view.
const PATIENCE: Duration = Duration::from_secs(30);
/// The setup checks ask boat, gh, GitHub and claude one after another, each with its own limit.
const DOCTOR_PATIENCE: Duration = Duration::from_secs(180);

struct Conn {
    client: Client,
    /// Where the server is, for its HTTP reads (older pages, whole strings), and this
    /// machine's agent token there (`--server`; none on the Unix socket).
    target: Target,
    token: Option<Secret>,
    rx: mpsc::Receiver<ServerMsg>,
    next: ReqId,
    views: HashMap<ViewKey, View>,
    answers: HashMap<ReqId, Result<Reply, String>>,
    notes: VecDeque<Note>,
    live: Option<(String, Vec<RawEvent>)>,
}

impl Conn {
    fn connect(address: &str) -> Result<Conn, String> {
        Conn::connect_as(address, None)
    }

    /// The server's Unix socket needs no token (this machine's user); another server, this
    /// machine's agent token there.
    fn connect_as(address: &str, token: Option<Secret>) -> Result<Conn, String> {
        let target = Target::parse(address)?;
        let (tx, rx) = mpsc::channel();
        let info = Info {
            app: "flotilla-cli".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            token: token.clone(),
            scope: None,
        };
        let client = Client::connect(target.clone(), info, move |m| tx.send(m).is_ok());
        Ok(Conn {
            client,
            target,
            token,
            rx,
            next: 1,
            views: HashMap::new(),
            answers: HashMap::new(),
            notes: VecDeque::new(),
            live: None,
        })
    }

    /// Take what the server sent, for up to `wait`. False if nothing came.
    fn pump(&mut self, wait: Duration) -> Result<bool, String> {
        let msg = match self.rx.recv_timeout(wait) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Lost the connection to the server".into());
            }
        };
        match msg {
            ServerMsg::Snap { view } => {
                self.views.insert(view.key(), *view);
            }
            ServerMsg::Res { id, result } => {
                self.answers.insert(id, result);
            }
            ServerMsg::Note(n) | ServerMsg::Progress { note: n, .. } => self.notes.push_back(n),
            ServerMsg::Live { thread, partials } => self.live = Some((thread, partials)),
            ServerMsg::Connection(Connection::Refused { reason }) => return Err(reason),
            ServerMsg::Connection(_) => {}
        }
        Ok(true)
    }

    fn until<T>(
        &mut self,
        what: &str,
        done: impl FnMut(&mut Conn) -> Option<T>,
    ) -> Result<T, String> {
        self.until_within(what, PATIENCE, done)
    }

    fn until_within<T>(
        &mut self,
        what: &str,
        patience: Duration,
        mut done: impl FnMut(&mut Conn) -> Option<T>,
    ) -> Result<T, String> {
        let until = Instant::now() + patience;
        loop {
            if let Some(t) = done(self) {
                return Ok(t);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("The server didn't send {what} in time"));
            }
            self.pump(left)?;
        }
    }

    fn sub(&mut self, view: ViewKey) {
        self.client.send(ClientMsg::Sub { view });
    }

    fn view(&mut self, key: ViewKey) -> Result<View, String> {
        if !self.views.contains_key(&key) {
            self.sub(key.clone());
        }
        let name = key.name();
        self.until(name, |c| c.views.get(&key).cloned())
    }

    fn sidebar(&mut self) -> Result<Sidebar, String> {
        match self.view(ViewKey::Sidebar)? {
            View::Sidebar(s) => Ok(s),
            _ => Err("not a sidebar".into()),
        }
    }

    fn detail(&mut self, id: &str) -> Result<ThreadDetail, String> {
        match self.view(ViewKey::Thread(id.to_string()))? {
            View::Thread(d) => Ok(*d),
            _ => Err("not a thread".into()),
        }
    }

    /// The messages before a thread's view (which holds only its newest), a page at a time
    /// over the socket's HTTP.
    fn older(&self, id: &str, detail: &ThreadDetail) -> Result<Vec<Message>, String> {
        let mut out = Vec::new();
        for page in 0..detail.older / flotilla_proto::history::PAGE {
            out.extend(
                flotilla_client::history(
                    &self.target,
                    self.token.as_ref(),
                    id,
                    flotilla_proto::History::before(page),
                )?
                .messages,
            );
        }
        Ok(out)
    }

    /// A message with its cut strings whole again.
    fn uncut(&self, id: &str, mut m: Message) -> Result<Message, String> {
        for cut in std::mem::take(&mut m.cut) {
            let whole = flotilla_client::full(&self.target, self.token.as_ref(), id, m.seq, &cut)?;
            match cut.field.as_str() {
                "tool_input_json" => m.tool_input_json = Some(whole),
                _ => m.tool_result_json = Some(whole),
            }
        }
        Ok(m)
    }

    /// Send a request; its answer.
    fn ask(&mut self, req: Request) -> Result<Reply, String> {
        let id = self.next;
        self.next += 1;
        self.client.send(ClientMsg::Req { id, req });
        self.until("an answer", |c| c.answers.remove(&id))?
    }

    /// A thread by its id or the start of it, as the sidebar lists it.
    fn find(&mut self, query: &str) -> Result<ThreadSummary, String> {
        let rows = self.sidebar()?.threads;
        let found: Vec<&ThreadSummary> = rows
            .iter()
            .filter(|s| s.thread.id.starts_with(query))
            .collect();
        match found[..] {
            [one] => Ok(one.clone()),
            [] => Err(format!("no thread {query:?}")),
            _ => Err(format!(
                "{query:?} matches {} threads; use a longer prefix",
                found.len()
            )),
        }
    }

    fn row(&mut self, id: &str) -> Result<Option<ThreadSummary>, String> {
        Ok(self
            .sidebar()?
            .threads
            .into_iter()
            .find(|s| s.thread.id == id))
    }
}

/// The devices connected to the server now.
pub fn connected_devices(address: &str) -> Result<HashSet<String>, String> {
    let mut c = Conn::connect(address)?;
    match c.view(ViewKey::Devices)? {
        View::Devices(d) => Ok(d
            .devices
            .into_iter()
            .filter(|d| d.connected)
            .map(|d| d.id)
            .collect()),
        _ => Err("not the devices".into()),
    }
}

/// The server's setup checks, run now.
pub fn doctor(address: &str) -> Result<flotilla_proto::Doctor, String> {
    let mut c = Conn::connect(address)?;
    let id = c.next;
    c.next += 1;
    c.client.send(ClientMsg::Req {
        id,
        req: Request::RunDoctor,
    });
    c.until_within("the setup checks", DOCTOR_PATIENCE, |c| {
        c.answers.remove(&id)
    })??;
    match c.view(ViewKey::Settings)? {
        View::Settings(s) => s
            .doctor
            .ok_or_else(|| "The server hasn't checked yet".into()),
        _ => Err("not the settings".into()),
    }
}

pub fn run(cmd: Cmd, address: &str) -> Result<(), String> {
    run_as(cmd, address, None)
}

/// [`run`] with a device's token: `--server`.
pub fn run_as(cmd: Cmd, address: &str, token: Option<Secret>) -> Result<(), String> {
    let mut c = Conn::connect_as(address, token)?;
    match cmd {
        Cmd::Envs => {
            for p in c.sidebar()?.environments {
                println!(
                    "{}  {}{}",
                    p.id,
                    p.name,
                    if p.is_default { "  (default)" } else { "" }
                );
            }
        }
        Cmd::Models { provider } => match c.view(ViewKey::Catalog)? {
            View::Catalog(Some(catalog)) => print_catalog(&catalog, provider.as_deref()),
            _ => return Err("The server hasn't read boat's models yet".into()),
        },
        Cmd::Limits => match c.sidebar()?.usage {
            Some(u) => {
                let pair = |p: Option<(i64, i64)>| {
                    p.map(|(a, b)| format!("{a}/{b}"))
                        .unwrap_or_else(|| "-".into())
                };
                println!(
                    "VMs {}  starts: minute {}  hour {}  day {}  VM hours left {}",
                    pair(u.vms),
                    pair(u.minute),
                    pair(u.hour),
                    pair(u.day),
                    pair(u.vm_hours)
                );
                if let Some(b) = u.blocked {
                    println!("blocked: {b}");
                }
            }
            None => println!("The server hasn't read the account's limits yet"),
        },
        Cmd::Threads => {
            let rows = c.sidebar()?.threads;
            if rows.is_empty() {
                println!("no threads");
            }
            for s in rows {
                println!("{}", thread_line(&s.thread));
            }
        }
        Cmd::Search { query, limit } => {
            let started = Instant::now();
            match c.ask(Request::Search {
                query: query.join(" "),
                limit,
            })? {
                Reply::Hits(hits) => crate::print_hits(&hits, started.elapsed()),
                other => return Err(format!("unexpected answer: {other:?}")),
            }
        }
        Cmd::Sync => {
            c.ask(Request::SyncNow)?;
            println!("the server is syncing");
        }
        Cmd::New {
            environment,
            provider,
            model,
            effort,
            fast,
            title,
            machine_type,
            wait,
            no_follow,
            prompt,
        } => {
            let environment = match environment {
                Some(e) => e,
                None => c
                    .sidebar()?
                    .environments
                    .into_iter()
                    .find(|e| e.is_default)
                    .map(|e| e.name)
                    .ok_or("no default environment; pass --env")?,
            };
            let id = uuid::Uuid::new_v4().to_string();
            let req = flotilla_proto::NewThread {
                environment,
                project_id: None,
                provider,
                model,
                effort,
                fast,
                title,
                machine_type,
                prompt,
                attachments: vec![],
                wait_ready: wait,
                on_branch: None,
                id: Some(id.clone()),
                parent: None,
                branch_tag: None,
            };
            let started = Instant::now();
            c.ask(Request::NewThread(Box::new(req)))?;
            let sandbox = c
                .row(&id)?
                .and_then(|s| s.thread.sandbox_id)
                .unwrap_or_else(|| "-".into());
            println!(
                "created {id}  sandbox {sandbox}  ({:.1}s)",
                started.elapsed().as_secs_f64()
            );
            if !no_follow {
                follow(&mut c, &id, false)?;
            }
        }
        Cmd::Send {
            thread,
            text,
            no_follow,
        } => {
            let id = send(&mut c, &thread, text)?;
            if !no_follow {
                follow(&mut c, &id, false)?;
            }
        }
        // The server steers while a turn runs, and prompts otherwise.
        Cmd::Steer { thread, text } => {
            send(&mut c, &thread, text)?;
        }
        Cmd::Follow { thread } => {
            let t = c.find(&thread)?;
            follow(&mut c, &t.thread.id, true)?;
        }
        Cmd::Show { thread, verbose } => {
            let t = c.find(&thread)?;
            println!("{}", thread_line(&t.thread));
            let detail = c.detail(&t.thread.id)?;
            for m in c
                .older(&t.thread.id, &detail)?
                .into_iter()
                .chain(detail.messages)
            {
                // Verbose shows tool inputs whole: those cut on the wire are fetched.
                let m = if verbose && !m.cut.is_empty() {
                    c.uncut(&t.thread.id, m)?
                } else {
                    m
                };
                print_message(&m, verbose, "");
            }
        }
        Cmd::Interrupt { thread } => {
            let t = c.find(&thread)?;
            c.ask(Request::Interrupt(t.thread.id))?;
            println!("interrupted");
        }
        Cmd::Stop { thread, .. } => done(&mut c, &thread, Request::Stop)?,
        Cmd::Resume { thread } => done(&mut c, &thread, Request::Resume)?,
        Cmd::Rename { thread, title } => {
            done(&mut c, &thread, |id| Request::Rename { thread: id, title })?
        }
        Cmd::Archive { thread } => {
            let t = c.find(&thread)?;
            c.ask(Request::Archive(t.thread.id.clone()))?;
            println!("archived {}", t.thread.id);
        }
        Cmd::Unarchive { thread } => {
            let t = c.find(&thread)?;
            c.ask(Request::Unarchive(t.thread.id.clone()))?;
            println!("unarchived {}", t.thread.id);
        }
        Cmd::KeepRunning { thread, state } => done(&mut c, &thread, |id| Request::KeepRunning {
            thread: id,
            on: matches!(state, OnOff::On),
        })?,
        Cmd::Preview { thread, port } => {
            let t = c.find(&thread)?;
            match c.ask(Request::Preview {
                thread: t.thread.id,
                port,
            })? {
                Reply::Url(u) => println!("{u}"),
                other => println!("{other:?}"),
            }
        }
        Cmd::Rebuild { yes } => {
            if !yes {
                return Err(
                    "rebuild renames the database aside and re-downloads everything; pass --yes"
                        .into(),
                );
            }
            c.ask(Request::Rebuild)?;
            let result = c.until("the rebuild's end", |c| {
                let i = c.notes.iter().position(|n| matches!(n, Note::Rebuilt(_)))?;
                match c.notes.remove(i) {
                    Some(Note::Rebuilt(r)) => Some(r),
                    _ => None,
                }
            })?;
            println!("moved the old database to {}", result?);
        }
        Cmd::Delete { thread, yes } => {
            let t = c.find(&thread)?;
            let sandbox = t.thread.sandbox_id.clone().unwrap_or_else(|| "-".into());
            if !yes {
                return Err(format!(
                    "this deletes sandbox {sandbox} for good; pass --yes"
                ));
            }
            c.ask(Request::Delete(t.thread.id.clone()))?;
            println!("deleted {} ({sandbox})", t.thread.id);
        }
        Cmd::Schedule(sc) => schedule(&mut c, sc)?,
        Cmd::Services {
            thread,
            action,
            name,
        } => {
            let t = c.find(&thread)?;
            let id = t.thread.id.clone();
            let need_name = || name.clone().ok_or_else(|| "which service?".to_string());
            let cmd = match action.as_deref() {
                None => None,
                Some("ensure") => Some(ServiceCmd::Ensure(id.clone())),
                Some("restart") => Some(ServiceCmd::Restart { thread: id.clone(), name: need_name()? }),
                Some("stop") => Some(ServiceCmd::Stop { thread: id.clone(), name: need_name()? }),
                Some("check") => return Err("the server checks the open threads' services itself; `check` runs only without a server".into()),
                Some(other) => return Err(format!("unknown services action {other:?}: ensure, restart, stop or check")),
            };
            if let Some(cmd) = cmd {
                c.ask(Request::Services(cmd))?;
                println!("asked the server; it runs in the background");
            }
            match c.detail(&id)?.services {
                None => println!("no services"),
                Some(s) => {
                    println!("{} on {}", s.file.as_deref().unwrap_or("?"), s.sandbox);
                    if let Some(e) = &s.error {
                        println!("error: {e}");
                    }
                    for r in &s.services {
                        let url = r
                            .preview
                            .as_deref()
                            .map(flotilla_core::services::origin)
                            .unwrap_or_default();
                        println!("  {:<12} :{:<5} {:<15} {url}", r.name, r.port, r.state);
                    }
                }
            }
        }
    }
    Ok(())
}

/// `flotilla schedule …` through the server.
fn schedule(c: &mut Conn, sc: crate::schedule::ScheduleCmd) -> Result<(), String> {
    use crate::schedule::{self as cli, ScheduleCmd};
    let all = |c: &mut Conn| match c.view(ViewKey::Schedules)? {
        View::Schedules(s) => Ok(s.schedules),
        _ => Err("not the schedules".to_string()),
    };
    let now = flotilla_core::util::now_ms();
    match sc {
        ScheduleCmd::List => {
            let schedules = all(c)?;
            let rows = c.sidebar()?.threads;
            if schedules.is_empty() {
                println!("nothing scheduled");
            }
            for s in &schedules {
                let to = match &s.target {
                    flotilla_proto::Target::Thread(t) => rows
                        .iter()
                        .find(|r| &r.thread.id == t)
                        .map(|r| r.thread.title.clone())
                        .unwrap_or_else(|| t.clone()),
                    flotilla_proto::Target::New(f) => {
                        format!("new thread ({} {})", f.provider, f.model)
                    }
                };
                println!("{}", cli::line(s, &to, now));
            }
        }
        ScheduleCmd::Add(a) => {
            let thread = match &a.thread {
                Some(t) => Some(c.find(t)?.thread.id),
                None => None,
            };
            let environment = c
                .sidebar()?
                .environments
                .into_iter()
                .find(|e| e.is_default)
                .map(|e| e.name);
            let spec = cli::spec(&a, thread, environment, now)?;
            let id = spec.id.clone();
            c.ask(Request::PutSchedule(Box::new(spec)))?;
            let kept = all(c)?
                .into_iter()
                .find(|s| s.id == id)
                .ok_or("the server didn't keep it")?;
            println!(
                "scheduled {}  {}",
                cli::short(&id),
                flotilla_model::schedule::summary(&kept, now, &flotilla_model::schedule::here())
            );
        }
        ScheduleCmd::Rm { id } => {
            let s = cli::find(&all(c)?, &id)?.clone();
            c.ask(Request::DeleteSchedule(s.id.clone()))?;
            println!("deleted {}", cli::short(&s.id));
        }
        ScheduleCmd::Pause { id } => {
            let s = cli::find(&all(c)?, &id)?.clone();
            c.ask(Request::PauseSchedule {
                schedule: s.id.clone(),
                paused: true,
            })?;
            println!("paused {}", cli::short(&s.id));
        }
        ScheduleCmd::Resume { id } => {
            let s = cli::find(&all(c)?, &id)?.clone();
            c.ask(Request::PauseSchedule {
                schedule: s.id.clone(),
                paused: false,
            })?;
            println!("resumed {}", cli::short(&s.id));
        }
        ScheduleCmd::Run { id } => {
            let s = cli::find(&all(c)?, &id)?.clone();
            c.ask(Request::RunSchedule(s.id.clone()))?;
            println!("running {}", cli::short(&s.id));
        }
    }
    Ok(())
}

/// Send a message (with this CLI's own `local-` id, so a resend is a no-op); its thread.
fn send(c: &mut Conn, thread: &str, text: String) -> Result<String, String> {
    let t = c.find(thread)?;
    let local = format!(
        "{}{}",
        flotilla_core::lifecycle::LOCAL_PREFIX,
        uuid::Uuid::new_v4()
    );
    match c.ask(Request::Send {
        thread: t.thread.id.clone(),
        text,
        files: vec![],
        local,
    })? {
        Reply::Sent(Sent::Prompt(id)) => println!("prompted ({id})"),
        Reply::Sent(Sent::Steer(id)) => println!("steered ({id})"),
        other => println!("{other:?}"),
    }
    Ok(t.thread.id)
}

/// An action on a thread, then its row.
fn done(c: &mut Conn, thread: &str, req: impl FnOnce(String) -> Request) -> Result<(), String> {
    let t = c.find(thread)?;
    c.ask(req(t.thread.id.clone()))?;
    // The views it changed came before the answer.
    match c.row(&t.thread.id)? {
        Some(s) => println!("{}", thread_line(&s.thread)),
        None => println!("{}", t.thread.id),
    }
    Ok(())
}

/// Print a thread's messages as they settle, and its streaming text as it grows, until its
/// turn ends.
fn follow(c: &mut Conn, id: &str, from_start: bool) -> Result<(), String> {
    let mut out = std::io::stdout();
    if from_start {
        // What came before the view, once: the loop prints the view's as they settle.
        let first = c.detail(id)?;
        for m in c.older(id, &first)? {
            print_message(&m, false, "");
        }
    }
    follow_views(c, id, from_start, &mut out)
}

/// What `follow` reads of a server: a thread's view and its sidebar row as they come, and its
/// streaming text.
trait Views {
    fn detail(&mut self, id: &str) -> Result<ThreadDetail, String>;
    fn row(&mut self, id: &str) -> Result<Option<ThreadSummary>, String>;
    /// The thread's streaming text, if more came since the last look.
    fn partials(&mut self, id: &str) -> Option<Vec<RawEvent>>;
    /// Take what the server sent, for up to `wait`. False if nothing came.
    fn pump(&mut self, wait: Duration) -> Result<bool, String>;
}

impl Views for Conn {
    fn detail(&mut self, id: &str) -> Result<ThreadDetail, String> {
        Conn::detail(self, id)
    }
    fn row(&mut self, id: &str) -> Result<Option<ThreadSummary>, String> {
        Conn::row(self, id)
    }
    fn partials(&mut self, id: &str) -> Option<Vec<RawEvent>> {
        self.live.take().filter(|(t, _)| t == id).map(|(_, p)| p)
    }
    fn pump(&mut self, wait: Duration) -> Result<bool, String> {
        Conn::pump(self, wait)
    }
}

/// Once the turn's over, how long a quiet spell means the server has sent the rest of what
/// changed with it, and how long to wait for that at most.
const SETTLED_QUIET: Duration = Duration::from_millis(250);
const SETTLED_PATIENCE: Duration = Duration::from_secs(3);

/// [`follow`]'s loop, writing to `out`: `from_start`, the view's messages too, else only
/// what comes after it.
fn follow_views(
    c: &mut impl Views,
    id: &str,
    from_start: bool,
    out: &mut impl std::io::Write,
) -> Result<(), String> {
    let busy = |s: &ThreadSummary| {
        s.activity == flotilla_proto::Activity::Working
            || matches!(
                s.thread.state,
                ThreadState::Creating | ThreadState::Starting
            )
    };
    let mut shown = crate::shown::Shown::default();
    if !from_start {
        shown.seed(&c.detail(id)?.messages);
    }
    let started = Instant::now();
    let mut was_busy = false;
    let io = |e: std::io::Error| e.to_string();
    loop {
        let detail = c.detail(id)?;
        let blocked = shown.settle(out, &detail.messages).map_err(io)?;
        if !blocked && let Some(partials) = c.partials(id) {
            shown.stream(out, &partials).map_err(io)?;
        }
        let row = c.row(id)?;
        let now_busy = row.as_ref().is_some_and(busy);
        was_busy |= now_busy;
        // Done once it was at work and isn't, or it never got going.
        if (was_busy && !now_busy)
            || (!now_busy && started.elapsed() > Duration::from_secs(60))
            || row.is_none()
        {
            // The server sends the sidebar before the thread's view: the row can say the turn's
            // over while the view with its last messages (Codex's answer, which comes with the
            // turn's end) is still on its way. Take what's still coming, then look once more.
            let until = Instant::now() + SETTLED_PATIENCE;
            while Instant::now() < until && c.pump(SETTLED_QUIET)? {}
            shown.settle(out, &c.detail(id)?.messages).map_err(io)?;
            shown.finish(out).map_err(io)?;
            if let Some(s) = c.row(id)? {
                writeln!(out, "\n[{}]", thread_line(&s.thread)).map_err(io)?;
            }
            return Ok(());
        }
        c.pump(Duration::from_secs(1))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_proto::{Activity, Thread};

    /// A server as `follow` reads it: what it shows now, and what it sends next, one message a
    /// pump, in order (a batch's sidebar before its thread's view, as the host sends them).
    struct Scripted {
        detail: ThreadDetail,
        row: ThreadSummary,
        coming: VecDeque<View>,
    }

    impl Views for Scripted {
        fn detail(&mut self, _: &str) -> Result<ThreadDetail, String> {
            Ok(self.detail.clone())
        }
        fn row(&mut self, _: &str) -> Result<Option<ThreadSummary>, String> {
            Ok(Some(self.row.clone()))
        }
        fn partials(&mut self, _: &str) -> Option<Vec<RawEvent>> {
            None
        }
        fn pump(&mut self, _: Duration) -> Result<bool, String> {
            match self.coming.pop_front() {
                Some(View::Thread(d)) => self.detail = *d,
                Some(View::Sidebar(s)) => self.row = s.threads[0].clone(),
                Some(_) => {}
                None => return Ok(false),
            }
            Ok(true)
        }
    }

    fn detail(messages: Vec<Message>) -> ThreadDetail {
        serde_json::from_value(serde_json::json!({"id": "t1", "thread": null, "messages": messages, "sent": [], "children": []})).unwrap()
    }

    fn row(activity: Activity) -> ThreadSummary {
        let thread = Thread {
            id: "t1".into(),
            environment_id: None,
            environment_name: None,
            title: "Astra check".into(),
            sandbox_id: Some("bx_1".into()),
            conversation_id: Some("c1".into()),
            provider: "codex".into(),
            model: "gpt-6-astra".into(),
            effort: None,
            fast: false,
            state: ThreadState::Idle,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: "2026-11-02T18:00:00Z".into(),
            updated_at: "2026-11-02T18:00:00Z".into(),
            repo_project_id: None,
            branch: None,
            base_sha: None,
        };
        serde_json::from_value(serde_json::json!({"thread": thread, "activity": activity, "last_activity_ms": 0, "turn_started_ms": null, "last_reply": null, "last_prompt": null, "pr": null, "start_failed": false})).unwrap()
    }

    fn sidebar(activity: Activity) -> View {
        View::Sidebar(Sidebar {
            threads: vec![row(activity)],
            parents: Default::default(),
            waiting: vec![],
            projects: vec![],
            environments: vec![],
            status: Default::default(),
            github: flotilla_proto::GhStatus::Unknown,
            usage: None,
            engine: flotilla_proto::EngineState::Offline,
            limits: vec![],
        })
    }

    fn message(seq: i64, kind: &str, key: &str, text: &str) -> Message {
        Message {
            seq,
            kind: kind.into(),
            text: Some(text.into()),
            event_key: Some(key.into()),
            ..Default::default()
        }
    }

    /// Codex's answer comes with its turn's end, in the thread's view the server sends just
    /// after the sidebar saying it's idle: it's printed, then the row.
    #[test]
    fn the_answer_that_comes_with_the_turns_end_is_printed() {
        let prompt = message(0, "prompt", "p1", "What kernel is this?");
        let tool = Message {
            kind: "tool".into(),
            tool_name: Some("Bash".into()),
            tool_summary: Some("uname -sr".into()),
            tool_id: Some("exec-1".into()),
            ..message(1, "tool", "r1", "")
        };
        let answer = message(2, "text", "r2", "Kernel release: 6.8.0-117-generic");
        let mut server = Scripted {
            detail: detail(vec![prompt.clone()]),
            row: row(Activity::Working),
            coming: VecDeque::from([
                View::Thread(Box::new(detail(vec![prompt.clone(), tool.clone()]))),
                sidebar(Activity::Unread),
                View::Thread(Box::new(detail(vec![prompt, tool, answer]))),
            ]),
        };
        let mut out = Vec::new();
        follow_views(&mut server, "t1", false, &mut out).unwrap();
        let said = String::from_utf8(out).unwrap();
        assert!(
            said.contains("uname -sr") && said.contains("Kernel release: 6.8.0-117-generic"),
            "{said}"
        );
        assert!(
            said.find("Kernel release").unwrap()
                < said
                    .find("[t1")
                    .or_else(|| said.find("Astra check"))
                    .unwrap(),
            "the answer, then the row: {said}"
        );
        assert!(
            !said.contains("What kernel is this?"),
            "what the view showed already isn't printed: {said}"
        );
    }
}
