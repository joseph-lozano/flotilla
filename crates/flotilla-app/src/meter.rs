//! What the agents' own records say about their use (`flotilla_proto::limits`): a thread's
//! context as a ring of pips by the composer, its exact numbers on hover; and the agent
//! accounts' limits, a readout beside boat's counts that opens each window. Nothing shows for
//! a number the harness hasn't given.

use crate::app::FlotillaApp;
use crate::fmt;
use flotilla_model::limits::{self as lim, Tone};
use flotilla_model::spend::{self as sp, Summary};
use flotilla_proto::{AccountLimits, AgentUsage, ContextUse, LimitWindow, UsageCost};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Sizable, StyledExt as _, Theme, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// The ring's box, radius and stroke.
const RING_BOX: f32 = 18.;
const RING_RADIUS: f32 = 6.5;
const RING_STROKE: f32 = 2.5;

fn color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Plain => theme.muted_foreground,
        Tone::Warn => theme.warning,
        Tone::Bad => theme.danger,
    }
}

/// A ring filling clockwise from the top as the share grows, over a track for the rest (as
/// T3's context meter).
pub fn ring(percent: f64, lit: Hsla, unlit: Hsla) -> impl IntoElement {
    let share = (percent / 100.).clamp(0., 1.) as f32;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let c = bounds.center();
            let r = px(RING_RADIUS);
            let at = |turn: f32| {
                let a = std::f32::consts::TAU * turn - std::f32::consts::FRAC_PI_2;
                point(c.x + r * a.cos(), c.y + r * a.sin())
            };
            // A whole circle is two half arcs.
            let arc = |from: f32, to: f32| {
                let mut b = PathBuilder::stroke(px(RING_STROKE));
                b.move_to(at(from));
                let mid = (from + to) / 2.;
                b.arc_to(point(r, r), px(0.), false, true, at(mid));
                b.arc_to(point(r, r), px(0.), false, true, at(to));
                b.build().ok()
            };
            if let Some(track) = arc(share, 1.) {
                window.paint_path(track, unlit);
            }
            if share > 0.
                && let Some(used) = arc(0., share)
            {
                window.paint_path(used, lit);
            }
        },
    )
    .flex_none()
    .size(px(RING_BOX))
}

/// The composer's context meter: the ring once the window is known (the count alone before),
/// and on hover the numbers in full. Returns None when nothing's known yet.
pub fn context(id: &str, c: Option<&ContextUse>, cx: &App) -> Option<AnyElement> {
    let c = c?.clone();
    let theme = cx.theme();
    let percent = c.percent();
    let tone = percent.map(lim::context_tone).unwrap_or(Tone::Plain);
    let face = match percent {
        Some(p) => ring(p, color(theme, tone), theme.border).into_any_element(),
        None => div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(lim::tokens(c.used))
            .into_any_element(),
    };
    Some(
        div()
            .id(SharedString::from(format!("context-{id}")))
            .flex_none()
            .px_1()
            .py(px(5.))
            .child(face)
            .tooltip(move |window, cx| {
                let c = c.clone();
                Tooltip::element(move |_, cx| context_card(&c, cx)).build(window, cx)
            })
            .into_any_element(),
    )
}

/// "Context window  2.4% · 24k / 1m", then "23,767 of 1,000,000 tokens".
fn context_card(c: &ContextUse, cx: &App) -> Div {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let head = match (c.percent(), c.window) {
        (Some(p), Some(w)) => format!(
            "{} · {} / {}",
            lim::percent(p),
            lim::tokens(c.used),
            lim::tokens(w)
        ),
        _ => lim::tokens(c.used),
    };
    let full = match c.window {
        Some(w) => format!("{} of {} tokens", lim::exact(c.used), lim::exact(w)),
        None => format!("{} tokens", lim::exact(c.used)),
    };
    v_flex()
        .gap_1()
        .py_1()
        .text_xs()
        .child(
            h_flex()
                .gap_4()
                .justify_between()
                .child(div().font_semibold().child("Context window"))
                .child(div().text_color(muted).child(head)),
        )
        .child(div().text_color(muted).child(full))
}

/// "Claude limit reached · resets 3:00 PM": the thread's account is used up, so a turn
/// would fail until the window resets.
pub fn reached(all: &[AccountLimits], c: Option<&ContextUse>, cx: &App) -> Option<AnyElement> {
    let now = flotilla_model::util::now_ms();
    let (l, w) = lim::reached(all, c?, now)?;
    let when = w
        .resets_at
        .as_deref()
        .and_then(|at| fmt::reset_at(at, now))
        .map(|t| format!(" · resets {t}"))
        .unwrap_or_default();
    Some(
        div()
            .text_xs()
            .text_color(cx.theme().danger)
            .child(format!("{} limit reached{when}", l.name()))
            .into_any_element(),
    )
}

/// Beside boat's counts: the agent window closest to its limit, as its account's logo and its
/// share (amber from 80%, red when used up; see `shows_share`), with the account's name as
/// the sidebar has room; it opens every account's windows. None until a VM has told them.
pub fn readout(
    app: &FlotillaApp,
    width: f32,
    theme: &Theme,
    cx: &mut Context<FlotillaApp>,
) -> Option<AnyElement> {
    let now = flotilla_model::util::now_ms();
    let (l, w) = lim::tightest(&app.limits, now)?;
    let tone = lim::limit_tone(w.used);
    let strong = if tone == Tone::Plain {
        theme.foreground
    } else {
        color(theme, tone)
    };
    // The account's logo (not a gauge: boat's counts are beside it) and its share, in its tone,
    // and its name too once there's room.
    let logo = match lim::logo_of(&l.account)
        .and_then(|p| crate::assets::provider_logo(p, theme.mode.is_dark()))
    {
        Some(path) => img(path).size(px(12.)).flex_none().into_any_element(),
        None => Icon::new(IconName::Gauge)
            .xsmall()
            .text_color(theme.muted_foreground)
            .into_any_element(),
    };
    let label = h_flex()
        .gap(px(3.))
        .text_xs()
        .child(logo)
        .when(width >= 380., |el| {
            el.child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(l.name().to_string()),
            )
        })
        .when(shows_share(app, width), |el| {
            el.child(div().text_color(strong).child(format!("{}%", w.used)))
        });
    let card = crate::sidebar::UsageCard::of(app);
    let weak = cx.weak_entity();
    let close = weak.clone();
    Some(
        Popover::new("limits")
            .anchor(Anchor::BottomRight)
            .open(app.limits_open)
            .on_open_change(move |open, _, cx| {
                let _ = close.update(cx, |app, cx| {
                    app.limits_open = *open;
                    if *open {
                        app.usage_tab = UsageTab::Agents;
                    }
                    app.sync_agent_usage();
                    cx.notify();
                });
            })
            .trigger(
                Button::new("limits")
                    .ghost()
                    .xsmall()
                    .compact()
                    .tooltip(format!(
                        "{} {}: {}% used",
                        l.name(),
                        w.label.to_lowercase(),
                        w.used
                    ))
                    .child(label),
            )
            .content(move |_, window, cx| {
                crate::sidebar::usage_card(
                    &card,
                    weak.clone(),
                    f32::from(window.viewport_size().height),
                    cx,
                )
            })
            .into_any_element(),
    )
}

/// How wide [`readout`] lays out (its button's padding, the logo, and the name and share it
/// shows at `width`), 0 with none; `text` says how wide a string of the footer's is.
pub fn readout_width(app: &FlotillaApp, width: f32, text: impl Fn(&str) -> f32) -> f32 {
    let Some((l, w)) = lim::tightest(&app.limits, flotilla_model::util::now_ms()) else {
        return 0.;
    };
    let mut wide = 8. + 12.;
    if width >= 380. {
        wide += 3. + text(l.name());
    }
    if shows_share(app, width) {
        wide += 3. + text(&format!("{}%", w.used));
    }
    wide
}

/// Whether the readout shows its share: where the sidebar has room beside boat's counts, and
/// at any width once the window is 80% used (boat's counts then keep one, to make room).
/// Below that, a narrow sidebar's readout is the logo alone, its share on hover.
pub fn shows_share(app: &FlotillaApp, width: f32) -> bool {
    lim::tightest(&app.limits, flotilla_model::util::now_ms())
        .is_some_and(|(_, w)| width >= ROOMY || lim::limit_tone(w.used) != Tone::Plain)
}

/// A sidebar this wide shows the readout's share (boat's counts beside it keep what fits).
pub const ROOMY: f32 = 330.;

/// "Weekly  92%  [meter]", and under it when it resets.
fn window_row(w: &LimitWindow, now: i64, theme: &Theme) -> Div {
    let muted = theme.muted_foreground;
    let tone = lim::limit_tone(w.used);
    let lit = color(theme, tone);
    let resets = lim::resets_ms(w)
        .zip(w.resets_at.as_deref().and_then(|at| fmt::reset_at(at, now)))
        .map(|(ms, at)| format!("Resets {at} · in {}", lim::until(ms, now)));
    v_flex()
        .gap(px(2.))
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(muted)
                        .child(w.label.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(if tone == Tone::Plain {
                            theme.foreground
                        } else {
                            lit
                        })
                        .child(format!("{}% used", w.used)),
                )
                .child(crate::sidebar::meter(
                    (w.used as i64, 100),
                    lit,
                    theme.border,
                )),
        )
        .children(resets.map(|r| {
            div()
                .text_color(if w.reached() { theme.danger } else { muted })
                .child(r)
        }))
}

/// A plan as its service names it ("pro" → "Pro").
fn plan_name(plan: &str) -> String {
    let mut c = plan.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The usage popovers' tabs: boat's VMs, or the agents (what they spent and their limits).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UsageTab {
    #[default]
    Vms,
    Agents,
}

/// What the Agents tab shows, cloned from the app as it renders.
#[derive(Clone)]
pub struct AgentsCard {
    usage: Option<AgentUsage>,
    limits: Vec<AccountLimits>,
    selected: Option<(String, String)>,
    refreshing: bool,
    error: Option<String>,
    can_refresh: bool,
}

impl AgentsCard {
    pub fn of(app: &FlotillaApp) -> AgentsCard {
        let online = !matches!(
            app.connection,
            Some(
                flotilla_proto::Connection::Offline { .. }
                    | flotilla_proto::Connection::Refused { .. }
            )
        );
        AgentsCard {
            usage: app.agent_usage.clone(),
            limits: app.limits.clone(),
            selected: app.agent_model.clone(),
            refreshing: app.agents_refreshing,
            error: app.agents_error.clone(),
            can_refresh: online && matches!(app.engine, Some(flotilla_proto::EngineState::Running)),
        }
    }

    fn summary(&self, only: Option<(&str, &str)>) -> Summary {
        let Some(u) = &self.usage else {
            return Summary::default();
        };
        let (today, day_of) = local_days();
        sp::summarize(u, today, day_of, only)
    }

    /// Anything to show: some use in the last 30 days, or a weekly limit.
    pub fn any(&self, now: i64) -> bool {
        self.summary(None).any() || self.limits.iter().any(|l| !lim::windows(l, now).is_empty())
    }

    /// When the VMs were last read for it.
    pub fn read_ms(&self) -> Option<i64> {
        self.usage
            .as_ref()
            .and_then(|u| u.read_ms)
            .or_else(|| self.limits.iter().map(|l| l.read_ms).max())
    }
}

/// Days as this computer counts them: today's number, and a quarter hour's (ms) day.
fn local_days() -> (i64, impl Fn(i64) -> i64) {
    use chrono::{Datelike, Local, TimeZone};
    let day = |ms: i64| {
        Local
            .timestamp_millis_opt(ms)
            .single()
            .map_or(ms.div_euclid(86_400_000), |d| {
                d.date_naive().num_days_from_ce() as i64
            })
    };
    (day(flotilla_model::util::now_ms()), day)
}

/// A day's short name ("Oct 3").
fn day_name(day: i64) -> String {
    chrono::NaiveDate::from_num_days_from_ce_opt(day as i32)
        .map(|d| d.format("%b %-d").to_string())
        .unwrap_or_default()
}

/// The kinds of token in a cost, each in its ink (as T3 mixes the foreground into the ground).
fn kinds(c: &UsageCost) -> [(&'static str, f64, f32); 5] {
    [
        ("Input", c.input, 0.6),
        ("Cache read", c.cache_read, 0.3),
        ("Cache write", c.cache_write, 0.72),
        ("Output", c.output, 1.),
        ("Other", c.other, 0.44),
    ]
}

/// The Agents tab, as T3 Code's Usage: the last 30 days' estimated cost at API prices (the
/// tokens alone while no cost is known), what the cache saved, the cost by kind and speed, a
/// trend by day, each model (picking one shows its trend and its rate), each account's weekly
/// limits; Refresh, and each provider's own usage page.
/// It scrolls past what a window `tall` points high leaves it.
pub fn agents_body(
    card: &AgentsCard,
    app: WeakEntity<FlotillaApp>,
    tall: f32,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme().clone();
    let muted = theme.muted_foreground;
    let now = flotilla_model::util::now_ms();
    let all = card.summary(None);
    let selected = card
        .selected
        .clone()
        .filter(|(p, m)| all.models.iter().any(|x| &x.provider == p && &x.model == m));
    let mut body = v_flex().gap_4();
    // Each account's weekly limits: what's left now, first (T3's Usage opens on them).
    let accounts: Vec<&AccountLimits> = card
        .limits
        .iter()
        .filter(|l| !lim::windows(l, now).is_empty())
        .collect();
    if !accounts.is_empty() {
        body = body.child(
            v_flex()
                .gap_2()
                .child(div().font_semibold().child("Limits"))
                .children(accounts.iter().map(|l| {
                    let name = match &l.plan {
                        Some(plan) => format!("{} · {}", l.name(), plan_name(plan)),
                        None => l.name().to_string(),
                    };
                    v_flex()
                        .gap(px(6.))
                        .child(div().text_color(muted).child(name))
                        .children(
                            lim::windows(l, now)
                                .into_iter()
                                .map(|w| window_row(w, now, &theme)),
                        )
                })),
        );
    }
    if all.any() {
        let mut spent = v_flex()
            .gap_3()
            .child(div().font_semibold().child("Last 30 days"));
        let cost = all.cost;
        // The whole, then what it's made of.
        let head = match cost {
            Some(c) => h_flex()
                .items_baseline()
                .gap_2()
                .child(div().text_xl().font_semibold().child(sp::usd(c.total())))
                .child(div().text_color(muted).child("API estimate")),
            None => h_flex().child(
                div()
                    .text_xl()
                    .font_semibold()
                    .child(format!("{} tokens", sp::tokens(all.tokens.total()))),
            ),
        };
        let mut line = Vec::new();
        if cost.is_some() {
            line.push(format!("{} tokens", sp::tokens(all.tokens.total())));
        }
        if let Some(c) = cost.filter(|c| c.saved > 0.) {
            line.push(format!("{} saved by the cache", sp::usd(c.saved)));
        }
        let mut top = v_flex().gap_1().child(head).when(!line.is_empty(), |el| {
            el.child(div().text_color(muted).child(line.join(" · ")))
        });
        if let Some(c) = cost.filter(|c| c.total() > 0.) {
            let total = c.total();
            let segments: Vec<(&str, f64, f32)> = kinds(&c)
                .into_iter()
                .filter(|(_, v, _)| *v / total >= 0.0005)
                .collect();
            top = top
                .child(
                    h_flex()
                        .mt_1()
                        .h(px(6.))
                        .w_full()
                        .rounded(px(3.))
                        .overflow_hidden()
                        .gap(px(1.))
                        .children(segments.iter().map(|(_, v, ink)| {
                            div()
                                .h_full()
                                .w(relative((*v / total) as f32))
                                .bg(theme.foreground.opacity(*ink))
                        })),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_x_3()
                        .gap_y(px(2.))
                        .children(segments.iter().map(|(label, v, ink)| {
                            h_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .size(px(6.))
                                        .rounded(px(1.))
                                        .bg(theme.foreground.opacity(*ink)),
                                )
                                .child(div().text_color(muted).child(*label))
                                .child(div().child(sp::usd(*v)))
                        })),
                )
                .when(all.fast > 0., |el| {
                    let premium = c
                        .premium
                        .map(|p| format!(" · Premium {}", sp::usd(p)))
                        .unwrap_or_default();
                    el.child(
                        div()
                            .text_color(muted)
                            .child(format!("Fast {}{premium}", sp::usd(all.fast))),
                    )
                });
        }
        spent = spent.child(top);
        // The trend: the model picked, or all of them.
        let shown = match &selected {
            Some((p, m)) => card.summary(Some((p.as_str(), m.as_str()))),
            None => all.clone(),
        };
        let by_cost = shown.cost.is_some();
        let value = |d: &sp::DaySum| if by_cost { d.cost } else { d.tokens as f64 };
        let top_day = shown.days.iter().map(value).fold(0., f64::max);
        let first = shown.days.first().map(|d| d.day).unwrap_or_default();
        let caption = match &selected {
            Some((_, m)) => format!("{m} by day"),
            None => "By day".to_string(),
        };
        spent =
            spent.child(
                v_flex()
                    .gap_1()
                    .child(h_flex().items_end().h(px(32.)).gap(px(2.)).children(
                        shown.days.iter().map(|d| {
                            let share = if top_day > 0. {
                                (value(d) / top_day) as f32
                            } else {
                                0.
                            };
                            let bar = div().flex_1().rounded_t(px(1.));
                            if share > 0. {
                                bar.h(relative(share.max(0.04)))
                                    .bg(theme.foreground.opacity(0.45))
                            } else {
                                bar.h(px(1.)).bg(theme.border)
                            }
                        }),
                    ))
                    .child(
                        h_flex()
                            .justify_between()
                            .text_color(muted)
                            .child(caption)
                            .child(format!("{} – today", day_name(first))),
                    ),
            );
        // Each model: its cost (none without a price), its tokens. A click shows its trend.
        let rows = all.models.iter().map(|m| {
            let key = (m.provider.clone(), m.model.clone());
            let on = selected.as_ref() == Some(&key);
            let logo = crate::assets::provider_logo(&m.provider, theme.mode.is_dark());
            let pick = app.clone();
            let detail = on.then(|| {
                let mut parts = Vec::new();
                if let Some(r) = m.per_million() {
                    parts.push(format!("{} per 1M tokens", sp::usd(r)));
                }
                if let Some(hit) = sp::cache_hit(&m.tokens) {
                    parts.push(format!("{} cache hit", lim::percent(hit * 100.)));
                }
                parts.join(" · ")
            });
            v_flex()
                .child(
                    h_flex()
                        .id(SharedString::from(format!(
                            "agent-model-{}-{}",
                            m.provider, m.model
                        )))
                        .gap_2()
                        .px_1()
                        .py(px(3.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(on, |el| el.bg(theme.accent))
                        .hover(|s| s.bg(theme.accent.opacity(0.6)))
                        .child(match logo {
                            Some(path) => img(path).size(px(12.)).flex_none().into_any_element(),
                            None => div().size(px(12.)).flex_none().into_any_element(),
                        })
                        .child(div().flex_1().min_w_0().truncate().child(m.model.clone()))
                        .child(
                            div()
                                .flex_none()
                                .child(m.cost.map(|c| sp::usd(c.total())).unwrap_or_default()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(44.))
                                .text_right()
                                .text_color(muted)
                                .child(sp::tokens(m.tokens.total())),
                        )
                        .on_click(move |_, _, cx| {
                            let key = key.clone();
                            let _ = pick.update(cx, |app, cx| {
                                app.agent_model = if app.agent_model.as_ref() == Some(&key) {
                                    None
                                } else {
                                    Some(key)
                                };
                                cx.notify();
                            });
                        }),
                )
                .children(
                    detail
                        .filter(|d| !d.is_empty())
                        .map(|d| div().pl(px(24.)).pt(px(2.)).text_color(muted).child(d)),
                )
        });
        spent = spent.child(v_flex().gap(px(2.)).children(rows));
        body = body.child(spent);
    }
    // The way out to each provider's own usage page: the accounts seen here.
    let mut pages: Vec<&'static str> = card
        .limits
        .iter()
        .map(|l| l.account.as_str())
        .chain(
            all.models
                .iter()
                .filter_map(|m| sp::account_of(&m.provider)),
        )
        .filter_map(|a| match a {
            "claude" => Some("claude"),
            "chatgpt" => Some("chatgpt"),
            _ => None,
        })
        .collect();
    pages.sort();
    pages.dedup();
    let (refreshing, can_refresh) = (card.refreshing, card.can_refresh);
    let refresh = app.clone();
    v_flex()
        .gap_3()
        .child(
            div()
                .id("agents-scroll")
                .max_h(px((tall - 210.).max(240.)))
                .overflow_y_scroll()
                .child(body),
        )
        .children(card.error.clone().map(|e| {
            div()
                .text_color(theme.danger)
                .child(format!("Couldn't refresh: {e}"))
        }))
        .child(
            h_flex()
                .justify_between()
                .pt_2()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    Button::new("agents-refresh")
                        .ghost()
                        .xsmall()
                        .icon(IconName::RefreshCw)
                        .label(if refreshing {
                            "Refreshing…"
                        } else {
                            "Refresh"
                        })
                        .loading(refreshing)
                        .disabled(!can_refresh || refreshing)
                        .on_click(move |_, _, cx| {
                            let _ = refresh.update(cx, |app, cx| app.refresh_agents(cx));
                        }),
                )
                .child(h_flex().children(pages.into_iter().filter_map(|a| {
                    let url = flotilla_proto::spend::usage_page(a)?;
                    let site = if a == "claude" {
                        "claude.ai"
                    } else {
                        "chatgpt.com"
                    };
                    Some(
                        Button::new(SharedString::from(format!("usage-page-{a}")))
                            .ghost()
                            .xsmall()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(site)
                                    .child(Icon::new(IconName::ArrowUpRight).xsmall()),
                            )
                            .on_click(move |_, _, cx| cx.open_url(url)),
                    )
                }))),
        )
        .into_any_element()
}
