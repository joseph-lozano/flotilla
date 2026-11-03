//! The per-thread commands, shared by the sidebar's right-click menu and the thread
//! header's ⋯ menu.

use flotilla_proto::{Activity, ThreadState, ThreadSummary};
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::*;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadCommand {
    MarkUnread,
    MarkRead,
    Archive,
    Unarchive,
    StopVm,
    ResumeVm,
    Rename,
    /// Copy Flotilla's id for the thread.
    CopyThreadId,
    /// Copy the boat sandbox id (`bx_…`).
    CopyVmId,
    /// Deletes the VM and its disk on boat, and the thread from this Mac.
    Delete,
    OpenPr,
    CopyBranch,
    /// Link a pull request by number instead of the one from the thread's branch.
    LinkPr,
    /// From the PR bar: take it out of draft. (Merging is the bar's press-and-hold button.)
    MarkReady,
    /// Open New thread on this thread's branch (it must be on GitHub).
    NewThreadOnBranch,
}

impl ThreadCommand {
    pub fn label(self) -> &'static str {
        match self {
            ThreadCommand::MarkUnread => "Mark as unread",
            ThreadCommand::MarkRead => "Mark as read",
            ThreadCommand::Archive => "Archive",
            ThreadCommand::Unarchive => "Unarchive",
            ThreadCommand::StopVm => "Stop VM",
            ThreadCommand::ResumeVm => "Resume VM",
            ThreadCommand::Rename => "Rename…",
            ThreadCommand::CopyThreadId => "Copy thread ID",
            ThreadCommand::CopyVmId => "Copy VM ID",
            ThreadCommand::Delete => "Delete…",
            ThreadCommand::OpenPr => "Open PR on GitHub",
            ThreadCommand::CopyBranch => "Copy branch name",
            ThreadCommand::LinkPr => "Link a PR…",
            ThreadCommand::MarkReady => "Ready for review",
            ThreadCommand::NewThreadOnBranch => "New thread on this branch",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            ThreadCommand::MarkUnread => IconName::Mail,
            ThreadCommand::MarkRead => IconName::MailOpen,
            ThreadCommand::Archive => IconName::Archive,
            ThreadCommand::Unarchive => IconName::ArchiveRestore,
            ThreadCommand::StopVm => IconName::Square,
            ThreadCommand::ResumeVm => IconName::Play,
            ThreadCommand::Rename => IconName::Pencil,
            ThreadCommand::CopyThreadId | ThreadCommand::CopyVmId => IconName::Copy,
            ThreadCommand::Delete => IconName::Trash,
            ThreadCommand::OpenPr => IconName::ExternalLink,
            ThreadCommand::CopyBranch => IconName::GitBranch,
            ThreadCommand::LinkPr => IconName::GitPullRequestArrow,
            ThreadCommand::MarkReady => IconName::Eye,
            ThreadCommand::NewThreadOnBranch => IconName::GitBranchPlus,
        }
    }
}

/// The commands that apply to a thread right now, in menu order; `None` is a separator.
/// `own_row`: the sidebar lists it (not inside the thread that handed it its task). Unread
/// shows only on a row of its own, and not under Archived: elsewhere it wouldn't show.
pub fn commands(s: &ThreadSummary, own_row: bool) -> Vec<Option<ThreadCommand>> {
    use ThreadCommand::*;
    let state = s.thread.state;
    let expired = state == ThreadState::Expired;
    let mut out = Vec::new();
    if !expired {
        match s.activity {
            Activity::Unread => out.push(Some(MarkRead)),
            Activity::Archived => {}
            _ if own_row => out.push(Some(MarkUnread)),
            _ => {}
        }
        out.push(Some(if s.activity == Activity::Archived {
            Unarchive
        } else {
            Archive
        }));
        match state {
            ThreadState::Idle | ThreadState::Running | ThreadState::Starting => {
                out.push(Some(StopVm))
            }
            ThreadState::Stopped => out.push(Some(ResumeVm)),
            _ => {}
        }
        out.push(Some(Rename));
        out.push(None);
    }
    if s.thread.branch.is_some() && s.thread.repo_project_id.is_some() {
        if s.pr.is_some() {
            out.push(Some(OpenPr));
        }
        out.push(Some(CopyBranch));
        out.push(Some(NewThreadOnBranch));
        if !expired {
            out.push(Some(LinkPr));
        }
        out.push(None);
    }
    out.push(Some(CopyThreadId));
    // Not once the VM is gone.
    if s.thread.sandbox_id.is_some() && !expired {
        out.push(Some(CopyVmId));
    }
    out.push(None);
    out.push(Some(Delete));
    out
}

/// Runs the command a menu item picked.
pub type RunCommand = Rc<dyn Fn(ThreadCommand, &mut Window, &mut App)>;

/// Fill a popup menu with `commands`, calling `run` for the one picked.
pub fn fill(menu: PopupMenu, commands: Vec<Option<ThreadCommand>>, run: RunCommand) -> PopupMenu {
    commands.into_iter().fold(menu, |menu, c| match c {
        None => menu.separator(),
        Some(c) => {
            let run = run.clone();
            menu.item(
                PopupMenuItem::new(c.label())
                    .icon(c.icon())
                    .on_click(move |_, window, cx| run(c, window, cx)),
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{ThreadCommand, commands};
    use flotilla_proto::{Activity, Thread, ThreadState, ThreadSummary};

    fn summary(state: ThreadState, activity: Activity) -> ThreadSummary {
        let thread = Thread {
            id: "t".into(),
            environment_id: None,
            environment_name: None,
            title: "t".into(),
            sandbox_id: Some("bx_1".into()),
            conversation_id: None,
            provider: "claude-code".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state,
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

    #[test]
    fn copy_vm_id_only_while_there_is_a_vm() {
        let has = |s: &ThreadSummary| commands(s, true).contains(&Some(ThreadCommand::CopyVmId));
        assert!(has(&summary(ThreadState::Stopped, Activity::Archived)));
        assert!(has(&summary(ThreadState::Idle, Activity::Idle)));
        assert!(!has(&summary(ThreadState::Expired, Activity::Expired)));
        assert!(!has(&summary(ThreadState::Expired, Activity::Archived)));
    }

    /// Mark as unread only where a row would show it: not on a thread listed inside the one
    /// that handed it its task, nor under Archived. An unread one can always be read.
    #[test]
    fn unread_is_offered_only_where_it_would_show() {
        let offered = |s: &ThreadSummary, own_row| {
            commands(s, own_row).contains(&Some(ThreadCommand::MarkUnread))
        };
        assert!(offered(&summary(ThreadState::Idle, Activity::Idle), true));
        assert!(!offered(&summary(ThreadState::Idle, Activity::Idle), false));
        assert!(!offered(
            &summary(ThreadState::Stopped, Activity::Archived),
            true
        ));
        assert!(
            commands(&summary(ThreadState::Idle, Activity::Unread), false)
                .contains(&Some(ThreadCommand::MarkRead))
        );
    }
}
