//! A question the thread's agent asked you (a message of kind `ask`, see
//! `flotilla_proto::ask`), as its row in the thread shows it: waiting, its options to pick (a
//! radio each, or a checkbox each where several can be picked), Other to type your own, and
//! Send; once answered, what was asked and what you chose.

use crate::thread_view::ThreadView;
use flotilla_proto::Message;
use flotilla_proto::ask::{self, Question};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::hash::{Hash, Hasher};

/// Your answer in the making to one question the agent waits on, kept by the thread view.
#[derive(Default)]
pub struct AskDraft {
    /// Per question: the options picked (their labels), in the order picked.
    pub picked: Vec<Vec<String>>,
    /// Per question: where you type your own answer, once Other was chosen.
    pub other: Vec<Option<Entity<InputState>>>,
    /// Sent; waiting for the server to take it.
    pub sending: bool,
    /// Why it couldn't go.
    pub error: Option<String>,
}

impl AskDraft {
    pub fn new(questions: usize) -> AskDraft {
        AskDraft {
            picked: vec![Vec::new(); questions],
            other: vec![None; questions],
            sending: false,
            error: None,
        }
    }

    /// What the row shows of it.
    pub fn shown(&self) -> AskShown {
        AskShown {
            picked: self.picked.clone(),
            other: self.other.clone(),
            sending: self.sending,
            error: self.error.clone(),
        }
    }

    /// The answers to send, one per question: what was typed (with what was picked, where
    /// several can be), else what was picked.
    pub fn answers(&self, questions: &[Question], cx: &App) -> Vec<Vec<String>> {
        questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let picked = self.picked.get(i).cloned().unwrap_or_default();
                let typed = self
                    .other
                    .get(i)
                    .cloned()
                    .flatten()
                    .map(|s| s.read(cx).value().trim().to_string())
                    .filter(|t| !t.is_empty());
                match typed {
                    Some(t) if q.multi => picked.into_iter().chain([t]).collect(),
                    Some(t) => vec![t],
                    None => picked,
                }
            })
            .collect()
    }
}

/// A snapshot of an [`AskDraft`] for its row (rows are compared by what they show).
#[derive(Clone, PartialEq, Eq)]
pub struct AskShown {
    pub picked: Vec<Vec<String>>,
    pub other: Vec<Option<Entity<InputState>>>,
    pub sending: bool,
    pub error: Option<String>,
}

impl Hash for AskShown {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.picked.hash(h);
        for o in &self.other {
            o.as_ref().map(|e| e.entity_id()).hash(h);
        }
        (self.sending, &self.error).hash(h);
    }
}

/// The row for ask message `m`. `draft`: your answer in the making, while it waits. `answer`:
/// once answered, how the message that took the answer is going, where it isn't done (that
/// message isn't shown: this card stands for it).
pub fn card(
    m: &Message,
    draft: Option<&AskShown>,
    answer: Option<&str>,
    ident: &str,
    view: WeakEntity<ThreadView>,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let questions = ask::questions(m);
    let ask_id = m.tool_id.clone().unwrap_or_default();
    let state = m.status.as_deref().unwrap_or(ask::WAITING);
    let waiting = state == ask::WAITING;
    let answers = ask::answers(m);
    let (status, status_color) = match state {
        ask::ANSWERED => (answered_words(answer), theme.muted_foreground),
        ask::PASSED => ("not answered".to_string(), theme.muted_foreground),
        _ => ("waiting".to_string(), theme.warning),
    };
    let label = match questions.len() {
        0 | 1 => "Question".to_string(),
        n => format!("{n} questions"),
    };
    let empty = AskShown {
        picked: vec![Vec::new(); questions.len()],
        other: vec![None; questions.len()],
        sending: false,
        error: None,
    };
    let draft = draft.unwrap_or(&empty);
    let mut body = v_flex()
        .id(SharedString::from(format!("ask-{ident}")))
        .gap_3()
        .px_3()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(if waiting {
            theme.warning.opacity(0.5)
        } else {
            theme.border
        })
        .child(
            h_flex()
                .justify_between()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(
                            Icon::new(IconName::MessageCircleQuestionMark)
                                .xsmall()
                                .text_color(status_color),
                        )
                        .child(label),
                )
                .child(div().text_color(status_color).child(status)),
        );
    for (qi, q) in questions.iter().enumerate() {
        let mut block = v_flex().gap_1p5();
        if let Some(h) = q.header.as_deref().filter(|h| !h.trim().is_empty()) {
            block = block.child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child(h.to_string()),
            );
        }
        block = block.child(
            div()
                .text_color(if state == ask::PASSED {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .child(
                    TextView::markdown(
                        SharedString::from(format!("ask-q-{ident}-{qi}")),
                        q.question.clone(),
                    )
                    .selectable(true),
                ),
        );
        if waiting {
            block = block.child(choices(q, qi, &ask_id, draft, ident, view.clone(), theme));
        } else if let Some(a) = answers.as_ref().and_then(|a| a.get(qi)) {
            block = block.child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        div().pt(px(3.)).child(
                            Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(theme.success),
                        ),
                    )
                    .child(div().min_w_0().text_sm().child(a.join(", "))),
            );
        }
        body = body.child(block);
    }
    if waiting {
        let (send_view, send_ask) = (view.clone(), ask_id.clone());
        body = body.child(
            h_flex()
                .gap_3()
                .justify_end()
                .when_some(draft.error.clone(), |el, e| {
                    el.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.danger)
                            .child(e),
                    )
                })
                .child(
                    Button::new(SharedString::from(format!("ask-send-{ident}")))
                        .primary()
                        .small()
                        .label(if draft.sending { "Sending…" } else { "Send" })
                        .disabled(draft.sending)
                        .on_click(move |_, window, cx| {
                            let ask = send_ask.clone();
                            let _ = send_view.update(cx, |v, cx| v.send_answer(&ask, window, cx));
                        }),
                ),
        );
    }
    body.into_any_element()
}

/// An answered card's status: "answered", and how its answer's message is going while it isn't
/// done ("answered · sending", "answered · running").
pub fn answered_words(answer: Option<&str>) -> String {
    match answer {
        Some(s) => format!("answered · {s}"),
        None => "answered".into(),
    }
}

/// A question's options as you pick: one row each (the tool's order, recommended first), then
/// Other, which opens a field to type your own answer in.
fn choices(
    q: &Question,
    qi: usize,
    ask_id: &str,
    draft: &AskShown,
    ident: &str,
    view: WeakEntity<ThreadView>,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let picked = draft.picked.get(qi).cloned().unwrap_or_default();
    let other = draft.other.get(qi).cloned().flatten();
    let disabled = draft.sending;
    let mut list = v_flex().gap_0p5();
    for (oi, o) in q.options.iter().enumerate() {
        let on = picked.contains(&o.label);
        let id = SharedString::from(format!("ask-o-{ident}-{qi}-{oi}"));
        let pick = {
            let (view, ask, label, multi) =
                (view.clone(), ask_id.to_string(), o.label.clone(), q.multi);
            move |window: &mut Window, cx: &mut App| {
                let (ask, label) = (ask.clone(), label.clone());
                let _ = view.update(cx, |v, cx| {
                    v.pick_option(&ask, qi, label, multi, window, cx)
                });
            }
        };
        let pick_row = pick.clone();
        let indicator = if q.multi {
            Checkbox::new(SharedString::from(format!("{id}-mark")))
                .checked(on)
                .disabled(disabled)
                .on_click(move |_, window, cx| pick(window, cx))
                .into_any_element()
        } else {
            Radio::new(SharedString::from(format!("{id}-mark")))
                .checked(on)
                .disabled(disabled)
                .on_click(move |_, window, cx| pick(window, cx))
                .into_any_element()
        };
        list = list.child(
            h_flex()
                .id(id)
                .gap_2()
                .items_start()
                .px_2()
                .py_1()
                .rounded_md()
                .when(!disabled, |el| {
                    el.cursor_pointer().hover(|el| el.bg(theme.secondary))
                })
                .when(on, |el| el.bg(theme.selection))
                .on_click(move |_, window, cx| {
                    if !disabled {
                        pick_row(window, cx)
                    }
                })
                .child(div().pt(px(2.)).child(indicator))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(div().text_sm().child(o.label.clone()))
                        .when_some(
                            o.description.clone().filter(|d| !d.trim().is_empty()),
                            |el, d| {
                                el.child(
                                    div().text_xs().text_color(theme.muted_foreground).child(d),
                                )
                            },
                        ),
                ),
        );
    }
    list = list.child(match other {
        Some(input) => h_flex()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&input).small().disabled(disabled)),
            )
            .into_any_element(),
        None => {
            let (view, ask) = (view.clone(), ask_id.to_string());
            h_flex()
                .id(SharedString::from(format!("ask-other-{ident}-{qi}")))
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .text_color(theme.muted_foreground)
                .when(!disabled, |el| {
                    el.cursor_pointer()
                        .hover(|el| el.bg(theme.secondary).text_color(theme.foreground))
                })
                .on_click(move |_, window, cx| {
                    if !disabled {
                        let ask = ask.clone();
                        let _ = view.update(cx, |v, cx| v.open_other(&ask, qi, window, cx));
                    }
                })
                .child(Icon::new(IconName::Pencil).xsmall())
                .child("Other")
                .into_any_element()
        }
    });
    list.into_any_element()
}
