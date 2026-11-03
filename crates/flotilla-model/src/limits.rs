//! How the clients show a thread's context and the agent accounts' limits
//! (`flotilla_proto::limits`): token counts as T3 writes them, the meter's cells and tone,
//! and when a window resets.

use flotilla_proto::{AccountLimits, ContextUse, LimitWindow};

/// A count of tokens, short: 950, 9.5k, 22k, 1.2m (as T3 writes them).
pub fn tokens(n: u64) -> String {
    let short = |v: f64, unit: &str| {
        let s = format!("{v:.1}");
        format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
    };
    match n {
        n if n < 1_000 => n.to_string(),
        n if n < 10_000 => short(n as f64 / 1_000., "k"),
        n if n < 999_500 => format!("{}k", (n as f64 / 1_000.).round() as u64),
        n => short(n as f64 / 1_000_000., "m"),
    }
}

/// A count in full: 21,909.
pub fn exact(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A share as T3 writes it: one decimal under 10% (2.4%), whole above (42%).
pub fn percent(p: f64) -> String {
    if p < 10. {
        let s = format!("{:.1}", p.max(0.));
        format!("{}%", s.strip_suffix(".0").unwrap_or(&s))
    } else {
        format!("{}%", p.round() as u64)
    }
}

/// How full a meter looks: plain, nearly full or full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Warn,
    Bad,
}

/// A context's tone: amber from 80% of the window, red from 90% (the harness compacts near
/// the end of it).
pub fn context_tone(percent: f64) -> Tone {
    match percent {
        p if p >= 90. => Tone::Bad,
        p if p >= 80. => Tone::Warn,
        _ => Tone::Plain,
    }
}

/// A limit's tone: amber from 80% used, red when used up (as boat's counts).
pub fn limit_tone(used: u8) -> Tone {
    match used {
        u if u >= 100 => Tone::Bad,
        u if u >= 80 => Tone::Warn,
        _ => Tone::Plain,
    }
}

/// How many of `cells` a share lights: any use lights one, and all only when it's all used.
pub fn lit(percent: f64, cells: u32) -> u32 {
    match percent {
        p if p <= 0. => 0,
        p if p >= 100. => cells,
        p => ((p / 100. * cells as f64).ceil() as u32).clamp(1, cells - 1),
    }
}

/// When a window resets, in ms since the epoch.
pub fn resets_ms(w: &LimitWindow) -> Option<i64> {
    w.resets_at.as_deref().and_then(crate::util::iso_to_ms)
}

/// Still running at `now_ms`: it hasn't reset since it was read. One whose reset has passed
/// says nothing about now, so it isn't shown.
pub fn current(w: &LimitWindow, now_ms: i64) -> bool {
    resets_ms(w).is_none_or(|r| r > now_ms)
}

/// Whether a window reset after `since_ms` and by `now_ms`: what's shown of it has gone stale.
pub fn reset_between(all: &[AccountLimits], since_ms: i64, now_ms: i64) -> bool {
    all.iter()
        .flat_map(|l| &l.windows)
        .filter_map(resets_ms)
        .any(|r| r > since_ms && r <= now_ms)
}

/// A weekly window (a week or longer): the only kind shown. A 5-hour one comes and goes too
/// fast to plan by, so it's left out everywhere, as Joseph wants.
pub fn weekly(w: &LimitWindow) -> bool {
    w.minutes
        .map_or(w.id.starts_with("seven_day"), |m| m >= 7 * 24 * 60)
}

/// An account's weekly windows still running at `now_ms`: what the readout, its details and
/// "limit reached" go by.
pub fn windows(l: &AccountLimits, now_ms: i64) -> Vec<&LimitWindow> {
    l.windows
        .iter()
        .filter(|w| weekly(w) && current(w, now_ms))
        .collect()
}

/// The harness whose logo stands for an account: Claude Code's for Claude, Codex's for
/// ChatGPT.
pub fn logo_of(account: &str) -> Option<&'static str> {
    match account {
        "claude" => Some("claude-code"),
        "chatgpt" => Some("codex"),
        _ => None,
    }
}

/// The window closest to its limit across the accounts, with its account: what the readout
/// beside boat's counts shows.
pub fn tightest(all: &[AccountLimits], now_ms: i64) -> Option<(&AccountLimits, &LimitWindow)> {
    // On a tie, the first (accounts come by name), as the iPhone picks.
    all.iter()
        .flat_map(|l| windows(l, now_ms).into_iter().map(move |w| (l, w)))
        .max_by(|a, b| a.1.used.cmp(&b.1.used).then(std::cmp::Ordering::Greater))
}

/// The window that has used up the thread's account, if one has: no more turns until it
/// resets (the latest reset of those used up). One for another model alone doesn't count.
pub fn reached<'a>(
    all: &'a [AccountLimits],
    c: &ContextUse,
    now_ms: i64,
) -> Option<(&'a AccountLimits, &'a LimitWindow)> {
    let account = c.account.as_deref()?;
    let l = all.iter().find(|l| l.account == account)?;
    windows(l, now_ms)
        .into_iter()
        .filter(|w| w.reached() && w.applies_to(c.model.as_deref()))
        .max_by_key(|w| resets_ms(w).unwrap_or(i64::MAX))
        .map(|w| (l, w))
}

/// Where a window is heading if it keeps being used at the rate it has been.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pace {
    /// Used up at `at_ms`, before the window resets.
    RunsOut { at_ms: i64 },
    /// About this share used (0 to 100) when it resets.
    Ends { percent: f64 },
}

/// What a [`Pace`]'s rate is taken over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaceFrom {
    /// The last day: from the window's reading a day or more back (`LimitWindow::earlier`),
    /// once the window has run a day.
    LastDay,
    /// All the window has run since it reset.
    Reset,
}

const DAY_MS: i64 = 86_400_000;
/// Two paces whose window would be used up within this long of each other say the same
/// thing: only the sooner is shown.
pub const PACES_APART_MS: i64 = 12 * 3_600_000;

/// Where `w`, read at `read_ms`, is heading by `now_ms`, at each pace there's one for: the
/// last day's (once it has run a day, with a reading kept from a day back) and the one since
/// it reset (once it has run a 28th: 6 hours of a week). When the two would use it up
/// within [`PACES_APART_MS`] of each other (going on past the reset), only the sooner is
/// given; else both, the last day's first. None once it's used up or not used yet, or
/// without its reset.
pub fn paces(w: &LimitWindow, read_ms: i64, now_ms: i64) -> Vec<(Pace, PaceFrom)> {
    let day = pace_from(w, PaceFrom::LastDay, read_ms, now_ms);
    let reset = pace_from(w, PaceFrom::Reset, read_ms, now_ms);
    match (day, reset) {
        (Some((d, d_out)), Some((r, r_out))) => {
            let close = d_out
                .zip(r_out)
                .is_some_and(|(a, b)| (a - b).abs() < PACES_APART_MS);
            if !close {
                vec![(d, PaceFrom::LastDay), (r, PaceFrom::Reset)]
            } else if d_out <= r_out {
                vec![(d, PaceFrom::LastDay)]
            } else {
                vec![(r, PaceFrom::Reset)]
            }
        }
        (Some((d, _)), None) => vec![(d, PaceFrom::LastDay)],
        (None, Some((r, _))) => vec![(r, PaceFrom::Reset)],
        (None, None) => vec![],
    }
}

/// `w`'s pace over `from`, and when at that rate it would be used up (past its reset too;
/// None if it isn't being used).
fn pace_from(
    w: &LimitWindow,
    from: PaceFrom,
    read_ms: i64,
    now_ms: i64,
) -> Option<(Pace, Option<i64>)> {
    if w.reached() || w.used == 0 {
        return None;
    }
    let reset = resets_ms(w)?;
    if reset <= now_ms {
        return None;
    }
    let len = w.minutes.filter(|m| *m > 0).map(|m| i64::from(m) * 60_000);
    let start = len.map(|l| reset - l);
    let (from_used, from_at) = match from {
        PaceFrom::LastDay => {
            let e = w.earlier?;
            let ran_a_day = start.is_none_or(|s| read_ms - s >= DAY_MS && e.at_ms >= s);
            if !ran_a_day || e.at_ms >= read_ms {
                return None;
            }
            (e.used, e.at_ms)
        }
        PaceFrom::Reset => {
            let (len, start) = (len?, start?);
            if read_ms - start < len / 28 {
                return None;
            }
            (0, start)
        }
    };
    // Percent per ms.
    let rate = f64::from(w.used.saturating_sub(from_used)) / (read_ms - from_at) as f64;
    let used = f64::from(w.used);
    let out = (rate > 0.).then(|| read_ms + ((100. - used) / rate) as i64);
    let pace = match out {
        Some(at) if at < reset => Pace::RunsOut {
            at_ms: at.max(now_ms),
        },
        _ => Pace::Ends {
            percent: used + rate * (reset - read_ms) as f64,
        },
    };
    Some((pace, out))
}

/// How long until `at_ms`: "4d 6h", "2h 13m", "12m", "under a minute".
pub fn until(at_ms: i64, now_ms: i64) -> String {
    let mins = (at_ms - now_ms).max(0) / 60_000;
    let (d, h, m) = (mins / 1440, mins % 1440 / 60, mins % 60);
    match (d, h, m) {
        (0, 0, 0) => "under a minute".to_string(),
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso_to_ms_or(t: &str) -> i64 {
        crate::util::iso_to_ms(t).unwrap()
    }

    fn w(used: u8, resets: Option<&str>) -> LimitWindow {
        LimitWindow {
            id: "seven_day".into(),
            label: "Weekly".into(),
            used,
            resets_at: resets.map(Into::into),
            minutes: Some(10080),
            scope: None,
            earlier: None,
        }
    }

    #[test]
    fn writes_counts_as_t3_does() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(9_500), "9.5k");
        assert_eq!(tokens(9_000), "9k");
        assert_eq!(tokens(21_909), "22k");
        assert_eq!(tokens(828_400), "828k");
        assert_eq!(tokens(999_600), "1m");
        assert_eq!(tokens(1_000_000), "1m");
        assert_eq!(tokens(1_048_576), "1m");
        assert_eq!(tokens(1_260_000), "1.3m");
        assert_eq!(exact(21_909), "21,909");
        assert_eq!(exact(1_000_000), "1,000,000");
        assert_eq!(exact(7), "7");
        assert_eq!(percent(2.38), "2.4%");
        assert_eq!(percent(2.0), "2%");
        assert_eq!(percent(42.6), "43%");
    }

    #[test]
    fn meters_light_one_for_any_use_and_all_only_when_full() {
        assert_eq!(lit(0., 12), 0);
        assert_eq!(lit(0.1, 12), 1);
        assert_eq!(lit(50., 12), 6);
        assert_eq!(lit(99.9, 12), 11);
        assert_eq!(lit(100., 12), 12);
        assert_eq!(context_tone(79.9), Tone::Plain);
        assert_eq!(context_tone(80.), Tone::Warn);
        assert_eq!(context_tone(90.), Tone::Bad);
        assert_eq!(limit_tone(92), Tone::Warn);
        assert_eq!(limit_tone(100), Tone::Bad);
    }

    /// Only weekly windows are shown: a 5-hour one, however full, is left out of the readout,
    /// its details and "limit reached", whether its length is told or only its id.
    #[test]
    fn only_weekly_windows_are_shown() {
        let now = crate::util::iso_to_ms("2026-11-01T14:00:00Z").unwrap();
        let five = LimitWindow {
            id: "five_hour".into(),
            label: "5-hour".into(),
            minutes: Some(300),
            ..w(100, Some("2026-11-01T15:00:00Z"))
        };
        let codex_five = LimitWindow {
            id: "primary".into(),
            label: "5-hour".into(),
            minutes: Some(300),
            ..w(97, Some("2026-11-01T15:00:00Z"))
        };
        let untold = LimitWindow {
            id: "five_hour".into(),
            minutes: None,
            ..w(99, Some("2026-11-01T15:00:00Z"))
        };
        let l = AccountLimits {
            account: "claude".into(),
            plan: None,
            windows: vec![five, codex_five, untold, w(8, Some("2026-11-05T15:00:00Z"))],
            read_ms: now,
        };
        assert_eq!(
            windows(&l, now).iter().map(|w| w.used).collect::<Vec<_>>(),
            [8]
        );
        assert_eq!(
            tightest(std::slice::from_ref(&l), now).map(|(_, w)| w.used),
            Some(8)
        );
        let c = ContextUse {
            used: 1,
            account: Some("claude".into()),
            ..Default::default()
        };
        assert!(
            reached(std::slice::from_ref(&l), &c, now).is_none(),
            "a full 5-hour window isn't \"limit reached\""
        );
    }

    /// Each pace carries its rate on to the reset: the last day's (from a day into the
    /// window) and the one since the reset, both unless they'd run out within 12 hours of
    /// each other, then the sooner alone.
    #[test]
    fn paces_carry_each_rate_on_to_the_reset() {
        let now = crate::util::iso_to_ms("2026-11-03T12:00:00Z").unwrap();
        let (hour, day) = (3_600_000, 86_400_000);
        let at = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .unwrap()
                .to_rfc3339()
        };
        let a_day_back = |used| {
            Some(flotilla_proto::LimitReading {
                used,
                at_ms: now - day,
            })
        };
        let froms = |p: &[(Pace, PaceFrom)]| p.iter().map(|(_, f)| *f).collect::<Vec<_>>();
        let out_in = |p: Pace| match p {
            Pace::RunsOut { at_ms } => Some((at_ms - now) / hour),
            Pace::Ends { .. } => None,
        };
        // 2 days in at 73%, with no reading from a day back: 36.5% a day since the reset, so
        // out in about 17 hours.
        let fast = w(73, Some(&at(now + 5 * day)));
        let p = paces(&fast, now, now);
        assert_eq!(froms(&p), [PaceFrom::Reset]);
        assert_eq!(out_in(p[0].0), Some(17));
        // 32% of it in the last day: out in about 20 hours, within 12 of 17: the sooner alone.
        let close = LimitWindow {
            earlier: a_day_back(41),
            ..fast.clone()
        };
        let p = paces(&close, now, now);
        assert_eq!(froms(&p), [PaceFrom::Reset]);
        assert_eq!(out_in(p[0].0), Some(17));
        // 60% in the last day: out in about 11 hours, the sooner now.
        let busy = LimitWindow {
            earlier: a_day_back(13),
            ..fast.clone()
        };
        assert_eq!(froms(&paces(&busy, now, now)), [PaceFrom::LastDay]);
        // 3% in the last day: ending near 88%, far from the reset pace's 17 hours: both.
        let calm = LimitWindow {
            earlier: a_day_back(70),
            ..fast.clone()
        };
        let p = paces(&calm, now, now);
        assert_eq!(froms(&p), [PaceFrom::LastDay, PaceFrom::Reset]);
        let Pace::Ends { percent } = p[0].0 else {
            panic!("3% a day lasts");
        };
        assert_eq!(percent.round(), 88.);
        // None in the last day: it ends where it is, and is never used up.
        let idle = LimitWindow {
            earlier: a_day_back(73),
            ..fast.clone()
        };
        assert_eq!(
            paces(&idle, now, now)[0],
            (Pace::Ends { percent: 73. }, PaceFrom::LastDay)
        );
        // Read an hour ago: the projection runs from the reading, not from now.
        let p = paces(&busy, now - hour, now);
        assert_eq!(out_in(p[0].0), Some(9));
        // Under a day since the reset: no last day's pace, whatever was kept.
        let young = LimitWindow {
            earlier: Some(flotilla_proto::LimitReading {
                used: 1,
                at_ms: now - day,
            }),
            ..w(30, Some(&at(now + 6 * day + hour)))
        };
        assert_eq!(froms(&paces(&young, now, now)), [PaceFrom::Reset]);
        // Late in a slow week: 13% in 5 days 20 hours, about 16% by its reset.
        let slow = w(13, Some(&at(now + day + 4 * hour)));
        let p = paces(&slow, now, now);
        let Pace::Ends { percent } = p[0].0 else {
            panic!("13% in most of a week lasts");
        };
        assert_eq!(percent.round(), 16.);
        // Too soon, used up, unused, or no reset: nothing to say.
        assert!(paces(&w(40, Some(&at(now + 7 * day - hour))), now, now).is_empty());
        assert!(paces(&w(100, Some(&at(now + day))), now, now).is_empty());
        assert!(paces(&w(0, Some(&at(now + day))), now, now).is_empty());
        assert!(paces(&w(50, None), now, now).is_empty());
        // No length: only the last day's pace can say.
        let untold = |w: &LimitWindow| LimitWindow {
            minutes: None,
            ..w.clone()
        };
        assert!(paces(&untold(&fast), now, now).is_empty());
        assert_eq!(froms(&paces(&untold(&busy), now, now)), [PaceFrom::LastDay]);
    }

    #[test]
    fn a_window_whose_reset_passed_is_not_shown() {
        let now = crate::util::iso_to_ms("2026-11-01T14:00:00Z").unwrap();
        let l = AccountLimits {
            account: "claude".into(),
            plan: None,
            windows: vec![
                w(92, Some("2026-11-01T15:00:00Z")),
                LimitWindow {
                    id: "five_hour".into(),
                    used: 100,
                    ..w(100, Some("2026-11-01T13:00:00Z"))
                },
            ],
            read_ms: now,
        };
        assert_eq!(windows(&l, now).len(), 1);
        // A reset crossed since the last look, however long ago that was; none twice.
        let reset = iso_to_ms_or("2026-11-01T15:00:00Z");
        assert!(
            reset_between(std::slice::from_ref(&l), reset - 1_000, reset + 9_000),
            "a tick 9 s late"
        );
        assert!(!reset_between(
            std::slice::from_ref(&l),
            reset + 9_000,
            reset + 10_000
        ));
        assert!(!reset_between(
            std::slice::from_ref(&l),
            reset - 2_000,
            reset - 1_000
        ));
        let all = [
            l.clone(),
            AccountLimits {
                account: "chatgpt".into(),
                windows: vec![w(10, Some("2026-11-04T19:09:31Z"))],
                ..l.clone()
            },
        ];
        let (a, t) = tightest(&all, now).unwrap();
        assert_eq!((a.account.as_str(), t.used), ("claude", 92));
        let tied = [
            AccountLimits {
                account: "chatgpt".into(),
                windows: vec![w(92, None)],
                ..l.clone()
            },
            l.clone(),
        ];
        assert_eq!(
            tightest(&tied, now).map(|(a, _)| a.account.as_str()),
            Some("chatgpt"),
            "a tie keeps the first"
        );
        // Used up and not reset yet: the thread's account can't run a turn.
        let c = ContextUse {
            used: 1,
            account: Some("claude".into()),
            ..Default::default()
        };
        assert!(reached(&all, &c, now).is_none());
        let full = [AccountLimits {
            windows: vec![w(100, Some("2026-11-01T15:00:00Z"))],
            ..all[0].clone()
        }];
        assert_eq!(reached(&full, &c, now).map(|(_, w)| w.used), Some(100));
        assert!(
            reached(
                &full,
                &ContextUse {
                    account: Some("chatgpt".into()),
                    ..c.clone()
                },
                now
            )
            .is_none()
        );
        // A model's own window used up stops only threads on that model.
        let fable = [AccountLimits {
            windows: vec![LimitWindow {
                id: "seven_day_fable".into(),
                scope: Some("Fable".into()),
                ..w(100, Some("2026-11-01T15:00:00Z"))
            }],
            ..all[0].clone()
        }];
        let on = |m: &str| ContextUse {
            model: Some(m.into()),
            ..c.clone()
        };
        assert!(reached(&fable, &on("claude-haiku-5-5"), now).is_none());
        assert!(reached(&fable, &on("claude-fable-5-1[1m]"), now).is_some());
        assert_eq!(until(now + 2 * 3_600_000 + 13 * 60_000, now), "2h 13m");
        assert_eq!(
            until(now + 4 * 86_400_000 + 6 * 3_600_000 + 5, now),
            "4d 6h"
        );
        assert_eq!(until(now + 30_000, now), "under a minute");
    }
}
