//! Scheduled: every scheduled prompt, with when it runs next and how its last run went, and
//! a switch to pause it, Run now, Edit and Delete.

use flotilla_model::schedule as sched;
use flotilla_proto::{RunOutcome, Schedule};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub enum SchedulesEvent {
    Pause {
        id: String,
        paused: bool,
    },
    RunNow(String),
    Edit(String),
    Delete(String),
    /// Open the thread it sends to, or its last run started.
    Open(String),
}

/// A schedule as its row shows it.
#[derive(Clone)]
pub struct Row {
    pub schedule: Schedule,
    /// Where it goes, in words.
    pub to: String,
    /// The thread to open from it, if there is one.
    pub thread: Option<String>,
}

pub struct SchedulesView {
    rows: Vec<Row>,
    /// This Mac's own server runs them: only while the app is open.
    this_mac: bool,
    /// The one asked about (from a thread), shown first.
    focus: Option<String>,
    scroll: ScrollHandle,
}

impl EventEmitter<SchedulesEvent> for SchedulesView {}

impl SchedulesView {
    pub fn new(rows: Vec<Row>, this_mac: bool, focus: Option<String>) -> SchedulesView {
        SchedulesView {
            rows,
            this_mac,
            focus,
            scroll: ScrollHandle::new(),
        }
    }

    pub fn set_rows(&mut self, rows: Vec<Row>, this_mac: bool, cx: &mut Context<Self>) {
        self.rows = rows;
        self.this_mac = this_mac;
        cx.notify();
    }

    fn render_row(
        &self,
        row: &Row,
        now: i64,
        here: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let s = &row.schedule;
        let id = s.id.clone();
        let first = flotilla_model::util::one_line(s.prompt.lines().next().unwrap_or_default());
        let last = sched::last_words(s, now, here);
        let failed = s
            .last
            .as_ref()
            .is_some_and(|l| l.outcome == RunOutcome::Failed);
        let done = s.done();
        let focused = self.focus.as_deref() == Some(id.as_str());
        let act = |e: fn(String) -> SchedulesEvent, id: &String| {
            let id = id.clone();
            cx.listener(move |_, _, _, cx| cx.emit(e(id.clone())))
        };
        v_flex()
            .id(SharedString::from(format!("schedule-{id}")))
            .gap_0p5()
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(if focused { theme.ring } else { theme.border })
            .when(s.paused || done, |el| el.opacity(0.75))
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_medium()
                            .child(first),
                    )
                    // A one-off that ran has nothing to pause.
                    .when(!done, |el| {
                        let id = id.clone();
                        el.child(
                            Switch::new(SharedString::from(format!("schedule-on-{id}")))
                                .small()
                                .checked(!s.paused)
                                .tooltip(if s.paused { "Resume" } else { "Pause" })
                                .on_click(cx.listener(move |_, on: &bool, _, cx| {
                                    cx.emit(SchedulesEvent::Pause {
                                        id: id.clone(),
                                        paused: !*on,
                                    })
                                })),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("schedule-run-{id}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Play)
                            .tooltip("Run now")
                            .on_click(act(SchedulesEvent::RunNow, &id)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("schedule-edit-{id}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Pencil)
                            .tooltip("Edit")
                            .on_click(act(SchedulesEvent::Edit, &id)),
                    )
                    .child(
                        Button::new(SharedString::from(format!("schedule-delete-{id}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("Delete")
                            .on_click(act(SchedulesEvent::Delete, &id)),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(sched::summary(s, now, here)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_none().child("To"))
                    .child(match &row.thread {
                        Some(t) => {
                            let t = t.clone();
                            div()
                                .id(SharedString::from(format!("schedule-to-{id}")))
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_color(theme.foreground)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .child(row.to.clone())
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(SchedulesEvent::Open(t.clone()))
                                }))
                                .into_any_element()
                        }
                        None => div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(row.to.clone())
                            .into_any_element(),
                    }),
            )
            .children(last.map(|l| {
                div()
                    .text_sm()
                    .text_color(if failed {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(l)
            }))
    }
}

impl Render for SchedulesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now = flotilla_model::util::now_ms();
        let here = sched::here();
        let mut rows = self.rows.clone();
        if let Some(f) = &self.focus
            && let Some(i) = rows.iter().position(|r| &r.schedule.id == f)
        {
            let r = rows.remove(i);
            rows.insert(0, r);
        }
        let mut shown = Vec::with_capacity(rows.len());
        for r in &rows {
            shown.push(self.render_row(r, now, &here, cx).into_any_element());
        }
        v_flex()
            .gap_2()
            .child(
                v_flex()
                    .id("schedules")
                    .gap_2()
                    .max_h(px(520.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(shown),
            )
            .when(self.this_mac, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("This Mac runs these only while Flotilla is open."),
                )
            })
    }
}
