//! Send later: a prompt sent at a time, or again and again (at most daily), to this thread or
//! to a new thread each time (`flotilla_proto::schedule`). The same form edits a schedule from
//! Scheduled.

use chrono::{Datelike as _, NaiveDate, NaiveTime, Timelike as _};
use flotilla_model::schedule as sched;
use flotilla_proto::{ScheduleSpec, Target, When};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::date_picker::{DatePicker, DatePickerState};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::time_field::{TimeField, TimeFieldEvent, TimeFieldState, TimePrecision};
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Selectable as _, Sizable, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub enum EditorEvent {
    /// Keep these: one per target (New thread's models each get one).
    Save(Vec<ScheduleSpec>),
    Cancel,
}

/// What the form starts with.
pub struct Seed {
    /// Editing this one; None: new ones.
    pub id: Option<String>,
    pub prompt: String,
    pub targets: Vec<Target>,
    /// Where it goes, in words: the thread's title, or "New thread · Opus 5.5 · acme/web".
    pub to: String,
    pub when: Option<When>,
    /// Its times' zone: this device's for a new one.
    pub zone: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repeat {
    Daily,
    Weekdays,
    Weekly,
    Cron,
}

const REPEATS: [(Repeat, &str); 4] = [
    (Repeat::Daily, "Daily"),
    (Repeat::Weekdays, "Weekdays"),
    (Repeat::Weekly, "Weekly"),
    (Repeat::Cron, "Cron"),
];
const DAY_LETTERS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub struct ScheduleEditor {
    id: Option<String>,
    /// One id per target, made once: saved again after a refusal or a lost answer, a schedule
    /// already kept is the same one, not another.
    ids: Vec<String>,
    /// What it was, when editing: a timing kept as it was isn't checked again.
    was: Option<ScheduleSpec>,
    /// A one-off's time as set (to the ms) and as the picker shows it: kept until the picker
    /// changes, so an edit of the prompt doesn't move it to the minute.
    once_was: Option<(i64, chrono::NaiveDateTime)>,
    targets: Vec<Target>,
    to: String,
    zone: String,
    prompt: Entity<TextareaState>,
    repeat: bool,
    kind: Repeat,
    once: Entity<DatePickerState>,
    at: Entity<TimeFieldState>,
    days: [bool; 7],
    cron: Entity<InputState>,
    error: Option<String>,
    saving: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for ScheduleEditor {}

/// `ms` in `zone` for the pickers (chrono's naive date and time).
fn naive(ms: i64, zone: &str) -> Option<chrono::NaiveDateTime> {
    let (y, m, d, minutes) = sched::local_parts(ms, zone)?;
    Some(
        NaiveDate::from_ymd_opt(y, m, d)?.and_time(NaiveTime::from_hms_opt(
            minutes / 60,
            minutes % 60,
            0,
        )?),
    )
}

impl ScheduleEditor {
    pub fn new(seed: Seed, window: &mut Window, cx: &mut Context<Self>) -> ScheduleEditor {
        let now = flotilla_model::util::now_ms();
        let zone = seed.zone.clone();
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("What should the agent do?")
                .default_value(seed.prompt.clone())
        });
        let once = cx.new(|cx| {
            DatePickerState::new(window, cx)
                .time_precision(TimePrecision::Minute)
                .date_format("%a %-d %b %Y, %H:%M")
                .first_day_of_week(chrono::Weekday::Mon)
        });
        let at = cx.new(|cx| TimeFieldState::new(window, cx));
        let cron = cx.new(|cx| InputState::new(window, cx).placeholder("0 9 * * 1-5"));
        let was = match (&seed.id, &seed.when, seed.targets.first()) {
            (Some(id), Some(when), Some(target)) => Some(ScheduleSpec {
                id: id.clone(),
                prompt: seed.prompt.clone(),
                target: target.clone(),
                when: when.clone(),
                zone: seed.zone.clone(),
            }),
            _ => None,
        };
        let once_was = match &seed.when {
            Some(When::Once { at_ms }) => naive(*at_ms, &seed.zone).map(|v| (*at_ms, v)),
            _ => None,
        };
        let ids = (0..seed.targets.len())
            .map(|i| match (&seed.id, i) {
                (Some(id), 0) => id.clone(),
                _ => uuid::Uuid::new_v4().to_string(),
            })
            .collect();
        let mut editor = ScheduleEditor {
            ids,
            was,
            once_was,
            id: seed.id,
            targets: seed.targets,
            to: seed.to,
            zone,
            prompt,
            repeat: false,
            kind: Repeat::Daily,
            once,
            at,
            days: [true, false, false, false, false, false, false],
            cron,
            error: None,
            saving: false,
            _subscriptions: vec![],
        };
        // A new one starts an hour on (a round minute); recurring ones at 09:00.
        let first = sched::quick_times(now, &editor.zone)
            .into_iter()
            .next()
            .map(|(_, ms)| ms)
            .unwrap_or(now);
        let mut once_at = first;
        let mut at_minutes = 9 * 60;
        match &seed.when {
            None | Some(When::Once { .. }) => {}
            Some(_) => editor.repeat = true,
        }
        match seed.when {
            Some(When::Once { at_ms }) => once_at = at_ms,
            // Every N hours, set before runs had to be a day apart: daily at its first run's
            // time, for an edit to make it so.
            Some(When::Hours { from_ms, .. }) => {
                editor.kind = Repeat::Daily;
                at_minutes =
                    sched::local_parts(from_ms, &editor.zone).map_or(at_minutes, |(.., m)| m);
            }
            Some(When::Daily { at }) => (editor.kind, at_minutes) = (Repeat::Daily, at),
            Some(When::Weekdays { at }) => (editor.kind, at_minutes) = (Repeat::Weekdays, at),
            Some(When::Weekly { days, at }) => {
                editor.kind = Repeat::Weekly;
                at_minutes = at;
                editor.days = std::array::from_fn(|i| days.contains(&(i as u8 + 1)));
            }
            Some(When::Cron { expr }) => {
                editor.kind = Repeat::Cron;
                editor
                    .cron
                    .update(cx, |c, cx| c.set_value(expr, window, cx));
            }
            None => {}
        }
        if let Some(v) = naive(once_at, &editor.zone) {
            editor
                .once
                .update(cx, |o, cx| o.set_date_time(v, window, cx));
        }
        if let Some(t) = NaiveTime::from_hms_opt(at_minutes / 60, at_minutes % 60, 0) {
            editor.at.update(cx, |a, cx| a.set_time(t, window, cx));
        }
        // What's typed or picked changes the next time shown.
        let subs = vec![
            cx.subscribe(
                &editor.once,
                |this, _, _: &gpui_kit::component::date_picker::DatePickerEvent, cx| {
                    this.edited(cx)
                },
            ),
            cx.subscribe(&editor.at, |this, _, _: &TimeFieldEvent, cx| {
                this.edited(cx)
            }),
            cx.subscribe(&editor.cron, |this, _, _: &InputEvent, cx| this.edited(cx)),
            cx.subscribe(&editor.prompt, |this, _, _: &InputEvent, cx| {
                this.edited(cx)
            }),
        ];
        editor._subscriptions = subs;
        editor
    }

    pub fn prompt(&self) -> Entity<TextareaState> {
        self.prompt.clone()
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        cx.notify();
    }

    /// The server said no: why, and the form back to edit.
    pub fn refused(&mut self, why: String, cx: &mut Context<Self>) {
        self.saving = false;
        self.error = Some(why);
        cx.notify();
    }

    fn when(&self, cx: &App) -> Result<When, String> {
        if !self.repeat {
            let v = self
                .once
                .read(cx)
                .date_time()
                .start()
                .ok_or("Pick a day and a time.")?;
            if let Some((at_ms, shown)) = self.once_was
                && shown.date() == v.date()
                && (shown.hour(), shown.minute()) == (v.hour(), v.minute())
            {
                return Ok(When::Once { at_ms });
            }
            let at_ms = sched::local_ms(
                &self.zone,
                v.year(),
                v.month(),
                v.day(),
                v.hour() * 60 + v.minute(),
            )?;
            return Ok(When::Once { at_ms });
        }
        let t = self.at.read(cx).time();
        let at = t.hour() * 60 + t.minute();
        Ok(match self.kind {
            Repeat::Daily => When::Daily { at },
            Repeat::Weekdays => When::Weekdays { at },
            Repeat::Weekly => When::Weekly {
                days: (1..=7u8)
                    .filter(|d| self.days[usize::from(*d) - 1])
                    .collect(),
                at,
            },
            Repeat::Cron => When::Cron {
                expr: self.cron.read(cx).value().trim().to_string(),
            },
        })
    }

    /// What it would keep, checked as the server checks it. (`any_prompt`: as if one were
    /// typed, for the line under the form while it's empty.)
    fn specs(&self, now: i64, any_prompt: bool, cx: &App) -> Result<Vec<ScheduleSpec>, String> {
        let mut prompt = self.prompt.read(cx).value().trim().to_string();
        if any_prompt && prompt.is_empty() {
            prompt.push('…');
        }
        let when = self.when(cx)?;
        let specs: Vec<ScheduleSpec> = self
            .targets
            .iter()
            .enumerate()
            .map(|(i, target)| ScheduleSpec {
                id: self.ids[i].clone(),
                prompt: prompt.clone(),
                target: target.clone(),
                when: when.clone(),
                zone: self.zone.clone(),
            })
            .collect();
        for s in &specs {
            sched::check(s, now, self.was.as_ref())?;
        }
        Ok(specs)
    }

    /// The next time it would run, in words, or why it can't.
    fn next(&self, cx: &App) -> Result<String, String> {
        let now = flotilla_model::util::now_ms();
        let specs = self.specs(now, true, cx)?;
        let when = specs
            .first()
            .map(|s| s.when.clone())
            .ok_or("Nowhere to send it.")?;
        let first = match &when {
            When::Once { at_ms } => Some(*at_ms),
            w => sched::next_after(w, &self.zone, now)?,
        };
        let here = sched::here();
        let words = first
            .map(|ms| sched::when_words(ms, now, &here))
            .ok_or("That never comes round.")?;
        Ok(if when.once() {
            format!("Sends {words}")
        } else {
            format!("Next {words}")
        })
    }

    pub fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        match self.specs(flotilla_model::util::now_ms(), false, cx) {
            Ok(specs) => {
                self.saving = true;
                self.error = None;
                cx.emit(EditorEvent::Save(specs));
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    /// Dev: the form's controls, as the harness drives them.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_step(
        &mut self,
        step: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match step {
            "once" => self.set_repeat(false, cx),
            "repeat" => self.set_repeat(true, cx),
            "daily" | "weekdays" | "weekly" | "cron" => {
                self.repeat = true;
                self.kind = match step {
                    "daily" => Repeat::Daily,
                    "weekdays" => Repeat::Weekdays,
                    "weekly" => Repeat::Weekly,
                    _ => Repeat::Cron,
                };
                self.edited(cx);
            }
            s if s.starts_with("cron:") => {
                let expr = s["cron:".len()..].to_string();
                self.cron.update(cx, |c, cx| c.set_value(expr, window, cx));
                self.edited(cx);
            }
            s if s.starts_with("day:") => {
                if let Ok(d) = s["day:".len()..].parse::<usize>()
                    && (1..=7).contains(&d)
                {
                    self.days[d - 1] = !self.days[d - 1];
                    self.edited(cx);
                }
            }
            s if s.starts_with("prompt:") => {
                let text = s["prompt:".len()..].to_string();
                self.prompt
                    .update(cx, |p, cx| p.set_value(text, window, cx));
                self.edited(cx);
            }
            s if s.starts_with("at:") => {
                if let Some(m) = flotilla_model::schedule::parse_clock(&s["at:".len()..])
                    && let Some(t) = NaiveTime::from_hms_opt(m / 60, m % 60, 0)
                {
                    self.at.update(cx, |a, cx| a.set_time(t, window, cx));
                    self.edited(cx);
                }
            }
            "save" => self.save(cx),
            "cancel" => cx.emit(EditorEvent::Cancel),
            _ => eprintln!("[script] unknown later- step: {step}"),
        }
    }

    fn set_repeat(&mut self, repeat: bool, cx: &mut Context<Self>) {
        self.repeat = repeat;
        self.error = None;
        cx.notify();
    }

    fn render_once(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let zone = self.zone.clone();
        let picks = sched::quick_times(flotilla_model::util::now_ms(), &zone);
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(div().w(px(250.)).child(DatePicker::new(&self.once).small()))
            .children(picks.into_iter().enumerate().map(|(i, (label, ms))| {
                let zone = zone.clone();
                Button::new(SharedString::from(format!("quick-{i}")))
                    .ghost()
                    .small()
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(v) = naive(ms, &zone) {
                            this.once.update(cx, |o, cx| o.set_date_time(v, window, cx));
                        }
                        this.edited(cx);
                    }))
            }))
    }

    fn render_repeat(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let kind = self.kind;
        let kinds = ButtonGroup::new("repeat-kind")
            .outline()
            .small()
            .children(REPEATS.iter().map(|(k, label)| {
                segment(
                    Button::new(SharedString::from(format!("repeat-{label}"))).label(*label),
                    *k == kind,
                    &theme,
                )
            }))
            .on_click(cx.listener(|this, picked: &Vec<usize>, _, cx| {
                if let Some((k, _)) = picked.first().and_then(|&i| REPEATS.get(i)) {
                    this.kind = *k;
                    this.edited(cx);
                }
            }));
        let at = || {
            h_flex()
                .gap_2()
                .items_center()
                .child(div().text_sm().child("At"))
                .child(div().w(px(90.)).child(TimeField::new(&self.at).small()))
        };
        let detail = match kind {
            Repeat::Daily | Repeat::Weekdays => at().into_any_element(),
            Repeat::Weekly => h_flex()
                .gap_3()
                .items_center()
                .flex_wrap()
                .child(
                    h_flex()
                        .gap_1()
                        .children(DAY_LETTERS.iter().enumerate().map(|(i, d)| {
                            segment(
                                Button::new(SharedString::from(format!("day-{i}")))
                                    .outline()
                                    .xsmall()
                                    .label(*d),
                                self.days[i],
                                &theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.days[i] = !this.days[i];
                                    this.edited(cx);
                                },
                            ))
                        })),
                )
                .child(at())
                .into_any_element(),
            Repeat::Cron => div()
                .w(px(220.))
                .child(Input::new(&self.cron).small())
                .into_any_element(),
        };
        v_flex().gap_2().child(h_flex().child(kinds)).child(detail)
    }
}

/// A segment of a group, filled when it's the one picked.
fn segment(button: Button, selected: bool, theme: &gpui_kit::component::Theme) -> Button {
    button.selected(selected).when(selected, |b| {
        b.bg(theme.primary)
            .text_color(theme.primary_foreground)
            .border_color(theme.primary)
    })
}

impl Render for ScheduleEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let repeat = self.repeat;
        let next = self.next(cx);
        let here = sched::here();
        let has_text = !self.prompt.read(cx).value().trim().is_empty();
        let modes = ButtonGroup::new("once-or-repeat")
            .outline()
            .small()
            .child(segment(
                Button::new("mode-once").label("Once"),
                !repeat,
                &theme,
            ))
            .child(segment(
                Button::new("mode-repeat").label("Repeat"),
                repeat,
                &theme,
            ))
            .on_click(cx.listener(|this, picked: &Vec<usize>, _, cx| {
                if let Some(&i) = picked.first() {
                    this.set_repeat(i == 1, cx);
                }
            }));
        let (line, bad) = match (&self.error, &next) {
            (Some(e), _) => (e.clone(), true),
            (None, Ok(words)) => (words.clone(), false),
            (None, Err(e)) => (e.clone(), true),
        };
        v_flex()
            .gap_3()
            .child(Textarea::new(&self.prompt).disabled(self.saving))
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child("To"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(self.to.clone()),
                    ),
            )
            .child(h_flex().child(modes))
            .child(if repeat {
                self.render_repeat(cx).into_any_element()
            } else {
                self.render_once(cx).into_any_element()
            })
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(if bad {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(line)
                    // Set on a device in another zone: its times are that zone's.
                    .when(self.zone != here, |el| {
                        el.child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(format!("· times in {}", self.zone)),
                        )
                    }),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("schedule-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(EditorEvent::Cancel))),
                    )
                    .child(
                        Button::new("schedule-save")
                            .primary()
                            .small()
                            .label(if self.id.is_some() {
                                "Save"
                            } else {
                                "Schedule"
                            })
                            .loading(self.saving)
                            .disabled(self.saving || !has_text || next.is_err())
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}
