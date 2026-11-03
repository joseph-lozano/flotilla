//! Add a project: pick a GitHub repo gh's account can see, check the setup scripts Flotilla
//! suggests from the repo, then add it.

use flotilla_proto::{GhStatus, RepoInfo, SetupScript, Suggestions};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Sizable, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::HashSet;

pub enum AddProjectEvent {
    /// gh is signed in now: last time's repo list, then a fresh one.
    Load,
    /// Fetch the repo list again.
    Retry,
    /// Look at gh again (signed in yet?).
    CheckGh,
    /// Suggest setup scripts for this repo.
    Suggest(RepoInfo),
    Add {
        repo: RepoInfo,
        scripts: Vec<SetupScript>,
    },
    Cancel,
}

struct ScriptRow {
    input: Entity<InputState>,
    fresh_only: bool,
}

pub struct AddProjectView {
    search: Entity<InputState>,
    /// gh's state: the repos are its account's, so nothing loads until it's signed in.
    gh: GhStatus,
    /// Check again was pressed; gh hasn't answered yet.
    checking: bool,
    /// The pick being added is another account's (gh's changed since): if the add fails,
    /// it goes.
    stale_pick: bool,
    /// `None` while loading.
    repos: Option<Result<Vec<RepoInfo>, String>>,
    /// The list shown is last time's (or the first pages) and a fresh one is on its way.
    refreshing: bool,
    /// The list shown came from the cache: later pages wait for the whole new list, so it
    /// doesn't shrink while you look.
    cached: bool,
    /// Refreshing a cached list failed; the list stays.
    refresh_failed: Option<String>,
    /// Repos that are projects already (`owner/name`, lowercase).
    taken: HashSet<String>,
    selected: Option<RepoInfo>,
    scripts: Vec<ScriptRow>,
    /// The picked repo has its own `.agents/setup`.
    agents_setup: bool,
    suggesting: bool,
    adding: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AddProjectEvent> for AddProjectView {}

impl AddProjectView {
    pub fn new(
        taken: HashSet<String>,
        gh: GhStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search your repos"));
        let sub = cx.subscribe_in(&search, window, |_, _, event, _, cx| {
            if let InputEvent::Change = event {
                cx.notify();
            }
        });
        AddProjectView {
            search,
            gh,
            checking: false,
            stale_pick: false,
            repos: None,
            refreshing: false,
            cached: false,
            refresh_failed: None,
            taken,
            selected: None,
            scripts: Vec::new(),
            agents_setup: false,
            suggesting: false,
            adding: false,
            error: None,
            _subscriptions: vec![sub],
        }
    }

    /// gh's state changed. Another account (or none): nothing of the last one's stays, and
    /// a signed-in one's repos load.
    pub fn set_gh(&mut self, gh: GhStatus, cx: &mut Context<Self>) {
        let other_account = self.gh.login() != gh.login();
        self.gh = gh;
        self.checking = false;
        if other_account {
            (
                self.repos,
                self.cached,
                self.refreshing,
                self.refresh_failed,
            ) = (None, false, false, None);
            if self.adding {
                self.stale_pick = true;
            } else {
                self.clear_pick();
            }
            if self.gh.signed_in() {
                cx.emit(AddProjectEvent::Load);
            }
        }
        cx.notify();
    }

    /// The search box, where typing starts.
    pub fn search(&self) -> Entity<InputState> {
        self.search.clone()
    }

    /// Enter: in the search box, pick the first repo it shows (or, once that's picked, add
    /// it); anywhere else, Add project, if it can.
    pub fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.focus_handle(cx).is_focused(window) {
            let first = self
                .visible(cx)
                .into_iter()
                .find(|r| !self.taken.contains(&r.full_name().to_lowercase()));
            if let Some(repo) =
                first.filter(|r| !self.selected.as_ref().is_some_and(|s| same_repo(s, r)))
            {
                return self.pick(repo, cx);
            }
        }
        if !self.suggesting {
            self.add(cx);
        }
    }

    /// Last time's list, shown while a fresh one loads.
    pub fn set_cached(&mut self, repos: Vec<RepoInfo>, cx: &mut Context<Self>) {
        self.repos = Some(Ok(repos));
        self.cached = true;
        self.refreshing = true;
        cx.notify();
    }

    /// The list so far (`done` false: more pages coming) or all of it, for `login`'s account
    /// (one that isn't gh's now is dropped).
    pub fn set_repos(
        &mut self,
        repos: Result<Vec<RepoInfo>, String>,
        done: bool,
        login: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if login.is_some() && login != self.gh.login() {
            return;
        }
        let showing = matches!(self.repos, Some(Ok(_)));
        match repos {
            // A first load shows pages as they come; a cached list waits for the whole one.
            Ok(_) if !done && self.cached => return,
            Ok(list) => {
                // The pick survives a fresh list: the same repo's new record (its pushed time
                // has moved on), or gone once a whole list doesn't have it.
                match rebind(self.selected.as_ref(), &list, done) {
                    Some(Some(r)) => self.selected = Some(r),
                    Some(None) if !self.adding => {
                        self.selected = None;
                        self.scripts.clear();
                        self.agents_setup = false;
                        self.suggesting = false;
                    }
                    _ => {}
                }
                self.repos = Some(Ok(list));
                self.refreshing = !done;
                self.refresh_failed = None;
            }
            Err(e) if showing => {
                self.refreshing = false;
                self.refresh_failed = Some(e);
            }
            Err(e) => {
                self.repos = Some(Err(e));
                self.refreshing = false;
            }
        }
        cx.notify();
    }

    /// Suggestions for `repo` arrived; ignored if another repo is picked by now.
    pub fn set_suggestions(
        &mut self,
        repo: &str,
        result: Result<Suggestions, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.as_ref().map(RepoInfo::full_name).as_deref() != Some(repo) {
            return;
        }
        self.suggesting = false;
        match result {
            Ok(s) => {
                self.agents_setup = s.agents_setup;
                self.scripts = s
                    .scripts
                    .into_iter()
                    .map(|s| self.row(&s.command, s.fresh_only, window, cx))
                    .collect();
            }
            // Not worth blocking on: the user can type scripts.
            Err(e) => self.error = Some(format!("Couldn't read the repo to suggest scripts: {e}")),
        }
        cx.notify();
    }

    pub fn failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.adding = false;
        if std::mem::take(&mut self.stale_pick) {
            self.clear_pick();
        }
        self.error = Some(error);
        cx.notify();
    }

    /// No repo picked, nor its scripts.
    fn clear_pick(&mut self) {
        self.selected = None;
        self.scripts.clear();
        (self.agents_setup, self.suggesting, self.error) = (false, false, None);
    }

    fn row(
        &self,
        command: &str,
        fresh_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ScriptRow {
        let input = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("e.g. npm ci");
            s.set_value(command.to_string(), window, cx);
            s
        });
        ScriptRow { input, fresh_only }
    }

    /// Dev: pick the repo whose name contains `name`, as a click would.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_pick(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(r) = self
            .visible(cx)
            .into_iter()
            .find(|r| r.full_name().contains(name))
        {
            self.pick(r, cx);
        }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_search(&mut self, q: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |s, cx| s.set_value(q.to_string(), window, cx));
        cx.notify();
    }

    /// Dev: a cached list (`refresh`: the same repos fetched again, pushed later), as if
    /// from GitHub.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_repos(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let pushed = if refresh {
            "2026-10-30T12:00:00Z"
        } else {
            "2026-10-30T10:00:00Z"
        };
        let repo = |name: &str| RepoInfo {
            owner: "acme".into(),
            name: name.into(),
            private: true,
            default_branch: "main".into(),
            pushed_at: Some(pushed.into()),
            description: None,
        };
        let list = vec![
            repo("flotilla-playground"),
            repo("retry-lab"),
            repo("webhooks"),
        ];
        // As if gh were signed in, which it isn't offline.
        self.gh = GhStatus::SignedIn {
            login: "octocat".into(),
            version: "2.60.0".into(),
            scopes: Vec::new(),
        };
        if refresh {
            self.set_repos(Ok(list), true, None, cx)
        } else {
            self.set_cached(list, cx)
        }
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_loaded(&self) -> bool {
        matches!(self.repos, Some(Ok(_)))
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_add(&mut self, cx: &mut Context<Self>) {
        self.add(cx);
    }

    fn pick(&mut self, repo: RepoInfo, cx: &mut Context<Self>) {
        if self.adding || self.selected.as_ref().is_some_and(|s| same_repo(s, &repo)) {
            return;
        }
        self.scripts.clear();
        self.agents_setup = false;
        self.error = None;
        self.suggesting = true;
        cx.emit(AddProjectEvent::Suggest(repo.clone()));
        self.selected = Some(repo);
        cx.notify();
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.selected.clone() else {
            return;
        };
        if self.adding || !self.gh.signed_in() {
            return;
        }
        let scripts = self
            .scripts
            .iter()
            .map(|r| SetupScript {
                command: r.input.read(cx).value().trim().to_string(),
                fresh_only: r.fresh_only,
            })
            .filter(|s| !s.command.is_empty())
            .collect();
        self.adding = true;
        self.error = None;
        cx.emit(AddProjectEvent::Add { repo, scripts });
        cx.notify();
    }

    fn visible(&self, cx: &App) -> Vec<RepoInfo> {
        let q = self.search.read(cx).value().trim().to_lowercase();
        let Some(Ok(repos)) = &self.repos else {
            return vec![];
        };
        repos
            .iter()
            .filter(|r| q.is_empty() || r.full_name().to_lowercase().contains(&q))
            .take(50)
            .cloned()
            .collect()
    }
}

impl Render for AddProjectView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let note = |t: String| div().text_xs().text_color(muted).child(t);
        let now = chrono::Utc::now().timestamp_millis();

        let query = !self.search.read(cx).value().trim().is_empty();
        // Nothing yet, or an empty list that's last time's: the fresh one is on its way.
        let loading = match &self.repos {
            None => true,
            Some(Ok(list)) => list.is_empty() && self.refreshing,
            Some(Err(_)) => false,
        };
        let list: AnyElement = match &self.repos {
            // The repos are gh's account's: it says what to do first.
            _ if !self.gh.signed_in() => match self.gh {
                GhStatus::Unknown => note("Checking the GitHub CLI…".into()).into_any_element(),
                _ => v_flex()
                    .gap_2()
                    .child(div().text_sm().child(self.gh.message()))
                    .child(
                        h_flex().child(
                            Button::new("check-gh")
                                .small()
                                .outline()
                                .label("Check again")
                                .loading(self.checking)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.checking = true;
                                    cx.emit(AddProjectEvent::CheckGh);
                                    cx.notify();
                                })),
                        ),
                    )
                    .into_any_element(),
            },
            None => note("Loading your repos…".into()).into_any_element(),
            Some(Ok(_)) if loading => note("Loading your repos…".into()).into_any_element(),
            Some(Err(e)) => v_flex()
                .gap_2()
                .child(div().text_sm().text_color(theme.danger).child(e.clone()))
                .child(
                    h_flex().child(
                        Button::new("retry-repos")
                            .small()
                            .outline()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.repos = None;
                                this.cached = false;
                                cx.emit(AddProjectEvent::Retry);
                                cx.notify();
                            })),
                    ),
                )
                .into_any_element(),
            Some(Ok(_)) => {
                let rows = self.visible(cx);
                let empty = rows.is_empty();
                v_flex()
                    .id("repo-list")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .p_1()
                    .children(rows.into_iter().enumerate().map(|(i, r)| {
                        let taken = self.taken.contains(&r.full_name().to_lowercase());
                        let selected = self.selected.as_ref().is_some_and(|s| same_repo(s, &r));
                        let when = r
                            .pushed_at
                            .as_deref()
                            .and_then(flotilla_model::util::iso_to_ms)
                            .map(|t| format!(" · pushed {}", crate::fmt::ago_phrase(t, now)))
                            .unwrap_or_default();
                        // The list doesn't say a repo's default branch (a cached one may).
                        let branch = if r.default_branch.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", r.default_branch)
                        };
                        let meta = format!(
                            "{}{branch}{when}",
                            if r.private { "private" } else { "public" }
                        );
                        let repo = r.clone();
                        h_flex()
                            .id(("repo", i))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .when(selected, |el| el.bg(theme.accent))
                            .when(!taken && !selected, |el| {
                                el.hover(|s| s.bg(theme.secondary)).cursor_pointer()
                            })
                            .when(taken, |el| el.opacity(0.5))
                            .child(
                                div()
                                    .flex_none()
                                    .text_sm()
                                    .font_semibold()
                                    .child(r.full_name()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(meta),
                            )
                            .when(taken, |el| {
                                el.child(
                                    div().flex_none().text_xs().text_color(muted).child("added"),
                                )
                            })
                            .when(!taken, |el| {
                                el.on_click(
                                    cx.listener(move |this, _, _, cx| this.pick(repo.clone(), cx)),
                                )
                            })
                    }))
                    .when(empty, |el| {
                        el.child(
                            div().p_2().child(note(
                                if query {
                                    "No matching repos"
                                } else {
                                    "No repos on this GitHub account"
                                }
                                .into(),
                            )),
                        )
                    })
                    .into_any_element()
            }
        };

        let details = self.selected.clone().map(|repo| {
            let rows = self.scripts.iter().enumerate().map(|(i, row)| {
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&row.input).small()),
                    )
                    .child(
                        Checkbox::new(("fresh-only", i))
                            .label("From scratch only")
                            .checked(row.fresh_only)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(r) = this.scripts.get_mut(i) {
                                    r.fresh_only = *checked;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("remove-script", i))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if i < this.scripts.len() {
                                    this.scripts.remove(i);
                                }
                                cx.notify();
                            })),
                    )
            });
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(div().font_semibold().child(repo.full_name())),
                )
                .child(div().text_sm().font_semibold().child("Setup scripts"))
                .child(note(if self.suggesting {
                    "Suggesting scripts…".into()
                } else if self.agents_setup {
                    "Runs .agents/setup. Scripts here run first.".into()
                } else {
                    "Suggested from the repo.".into()
                }))
                .children(rows)
                .child(
                    h_flex().child(
                        Button::new("add-script")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add a script")
                            .disabled(self.suggesting)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let row = this.row("", false, window, cx);
                                row.input.update(cx, |s, cx| s.focus(window, cx));
                                this.scripts.push(row);
                                cx.notify();
                            })),
                    ),
                )
        });

        v_flex()
            .gap_3()
            .w_full()
            .when(self.gh.signed_in(), |el| {
                el.child(
                    Input::new(&self.search)
                        .prefix(Icon::new(IconName::Search).small().text_color(muted))
                        .suffix(if self.refreshing && !loading {
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("Refreshing…")
                                .into_any_element()
                        } else if let Some(e) = &self.refresh_failed {
                            div()
                                .id("refresh-failed")
                                .text_xs()
                                .text_color(muted)
                                .child("Couldn't refresh")
                                .tooltip({
                                    let e = e.clone();
                                    move |window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(e.clone())
                                            .build(window, cx)
                                    }
                                })
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                )
            })
            .child(list)
            .children(details)
            .children(self.error.clone().map(|e| {
                h_flex()
                    .gap_1()
                    .items_start()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(
                        div()
                            .flex_none()
                            .pt(px(2.))
                            .child(Icon::new(IconName::TriangleAlert).xsmall()),
                    )
                    .child(div().flex_1().min_w_0().child(e))
            }))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-add")
                            .small()
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(AddProjectEvent::Cancel))),
                    )
                    .child(
                        Button::new("add-project")
                            .small()
                            .primary()
                            .label(if self.adding {
                                "Adding…"
                            } else {
                                "Add project"
                            })
                            .loading(self.adding)
                            .disabled(
                                self.selected.is_none()
                                    || self.adding
                                    || self.suggesting
                                    || !self.gh.signed_in(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                    ),
            )
    }
}

fn same_repo(a: &RepoInfo, b: &RepoInfo) -> bool {
    a.full_name().eq_ignore_ascii_case(&b.full_name())
}

/// What a fresh repo list does to the pick: `Some(Some(r))` the same repo's new record,
/// `Some(None)` gone (only once the list is complete), `None` nothing to change.
fn rebind(selected: Option<&RepoInfo>, list: &[RepoInfo], done: bool) -> Option<Option<RepoInfo>> {
    let picked = selected?;
    match list.iter().find(|r| same_repo(r, picked)) {
        Some(r) => Some(Some(r.clone())),
        None if done => Some(None),
        // A list still loading may not have reached it yet.
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{RepoInfo, rebind};

    fn repo(name: &str, pushed: &str) -> RepoInfo {
        RepoInfo {
            owner: "acme".into(),
            name: name.into(),
            private: true,
            default_branch: "main".into(),
            pushed_at: Some(pushed.into()),
            description: None,
        }
    }

    #[test]
    fn a_pick_survives_a_fresh_list() {
        let picked = repo("web", "2026-10-30T10:00:00Z");
        // The refresh moved its pushed time on: the pick takes the new record.
        let fresh = [
            repo("api", "2026-10-30T11:00:00Z"),
            repo("web", "2026-10-30T12:00:00Z"),
        ];
        assert_eq!(
            rebind(Some(&picked), &fresh, true),
            Some(Some(fresh[1].clone()))
        );
        // Not in a page that's still loading: leave it.
        assert_eq!(rebind(Some(&picked), &fresh[..1], false), None);
        // Not in the whole list: it's gone.
        assert_eq!(rebind(Some(&picked), &fresh[..1], true), Some(None));
        assert_eq!(rebind(None, &fresh, true), None);
    }
}
