//! `flotilla schedule`: scheduled prompts on the server (`flotilla_proto::schedule`). They run
//! where the engine runs, so this goes through the running server (or the app's own).

use clap::{Args, Subcommand};
use flotilla_model::schedule as sched;
use flotilla_proto::{Fresh, Schedule, ScheduleSpec, Target, When};

#[derive(Subcommand, Clone)]
pub enum ScheduleCmd {
    /// List the scheduled prompts
    List,
    /// Schedule a prompt: to a thread (`--thread`) or a new thread each run (`--provider`,
    /// `--model`), once (`--at`, `--in`) or at most daily (`--daily`, `--weekdays`, `--weekly`,
    /// `--cron`)
    Add(Box<Add>),
    /// Delete a scheduled prompt (its id, or the start of it)
    Rm { id: String },
    /// Pause a scheduled prompt
    Pause { id: String },
    /// Let a paused one run again, from now
    Resume { id: String },
    /// Run a scheduled prompt now, leaving its next run as it is
    Run { id: String },
}

#[derive(Args, Clone)]
pub struct Add {
    /// Send to this thread (its id, or the start of it), as a follow-up
    #[arg(long, conflicts_with_all = ["provider", "model"])]
    pub thread: Option<String>,
    /// A new thread each run: its agent and model (as `flotilla new` takes them)
    #[arg(long, requires = "model")]
    pub provider: Option<String>,
    #[arg(long, requires = "provider")]
    pub model: Option<String>,
    #[arg(long)]
    pub effort: Option<String>,
    /// boat environment name; defaults to the default environment
    #[arg(long = "env")]
    pub environment: Option<String>,
    /// Once, at `HH:MM` (today, or tomorrow once it's passed) or `YYYY-MM-DD HH:MM`
    #[arg(long, group = "when")]
    pub at: Option<String>,
    /// Once, after `30m`, `2h` or `1d`
    #[arg(long = "in", group = "when")]
    pub after: Option<String>,
    /// Daily at `HH:MM`
    #[arg(long, group = "when")]
    pub daily: Option<String>,
    /// Monday to Friday at `HH:MM`
    #[arg(long, group = "when")]
    pub weekdays: Option<String>,
    /// On these days at a time: `mon,wed@18:00`
    #[arg(long, group = "when")]
    pub weekly: Option<String>,
    /// A cron expression: minute hour day-of-month month day-of-week, one time of day
    #[arg(long, group = "when")]
    pub cron: Option<String>,
    /// The time zone its times are in (IANA); this machine's by default
    #[arg(long)]
    pub zone: Option<String>,
    pub prompt: String,
}

fn clock(s: &str) -> Result<u32, String> {
    sched::parse_clock(s).ok_or_else(|| format!("{s:?} isn't a time: use HH:MM"))
}

/// The timing asked for, at `now_ms` in `zone`.
pub fn when(a: &Add, zone: &str, now_ms: i64) -> Result<When, String> {
    if let Some(at) = &a.at {
        let (date, time) = match at.trim().split_once(' ') {
            Some((d, t)) => (Some(d), t),
            None => (None, at.trim()),
        };
        let minutes = clock(time)?;
        let at_ms = match date {
            Some(d) => {
                let parts: Vec<u32> = d.split('-').filter_map(|p| p.parse().ok()).collect();
                let [y, m, day] = parts[..] else {
                    return Err(format!("{d:?} isn't a date: use YYYY-MM-DD"));
                };
                sched::local_ms(zone, y as i32, m, day, minutes)?
            }
            None => {
                let (y, m, d, _) = sched::local_parts(now_ms, zone).ok_or("unknown time zone")?;
                let today = sched::local_ms(zone, y, m, d, minutes)?;
                if today > now_ms {
                    today
                } else {
                    sched::next_after(&When::Daily { at: minutes }, zone, now_ms)?
                        .ok_or("no such time")?
                }
            }
        };
        return Ok(When::Once { at_ms });
    }
    if let Some(after) = &a.after {
        let after = after.trim();
        let (n, unit) = after.split_at(
            after
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after.len()),
        );
        let n: i64 = n
            .parse()
            .map_err(|_| format!("{after:?}: use 30m, 2h or 1d"))?;
        let ms = match unit {
            "m" | "min" => n * 60_000,
            "h" => n * 3_600_000,
            "d" => n * 86_400_000,
            _ => return Err(format!("{after:?}: use 30m, 2h or 1d")),
        };
        return Ok(When::Once { at_ms: now_ms + ms });
    }
    if let Some(t) = &a.daily {
        return Ok(When::Daily { at: clock(t)? });
    }
    if let Some(t) = &a.weekdays {
        return Ok(When::Weekdays { at: clock(t)? });
    }
    if let Some(w) = &a.weekly {
        let (days, t) = w
            .split_once('@')
            .ok_or("--weekly takes days@time: mon,wed@18:00")?;
        let days = days
            .split(',')
            .map(|d| {
                let d = d.trim().to_ascii_lowercase();
                sched::DAY_NAMES
                    .iter()
                    .position(|n| n.to_ascii_lowercase().starts_with(&d) && d.len() >= 2)
                    .map(|i| i as u8 + 1)
                    .ok_or(format!("{d:?} isn't a day"))
            })
            .collect::<Result<Vec<u8>, String>>()?;
        return Ok(When::Weekly {
            days,
            at: clock(t)?,
        });
    }
    if let Some(c) = &a.cron {
        return Ok(When::Cron {
            expr: c.trim().to_string(),
        });
    }
    Err("When? --at, --in, --daily, --weekdays, --weekly or --cron".into())
}

/// What it would ask the server to keep. `thread`: the thread's id, when it sends to one;
/// `environment`: the default, when a new thread's isn't given.
pub fn spec(
    a: &Add,
    thread: Option<String>,
    environment: Option<String>,
    now_ms: i64,
) -> Result<ScheduleSpec, String> {
    let zone = a.zone.clone().unwrap_or_else(sched::here);
    let target = match (thread, &a.provider, &a.model) {
        (Some(t), _, _) => Target::Thread(t),
        (None, Some(provider), Some(model)) => Target::New(Fresh {
            environment: a
                .environment
                .clone()
                .or(environment)
                .ok_or("no default environment; pass --env")?,
            project_id: None,
            provider: provider.clone(),
            model: model.clone(),
            effort: a.effort.clone(),
            fast: false,
            machine_type: None,
            on_branch: None,
        }),
        _ => {
            return Err(
                "Where to? --thread, or --provider and --model for a new thread each run".into(),
            );
        }
    };
    let spec = ScheduleSpec {
        id: uuid::Uuid::new_v4().to_string(),
        prompt: a.prompt.clone(),
        target,
        when: when(a, &zone, now_ms)?,
        zone,
    };
    sched::check(&spec, now_ms, None)?;
    Ok(spec)
}

/// One schedule, as `list` shows it: its id, timing and next run, where to, how its last run
/// went, and its prompt's first line.
pub fn line(s: &Schedule, to: &str, now_ms: i64) -> String {
    let here = sched::here();
    let mut out = format!(
        "{}  {}  → {to}",
        short(&s.id),
        sched::summary(s, now_ms, &here)
    );
    if let Some(last) = sched::last_words(s, now_ms, &here) {
        out.push_str(&format!("  ({last})"));
    }
    out.push_str(&format!(
        "\n          {}",
        flotilla_model::util::ellipsize(&flotilla_model::util::one_line(&s.prompt), 100)
    ));
    out
}

/// An id's first 8 characters, as `list` shows it.
pub fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// The one whose id starts with `query`.
pub fn find<'a>(all: &'a [Schedule], query: &str) -> Result<&'a Schedule, String> {
    let found: Vec<&Schedule> = all.iter().filter(|s| s.id.starts_with(query)).collect();
    match found[..] {
        [one] => Ok(one),
        [] => Err(format!("no scheduled prompt {query:?}")),
        _ => Err(format!(
            "{query:?} matches {} scheduled prompts; use a longer prefix",
            found.len()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(subcommand)]
        cmd: ScheduleCmd,
    }

    fn add(args: &[&str]) -> Add {
        match T::try_parse_from(std::iter::once("schedule").chain(args.iter().copied()))
            .unwrap()
            .cmd
        {
            ScheduleCmd::Add(a) => *a,
            _ => panic!(),
        }
    }

    #[test]
    fn the_timings_it_takes() {
        let ny = "America/New_York";
        // Wednesday 4 November 2026, 12:00.
        let now = sched::local_ms(ny, 2026, 11, 4, 12 * 60).unwrap();
        let w = |args: &[&str]| when(&add(args), ny, now).unwrap();
        assert_eq!(
            w(&["add", "--thread", "t", "--at", "18:30", "p"]),
            When::Once {
                at_ms: now + 6 * 3_600_000 + 30 * 60_000
            }
        );
        // Passed today: tomorrow.
        assert_eq!(
            w(&["add", "--thread", "t", "--at", "09:00", "p"]),
            When::Once {
                at_ms: sched::local_ms(ny, 2026, 11, 5, 540).unwrap()
            }
        );
        assert_eq!(
            w(&["add", "--thread", "t", "--at", "2026-12-24 08:00", "p"]),
            When::Once {
                at_ms: sched::local_ms(ny, 2026, 12, 24, 480).unwrap()
            }
        );
        assert_eq!(
            w(&["add", "--thread", "t", "--in", "90m", "p"]),
            When::Once {
                at_ms: now + 90 * 60_000
            }
        );
        assert_eq!(
            w(&["add", "--thread", "t", "--daily", "9:00", "p"]),
            When::Daily { at: 540 }
        );
        assert_eq!(
            w(&["add", "--thread", "t", "--weekdays", "8:30", "p"]),
            When::Weekdays { at: 510 }
        );
        assert_eq!(
            w(&["add", "--thread", "t", "--weekly", "mon,Wed,su@18:00", "p"]),
            When::Weekly {
                days: vec![1, 3, 7],
                at: 1080
            }
        );
        assert_eq!(
            w(&["add", "--thread", "t", "--cron", "0 9 * * 1-5", "p"]),
            When::Cron {
                expr: "0 9 * * 1-5".into()
            }
        );
        assert!(
            when(
                &add(&["add", "--thread", "t", "--weekly", "funday@18:00", "p"]),
                ny,
                now
            )
            .is_err()
        );
        assert!(when(&add(&["add", "--thread", "t", "p"]), ny, now).is_err());
        // One timing at a time; a thread or a new thread, not both.
        assert!(
            T::try_parse_from([
                "schedule",
                "add",
                "--thread",
                "t",
                "--daily",
                "9:00",
                "--cron",
                "* * * * *",
                "p"
            ])
            .is_err()
        );
        assert!(
            T::try_parse_from([
                "schedule",
                "add",
                "--thread",
                "t",
                "--provider",
                "codex",
                "--model",
                "m",
                "--daily",
                "9:00",
                "p"
            ])
            .is_err()
        );
        // No more every N hours.
        assert!(
            T::try_parse_from(["schedule", "add", "--thread", "t", "--every", "3", "p"]).is_err()
        );
        // Checked as the server checks: more often than daily is refused, to a thread too.
        let new = add(&[
            "add",
            "--provider",
            "codex",
            "--model",
            "gpt-6-luna",
            "--cron",
            "0 */4 * * 1-5",
            "--zone",
            ny,
            "p",
        ]);
        assert_eq!(
            spec(&new, None, Some("base".into()), now).unwrap_err(),
            sched::TOO_OFTEN
        );
        let thread = add(&[
            "add",
            "--thread",
            "t",
            "--cron",
            "0 9,17 * * *",
            "--zone",
            ny,
            "p",
        ]);
        assert_eq!(
            spec(&thread, Some("t".into()), None, now).unwrap_err(),
            sched::TOO_OFTEN
        );
        let ok = add(&[
            "add",
            "--provider",
            "codex",
            "--model",
            "gpt-6-luna",
            "--daily",
            "9:00",
            "--zone",
            ny,
            "p",
        ]);
        let s = spec(&ok, None, Some("base".into()), now).unwrap();
        assert!(
            matches!(s.target, Target::New(Fresh { ref environment, .. }) if environment == "base")
        );
    }
}
