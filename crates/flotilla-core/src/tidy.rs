//! Archiving on its own (Settings → Archiving): a thread whose pull request merges, and
//! threads left idle for days (not one a schedule sends to). Both go through the menu's Archive (`.agents/archive`, the VM
//! stopped, unread kept), and both can be undone.

use crate::store::{Activity, Store, ThreadState, ThreadSummary};

/// `kv`: archive a thread when its pull request merges ("0": don't). On unless turned off.
pub const ON_MERGE_KEY: &str = "archive_on_merge";
/// `kv`: archive threads idle for this many days ("0" or none: never).
pub const IDLE_DAYS_KEY: &str = "archive_idle_days";
pub use flotilla_model::tidy::IDLE_DAY_CHOICES;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// A merge first seen by a database that never saw the pull request open (a rebuild, say)
/// archives only if it's this recent.
const FRESH_MERGE_MS: i64 = DAY_MS;

pub fn on_merge(store: &Store) -> bool {
    store
        .kv_get(ON_MERGE_KEY)
        .ok()
        .flatten()
        .is_none_or(|v| v != "0")
}

pub fn idle_days(store: &Store) -> u32 {
    store
        .kv_get(IDLE_DAYS_KEY)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// `kv`: when the thread last came back from Archived (Unarchive, Undo, a message sent to it),
/// in ms. Its idle days count from then, and a merge from before then doesn't archive it.
pub fn unarchived_key(thread: &str) -> String {
    format!("unarchived:{thread}")
}

pub fn unarchived_ms(store: &Store, thread: &str) -> Option<i64> {
    store
        .kv_get(&unarchived_key(thread))
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
}

/// A thread as these rules see it.
#[derive(Debug, Clone, Copy)]
pub struct Seen<'a> {
    pub summary: &'a ThreadSummary,
    /// Open on some device's screen right now.
    pub open: bool,
    /// An action on it (a send, a stop…) is under way.
    pub busy: bool,
    /// Another thread handed it its work: it lives inside that one, not in the sidebar.
    pub handed: bool,
    pub unarchived_ms: Option<i64>,
    /// A schedule that isn't paused sends to it: idle between its runs, not done.
    pub scheduled: bool,
}

/// Why a thread isn't archived by itself, whatever the rule: it's archived (or expired)
/// already, at work (a turn running, its VM starting or stopping), kept running, or in the
/// middle of an action.
fn held(t: &Seen) -> bool {
    let s = t.summary;
    matches!(
        s.activity,
        Activity::Archived | Activity::Expired | Activity::Working
    ) || matches!(
        s.thread.state,
        ThreadState::Creating
            | ThreadState::Starting
            | ThreadState::Running
            | ThreadState::Stopping
            | ThreadState::Expired
    ) || s.thread.keep_running
        || t.busy
}

/// When the thread last did anything: its last event (or its start), or its coming back from
/// Archived. Reading it doesn't count.
pub fn last_active_ms(t: &Seen) -> i64 {
    t.summary.last_activity_ms.max(t.unarchived_ms.unwrap_or(0))
}

/// Whether the idle rule archives the thread now: `days` set, nothing holding it, not unread
/// (a reply waits for you), not open anywhere, not inside another thread, no schedule sending
/// to it (archiving would pause that), and nothing done for `days` days.
pub fn idle(t: &Seen, days: u32, now_ms: i64) -> bool {
    days > 0
        && !held(t)
        && t.summary.activity != Activity::Unread
        && !t.open
        && !t.handed
        && !t.scheduled
        && now_ms - last_active_ms(t) >= days as i64 * DAY_MS
}

/// What's known about a merge just seen.
#[derive(Debug, Clone, Copy)]
pub struct Merge {
    /// When GitHub says it merged (ms), if it says.
    pub merged_ms: Option<i64>,
    /// This database had the pull request before, not merged: the merge happened while
    /// Flotilla watched. Otherwise (a rebuild, a PR found already merged) it must be fresh.
    pub seen_open: bool,
    /// The newest message sent to the thread (a prompt or a steer), in ms.
    pub last_prompt_ms: Option<i64>,
}

/// Whether a merge just seen archives the thread: nothing holds it (see `held`; unread and
/// open are fine, since a merge is the work done, often with the thread open for Hold to
/// merge), nothing was asked of it after the merge, it wasn't brought back from Archived
/// after the merge, and the merge is news.
pub fn merge_archives(t: &Seen, m: &Merge, now_ms: i64) -> bool {
    if held(t) {
        return false;
    }
    if let Some(merged) = m.merged_ms
        && (m.last_prompt_ms.is_some_and(|p| p > merged)
            || t.unarchived_ms.is_some_and(|u| u > merged))
    {
        return false;
    }
    if m.seen_open {
        return true;
    }
    match m.merged_ms {
        Some(merged) => now_ms - merged <= FRESH_MERGE_MS,
        // GitHub didn't say when: a thread started in the last day.
        None => crate::util::iso_to_ms(&t.summary.thread.created_at)
            .is_some_and(|c| now_ms - c <= FRESH_MERGE_MS),
    }
}

/// The idle rule over every thread: the ones to archive now, longest idle first, at most
/// `limit` (archiving a stopped VM may wake it for `.agents/archive`, and boat limits starts).
pub fn idle_now(threads: &[Seen], days: u32, now_ms: i64, limit: usize) -> Vec<String> {
    let mut due: Vec<&Seen> = threads.iter().filter(|t| idle(t, days, now_ms)).collect();
    due.sort_by_key(|t| last_active_ms(t));
    due.into_iter()
        .take(limit)
        .map(|t| t.summary.thread.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Thread;

    const NOW: i64 = 1_800_000_000_000;

    fn summary(activity: Activity, state: ThreadState, idle_days: i64) -> ThreadSummary {
        let thread = Thread {
            id: "t1".into(),
            environment_id: None,
            environment_name: None,
            title: "Fix the retry".into(),
            sandbox_id: Some("bx_1".into()),
            conversation_id: Some("c1".into()),
            provider: "claude-code".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: "2026-10-01T00:00:00.000Z".into(),
            updated_at: "2026-10-01T00:00:00.000Z".into(),
            repo_project_id: Some("p1".into()),
            branch: Some("fix-the-retry".into()),
            base_sha: None,
        };
        ThreadSummary {
            thread,
            activity,
            last_activity_ms: NOW - idle_days * DAY_MS,
            turn_started_ms: None,
            last_reply: None,
            last_prompt: None,
            pr: None,
            start_failed: false,
            prompted_ms: None,
            opens: None,
        }
    }

    fn seen(s: &ThreadSummary) -> Seen<'_> {
        Seen {
            summary: s,
            open: false,
            busy: false,
            handed: false,
            unarchived_ms: None,
            scheduled: false,
        }
    }

    #[test]
    fn idle_threads_are_archived_after_the_days_set() {
        let s = summary(Activity::Idle, ThreadState::Stopped, 8);
        assert!(idle(&seen(&s), 7, NOW));
        // Not yet, never, or not this long.
        assert!(!idle(
            &seen(&summary(Activity::Idle, ThreadState::Stopped, 6)),
            7,
            NOW
        ));
        assert!(!idle(&seen(&s), 0, NOW));
        assert!(!idle(&seen(&s), 14, NOW));
        // An idle VM that's still up counts too.
        assert!(idle(
            &seen(&summary(Activity::Idle, ThreadState::Idle, 8)),
            7,
            NOW
        ));
        // A failed start with no VM is just a flag to set.
        assert!(idle(
            &seen(&summary(Activity::Idle, ThreadState::Error, 30)),
            7,
            NOW
        ));
    }

    #[test]
    fn idle_leaves_unread_working_kept_running_open_busy_and_scheduled_threads_alone() {
        // A reply nobody has read.
        assert!(!idle(
            &seen(&summary(Activity::Unread, ThreadState::Stopped, 30)),
            7,
            NOW
        ));
        // A turn running, or a VM coming up or going down.
        for state in [
            ThreadState::Running,
            ThreadState::Starting,
            ThreadState::Creating,
            ThreadState::Stopping,
        ] {
            assert!(
                !idle(&seen(&summary(Activity::Idle, state, 30)), 7, NOW),
                "{state:?}"
            );
        }
        assert!(!idle(
            &seen(&summary(Activity::Working, ThreadState::Idle, 30)),
            7,
            NOW
        ));
        // Kept running.
        let mut kept = summary(Activity::Idle, ThreadState::Idle, 30);
        kept.thread.keep_running = true;
        assert!(!idle(&seen(&kept), 7, NOW));
        // On someone's screen, or with an action under way.
        let s = summary(Activity::Idle, ThreadState::Stopped, 30);
        assert!(!idle(
            &Seen {
                open: true,
                ..seen(&s)
            },
            7,
            NOW
        ));
        assert!(!idle(
            &Seen {
                busy: true,
                ..seen(&s)
            },
            7,
            NOW
        ));
        // Inside the thread that handed it its work.
        assert!(!idle(
            &Seen {
                handed: true,
                ..seen(&s)
            },
            7,
            NOW
        ));
        // A schedule that isn't paused sends to it (a weekly one, between its runs).
        assert!(!idle(
            &Seen {
                scheduled: true,
                ..seen(&s)
            },
            7,
            NOW
        ));
        assert!(idle(&seen(&s), 7, NOW));
        // Archived or expired already.
        assert!(!idle(
            &seen(&summary(Activity::Archived, ThreadState::Stopped, 30)),
            7,
            NOW
        ));
        assert!(!idle(
            &seen(&summary(Activity::Expired, ThreadState::Expired, 30)),
            7,
            NOW
        ));
    }

    #[test]
    fn coming_back_from_archived_starts_the_idle_days_again() {
        let s = summary(Activity::Idle, ThreadState::Stopped, 30);
        // Undone (or unarchived) yesterday: it's had a day, not seven.
        let back = Seen {
            unarchived_ms: Some(NOW - DAY_MS),
            ..seen(&s)
        };
        assert!(!idle(&back, 7, NOW));
        assert!(idle(&back, 7, NOW + 6 * DAY_MS));
    }

    #[test]
    fn the_longest_idle_go_first_a_few_at_a_time() {
        let mut a = summary(Activity::Idle, ThreadState::Stopped, 9);
        a.thread.id = "a".into();
        let mut b = summary(Activity::Idle, ThreadState::Stopped, 20);
        b.thread.id = "b".into();
        let mut c = summary(Activity::Idle, ThreadState::Stopped, 3);
        c.thread.id = "c".into();
        let mut d = summary(Activity::Unread, ThreadState::Stopped, 40);
        d.thread.id = "d".into();
        let all = [seen(&a), seen(&b), seen(&c), seen(&d)];
        assert_eq!(idle_now(&all, 7, NOW, 5), ["b", "a"]);
        assert_eq!(idle_now(&all, 7, NOW, 1), ["b"]);
        assert!(idle_now(&all, 0, NOW, 5).is_empty());
    }

    fn merge(merged_ago_ms: Option<i64>, seen_open: bool, prompt_ago_ms: Option<i64>) -> Merge {
        Merge {
            merged_ms: merged_ago_ms.map(|a| NOW - a),
            seen_open,
            last_prompt_ms: prompt_ago_ms.map(|a| NOW - a),
        }
    }

    #[test]
    fn a_merge_seen_happening_archives_the_thread_even_unread_or_open() {
        let minute = 60_000;
        let s = summary(Activity::Idle, ThreadState::Idle, 0);
        assert!(merge_archives(
            &seen(&s),
            &merge(Some(minute), true, Some(10 * minute)),
            NOW
        ));
        // Hold to merge: the thread is open.
        assert!(merge_archives(
            &Seen {
                open: true,
                ..seen(&s)
            },
            &merge(Some(minute), true, None),
            NOW
        ));
        // Merged on GitHub before reading the last reply: archived, still unread.
        let unread = summary(Activity::Unread, ThreadState::Stopped, 0);
        assert!(merge_archives(
            &seen(&unread),
            &merge(Some(minute), true, None),
            NOW
        ));
    }

    #[test]
    fn a_merge_leaves_working_kept_running_and_busy_threads_alone() {
        let m = merge(Some(60_000), true, None);
        for (activity, state) in [
            (Activity::Working, ThreadState::Running),
            (Activity::Idle, ThreadState::Starting),
            (Activity::Idle, ThreadState::Stopping),
            (Activity::Archived, ThreadState::Stopped),
        ] {
            assert!(
                !merge_archives(&seen(&summary(activity, state, 0)), &m, NOW),
                "{activity:?} {state:?}"
            );
        }
        let mut kept = summary(Activity::Idle, ThreadState::Idle, 0);
        kept.thread.keep_running = true;
        assert!(!merge_archives(&seen(&kept), &m, NOW));
        let s = summary(Activity::Idle, ThreadState::Idle, 0);
        assert!(!merge_archives(
            &Seen {
                busy: true,
                ..seen(&s)
            },
            &m,
            NOW
        ));
    }

    #[test]
    fn work_asked_for_after_the_merge_keeps_the_thread_out() {
        let s = summary(Activity::Idle, ThreadState::Stopped, 0);
        // A message sent after the merge: the thread's in use for something else.
        assert!(!merge_archives(
            &seen(&s),
            &merge(Some(10 * 60_000), true, Some(60_000)),
            NOW
        ));
        // Brought back from Archived after the merge.
        let back = Seen {
            unarchived_ms: Some(NOW - 60_000),
            ..seen(&s)
        };
        assert!(!merge_archives(
            &back,
            &merge(Some(10 * 60_000), true, None),
            NOW
        ));
    }

    #[test]
    fn an_old_merge_seen_for_the_first_time_after_a_rebuild_doesnt_archive() {
        let s = summary(Activity::Idle, ThreadState::Stopped, 40);
        // Never seen open here, merged weeks ago.
        assert!(!merge_archives(
            &seen(&s),
            &merge(Some(20 * DAY_MS), false, None),
            NOW
        ));
        // Never seen open here, but merged in the last hour: news.
        assert!(merge_archives(
            &seen(&s),
            &merge(Some(60 * 60_000), false, None),
            NOW
        ));
        // No merge time from GitHub, and a thread from weeks ago.
        assert!(!merge_archives(&seen(&s), &merge(None, false, None), NOW));
    }
}
