//! Public preview links: a port's link with no sign-in token, so anyone who has it can open
//! it, for an hour, a day, a week or until you make it private (`thread.make_public`,
//! `thread.make_private`). The server keeps each one and makes it private when its time is up
//! (`flotilla_core::public`).

use serde::{Deserialize, Serialize};

/// How long a link stays public.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicFor {
    Hour,
    Day,
    Week,
    /// Until you make it private (or archive or delete the thread).
    UntilStopped,
}

impl PublicFor {
    /// The choices, in the order the menus show them.
    pub const ALL: [PublicFor; 4] = [
        PublicFor::Hour,
        PublicFor::Day,
        PublicFor::Week,
        PublicFor::UntilStopped,
    ];

    /// How long, in ms; None until it's stopped.
    pub fn millis(self) -> Option<i64> {
        const HOUR: i64 = 3600 * 1000;
        match self {
            PublicFor::Hour => Some(HOUR),
            PublicFor::Day => Some(24 * HOUR),
            PublicFor::Week => Some(7 * 24 * HOUR),
            PublicFor::UntilStopped => None,
        }
    }

    /// As a menu lists it.
    pub fn label(self) -> &'static str {
        match self {
            PublicFor::Hour => "1 hour",
            PublicFor::Day => "1 day",
            PublicFor::Week => "1 week",
            PublicFor::UntilStopped => "Until I stop it",
        }
    }

    /// After "Public": "for 1 hour", or "until you stop it".
    pub fn words(self) -> &'static str {
        match self {
            PublicFor::Hour => "for 1 hour",
            PublicFor::Day => "for 1 day",
            PublicFor::Week => "for 1 week",
            PublicFor::UntilStopped => "until you stop it",
        }
    }
}

/// A port's link while anyone may open it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicLink {
    pub port: u16,
    /// The link, with no token.
    pub url: String,
    pub since_ms: i64,
    /// When it goes private by itself; None: when you make it private.
    pub until_ms: Option<i64>,
}

impl PublicLink {
    /// How long it has left, in words ("58 min left"); None when it lasts until it's stopped.
    pub fn left(&self, now_ms: i64) -> Option<String> {
        self.until_ms.map(|until| time_left(until - now_ms))
    }
}

/// `ms` left, in words, rounded up: "less than a minute left", "58 min left", "23 h left",
/// "6 days left"; "ending" once it's up (the server makes it private within moments).
pub fn time_left(ms: i64) -> String {
    const MIN: i64 = 60 * 1000;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const TWO_DAYS: i64 = 2 * DAY;
    let up = |unit: i64| (ms + unit - 1) / unit;
    match ms {
        ..=0 => "ending".into(),
        1..MIN => "less than a minute left".into(),
        MIN..HOUR => format!("{} min left", up(MIN)),
        // Up to two days in hours ("47 h left"), so a day's link counts down by the hour.
        HOUR..=TWO_DAYS => format!("{} h left", up(HOUR).min(48)),
        _ => match up(DAY) {
            1 => "1 day left".into(),
            n => format!("{n} days left"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_left_rounds_up_in_the_largest_unit_that_reads() {
        const MIN: i64 = 60 * 1000;
        const HOUR: i64 = 60 * MIN;
        const DAY: i64 = 24 * HOUR;
        for (ms, words) in [
            (-5, "ending"),
            (0, "ending"),
            (1, "less than a minute left"),
            (59_999, "less than a minute left"),
            (MIN, "1 min left"),
            (MIN + 1, "2 min left"),
            (58 * MIN, "58 min left"),
            (HOUR - 1, "60 min left"),
            (HOUR, "1 h left"),
            (HOUR + 1, "2 h left"),
            (DAY, "24 h left"),
            (DAY - 1, "24 h left"),
            (2 * DAY, "48 h left"),
            (2 * DAY + 1, "3 days left"),
            (7 * DAY, "7 days left"),
        ] {
            assert_eq!(time_left(ms), words, "{ms}");
        }
    }

    #[test]
    fn each_choice_lasts_as_long_as_it_says() {
        assert_eq!(PublicFor::Hour.millis(), Some(3_600_000));
        assert_eq!(PublicFor::Day.millis(), Some(86_400_000));
        assert_eq!(PublicFor::Week.millis(), Some(604_800_000));
        assert_eq!(PublicFor::UntilStopped.millis(), None);
        let link = PublicLink {
            port: 3000,
            url: "https://x-3000.example.com".into(),
            since_ms: 0,
            until_ms: None,
        };
        assert_eq!(link.left(5), None);
        assert_eq!(
            PublicLink {
                until_ms: Some(3_600_000),
                ..link
            }
            .left(0)
            .as_deref(),
            Some("1 h left")
        );
    }
}
