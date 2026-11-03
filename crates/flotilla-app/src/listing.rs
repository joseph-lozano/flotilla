//! Which threads the sidebar lists, under which group and in which order: one pass over
//! plain data. The sidebar draws it; the keyboard walks it (⌘1–⌘9, ⌘⇧[ / ⌘⇧], ⌘⇧S); the
//! app reads it for the rows that moved, the row an open thread is listed under, and which
//! group to unfold to reveal it.

use crate::app::NO_PROJECT;
use flotilla_proto::{Activity, ThreadSummary};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The groups, top to bottom.
pub const GROUPS: [Activity; 4] = [
    Activity::Unread,
    Activity::Idle,
    Activity::Working,
    Activity::Archived,
];

/// What the listing is made from (the app's own fields).
pub struct Inputs<'a> {
    /// Every thread's row, newest activity first.
    pub summaries: &'a [ThreadSummary],
    /// Threads handed their task by another: child id → parent id.
    pub parents: &'a HashMap<String, String>,
    /// Threads waiting on an answer from a thread they handed work to.
    pub waiting: &'a HashSet<String>,
    /// Rows held in the group they were in (see `FlotillaApp::held`).
    pub held: &'a HashMap<String, Activity>,
    /// The sidebar's filter: projects (and `NO_PROJECT`) whose threads are hidden.
    pub hidden_projects: &'a BTreeSet<String>,
}

/// A row the sidebar lists.
pub struct Listed<'a> {
    pub summary: &'a ThreadSummary,
    /// It was handed its task by another thread (the row says so).
    pub handed: bool,
    /// That thread, when the sidebar has it. A thread handed its task has no row of its own
    /// while that one is there unless it's archived, so only an archived row has one.
    pub parent: Option<&'a ThreadSummary>,
    /// Another thread with a row of its own (under any group, whatever the filter) has its
    /// title: started from one prompt on several models, before their own titles are written.
    pub twin: bool,
}

pub struct Listing<'a> {
    by_id: HashMap<&'a str, &'a ThreadSummary>,
    parents: &'a HashMap<String, String>,
    /// Every thread's group, listed or not.
    groups: HashMap<&'a str, Activity>,
    /// The listed rows, group by group in `GROUPS`' order.
    rows: [Vec<Listed<'a>>; 4],
}

impl<'a> Listing<'a> {
    pub fn build(i: Inputs<'a>) -> Listing<'a> {
        let mut by_id: HashMap<&str, &ThreadSummary> = HashMap::with_capacity(i.summaries.len());
        for s in i.summaries {
            by_id.entry(s.thread.id.as_str()).or_insert(s);
        }
        let inside = |s: &ThreadSummary| inside_parent(s, i.parents, &by_id);
        let mut titles: HashMap<&str, usize> = HashMap::new();
        for s in i.summaries.iter().filter(|s| !inside(s)) {
            *titles.entry(s.thread.title.as_str()).or_default() += 1;
        }
        let groups: HashMap<&str, Activity> = i
            .summaries
            .iter()
            .map(|s| (s.thread.id.as_str(), shown_group(s, i.held, i.waiting)))
            .collect();
        let mut rows: [Vec<Listed>; 4] = Default::default();
        for s in i.summaries {
            if !in_filter(s, i.hidden_projects) || inside(s) {
                continue;
            }
            let Some(at) = GROUPS
                .iter()
                .position(|g| *g == shown_group(s, i.held, i.waiting))
            else {
                continue;
            };
            let parent = i.parents.get(&s.thread.id);
            rows[at].push(Listed {
                summary: s,
                handed: parent.is_some(),
                parent: parent.and_then(|p| by_id.get(p.as_str()).copied()),
                twin: titles.get(s.thread.title.as_str()).is_some_and(|n| *n > 1),
            });
        }
        // Working ones by when they started, so they hold still while their events come in;
        // the rest by activity (as they came).
        rows[2].sort_by_key(|l| {
            std::cmp::Reverse(
                l.summary
                    .turn_started_ms
                    .unwrap_or(l.summary.last_activity_ms),
            )
        });
        Listing {
            by_id,
            parents: i.parents,
            groups,
            rows,
        }
    }

    /// A group's rows, in the order they're drawn.
    pub fn group(&self, group: Activity) -> &[Listed<'a>] {
        GROUPS
            .iter()
            .position(|g| *g == group)
            .map_or(&[], |at| self.rows[at].as_slice())
    }

    /// The rows shown, in order, with the group each shows under, leaving out collapsed groups.
    pub fn visible(&self, collapsed: &HashSet<Activity>) -> Vec<(String, Activity)> {
        GROUPS
            .into_iter()
            .filter(|g| !collapsed.contains(g))
            .flat_map(|g| {
                self.group(g)
                    .iter()
                    .map(move |l| (l.summary.thread.id.clone(), g))
            })
            .collect()
    }

    /// The group a thread shows in (listed or not: a thread listed inside another, or hidden
    /// by the filter, still has one).
    pub fn shown_group(&self, id: &str) -> Option<Activity> {
        self.groups.get(id).copied()
    }

    /// Every thread's group, to spot the rows that moved.
    pub fn groups(&self) -> HashMap<String, Activity> {
        self.groups
            .iter()
            .map(|(id, g)| (id.to_string(), *g))
            .collect()
    }

    pub fn summary(&self, id: &str) -> Option<&'a ThreadSummary> {
        self.by_id.get(id).copied()
    }

    /// The row that stands for thread `id`: its own, or for a thread listed inside the one
    /// that handed it its task, that one's (and so on up).
    pub fn row_for(&self, id: &str) -> String {
        let mut at = id;
        for _ in 0..8 {
            let Some(s) = self.by_id.get(at) else { break };
            match self.parents.get(at) {
                Some(parent) if inside_parent(s, self.parents, &self.by_id) => at = parent,
                _ => break,
            }
        }
        at.to_string()
    }
}

/// A thread another handed work to lives in that one's row, as a subagent does: it has no
/// row of its own while that one is there, unless it's archived (then it's under Archived).
fn inside_parent(
    s: &ThreadSummary,
    parents: &HashMap<String, String>,
    present: &HashMap<&str, &ThreadSummary>,
) -> bool {
    !matches!(s.activity, Activity::Archived | Activity::Expired)
        && parents
            .get(&s.thread.id)
            .is_some_and(|p| present.contains_key(p.as_str()))
}

/// Whether a thread shows under the sidebar's project filter.
fn in_filter(s: &ThreadSummary, hidden: &BTreeSet<String>) -> bool {
    !hidden.contains(s.thread.repo_project_id.as_deref().unwrap_or(NO_PROJECT))
}

/// The group a thread shows in: its activity's (expired threads sit with the archived ones),
/// except for rows held where they were so no row moves out from under you: opened unread,
/// it stays under Unread; its turn just ended, under Working. Waiting on a thread it handed
/// work to, it's still at work, beside it.
fn shown_group(
    s: &ThreadSummary,
    held: &HashMap<String, Activity>,
    waiting: &HashSet<String>,
) -> Activity {
    match (held.get(&s.thread.id), s.activity) {
        (Some(Activity::Unread), Activity::Idle) => Activity::Unread,
        (Some(Activity::Working), Activity::Idle | Activity::Unread) => Activity::Working,
        (_, Activity::Idle) if waiting.contains(&s.thread.id) => Activity::Working,
        (_, Activity::Expired) => Activity::Archived,
        (_, a) => a,
    }
}

#[cfg(test)]
mod tests {
    use super::{GROUPS, Inputs, Listing};
    use Activity::*;
    use flotilla_proto::{Activity, Thread, ThreadState, ThreadSummary};
    use std::collections::{BTreeSet, HashMap, HashSet};

    fn row(id: &str, activity: Activity) -> ThreadSummary {
        let thread = Thread {
            id: id.into(),
            environment_id: None,
            environment_name: None,
            title: format!("Title {id}"),
            sandbox_id: Some("bx_1".into()),
            conversation_id: None,
            provider: "claude-code".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state: ThreadState::Idle,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: String::new(),
            updated_at: String::new(),
            repo_project_id: None,
            branch: None,
            base_sha: None,
        };
        ThreadSummary {
            thread,
            activity,
            last_activity_ms: 0,
            turn_started_ms: None,
            last_reply: None,
            last_prompt: None,
            pr: None,
            start_failed: false,
            prompted_ms: None,
            opens: None,
        }
    }

    /// The app's fields, to build a listing from.
    #[derive(Default)]
    struct World {
        summaries: Vec<ThreadSummary>,
        parents: HashMap<String, String>,
        waiting: HashSet<String>,
        held: HashMap<String, Activity>,
        hidden: BTreeSet<String>,
    }

    impl World {
        fn of(rows: &[(&str, Activity)]) -> World {
            World {
                summaries: rows.iter().map(|(id, a)| row(id, *a)).collect(),
                ..Default::default()
            }
        }

        fn handed(mut self, child: &str, parent: &str) -> World {
            self.parents.insert(child.into(), parent.into());
            self
        }

        fn listing(&self) -> Listing<'_> {
            Listing::build(Inputs {
                summaries: &self.summaries,
                parents: &self.parents,
                waiting: &self.waiting,
                held: &self.held,
                hidden_projects: &self.hidden,
            })
        }

        /// Each group's ids, top to bottom.
        fn shown(&self) -> Vec<(Activity, Vec<String>)> {
            let l = self.listing();
            GROUPS
                .into_iter()
                .map(|g| {
                    (
                        g,
                        l.group(g)
                            .iter()
                            .map(|r| r.summary.thread.id.clone())
                            .collect::<Vec<_>>(),
                    )
                })
                .filter(|(_, ids)| !ids.is_empty())
                .collect()
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn each_activity_has_its_group_and_expired_threads_sit_with_the_archived() {
        let w = World::of(&[
            ("u", Unread),
            ("i", Idle),
            ("w", Working),
            ("a", Archived),
            ("e", Expired),
        ]);
        assert_eq!(
            w.shown(),
            vec![
                (Unread, ids(&["u"])),
                (Idle, ids(&["i"])),
                (Working, ids(&["w"])),
                (Archived, ids(&["a", "e"]))
            ]
        );
        assert_eq!(w.listing().shown_group("e"), Some(Archived));
    }

    #[test]
    fn a_handed_thread_is_listed_inside_its_parent_only_while_its_parent_is_there() {
        // The parent there: the child has no row, and stands in its parent's.
        let w = World::of(&[("p", Idle), ("c", Working)]).handed("c", "p");
        assert_eq!(w.shown(), vec![(Idle, ids(&["p"]))]);
        assert_eq!(w.listing().row_for("c"), "p");
        // Its group is still known (for the rows that moved), though it has no row.
        assert_eq!(w.listing().shown_group("c"), Some(Working));
        // The parent gone (deleted, forgotten): its own row, marked as handed, with no parent.
        let w = World::of(&[("c", Working)]).handed("c", "p");
        assert_eq!(w.shown(), vec![(Working, ids(&["c"]))]);
        let l = w.listing();
        let c = &l.group(Working)[0];
        assert!(c.handed && c.parent.is_none());
        assert_eq!(l.row_for("c"), "c");
    }

    #[test]
    fn an_archived_handed_thread_is_under_archived_with_its_parent_named() {
        for done in [Archived, Expired] {
            let w = World::of(&[("p", Idle), ("c", done)]).handed("c", "p");
            assert_eq!(
                w.shown(),
                vec![(Idle, ids(&["p"])), (Archived, ids(&["c"]))]
            );
            let l = w.listing();
            assert_eq!(
                l.group(Archived)[0].parent.map(|p| p.thread.id.as_str()),
                Some("p")
            );
            assert_eq!(l.row_for("c"), "c");
        }
    }

    #[test]
    fn a_thread_handed_its_task_by_a_handed_thread_stands_in_the_top_row() {
        let w = World::of(&[("top", Idle), ("mid", Working), ("leaf", Working)])
            .handed("mid", "top")
            .handed("leaf", "mid");
        assert_eq!(w.shown(), vec![(Idle, ids(&["top"]))]);
        assert_eq!(w.listing().row_for("leaf"), "top");
        // A thread the sidebar doesn't have: itself.
        assert_eq!(w.listing().row_for("gone"), "gone");
    }

    #[test]
    fn waiting_on_a_hand_off_an_idle_thread_is_still_at_work() {
        let mut w = World::of(&[("p", Idle), ("q", Unread)]);
        w.waiting = ["p".to_string(), "q".to_string()].into();
        // Unread stays unread: it has something to read.
        assert_eq!(
            w.shown(),
            vec![(Unread, ids(&["q"])), (Working, ids(&["p"]))]
        );
    }

    #[test]
    fn held_rows_stay_where_they_were() {
        let mut w = World::of(&[
            ("read", Idle),
            ("done", Idle),
            ("done-unread", Unread),
            ("again", Working),
            ("gone", Archived),
        ]);
        // Opened unread, now read: under Unread. Its turn just ended (read or not): under Working.
        // At work again, or archived: a hold doesn't keep it.
        w.held = [
            ("read", Unread),
            ("done", Working),
            ("done-unread", Working),
            ("again", Unread),
            ("gone", Unread),
        ]
        .into_iter()
        .map(|(id, g)| (id.to_string(), g))
        .collect();
        assert_eq!(
            w.shown(),
            vec![
                (Unread, ids(&["read"])),
                (Working, ids(&["done", "done-unread", "again"])),
                (Archived, ids(&["gone"]))
            ]
        );
    }

    #[test]
    fn working_rows_are_ordered_by_when_their_turn_started_and_the_rest_as_they_came() {
        let mut w = World::of(&[
            ("i1", Idle),
            ("w-old", Working),
            ("i2", Idle),
            ("w-new", Working),
            ("w-none", Working),
        ]);
        for s in &mut w.summaries {
            (s.turn_started_ms, s.last_activity_ms) = match s.thread.id.as_str() {
                "w-old" => (Some(100), 900),
                "w-new" => (Some(300), 300),
                // No turn start (a held row): when it was last active.
                "w-none" => (None, 200),
                "i1" => (None, 10),
                _ => (None, 20),
            };
        }
        assert_eq!(
            w.shown(),
            vec![
                (Idle, ids(&["i1", "i2"])),
                (Working, ids(&["w-new", "w-none", "w-old"]))
            ]
        );
    }

    #[test]
    fn the_filter_hides_projects_and_threads_with_none() {
        let mut w = World::of(&[("a", Idle), ("b", Idle), ("none", Idle)]);
        w.summaries[0].thread.repo_project_id = Some("p_a".into());
        w.summaries[1].thread.repo_project_id = Some("p_b".into());
        w.hidden = ["p_a".to_string()].into();
        assert_eq!(w.shown(), vec![(Idle, ids(&["b", "none"]))]);
        w.hidden = [crate::app::NO_PROJECT.to_string()].into();
        assert_eq!(w.shown(), vec![(Idle, ids(&["a", "b"]))]);
        // Hidden, it still has a group.
        assert_eq!(w.listing().shown_group("none"), Some(Idle));
    }

    #[test]
    fn visible_rows_leave_out_collapsed_groups_in_the_sidebars_order() {
        let w = World::of(&[("a", Archived), ("w", Working), ("i", Idle), ("u", Unread)]);
        let l = w.listing();
        let all = l.visible(&HashSet::new());
        assert_eq!(
            all,
            vec![
                ("u".into(), Unread),
                ("i".into(), Idle),
                ("w".into(), Working),
                ("a".into(), Archived)
            ]
        );
        assert_eq!(
            l.visible(&[Archived, Idle].into()),
            vec![("u".to_string(), Unread), ("w".to_string(), Working)]
        );
    }

    #[test]
    fn rows_from_one_prompt_on_several_models_are_twins() {
        let mut w = World::of(&[
            ("claude", Working),
            ("codex", Idle),
            ("other", Idle),
            ("hidden", Idle),
            ("child", Idle),
            ("p", Idle),
        ])
        .handed("child", "p");
        for s in &mut w.summaries {
            if matches!(s.thread.id.as_str(), "claude" | "codex" | "child") {
                s.thread.title = "Fix the flaky test".into();
            }
            if s.thread.id == "hidden" {
                s.thread.title = "Title other".into();
                s.thread.repo_project_id = Some("p_x".into());
            }
        }
        w.hidden = ["p_x".to_string()].into();
        let l = w.listing();
        let twin = |g: Activity, id: &str| {
            l.group(g)
                .iter()
                .find(|r| r.summary.thread.id == id)
                .map(|r| r.twin)
        };
        // Under different groups, still twins.
        assert_eq!(
            (twin(Working, "claude"), twin(Idle, "codex")),
            (Some(true), Some(true))
        );
        // A thread listed inside another doesn't make one: it has no row to tell apart.
        let mut w2 =
            World::of(&[("solo", Idle), ("child", Idle), ("p", Idle)]).handed("child", "p");
        w2.summaries[1].thread.title = w2.summaries[0].thread.title.clone();
        assert_eq!(
            w2.listing()
                .group(Idle)
                .iter()
                .find(|r| r.summary.thread.id == "solo")
                .map(|r| r.twin),
            Some(false)
        );
        // One the filter hides still counts (its row is there with the filter off).
        assert_eq!(twin(Idle, "other"), Some(true));
    }

    #[test]
    fn every_thread_has_a_group_to_spot_moves_by() {
        let mut w = World::of(&[("p", Idle), ("c", Working), ("x", Unread)]).handed("c", "p");
        w.summaries[2].thread.repo_project_id = Some("p_x".into());
        w.hidden = ["p_x".to_string()].into();
        let groups = w.listing().groups();
        assert_eq!(groups.len(), 3);
        assert_eq!(
            (groups["p"], groups["c"], groups["x"]),
            (Idle, Working, Unread)
        );
    }
}
