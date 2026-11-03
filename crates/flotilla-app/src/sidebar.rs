//! The thread list, grouped Unread, Idle, Working, Archived (Archived starts collapsed). Every row has the same lines: the title and a fixed-width time (which turns into
//! an archive button on hover); up to two lines of what the agent said last (while it works,
//! the latest prompt); then the agent's logo, model and VM state. Hovering a row shows a card
//! with the rest. Right-click a row for the thread's commands.

use crate::app::FlotillaApp;
use crate::assets::provider_logo;
use crate::fmt;
use crate::listing::Listed;
use crate::thread_menu::{self, ThreadCommand};
use flotilla_model::lifecycle::days_left;
use flotilla_model::util::plain_text;
use flotilla_proto::{Activity, ThreadState, ThreadSummary};
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Selectable as _, Sizable, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

const TIME_COLUMN: f32 = 44.;
/// A preview line's height, in rems (text_xs at GPUI's usual 1.618 line height, near enough).
const PREVIEW_LINE: f32 = 1.2;
/// The sidebar hidden (⌃⌘S): a rail just wide enough for its buttons.
const RAIL: f32 = 40.;

pub fn render(
    app: &mut FlotillaApp,
    window: &mut Window,
    cx: &mut Context<FlotillaApp>,
) -> AnyElement {
    let theme = cx.theme().clone();
    if app.sidebar_hidden {
        return rail(&theme, cx);
    }
    let width = app.sidebar_width_for(f32::from(window.viewport_size().width));
    let now = flotilla_model::util::now_ms();
    // Unread and Idle scroll at the top (Unread only when there's something unread). Working
    // and Archived sit at the bottom, always shown with a count; collapsed, they're just their
    // headers. Expired threads can't run again, so they live with the archived ones.
    // Headers and rows are the lists' own children, so a row can be scrolled into view.
    let listing = app.listing();
    let open_row = app.selected().map(|s| listing.row_for(s));
    let mut shown: [Vec<AnyElement>; 2] = [vec![], vec![]];
    let mut open_at = None;
    // Before the first list comes (the server not reached yet), no groups: their counts would
    // say there's nothing.
    let loaded = app.engine.is_some();
    for (list, group, label) in [
        (0, Activity::Unread, "Unread"),
        (0, Activity::Idle, "Idle"),
        (1, Activity::Working, "Working"),
        (1, Activity::Archived, "Archived"),
    ] {
        let rows = listing.group(group);
        // Unread and Idle only when they have rows; Working and Archived always, with a count.
        if (list == 0 && rows.is_empty()) || (list == 1 && !loaded) {
            continue;
        }
        let at = shown[list].len();
        let children = section(
            app,
            group,
            label,
            rows,
            open_row.as_deref(),
            now,
            &theme,
            cx,
        );
        if let Some(i) = rows
            .iter()
            .position(|r| Some(r.summary.thread.id.as_str()) == open_row.as_deref())
            .filter(|_| children.len() > 1)
        {
            open_at = Some((list, at + 1 + i));
        }
        shown[list].extend(children);
    }
    // A thread just opened: its row in view.
    if std::mem::take(&mut app.reveal)
        && let Some((list, i)) = open_at
    {
        app.sidebar_scroll[list].scroll_to_item(i);
    }
    let [top, bottom] = shown;
    let list = v_flex()
        .id("thread-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(&app.sidebar_scroll[0])
        .pb_2()
        .children(top);
    let pinned = v_flex()
        .id("thread-list-bottom")
        .flex_none()
        .max_h(px(360.))
        .overflow_y_scroll()
        .track_scroll(&app.sidebar_scroll[1])
        .pb_1()
        .border_t_1()
        .border_color(theme.sidebar_border)
        .children(bottom);

    let offline = matches!(
        app.connection,
        Some(flotilla_proto::Connection::Offline { .. })
    );
    let status: AnyElement =
        if let Some(flotilla_proto::Connection::Offline { error }) = app.connection.clone() {
            div()
                .id("reconnecting")
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(theme.muted_foreground)
                // Never reached yet this time: it isn't reconnecting.
                .child(if loaded {
                    "Reconnecting…"
                } else {
                    "Connecting…"
                })
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(error.clone()).build(window, cx)
                })
                .into_any_element()
        } else if let Some(err) = app.status.error.clone() {
            div()
                .id("sync-error")
                .flex_none()
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .xsmall()
                        .text_color(theme.warning),
                )
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(err.clone()).build(window, cx)
                })
                .into_any_element()
        } else if app.status.syncing {
            Spinner::new().xsmall().into_any_element()
        } else {
            div().into_any_element()
        };

    v_flex()
        .id("sidebar")
        .relative()
        .w(px(width))
        .flex_none()
        .h_full()
        .bg(theme.sidebar)
        // Rows held where they were (opened unread, a turn ended) go where they belong once
        // the pointer leaves, not under it.
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            this.sidebar_hovered = *hovered;
            if !*hovered {
                this.release_held(cx);
            }
        }))
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(
            h_flex()
                .justify_between()
                .gap_1p5()
                .pl(px(6.))
                .pr(px(12.))
                .pt(px(12.))
                .pb(px(6.))
                .child(
                    // At the narrowest, the tags give way to New.
                    h_flex()
                        .gap_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(
                            Button::new("hide-sidebar")
                                .ghost()
                                .small()
                                .icon(IconName::PanelLeftClose)
                                .tooltip("Hide the sidebar (⌃⌘S)")
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                        )
                        .child(div().font_semibold().child("Threads"))
                        .child(tag("Alpha", &theme))
                        // Beside the release app, which window is which (not in a demo recording).
                        .when(flotilla_model::flavor::is_dev() && !demo(), |row| {
                            row.child(tag("Dev", &theme))
                        }),
                )
                .child(
                    Button::new("new-thread")
                        .flex_none()
                        .small()
                        .outline()
                        .icon(IconName::Plus)
                        .label("New")
                        .tooltip("New thread (⌘N)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.new_thread_clicked(window, cx)),
                        ),
                ),
        )
        .children(project_filter(app, &theme, cx))
        .child(list)
        .when(loaded, |el| el.child(pinned))
        .child(
            h_flex()
                .gap_2()
                .px(px(12.))
                .py(px(8.))
                .border_t_1()
                .border_color(theme.sidebar_border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(status)
                .children(app.undo.as_ref().map(|u| {
                    let n = u.ids.len();
                    h_flex()
                        .flex_none()
                        .gap_1()
                        .child(format!(
                            "Archived {n} thread{}",
                            if n == 1 { "" } else { "s" }
                        ))
                        .child(
                            Button::new("undo-archive")
                                .ghost()
                                .xsmall()
                                .label(match (u.owns_cmd_z, cfg!(target_os = "macos")) {
                                    (true, true) => "Undo ⌘Z",
                                    (true, false) => "Undo Ctrl+Z",
                                    (false, _) => "Undo",
                                })
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.undo_archive(window, cx)
                                    }),
                                ),
                        )
                }))
                .child(div().flex_1())
                // The undo notice needs the room while it lasts; offline, the counts are stale.
                // Squeezed, they give way rather than Settings. A narrow sidebar whose agent
                // readout shows its share keeps one of boat's counts.
                .children(
                    (app.undo.is_none() && !offline)
                        .then(|| crate::meter::readout(app, width, &theme, cx))
                        .flatten(),
                )
                .children(
                    (app.undo.is_none() && !offline)
                        .then(|| {
                            let text = |s: &str| footer_text_width(window, s);
                            usage(app, counts_room(app, width, &text), &text, &theme, cx)
                        })
                        .flatten()
                        .map(|u| div().min_w_0().overflow_hidden().child(u)),
                )
                // Scheduled prompts, once there are any.
                .when(!app.schedules.is_empty(), |el| {
                    let n = app.schedules.len();
                    el.child(
                        div().flex_none().child(
                            Button::new("scheduled")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Clock)
                                .tooltip(if n == 1 {
                                    "Scheduled: 1 prompt".to_string()
                                } else {
                                    format!("Scheduled: {n} prompts")
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_scheduled(None, window, cx)
                                })),
                        ),
                    )
                })
                .child(
                    div().flex_none().child(
                        Button::new("settings")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Settings)
                            .tooltip("Settings (⌘,)")
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.settings_clicked(window, cx)
                                }),
                            ),
                    ),
                ),
        )
        // The edge to drag: wider than it looks, and lit while you're on it.
        .child(
            div()
                .id("sidebar-edge")
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(-3.))
                .w(px(6.))
                .cursor_col_resize()
                .hover(|s| s.bg(theme.ring.opacity(0.35)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, e: &MouseDownEvent, window, cx| {
                        this.start_sidebar_drag(
                            f32::from(e.position.x),
                            f32::from(window.viewport_size().width),
                        );
                        cx.stop_propagation();
                    }),
                ),
        )
        .into_any_element()
}

/// A count against its limit: amber from 80% of it, red at it.
fn tone(theme: &gpui_kit::component::Theme, (used, limit): (i64, i64)) -> Hsla {
    match limit {
        l if l > 0 && used >= l => theme.danger,
        l if l > 0 && used * 5 >= l * 4 => theme.warning,
        _ => theme.muted_foreground,
    }
}

/// How wide `text` lays out at the footer's size (`text_xs`).
fn footer_text_width(window: &Window, text: &str) -> f32 {
    let style = window.text_style();
    let size = window.rem_size() * 0.75;
    f32::from(
        window
            .text_system()
            .shape_line(
                SharedString::from(text.to_string()),
                size,
                &[style.to_run(text.len())],
                None,
            )
            .width,
    )
}

/// An `xsmall` icon button (Scheduled's clock, Settings): 20 points square.
const ICON_BUTTON: f32 = 20.;
/// The status's `xsmall` spinner or warning.
const STATUS: f32 = 12.;
/// The footer's gap between its children, and its padding at each end.
const FOOTER_GAP: f32 = 8.;
const FOOTER_PAD: f32 = 12.;

/// How wide boat's counts may be in a sidebar `width` wide: what the footer has left beside
/// its padding, the agents' readout, Scheduled's clock (once there are schedules) and Settings,
/// and the gaps between them (its status and the spacer before the readout take a gap each).
fn counts_room(app: &FlotillaApp, width: f32, text: &impl Fn(&str) -> f32) -> f32 {
    let readout = crate::meter::readout_width(app, width, text);
    let clock = !app.schedules.is_empty();
    // Status, spacer, the counts, Settings; the readout and the clock as they show.
    let children = 4 + usize::from(readout > 0.) + usize::from(clock);
    // The status's spinner, while it syncs, has its place kept: the counts don't come and go
    // with it. And a point to spare, for text measured a hair narrow.
    width
        - 2. * FOOTER_PAD
        - (children - 1) as f32 * FOOTER_GAP
        - STATUS
        - readout
        - if clock { ICON_BUTTON } else { 0. }
        - ICON_BUTTON
        - 1.
}

/// Beside Settings: VMs running of the most at once, and starts in the last hour, or in
/// whichever window (minute, 24 hours) is closer to its limit once it's three-quarters used.
/// The count that moves stands out; its limit and unit stay quiet. Where both don't fit in
/// `room` (as wide as they lay out: `text` measures), it keeps the count closest to its limit,
/// and then, still too narrow, leaves out its info icon, then its unit. Clicking it opens every
/// count.
fn usage(
    app: &FlotillaApp,
    room: f32,
    text: &impl Fn(&str) -> f32,
    theme: &gpui_kit::component::Theme,
    cx: &mut Context<FlotillaApp>,
) -> Option<AnyElement> {
    let u = app.usage.clone()?;
    let share = |(used, limit): (i64, i64)| {
        if limit > 0 {
            used as f64 / limit as f64
        } else {
            0.
        }
    };
    let mut stats: Vec<((i64, i64), &str)> = Vec::new();
    if let Some(v) = u.vms {
        stats.push((v, "VMs"));
    }
    if u.blocked.is_none() {
        let tight = [(u.minute, "starts/min"), (u.day, "starts/day")]
            .into_iter()
            .filter_map(|(w, unit)| w.map(|w| (w, unit)))
            .filter(|(w, _)| share(*w) >= 0.75 && u.hour.is_none_or(|h| share(*w) > share(h)))
            .max_by(|a, b| share(a.0).total_cmp(&share(b.0)));
        if let Some(stat) = tight.or(u.hour.map(|h| (h, "starts/h"))) {
            stats.push(stat);
        }
    }
    // How wide they lay out: the button's padding, each count's two runs of text with a gap
    // between counts, "Can't start VMs", and the info icon after a gap.
    let blocked = "Can't start VMs";
    let after = |limit: i64, unit: &str, units: bool| {
        if units {
            format!("/{limit} {unit}")
        } else {
            format!("/{limit}")
        }
    };
    let wide = |stats: &[((i64, i64), &str)], info: bool, units: bool| {
        let counts: f32 = stats
            .iter()
            .map(|((used, limit), unit)| {
                text(&used.to_string()) + text(&after(*limit, unit, units))
            })
            .sum();
        let items = stats.len() + usize::from(u.blocked.is_some());
        8. + counts
            + u.blocked.as_ref().map_or(0., |_| text(blocked))
            + 12. * items.saturating_sub(1) as f32
            + if info { 8. + 12. } else { 0. }
    };
    // What fits: both counts with the info icon, else the one closest to its limit (the later,
    // starts, on a tie) with it, then without it, then its numbers alone (its unit on hover).
    let one = stats
        .iter()
        .enumerate()
        .max_by(|a, b| share(a.1.0).total_cmp(&share(b.1.0)))
        .map(|(i, _)| vec![stats[i]])
        .unwrap_or_default();
    let (stats, info, units) = [
        (stats.clone(), true, true),
        (one.clone(), true, true),
        (one.clone(), false, true),
        (one, false, false),
    ]
    .into_iter()
    .find(|(s, info, units)| wide(s, *info, *units) <= room)
    .unwrap_or_else(|| (stats.iter().take(1).copied().collect(), false, false));
    let hover = (!units).then(|| {
        stats
            .iter()
            .map(|((used, limit), unit)| format!("{used}{}", after(*limit, unit, true)))
            .collect::<Vec<_>>()
            .join(" · ")
    });
    let muted = theme.muted_foreground;
    let readout = stats
        .into_iter()
        .fold(h_flex().gap_3(), |row, ((used, limit), unit)| {
            let color = match tone(theme, (used, limit)) {
                t if t == muted => theme.foreground,
                t => t,
            };
            row.child(
                h_flex()
                    .child(div().text_color(color).child(used.to_string()))
                    .child(div().text_color(muted).child(after(limit, unit, units))),
            )
        })
        .when(u.blocked.is_some(), |row| {
            row.child(div().text_color(theme.danger).child(blocked))
        });
    let card = UsageCard::of(app);
    let weak = cx.weak_entity();
    let close = weak.clone();
    Some(
        Popover::new("usage")
            .anchor(Anchor::BottomRight)
            .open(app.usage_open)
            .on_open_change(move |open, _, cx| {
                let _ = close.update(cx, |app, cx| {
                    app.usage_open = *open;
                    if *open {
                        app.usage_tab = crate::meter::UsageTab::Vms;
                    }
                    app.sync_agent_usage();
                    cx.notify();
                });
            })
            .trigger(
                Button::new("usage")
                    .ghost()
                    .xsmall()
                    .when_some(hover, |b, h| b.tooltip(h))
                    .child(h_flex().gap_2().text_xs().child(readout).when(info, |el| {
                        el.child(Icon::new(IconName::Info).xsmall().text_color(muted))
                    })),
            )
            .content(move |_, window, cx| {
                usage_card(
                    &card,
                    weak.clone(),
                    f32::from(window.viewport_size().height),
                    cx,
                )
            })
            .into_any_element(),
    )
}

/// What the usage popovers show: boat's counts, and the agents' (`crate::meter::agents_body`).
pub(crate) struct UsageCard {
    usage: Option<flotilla_proto::Usage>,
    refreshing: bool,
    error: Option<String>,
    /// Connected to a server whose engine runs: a read can be asked for.
    can_refresh: bool,
    agents: crate::meter::AgentsCard,
    tab: crate::meter::UsageTab,
}

impl UsageCard {
    pub(crate) fn of(app: &FlotillaApp) -> UsageCard {
        let online = !matches!(
            app.connection,
            Some(
                flotilla_proto::Connection::Offline { .. }
                    | flotilla_proto::Connection::Refused { .. }
            )
        );
        UsageCard {
            usage: app.usage.clone(),
            refreshing: app.usage_refreshing,
            error: app.usage_error.clone(),
            can_refresh: online && matches!(app.engine, Some(flotilla_proto::EngineState::Running)),
            agents: crate::meter::AgentsCard::of(app),
            tab: app.usage_tab,
        }
    }
}

/// The usage popover: "Usage" and when it was read, then boat's VMs or the agents, a tab each
/// once both have something to show.
/// `tall`: the window's height, which the agents' tab scrolls within.
pub(crate) fn usage_card(
    card: &UsageCard,
    app: WeakEntity<FlotillaApp>,
    tall: f32,
    cx: &mut App,
) -> AnyElement {
    use crate::meter::UsageTab;
    let theme = cx.theme().clone();
    let muted = theme.muted_foreground;
    let now = flotilla_model::util::now_ms();
    let agents = card.agents.any(now);
    let tab = match (&card.usage, agents) {
        (None, _) => UsageTab::Agents,
        (Some(_), false) => UsageTab::Vms,
        _ => card.tab,
    };
    let read = match tab {
        UsageTab::Vms => card.usage.as_ref().and_then(|u| u.read_ms),
        UsageTab::Agents => card.agents.read_ms(),
    };
    let both = card.usage.is_some() && agents;
    let pick = app.clone();
    let tabs = both.then(|| {
        ButtonGroup::new("usage-tabs")
            .outline()
            .xsmall()
            .children(
                [
                    ("usage-tab-vms", "VMs", UsageTab::Vms),
                    ("usage-tab-agents", "Agents", UsageTab::Agents),
                ]
                .map(|(id, label, t)| tab_segment(Button::new(id).label(label), t == tab, &theme)),
            )
            .on_click(move |picked: &Vec<usize>, _, cx| {
                let t = if picked.first() == Some(&1) {
                    UsageTab::Agents
                } else {
                    UsageTab::Vms
                };
                let _ = pick.update(cx, |app, cx| {
                    app.usage_tab = t;
                    cx.notify();
                });
            })
    });
    let body = match (&card.usage, tab) {
        (Some(u), UsageTab::Vms) => vms_body(card, u, app, cx),
        _ => crate::meter::agents_body(&card.agents, app, tall, cx),
    };
    v_flex()
        .w(px(if both || tab == UsageTab::Agents {
            312.
        } else {
            272.
        }))
        .gap_3()
        .text_xs()
        .child(
            h_flex()
                .justify_between()
                .child(div().text_sm().font_semibold().child("Usage"))
                .children(read.map(|ms| {
                    div()
                        .text_color(muted)
                        .child(format!("Read {}", fmt::ago_phrase(ms, now)))
                })),
        )
        .children(tabs.map(|t| h_flex().child(t)))
        .child(body)
        .into_any_element()
}

/// A tab's segment: the chosen one filled, as Settings' are.
fn tab_segment(button: Button, selected: bool, theme: &gpui_kit::component::Theme) -> Button {
    button.selected(selected).when(selected, |b| {
        b.bg(theme.primary)
            .text_color(theme.primary_foreground)
            .border_color(theme.primary)
    })
}

/// Every count, each with a meter: VMs, starts in each window (and when a full one frees
/// up), the plan's VM hours, extra starts; why boat won't start a VM. Refresh reads boat
/// again; prices and plans are on boat.dev.
fn vms_body(
    card: &UsageCard,
    u: &flotilla_proto::Usage,
    app: WeakEntity<FlotillaApp>,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let strong = |color: Hsla| {
        if color == muted {
            theme.foreground
        } else {
            color
        }
    };
    // A label, its count (in its tone when near or at the limit) and meter, and a line under
    // them when there's more to say.
    let row = |label: &str,
               value: String,
               pair: Option<(i64, i64)>,
               color: Hsla,
               note: Option<(String, Hsla)>| {
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
                            .child(label.to_string()),
                    )
                    .child(div().flex_none().text_color(strong(color)).child(value))
                    .child(match pair {
                        Some(pair) => meter(pair, color, theme.border),
                        None => div().flex_none().w(px(METER_WIDTH)),
                    }),
            )
            .children(note.map(|(text, color)| div().text_color(color).child(text)))
    };
    let mut rows = v_flex().gap(px(6.));
    if let Some(v) = u.vms {
        rows = rows.child(row(
            "VMs running",
            format!("{} of {}", v.0, v.1),
            Some(v),
            tone(theme, v),
            None,
        ));
    }
    let windows: Vec<((i64, i64), &str)> = [
        (u.minute, "Starts, last minute"),
        (u.hour, "Starts, last hour"),
        (u.day, "Starts, last 24 hours"),
    ]
    .into_iter()
    .filter_map(|(w, l)| w.map(|w| (w, l)))
    .collect();
    // When a full window frees up (until then) goes under the last of them.
    let now = flotilla_model::util::now_ms();
    let free = u
        .starts_free
        .as_deref()
        .filter(|at| {
            chrono::DateTime::parse_from_rfc3339(at).is_ok_and(|t| t.timestamp_millis() > now)
        })
        .filter(|_| {
            windows
                .iter()
                .any(|((used, limit), _)| *limit > 0 && used >= limit)
        })
        .and_then(fmt::clock)
        .map(|at| (format!("VMs can start again at {at}"), theme.danger));
    for (i, (w, label)) in windows.iter().enumerate() {
        let note = if i + 1 == windows.len() {
            free.clone()
        } else {
            None
        };
        rows = rows.child(row(
            label,
            format!("{} of {}", w.0, w.1),
            Some(*w),
            tone(theme, *w),
            note,
        ));
    }
    if let Some((left, of)) = u.vm_hours {
        // Used against the plan, as every row here is: the meter fills as they go.
        let used = (of - left, of);
        let renews = u
            .renews
            .as_deref()
            .and_then(fmt::date)
            .map(|d| (format!("Renews {d}"), muted));
        rows = rows.child(row(
            "VM hours used",
            format!("{} of {of}", used.0),
            Some(used),
            tone(theme, used),
            renews,
        ));
    }
    if let Some(x) = &u.extra_starts {
        // What's been used and spent this month shows whether they're on or off now.
        let spend = x
            .spend_cents
            .filter(|(spent, _)| x.on || *spent > 0 || x.cap_reached);
        let color = match spend {
            _ if x.cap_reached => theme.danger,
            Some(spend) => tone(theme, spend),
            None => muted,
        };
        let state = if x.on { "On" } else { "Off" };
        let value = if x.on || x.used > 0 {
            format!("{state} · {} used", x.used)
        } else {
            state.to_string()
        };
        let note = match (spend, x.cap_reached) {
            (Some((spent, cap)), true) => Some((
                format!(
                    "Cap reached: {} of {} this month",
                    dollars(spent),
                    dollars(cap)
                ),
                theme.danger,
            )),
            (None, true) => Some(("Cap reached this month".to_string(), theme.danger)),
            (Some((spent, cap)), false) => Some((
                format!("{} of {} this month", dollars(spent), dollars(cap)),
                muted,
            )),
            (None, false) => None,
        };
        rows = rows.child(row("Extra starts", value, spend, color, note));
    }
    let (refreshing, can_refresh) = (card.refreshing, card.can_refresh);
    v_flex()
        .gap_3()
        .children(u.blocked.clone().map(|why| {
            h_flex()
                .gap_1()
                .items_start()
                .text_color(theme.danger)
                .child(
                    div()
                        .flex_none()
                        .pt(px(1.))
                        .child(Icon::new(IconName::TriangleAlert).xsmall()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(format!("Can't start VMs: {why}")),
                )
        }))
        .child(rows)
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
                    Button::new("usage-refresh")
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
                            let _ = app.update(cx, |app, cx| app.refresh_usage(cx));
                        }),
                )
                .child(
                    Button::new("usage-billing")
                        .ghost()
                        .xsmall()
                        .child(
                            h_flex()
                                .gap_1()
                                .child("Billing on boat.dev")
                                .child(Icon::new(IconName::ArrowUpRight).xsmall()),
                        )
                        .on_click(|_, _, cx| cx.open_url(crate::settings::BILLING_URL)),
                ),
        )
        .into_any_element()
}

const CELLS: i64 = 10;
/// Ten 6px cells, 2px apart.
const METER_WIDTH: f32 = CELLS as f32 * 8. - 2.;

/// A count against its limit as ten soft pixel cells: any use lights one, and all ten only
/// at the limit.
pub(crate) fn meter(pair: (i64, i64), lit: Hsla, unlit: Hsla) -> Div {
    let on = lit_cells(pair);
    h_flex()
        .flex_none()
        .gap(px(2.))
        .children((0..CELLS).map(|i| crate::pixel::pip(if i < on { lit } else { unlit }, 6.)))
}

fn lit_cells((used, limit): (i64, i64)) -> i64 {
    match limit {
        l if l <= 0 || used <= 0 => 0,
        l if used >= l => CELLS,
        l => ((used * CELLS + l - 1) / l).clamp(1, CELLS - 1),
    }
}

/// Cents as dollars: "$80", "$0.29".
fn dollars(cents: i64) -> String {
    if cents % 100 == 0 {
        format!("${}", cents / 100)
    } else {
        format!("${}.{:02}", cents / 100, cents % 100)
    }
}

/// Dev (`snapshot` builds): `FLOTILLA_DEMO` records a dev build without its Dev tag.
#[cfg(feature = "snapshot")]
fn demo() -> bool {
    std::env::var_os("FLOTILLA_DEMO").is_some()
}

#[cfg(not(feature = "snapshot"))]
fn demo() -> bool {
    false
}

/// A small outlined tag beside the sidebar's title (Alpha, Dev).
fn tag(label: &'static str, theme: &gpui_kit::component::Theme) -> Div {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(label)
}

/// The hidden sidebar: Show, New thread and Settings, so the way back is always in view.
fn rail(theme: &gpui_kit::component::Theme, cx: &mut Context<FlotillaApp>) -> AnyElement {
    v_flex()
        .w(px(RAIL))
        .flex_none()
        .h_full()
        .items_center()
        .gap_1()
        .pt(px(12.))
        .pb(px(8.))
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(
            Button::new("show-sidebar")
                .ghost()
                .small()
                .icon(IconName::PanelLeftOpen)
                .tooltip("Show the sidebar (⌃⌘S)")
                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
        )
        .child(
            Button::new("rail-new-thread")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .tooltip("New thread (⌘N)")
                .on_click(cx.listener(|this, _, window, cx| this.new_thread_clicked(window, cx))),
        )
        .child(div().flex_1())
        .child(
            Button::new("rail-settings")
                .ghost()
                .xsmall()
                .icon(IconName::Settings)
                .tooltip("Settings (⌘,)")
                .on_click(cx.listener(|this, _, window, cx| this.settings_clicked(window, cx))),
        )
        .into_any_element()
}

/// A pull request's look: icon, colour and state word (open green, draft grey, merged
/// violet, closed red).
pub fn pr_look(
    pr: &flotilla_proto::PullRequest,
    theme: &gpui_kit::component::Theme,
) -> (IconName, Hsla, &'static str) {
    match pr.state.as_str() {
        "MERGED" => (IconName::GitMerge, theme.magenta, "Merged"),
        "CLOSED" => (IconName::GitPullRequestClosed, theme.danger, "Closed"),
        _ if pr.is_draft => (
            IconName::GitPullRequestDraft,
            theme.muted_foreground,
            "Draft",
        ),
        _ => (IconName::GitPullRequestArrow, theme.success, "Open"),
    }
}

/// `#12` in the PR's colour, with a dot for its checks while it's open.
fn pr_badge(pr: &flotilla_proto::PullRequest, theme: &gpui_kit::component::Theme) -> AnyElement {
    let (icon, color, _) = pr_look(pr, theme);
    let dot = (pr.state == "OPEN" && !pr.checks.is_empty()).then(|| {
        let c = if pr.failing() > 0 {
            theme.danger
        } else if pr.pending() > 0 {
            theme.warning
        } else {
            theme.success
        };
        crate::pixel::pip(c, 6.)
    });
    h_flex()
        .flex_none()
        .gap(px(3.))
        .text_color(color)
        .child(Icon::new(icon).size(px(11.)))
        .child(format!("#{}", pr.number))
        .when(
            pr.mergeable.as_deref() == Some("CONFLICTING") && pr.state == "OPEN",
            |el| {
                el.child(
                    Icon::new(IconName::TriangleAlert)
                        .size(px(10.))
                        .text_color(theme.danger),
                )
            },
        )
        .children(dot)
        .into_any_element()
}

/// With projects: All · each project · No project, to show one kind of thread.
/// Which projects' threads the list shows: one control, opening a menu of checkboxes (All,
/// each project, No project). Hidden when there are no projects.
fn project_filter(
    app: &FlotillaApp,
    theme: &gpui_kit::component::Theme,
    cx: &mut Context<FlotillaApp>,
) -> Option<AnyElement> {
    if app.projects_cache.is_empty() {
        return None;
    }
    let mut keys: Vec<(String, SharedString)> = app
        .projects_cache
        .iter()
        .map(|p| (p.id.clone(), SharedString::from(p.repo.clone())))
        .collect();
    keys.push((crate::app::NO_PROJECT.to_string(), "No project".into()));
    let hidden = app.hidden_projects.clone();
    let shown: Vec<&SharedString> = keys
        .iter()
        .filter(|(k, _)| !hidden.contains(k))
        .map(|(_, n)| n)
        .collect();
    let label: SharedString = match shown.len() {
        n if n == keys.len() => "All threads".into(),
        0 => "No threads".into(),
        1 => shown[0].clone(),
        n => format!("{n} projects").into(),
    };
    let filtered = shown.len() != keys.len();
    let weak = cx.weak_entity();
    let close = weak.clone();
    Some(
        h_flex()
            .px(px(8.))
            .pb(px(6.))
            .child(
                Popover::new("project-filter")
                    .anchor(Anchor::TopLeft)
                    .open(app.filter_open)
                    .on_open_change(move |open, _, cx| {
                        let _ = close.update(cx, |app, cx| {
                            app.filter_open = *open;
                            cx.notify();
                        });
                    })
                    .trigger(
                        Button::new("project-filter")
                            .ghost()
                            .small()
                            .icon(Icon::new(IconName::ListFilter).text_color(if filtered {
                                theme.ring
                            } else {
                                theme.muted_foreground
                            }))
                            .label(label)
                            .dropdown_caret(true),
                    )
                    .content(move |_, _, cx| {
                        let theme = cx.theme().clone();
                        let all = hidden.is_empty();
                        let toggle_all = weak.clone();
                        v_flex()
                            .w(px(240.))
                            .p_1()
                            .gap_1()
                            .child(
                                Checkbox::new("filter-all")
                                    .label("All")
                                    .checked(all)
                                    .on_click(move |checked: &bool, _, cx| {
                                        let shown = *checked;
                                        let _ = toggle_all.update(cx, |app, cx| {
                                            app.set_all_projects_shown(shown, cx)
                                        });
                                    }),
                            )
                            .child(div().h(px(1.)).my_1().bg(theme.border))
                            .children(keys.iter().enumerate().map(|(i, (key, name))| {
                                let (weak, key) = (weak.clone(), key.clone());
                                Checkbox::new(("filter-project", i))
                                    .label(name.clone())
                                    .checked(!hidden.contains(&key))
                                    .on_click(move |checked: &bool, _, cx| {
                                        let shown = *checked;
                                        let _ = weak.update(cx, |app, cx| {
                                            app.set_project_shown(&key, shown, cx)
                                        });
                                    })
                            }))
                    }),
            )
            .into_any_element(),
    )
}

/// The threads in the order the sidebar lists them, leaving out collapsed groups: what
/// ⌘1–⌘9 and ⌘⇧[ / ⌘⇧] move through.
pub fn visible_order(app: &FlotillaApp) -> Vec<String> {
    visible_rows(app).into_iter().map(|(id, _)| id).collect()
}

/// The rows the sidebar shows, in its order, with the group each shows under.
pub fn visible_rows(app: &FlotillaApp) -> Vec<(String, Activity)> {
    app.listing().visible(&app.collapsed)
}

/// After archiving `archived` (⌘⇧S), the thread to open: the first unread one, else the
/// next row in the sidebar, else the one before it. Never one under Archived. None: there's
/// nothing else to open.
pub fn next_after_archive(rows: &[(String, Activity)], archived: &str) -> Option<String> {
    let open = |(id, g): &&(String, Activity)| id != archived && *g != Activity::Archived;
    if let Some((id, _)) = rows
        .iter()
        .filter(open)
        .find(|(_, g)| *g == Activity::Unread)
    {
        return Some(id.clone());
    }
    let at = rows.iter().position(|(id, _)| id == archived);
    let (before, after) = match at {
        Some(i) => (&rows[..i], &rows[i + 1..]),
        None => (&rows[..0], rows),
    };
    after
        .iter()
        .find(open)
        .or_else(|| before.iter().rev().find(open))
        .map(|(id, _)| id.clone())
}

/// A collapsible group: its header (name and count) and, when open, its rows.
#[allow(clippy::too_many_arguments)]
fn section(
    app: &FlotillaApp,
    group: Activity,
    label: &'static str,
    rows: &[Listed],
    open_row: Option<&str>,
    now: i64,
    theme: &gpui_kit::component::Theme,
    cx: &mut Context<FlotillaApp>,
) -> Vec<AnyElement> {
    let open = !app.collapsed.contains(&group);
    let header = h_flex()
        .id(SharedString::from(format!("group-{label}")))
        .gap_1()
        .px(px(10.))
        .pt(px(10.))
        .pb(px(4.))
        .text_xs()
        .font_semibold()
        .text_color(theme.muted_foreground)
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(group, cx)))
        .child(
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall(),
        )
        .child(label.to_uppercase())
        .child(div().font_normal().child(rows.len().to_string()));
    let mut out = vec![header.into_any_element()];
    if open {
        for r in rows {
            let selected = open_row == Some(r.summary.thread.id.as_str());
            out.push(arriving(
                app,
                &r.summary.thread.id,
                row(app, r, selected, now, theme, cx).into_any_element(),
            ));
        }
    }
    out
}

/// `selected`: the open thread's row, or the row it's listed inside (a thread handed its task
/// by this one).
fn row(
    app: &FlotillaApp,
    r: &Listed,
    selected: bool,
    now: i64,
    theme: &gpui_kit::component::Theme,
    cx: &mut Context<FlotillaApp>,
) -> impl IntoElement {
    let s = r.summary;
    let t = &s.thread;
    let weak = cx.weak_entity();
    let menu_id = t.id.clone();
    let commands = thread_menu::commands(s, true);
    let archived = matches!(s.activity, Activity::Archived | Activity::Expired);
    let can_toggle_archive = s.activity != Activity::Expired;
    let (archive_cmd, archive_icon, archive_tip) = if archived {
        (
            ThreadCommand::Unarchive,
            IconName::ArchiveRestore,
            "Unarchive",
        )
    } else {
        (ThreadCommand::Archive, IconName::Archive, "Archive")
    };
    let archive_id = t.id.clone();
    let unread = s.activity == Activity::Unread;
    let working = s.activity == Activity::Working;
    let expired = s.activity == Activity::Expired;
    let time = match s.turn_started_ms {
        Some(start) => fmt::elapsed(start, now),
        None => fmt::ago(s.last_activity_ms, now),
    };
    // The model as the catalog names it ("Haiku 5.5"), as the composer does.
    let model = app
        .catalog
        .as_ref()
        .and_then(|c| c.provider(&t.provider))
        .and_then(|p| p.models.iter().find(|m| m.id == t.model))
        .and_then(|m| m.label.clone())
        .unwrap_or_else(|| t.model.clone());
    // Threads started from one prompt on several models share a title until theirs are
    // written: those rows say which model on the title's line.
    let twin = r.twin.then(|| model.clone());
    let meta = [
        Some(model),
        crate::fmt::effort(t.effort.as_deref()),
        t.fast.then(|| "fast".to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let project = t
        .repo_project_id
        .as_ref()
        .and_then(|id| app.projects_cache.iter().find(|p| &p.id == id));
    let days = days_left(t);
    // The VM: nothing while it's simply up or stopped (the header and hover card say which;
    // a second square next to the checks' would read as one), an amber pip while it starts or
    // stops, and words only when it needs you (a disk about to go, a start that failed).
    let (vm, vm_color, pip) = match t.state {
        // Its start failed after the VM was made (boat refused the first prompt, say).
        _ if s.start_failed && t.state != ThreadState::Expired => {
            ("not started".to_string(), theme.danger, None)
        }
        ThreadState::Creating | ThreadState::Starting | ThreadState::Stopping => {
            (String::new(), theme.muted_foreground, Some(theme.warning))
        }
        ThreadState::Idle | ThreadState::Running => (String::new(), theme.muted_foreground, None),
        ThreadState::Stopped => match days {
            Some(d) if d <= 3 => (format!("{}d left", d.max(0)), theme.warning, None),
            _ => (String::new(), theme.muted_foreground, None),
        },
        ThreadState::Expired => (String::new(), theme.muted_foreground, None),
        ThreadState::Error if t.sandbox_id.is_none() => {
            ("not started".to_string(), theme.danger, None)
        }
        ThreadState::Error => ("error".to_string(), theme.danger, None),
    };
    // Handed its task by another thread, that one's title when the sidebar has it (only an
    // archived row has one: see `Listed::parent`).
    let from = r.parent.map(|p| p.thread.title.clone());
    // What the agent said last, or while it works, what it was asked.
    // While it works, what it was last asked, and by whom: you, a thread it handed work to (its
    // answer), or the thread that handed it its task.
    let preview = if working {
        s.last_prompt.as_ref().map(|p| {
            match (
                flotilla_model::orchestrate::parse_answer(p),
                flotilla_model::orchestrate::task_of(p),
            ) {
                _ if p.starts_with("[flotilla] Agents answered") => {
                    match flotilla_model::side::parse_report(p)
                        .unwrap_or_default()
                        .as_slice()
                    {
                        [(title, _)] => format!("From {title}"),
                        members => format!("From {} agents", members.len()),
                    }
                }
                _ if let Some((title, _, why)) =
                    flotilla_model::orchestrate::parse_couldnt_start(p) =>
                {
                    format!("Couldn't start {title}: {why}")
                }
                _ if flotilla_model::orchestrate::parse_unreadable(p).is_some() => {
                    crate::thread_view::UNREADABLE_LINE.to_string()
                }
                (Some((title, _, _, answer)), _) => format!("From {title}: {}", plain_text(answer)),
                (None, Some(task)) => format!(
                    "From {}: {}",
                    from.clone().unwrap_or_else(|| "another thread".into()),
                    plain_text(task)
                ),
                (None, None) => format!("You: {}", plain_text(p)),
            }
        })
    } else {
        // Plain text already (the store's preview line, or the summariser's).
        s.last_reply.clone()
    };
    let card = HoverCard::new(s, preview.clone(), &meta, project, days, now, from);
    // A project thread's row shows its project (the branch and model are in the hover card).
    let meta = match project {
        Some(p) => p.repo.clone(),
        None => meta,
    };
    let id = t.id.clone();
    let (card_id, menu_row, left_row) = (t.id.clone(), t.id.clone(), t.id.clone());
    let clicked_row = t.id.clone();
    let app_handle = cx.weak_entity();
    h_flex()
        .id(ElementId::Name(SharedString::from(format!(
            "thread-{}",
            t.id
        ))))
        .group("thread-row")
        .items_start()
        .mx(px(6.))
        .pl(px(4.))
        .pr(px(8.))
        .py(px(7.))
        .gap(px(7.))
        .rounded(px(7.))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme.sidebar_accent))
        .when(!selected, |el| {
            el.hover(|el| el.bg(theme.sidebar_accent.opacity(0.5)))
        })
        // Double-click renames (the first click has already opened the thread).
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            if event.click_count() >= 2 && !expired {
                this.run_command(id.clone(), ThreadCommand::Rename, window, cx);
            } else {
                this.select(id.clone(), window, cx);
            }
        }))
        .tooltip(move |window, cx| {
            let suppressed = app_handle
                .upgrade()
                .is_some_and(|a| a.read(cx).card_suppressed.as_deref() == Some(card_id.as_str()));
            if suppressed {
                cx.new(|_| EmptyView).into()
            } else {
                card.build(window, cx)
            }
        })
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, _, _, _| this.card_suppressed = Some(menu_row.clone())),
        )
        // Nor after a click: the card would cover the thread just opened.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, _| this.card_suppressed = Some(clicked_row.clone())),
        )
        // Only hovering another row lets this one's card show again: the context menu covers
        // the row (its hover ends) while the card's delay is still running.
        .on_hover(cx.listener(move |this, hovered: &bool, _, _| {
            if *hovered
                && this
                    .card_suppressed
                    .as_deref()
                    .is_some_and(|s| s != left_row.as_str())
            {
                this.card_suppressed = None;
            }
        }))
        .tooltip_show_delay(HOVER_CARD_DELAY)
        // Unread: a dot in its own gutter, level with the title, so nothing else moves.
        .child(
            div()
                .w(px(8.))
                .h(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .when(unread, |el| el.child(crate::pixel::pip(theme.ring, 8.))),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    h_flex()
                        .gap_2()
                        // Handed its task by another thread.
                        .when(r.handed, |el| {
                            el.child(
                                Icon::new(IconName::CornerDownRight)
                                    .size(px(13.))
                                    .flex_none()
                                    .text_color(theme.muted_foreground),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(expired, |el| el.text_color(theme.muted_foreground))
                                .child(t.title.clone()),
                        )
                        .children(twin.map(|m| {
                            div()
                                .flex_none()
                                .max_w(px(96.))
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(m)
                        }))
                        .child(
                            // The time, or on hover an archive button in the same spot.
                            div()
                                .relative()
                                .min_w(px(TIME_COLUMN))
                                .flex_none()
                                .child(
                                    div()
                                        .text_right()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .text_color(if working {
                                            theme.success
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .when(can_toggle_archive, |el| {
                                            el.group_hover("thread-row", |s| s.invisible())
                                        })
                                        .child(time),
                                )
                                .when(can_toggle_archive, |el| {
                                    el.child(
                                        div()
                                            .absolute()
                                            .top(px(-3.))
                                            .right_0()
                                            .invisible()
                                            .group_hover("thread-row", |s| s.visible())
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "archive-{}",
                                                    t.id
                                                )))
                                                .ghost()
                                                .xsmall()
                                                .icon(archive_icon)
                                                .tooltip(archive_tip)
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        cx.stop_propagation();
                                                        this.run_command(
                                                            archive_id.clone(),
                                                            archive_cmd,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                            ),
                                    )
                                }),
                        ),
                )
                // Up to two lines, always two lines' room, so a preview growing or shrinking moves
                // no row; unread at full strength.
                .child(
                    div()
                        .text_xs()
                        .line_height(rems(PREVIEW_LINE))
                        .h(rems(PREVIEW_LINE * 2.))
                        .line_clamp(2)
                        .text_ellipsis()
                        .text_color(if unread {
                            theme.foreground
                        } else {
                            theme.foreground.opacity(0.7)
                        })
                        .children(preview),
                )
                .child(
                    h_flex()
                        .gap(px(5.))
                        .text_size(px(11.))
                        .text_color(theme.muted_foreground)
                        .child(logo(&t.provider, expired, theme))
                        .child(div().flex_1().min_w_0().truncate().child(meta))
                        .children(s.pr.as_ref().map(|(pr, _)| pr_badge(pr, theme)))
                        .children(pip.map(|c| {
                            let tip = if t.state == ThreadState::Stopping {
                                "VM stopping"
                            } else {
                                "VM starting"
                            };
                            div()
                                .id(SharedString::from(format!("pip-{}", t.id)))
                                .child(crate::pixel::pip(c, 6.))
                                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                        }))
                        .when(
                            vm.is_empty() && !working && app.waiting.contains(&t.id),
                            |el| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .text_right()
                                        .child("waiting on a hand-off"),
                                )
                            },
                        )
                        .when(!vm.is_empty(), |el| {
                            el.child(
                                div()
                                    .flex_none()
                                    .text_right()
                                    .text_color(vm_color)
                                    .child(vm),
                            )
                        }),
                ),
        )
        .context_menu(move |menu, _, _| {
            let (weak, id) = (weak.clone(), menu_id.clone());
            thread_menu::fill(
                menu,
                commands.clone(),
                std::rc::Rc::new(move |cmd, window, cx| {
                    let _ = weak.update(cx, |app, cx| app.run_command(id.clone(), cmd, window, cx));
                }),
            )
        })
}

/// The agent's logo, small, at the start of the model line.
fn logo(provider: &str, expired: bool, theme: &gpui_kit::component::Theme) -> AnyElement {
    let el = match provider_logo(provider, theme.mode.is_dark()) {
        Some(path) => img(path).size(px(12.)).flex_none().into_any_element(),
        None => div()
            .flex_none()
            .text_size(px(10.))
            .font_semibold()
            .child(fmt::monogram(provider))
            .into_any_element(),
    };
    div()
        .flex_none()
        .when(expired, |d| d.opacity(0.5))
        .child(el)
        .into_any_element()
}

/// How long a row that just landed in a group takes to fade and slide in (T3 Code's 150 ms
/// ease-out). Its travel is in whole points and short, so it lands in about 100 ms and then
/// holds still: a longer one crept its last point in sub-point steps, which read as the row
/// settling a point after it moved (round 3).
pub const ARRIVAL: std::time::Duration = std::time::Duration::from_millis(150);
const ARRIVAL_TRAVEL: f32 = 8.;

/// A row that just arrived in its group fades in from a little above its place.
fn arriving(app: &FlotillaApp, id: &str, row: AnyElement) -> AnyElement {
    match app.arrivals.get(id) {
        Some((seq, at)) if at.elapsed() < ARRIVAL * 2 => div()
            .child(row)
            .with_animation(
                ElementId::Name(SharedString::from(format!("arrive-{id}-{seq}"))),
                Animation::new(ARRIVAL).with_easing(|t: f32| 1. - (1. - t).powi(3)),
                |el, delta| {
                    el.opacity(delta)
                        .relative()
                        .top(px(-(ARRIVAL_TRAVEL * (1. - delta)).round()))
                },
            )
            .into_any_element(),
        _ => row,
    }
}

/// Waiting this long on a row before the card shows keeps it from flickering up while the
/// pointer passes over the list.
const HOVER_CARD_DELAY: std::time::Duration = std::time::Duration::from_millis(700);

/// Everything a row cuts short: the full title and preview, the agent, the VM's state in
/// words, and when the thread was last active and started.
#[derive(Clone)]
struct HoverCard {
    title: SharedString,
    preview: Option<SharedString>,
    facts: Vec<(&'static str, String)>,
}

impl HoverCard {
    fn new(
        s: &ThreadSummary,
        preview: Option<String>,
        meta: &str,
        project: Option<&flotilla_proto::Project>,
        days: Option<i64>,
        now: i64,
        from: Option<String>,
    ) -> HoverCard {
        let t = &s.thread;
        let vm = match t.state {
            ThreadState::Creating | ThreadState::Starting => "starting".to_string(),
            ThreadState::Idle | ThreadState::Running if t.keep_running => {
                "kept running".to_string()
            }
            ThreadState::Idle | ThreadState::Running => match t
                .archive_after
                .as_deref()
                .and_then(|at| fmt::minutes_until(at, now))
            {
                Some(m) if m > 0 => format!("{m} min left"),
                _ => "up".to_string(),
            },
            ThreadState::Stopping => "stopping".to_string(),
            ThreadState::Stopped => match days {
                Some(d) => format!("stopped · expires in {}d", d.max(0)),
                None => "stopped".to_string(),
            },
            ThreadState::Expired => "expired".to_string(),
            ThreadState::Error => "error".to_string(),
        };
        let mut facts = vec![
            (
                "Agent",
                format!("{} · {meta}", fmt::provider_name(&t.provider)),
            ),
            ("VM", vm),
        ];
        match project {
            Some(p) => {
                facts.push(("Project", p.full_name()));
                if let Some(b) = &t.branch {
                    facts.push(("Branch", b.clone()));
                }
                if let Some((pr, _)) = &s.pr {
                    // A merged or closed PR's checks are history.
                    let checks = match (pr.failing(), pr.pending(), pr.checks.len()) {
                        _ if pr.state != "OPEN" => String::new(),
                        (_, _, 0) => String::new(),
                        (f, _, _) if f > 0 => format!(" · {f} failing"),
                        (_, p, n) if p > 0 => format!(" · {}/{n} checks", n - p),
                        (_, _, _) => " · checks pass".to_string(),
                    };
                    let state = match pr.state.as_str() {
                        "MERGED" => "merged",
                        "CLOSED" => "closed",
                        _ if pr.is_draft => "draft",
                        _ => "open",
                    };
                    facts.push(("PR", format!("#{} {state}{checks}", pr.number)));
                }
            }
            None => facts.push(("Project", "No project".into())),
        }
        if let Some(parent) = from {
            facts.push(("From", parent));
        }
        facts.push(("Active", fmt::ago_phrase(s.last_activity_ms, now)));
        if let Some(started) = fmt::date_time(&t.created_at) {
            facts.push(("Started", started));
        }
        HoverCard {
            title: t.title.clone().into(),
            preview: preview.map(Into::into),
            facts,
        }
    }

    fn build(&self, window: &mut Window, cx: &mut App) -> AnyView {
        let card = self.clone();
        Tooltip::element(move |_, cx| {
            let muted = cx.theme().muted_foreground;
            v_flex()
                .w(px(260.))
                .gap_1()
                .py_1()
                .child(div().text_sm().font_semibold().child(card.title.clone()))
                .children(
                    card.preview
                        .clone()
                        .map(|p| div().text_xs().line_clamp(6).text_ellipsis().child(p)),
                )
                .child(
                    v_flex()
                        .gap(px(2.))
                        .pt_1()
                        .text_xs()
                        .children(card.facts.iter().map(|(label, value)| {
                            h_flex()
                                .gap_2()
                                .items_start()
                                .child(div().w(px(48.)).flex_none().text_color(muted).child(*label))
                                .child(div().flex_1().min_w_0().child(value.clone()))
                        })),
                )
        })
        .build(window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::{Activity, dollars, lit_cells, next_after_archive};

    #[test]
    fn meters_fill_in_whole_cells() {
        // Any use shows; all ten only at the limit.
        assert_eq!(
            [
                lit_cells((0, 12)),
                lit_cells((3, 100)),
                lit_cells((22, 60)),
                lit_cells((199, 200)),
                lit_cells((12, 12)),
                lit_cells((13, 12))
            ],
            [0, 1, 4, 9, 10, 10]
        );
        assert_eq!(lit_cells((5, 0)), 0);
        assert_eq!(
            (dollars(8000), dollars(29), dollars(1205)),
            ("$80".into(), "$0.29".into(), "$12.05".into())
        );
    }

    fn rows(spec: &[(&str, Activity)]) -> Vec<(String, Activity)> {
        spec.iter().map(|(id, g)| (id.to_string(), *g)).collect()
    }

    #[test]
    fn archiving_opens_the_first_unread_thread() {
        use Activity::*;
        let r = rows(&[
            ("u1", Unread),
            ("u2", Unread),
            ("i1", Idle),
            ("me", Idle),
            ("i2", Idle),
            ("w1", Working),
        ]);
        assert_eq!(next_after_archive(&r, "me").as_deref(), Some("u1"));
        // Archiving the first unread one: the next unread.
        assert_eq!(next_after_archive(&r, "u1").as_deref(), Some("u2"));
    }

    #[test]
    fn with_nothing_unread_it_opens_the_next_row_then_the_one_before() {
        use Activity::*;
        let r = rows(&[
            ("i1", Idle),
            ("me", Idle),
            ("w1", Working),
            ("a1", Archived),
        ]);
        assert_eq!(next_after_archive(&r, "me").as_deref(), Some("w1"));
        // The last before Archived: the one above it.
        let r = rows(&[
            ("i1", Idle),
            ("i2", Idle),
            ("me", Idle),
            ("a1", Archived),
            ("a2", Archived),
        ]);
        assert_eq!(next_after_archive(&r, "me").as_deref(), Some("i2"));
        // Only archived threads left: nothing (New thread opens).
        let r = rows(&[("me", Idle), ("a1", Archived)]);
        assert_eq!(next_after_archive(&r, "me"), None);
        // The open thread isn't listed (its group folded): the first one shown.
        let r = rows(&[("i1", Idle), ("w1", Working)]);
        assert_eq!(next_after_archive(&r, "me").as_deref(), Some("i1"));
    }
}
