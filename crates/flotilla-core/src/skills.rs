//! What you give every thread's agent from Settings: instructions, and skills (folders with a
//! SKILL.md). A client packs a skill's folder ([`pack`]) and uploads it; the engine keeps the
//! packed copy (in `blobs`), which `provision` copies into each VM. Kept in `kv`.

use crate::error::{Error, Result};
use crate::store::Store;

const INSTRUCTIONS_KEY: &str = "user_instructions";
const SKILLS_KEY: &str = "user_skills";
const ERROR_KEY: &str = "user_skills_error";

/// Your instructions for every agent.
pub fn instructions(store: &Store) -> String {
    store
        .kv_get(INSTRUCTIONS_KEY)
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub fn set_instructions(store: &Store, text: &str) -> Result<()> {
    store.kv_set(INSTRUCTIONS_KEY, text.trim())
}

pub use flotilla_proto::UserSkill;

/// The skills every agent gets, in the order they were added.
pub fn chosen(store: &Store) -> Vec<UserSkill> {
    store
        .kv_get(SKILLS_KEY)
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

fn set_chosen(store: &Store, skills: &[UserSkill]) -> Result<()> {
    store.kv_set(SKILLS_KEY, &serde_json::to_string(skills)?)
}

/// Whether `name` can be a skill's folder in a VM: a plain name, no path.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Add `skill`, in place of one of its name (Update), or at the end.
pub fn put(store: &Store, skill: UserSkill) -> Result<()> {
    if !valid_name(&skill.name) {
        return Err(Error::Invalid(format!(
            "{:?} can't be a skill's name: use letters, digits, - and _",
            skill.name
        )));
    }
    let mut skills = chosen(store);
    match skills.iter_mut().find(|s| s.name == skill.name) {
        Some(s) => *s = skill,
        None => skills.push(skill),
    }
    set_chosen(store, &skills)
}

/// Stop giving agents the skill `name`. False if there was none.
pub fn remove(store: &Store, name: &str) -> Result<bool> {
    let mut skills = chosen(store);
    let before = skills.len();
    skills.retain(|s| s.name != name);
    set_chosen(store, &skills)?;
    Ok(skills.len() != before)
}

/// Why the skills last couldn't go to a VM, if they couldn't.
pub fn error(store: &Store) -> Option<String> {
    store
        .kv_get(ERROR_KEY)
        .ok()
        .flatten()
        .filter(|e| !e.is_empty())
}

pub fn set_error(store: &Store, error: Option<&str>) -> Result<()> {
    store.kv_set(ERROR_KEY, error.unwrap_or(""))
}

pub use flotilla_proto::Skill;

pub use flotilla_model::skills::{MAX_PACKED, on_this_mac, pack, read};

/// A skill as a test needs it.
#[cfg(test)]
pub(crate) fn tests_skill(name: &str, blob: &str) -> UserSkill {
    UserSkill {
        name: name.into(),
        description: format!("my {name}"),
        blob: blob.into(),
        size: 10,
        folder: std::path::PathBuf::from("/tmp").join(name),
        from: None,
        from_name: "This Mac".into(),
        added_ms: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_replaces_by_name_and_checks_it() {
        let store = Store::open_in_memory().unwrap();
        put(&store, tests_skill("review", "b_1")).unwrap();
        put(&store, tests_skill("deploy", "b_2")).unwrap();
        put(&store, tests_skill("review", "b_3")).unwrap();
        assert_eq!(
            chosen(&store)
                .iter()
                .map(|s| (s.name.as_str(), s.blob.as_str()))
                .collect::<Vec<_>>(),
            [("review", "b_3"), ("deploy", "b_2")]
        );
        for bad in ["", "..", ".hidden", "a/b", "a b", "../x"] {
            assert!(put(&store, tests_skill(bad, "b_4")).is_err(), "{bad}");
        }
        assert!(remove(&store, "review").unwrap());
        assert!(!remove(&store, "review").unwrap());
        assert_eq!(chosen(&store).len(), 1);
    }
}
