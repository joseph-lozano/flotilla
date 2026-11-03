//! A project's settings: its setup (what new threads start ready on, a run in progress or a
//! failed one, Run setup again, Rebuild from scratch), its setup scripts, and recent runs.
//! The prepared VM threads copy is never mentioned: to the user this is the project's setup.

use flotilla_model::setup::{self, short};
use flotilla_model::util::iso_to_ms;
use flotilla_proto::{Project, SetupRun, SetupScript};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Sizable, StyledExt as _, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;

pub enum ProjectEvent {
    RunSetup {
        fresh: bool,
    },
    Cancel,
    SaveScripts {
        scripts: Vec<SetupScript>,
        run: bool,
    },
    Remove,
    /// The machine size new threads start on unless they pick one (boat's name for it).
    SetMachineType(String),
    /// Open New thread with a prompt to fix this failed run.
    FixSetup {
        run: String,
    },
}

struct ScriptRow {
    input: Entity<InputState>,
    fresh_only: bool,
}

pub struct ProjectView {
    project: Project,
    /// Newest first.
    runs: Vec<SetupRun>,
    saved: Vec<SetupScript>,
    rows: Vec<ScriptRow>,
    github_error: Option<String>,
    show_log: bool,
    /// Asked for a run that hasn't shown up yet.
    requested: bool,
    _tick: Task<()>,
}

impl EventEmitter<ProjectEvent> for ProjectView {}

/// Why a run happened, for the history.
fn trigger_label(t: &str) -> &'static str {
    match t {
        "added" => "Project added",
        "main" => "main moved",
        "merge" => "PR merged",
        "manual" => "Manual",
        "fresh" => "Rebuilt from scratch",
        "refresh" => "Refreshed",
        "scripts" => "Scripts changed",
        "gone" => "VM deleted",
        _ => "",
    }
}

fn took(run: &SetupRun, now: i64) -> String {
    let start = iso_to_ms(&run.started_at).unwrap_or(now);
    let end = run
        .finished_at
        .as_deref()
        .and_then(iso_to_ms)
        .unwrap_or(now);
    let s = ((end - start).max(0) / 1000) as u64;
    if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

impl ProjectView {
    pub fn new(
        project: Project,
        runs: Vec<SetupRun>,
        scripts: Vec<SetupScript>,
        github_error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Redraw each second while a run is going, for its timer.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(()) = this.update(cx, |v, cx| {
                    if v.running().is_some() {
                        cx.notify();
                    }
                }) else {
                    return;
                };
            }
        });
        let mut view = ProjectView {
            project,
            runs,
            saved: Vec::new(),
            rows: Vec::new(),
            github_error,
            show_log: false,
            requested: false,
            _tick: tick,
        };
        view.set_scripts(scripts, window, cx);
        view
    }

    /// New data from the database.
    pub fn refresh(
        &mut self,
        project: Project,
        runs: Vec<SetupRun>,
        github_error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if runs.first().map(|r| &r.id) != self.runs.first().map(|r| &r.id) {
            self.requested = false;
        }
        self.project = project;
        self.runs = runs;
        self.github_error = github_error;
        cx.notify();
    }

    fn set_scripts(
        &mut self,
        scripts: Vec<SetupScript>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rows = scripts
            .iter()
            .map(|s| Self::row(&s.command, s.fresh_only, window, cx))
            .collect();
        self.saved = scripts;
    }

    fn row(
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
        cx.observe(&input, |_, _, cx| cx.notify()).detach();
        ScriptRow { input, fresh_only }
    }

    fn edited(&self, cx: &App) -> Vec<SetupScript> {
        self.rows
            .iter()
            .map(|r| SetupScript {
                command: r.input.read(cx).value().trim().to_string(),
                fresh_only: r.fresh_only,
            })
            .filter(|s| !s.command.is_empty())
            .collect()
    }

    /// Keep the scripts as edited (and run setup with them, `run`).
    fn save(&mut self, run: bool, cx: &mut Context<Self>) {
        let scripts = self.edited(cx);
        self.saved = scripts.clone();
        self.requested |= run;
        cx.emit(ProjectEvent::SaveScripts { scripts, run });
        cx.notify();
    }

    /// The size new threads start on, saved at once.
    fn set_machine_type(&mut self, t: String, cx: &mut Context<Self>) {
        if self.project.machine_type.as_deref().unwrap_or("default") == t {
            return;
        }
        self.project.machine_type = Some(t.clone());
        cx.emit(ProjectEvent::SetMachineType(t));
        cx.notify();
    }

    /// Dev: Add a script, as its button does (a new row, focused).
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_add_script(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = Self::row("", false, window, cx);
        row.input.update(cx, |s, cx| s.focus(window, cx));
        self.rows.push(row);
        cx.notify();
    }

    /// Enter: Save, if the scripts changed.
    pub fn save_on_enter(&mut self, cx: &mut Context<Self>) {
        if self.edited(cx) != self.saved {
            self.save(false, cx);
        }
    }

    fn running(&self) -> Option<&SetupRun> {
        self.runs.first().filter(|r| r.running())
    }

    fn ask(&mut self, fresh: bool, cx: &mut Context<Self>) {
        self.requested = true;
        cx.emit(ProjectEvent::RunSetup { fresh });
        cx.notify();
    }

    fn confirm_fresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let name = self.project.repo.clone();
        let last = self
            .runs
            .iter()
            .find(|r| r.kind == "fresh" && r.state == "done")
            .map(|r| took(r, 0));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let length = last
                .clone()
                .map(|t| format!(" (about {t} last time)"))
                .unwrap_or_default();
            alert
                .title(format!("Rebuild {name} from scratch?"))
                .description(format!("Clones the repo and runs setup again{length}."))
                .confirm()
                .ok_text("Rebuild")
                .on_ok(move |_, _, cx| {
                    let _ = this.update(cx, |v, cx| v.ask(true, cx));
                    true
                })
        });
    }

    fn render_setup(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let now = chrono::Utc::now().timestamp_millis();
        let p = &self.project;
        let branch = &p.default_branch;
        let tail_box = |lines: Vec<String>| {
            div()
                .w_full()
                .rounded_md()
                .bg(theme.secondary)
                .px_2()
                .py_1()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(muted)
                .children(lines.into_iter().map(|l| div().truncate().child(l)))
        };

        let status: AnyElement = if let Some(run) = self.running() {
            let scripts = &self.saved;
            let stages = setup::stages(
                p,
                &run.kind,
                &run.sha,
                scripts,
                setup::ran_agents_setup(run),
            );
            let current = run
                .step
                .as_deref()
                .and_then(|s| stages.iter().position(|x| x == s))
                .unwrap_or(0);
            let tail = setup::tail(&run.log, 4);
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .font_semibold()
                                .child(format!("Setting up {branch}@{}", short(&run.sha))),
                        )
                        .child(div().text_xs().text_color(muted).child(took(run, now)))
                        .child(
                            Button::new("cancel-setup")
                                .small()
                                .ghost()
                                .label("Cancel")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(ProjectEvent::Cancel))),
                        ),
                )
                .children(stages.iter().enumerate().map(|(i, st)| {
                    let (icon, color) = match i.cmp(&current) {
                        std::cmp::Ordering::Less => (IconName::CircleCheck, theme.success),
                        std::cmp::Ordering::Equal => (IconName::Loader, theme.warning),
                        std::cmp::Ordering::Greater => (IconName::Circle, muted),
                    };
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(Icon::new(icon).xsmall().text_color(color))
                        .child(
                            div()
                                .when(i > 1 && i + 1 < stages.len(), |el| {
                                    el.font_family(theme.mono_font_family.clone()).text_xs()
                                })
                                .child(st.clone()),
                        )
                }))
                .when(!tail.is_empty(), |el| el.child(tail_box(tail)))
                .into_any_element()
        } else {
            let last = self.runs.first();
            let failed = last.filter(|r| r.state == "failed");
            let mut col = v_flex().gap_1();
            if let Some(run) = failed {
                let at = run.step.clone().unwrap_or_else(|| "setup".into());
                col = col
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(Icon::new(IconName::CircleX).small())
                            .child(div().font_semibold().child(format!("Setup failed at {at}")))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("on {branch}@{}", short(&run.sha))),
                            ),
                    )
                    .children(
                        run.error
                            .clone()
                            .filter(|e| !e.is_empty())
                            .map(|e| div().text_xs().text_color(muted).child(e)),
                    )
                    .child(tail_box(setup::tail(&run.log, 4)));
                if flotilla_model::fix::setup_fixable(run, &self.saved) {
                    let id = run.id.clone();
                    col = col.child(
                        h_flex().child(
                            Button::new("fix-setup")
                                .small()
                                .outline()
                                .icon(IconName::Sparkles)
                                .label("Fix in a new thread")
                                .tooltip("Ask an agent to fix it")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(ProjectEvent::FixSetup { run: id.clone() })
                                })),
                        ),
                    );
                }
            }
            match (&p.template_sha, &p.template_built_at) {
                (Some(sha), built) => {
                    let ago = built
                        .as_deref()
                        .and_then(iso_to_ms)
                        .map(|t| crate::fmt::ago_phrase(t, now))
                        .unwrap_or_default();
                    let length = self
                        .runs
                        .iter()
                        .find(|r| r.state == "done" && &r.sha == sha)
                        .map(|r| format!(", took {}", took(r, now)))
                        .unwrap_or_default();
                    col = col.child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(
                                Icon::new(IconName::CircleCheck)
                                    .small()
                                    .text_color(theme.success),
                            )
                            .child(
                                div()
                                    .font_semibold()
                                    .child(format!("Ready on {branch}@{}", short(sha))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("set up {ago}{length}")),
                            ),
                    );
                }
                (None, _) if failed.is_none() => {
                    col = col.child(div().text_sm().text_color(muted).child(if self.requested {
                        "Starting setup…"
                    } else {
                        "Not set up yet."
                    }));
                }
                _ => {}
            }
            col.into_any_element()
        };

        let checked = p
            .checked_at
            .as_deref()
            .and_then(iso_to_ms)
            .map(|t| crate::fmt::ago_phrase(t, now));
        let busy = self.running().is_some() || self.requested;
        let log = self.runs.first().map(|r| r.log.clone()).unwrap_or_default();
        v_flex()
            .gap_2()
            .child(status)
            .children(checked.map(|c| {
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Checked {c}"))
            }))
            .children(
                self.github_error
                    .clone()
                    .filter(|e| !e.is_empty())
                    .map(|e| {
                        h_flex()
                            .gap_1()
                            .items_start()
                            .text_xs()
                            .text_color(theme.warning)
                            .child(
                                div()
                                    .flex_none()
                                    .pt(px(1.))
                                    .child(Icon::new(IconName::TriangleAlert).xsmall()),
                            )
                            .child(div().flex_1().min_w_0().child(e))
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("run-setup")
                            .small()
                            .outline()
                            .label(if self.runs.first().is_some_and(|r| r.state == "failed") {
                                "Try again"
                            } else {
                                "Run setup again"
                            })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.ask(false, cx))),
                    )
                    .child(
                        Button::new("rebuild")
                            .small()
                            .outline()
                            .with_variant(ButtonVariant::Danger)
                            .label("Rebuild from scratch…")
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.confirm_fresh(window, cx)),
                            ),
                    )
                    .when(!log.is_empty(), |el| {
                        el.child(
                            Button::new("view-log")
                                .small()
                                .ghost()
                                .label(if self.show_log {
                                    "Hide log"
                                } else {
                                    "View log"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_log = !this.show_log;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(self.show_log && !log.is_empty(), |el| {
                el.child(
                    div()
                        .id("setup-log")
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .rounded_md()
                        .bg(theme.secondary)
                        .p_2()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .child(log),
                )
            })
    }
}

impl Render for ProjectView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let muted = theme.muted_foreground;
        let heading = |t: &'static str| div().text_sm().font_semibold().child(t);
        let section = || {
            v_flex()
                .gap_2()
                .py_3()
                .border_b_1()
                .border_color(theme.border)
        };
        let edited = self.edited(cx);
        let changed = edited != self.saved;
        let now = chrono::Utc::now().timestamp_millis();
        let setup = self.render_setup(cx).into_any_element();

        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&row.input).small()),
                    )
                    .child(
                        Checkbox::new(("p-fresh-only", i))
                            .label("From scratch only")
                            .checked(row.fresh_only)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(r) = this.rows.get_mut(i) {
                                    r.fresh_only = *checked;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(("p-remove-script", i))
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if i < this.rows.len() {
                                    this.rows.remove(i);
                                }
                                cx.notify();
                            })),
                    )
            })
            .collect();

        let history = self.runs.iter().take(8).map(|r| {
            let (result, color) = match r.state.as_str() {
                "done" => ("ready".to_string(), theme.success),
                "running" => ("running".to_string(), theme.warning),
                "cancelled" => ("cancelled".to_string(), muted),
                _ => (
                    format!("failed: {}", r.step.clone().unwrap_or_default()),
                    theme.danger,
                ),
            };
            h_flex()
                .gap_3()
                .text_xs()
                .py(px(2.))
                .child(
                    div()
                        .w(px(64.))
                        .font_family(theme.mono_font_family.clone())
                        .child(short(&r.sha).to_string()),
                )
                .child(
                    div()
                        .w(px(150.))
                        .text_color(muted)
                        .child(trigger_label(&r.trigger)),
                )
                .child(div().w(px(60.)).text_color(muted).child(took(r, now)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(color)
                        .child(result),
                )
        });

        v_flex()
            .id("project-scroll")
            .max_h(px(620.))
            .overflow_y_scroll()
            .pr_2()
            .child(div().text_xs().text_color(muted).pb_2().child(format!(
                "{} · {}",
                self.project.full_name(),
                self.project.default_branch
            )))
            .child(section().pt_0().child(heading("Setup")).child(setup))
            .child({
                let weak = cx.weak_entity();
                section().child(heading("Machine size")).child(
                    h_flex()
                        .gap_3()
                        .child(crate::machine_size::menu(
                            "p-machine-size",
                            self.project.machine_type.as_deref(),
                            false,
                            move |t, _, cx| {
                                let _ = weak.update(cx, |v, cx| v.set_machine_type(t, cx));
                            },
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child("New threads start on this size unless they pick another."),
                        ),
                )
            })
            .child(
                section()
                    .child(heading("Setup scripts"))
                    .children(rows)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("p-add-script")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Plus)
                                    .label("Add a script")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let row = Self::row("", false, window, cx);
                                        row.input.update(cx, |s, cx| s.focus(window, cx));
                                        this.rows.push(row);
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1())
                            .when(changed, |el| {
                                el.child(
                                    Button::new("save-scripts")
                                        .small()
                                        .outline()
                                        .label("Save")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.save(false, cx)),
                                        ),
                                )
                                .child(
                                    Button::new("save-run-scripts")
                                        .small()
                                        .primary()
                                        .label("Save and run setup")
                                        .disabled(self.running().is_some())
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.save(true, cx)),
                                        ),
                                )
                            }),
                    ),
            )
            .when(!self.runs.is_empty(), |el| {
                el.child(section().child(heading("Setup history")).children(history))
            })
            .child(
                h_flex().pt_3().child(
                    Button::new("remove-project")
                        .small()
                        .ghost()
                        .with_variant(ButtonVariant::Danger)
                        .icon(IconName::Trash)
                        .label("Remove project…")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ProjectEvent::Remove))),
                ),
            )
    }
}
