//! Projects: GitHub repos Flotilla starts threads in. Each has its own boat environment that
//! lists the repo, so boat clones it into every VM (the agent starts in it) and passes GitHub
//! access for pushes and pull requests.

use crate::boat::BoatClient;
use crate::error::{Error, Result};
use crate::github::{GitHub, GitHubError, RepoInfo, SetupScript, Suggestions, suggest};
use crate::store::{Project, Store};
use crate::util::now_iso;

/// What to suggest when adding `repo`: setup scripts from its root files, unless it has its
/// own `.agents/setup`.
/// `repo` with its default branch, asked of GitHub if the list it came from didn't say (the
/// repo list leaves it out: asked for a page of repos, it takes GitHub seconds more).
pub fn with_default_branch(github: &GitHub, repo: RepoInfo) -> Result<RepoInfo, GitHubError> {
    if !repo.default_branch.is_empty() {
        return Ok(repo);
    }
    let default_branch = github.repo(&repo.owner, &repo.name)?.default_branch;
    Ok(RepoInfo {
        default_branch,
        ..repo
    })
}

pub fn suggestions(github: &GitHub, repo: &RepoInfo) -> Result<Suggestions, GitHubError> {
    let repo = &with_default_branch(github, repo.clone())?;
    let root = github.root_files(&repo.owner, &repo.name, &repo.default_branch)?;
    let agents = if root.iter().any(|f| f == ".agents") {
        github.dir_files(&repo.owner, &repo.name, ".agents", &repo.default_branch)?
    } else {
        vec![]
    };
    let package_json = if root.iter().any(|f| f == "package.json") {
        github.file_text(
            &repo.owner,
            &repo.name,
            "package.json",
            &repo.default_branch,
        )?
    } else {
        None
    };
    Ok(suggest(&root, &agents, package_json.as_deref()))
}

/// A boat environment name for a repo: `flotilla-<repo>` (`flotilla-dev-<repo>` in a dev
/// build), made unique against `taken`.
fn environment_name(repo: &RepoInfo, taken: &[String], prefix: &str) -> String {
    let clean = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
    };
    // A repo already named flotilla-something doesn't need the prefix twice.
    let name = clean(&repo.name);
    let first = if name.to_lowercase().starts_with(prefix) {
        name.clone()
    } else {
        format!("{prefix}-{name}")
    };
    let candidates = [first, format!("{prefix}-{}-{}", clean(&repo.owner), name)];
    let fits = |n: &String| n.len() <= 64 && !taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    if let Some(n) = candidates.iter().find(|n| fits(n)) {
        return n.clone();
    }
    // A random suffix, not -2, -3…: boat keeps a deleted environment's name taken without
    // listing it, so numbered names run out after a few removals and re-adds.
    let base: String = candidates[0].chars().take(57).collect();
    loop {
        let n = format!("{base}-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        if fits(&n) {
            return n;
        }
    }
}

/// Add `repo` as a project: make its boat environment (listing the repo) and save it with
/// its setup scripts. boat must already see the repo through its GitHub connection.
pub fn add_project(
    boat: &BoatClient,
    store: &Store,
    repo: &RepoInfo,
    scripts: &[SetupScript],
) -> Result<Project> {
    if store.project_by_repo(&repo.owner, &repo.name)?.is_some() {
        return Err(Error::Invalid(format!(
            "{} is already a project",
            repo.full_name()
        )));
    }
    let full = repo.full_name();
    let boat_repo = boat.repos(&repo.name)?.into_iter().find(|r| r.full_name.eq_ignore_ascii_case(&full)).ok_or_else(|| {
        Error::Invalid(format!(
            "boat can't see {full} yet. Give boat access to it in boat's dashboard (Environment → GitHub repositories), then try again."
        ))
    })?;
    let mut taken: Vec<String> = boat.environments()?.into_iter().map(|e| e.name).collect();
    // boat keeps a deleted environment's name taken without listing it (a project removed
    // and added again): try the next name.
    let mut tries = 0;
    let env = loop {
        let name = environment_name(repo, &taken, crate::flavor::environment_prefix());
        tries += 1;
        match boat.create_environment(&name) {
            Err(e) if e.is("create_failed") && e.status() == Some(409) && tries < 5 => {
                taken.push(name)
            }
            r => break r?,
        }
    };
    // New environments start with every flag on (GitHub access included); say so anyway, in
    // case boat's defaults change.
    let setup = boat
        .update_environment(
            &env.id,
            &serde_json::json!({"passGithub": true, "safeForThirdParties": false}),
        )
        .and_then(|_| {
            boat.add_environment_repo(&env.id, &boat_repo.database_id, &repo.default_branch)
        });
    if let Err(e) = setup {
        let _ = boat.delete_environment(&env.id);
        return Err(e.into());
    }
    let project = Project {
        id: uuid::Uuid::new_v4().to_string(),
        owner: repo.owner.clone(),
        repo: repo.name.clone(),
        default_branch: repo.default_branch.clone(),
        environment_name: env.name.clone(),
        created_at: now_iso(),
        template_sandbox_id: None,
        template_sha: None,
        template_built_at: None,
        prev_template_sandbox_id: None,
        head_sha: None,
        head_etag: None,
        checked_at: None,
    };
    if let Err(e) = store
        .upsert_environment(&env)
        .and_then(|_| store.insert_project(&project, scripts))
    {
        let _ = boat.delete_environment(&env.id);
        let _ = store.delete_environment(&env.id);
        return Err(e);
    }
    Ok(project)
}

/// Remove a project: its prepared VMs and its boat environment go too. Threads already
/// started keep their VMs and branches.
pub fn remove_project(boat: &BoatClient, store: &Store, project_id: &str) -> Result<()> {
    if store.project(project_id)?.is_none() {
        return Ok(());
    }
    // First: setup runs stop and can't publish, so what's read below is all there is.
    store.set_removing(project_id, true)?;
    let r = remove_marked(boat, store, project_id);
    if r.is_err() {
        let _ = store.set_removing(project_id, false);
    }
    r
}

fn remove_marked(boat: &BoatClient, store: &Store, project_id: &str) -> Result<()> {
    let Some(p) = store.project(project_id)? else {
        return Ok(());
    };
    // A setup run still going has a VM of its own (it then stops on its own).
    let running: Vec<String> = store
        .setup_runs(project_id, 20)?
        .into_iter()
        .filter(|r| r.state == "running")
        .filter_map(|r| r.sandbox_id)
        .collect();
    // One another boat key made (an imported database's) can't be deleted with this one: it's
    // left, stopped, rather than keeping the project.
    let gone = |e: &crate::BoatError| e.status() == Some(404) || e.is("api_key_action_forbidden");
    let owner = crate::owner::Owner::load(store)?;
    for sb in [&p.template_sandbox_id, &p.prev_template_sandbox_id]
        .into_iter()
        .flatten()
        .chain(running.iter())
    {
        match owner.delete_own(boat, sb) {
            Ok(_) => {}
            Err(e) if gone(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    // And any other VM a setup run left (an old one a publish couldn't delete, a run cut short
    // before its VM was saved): every VM labelled for this project, that's this Flotilla's.
    for sb in boat
        .list_sandboxes(&[(crate::setup::SETUP_LABEL, project_id)])?
        .into_iter()
        .filter(|sb| owner.mine(sb))
    {
        match boat.delete_sandbox(&sb.id) {
            Ok(()) => {}
            Err(e) if gone(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(env) = store.environment(&p.environment_name)? {
        match boat.delete_environment(&env.id) {
            Ok(()) => {}
            Err(e) if e.status() == Some(404) => {}
            Err(e) => return Err(e.into()),
        }
        store.delete_environment(&env.id)?;
    }
    store.delete_project(project_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> RepoInfo {
        RepoInfo {
            owner: owner.into(),
            name: name.into(),
            private: true,
            default_branch: "main".into(),
            pushed_at: None,
            description: None,
        }
    }

    #[test]
    fn environment_names_are_unique_and_valid() {
        assert_eq!(
            environment_name(&repo("acme", "flotilla-playground"), &[], "flotilla"),
            "flotilla-playground"
        );
        assert_eq!(
            environment_name(&repo("acme", "web"), &["flotilla-web".into()], "flotilla"),
            "flotilla-acme-web"
        );
        let third = environment_name(
            &repo("acme", "web"),
            &["flotilla-web".into(), "flotilla-acme-web".into()],
            "flotilla",
        );
        assert!(
            third.starts_with("flotilla-web-") && third.len() == "flotilla-web-".len() + 6,
            "{third}"
        );
        assert_eq!(
            environment_name(&repo("a", "we b+c"), &[], "flotilla"),
            "flotilla-we-b-c"
        );
        // A dev build's: a flotilla-something repo still gets the dev prefix.
        assert_eq!(
            environment_name(&repo("acme", "web"), &[], "flotilla-dev"),
            "flotilla-dev-web"
        );
        assert_eq!(
            environment_name(&repo("acme", "flotilla-playground"), &[], "flotilla-dev"),
            "flotilla-dev-flotilla-playground"
        );
    }
}
