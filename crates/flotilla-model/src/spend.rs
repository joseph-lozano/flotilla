//! How the clients show what the agents spent (`flotilla_proto::spend`), as T3 Code's Usage
//! does: the last 30 days (its default period) in the device's own days, every token and what
//! it would cost at list prices, what the cache saved, each model's share and its trend. A
//! model with no known price shows its tokens and no cost; a total leaves it out.

use flotilla_proto::{AgentUsage, Speed, TokenTotals, UsageCost, UsageHour};

/// T3's default period.
pub const DAYS: usize = 30;

/// What a period of use comes to, for every model or one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub tokens: TokenTotals,
    /// What the priced requests cost; None when none of them is priced.
    pub cost: Option<UsageCost>,
    /// What the Fast and Ultrafast requests cost, all of it (`cost.premium` is what they
    /// added over Standard).
    pub fast: f64,
    pub requests: u64,
    /// Tokens of models with no known price.
    pub unpriced: u64,
    /// The models, the most costly first (then the most tokens).
    pub models: Vec<ModelSum>,
    /// Each day, oldest first: its cost (what's priced) and tokens.
    pub days: Vec<DaySum>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelSum {
    pub provider: String,
    pub model: String,
    pub tokens: TokenTotals,
    /// None when none of its requests is priced.
    pub cost: Option<UsageCost>,
    pub requests: u64,
}

impl ModelSum {
    /// What a million of its tokens cost, when it's priced (T3's "Per 1M tokens").
    pub fn per_million(&self) -> Option<f64> {
        let total = self.tokens.total();
        self.cost
            .filter(|_| total > 0)
            .map(|c| c.total() / total as f64 * 1e6)
    }
}

/// Of the input, the share read from the cache (a cache write is a miss: it was all sent).
pub fn cache_hit(t: &TokenTotals) -> Option<f64> {
    let input = t.input + t.cache_read + t.cache_write;
    (input > 0).then(|| t.cache_read as f64 / input as f64)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DaySum {
    /// The day, as the device's days count (`day_of` gives each quarter hour's).
    pub day: i64,
    pub cost: f64,
    pub tokens: u64,
}

fn add_cost(into: &mut Option<UsageCost>, c: &Option<UsageCost>) {
    if let Some(c) = c {
        into.get_or_insert_with(UsageCost::default).add(c);
    }
}

/// The last [`DAYS`] days up to `today`, in the device's days: `day_of` gives a quarter hour's day (a
/// number that counts days), `today` is today's. `only`: one model (its harness and name).
pub fn summarize(
    u: &AgentUsage,
    today: i64,
    day_of: impl Fn(i64) -> i64,
    only: Option<(&str, &str)>,
) -> Summary {
    let first = today - DAYS as i64 + 1;
    let mut s = Summary {
        days: (first..=today)
            .map(|day| DaySum {
                day,
                ..DaySum::default()
            })
            .collect(),
        ..Summary::default()
    };
    let rows = u
        .hours
        .iter()
        .filter(|h| only.is_none_or(|(p, m)| h.provider == p && h.model == m));
    for h in rows {
        let day = day_of(h.hour_ms);
        if day < first || day > today {
            continue;
        }
        add(&mut s, h);
        let d = &mut s.days[(day - first) as usize];
        d.tokens += h.tokens.total();
        d.cost += h.cost.map_or(0., |c| c.total());
        let m = match s
            .models
            .iter_mut()
            .position(|m| m.provider == h.provider && m.model == h.model)
        {
            Some(i) => &mut s.models[i],
            None => {
                s.models.push(ModelSum {
                    provider: h.provider.clone(),
                    model: h.model.clone(),
                    ..ModelSum::default()
                });
                s.models.last_mut().unwrap()
            }
        };
        m.tokens.add(&h.tokens);
        m.requests += h.requests;
        add_cost(&mut m.cost, &h.cost);
    }
    s.models.sort_by(|a, b| {
        let cost = |m: &ModelSum| m.cost.map_or(0., |c| c.total());
        cost(b)
            .total_cmp(&cost(a))
            .then(b.tokens.total().cmp(&a.tokens.total()))
            .then(a.model.cmp(&b.model))
    });
    s
}

fn add(s: &mut Summary, h: &UsageHour) {
    s.tokens.add(&h.tokens);
    s.requests += h.requests;
    match &h.cost {
        Some(c) => {
            add_cost(&mut s.cost, &h.cost);
            if h.speed != Speed::Standard {
                s.fast += c.total();
            }
        }
        None => s.unpriced += h.tokens.total(),
    }
}

impl Summary {
    /// Anything to show: some use in the period.
    pub fn any(&self) -> bool {
        self.tokens.total() > 0
    }
}

/// A count of tokens as T3's Usage writes it: three figures at most ("568m", "45.8m", "9.53k").
pub fn tokens(n: u64) -> String {
    let v = n as f64;
    let (scaled, unit) = match n {
        n if n >= 1_000_000_000_000 => (v / 1e12, "t"),
        n if n >= 1_000_000_000 => (v / 1e9, "b"),
        n if n >= 1_000_000 => (v / 1e6, "m"),
        n if n >= 1_000 => (v / 1e3, "k"),
        n => return n.to_string(),
    };
    let digits = if scaled >= 100. {
        0
    } else if scaled >= 10. {
        1
    } else {
        2
    };
    let s = format!("{scaled:.digits$}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        &s
    };
    format!("{s}{unit}")
}

/// Dollars as T3 writes them: "$1,234.56", "$0.42"; a cost under half a cent, "<$0.01".
pub fn usd(v: f64) -> String {
    if v > 0. && v < 0.005 {
        return "<$0.01".into();
    }
    let cents = (v * 100.).round() as i64;
    format!(
        "${}.{:02}",
        crate::limits::exact((cents / 100) as u64),
        cents % 100
    )
}

/// The account whose usage page shows a harness's use: Claude's for Claude Code, ChatGPT's
/// for Codex.
pub fn account_of(provider: &str) -> Option<&'static str> {
    match provider {
        "claude-code" => Some("claude"),
        "codex" => Some("chatgpt"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    fn hour(
        day: i64,
        provider: &str,
        model: &str,
        speed: Speed,
        tokens: u64,
        cost: Option<f64>,
    ) -> UsageHour {
        UsageHour {
            hour_ms: day * DAY + 3_600_000,
            provider: provider.into(),
            model: model.into(),
            speed,
            tokens: TokenTotals {
                input: tokens,
                ..Default::default()
            },
            requests: 1,
            cost: cost.map(|c| UsageCost {
                input: c,
                saved: c / 10.,
                premium: Some(if speed == Speed::Standard { 0. } else { c / 2. }),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn adds_up_the_last_thirty_days_by_model() {
        let today = 100;
        let u = AgentUsage {
            hours: vec![
                hour(
                    today - 30,
                    "codex",
                    "gpt-6-luna",
                    Speed::Standard,
                    999,
                    Some(9.),
                ),
                hour(
                    today - 29,
                    "codex",
                    "gpt-6-luna",
                    Speed::Standard,
                    100,
                    Some(1.),
                ),
                hour(
                    today,
                    "claude-code",
                    "claude-opus-5-5",
                    Speed::Fast,
                    200,
                    Some(4.),
                ),
                hour(
                    today,
                    "claude-code",
                    "claude-opus-5-5",
                    Speed::Standard,
                    300,
                    Some(2.),
                ),
                hour(
                    today - 3,
                    "opencode",
                    "big-pickle",
                    Speed::Standard,
                    5_000,
                    None,
                ),
            ],
            ..Default::default()
        };
        let s = summarize(&u, today, |ms| ms.div_euclid(DAY), None);
        // The 31st day back is out.
        assert_eq!(
            (s.tokens.total(), s.requests, s.unpriced, s.days.len()),
            (5_600, 4, 5_000, 30)
        );
        let c = s.cost.unwrap();
        assert_eq!((c.total(), c.premium, s.fast), (7., Some(2.), 4.));
        // A premium that isn't known anywhere isn't known in the whole.
        let mut unknown = u.clone();
        unknown.hours[2].cost.as_mut().unwrap().premium = None;
        assert_eq!(
            summarize(&unknown, today, |ms| ms.div_euclid(DAY), None)
                .cost
                .unwrap()
                .premium,
            None
        );
        assert!((c.saved - 0.7).abs() < 1e-9);
        let models: Vec<(&str, Option<f64>, u64)> = s
            .models
            .iter()
            .map(|m| {
                (
                    m.model.as_str(),
                    m.cost.map(|c| c.total()),
                    m.tokens.total(),
                )
            })
            .collect();
        assert_eq!(
            models,
            [
                ("claude-opus-5-5", Some(6.), 500),
                ("gpt-6-luna", Some(1.), 100),
                ("big-pickle", None, 5_000)
            ]
        );
        assert_eq!(
            (
                s.days[0].day,
                s.days[0].cost,
                s.days[29].cost,
                s.days[29].tokens
            ),
            (today - 29, 1., 6., 500)
        );
        // One model: its own days and totals.
        let one = summarize(
            &u,
            today,
            |ms| ms.div_euclid(DAY),
            Some(("opencode", "big-pickle")),
        );
        assert_eq!(
            (
                one.cost,
                one.unpriced,
                one.days[26].tokens,
                one.models.len()
            ),
            (None, 5_000, 5_000, 1)
        );
        assert_eq!(one.models[0].per_million(), None);
        assert_eq!(s.models[0].per_million(), Some(6. / 500. * 1e6));
        assert!(!summarize(&AgentUsage::default(), today, |ms| ms / DAY, None).any());
    }

    #[test]
    fn writes_dollars_and_cache_hits_as_t3_does() {
        assert_eq!(tokens(568_300_000), "568m");
        assert_eq!(tokens(141_900_000), "142m");
        assert_eq!(tokens(45_800_000), "45.8m");
        assert_eq!(tokens(9_530), "9.53k");
        assert_eq!(tokens(2_000_000), "2m");
        assert_eq!(tokens(950), "950");
        assert_eq!(usd(1234.567), "$1,234.57");
        assert_eq!(usd(0.42), "$0.42");
        assert_eq!(usd(0.004), "<$0.01");
        assert_eq!(usd(0.), "$0.00");
        assert_eq!(
            cache_hit(&TokenTotals {
                input: 10,
                cache_read: 80,
                cache_write: 10,
                ..Default::default()
            }),
            Some(0.8)
        );
        assert_eq!(
            cache_hit(&TokenTotals {
                output: 5,
                ..Default::default()
            }),
            None
        );
    }
}
