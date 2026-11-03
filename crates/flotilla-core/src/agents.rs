//! Scripts a repo checks in under `.agents/`, which Flotilla runs at fixed points, as Amp's
//! orbs do (ampcode.com/docs/orbs/customizing):
//!
//! - `.agents/setup` runs in every setup run and at every project thread's start, after the
//!   project's own scripts (Amp's pre-setup script). `FLOTILLA_SETUP` says `fresh` or
//!   `update`. It's stopped after 20 min.
//! - `.agents/resume` runs when a project thread's stopped VM starts again (a send that wakes
//!   it, or Resume VM), before the agent gets the prompt. It's for quick work after a wake:
//!   signing in again, reconnecting, repairing state. It must finish within 10 s: one still
//!   running then is stopped, and the prompt goes anyway. It doesn't run when a thread first
//!   starts (setup just ran), and it isn't for dev servers: long-running services will be
//!   declared in a services file of their own. (Amp also runs it after an orb's first
//!   activation, since its orbs start from a snapshot without rerunning setup, and lets it
//!   keep running past 10 s.)
//!
//! - `.agents/archive` runs when a project thread is archived (or deleted without being
//!   archived), before its VM stops: for removing third-party or remote resources made for the
//!   thread, say a PlanetScale database branch or a preview deployment. It gets 2 min, and the
//!   archive goes ahead whatever it does. There's no unarchive script: the next send wakes the
//!   VM and `.agents/resume` brings back what archive removed.
//!
//! All get `FLOTILLA=1`, run with their own shebang if executable (bash otherwise), and leave
//! their output in `~/.cache/flotilla/logs/{setup,resume,archive}.log` in the VM, replaced each
//! run. In a thread (setup at its start, resume, archive) they also get `FLOTILLA_THREAD_ID`
//! and `FLOTILLA_BRANCH`; a setup run preparing the project's VM doesn't set them, so a script
//! can tell the two apart.

use crate::boat::{BoatClient, CommandStatus};
use crate::error::{Error, Result};
use crate::setup::sh_quote;
use crate::store::{Store, Thread};
use crate::util::now_iso;
use std::thread::sleep;
use std::time::{Duration, Instant};

pub use flotilla_model::agents::{
    ARCHIVE, ARCHIVE_TIMEOUT, Hook, RESUME, RESUME_TIMEOUT, SETUP, SETUP_TIMEOUT,
};
/// How much of a hook's output to keep.
const OUTPUT_CHARS: usize = 2000;

/// Where a thread's last run of `hook` is kept in `kv`.
fn key(hook: Hook, thread_id: &str) -> String {
    match hook {
        Hook::Resume => format!("resume:{thread_id}"),
        Hook::Archive => format!("archive:{thread_id}"),
    }
}

/// What the wrapper prints when the repo has no such script.
fn none(hook: Hook) -> String {
    format!("flotilla: no {}", hook.path())
}

/// A thread's own variables for its hooks.
pub fn thread_env(thread_id: &str, branch: Option<&str>) -> Vec<(&'static str, String)> {
    let mut env = vec![("FLOTILLA_THREAD_ID", thread_id.to_string())];
    if let Some(b) = branch {
        env.push(("FLOTILLA_BRANCH", b.to_string()));
    }
    env
}
/// Where the hooks' output goes in the VM, for the agent to read.
const LOG_DIR: &str = "~/.cache/flotilla/logs";

/// The shell line (for `bash -lc`) that runs repo script `path` from the repo's folder with
/// `FLOTILLA=1` and `env` set, copying its output to `~/.cache/flotilla/logs/<name>.log`. The
/// exit code is the script's.
pub fn command(path: &str, env: &[(&str, &str)]) -> String {
    let env: String = [("FLOTILLA", "1")]
        .iter()
        .chain(env)
        .map(|(k, v)| format!("{k}={} ", sh_quote(v)))
        .collect();
    let p = sh_quote(path);
    let log = format!("{LOG_DIR}/{}.log", path.rsplit('/').next().unwrap_or(path));
    format!(
        "mkdir -p {LOG_DIR} && set -o pipefail && {{ if [ -x {p} ]; then {env}./{p}; else {env}bash {p}; fi; }} 2>&1 | tee {log}"
    )
}

/// The hook's script, or a note and a clean exit when the repo has none.
pub fn hook_command(hook: Hook, env: &[(&str, &str)]) -> String {
    format!(
        "[ -e {p} ] || {{ echo {}; exit 0; }}; {}",
        sh_quote(&none(hook)),
        command(hook.path(), env),
        p = sh_quote(hook.path())
    )
}

pub use flotilla_proto::HookRun;

/// Hooks Flotilla was still waiting on when it quit (started before `started`, this run's
/// start): no longer shown as running, since nothing will look at them again.
pub fn interrupt_stale_hooks(store: &Store, started: &str) -> Result<()> {
    for prefix in ["resume:", "archive:"] {
        for (key, json) in store.kv_with_prefix(prefix)? {
            let Ok(mut run) = serde_json::from_str::<HookRun>(&json) else {
                continue;
            };
            if run.state == "running" && run.started_at.as_str() < started {
                run.state = "failed".into();
                run.output.push_str("\n(Flotilla quit while it ran)");
                store.kv_set(&key, &serde_json::to_string(&run)?)?;
            }
        }
    }
    Ok(())
}

pub fn hook_run(store: &Store, hook: Hook, thread_id: &str) -> Result<Option<HookRun>> {
    Ok(store
        .kv_get(&key(hook, thread_id))?
        .and_then(|j| serde_json::from_str(&j).ok()))
}

fn save(store: &Store, hook: Hook, thread_id: &str, run: Option<&HookRun>) -> Result<()> {
    store.kv_set(
        &key(hook, thread_id),
        &run.and_then(|r| serde_json::to_string(r).ok())
            .unwrap_or_default(),
    )
}

/// Fold one look at the command into `run`. False when the repo turned out to have no such
/// script: there's nothing to show.
fn absorb(hook: Hook, run: &mut HookRun, st: &CommandStatus) -> bool {
    run.output.push_str(&st.stdout);
    run.output.push_str(&st.stderr);
    if run.output.len() > OUTPUT_CHARS {
        let cut = (run.output.len() - OUTPUT_CHARS..run.output.len())
            .find(|i| run.output.is_char_boundary(*i))
            .unwrap_or(0);
        run.output = run.output[cut..].to_string();
    }
    run.out_at = st
        .stdout_next_offset
        .unwrap_or(run.out_at + st.stdout.len() as u64);
    run.err_at = st
        .stderr_next_offset
        .unwrap_or(run.err_at + st.stderr.len() as u64);
    if !st.running {
        if st.exit_code == Some(0) && run.output.trim() == none(hook) {
            return false;
        }
        run.exit_code = st.exit_code;
        run.state = if st.exit_code == Some(0) {
            "done"
        } else {
            "failed"
        }
        .into();
    }
    true
}

/// Run a hook (if the repo has it) in a thread's VM and wait for it: `.agents/resume` after a
/// wake, before the agent gets its prompt; `.agents/archive` before the VM stops. One still
/// running when its time is up is stopped. `notify` is called once the running row is saved,
/// so the header can show it during the wait.
pub fn run_hook(
    boat: &BoatClient,
    store: &Store,
    hook: Hook,
    t: &Thread,
    dir: &str,
    notify: &dyn Fn(),
) -> Result<()> {
    let thread_id = t.id.as_str();
    let sandbox = t
        .sandbox_id
        .as_deref()
        .ok_or_else(|| Error::Invalid(format!("thread {thread_id} has no VM")))?;
    let env = thread_env(thread_id, t.branch.as_deref());
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let pid = match boat.exec_detached(
        sandbox,
        &format!("bash -lc {}", sh_quote(&hook_command(hook, &env))),
        Some(dir),
    ) {
        Ok(pid) => pid,
        Err(e) => {
            save(store, hook, thread_id, None)?;
            return Err(e.into());
        }
    };
    let mut run = HookRun {
        sandbox: sandbox.to_string(),
        pid,
        started_at: now_iso(),
        state: "running".into(),
        ..Default::default()
    };
    save(store, hook, thread_id, Some(&run))?;
    notify();
    let started = Instant::now();
    while run.state == "running" && started.elapsed() < hook.timeout() {
        sleep(Duration::from_millis(500));
        // A failed look is tried again until the time is up.
        let Ok(st) = boat.command_status(sandbox, pid, run.out_at, run.err_at) else {
            continue;
        };
        if !absorb(hook, &mut run, &st) {
            return save(store, hook, thread_id, None);
        }
    }
    if run.state == "running" {
        // Out of time: stop it (its output so far stays). A failed stop changes nothing here.
        let _ = boat.stop_command(sandbox, pid);
        time_out(&mut run);
    }
    save(store, hook, thread_id, Some(&run))
}

/// What archiving a thread does about `.agents/archive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivePlan {
    /// The VM is up: run it (the wrapper skips a repo without one).
    Run,
    /// The VM is stopped and the repo has one: wake the VM for it (without `.agents/resume`).
    Wake,
    /// Nothing to run: no VM, or stopped and the repo has none (or that's unknown).
    Skip,
}

/// `up`: the VM can take a command now. `stopped`: it's stopped and could be woken.
/// `has_script`: whether the repo has `.agents/archive`, if known.
pub fn archive_plan(up: bool, stopped: bool, has_script: Option<bool>) -> ArchivePlan {
    match (up, stopped, has_script) {
        (true, _, _) => ArchivePlan::Run,
        (false, true, Some(true)) => ArchivePlan::Wake,
        _ => ArchivePlan::Skip,
    }
}

/// A hook that had its time and was stopped.
fn time_out(run: &mut HookRun) {
    if run.state == "running" {
        run.state = "timed_out".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_use_the_shebang_or_bash() {
        assert_eq!(
            command(SETUP, &[("FLOTILLA_SETUP", "fresh")]),
            "mkdir -p ~/.cache/flotilla/logs && set -o pipefail && { if [ -x '.agents/setup' ]; then FLOTILLA='1' FLOTILLA_SETUP='fresh' ./'.agents/setup'; \
             else FLOTILLA='1' FLOTILLA_SETUP='fresh' bash '.agents/setup'; fi; } 2>&1 | tee ~/.cache/flotilla/logs/setup.log"
        );
        assert!(command(RESUME, &[]).ends_with("| tee ~/.cache/flotilla/logs/resume.log"));
        assert!(hook_command(Hook::Resume, &[]).starts_with(
            "[ -e '.agents/resume' ] || { echo 'flotilla: no .agents/resume'; exit 0; }; mkdir -p"
        ));
        let archive = hook_command(
            Hook::Archive,
            &[("FLOTILLA_THREAD_ID", "t1"), ("FLOTILLA_BRANCH", "add-x")],
        );
        assert!(archive.starts_with(
            "[ -e '.agents/archive' ] || { echo 'flotilla: no .agents/archive'; exit 0; }"
        ));
        assert!(archive.contains(
            "FLOTILLA='1' FLOTILLA_THREAD_ID='t1' FLOTILLA_BRANCH='add-x' ./'.agents/archive'"
        ));
        assert!(archive.ends_with("| tee ~/.cache/flotilla/logs/archive.log"));
    }

    #[test]
    fn hook_runs_round_trip_through_kv() {
        let store = Store::open_in_memory().unwrap();
        assert!(hook_run(&store, Hook::Resume, "t1").unwrap().is_none());
        let run = HookRun {
            sandbox: "bx_1".into(),
            pid: 7,
            started_at: now_iso(),
            state: "running".into(),
            ..Default::default()
        };
        save(&store, Hook::Resume, "t1", Some(&run)).unwrap();
        assert_eq!(hook_run(&store, Hook::Resume, "t1").unwrap(), Some(run));
        // Each hook keeps its own.
        assert!(hook_run(&store, Hook::Archive, "t1").unwrap().is_none());
        save(&store, Hook::Resume, "t1", None).unwrap();
        assert!(hook_run(&store, Hook::Resume, "t1").unwrap().is_none());
    }

    #[test]
    fn thread_hooks_get_the_thread_and_branch() {
        assert_eq!(
            thread_env("t1", Some("add-x")),
            vec![
                ("FLOTILLA_THREAD_ID", "t1".to_string()),
                ("FLOTILLA_BRANCH", "add-x".to_string())
            ]
        );
        assert_eq!(
            thread_env("t1", None),
            vec![("FLOTILLA_THREAD_ID", "t1".to_string())]
        );
    }

    #[test]
    fn archiving_wakes_a_stopped_vm_only_for_a_script() {
        assert_eq!(archive_plan(true, false, None), ArchivePlan::Run);
        assert_eq!(archive_plan(false, true, Some(true)), ArchivePlan::Wake);
        assert_eq!(archive_plan(false, true, Some(false)), ArchivePlan::Skip);
        assert_eq!(archive_plan(false, true, None), ArchivePlan::Skip);
        assert_eq!(archive_plan(false, false, Some(true)), ArchivePlan::Skip);
    }

    fn run(state: &str) -> HookRun {
        HookRun {
            started_at: now_iso(),
            state: state.into(),
            ..Default::default()
        }
    }

    #[test]
    fn only_a_running_script_times_out() {
        let mut r = run("running");
        time_out(&mut r);
        assert_eq!(r.state, "timed_out");
        let mut done = run("done");
        time_out(&mut done);
        assert_eq!(done.state, "done");
    }

    #[test]
    fn looks_fold_into_the_run() {
        let st = |running: bool, code: Option<i64>, out: &str| CommandStatus {
            running,
            exit_code: code,
            stdout: out.into(),
            stderr: String::new(),
            stdout_next_offset: None,
            stderr_next_offset: None,
            oom_killed: false,
            signal: None,
        };
        let mut r = run("running");
        assert!(absorb(Hook::Resume, &mut r, &st(true, None, "starting\n")));
        assert_eq!((r.state.as_str(), r.out_at), ("running", 9));
        assert!(absorb(Hook::Resume, &mut r, &st(false, Some(3), "boom\n")));
        assert_eq!(
            (r.state.as_str(), r.exit_code, r.output.as_str()),
            ("failed", Some(3), "starting\nboom\n")
        );
        let mut none = run("running");
        assert!(!absorb(
            Hook::Resume,
            &mut none,
            &st(false, Some(0), "flotilla: no .agents/resume\n")
        ));
        let mut no_archive = run("running");
        assert!(!absorb(
            Hook::Archive,
            &mut no_archive,
            &st(false, Some(0), "flotilla: no .agents/archive\n")
        ));
    }
}
