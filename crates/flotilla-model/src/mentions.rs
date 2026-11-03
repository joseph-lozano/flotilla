//! How what a composer offers after `/`, `$` or `@` goes into a prompt for each harness
//! (`flotilla_core::mentions` reads what the thread's VM offers).

/// Whether `provider` runs a skill or command only when it starts the prompt (`/name`).
pub fn leads_prompt(provider: &str) -> bool {
    matches!(provider, "claude-code" | "opencode")
}

/// How to mention `path` (relative to the repo at `root`) to `provider`'s agent. Claude Code
/// works in the repo and attaches the file itself (quoted if it has spaces); Codex and OpenCode
/// work in the home folder, so they get the full path.
pub fn file_mention(provider: &str, root: &str, path: &str) -> String {
    if provider == "claude-code" {
        if path.contains(char::is_whitespace) {
            format!("@\"{path}\"")
        } else {
            format!("@{path}")
        }
    } else if root.is_empty() {
        format!("@{path}")
    } else {
        format!("@{}/{path}", root.trim_end_matches('/'))
    }
}

/// OpenCode drops a prompt that starts with `/` and an unknown name (a typo, a pasted path)
/// without a word; a zero-width space in front keeps it a plain prompt. `known`: the names of
/// its commands and skills.
pub fn guard_slash(provider: &str, text: &str, known: &[String]) -> Option<String> {
    if provider != "opencode" {
        return None;
    }
    let first = text
        .strip_prefix('/')?
        .split_whitespace()
        .next()
        .unwrap_or("");
    let builtin = matches!(first, "init" | "compact" | "customize-opencode");
    (!builtin && !known.iter().any(|k| k == first)).then(|| format!("\u{200B}{text}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_proto::Entry;

    fn entry(harness: &str, name: &str, insert: &str) -> Entry {
        Entry {
            kind: "skill".into(),
            name: name.into(),
            description: String::new(),
            harness: harness.into(),
            insert: insert.into(),
            shadowed: false,
        }
    }

    #[test]
    fn each_harness_its_own() {
        let claude = entry("claude-code", "boat", "/boat");
        assert_eq!(claude.insert("claude-code"), Some("/boat"));
        assert_eq!(claude.insert("codex"), None);
        let hidden = Entry {
            shadowed: true,
            ..entry("opencode", "boat", "/boat")
        };
        assert_eq!(hidden.insert("opencode"), None);
        assert!(leads_prompt("opencode") && !leads_prompt("codex"));
    }

    #[test]
    fn files_are_mentioned_the_way_each_harness_finds_them() {
        assert_eq!(
            file_mention("claude-code", "/home/user/app", "src/main.rs"),
            "@src/main.rs"
        );
        assert_eq!(
            file_mention("claude-code", "/home/user/app", "docs/My notes.md"),
            "@\"docs/My notes.md\""
        );
        assert_eq!(
            file_mention("codex", "/home/user/app", "src/main.rs"),
            "@/home/user/app/src/main.rs"
        );
        assert_eq!(file_mention("opencode", "", "notes.md"), "@notes.md");
    }

    #[test]
    fn opencode_keeps_unknown_slashes_as_text() {
        let known = vec!["review".to_string()];
        assert_eq!(guard_slash("opencode", "/review the PR", &known), None);
        assert_eq!(guard_slash("opencode", "/init", &known), None);
        assert_eq!(
            guard_slash("opencode", "/usr/bin/env is missing", &known).as_deref(),
            Some("\u{200B}/usr/bin/env is missing")
        );
        assert_eq!(
            guard_slash("opencode", "/reveiw", &known).as_deref(),
            Some("\u{200B}/reveiw")
        );
        assert_eq!(guard_slash("claude-code", "/usr/bin", &known), None);
        assert_eq!(guard_slash("opencode", "fix /usr/bin", &known), None);
    }
}
