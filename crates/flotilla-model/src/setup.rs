//! What a project's setup runs show: their stages, in order, and the end of their logs
//! (`flotilla_core::setup` runs them). The UI calls all of this the project's *setup*.

use crate::agents;
use flotilla_proto::{Project, SetupRun, SetupScript};

pub const STEP_VM: &str = "Start a VM";
pub const STEP_FINISH: &str = "Finish";

/// The checkout stage's label, e.g. "Check out main@3f2a1c9".
pub fn checkout_step(branch: &str, sha: &str) -> String {
    format!("Check out {branch}@{}", short(sha))
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// The log line that says a run's commit has `.agents/setup`, written right after the checkout.
pub const FOUND_SETUP: &str = "Found .agents/setup: it runs after the project's scripts.";

/// The stages of a run, in order, for the progress card. `agents_setup`: the commit has
/// `.agents/setup` (known once it's checked out, see [`ran_agents_setup`]).
pub fn stages(
    project: &Project,
    kind: &str,
    sha: &str,
    scripts: &[SetupScript],
    agents_setup: bool,
) -> Vec<String> {
    let mut out = vec![
        STEP_VM.to_string(),
        checkout_step(&project.default_branch, sha),
    ];
    out.extend(
        scripts
            .iter()
            .filter(|s| kind == "fresh" || !s.fresh_only)
            .map(|s| s.command.clone()),
    );
    if agents_setup {
        out.push(agents::SETUP.to_string());
    }
    out.push(STEP_FINISH.to_string());
    out
}

/// The run's commit has `.agents/setup`: the run found it after checking out, or ran it.
pub fn ran_agents_setup(run: &SetupRun) -> bool {
    run.log.contains(FOUND_SETUP) || run.log.contains(&format!("\n$ {}\n", agents::SETUP))
}

/// The last N non-empty lines of a log, for the progress card.
pub fn tail(log: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = log
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_put_the_repos_setup_first() {
        let p = Project {
            id: "p".into(),
            owner: "o".into(),
            repo: "r".into(),
            default_branch: "main".into(),
            environment_name: "flotilla-r".into(),
            created_at: String::new(),
            template_sandbox_id: None,
            template_sha: None,
            template_built_at: None,
            prev_template_sandbox_id: None,
            head_sha: None,
            head_etag: None,
            checked_at: None,
            machine_type: None,
        };
        let scripts = [
            SetupScript::every("npm ci"),
            SetupScript {
                command: "npm run seed".into(),
                fresh_only: true,
            },
        ];
        assert_eq!(
            stages(&p, "update", "3f2a1c9d", &scripts, true),
            [
                STEP_VM,
                "Check out main@3f2a1c9",
                "npm ci",
                ".agents/setup",
                STEP_FINISH
            ]
        );
        assert_eq!(
            stages(&p, "fresh", "3f2a1c9d", &scripts, false),
            [
                STEP_VM,
                "Check out main@3f2a1c9",
                "npm ci",
                "npm run seed",
                STEP_FINISH
            ]
        );
        // A run as `flotilla_core::setup::new_run` starts it.
        let mut run = SetupRun {
            id: "r1".into(),
            project_id: "p".into(),
            kind: "update".into(),
            trigger: "push".into(),
            sha: "3f2a1c9d".into(),
            sandbox_id: None,
            state: "running".into(),
            step: None,
            error: None,
            log: String::new(),
            started_at: String::new(),
            finished_at: None,
        };
        run.log = "\n$ Check out main@3f2a1c9\nHEAD is now at 3f2a1c9\n".into();
        assert!(!ran_agents_setup(&run));
        run.log.push_str(&format!("{FOUND_SETUP}\n"));
        assert!(ran_agents_setup(&run));
        run.log = "\n$ .agents/setup\nok\n".into();
        assert!(ran_agents_setup(&run));
    }

    #[test]
    fn tails() {
        assert_eq!(tail("a\n\nb\nc\n  \nd\n", 2), vec!["c", "d"]);
        assert!(tail("", 4).is_empty());
    }
}
