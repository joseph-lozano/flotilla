//! Display strings.

use chrono::{DateTime, Datelike, Local, Utc};

pub fn provider_name(key: &str) -> &str {
    match key {
        "claude-code" => "Claude Code",
        "codex" => "Codex",
        "pi" => "Pi",
        "prime-agent" => "Prime",
        "opencode" => "OpenCode",
        "kimi" => "Kimi",
        "mistral" => "Mistral",
        other => other,
    }
}

pub fn monogram(key: &str) -> String {
    provider_name(key)
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_default()
}

/// "now", "3m", "1h", "yday", "12d", then a date.
pub fn ago(ms: i64, now_ms: i64) -> String {
    let secs = (now_ms - ms).max(0) / 1000;
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        86_400..172_800 => "yday".into(),
        172_800..2_592_000 => format!("{}d", secs / 86_400),
        _ => DateTime::<Utc>::from_timestamp_millis(ms)
            .map(|d| d.with_timezone(&Local).format("%b %-d").to_string())
            .unwrap_or_default(),
    }
}

/// A file's size in binary units, as boat's limits are: "12 B", "340 KB", "1.2 MB".
pub fn bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = flotilla_model::attach::MB;
    match n {
        0..KB => format!("{n} B"),
        KB..MB => format!("{} KB", n / KB),
        _ if n < 10 * MB => format!("{:.1} MB", n as f64 / MB as f64),
        _ => format!("{} MB", n / MB),
    }
}

/// "0:41", "12:03", "1:02:03".
pub fn elapsed(ms: i64, now_ms: i64) -> String {
    let s = (now_ms - ms).max(0) / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// `ago` as a phrase: "just now", "5m ago", "yesterday", "Oct 3".
pub fn ago_phrase(ms: i64, now_ms: i64) -> String {
    match ago(ms, now_ms).as_str() {
        "now" => "just now".into(),
        "yday" => "yesterday".into(),
        s if s.ends_with(['m', 'h', 'd']) && s.starts_with(|c: char| c.is_ascii_digit()) => {
            format!("{s} ago")
        }
        s => s.into(),
    }
}

/// Whole minutes from now until an RFC 3339 instant (negative once it has passed).
pub fn minutes_until(iso: &str, now_ms: i64) -> Option<i64> {
    let at = DateTime::parse_from_rfc3339(iso).ok()?.timestamp_millis();
    Some((at - now_ms) / 60_000)
}

/// A local date and time for an RFC 3339 instant, e.g. "Oct 29, 4:52 PM".
pub fn date_time(iso: &str) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(iso)
        .ok()?
        .with_timezone(&Local);
    Some(t.format("%b %-d, %-I:%M %p").to_string())
}

/// A local date for an RFC 3339 instant, e.g. "Nov 30".
pub fn date(iso: &str) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(iso)
        .ok()?
        .with_timezone(&Local);
    Some(t.format("%b %-d").to_string())
}

/// When a message was sent, short: "3:04 PM" today, "Yesterday 3:04 PM", "Oct 27, 3:04 PM",
/// and the year too when it isn't this one.
pub fn message_time(ms: i64, now_ms: i64) -> String {
    let (Some(t), Some(now)) = (local(ms), local(now_ms)) else {
        return String::new();
    };
    let clock = t.format("%-I:%M %p");
    if t.date_naive() == now.date_naive() {
        clock.to_string()
    } else if now.date_naive().pred_opt() == Some(t.date_naive()) {
        format!("Yesterday {clock}")
    } else if t.year() == now.year() {
        format!("{}, {clock}", t.format("%b %-d"))
    } else {
        format!("{}, {clock}", t.format("%b %-d %Y"))
    }
}

/// The full date and time, e.g. "Thursday, October 29, 2026 at 3:04 PM".
pub fn full_date_time(ms: i64) -> String {
    local(ms)
        .map(|t| t.format("%A, %B %-d, %Y at %-I:%M %p").to_string())
        .unwrap_or_default()
}

fn local(ms: i64) -> Option<DateTime<Local>> {
    DateTime::<Utc>::from_timestamp_millis(ms).map(|d| d.with_timezone(&Local))
}

/// A local clock time for an RFC 3339 instant, e.g. "4:52 PM".
pub fn clock(iso: &str) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(iso)
        .ok()?
        .with_timezone(&Local);
    Some(t.format("%-I:%M %p").to_string())
}

/// When a limit resets: its time today or within a day ("3:00 PM"), else its day too
/// ("Wed 7:09 PM").
pub fn reset_at(iso: &str, now_ms: i64) -> Option<String> {
    soon_at(
        DateTime::parse_from_rfc3339(iso).ok()?.timestamp_millis(),
        now_ms,
    )
}

/// A moment ahead (ms since the epoch) as [`reset_at`] writes it: "3:00 PM" within a day,
/// "Wed 7:09 PM" after.
pub fn soon_at(at_ms: i64, now_ms: i64) -> Option<String> {
    let t = DateTime::from_timestamp_millis(at_ms)?.with_timezone(&Local);
    let soon = at_ms - now_ms < 86_400_000;
    Some(
        t.format(if soon { "%-I:%M %p" } else { "%a %-I:%M %p" })
            .to_string(),
    )
}

/// A reasoning effort as shown beside a model: none for "none", which reads like "no model".
pub fn effort(effort: Option<&str>) -> Option<String> {
    effort
        .filter(|e| !e.is_empty() && *e != "none")
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(bytes(12), "12 B");
        assert_eq!(bytes(348_160), "340 KB");
        assert_eq!(bytes(1_258_291), "1.2 MB");
        // Under boat's 25 MB limit, as boat counts it.
        assert_eq!(bytes(26_000_000), "24 MB");
    }

    #[test]
    fn formats_times() {
        let now = 1_000_000_000_000;
        assert_eq!(ago(now - 5_000, now), "now");
        assert_eq!(ago(now - 180_000, now), "3m");
        assert_eq!(ago(now - 3_700_000, now), "1h");
        assert_eq!(ago(now - 90_000_000, now), "yday");
        assert_eq!(ago(now - 12 * 86_400_000, now), "12d");
        assert_eq!(elapsed(now - 41_000, now), "0:41");
        assert_eq!(elapsed(now - 3_723_000, now), "1:02:03");
        assert_eq!(monogram("prime-agent"), "P");
        assert_eq!(ago_phrase(now - 180_000, now), "3m ago");
        let day = 86_400_000;
        assert!(
            !message_time(now, now).contains(','),
            "{}",
            message_time(now, now)
        );
        assert!(message_time(now - day, now).starts_with("Yesterday "));
        assert!(
            message_time(now - 400 * day, now)
                .contains(&local(now - 400 * day).unwrap().year().to_string())
        );
        assert_eq!(ago_phrase(now - 5_000, now), "just now");
        assert_eq!(ago_phrase(now - 90_000_000, now), "yesterday");
    }
}
