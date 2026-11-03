//! Subagents as the harness itself records them in the VM. boat's events show a Claude Code
//! subagent's calls as the thread's own, with nothing linking them to it; leave Codex's
//! subagents out (their calls show as the thread's, their answers merged into its reply); and
//! show an OpenCode subagent as a `task` call that never ends, without its calls. Each harness
//! keeps the links in the VM, and one command reads them: `lineage.sh`, which prints the
//! subagents of the harness session `session.sh` finds behind a boat conversation. It runs in
//! the same exec as `meter.sh` (`crate::meter::read_with_lineage`).

use crate::boat::BoatClient;
use crate::error::{Error, Result};
use crate::store::Store;
use crate::util::ellipsize;

const SESSION: &str = include_str!("session.sh");
const SCRIPT: &str = include_str!("lineage.sh");

pub use flotilla_proto::{Lineage, Subagent as Child};

pub use flotilla_model::lineage::reads;

/// A boat conversation's subagents, read in its VM (which must be up). None recorded, or a
/// harness that keeps no such records: an empty list.
pub fn read(boat: &BoatClient, sandbox: &str, conversation: &str) -> Result<Lineage> {
    let Some(command) = command(conversation) else {
        return Ok(Lineage::default());
    };
    let res = boat.exec(sandbox, &command, None, 30)?;
    if !res.ok() {
        return Err(Error::Invalid(format!(
            "couldn't read the thread's subagents: {}",
            ellipsize(res.stderr.trim(), 200)
        )));
    }
    parse(&res.stdout)
}

/// The command that prints a boat conversation's subagents, or None for an id that can't be one.
pub fn command(conversation: &str) -> Option<String> {
    if conversation.is_empty()
        || !conversation
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(format!(
        "CONV={conversation} bash -c {}",
        crate::setup::sh_quote(&format!("{SESSION}{SCRIPT}"))
    ))
}

pub fn parse(out: &str) -> Result<Lineage> {
    serde_json::from_str(out.trim())
        .map_err(|e| Error::Invalid(format!("the thread's subagents didn't parse: {e}")))
}

fn key(thread_id: &str) -> String {
    format!("lineage:{thread_id}")
}

/// What was last read for a thread.
pub fn stored(store: &Store, thread_id: &str) -> Option<Lineage> {
    store
        .kv_get(&key(thread_id))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
}

/// Keep what was read; whether it changed.
pub fn store(store: &Store, thread_id: &str, lineage: &Lineage) -> Result<bool> {
    if stored(store, thread_id).as_ref() == Some(lineage) {
        return Ok(false);
    }
    store.kv_set(&key(thread_id), &serde_json::to_string(lineage)?)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_the_script_prints() {
        // As the script printed it on 2026-10-30, trimmed: a Codex turn and a Claude Code one.
        let codex = r#"{"subagents":[{"id":"01a18f1a-0a90","name":"system info","kind":"Hypatia","model":"gpt-5.6-terra","running":false,"background":false,"started_ms":1793402210960,"finished_ms":1793402213000,"calls":["exec-8f9f57f8"],"texts":["`uname -a`: Linux box-node"]}]}"#;
        let l = parse(codex).unwrap();
        assert_eq!(l.subagents[0].parent_call, None);
        assert_eq!(l.subagents[0].calls, ["exec-8f9f57f8"]);
        let claude = r#"{"subagents":[{"id":"ad95f26fb7c2b1dae","parent_call":"toolu_01JG","name":"Check the kernel","kind":"general-purpose","model":"claude-haiku-5-5","running":false,"background":false,"started_ms":1793402223000,"finished_ms":1793402227000,"calls":["toolu_019V","toolu_01Sk"],"texts":["Output of uname"]}]}"#;
        let l = parse(claude).unwrap();
        assert_eq!(l.subagents[0].parent_call.as_deref(), Some("toolu_01JG"));
        assert_eq!(parse(r#"{"subagents":[]}"#).unwrap(), Lineage::default());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn keeps_and_reports_changes() {
        let store = Store::open_in_memory().unwrap();
        let l = Lineage {
            subagents: vec![Child {
                id: "a".into(),
                ..Default::default()
            }],
        };
        assert!(store_fn(&store, &l));
        assert!(!store_fn(&store, &l));
        assert_eq!(stored(&store, "t1"), Some(l));
    }

    fn store_fn(s: &Store, l: &Lineage) -> bool {
        store(s, "t1", l).unwrap()
    }
}
