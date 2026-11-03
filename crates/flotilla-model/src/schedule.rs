//! When a scheduled prompt runs (`flotilla_proto::schedule`): its next time in its zone, cron
//! expressions, whether a late run still goes, what a schedule may be, and its words.
//!
//! Times of day are wall-clock times in the schedule's zone, so 09:00 stays 09:00 across a
//! daylight-saving change. A time the clocks skip (02:30 when they jump from 02:00 to 03:00)
//! runs as far after the jump as it was into the gap (03:30), as Temporal's "compatible" does;
//! a time the clocks repeat (01:30 when they go back) runs once, the first time. Every N hours
//! (only made before runs had to be a day apart) counts real hours from the first run.
//!
//! No schedule runs more than once a day ([`often_enough`]).

use flotilla_proto::{ScheduleSpec, When};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

/// A daily, weekly or cron time missed by more than this (the server was off) is skipped to
/// the next one, as T3 does. One-offs and every-N-hours runs go late instead.
pub const LATE_MS: i64 = 10 * MINUTE;
/// Why a schedule may not run as often as it says: runs at most once a day, whatever they
/// send to (each one a turn, and a new thread's a VM start).
pub const TOO_OFTEN: &str = "Runs must be at least a day apart.";
/// The longest prompt a schedule keeps.
pub const PROMPT_MAX: usize = 100_000;
/// How far ahead a time is looked for (a cron for 29 February finds one within 8 years).
const SEARCH_DAYS: i32 = 366 * 8 + 2;

/// The zone, by its IANA name.
pub fn zone(name: &str) -> Result<TimeZone, String> {
    TimeZone::get(name).map_err(|_| format!("Unknown time zone: {name}"))
}

/// This device's zone's IANA name, if it has one (else UTC).
pub fn here() -> String {
    TimeZone::system().iana_name().unwrap_or("UTC").to_string()
}

/// Its first run after `after_ms` (strictly), or none: a one-off whose time has passed, or a
/// cron that never matches.
pub fn next_after(when: &When, zone_name: &str, after_ms: i64) -> Result<Option<i64>, String> {
    match when {
        When::Once { at_ms } => Ok((*at_ms > after_ms).then_some(*at_ms)),
        When::Hours { every, from_ms } => {
            let step = i64::from((*every).max(1)) * HOUR;
            if *from_ms > after_ms {
                return Ok(Some(*from_ms));
            }
            let k = (after_ms - from_ms) / step + 1;
            Ok(Some(from_ms + k * step))
        }
        _ => {
            let cron = Cron::of(when)?;
            cron.next_after(&zone(zone_name)?, after_ms)
        }
    }
}

/// Whether a run due at `due_ms` still goes at `now_ms`, or its time passed while the server
/// was off and it's skipped to the next.
pub fn still_due(when: &When, due_ms: i64, now_ms: i64) -> bool {
    match when {
        When::Once { .. } | When::Hours { .. } => true,
        _ => now_ms - due_ms <= LATE_MS,
    }
}

/// Whether a schedule may be kept as it is, at `now_ms`, else why not, in words for the user.
/// `was`: what it was before this edit; a timing kept as it was isn't checked again (a one-off
/// waiting on its thread past its time may still have its prompt changed), except that it
/// runs at most once a day.
pub fn check(spec: &ScheduleSpec, now_ms: i64, was: Option<&ScheduleSpec>) -> Result<(), String> {
    if spec.prompt.trim().is_empty() {
        return Err("The prompt is empty.".into());
    }
    if spec.prompt.len() > PROMPT_MAX {
        return Err("The prompt is too long.".into());
    }
    zone(&spec.zone)?;
    if was.is_some_and(|w| {
        w.when == spec.when
            && w.zone == spec.zone
            && std::mem::discriminant(&w.target) == std::mem::discriminant(&spec.target)
    }) {
        return often_enough(&spec.when);
    }
    match &spec.when {
        When::Once { at_ms } => {
            // A minute's grace for a clock a little behind the server's.
            if *at_ms <= now_ms - MINUTE {
                return Err("That time has passed.".into());
            }
            return Ok(());
        }
        When::Daily { at } | When::Weekdays { at } | When::Weekly { at, .. } if *at >= 24 * 60 => {
            return Err("That isn't a time of day.".into());
        }
        When::Weekly { days, .. }
            if days.is_empty() || days.iter().any(|d| !(1..=7).contains(d)) =>
        {
            return Err("Pick a day.".into());
        }
        _ => {}
    }
    often_enough(&spec.when)?;
    if !matches!(spec.when, When::Hours { .. }) {
        Cron::of(&spec.when)?
            .next_after(&zone(&spec.zone)?, now_ms)?
            .ok_or("That never comes round.")?;
    }
    Ok(())
}

/// Whether its runs are at least a day apart, else why not ([`TOO_OFTEN`]), whatever its
/// target. A daily, weekday or weekly time, or a cron with one time of day, is: on the
/// clock, its runs are whole days apart. A change of the clocks may bring two a little
/// closer in real time (a daily 09:00 is 23 hours from the one before when they go forward),
/// and that's still once a day. A cron with two times of day or more runs twice on a day it
/// matches. Every N hours must be every 24 or more. A cron that doesn't read says why.
pub fn often_enough(when: &When) -> Result<(), String> {
    match when {
        When::Once { .. } => Ok(()),
        When::Hours { every, .. } if *every < 24 => Err(TOO_OFTEN.into()),
        When::Hours { .. } => Ok(()),
        _ => {
            let cron = Cron::of(when)?;
            if cron.minutes.len() == 1 && cron.hours.len() == 1 {
                Ok(())
            } else {
                Err(TOO_OFTEN.into())
            }
        }
    }
}

/// Its timing in words: "Daily at 09:00", "Mon, Wed and Fri at 18:30", "Every 3 hours",
/// "0 9 * * 1-5 (cron)"; a one-off is "Once".
pub fn describe(when: &When) -> String {
    match when {
        When::Once { .. } => "Once".into(),
        When::Hours { every: 1, .. } => "Every hour".into(),
        When::Hours { every, .. } => format!("Every {every} hours"),
        When::Daily { at } => format!("Daily at {}", clock(*at)),
        When::Weekdays { at } => format!("Weekdays at {}", clock(*at)),
        When::Weekly { days, at } => {
            let mut days: Vec<u8> = days
                .iter()
                .copied()
                .filter(|d| (1..=7).contains(d))
                .collect();
            days.sort_unstable();
            days.dedup();
            match days.as_slice() {
                [1, 2, 3, 4, 5, 6, 7] => format!("Daily at {}", clock(*at)),
                [1, 2, 3, 4, 5] => format!("Weekdays at {}", clock(*at)),
                [d] => format!("{}s at {}", DAY_NAMES[usize::from(*d) - 1], clock(*at)),
                many => format!(
                    "{} at {}",
                    and_list(
                        &many
                            .iter()
                            .map(|d| &DAY_NAMES[usize::from(*d) - 1][..3])
                            .collect::<Vec<_>>()
                    ),
                    clock(*at)
                ),
            }
        }
        When::Cron { expr } => format!(
            "{} (cron)",
            expr.split_whitespace().collect::<Vec<_>>().join(" ")
        ),
    }
}

/// A moment in words, in `zone_name`, beside `now_ms`: "today 09:00", "tomorrow 09:00",
/// "yesterday 18:30", "Fri 09:00" (this week), else "Mon 9 Nov 09:00" (with the year when it
/// isn't this one's).
pub fn when_words(ms: i64, now_ms: i64, zone_name: &str) -> String {
    let (Some(t), Some(now)) = (zoned(ms, zone_name), zoned(now_ms, zone_name)) else {
        return String::new();
    };
    let time = t.strftime("%H:%M").to_string();
    let days = (t.date() - now.date()).get_days();
    match days {
        0 => format!("today {time}"),
        1 => format!("tomorrow {time}"),
        -1 => format!("yesterday {time}"),
        2..=6 => format!("{} {time}", t.strftime("%a")),
        _ if t.year() == now.year() => format!("{} {time}", t.strftime("%a %-d %b")),
        _ => format!("{} {time}", t.strftime("%a %-d %b %Y")),
    }
}

/// A schedule's timing and next run in words, the times in `here` (this device's zone):
/// "Daily at 09:00 · next tomorrow 09:00", "Once · tomorrow 09:00", "Weekdays at 09:00 ·
/// paused", "Once · sent today 09:00". A recurring time names its zone when it isn't `here`.
pub fn summary(s: &flotilla_proto::Schedule, now_ms: i64, here: &str) -> String {
    let timing = match &s.when {
        When::Once { .. } | When::Hours { .. } => describe(&s.when),
        when if s.zone != here => format!("{} ({})", describe(when), s.zone),
        when => describe(when),
    };
    let state = if s.paused {
        "paused".to_string()
    } else if s.waiting_ms.is_some() {
        "waiting for the turn to end".to_string()
    } else if let Some(next) = s.next_ms {
        if s.when.once() {
            when_words(next, now_ms, here)
        } else {
            format!("next {}", when_words(next, now_ms, here))
        }
    } else {
        match &s.last {
            Some(l) if l.outcome == flotilla_proto::RunOutcome::Sent => {
                format!("sent {}", when_words(l.at_ms, now_ms, here))
            }
            Some(l) => format!("didn't run {}", when_words(l.at_ms, now_ms, here)),
            None => "done".to_string(),
        }
    };
    format!("{timing} · {state}")
}

/// Its last run in words, if it had one worth saying: "Last sent today 09:00", "Missed
/// today 09:00: the server was off.", "Didn't run today 09:00: the thread is archived."
/// (A one-off's own run is in its summary.)
pub fn last_words(s: &flotilla_proto::Schedule, now_ms: i64, here: &str) -> Option<String> {
    use flotilla_proto::RunOutcome;
    let l = s.last.as_ref()?;
    let at = when_words(l.at_ms, now_ms, here);
    let why = l.why.as_deref().map(|w| {
        let mut c = w.chars();
        c.next()
            .map(|f| f.to_lowercase().chain(c).collect::<String>())
            .unwrap_or_default()
    });
    match (l.outcome, why) {
        (RunOutcome::Sent, _) if s.when.once() && s.next_ms.is_none() => None,
        (RunOutcome::Sent, _) => Some(format!("Last sent {at}")),
        (RunOutcome::Missed, Some(w)) => Some(format!("Missed {at}: {w}")),
        (RunOutcome::Missed, None) => Some(format!("Missed {at}")),
        (RunOutcome::Failed, Some(w)) => Some(format!("Didn't run {at}: {w}")),
        (RunOutcome::Failed, None) => Some(format!("Didn't run {at}")),
    }
}

/// ISO weekdays' names, Monday first.
pub const DAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Minutes after midnight as `09:30`.
pub fn clock(at: u32) -> String {
    format!("{:02}:{:02}", (at / 60) % 24, at % 60)
}

/// `9`, `9:30`, `09:30`, `21:05`, `9am`, `9:30 pm` as minutes after midnight.
pub fn parse_clock(s: &str) -> Option<u32> {
    let s = s.trim().to_ascii_lowercase();
    let (s, half) = match (s.strip_suffix("am"), s.strip_suffix("pm")) {
        (Some(t), _) => (t.trim().to_string(), Some(0)),
        (_, Some(t)) => (t.trim().to_string(), Some(12)),
        _ => (s, None),
    };
    let (h, m) = match s.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        Some(_) => return None,
        None => (s.parse::<u32>().ok()?, 0),
    };
    if m > 59 {
        return None;
    }
    let h = match half {
        Some(add) if (1..=12).contains(&h) => h % 12 + add,
        Some(_) => return None,
        None if h <= 23 => h,
        None => return None,
    };
    Some(h * 60 + m)
}

/// `ms` in `zone` as a wall-clock time (for words and tests).
pub fn zoned(ms: i64, zone_name: &str) -> Option<Zoned> {
    Some(
        Timestamp::from_millisecond(ms)
            .ok()?
            .to_zoned(zone(zone_name).ok()?),
    )
}

/// A wall-clock time in `zone` as ms, the first of two (a time the clocks repeat), or shifted
/// past a gap (a time they skip).
pub fn at_local(zone_name: &str, date: Date, minutes: u32) -> Result<i64, String> {
    let tz = zone(zone_name)?;
    instant(&tz, date, minutes / 60, minutes % 60)
}

/// [`at_local`] from a year, month and day.
pub fn local_ms(
    zone_name: &str,
    year: i32,
    month: u32,
    day: u32,
    minutes: u32,
) -> Result<i64, String> {
    let date = Date::new(year as i16, month as i8, day as i8)
        .map_err(|_| "That isn't a day.".to_string())?;
    at_local(zone_name, date, minutes)
}

/// `ms` in `zone` as its year, month, day and minutes after midnight.
pub fn local_parts(ms: i64, zone_name: &str) -> Option<(i32, u32, u32, u32)> {
    let z = zoned(ms, zone_name)?;
    Some((
        i32::from(z.year()),
        z.month() as u32,
        z.day() as u32,
        z.hour() as u32 * 60 + z.minute() as u32,
    ))
}

/// Times a "Send later" offers at a click, after `now_ms` in `zone`: in an hour, tomorrow at
/// 09:00, and next Monday at 09:00 (when tomorrow isn't Monday).
pub fn quick_times(now_ms: i64, zone_name: &str) -> Vec<(String, i64)> {
    let mut out = vec![(
        "In 1 hour".to_string(),
        now_ms - now_ms.rem_euclid(MINUTE) + HOUR,
    )];
    let Some(now) = zoned(now_ms, zone_name) else {
        return out;
    };
    let Ok(tomorrow) = now.date().tomorrow() else {
        return out;
    };
    if let Ok(ms) = at_local(zone_name, tomorrow, 9 * 60) {
        out.push(("Tomorrow 09:00".into(), ms));
    }
    if tomorrow.weekday() != jiff::civil::Weekday::Monday {
        let monday = (1..=7)
            .filter_map(|n| now.date().checked_add(jiff::Span::new().days(n)).ok())
            .find(|d| d.weekday() == jiff::civil::Weekday::Monday);
        if let Some(ms) = monday.and_then(|d| at_local(zone_name, d, 9 * 60).ok()) {
            out.push(("Monday 09:00".into(), ms));
        }
    }
    out
}

fn instant(tz: &TimeZone, date: Date, hour: u32, minute: u32) -> Result<i64, String> {
    let dt = date.at(hour as i8, minute as i8, 0, 0);
    let z = tz
        .to_ambiguous_zoned(dt)
        .compatible()
        .map_err(|e| e.to_string())?;
    Ok(z.timestamp().as_millisecond())
}

/// A cron expression's sets: which minutes, hours, days of the month, months and days of the
/// week (0 = Sunday) match. Daily, weekday and weekly times are crons too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days: Bits,
    months: Bits,
    weekdays: Bits,
    /// Both day fields were given (not `*`): either matches, as in Vixie cron.
    either: bool,
}

/// A set of small numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bits(u64);

impl Bits {
    fn has(self, n: u32) -> bool {
        n < 64 && self.0 & (1 << n) != 0
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Cron {
    /// Parse five fields (minute, hour, day of the month, month, day of the week), or one of
    /// `@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`.
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let expr = match expr.trim() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            e => e,
        };
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let [min, hour, dom, month, dow] = fields.as_slice() else {
            return Err("A cron expression has five fields: minute, hour, day of the month, month and day of the week.".into());
        };
        let minutes = field(min, 0, 59, &[], "minute")?;
        let hours = field(hour, 0, 23, &[], "hour")?;
        let days = field(dom, 1, 31, &[], "day of the month")?;
        let months = field(month, 1, 12, &MONTHS, "month")?;
        // 7 is Sunday too.
        let weekdays = field(dow, 0, 7, &WEEKDAYS, "day of the week")?;
        let weekdays = if weekdays.has(7) {
            Bits(weekdays.0 | 1)
        } else {
            weekdays
        };
        let list = |b: Bits, hi: u32| (0..=hi).filter(|n| b.has(*n)).collect::<Vec<u32>>();
        let star = |f: &str| f == "*" || f == "?";
        Ok(Cron {
            minutes: list(minutes, 59),
            hours: list(hours, 23),
            days,
            months,
            weekdays,
            either: !star(dom) && !star(dow),
        })
    }

    /// A daily, weekday or weekly time as a cron; a cron's own.
    fn of(when: &When) -> Result<Cron, String> {
        let at = |at: u32, days: &str| Cron::parse(&format!("{} {} * * {days}", at % 60, at / 60));
        match when {
            When::Daily { at: t } => at(*t, "*"),
            When::Weekdays { at: t } => at(*t, "1-5"),
            When::Weekly { days, at: t } => {
                let days: Vec<String> = days.iter().map(|d| (d % 7).to_string()).collect();
                if days.is_empty() {
                    return Err("Pick a day.".into());
                }
                at(*t, &days.join(","))
            }
            When::Cron { expr } => Cron::parse(expr),
            When::Once { .. } | When::Hours { .. } => Err("not a time of day".into()),
        }
    }

    fn date_matches(&self, d: Date) -> bool {
        if !self.months.has(d.month() as u32) {
            return false;
        }
        let dom = self.days.has(d.day() as u32);
        let dow = self
            .weekdays
            .has(d.weekday().to_sunday_zero_offset() as u32);
        if self.either { dom || dow } else { dom && dow }
    }

    /// The earliest of `date`'s times after `after_ms`. (The earliest, not the first in clock
    /// order: a skipped time runs later than the times after it.)
    fn earliest_on(&self, tz: &TimeZone, date: Date, after_ms: i64) -> Result<Option<i64>, String> {
        if !self.date_matches(date) {
            return Ok(None);
        }
        let mut best: Option<i64> = None;
        for &h in &self.hours {
            for &m in &self.minutes {
                let at = instant(tz, date, h, m)?;
                if at > after_ms && best.is_none_or(|b| at < b) {
                    best = Some(at);
                }
            }
        }
        Ok(best)
    }

    /// Its first time after `after_ms` in `tz`.
    pub fn next_after(&self, tz: &TimeZone, after_ms: i64) -> Result<Option<i64>, String> {
        let start = Timestamp::from_millisecond(after_ms)
            .map_err(|e| e.to_string())?
            .to_zoned(tz.clone());
        // From the day before: a time shifted past a gap at midnight lands on the next day.
        let mut date = start.date().yesterday().map_err(|e| e.to_string())?;
        for _ in 0..SEARCH_DAYS {
            if let Some(found) = self.earliest_on(tz, date, after_ms)? {
                // The next day's may still come first (a skipped time late in this one).
                let next = date.tomorrow().map_err(|e| e.to_string())?;
                let then = self.earliest_on(tz, next, after_ms)?;
                return Ok(Some(then.map_or(found, |t| t.min(found))));
            }
            date = date.tomorrow().map_err(|e| e.to_string())?;
        }
        Ok(None)
    }
}

/// One cron field: `*`, numbers or names, `a-b`, steps (`*/15`, `a-b/2`, `a/5`), lists.
fn field(text: &str, lo: u32, hi: u32, names: &[&str], what: &str) -> Result<Bits, String> {
    let bad = || format!("Can't read the {what} field: {text}");
    let value = |s: &str| -> Result<u32, String> {
        let s = s.to_ascii_lowercase();
        let n = match names.iter().position(|n| *n == s) {
            Some(i) => i as u32 + if names.len() == 12 { 1 } else { 0 },
            None => s.parse::<u32>().map_err(|_| bad())?,
        };
        if n < lo || n > hi {
            Err(format!("The {what} field goes from {lo} to {hi}: {text}"))
        } else {
            Ok(n)
        }
    };
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|s| *s > 0).ok_or_else(bad)?),
            None => (part, 1),
        };
        let (from, to) = match range {
            "*" | "?" => (lo, hi),
            r => match r.split_once('-') {
                Some((a, b)) => (value(a)?, value(b)?),
                // `a/5`: from a to the end.
                None if part.contains('/') => (value(r)?, hi),
                None => {
                    let n = value(r)?;
                    (n, n)
                }
            },
        };
        if from > to {
            return Err(bad());
        }
        let mut n = from;
        while n <= to {
            bits |= 1 << n;
            let Some(next) = n.checked_add(step) else {
                break;
            };
            n = next;
        }
    }
    if bits == 0 {
        Err(bad())
    } else {
        Ok(Bits(bits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_proto::{Fresh, OnBranch, Target};
    use jiff::civil::date;

    /// `y-m-d h:m` in `zone`, as ms.
    fn at(zone_name: &str, y: i16, mo: i8, d: i8, h: u32, m: u32) -> i64 {
        at_local(zone_name, date(y, mo, d), h * 60 + m).unwrap()
    }

    /// `ms` in `zone`, as `y-m-d h:m ±hh` (the offset says which of a repeated hour).
    fn show(ms: i64, zone_name: &str) -> String {
        zoned(ms, zone_name)
            .unwrap()
            .strftime("%Y-%m-%d %H:%M %:z")
            .to_string()
    }

    fn next(when: &When, zone_name: &str, after: i64) -> String {
        show(
            next_after(when, zone_name, after).unwrap().unwrap(),
            zone_name,
        )
    }

    fn cron(expr: &str) -> When {
        When::Cron { expr: expr.into() }
    }

    fn spec(when: When, target: Target) -> ScheduleSpec {
        ScheduleSpec {
            id: "s".into(),
            prompt: "Check CI".into(),
            target,
            when,
            zone: "Europe/London".into(),
        }
    }

    fn fresh() -> Target {
        Target::New(Fresh {
            environment: "base".into(),
            project_id: None,
            provider: "claude-code".into(),
            model: "m".into(),
            effort: None,
            fast: false,
            machine_type: None,
            on_branch: None::<OnBranch>,
        })
    }

    #[test]
    fn daily_weekday_and_weekly_times_come_round_in_their_zone() {
        let ny = "America/New_York";
        // Monday 3 November 2025, 08:00 in New York.
        let mon8 = at(ny, 2025, 11, 3, 8, 0);
        assert_eq!(
            next(&When::Daily { at: 9 * 60 }, ny, mon8),
            "2025-11-03 09:00 -05:00"
        );
        // At 09:00 exactly, the next is tomorrow's.
        assert_eq!(
            next(&When::Daily { at: 9 * 60 }, ny, at(ny, 2025, 11, 3, 9, 0)),
            "2025-11-04 09:00 -05:00"
        );
        // Friday evening: Monday.
        assert_eq!(
            next(
                &When::Weekdays { at: 9 * 60 + 30 },
                ny,
                at(ny, 2025, 11, 7, 18, 0)
            ),
            "2025-11-10 09:30 -05:00"
        );
        // Mondays, Wednesdays and Sundays at 20:00, from Monday 21:00: Wednesday.
        let weekly = When::Weekly {
            days: vec![1, 3, 7],
            at: 20 * 60,
        };
        assert_eq!(
            next(&weekly, ny, at(ny, 2025, 11, 3, 21, 0)),
            "2025-11-05 20:00 -05:00"
        );
        assert_eq!(
            next(&weekly, ny, at(ny, 2025, 11, 6, 0, 0)),
            "2025-11-09 20:00 -05:00"
        );
        // The same moment is a different day and time in Tokyo.
        assert_eq!(
            next(&When::Daily { at: 9 * 60 }, "Asia/Tokyo", mon8),
            "2025-11-04 09:00 +09:00"
        );
        // A zone that doesn't exist.
        assert!(next_after(&When::Daily { at: 0 }, "Mars/Olympus", mon8).is_err());
    }

    #[test]
    fn a_time_the_clocks_skip_runs_after_the_jump_and_one_they_repeat_runs_once() {
        let ny = "America/New_York";
        // Spring forward: Sunday 8 March 2026, 02:00 → 03:00. 02:30 doesn't happen that day.
        let sat = at(ny, 2026, 3, 7, 12, 0);
        let half_two = When::Daily { at: 2 * 60 + 30 };
        assert_eq!(next(&half_two, ny, sat), "2026-03-08 03:30 -04:00");
        assert_eq!(
            next(&half_two, ny, at(ny, 2026, 3, 8, 4, 0)),
            "2026-03-09 02:30 -04:00"
        );
        // 09:00 stays 09:00 on either side of the change.
        assert_eq!(
            next(&When::Daily { at: 9 * 60 }, ny, sat),
            "2026-03-08 09:00 -04:00"
        );
        assert_eq!(
            next(&When::Daily { at: 9 * 60 }, ny, at(ny, 2026, 3, 6, 12, 0)),
            "2026-03-07 09:00 -05:00"
        );
        // Fall back: Sunday 1 November 2026, 02:00 → 01:00. 01:30 happens twice; it runs once,
        // the first time (EDT), and next on Monday.
        let one_thirty = When::Daily { at: 90 };
        let first = next_after(&one_thirty, ny, at(ny, 2026, 10, 31, 12, 0))
            .unwrap()
            .unwrap();
        assert_eq!(show(first, ny), "2026-11-01 01:30 -04:00");
        assert_eq!(next(&one_thirty, ny, first), "2026-11-02 01:30 -05:00");
        // Every 15 minutes in the 1 o'clock hour: the hour runs once, not twice.
        let quarter = cron("*/15 1 * * *");
        let mut t = at(ny, 2026, 11, 1, 0, 59);
        let mut seen = vec![];
        for _ in 0..5 {
            t = next_after(&quarter, ny, t).unwrap().unwrap();
            seen.push(show(t, ny));
        }
        assert_eq!(
            seen,
            [
                "2026-11-01 01:00 -04:00",
                "2026-11-01 01:15 -04:00",
                "2026-11-01 01:30 -04:00",
                "2026-11-01 01:45 -04:00",
                "2026-11-02 01:00 -05:00"
            ]
        );
        // London's change (last Sunday of March, 01:00 → 02:00) skips 01:00 to 01:59.
        assert_eq!(
            next(
                &When::Daily { at: 60 + 15 },
                "Europe/London",
                at("Europe/London", 2026, 3, 28, 12, 0)
            ),
            "2026-03-29 02:15 +01:00"
        );
    }

    #[test]
    fn a_skipped_time_late_in_the_day_still_comes_in_order() {
        // Clocks that skipped from midnight to 01:00 (Santiago, 2024-09-08): a 00:30 that
        // day runs at 01:30, after that day's 01:00.
        let zone_name = "America/Santiago";
        let both = cron("30 0 * * *");
        let one = cron("0 1 * * *");
        let eve = at(zone_name, 2024, 9, 7, 22, 0);
        assert_eq!(next(&both, zone_name, eve), "2024-09-08 01:30 -03:00");
        assert_eq!(next(&one, zone_name, eve), "2024-09-08 01:00 -03:00");
    }

    #[test]
    fn every_n_hours_counts_real_hours_from_the_first_run() {
        let ny = "America/New_York";
        let from = at(ny, 2026, 3, 7, 22, 0);
        let every = When::Hours {
            every: 3,
            from_ms: from,
        };
        // Before the first: the first.
        assert_eq!(next_after(&every, ny, from - 1).unwrap(), Some(from));
        // Across the spring-forward night, 3 real hours each: 01:00, then 05:00 (EDT).
        assert_eq!(next(&every, ny, from), "2026-03-08 01:00 -05:00");
        assert_eq!(
            next(&every, ny, at(ny, 2026, 3, 8, 1, 0)),
            "2026-03-08 05:00 -04:00"
        );
        // Late (the server was off): the next slot on its rhythm, not three hours from now.
        assert_eq!(
            next_after(&every, ny, from + 7 * HOUR + 1).unwrap(),
            Some(from + 9 * HOUR)
        );
    }

    #[test]
    fn a_one_off_runs_once() {
        let at_ms = 1_800_000_000_000;
        let once = When::Once { at_ms };
        assert_eq!(next_after(&once, "UTC", at_ms - 1).unwrap(), Some(at_ms));
        assert_eq!(next_after(&once, "UTC", at_ms).unwrap(), None);
    }

    #[test]
    fn cron_reads_ranges_lists_steps_names_and_either_day() {
        let utc = "UTC";
        // 2026-11-02 is a Monday.
        let mon = at(utc, 2026, 11, 2, 0, 0);
        assert_eq!(
            next(&cron("*/20 9-10 * * MON-FRI"), utc, mon),
            "2026-11-02 09:00 +00:00"
        );
        assert_eq!(
            next(
                &cron("*/20 9-10 * * MON-FRI"),
                utc,
                at(utc, 2026, 11, 2, 10, 40)
            ),
            "2026-11-03 09:00 +00:00"
        );
        assert_eq!(
            next(&cron("5,35 */6 * * *"), utc, at(utc, 2026, 11, 2, 6, 5)),
            "2026-11-02 06:35 +00:00"
        );
        assert_eq!(
            next(&cron("0 12 * jan,jul *"), utc, mon),
            "2027-01-01 12:00 +00:00"
        );
        // `a/5`: from a to the end.
        assert_eq!(
            next(&cron("50/5 * * * *"), utc, at(utc, 2026, 11, 2, 0, 56)),
            "2026-11-02 01:50 +00:00"
        );
        // Sunday is 0 and 7.
        assert_eq!(
            next(&cron("0 8 * * 7"), utc, mon),
            "2026-11-08 08:00 +00:00"
        );
        assert_eq!(
            next(&cron("0 8 * * sun"), utc, mon),
            "2026-11-08 08:00 +00:00"
        );
        // Both day fields: either (the 15th, or any Friday).
        assert_eq!(
            next(&cron("0 9 15 * 5"), utc, mon),
            "2026-11-06 09:00 +00:00"
        );
        assert_eq!(
            next(&cron("0 9 15 * 5"), utc, at(utc, 2026, 11, 13, 10, 0)),
            "2026-11-15 09:00 +00:00"
        );
        // One day field: only it.
        assert_eq!(
            next(&cron("0 9 15 * *"), utc, mon),
            "2026-11-15 09:00 +00:00"
        );
        // 29 February: the next leap year.
        assert_eq!(
            next(&cron("0 0 29 2 *"), utc, mon),
            "2028-02-29 00:00 +00:00"
        );
        // Never: 31 February.
        assert_eq!(next_after(&cron("0 0 31 2 *"), utc, mon).unwrap(), None);
        // Macros.
        assert_eq!(next(&cron("@weekly"), utc, mon), "2026-11-08 00:00 +00:00");
        assert_eq!(next(&cron("@hourly"), utc, mon), "2026-11-02 01:00 +00:00");
        // What doesn't read.
        // A step past the field's end: only its first value.
        assert_eq!(
            next(&cron("1/4294967295 * * * *"), utc, mon),
            "2026-11-02 00:01 +00:00"
        );
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
            "* * * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad:?} read");
        }
    }

    #[test]
    fn late_runs_follow_t3() {
        let due = 1_800_000_000_000;
        // Fixed times: up to 10 minutes late still go; later, skipped.
        for when in [
            When::Daily { at: 0 },
            When::Weekdays { at: 0 },
            When::Weekly {
                days: vec![1],
                at: 0,
            },
            cron("0 9 * * *"),
        ] {
            assert!(still_due(&when, due, due + LATE_MS));
            assert!(!still_due(&when, due, due + LATE_MS + 1));
        }
        // A one-off and every N hours go however late (once).
        assert!(still_due(
            &When::Once { at_ms: due },
            due,
            due + 7 * 24 * HOUR
        ));
        assert!(still_due(
            &When::Hours {
                every: 2,
                from_ms: due
            },
            due,
            due + 7 * 24 * HOUR
        ));
    }

    #[test]
    fn what_a_schedule_may_be() {
        let now = at("Europe/London", 2026, 11, 2, 12, 0);
        let thread = || Target::Thread("t1".into());
        assert!(check(&spec(When::Daily { at: 9 * 60 }, thread()), now, None).is_ok());
        assert!(check(&spec(When::Once { at_ms: now + HOUR }, fresh()), now, None).is_ok());
        assert_eq!(
            check(&spec(When::Once { at_ms: now - HOUR }, thread()), now, None).unwrap_err(),
            "That time has passed."
        );
        assert_eq!(
            check(
                &ScheduleSpec {
                    prompt: "  ".into(),
                    ..spec(When::Daily { at: 0 }, thread())
                },
                now,
                None
            )
            .unwrap_err(),
            "The prompt is empty."
        );
        assert!(
            check(
                &ScheduleSpec {
                    zone: "Nowhere/Land".into(),
                    ..spec(When::Daily { at: 0 }, thread())
                },
                now,
                None
            )
            .is_err()
        );
        assert!(check(&spec(When::Daily { at: 24 * 60 }, thread()), now, None).is_err());
        assert!(
            check(
                &spec(
                    When::Weekly {
                        days: vec![],
                        at: 0
                    },
                    thread()
                ),
                now,
                None
            )
            .is_err()
        );
        assert!(
            check(
                &spec(
                    When::Weekly {
                        days: vec![8],
                        at: 0
                    },
                    thread()
                ),
                now,
                None
            )
            .is_err()
        );
        assert!(check(&spec(cron("0 0 31 2 *"), fresh()), now, None).is_err());
        assert!(check(&spec(cron("nonsense"), fresh()), now, None).is_err());
        // A one-off past its time (waiting on its thread) may have its prompt changed, not be
        // set to a time that's passed.
        let late = spec(When::Once { at_ms: now - HOUR }, thread());
        assert!(
            check(
                &ScheduleSpec {
                    prompt: "Check CI again".into(),
                    ..late.clone()
                },
                now,
                Some(&late)
            )
            .is_ok()
        );
        assert!(
            check(
                &late,
                now,
                Some(&spec(When::Once { at_ms: now + HOUR }, thread()))
            )
            .is_err()
        );
    }

    /// At most once a day, into a thread or starting one: a cron with one time of day, a
    /// daily, weekday or weekly time, or every 24 hours. One-offs are as they were.
    #[test]
    fn runs_are_at_least_a_day_apart() {
        let now = at("Europe/London", 2026, 11, 2, 12, 0);
        for target in [Target::Thread("t1".into()), fresh()] {
            let ok = |when: When| check(&spec(when, target.clone()), now, None);
            let refused = |when: When| check(&spec(when, target.clone()), now, None).unwrap_err();
            for when in [
                When::Daily { at: 9 * 60 },
                When::Weekdays { at: 9 * 60 },
                When::Weekly {
                    days: vec![1, 3, 5],
                    at: 18 * 60,
                },
                cron("0 9 * * *"),
                cron("30 8 * * 1-5"),
                cron("0 9 15 * 5"),
                cron("@daily"),
                cron("@weekly"),
                When::Hours {
                    every: 24,
                    from_ms: now,
                },
                When::Once { at_ms: now + HOUR },
            ] {
                assert_eq!(ok(when.clone()), Ok(()), "{when:?}");
            }
            // Every few hours, twice a day, two times an hour apart on one day, hourly, every
            // 30 minutes, every 3 hours.
            for when in [
                cron("0 */4 * * 1-5"),
                cron("0 9,21 * * *"),
                cron("0 0,23 * * 1"),
                cron("0,30 9 * * *"),
                cron("@hourly"),
                cron("*/30 * * * *"),
                When::Hours {
                    every: 3,
                    from_ms: now,
                },
                When::Hours {
                    every: 0,
                    from_ms: now,
                },
            ] {
                assert_eq!(refused(when.clone()), TOO_OFTEN, "{when:?}");
            }
        }
        // Kept as it was (a prompt edit), it's still too often.
        let every3 = spec(
            When::Hours {
                every: 3,
                from_ms: now,
            },
            Target::Thread("t1".into()),
        );
        assert_eq!(
            check(
                &ScheduleSpec {
                    prompt: "Check CI again".into(),
                    ..every3.clone()
                },
                now,
                Some(&every3)
            )
            .unwrap_err(),
            TOO_OFTEN
        );
        assert_eq!(often_enough(&cron("0 */4 * * 1-5")).unwrap_err(), TOO_OFTEN);
        // A cron that doesn't read says why, not that it's too often.
        assert!(
            often_enough(&cron("* 24 * * *"))
                .unwrap_err()
                .contains("hour field")
        );
    }

    /// A daily time is a day apart on the clock, so a change of the clocks never refuses it,
    /// though it's 23 real hours from the one before when they go forward, and less when the
    /// time is one they skip.
    #[test]
    fn the_day_apart_is_on_the_clock_across_a_change_of_the_clocks() {
        let ny = "America/New_York";
        // The day before spring forward (8 March 2026), and before fall back (1 November).
        for now in [at(ny, 2026, 3, 7, 12, 0), at(ny, 2026, 10, 31, 12, 0)] {
            for at_min in [9 * 60, 2 * 60 + 30, 90] {
                let s = ScheduleSpec {
                    zone: ny.into(),
                    ..spec(When::Daily { at: at_min }, fresh())
                };
                assert_eq!(check(&s, now, None), Ok(()), "{at_min}");
            }
        }
        // 02:30 on the day they skip it runs at 03:30, then 02:30 the next day: 23 hours on.
        let half_two = When::Daily { at: 2 * 60 + 30 };
        let first = next_after(&half_two, ny, at(ny, 2026, 3, 7, 12, 0))
            .unwrap()
            .unwrap();
        let second = next_after(&half_two, ny, first).unwrap().unwrap();
        assert_eq!(second - first, 23 * HOUR);
        // Lord Howe's clocks go forward half an hour (02:00 → 02:30) on 4 October 2026, and
        // Santiago's skip from midnight to 01:00: daily times there are fine.
        let lord_howe = |when| ScheduleSpec {
            zone: "Australia/Lord_Howe".into(),
            ..spec(when, fresh())
        };
        let may = at("Australia/Lord_Howe", 2026, 5, 1, 12, 0);
        assert_eq!(
            check(&lord_howe(When::Daily { at: 2 * 60 + 15 }), may, None),
            Ok(())
        );
        assert_eq!(
            check(&lord_howe(cron("0 * * * *")), may, None).unwrap_err(),
            TOO_OFTEN
        );
        let santiago = ScheduleSpec {
            zone: "America/Santiago".into(),
            ..spec(cron("30 0 * * *"), fresh())
        };
        assert_eq!(
            check(&santiago, at("America/Santiago", 2024, 9, 1, 12, 0), None),
            Ok(())
        );
    }

    #[test]
    fn rows_say_when_and_how_it_went() {
        use flotilla_proto::{Run, RunOutcome, Schedule};
        let ny = "America/New_York";
        // Wednesday 4 November 2026, 12:00 in New York.
        let now = at(ny, 2026, 11, 4, 12, 0);
        assert_eq!(
            when_words(at(ny, 2026, 11, 4, 18, 30), now, ny),
            "today 18:30"
        );
        assert_eq!(
            when_words(at(ny, 2026, 11, 5, 9, 0), now, ny),
            "tomorrow 09:00"
        );
        assert_eq!(
            when_words(at(ny, 2026, 11, 3, 9, 0), now, ny),
            "yesterday 09:00"
        );
        assert_eq!(when_words(at(ny, 2026, 11, 7, 9, 0), now, ny), "Sat 09:00");
        assert_eq!(
            when_words(at(ny, 2026, 11, 16, 9, 0), now, ny),
            "Mon 16 Nov 09:00"
        );
        assert_eq!(
            when_words(at(ny, 2027, 1, 4, 9, 0), now, ny),
            "Mon 4 Jan 2027 09:00"
        );
        let s = Schedule {
            id: "s".into(),
            prompt: "p".into(),
            target: Target::Thread("t".into()),
            when: When::Daily { at: 9 * 60 },
            zone: ny.into(),
            paused: false,
            next_ms: Some(at(ny, 2026, 11, 5, 9, 0)),
            waiting_ms: None,
            last: Some(Run {
                at_ms: at(ny, 2026, 11, 4, 9, 0),
                outcome: RunOutcome::Sent,
                thread: None,
                why: None,
            }),
            runs: 3,
            from: "Mac".into(),
            created_ms: 0,
            updated_ms: 0,
        };
        assert_eq!(summary(&s, now, ny), "Daily at 09:00 · next tomorrow 09:00");
        assert_eq!(
            last_words(&s, now, ny).as_deref(),
            Some("Last sent today 09:00")
        );
        // Set in another zone: it says which, and the next time is in this one's.
        assert_eq!(
            summary(&s, now, "Europe/London"),
            "Daily at 09:00 (America/New_York) · next tomorrow 14:00"
        );
        assert_eq!(
            summary(
                &Schedule {
                    paused: true,
                    next_ms: None,
                    ..s.clone()
                },
                now,
                ny
            ),
            "Daily at 09:00 · paused"
        );
        assert_eq!(
            summary(
                &Schedule {
                    waiting_ms: Some(now),
                    ..s.clone()
                },
                now,
                ny
            ),
            "Daily at 09:00 · waiting for the turn to end"
        );
        let failed = Run {
            at_ms: at(ny, 2026, 11, 4, 9, 0),
            outcome: RunOutcome::Failed,
            thread: None,
            why: Some("The thread is archived.".into()),
        };
        assert_eq!(
            last_words(
                &Schedule {
                    last: Some(failed.clone()),
                    ..s.clone()
                },
                now,
                ny
            )
            .as_deref(),
            Some("Didn't run today 09:00: the thread is archived.")
        );
        // A one-off: its time, then that it went.
        let once = Schedule {
            when: When::Once {
                at_ms: at(ny, 2026, 11, 5, 9, 0),
            },
            last: None,
            ..s.clone()
        };
        assert_eq!(summary(&once, now, ny), "Once · tomorrow 09:00");
        let sent = Schedule {
            next_ms: None,
            last: Some(Run {
                at_ms: at(ny, 2026, 11, 4, 9, 1),
                outcome: RunOutcome::Sent,
                thread: None,
                why: None,
            }),
            ..once.clone()
        };
        assert_eq!(
            (summary(&sent, now, ny).as_str(), last_words(&sent, now, ny)),
            ("Once · sent today 09:01", None)
        );
        let didnt = Schedule {
            next_ms: None,
            last: Some(failed),
            ..once
        };
        assert_eq!(summary(&didnt, now, ny), "Once · didn't run today 09:00");
    }

    #[test]
    fn quick_times_are_an_hour_on_tomorrow_and_monday() {
        let ny = "America/New_York";
        // Wednesday 4 November 2026, 12:00:30.
        let now = at(ny, 2026, 11, 4, 12, 0) + 30_000;
        let picks: Vec<(String, String)> = quick_times(now, ny)
            .into_iter()
            .map(|(l, ms)| (l, show(ms, ny)))
            .collect();
        assert_eq!(
            picks,
            [
                (
                    "In 1 hour".to_string(),
                    "2026-11-04 13:00 -05:00".to_string()
                ),
                (
                    "Tomorrow 09:00".to_string(),
                    "2026-11-05 09:00 -05:00".to_string()
                ),
                (
                    "Monday 09:00".to_string(),
                    "2026-11-09 09:00 -05:00".to_string()
                ),
            ]
        );
        // On a Sunday, tomorrow is Monday: offered once.
        assert_eq!(quick_times(at(ny, 2026, 11, 8, 12, 0), ny).len(), 2);
        assert_eq!(
            local_parts(at(ny, 2026, 11, 4, 18, 5), ny),
            Some((2026, 11, 4, 18 * 60 + 5))
        );
        assert_eq!(
            local_ms(ny, 2026, 11, 4, 18 * 60 + 5).unwrap(),
            at(ny, 2026, 11, 4, 18, 5)
        );
        assert!(local_ms(ny, 2026, 2, 30, 0).is_err());
    }

    #[test]
    fn words_for_each_timing() {
        assert_eq!(describe(&When::Once { at_ms: 0 }), "Once");
        assert_eq!(
            describe(&When::Hours {
                every: 1,
                from_ms: 0
            }),
            "Every hour"
        );
        assert_eq!(
            describe(&When::Hours {
                every: 6,
                from_ms: 0
            }),
            "Every 6 hours"
        );
        assert_eq!(describe(&When::Daily { at: 9 * 60 }), "Daily at 09:00");
        assert_eq!(
            describe(&When::Weekdays { at: 8 * 60 + 30 }),
            "Weekdays at 08:30"
        );
        assert_eq!(
            describe(&When::Weekly {
                days: vec![1],
                at: 18 * 60
            }),
            "Mondays at 18:00"
        );
        assert_eq!(
            describe(&When::Weekly {
                days: vec![5, 1, 3],
                at: 18 * 60
            }),
            "Mon, Wed and Fri at 18:00"
        );
        assert_eq!(
            describe(&When::Weekly {
                days: vec![1, 2, 3, 4, 5],
                at: 0
            }),
            "Weekdays at 00:00"
        );
        assert_eq!(describe(&cron("0  9 * *   1-5")), "0 9 * * 1-5 (cron)");
        assert_eq!(parse_clock("9"), Some(540));
        assert_eq!(parse_clock("09:30"), Some(570));
        assert_eq!(parse_clock("9:30 pm"), Some(21 * 60 + 30));
        assert_eq!(parse_clock("12am"), Some(0));
        assert_eq!(parse_clock("12pm"), Some(720));
        for bad in ["24:00", "9:5", "9:60", "13pm", "x"] {
            assert_eq!(parse_clock(bad), None, "{bad}");
        }
    }
}
