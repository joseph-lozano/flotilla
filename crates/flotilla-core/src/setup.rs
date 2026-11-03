//! Setup runs: prepare the VM a project's new threads copy, at a given commit.
//!
//! A run works in a copy, never on the VM threads copy now (blue/green). An update forks the
//! last prepared VM, checks out the commit and reruns the scripts marked for every run; a
//! fresh run starts a new VM in the project's environment (boat clones the repo) and runs every
//! script. A repo's own `.agents/setup` runs before the scripts (see [`crate::agents`]). The VM
//! is then stopped, so forks get a consistent disk, and published. The UI calls all of this
//! the project's *setup*; the prepared VM is never named.

use crate::agents;
use crate::boat::{BoatClient, CreateSandbox, Sandbox, Ttl};
use crate::error::{Error, Result};
use crate::store::{Project, SetupRun, Store};
use crate::util::now_iso;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Long enough for slow setup scripts; the run stops the VM itself when it's done.
pub(crate) const RUN_TTL_SECONDS: u32 = 3 * 3600;
const UP_TIMEOUT: Duration = Duration::from_secs(300);
const STOP_TIMEOUT: Duration = Duration::from_secs(900);
/// One of the project's own scripts may run this long (`.agents/setup`: 20 min).
pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(45 * 60);
/// boat keeps a stopped VM's disk 14 days; refresh the prepared VM before that.
pub const REFRESH_AFTER_DAYS: i64 = 12;

use flotilla_model::setup::FOUND_SETUP;
pub use flotilla_model::setup::{
    STEP_FINISH, STEP_VM, checkout_step, ran_agents_setup, short, stages, tail,
};

/// Quote for `bash -c '…'`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Start a run record. The caller runs it with [`run`] (usually on its own thread).
pub fn new_run(
    store: &Store,
    project_id: &str,
    kind: &str,
    trigger: &str,
    sha: &str,
) -> Result<SetupRun> {
    let run = SetupRun {
        id: uuid::Uuid::new_v4().to_string(),
        project_id: project_id.to_string(),
        kind: kind.to_string(),
        trigger: trigger.to_string(),
        sha: sha.to_string(),
        sandbox_id: None,
        state: "running".into(),
        step: None,
        error: None,
        log: String::new(),
        started_at: now_iso(),
        finished_at: None,
    };
    store.insert_setup_run(&run)?;
    Ok(run)
}

/// Why a run stopped early.
enum Stop {
    Cancelled,
    Failed(String),
}

impl From<Error> for Stop {
    fn from(e: Error) -> Stop {
        Stop::Failed(e.to_string())
    }
}

impl From<crate::BoatError> for Stop {
    fn from(e: crate::BoatError) -> Stop {
        Stop::Failed(e.to_string())
    }
}

struct Runner<'a> {
    boat: &'a BoatClient,
    store: &'a Store,
    run: SetupRun,
    project: Project,
    cancel: &'a AtomicBool,
    notify: &'a dyn Fn(),
    /// The run's VM once made, kept here too: if the project is removed meanwhile, its rows
    /// (this run's included) are gone.
    sandbox: Option<String>,
}

/// The labels on a setup run's VM: the project it's for, and the run.
pub const SETUP_LABEL: &str = "flotilla.setup";
pub const RUN_LABEL: &str = "flotilla.run";

/// Which of a project's setup VMs (id, its run label) nothing needs: not `keep` (the prepared
/// VM and the one before it) and not a run still going (by its run label: a run's VM exists
/// before its id is saved on the run).
pub fn strays(vms: &[(String, Option<String>)], keep: &[&str], running: &[&str]) -> Vec<String> {
    vms.iter()
        .filter(|(id, run)| {
            !keep.contains(&id.as_str()) && !run.as_deref().is_some_and(|r| running.contains(&r))
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// After a run published: delete the project's setup VMs left behind (an old one a publish
/// couldn't delete, a run cut short before its VM was saved). Best effort; the run's log says
/// what went.
fn sweep(boat: &BoatClient, store: &Store, project_id: &str, run_id: &str) {
    let Ok(vms) = boat.list_sandboxes(&[(SETUP_LABEL, project_id)]) else {
        return;
    };
    let Ok(Some(p)) = store.project(project_id) else {
        return;
    };
    // Only this Flotilla's: a copy of this database elsewhere has the same project.
    let Ok(owner) = crate::owner::Owner::load(store) else {
        return;
    };
    let vms: Vec<_> = vms.into_iter().filter(|v| owner.mine(v)).collect();
    let running: Vec<String> = store
        .setup_runs(project_id, 50)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.state == "running")
        .map(|r| r.id)
        .collect();
    let keep: Vec<&str> = [&p.template_sandbox_id, &p.prev_template_sandbox_id]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    let vms: Vec<(String, Option<String>)> = vms
        .iter()
        .map(|v| (v.id.clone(), v.label(RUN_LABEL)))
        .collect();
    let (mut gone, mut kept) = (0, Vec::new());
    for id in strays(
        &vms,
        &keep,
        &running.iter().map(String::as_str).collect::<Vec<_>>(),
    ) {
        match boat.delete_sandbox(&id) {
            Ok(()) => gone += 1,
            Err(e) if e.status() == Some(404) => {}
            Err(e) => kept.push(format!("{id}: {e}")),
        }
    }
    if gone > 0 {
        let _ = store.append_run_log(
            run_id,
            &format!("Deleted {gone} VM(s) earlier setup runs left behind.\n"),
        );
    }
    if !kept.is_empty() {
        let _ = store.append_run_log(
            run_id,
            &format!(
                "Couldn't delete VM(s) earlier setup runs left: {}\n",
                kept.join("; ")
            ),
        );
    }
}

/// Run setup `run_id` to the end: `done` (the VM is published), `failed` or `cancelled`.
/// `notify` is called whenever the run's row changes, so the UI can redraw.
pub fn run(
    boat: &BoatClient,
    store: &Store,
    run_id: &str,
    cancel: &AtomicBool,
    notify: &dyn Fn(),
) -> Result<SetupRun> {
    let run = store
        .setup_run(run_id)?
        .ok_or_else(|| Error::NotFound(format!("setup run {run_id}")))?;
    let project = store
        .project(&run.project_id)?
        .ok_or_else(|| Error::NotFound(format!("project {}", run.project_id)))?;
    let mut r = Runner {
        boat,
        store,
        run,
        project,
        cancel,
        notify,
        sandbox: None,
    };
    let outcome = r.go();
    let sandbox = r.sandbox.clone();
    match outcome {
        Ok(()) => {
            store.finish_run(run_id, "done", None)?;
            sweep(boat, store, &r.project.id, run_id);
        }
        Err(stop) => {
            // Its VM deleted outside Flotilla (the dashboard, the CLI) while it ran: not
            // setup's fault, so not a failure to fix, and the next check runs it again.
            let deleted = matches!(stop, Stop::Failed(_))
                && sandbox
                    .as_deref()
                    .is_some_and(|sb| boat.get_sandbox(sb).is_err_and(|e| e.status() == Some(404)));
            // The half-made VM is of no use: threads keep copying the last good one.
            if let Some(sb) = sandbox.as_deref().filter(|_| !deleted) {
                let _ = boat.delete_sandbox(sb);
            }
            match stop {
                Stop::Cancelled => store.finish_run(run_id, "cancelled", None)?,
                Stop::Failed(_) if deleted => {
                    store.append_run_log(
                        run_id,
                        "\nIts VM was deleted outside Flotilla, so this run stopped.\n",
                    )?;
                    store.finish_run(run_id, "cancelled", None)?;
                }
                Stop::Failed(e) => {
                    store.append_run_log(run_id, &format!("\n{e}\n"))?;
                    store.finish_run(run_id, "failed", Some(&e))?;
                }
            }
        }
    }
    notify();
    store
        .setup_run(run_id)?
        .ok_or_else(|| Error::NotFound(format!("setup run {run_id}")))
}

impl Runner<'_> {
    fn step(&self, step: &str) -> Result<(), Stop> {
        self.check_cancel()?;
        self.store.set_run_step(&self.run.id, step)?;
        self.store
            .append_run_log(&self.run.id, &format!("\n$ {step}\n"))?;
        (self.notify)();
        Ok(())
    }

    fn log(&self, text: &str) -> Result<(), Stop> {
        self.store.append_run_log(&self.run.id, text)?;
        Ok(())
    }

    /// Cancelled, or the project is being (or was) removed.
    fn check_cancel(&self) -> Result<(), Stop> {
        let id = &self.project.id;
        let removed = self.store.project(id).ok().is_some_and(|p| p.is_none())
            || self.store.removing(id).unwrap_or(false);
        if self.cancel.load(Ordering::Relaxed) || removed {
            Err(Stop::Cancelled)
        } else {
            Ok(())
        }
    }

    fn go(&mut self) -> Result<(), Stop> {
        let scripts = self.store.scripts(&self.project.id)?;
        let mut labels = vec![
            (SETUP_LABEL.to_string(), self.project.id.clone()),
            (RUN_LABEL.to_string(), self.run.id.clone()),
        ];
        labels.extend(crate::owner::Owner::load(self.store)?.labels());

        self.step(STEP_VM)?;
        let sb = match (&self.project.template_sandbox_id, self.run.kind.as_str()) {
            (Some(template), "update") => {
                match self.boat.fork_sandbox(
                    template,
                    &labels,
                    Ttl::Seconds(RUN_TTL_SECONDS),
                    None,
                    &self.run.id,
                ) {
                    Ok(sb) => sb,
                    // The prepared VM's disk is gone (or the VM was deleted): start over.
                    Err(e) if e.is("snapshot_data_expired") || e.status() == Some(404) => {
                        self.log(&format!("The last prepared VM can't be copied ({e}); starting from a fresh clone.\n"))?;
                        self.fresh(&labels)?
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            _ => self.fresh(&labels)?,
        };
        self.sandbox = Some(sb.id.clone());
        self.store.set_run_sandbox(&self.run.id, &sb.id)?;
        let name = format!(
            "{} setup: {}",
            crate::flavor::app_name(),
            self.project.full_name()
        );
        let _ = self.boat.update_sandbox(&sb.id, Ttl::Omit, Some(&name));
        self.wait(&sb.id, UP_TIMEOUT, Sandbox::is_up, "start")?;

        let dir = self.project.dir().to_string();
        self.step(&checkout_step(&self.project.default_branch, &self.run.sha))?;
        let checkout = format!(
            "git fetch --prune origin && git checkout --force --detach {} && git clean -fd",
            sh_quote(&self.run.sha)
        );
        let res = self.boat.exec(
            &sb.id,
            &format!("bash -lc {}", sh_quote(&checkout)),
            Some(&dir),
            600,
        )?;
        self.log(&res.stdout)?;
        self.log(&res.stderr)?;
        if !res.ok() {
            let why = if res.stderr.contains("No such file or directory")
                || res.stderr.contains("cannot change to")
            {
                format!(
                    "the repo isn't in the VM at ~/{dir}: check the project's boat environment lists it"
                )
            } else {
                format!("git exited {}", res.exit_code.unwrap_or(-1))
            };
            return Err(Stop::Failed(format!(
                "Couldn't check out {}: {why}",
                short(&self.run.sha)
            )));
        }

        let has = format!("test -e {}", sh_quote(agents::SETUP));
        let agents_setup = self
            .boat
            .exec(
                &sb.id,
                &format!("bash -c {}", sh_quote(&has)),
                Some(&dir),
                60,
            )?
            .ok();
        if agents_setup {
            self.log(&format!("{FOUND_SETUP}\n"))?;
        }
        // The project's own scripts first (as Amp's pre-setup script), then the repo's.
        let fresh = self.run.kind == "fresh";
        for script in scripts.iter().filter(|s| fresh || !s.fresh_only) {
            self.step(&script.command)?;
            self.script(
                &sb.id,
                &dir,
                &script.command,
                &script.command,
                SCRIPT_TIMEOUT,
            )?;
        }
        if agents_setup {
            self.step(agents::SETUP)?;
            let command = agents::command(agents::SETUP, &[("FLOTILLA_SETUP", &self.run.kind)]);
            self.script(&sb.id, &dir, agents::SETUP, &command, agents::SETUP_TIMEOUT)?;
        }

        self.step(STEP_FINISH)?;
        self.boat.stop_sandbox(&sb.id)?;
        self.wait(&sb.id, STOP_TIMEOUT, Sandbox::is_stopped, "stop")?;
        self.check_cancel()?;
        if let Some(old) = self
            .store
            .set_template(&self.project.id, &sb.id, &self.run.sha)?
        {
            match crate::owner::Owner::load(self.store)?.delete_own(self.boat, &old) {
                Ok(_) => {}
                Err(e) => self.log(&format!(
                    "Couldn't delete an old prepared VM ({old}): {e}\n"
                ))?,
            }
        }
        Ok(())
    }

    fn fresh(&mut self, labels: &[(String, String)]) -> Result<Sandbox, Stop> {
        if self.run.kind != "fresh" {
            self.run.kind = "fresh".into();
            self.store.set_run_kind(&self.run.id, "fresh")?;
        }
        let name = format!(
            "{} setup: {}",
            crate::flavor::app_name(),
            self.project.full_name()
        );
        let create = CreateSandbox {
            environment: &self.project.environment_name,
            name: &name,
            ttl: Ttl::Seconds(RUN_TTL_SECONDS),
            labels,
            machine_type: None,
        };
        Ok(self
            .boat
            .create_sandbox(&create, &format!("{}-fresh", self.run.id))?)
    }

    fn wait(
        &self,
        id: &str,
        timeout: Duration,
        done: fn(&Sandbox) -> bool,
        what: &str,
    ) -> Result<(), Stop> {
        let started = Instant::now();
        loop {
            self.check_cancel()?;
            let sb = self.boat.get_sandbox(id)?;
            if done(&sb) {
                return Ok(());
            }
            if matches!(sb.state.as_str(), "error" | "cancelled") {
                return Err(Stop::Failed(format!(
                    "the VM is in {} (it didn't {what})",
                    sb.state
                )));
            }
            if started.elapsed() > timeout {
                return Err(Stop::Failed(format!(
                    "the VM didn't {what} within {} min",
                    timeout.as_secs() / 60
                )));
            }
            sleep(Duration::from_secs(1));
        }
    }

    /// Run one setup script in the background and follow its output to the log, stopping it
    /// after `timeout`. `label` names it in errors.
    fn script(
        &self,
        sb: &str,
        dir: &str,
        label: &str,
        command: &str,
        timeout: Duration,
    ) -> Result<(), Stop> {
        let pid =
            self.boat
                .exec_detached(sb, &format!("bash -lc {}", sh_quote(command)), Some(dir))?;
        let started = Instant::now();
        let (mut out_at, mut err_at) = (0u64, 0u64);
        let mut last_notify = Instant::now();
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                let _ = self.boat.stop_command(sb, pid);
                return Err(Stop::Cancelled);
            }
            let st = self.boat.command_status(sb, pid, out_at, err_at)?;
            let fresh = format!("{}{}", st.stdout, st.stderr);
            if !fresh.is_empty() {
                self.log(&fresh)?;
                if last_notify.elapsed() > Duration::from_millis(900) {
                    (self.notify)();
                    last_notify = Instant::now();
                }
            }
            out_at = st
                .stdout_next_offset
                .unwrap_or(out_at + st.stdout.len() as u64);
            err_at = st
                .stderr_next_offset
                .unwrap_or(err_at + st.stderr.len() as u64);
            if !st.running {
                return match st.exit_code {
                    Some(0) => Ok(()),
                    _ if st.oom_killed => Err(Stop::Failed(format!("{label} ran out of memory"))),
                    Some(code) => Err(Stop::Failed(format!("{label} exited {code}"))),
                    None => Err(Stop::Failed(format!("{label} was stopped"))),
                };
            }
            if started.elapsed() > timeout {
                let _ = self.boat.stop_command(sb, pid);
                return Err(Stop::Failed(format!(
                    "{label} timed out after {} min",
                    timeout.as_secs() / 60
                )));
            }
            sleep(Duration::from_secs(1));
        }
    }
}

pub use flotilla_proto::ScriptFailure;

fn thread_setup_key(thread_id: &str) -> String {
    format!("thread_setup:{thread_id}")
}

/// The setup script that failed when the thread started, if one did.
pub fn thread_setup_failure(store: &Store, thread_id: &str) -> Result<Option<ScriptFailure>> {
    Ok(store
        .kv_get(&thread_setup_key(thread_id))?
        .and_then(|j| serde_json::from_str(&j).ok()))
}

pub fn set_thread_setup_failure(
    store: &Store,
    thread_id: &str,
    failure: Option<&ScriptFailure>,
) -> Result<()> {
    store.kv_set(
        &thread_setup_key(thread_id),
        &failure
            .and_then(|f| serde_json::to_string(f).ok())
            .unwrap_or_default(),
    )
}

/// Run one script in a VM in the background until it ends or `timeout` passes (a thread
/// setting itself up). `label` names it in the failure.
pub fn run_script(
    boat: &BoatClient,
    sb: &str,
    dir: &str,
    label: &str,
    command: &str,
    timeout: Duration,
) -> Result<(), ScriptFailure> {
    let failed = |exit_code, timed_out, output: &str| ScriptFailure {
        label: label.to_string(),
        exit_code,
        timed_out,
        output: output.to_string(),
    };
    let pid = boat
        .exec_detached(sb, &format!("bash -lc {}", sh_quote(command)), Some(dir))
        .map_err(|e| failed(None, false, &e.to_string()))?;
    let started = Instant::now();
    let (mut out_at, mut err_at) = (0u64, 0u64);
    let mut last = String::new();
    loop {
        let st = boat
            .command_status(sb, pid, out_at, err_at)
            .map_err(|e| failed(None, false, &format!("{last}{e}")))?;
        last.push_str(&st.stdout);
        last.push_str(&st.stderr);
        if last.len() > 4000 {
            let cut = (last.len() - 2000..last.len())
                .find(|i| last.is_char_boundary(*i))
                .unwrap_or(0);
            last = last[cut..].to_string();
        }
        out_at = st
            .stdout_next_offset
            .unwrap_or(out_at + st.stdout.len() as u64);
        err_at = st
            .stderr_next_offset
            .unwrap_or(err_at + st.stderr.len() as u64);
        if !st.running {
            return match st.exit_code {
                Some(0) => Ok(()),
                code => Err(failed(code, false, &last)),
            };
        }
        if started.elapsed() > timeout {
            let _ = boat.stop_command(sb, pid);
            return Err(failed(None, true, &last));
        }
        sleep(Duration::from_secs(1));
    }
}

/// After a quit mid-run: mark cut-short runs failed and delete their VMs.
pub fn clean_up_interrupted(boat: &BoatClient, store: &Store) -> Result<()> {
    let owner = crate::owner::Owner::load(store)?;
    for run in store.unfinished_setup_runs()? {
        if let Some(sb) = &run.sandbox_id {
            let _ = owner.delete_own(boat, sb);
        }
        store.finish_run(&run.id, "failed", Some("Flotilla quit while this ran"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sweep_keeps_the_prepared_vms_and_runs_still_going() {
        let vm = |id: &str, run: Option<&str>| (id.to_string(), run.map(str::to_string));
        let vms = [
            vm("bx_now", Some("r3")),    // the prepared VM
            vm("bx_before", Some("r2")), // the one before it
            vm("bx_old", Some("r1")),    // an old one a publish couldn't delete
            vm("bx_going", Some("r4")),  // a run still going, its id not saved yet
            vm("bx_cut", Some("r0")),    // a run cut short by a quit
            vm("bx_unlabelled", None), // no run label: not one we can vouch for, but it's the project's
        ];
        assert_eq!(
            strays(&vms, &["bx_now", "bx_before"], &["r4"]),
            ["bx_old", "bx_cut", "bx_unlabelled"]
        );
        assert!(
            strays(
                &vms,
                &["bx_now", "bx_before", "bx_old", "bx_cut", "bx_unlabelled"],
                &["r4"]
            )
            .is_empty()
        );
    }

    /// A stand-in for boat that makes VM `bx_1` and then answers 404 for it, as if it were
    /// deleted from the dashboard. Returns its base URL and the requests it saw.
    fn boat_that_loses_its_vm() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Headers, then as much body as Content-Length says.
                let (head, body_len) = loop {
                    let n = conn.read(&mut chunk).unwrap_or(0);
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        break (head, end + 4 + len);
                    }
                    if n == 0 {
                        break (String::new(), 0);
                    }
                };
                while buf.len() < body_len {
                    match conn.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let line = head.lines().next().unwrap_or_default().to_string();
                log.lock().unwrap().push(line.clone());
                let (status, body) = if line.starts_with("POST /api/v1/sandboxes ") {
                    ("200 OK", r#"{"id":"bx_1","state":"provisioned"}"#)
                } else {
                    (
                        "404 Not Found",
                        r#"{"ok":false,"status":404,"code":"not_found","message":"Sandbox not found"}"#,
                    )
                };
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (base, seen)
    }

    #[test]
    fn a_run_whose_vm_is_deleted_is_cancelled_not_failed() {
        let (base, seen) = boat_that_loses_its_vm();
        let boat =
            BoatClient::with_base("key".into(), &base, &format!("{base}/provider-models")).unwrap();
        let store = Store::open_in_memory().unwrap();
        let p = Project {
            id: "p".into(),
            owner: "o".into(),
            repo: "r".into(),
            default_branch: "main".into(),
            environment_name: "flotilla-r".into(),
            created_at: now_iso(),
            template_sandbox_id: None,
            template_sha: None,
            template_built_at: None,
            prev_template_sandbox_id: None,
            head_sha: None,
            head_etag: None,
            checked_at: None,
            machine_type: None,
        };
        store.insert_project(&p, &[]).unwrap();
        let run = new_run(&store, "p", "fresh", "added", "3f2a1c9d").unwrap();
        let done = super::run(&boat, &store, &run.id, &AtomicBool::new(false), &|| {}).unwrap();
        assert_eq!((done.state.as_str(), done.error), ("cancelled", None));
        assert!(
            done.log.contains("deleted outside Flotilla"),
            "{}",
            done.log
        );
        // Nothing left to delete.
        assert!(
            !seen.lock().unwrap().iter().any(|l| l.starts_with("DELETE")),
            "{:?}",
            seen.lock().unwrap()
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("npm ci"), "'npm ci'");
        assert_eq!(sh_quote("echo 'hi'"), r"'echo '\''hi'\'''");
    }

    #[test]
    fn thread_setup_failures_say_what_happened() {
        let f = |exit_code, timed_out| ScriptFailure {
            label: ".agents/setup".into(),
            exit_code,
            timed_out,
            output: "boom\n".into(),
        };
        assert_eq!(
            f(Some(1), false).summary(),
            "Setup failed (exit 1) at .agents/setup"
        );
        assert_eq!(f(None, true).summary(), "Setup timed out at .agents/setup");
        assert_eq!(f(None, false).summary(), "Setup failed at .agents/setup");
        let store = Store::open_in_memory().unwrap();
        assert_eq!(thread_setup_failure(&store, "t1").unwrap(), None);
        set_thread_setup_failure(&store, "t1", Some(&f(Some(2), false))).unwrap();
        assert_eq!(
            thread_setup_failure(&store, "t1").unwrap(),
            Some(f(Some(2), false))
        );
        set_thread_setup_failure(&store, "t1", None).unwrap();
        assert_eq!(thread_setup_failure(&store, "t1").unwrap(), None);
    }
}
