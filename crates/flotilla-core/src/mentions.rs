//! What a composer offers after `/`, `$` or `@`: the skills and commands the thread's agent can
//! run, and the files in its repo, read in its VM (`mentions.sh`), and how each goes into a
//! prompt for each harness.

use crate::boat::BoatClient;
use crate::error::{Error, Result};
use crate::store::Store;
use crate::util::ellipsize;
use serde_json::Value;

const SCRIPT: &str = include_str!("mentions.sh");

/// Files listed at most, so a huge repo stays quick to read and search.
pub const MAX_FILES: usize = 20_000;

pub use flotilla_proto::{Entry, Mentions};

pub use flotilla_model::mentions::{file_mention, guard_slash, leads_prompt};

/// Read what the VM offers (it must be up). `repo`: the project's folder under ~, if any.
pub fn read(boat: &BoatClient, sandbox: &str, repo: Option<&str>) -> Result<Mentions> {
    let command = format!(
        "REPO={} MAX_FILES={MAX_FILES} bash -c {}",
        crate::setup::sh_quote(repo.unwrap_or("")),
        crate::setup::sh_quote(SCRIPT)
    );
    let res = boat.exec(sandbox, &command, None, 30)?;
    if !res.ok() {
        return Err(Error::Invalid(format!(
            "couldn't list the thread's skills and files: {}",
            ellipsize(res.stderr.trim(), 200)
        )));
    }
    Ok(parse(&res.stdout))
}

/// `mentions.sh`'s output; lines it can't read are skipped.
pub fn parse(out: &str) -> Mentions {
    let mut m = Mentions::default();
    for line in out.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("kind").and_then(Value::as_str) == Some("files") {
            m.root = v
                .get("repo")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            m.files = v
                .get("files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
        } else if let Ok(e) = serde_json::from_value::<Entry>(v)
            && !e.name.is_empty()
        {
            m.entries.push(e);
        }
    }
    m
}

fn key(thread_id: &str) -> String {
    format!("mentions:{thread_id}")
}

fn project_key(project_id: &str) -> String {
    format!("mentions:project:{project_id}")
}

/// What was last read for a thread.
pub fn stored(store: &Store, thread_id: &str) -> Option<Mentions> {
    store
        .kv_get(&key(thread_id))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
}

/// What was last read in any thread of a project, for New thread's composer.
pub fn stored_for_project(store: &Store, project_id: &str) -> Option<Mentions> {
    store
        .kv_get(&project_key(project_id))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
}

/// Keep what was read for a thread (and its project); whether it changed.
pub fn store(
    store: &Store,
    thread_id: &str,
    project_id: Option<&str>,
    mentions: &Mentions,
) -> Result<bool> {
    let json = serde_json::to_string(mentions)?;
    if let Some(p) = project_id {
        store.kv_set(&project_key(p), &json)?;
    }
    if store.kv_get(&key(thread_id))?.as_deref() == Some(json.as_str()) {
        return Ok(false);
    }
    store.kv_set(&key(thread_id), &json)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_script() {
        let out = r#"{"kind":"skill","name":"boat","description":"Use boat","harness":"claude-code","dir":"/home/user/.claude/skills","path":"/home/user/.claude/skills/boat/SKILL.md","insert":"/boat","shadowed":false}
{"kind":"skill","name":"sandbox","description":"Use boat","harness":"codex","dir":"/home/user/.codex/skills","path":"/x","insert":"$sandbox","shadowed":false}
not json
{"kind":"command","name":"","harness":"opencode","insert":"/"}
{"kind":"files","repo":"/home/user/app","truncated":false,"files":["src/main.rs","README.md"]}"#;
        let m = parse(out);
        assert_eq!(
            m.entries
                .iter()
                .map(|e| e.insert.as_str())
                .collect::<Vec<_>>(),
            ["/boat", "$sandbox"]
        );
        assert_eq!(m.root, "/home/user/app");
        assert_eq!(m.files, ["src/main.rs", "README.md"]);
        assert_eq!(
            parse(r#"{"kind":"files","repo":null,"truncated":false,"files":[]}"#).root,
            ""
        );
    }
}
