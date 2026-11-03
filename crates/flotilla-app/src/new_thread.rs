//! The new-thread page: a composer with the thread's settings under it, like a chat app's
//! empty state. Enter starts the thread and opens it; ⌘Enter (Ctrl+Enter) starts it and stays
//! here for the next. With several models ⇧-clicked, one thread starts on each.

use crate::branch_picker::{self, BranchList};
use crate::mention_menu::Offer;
use crate::picker::ThreadSettingsPicker;
use flotilla_proto::{
    BranchChoices, Catalog, Environment, Mentions, NewThread, OnBranch, Project, ThreadSettings,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonRounded, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

const WIDTH: f32 = 720.;

pub enum NewThreadEvent {
    /// One thread per model chosen (the first is opened unless `stay`).
    Submit { threads: Vec<NewThread>, stay: bool },
    /// "Add a project…" in the project menu.
    AddProject,
    /// The branch picker wants this project's pull requests and branches.
    WantBranches(String),
    /// The prompt's `/` `$` `@` menu opened: it wants the skills and files last read in this
    /// project's threads.
    WantMentions(String),
    /// Send later: the prompt to a new thread each run, on each model chosen (`to`: where, in
    /// words).
    Later {
        prompt: String,
        targets: Vec<flotilla_proto::Fresh>,
        labels: Vec<String>,
        project: Option<String>,
    },
}

pub struct NewThreadPage {
    picker: Entity<ThreadSettingsPicker>,
    environments: Vec<Environment>,
    /// The default environment, for No project.
    environment: Option<String>,
    projects: Vec<Project>,
    /// The chosen project; `None`: No project.
    project: Option<String>,
    /// The project's branch picker, its lists (for the project they came for) and the
    /// branches other threads are on.
    branch: Entity<SelectState<BranchList>>,
    lists: Option<(String, Result<BranchChoices, String>)>,
    in_use: Vec<String>,
    /// An existing branch to start on; `None`: a new branch.
    on_branch: Option<OnBranch>,
    /// The machine size picked for the next thread (boat's name); `None`: the project's
    /// (with No project, boat's default).
    machine_type: Option<String>,
    /// The sizes the plan won't start, and why (from boat's limits).
    sizes_unavailable: Vec<(String, String)>,
    prompt: Entity<TextareaState>,
    /// Files to go with the first prompt.
    attachments: crate::attachments::Pending,
    /// The `/` `$` `@` menu, and what it offers: the skills and files last read in a thread
    /// of the chosen project (by project id), ready to search for the chosen harness.
    menu: crate::mention_menu::Menu,
    mentions: Option<(String, Option<Arc<Mentions>>)>,
    offer: Option<Rc<Offer>>,
    /// The prompt's caret and length when the menu last looked.
    caret: (usize, usize),
    starting: bool,
    error: Option<String>,
    /// How many more VMs boat will start right now (its per-minute, hour and day limits and
    /// the VMs it lets run at once), when known.
    room: Option<i64>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NewThreadEvent> for NewThreadPage {}

impl NewThreadPage {
    pub fn new(
        catalog: Catalog,
        defaults: Option<ThreadSettings>,
        environments: Vec<Environment>,
        projects: Vec<Project>,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| ThreadSettingsPicker::new(catalog, defaults, true, window, cx));
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 16)
                .placeholder("What should the agent do?")
                .submit_on_enter(true)
        });
        // The arrows and clicks move the caret without an edit: the menu follows it too.
        let caret = cx.observe(&prompt, |this, _, cx| this.caret_moved(cx));
        let sub = cx.subscribe_in(&prompt, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter {
                shift: false,
                secondary,
            } => this.start(*secondary, window, cx),
            // The send button enables once there's text.
            InputEvent::Change => this.follow_prompt(cx),
            InputEvent::Blur => {
                this.menu.close();
                cx.notify();
            }
            _ => {}
        });
        let environment = environments
            .iter()
            .find(|p| p.is_default)
            .or(environments.first())
            .map(|p| p.name.clone());
        let project = project.filter(|id| projects.iter().any(|p| &p.id == id));
        let branch = cx.new(|cx| {
            SelectState::new(BranchList::new(Vec::new()), None, window, cx).searchable(true)
        });
        let branch_sub = cx.subscribe_in(
            &branch,
            window,
            |this, _, event: &SelectEvent<BranchList>, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.on_branch = this
                        .chosen()
                        .and_then(|p| branch_picker::on_branch(p, this.lists_for(&p.id), value));
                    cx.notify();
                }
            },
        );
        let mut page = NewThreadPage {
            picker,
            environments,
            environment,
            projects,
            project,
            branch,
            lists: None,
            in_use: Vec::new(),
            on_branch: None,
            machine_type: None,
            sizes_unavailable: Vec::new(),
            prompt,
            attachments: Default::default(),
            menu: Default::default(),
            mentions: None,
            offer: None,
            caret: (0, 0),
            starting: false,
            error: None,
            room: None,
            _subscriptions: vec![sub, branch_sub, caret],
        };
        page.refresh_branches(window, cx);
        page
    }

    /// The chosen project's id, so its branches can be fetched.
    pub fn project_id(&self) -> Option<String> {
        self.project.clone()
    }

    fn lists_for(&self, project: &str) -> Option<&Result<BranchChoices, String>> {
        self.lists
            .as_ref()
            .filter(|(id, _)| id == project)
            .map(|(_, l)| l)
    }

    /// A project's pull requests and branches arrived (with the branches other threads are on).
    pub fn set_branches(
        &mut self,
        project: &str,
        lists: Result<BranchChoices, String>,
        in_use: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.lists = Some((project.to_string(), lists));
        self.in_use = in_use;
        self.refresh_branches(window, cx);
    }

    /// Rebuild the branch picker's rows for the chosen project, keeping the choice.
    fn refresh_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.chosen().cloned() else {
            return;
        };
        let rows = branch_picker::choices(
            &p,
            self.lists_for(&p.id),
            &self.in_use,
            self.on_branch.as_ref(),
        );
        let value = self
            .on_branch
            .as_ref()
            .map(|b| b.name.clone())
            .unwrap_or_default();
        self.branch.update(cx, |b, cx| {
            b.set_items(rows, window, cx);
            b.set_selected_value(&value, window, cx);
        });
        cx.notify();
    }

    /// Switch project: back to a new branch, and fetch the new project's lists.
    fn set_project(
        &mut self,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if project != self.project {
            self.project = project;
            self.on_branch = None;
            self.machine_type = None;
            if let Some(id) = &self.project {
                cx.emit(NewThreadEvent::WantBranches(id.clone()));
            }
        }
        self.refresh_branches(window, cx);
    }

    /// A project just added from here.
    pub fn pick_project(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_project(Some(project), window, cx);
    }

    /// Start on `branch` of `project` (a thread's, from its menu), with `prompt` filled in if
    /// there's one (a fix for a thread whose VM is gone).
    pub fn start_on_branch(
        &mut self,
        project: &str,
        branch: OnBranch,
        prompt: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.projects.iter().any(|p| p.id == project) {
            return;
        }
        self.set_project(Some(project.to_string()), window, cx);
        self.on_branch = Some(branch);
        self.refresh_branches(window, cx);
        self.error = None;
        self.prompt.update(cx, |p, cx| {
            if let Some(prompt) = prompt {
                p.set_value(prompt.to_string(), window, cx);
            }
            p.focus(window, cx);
        });
        cx.notify();
    }

    /// Files from the paperclip or dropped on the composer, for the first prompt.
    pub fn add_files(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        if !self.starting {
            self.attachments.add(paths);
            cx.notify();
        }
    }

    /// What's written and attached, and not sent yet (None while it's being started: then it's
    /// been sent).
    pub fn draft(&self, cx: &App) -> Option<crate::drafts::Draft> {
        if self.starting {
            return None;
        }
        crate::drafts::Draft::of(
            self.prompt.read(cx).value().to_string(),
            self.attachments.files.clone(),
        )
    }

    /// Put back what was written here before a switch to another server.
    pub fn put_draft(
        &mut self,
        draft: crate::drafts::Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let last = draft.text.split('\n').count().saturating_sub(1) as u32;
        self.prompt.update(cx, |p, cx| {
            p.set_value(draft.text, window, cx);
            p.set_cursor_position(
                gpui_kit::base::input::Position::new(last, u32::MAX),
                window,
                cx,
            );
        });
        self.attachments.files = draft.files;
        self.follow_prompt(cx);
    }

    pub fn focus_prompt(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt.update(cx, |p, cx| p.focus(window, cx));
    }

    /// Start from a prompt Flotilla wrote (a fix for a failed setup), in `project`, ready to
    /// read over and send.
    pub fn prefill(
        &mut self,
        project: &str,
        prompt: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.iter().any(|p| p.id == project) {
            self.set_project(Some(project.to_string()), window, cx);
        }
        // The fix goes on a new branch from the default branch.
        self.on_branch = None;
        self.refresh_branches(window, cx);
        self.error = None;
        self.prompt.update(cx, |p, cx| {
            p.set_value(prompt.to_string(), window, cx);
            p.focus(window, cx);
        });
        cx.notify();
    }

    /// Dev: pick the project whose repo contains `name`, as the menu would.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_project(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if name == "none" {
            return self.set_project(None, window, cx);
        }
        if let Some(id) = self
            .projects
            .iter()
            .find(|p| p.repo.contains(name))
            .map(|p| p.id.clone())
        {
            self.set_project(Some(id), window, cx);
        }
    }

    /// Dev: choose the agent, model and effort, as the picker would.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_agent(
        &mut self,
        s: flotilla_proto::ThreadSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker
            .update(cx, |p, cx| p.snapshot_choose(s, window, cx));
    }

    /// Dev: ⇧-click another model in the picker.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_also(
        &mut self,
        provider: &str,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker
            .update(cx, |p, cx| p.snapshot_also(provider, model, window, cx));
    }

    /// Dev: pick a branch (`#N`: that pull request's; empty: a new one), as the picker would.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_branch(
        &mut self,
        pick: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.chosen().cloned() else {
            return;
        };
        let lists = self.lists_for(&p.id);
        let value = match pick.strip_prefix('#').and_then(|n| n.parse::<i64>().ok()) {
            Some(n) => lists
                .and_then(|l| l.as_ref().ok())
                .and_then(|l| l.prs.iter().find(|pr| pr.number == n))
                .map(|pr| pr.head.clone())
                .unwrap_or_default(),
            None => pick.to_string(),
        };
        self.on_branch = branch_picker::on_branch(&p, lists, &value);
        self.refresh_branches(window, cx);
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn type_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let last = text.split('\n').count().saturating_sub(1) as u32;
        self.prompt.update(cx, |p, cx| {
            p.set_value(text.to_string(), window, cx);
            p.set_cursor_position(
                gpui_kit::base::input::Position::new(last, u32::MAX),
                window,
                cx,
            );
        });
        self.follow_prompt(cx);
    }

    /// Environments arrive with the first sync, which can finish after the page opens.
    pub fn set_environments(&mut self, environments: Vec<Environment>, cx: &mut Context<Self>) {
        if environments
            .iter()
            .map(|p| &p.name)
            .eq(self.environments.iter().map(|p| &p.name))
        {
            return;
        }
        if !self
            .environment
            .as_ref()
            .is_some_and(|cur| environments.iter().any(|p| &p.name == cur))
        {
            self.environment = environments
                .iter()
                .find(|p| p.is_default)
                .or(environments.first())
                .map(|p| p.name.clone());
        }
        self.environments = environments;
        cx.notify();
    }

    /// Projects change (added, removed, set up) while the page is open.
    pub fn set_projects(&mut self, projects: Vec<Project>, cx: &mut Context<Self>) {
        if projects == self.projects {
            return;
        }
        if self
            .project
            .as_ref()
            .is_some_and(|id| !projects.iter().any(|p| &p.id == id))
        {
            self.project = None;
        }
        self.projects = projects;
        cx.notify();
    }

    /// What the chosen project's threads last read in their VMs, for the menu.
    pub fn set_mentions(
        &mut self,
        project: String,
        mentions: Option<Arc<Mentions>>,
        cx: &mut Context<Self>,
    ) {
        self.mentions = Some((project, mentions));
        self.offer = None;
        if self.menu.is_open() {
            self.follow_prompt(cx);
        }
    }

    /// The menu's offer for the chosen project and harness. `None` with No project, or
    /// before any of the project's threads was read.
    fn offer(&mut self, cx: &App) -> Option<Rc<Offer>> {
        let project = self.project.as_ref()?;
        let provider = self.picker.read(cx).settings(cx)?.provider;
        let Some((id, Some(m))) = &self.mentions else {
            return None;
        };
        if id != project {
            return None;
        }
        if self.offer.as_ref().is_none_or(|o| o.provider() != provider) {
            self.offer = Some(Rc::new(Offer::new(&provider, m)));
        }
        self.offer.clone()
    }

    /// The prompt redrew: if its caret moved, the menu follows.
    fn caret_moved(&mut self, cx: &mut Context<Self>) {
        let (cursor, len) = {
            let p = self.prompt.read(cx);
            (p.cursor(), p.text().len())
        };
        if self.caret != (cursor, len) {
            self.caret = (cursor, len);
            self.follow_prompt(cx);
        }
    }

    /// The prompt's text or caret changed: open, narrow or close the menu.
    fn follow_prompt(&mut self, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let p = self.prompt.read(cx);
            (p.value().to_string(), p.cursor())
        };
        let offer = self.offer(cx);
        if self.menu.update(&text, cursor, offer.as_deref())
            && let Some(project) = self.project.clone()
        {
            cx.emit(NewThreadEvent::WantMentions(project));
        }
        cx.notify();
    }

    /// Whether the menu has rows for the word at the caret now: the caret may have moved
    /// without an edit (the arrows, a click), so look again first.
    fn menu_ready(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.menu.is_open() {
            return false;
        }
        self.follow_prompt(cx);
        self.menu.has_items()
    }

    /// Put the menu's row `i` (or the highlighted one) in place of the word being typed.
    fn accept_mention(&mut self, i: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(choice) = self.menu.choice(i) else {
            return;
        };
        self.menu.close();
        self.prompt.update(cx, |p, cx| {
            choice.apply(p, window, cx);
            p.focus(window, cx);
        });
        // A folder stays open: list what's in it.
        if choice.folder {
            self.follow_prompt(cx);
        }
        cx.notify();
    }

    /// The size the next thread starts on: the one picked, else the project's.
    fn machine_type(&self) -> Option<&str> {
        self.machine_type
            .as_deref()
            .or_else(|| self.chosen()?.machine_type.as_deref())
    }

    fn chosen(&self) -> Option<&Project> {
        self.project
            .as_ref()
            .and_then(|id| self.projects.iter().find(|p| &p.id == id))
    }

    /// The thread exists and has opened (it shows its own start): the page is free for the
    /// next one at once, keeping the project and model, rather than "Starting…" until this
    /// one's VM is up.
    pub fn handed_off(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.starting = false;
        self.error = None;
        self.on_branch = None;
        self.machine_type = None;
        self.refresh_branches(window, cx);
        self.prompt
            .update(cx, |p, cx| p.set_value(String::new(), window, cx));
        self.attachments.clear();
        self.menu.close();
        cx.notify();
    }

    /// boat refused or failed: show why and let the user try again.
    pub fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.starting = false;
        self.error = Some(error);
        cx.notify();
    }

    /// The sizes boat's limits say the plan won't start changed.
    pub fn set_sizes_unavailable(&mut self, sizes: Vec<(String, String)>, cx: &mut Context<Self>) {
        if self.sizes_unavailable != sizes {
            self.sizes_unavailable = sizes;
            cx.notify();
        }
    }

    /// Why the next thread's size won't start on this plan, in words, if it won't.
    fn size_refused(&self) -> Option<String> {
        let t = self.machine_type();
        crate::machine_size::unavailable(&self.sizes_unavailable, t).map(|why| {
            format!(
                "{} {why}: pick another size.",
                flotilla_proto::machine_type_label(t)
            )
        })
    }

    /// boat's room for more VMs changed.
    pub fn set_room(&mut self, room: Option<i64>, cx: &mut Context<Self>) {
        if self.room != room {
            self.room = room;
            cx.notify();
        }
    }

    /// A start that went in the background was refused before its thread was written: its
    /// prompt and files come back here, unless something new is being written.
    pub fn put_back(
        &mut self,
        prompt: &str,
        files: &[flotilla_proto::Attached],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.starting
            && self.prompt.read(cx).value().trim().is_empty()
            && self.attachments.files.is_empty()
        {
            self.put_draft(
                crate::drafts::Draft {
                    text: prompt.to_string(),
                    files: files.to_vec(),
                },
                window,
                cx,
            );
        }
    }

    /// Enter or the Start button: start, and open the (first) thread.
    pub(crate) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start(false, window, cx);
    }

    /// Send later: what Start would start, for a schedule's form.
    pub(crate) fn later(&mut self, cx: &mut Context<Self>) {
        if !self.attachments.files.is_empty() {
            self.error = Some("Send later takes no files.".into());
            cx.notify();
            return;
        }
        let project = self.chosen().cloned();
        let Some(environment) = project
            .as_ref()
            .map(|p| p.environment_name.clone())
            .or(self.environment.clone())
        else {
            self.error = Some("No boat environment. Make one in boat's dashboard.".into());
            cx.notify();
            return;
        };
        let models = self.picker.read(cx).chosen(cx);
        if models.is_empty() {
            self.error = Some("Pick a model.".into());
            cx.notify();
            return;
        }
        if let Some(why) = self.size_refused() {
            self.error = Some(why);
            cx.notify();
            return;
        }
        let on_branch = project.as_ref().and(self.on_branch.clone());
        let (targets, labels) = models
            .into_iter()
            .map(|(s, label)| {
                let fresh = flotilla_proto::Fresh {
                    environment: environment.clone(),
                    project_id: project.as_ref().map(|p| p.id.clone()),
                    provider: s.provider,
                    model: s.model,
                    effort: s.effort,
                    fast: s.fast,
                    machine_type: self.machine_type.clone(),
                    on_branch: on_branch.clone(),
                };
                (fresh, label)
            })
            .unzip();
        self.error = None;
        let prompt = self.prompt.read(cx).value().trim().to_string();
        cx.emit(NewThreadEvent::Later {
            prompt,
            targets,
            labels,
            project: project.map(|p| p.repo),
        });
    }

    /// Sent later: the prompt goes, as after ⌘Enter.
    pub fn scheduled(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt
            .update(cx, |p, cx| p.set_value(String::new(), window, cx));
        self.focus_prompt(window, cx);
        cx.notify();
    }

    /// Start a thread on each model chosen. `stay` (⌘Enter): they start in the background and
    /// the page is ready for the next prompt at once, keeping its project, branch and models.
    pub(crate) fn start(&mut self, stay: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting {
            return;
        }
        let prompt = self.prompt.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let project = self.chosen().cloned();
        let Some(environment) = project
            .as_ref()
            .map(|p| p.environment_name.clone())
            .or(self.environment.clone())
        else {
            self.error = Some("No boat environment. Make one in boat's dashboard.".into());
            cx.notify();
            return;
        };
        let models = self.picker.read(cx).chosen(cx);
        if models.is_empty() {
            self.error = Some("Pick a model.".into());
            cx.notify();
            return;
        }
        let on_branch = project.as_ref().and(self.on_branch.clone());
        if let Some(why) = self
            .size_refused()
            .or_else(|| cant_start(models.len(), on_branch.as_ref(), self.room))
        {
            self.error = Some(why);
            cx.notify();
            return;
        }
        // Several from one prompt: each branch carries its model's name, so they differ.
        let several = models.len() > 1;
        let threads = models
            .into_iter()
            .map(|(s, label)| NewThread {
                environment: environment.clone(),
                project_id: project.as_ref().map(|p| p.id.clone()),
                provider: s.provider,
                model: s.model,
                effort: s.effort,
                fast: s.fast,
                title: None,
                machine_type: self.machine_type.clone(),
                prompt: prompt.clone(),
                attachments: self.attachments.files.clone(),
                wait_ready: false,
                on_branch: on_branch.clone(),
                id: None,
                parent: None,
                branch_tag: (several && project.is_some() && on_branch.is_none()).then_some(label),
            })
            .collect();
        self.error = None;
        if stay {
            // Ready for the next one now: the prompt, files and size picked go; the rest
            // stays.
            self.prompt
                .update(cx, |p, cx| p.set_value(String::new(), window, cx));
            self.attachments.clear();
            self.machine_type = None;
            self.menu.close();
            self.focus_prompt(window, cx);
        } else {
            self.starting = true;
        }
        cx.emit(NewThreadEvent::Submit { threads, stay });
        cx.notify();
    }
}

/// Why `n` threads can't start from New thread now, if they can't: several on one existing
/// branch would share it, and boat starts only so many VMs (`room`, when known).
pub fn cant_start(n: usize, on_branch: Option<&OnBranch>, room: Option<i64>) -> Option<String> {
    if n > 1 && on_branch.is_some() {
        return Some(
            "Several models each need a new branch: pick New branch, or one model.".into(),
        );
    }
    match room {
        Some(r) if (n as i64) > r && r <= 0 => {
            Some("boat won't start another VM right now. Try again in a minute.".into())
        }
        Some(r) if (n as i64) > r => Some(format!(
            "boat will start {r} more VM{} right now: pick {r} model{} or fewer.",
            if r == 1 { "" } else { "s" },
            if r == 1 { "" } else { "s" }
        )),
        _ => None,
    }
}

/// How many more VMs boat will start now: the least room left in the VMs it lets run at once
/// and, unless its extra starts carry on past them, in its starts per minute, hour and day.
/// None when it hasn't said.
pub fn start_room(u: &flotilla_proto::Usage) -> Option<i64> {
    let past = u
        .extra_starts
        .as_ref()
        .is_some_and(|e| e.on && !e.cap_reached);
    let windows = if past {
        [None; 3]
    } else {
        [u.minute, u.hour, u.day]
    };
    windows
        .into_iter()
        .chain([u.vms])
        .flatten()
        .map(|(used, limit)| (limit - used).max(0))
        .min()
}

impl Render for NewThreadPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let has_text = !self.prompt.read(cx).value().trim().is_empty();
        let files = crate::attachments::pending_row(
            &self.attachments,
            cx,
            |p: &mut NewThreadPage, i, _| p.attachments.remove(i),
        );
        // With No project there's nothing to offer; a project's list comes from its threads.
        let menu = self.project.is_some().then(|| {
            let waiting = match &self.mentions {
                Some((id, Some(_))) if Some(id) == self.project.as_ref() => "",
                _ => "Shows after the project's first thread",
            };
            let weak = cx.weak_entity();
            self.menu.render(waiting, &theme, move |i, window, cx| {
                let _ = weak.update(cx, |p, cx| p.accept_mention(Some(i), window, cx));
            })
        });
        let menu = menu.flatten();
        let weak = cx.weak_entity();
        let current = self.project.clone();
        let projects = self.projects.clone();
        let project_menu = Button::new("project")
            .ghost()
            .xsmall()
            .icon(IconName::Box)
            .label(
                self.chosen()
                    .map(|p| p.repo.clone())
                    .unwrap_or_else(|| "No project".into()),
            )
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let pick = |id: Option<String>| {
                    let weak = weak.clone();
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        let id = id.clone();
                        let _ = weak.update(cx, |page, cx| page.set_project(id, window, cx));
                    }
                };
                let menu = projects.iter().fold(menu, |menu, p| {
                    menu.item(
                        PopupMenuItem::new(p.repo.clone())
                            .checked(current.as_deref() == Some(p.id.as_str()))
                            .on_click(pick(Some(p.id.clone()))),
                    )
                });
                let add = weak.clone();
                menu.separator()
                    .item(
                        PopupMenuItem::new("No project")
                            .checked(current.is_none())
                            .on_click(pick(None)),
                    )
                    .item(
                        PopupMenuItem::new("Add a project…").on_click(move |_, _, cx| {
                            let _ = add.update(cx, |_, cx| cx.emit(NewThreadEvent::AddProject));
                        }),
                    )
            });
        let size_menu = {
            let weak = cx.weak_entity();
            crate::machine_size::menu(
                "machine-size",
                self.machine_type(),
                &self.sizes_unavailable,
                self.starting,
                move |t, _, cx| {
                    let _ = weak.update(cx, |page, cx| {
                        page.machine_type = Some(t);
                        cx.notify();
                    });
                },
            )
        };
        // A set width: the select's trigger fills whatever it's given.
        let branch_menu = self.chosen().map(|_| {
            div().flex_none().w(px(240.)).child(
                Select::new(&self.branch)
                    .appearance(false)
                    .xsmall()
                    .menu_width(px(420.))
                    .search_placeholder("Search, #12 or a PR link")
                    .empty(|_, cx| {
                        div()
                            .p_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("No matches")
                    }),
            )
        });

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .px_6()
            .pb(px(80.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(WIDTH))
                    .gap_3()
                    .child(
                        v_flex()
                            .items_center()
                            .gap_4()
                            .child(crate::pixel::mark(4.))
                            .child(div().text_center().text_color(theme.muted_foreground).child(if self.starting { "Starting…" } else { "New thread" })),
                    )
                    .child(
                        v_flex()
                            .child(
                                v_flex()
                                    .relative()
                                    .rounded(px(16.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.background)
                                    .shadow_md()
                                    .when(self.starting, |el| el.opacity(0.7))
                                    .when(!self.starting, |el| {
                                        el.drag_over::<ExternalPaths>(|s, _, _, cx| s.border_color(cx.theme().ring))
                                            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.add_files(paths.paths(), cx)))
                                    })
                                    .children(menu.map(|m| div().absolute().left_0().right_0().bottom_full().mb_1().child(m)))
                                    .children(files.map(|f| div().px_3().pt_3().child(f)))
                                    .child(
                                        div()
                                            .px_3()
                                            .pt_3()
                                            // While the menu has rows, the arrows move through them, Enter and
                                            // Tab pick one and Escape closes it.
                                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::MoveUp, _, cx| {
                                                if this.menu_ready(cx) {
                                                    this.menu.step(-1);
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::MoveDown, _, cx| {
                                                if this.menu_ready(cx) {
                                                    this.menu.step(1);
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .capture_action(cx.listener(|this, e: &gpui_kit::component::input::Enter, window, cx| {
                                                if !e.shift && !e.secondary && this.menu_ready(cx) {
                                                    this.accept_mention(None, window, cx);
                                                    cx.stop_propagation();
                                                } else if !e.shift && !e.secondary && this.menu.is_open() {
                                                    // Nothing to pick: Enter closes the menu rather than send the sign.
                                                    this.menu.dismiss();
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            // Whole characters (an emoji family, a flag, an accented letter), not code points.
                                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::MoveLeft, window, cx| {
                                                if this.prompt.update(cx, |c, cx| crate::graphemes::handle(c, crate::graphemes::Key::Left, window, cx)) {
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::MoveRight, window, cx| {
                                                if this.prompt.update(cx, |c, cx| crate::graphemes::handle(c, crate::graphemes::Key::Right, window, cx)) {
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::Backspace, window, cx| {
                                                if this.prompt.update(cx, |c, cx| crate::graphemes::handle(c, crate::graphemes::Key::Backspace, window, cx)) {
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::Delete, window, cx| {
                                                if this.prompt.update(cx, |c, cx| crate::graphemes::handle(c, crate::graphemes::Key::Delete, window, cx)) {
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::IndentInline, window, cx| {
                                                if this.menu_ready(cx) {
                                                    this.accept_mention(None, window, cx);
                                                    cx.stop_propagation();
                                                } else if this.menu.is_open() {
                                                    this.menu.dismiss();
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::Escape, _, cx| {
                                                if this.menu.is_open() {
                                                    this.menu.dismiss();
                                                    cx.notify();
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .child(Textarea::new(&self.prompt).appearance(false).disabled(self.starting)),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .px_2()
                                            .pt_1()
                                            .pb_2()
                                            .child(
                                                crate::attachments::button(
                                                    "attach",
                                                    cx.listener(|_, _, window, cx| crate::attachments::pick(window, cx, |p: &mut NewThreadPage, paths, cx| p.add_files(&paths, cx))),
                                                )
                                                .disabled(self.starting),
                                            )
                                            .child(self.picker.clone())
                                            .child(div().flex_1())
                                            .child(
                                                Button::new("start-later")
                                                    .ghost()
                                                    .icon(IconName::Clock)
                                                    .rounded(ButtonRounded::Size(px(16.)))
                                                    .size(px(32.))
                                                    .disabled(self.starting)
                                                    .tooltip("Send later")
                                                    .on_click(cx.listener(|this, _, _, cx| this.later(cx))),
                                            )
                                            .child(
                                                Button::new("start")
                                                    .primary()
                                                    .icon(IconName::ArrowUp)
                                                    .rounded(ButtonRounded::Size(px(16.)))
                                                    .size(px(32.))
                                                    .loading(self.starting)
                                                    .disabled(!has_text || self.starting)
                                                    .tooltip(if cfg!(target_os = "macos") { "Start (Enter) · Start and stay here (⌘Enter)" } else { "Start (Enter) · Start and stay here (Ctrl+Enter)" })
                                                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                                            ),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .mx_4()
                                    .px_2()
                                    .py_1()
                                    .gap_2()
                                    .rounded_b(px(12.))
                                    .border_1()
                                    .border_t_0()
                                    .border_color(theme.border)
                                    .bg(theme.secondary)
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(project_menu)
                                    .children(branch_menu)
                                    .child(div().flex_1())
                                    .child(size_menu),
                            ),
                    )
                    .children(self.error.clone().map(|e| div().text_sm().text_color(theme.danger).child(e))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{OnBranch, cant_start, start_room};

    #[test]
    fn several_models_start_only_on_new_branches_and_within_boats_limits() {
        let branch = OnBranch {
            name: "fix-login".into(),
            base: "main".into(),
        };
        assert_eq!(cant_start(1, Some(&branch), None), None);
        assert!(cant_start(2, Some(&branch), None).is_some());
        assert_eq!(cant_start(3, None, None), None);
        assert_eq!(cant_start(3, None, Some(3)), None);
        assert_eq!(
            cant_start(3, None, Some(2)).as_deref(),
            Some("boat will start 2 more VMs right now: pick 2 models or fewer.")
        );
        assert_eq!(
            cant_start(2, None, Some(1)).as_deref(),
            Some("boat will start 1 more VM right now: pick 1 model or fewer.")
        );
        assert!(cant_start(1, None, Some(0)).is_some());
    }

    #[test]
    fn the_room_to_start_is_the_tightest_limit() {
        let u = flotilla_proto::Usage {
            vms: Some((3, 100)),
            minute: Some((10, 12)),
            hour: Some((20, 60)),
            day: Some((120, 200)),
            ..Default::default()
        };
        assert_eq!(start_room(&u), Some(2));
        let u = flotilla_proto::Usage {
            vms: Some((100, 100)),
            minute: Some((0, 12)),
            ..Default::default()
        };
        assert_eq!(start_room(&u), Some(0));
        // With extra starts on, boat starts past the windows (billed); only running VMs count.
        let extra = |cap_reached| {
            Some(flotilla_proto::ExtraStarts {
                on: true,
                used: 3,
                spend_cents: None,
                cap_reached,
            })
        };
        let u = flotilla_proto::Usage {
            vms: Some((3, 100)),
            minute: Some((12, 12)),
            extra_starts: extra(false),
            ..Default::default()
        };
        assert_eq!(start_room(&u), Some(97));
        let u = flotilla_proto::Usage {
            vms: Some((3, 100)),
            minute: Some((12, 12)),
            extra_starts: extra(true),
            ..Default::default()
        };
        assert_eq!(start_room(&u), Some(0));
        assert_eq!(start_room(&flotilla_proto::Usage::default()), None);
    }
}
