//! A thread's life as a client shows it: its title before any summary, the days left on a
//! stopped one's disk, the branch prefix, and the ids of messages boat hasn't got
//! (`flotilla_core::lifecycle` runs threads).

use crate::util::{one_line, parse_iso, truncate_chars};
use flotilla_proto::{Thread, ThreadState};

/// The choices Settings offers for the idle stop, in minutes.
pub const IDLE_MINUTE_CHOICES: [u32; 3] = [5, 10, 30];
/// boat deletes a stopped sandbox's disk this long after the stop.
pub const DISK_RETENTION_DAYS: i64 = 14;
const TITLE_CHARS: usize = 60;

/// A new thread's title before any summary: the start of its first prompt.
pub fn default_title(prompt: &str) -> String {
    let line = one_line(prompt);
    let cut = truncate_chars(&line, TITLE_CHARS);
    if cut.len() == line.len() {
        return cut;
    }
    // At the last word that fits, unless that loses most of it.
    let at_word = cut
        .rfind(' ')
        .filter(|&i| i >= cut.len() / 2)
        .map_or(cut.as_str(), |i| &cut[..i]);
    format!("{}…", at_word.trim_end_matches([' ', ',', ':', ';', '.']))
}

/// Whole days until boat deletes a stopped thread's disk.
pub fn days_left(thread: &Thread) -> Option<i64> {
    if thread.state != ThreadState::Stopped {
        return None;
    }
    let stopped = parse_iso(thread.last_stopped_at.as_deref()?)?;
    Some(DISK_RETENTION_DAYS - (chrono::Utc::now() - stopped).num_days())
}

/// A branch prefix as typed, made safe for git: characters git refuses in a branch name
/// become `-`, empty parts and leading dots go, and a non-empty prefix ends in `/`
/// ("flotilla" → `flotilla/`).
pub fn clean_branch_prefix(raw: &str) -> String {
    let safe: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let parts: Vec<String> = safe
        .split('/')
        .map(|part| {
            let mut part = part.trim_start_matches('.').to_string();
            while part.contains("..") {
                part = part.replace("..", ".");
            }
            part.strip_suffix(".lock").unwrap_or(&part).to_string()
        })
        .filter(|part| !part.is_empty())
        .collect();
    let joined = parts.join("/");
    let joined = joined.trim_start_matches('-');
    if joined.is_empty() {
        String::new()
    } else {
        format!("{joined}/")
    }
}

/// The id a message gets when Send is pressed, until boat's id replaces it.
pub const LOCAL_PREFIX: &str = "local-";
/// A message a quit cut short that boat never got (see `Core::reconcile_after_quit`).
pub const UNSENT_PREFIX: &str = "unsent-";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_prefixes_are_cleaned() {
        assert_eq!(clean_branch_prefix("flotilla"), "flotilla/");
        assert_eq!(clean_branch_prefix(" flotilla/ "), "flotilla/");
        assert_eq!(clean_branch_prefix("feat//x"), "feat/x/");
        assert_eq!(clean_branch_prefix("Jane Doe?"), "Jane-Doe-/");
        assert_eq!(clean_branch_prefix(".hidden/a..b/c.lock/"), "hidden/a.b/c/");
        assert_eq!(clean_branch_prefix("-~^:"), "");
        assert_eq!(clean_branch_prefix(""), "");
    }
}
