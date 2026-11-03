use chrono::{DateTime, SecondsFormat, TimeZone, Utc};

/// Now as an RFC 3339 UTC string with milliseconds, the format every TEXT time column uses.
pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

pub fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

pub fn iso_to_ms(s: &str) -> Option<i64> {
    parse_iso(s).map(|d| d.timestamp_millis())
}

/// Truncate to at most `max` chars, on a char boundary.
pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Like [`truncate_chars`], with a trailing ellipsis when something was cut.
pub fn ellipsize(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", s[..i].trim_end()),
        None => s.to_string(),
    }
}

/// Collapse runs of whitespace (including newlines) into single spaces.
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first line of a reply with words in it, as plain text, for one-line previews.
pub fn preview_line(text: &str) -> Option<String> {
    let line = text.lines().map(plain_text).find(|l| !l.is_empty())?;
    Some(ellipsize(&line, 160))
}

/// Markdown as the plain text a short preview shows, on one line: no heading, quote, list or
/// task marks; no bold, italic, strikethrough or code marks; links and images as their words;
/// a table's rows as their cells joined by " · " (its rule dropped); code fences dropped,
/// what's inside them kept. Headings, list items and table rows are set apart by " · " (a
/// line that ends a sentence needs none); a paragraph's lines join with a space.
pub fn plain_text(md: &str) -> String {
    let mut out = String::new();
    // The last line was a block of its own (a heading, item or row): what follows is apart.
    let mut apart = false;
    for line in md.lines() {
        let mut l = line.trim();
        if l.is_empty() {
            apart = true;
            continue;
        }
        if l.starts_with("```")
            || l.starts_with("~~~")
            || (l.starts_with('|') && l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')))
        {
            continue;
        }
        let start = l;
        loop {
            let before = l;
            let hashes = l.len() - l.trim_start_matches('#').len();
            if hashes > 0 && l[hashes..].chars().next().is_none_or(char::is_whitespace) {
                l = l[hashes..].trim_start();
            }
            l = l.strip_prefix('>').map_or(l, str::trim_start);
            for bullet in ["- ", "* ", "+ ", "[ ] ", "[x] ", "[X] "] {
                l = l.strip_prefix(bullet).map_or(l, str::trim_start);
            }
            let digits = l.len() - l.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            if digits > 0 && (l[digits..].starts_with(". ") || l[digits..].starts_with(") ")) {
                l = l[digits + 2..].trim_start();
            }
            if l == before {
                break;
            }
        }
        let block = l != start || l.starts_with('|');
        let l = if l.starts_with('|') {
            l.trim_matches('|')
                .split('|')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
                .join(" · ")
        } else {
            l.to_string()
        };
        let text = one_line(&inline_plain(&l));
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            let ended = out.ends_with(['.', '!', '?', ':', ';', '…']);
            out.push_str(if (apart || block) && !ended {
                " · "
            } else {
                " "
            });
        }
        out.push_str(&text);
        apart = block;
    }
    out
}

/// A line's inline Markdown as plain text: code spans as written without their backticks;
/// `[words](url)` and `![words](url)` as their words; `**`, `~~` and `*` only where they
/// pair up around words (so `2*3`, `*ptr` and an unmatched `**` stay). Underscores always
/// stay: in identifiers (`snake_case`, `__init__.py`) they're far likelier than emphasis.
fn inline_plain(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    // Code spans stand in as a placeholder each (their own, so one a link drops doesn't
    // shift the rest) while marks are paired across the whole line (`**Fixed `retry()`**`),
    // then go back as written.
    let mut text: Vec<char> = Vec::with_capacity(chars.len());
    let mut codes: Vec<&[char]> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '`' {
            let ticks = chars[i..].iter().take_while(|&&c| c == '`').count();
            let body = i + ticks;
            // The next run of exactly as many backticks closes it; none, and they're literal.
            let mut j = body;
            let mut close = None;
            while j < chars.len() {
                let run = chars[j..].iter().take_while(|&&c| c == '`').count();
                if run == ticks {
                    close = Some(j);
                    break;
                }
                j += run.max(1);
            }
            match (close, code_mark(codes.len())) {
                (Some(end), Some(mark)) => {
                    codes.push(&chars[body..end]);
                    text.push(mark);
                    i = end + ticks;
                }
                // More spans than placeholders: as written, unprotected.
                (Some(end), None) => {
                    text.extend(&chars[body..end]);
                    i = end + ticks;
                }
                (None, _) => {
                    text.extend(&chars[i..body]);
                    i = body;
                }
            }
            continue;
        }
        // A character from the placeholders' plane would pass for a span: it goes.
        if code_index(chars[i]).is_none() {
            text.push(chars[i]);
        }
        i += 1;
    }
    let mut out = String::with_capacity(line.len());
    for c in unmark(&text).chars() {
        match code_index(c).and_then(|n| codes.get(n)) {
            Some(code) => out.extend(*code),
            None => out.push(c),
        }
    }
    out
}

/// The placeholder for the `n`th code span while [`inline_plain`] pairs marks: a character
/// of Supplementary Private Use Area-A, which text all but never has.
const CODE_FIRST: u32 = 0xF0000;
const CODE_LAST: u32 = 0xFFFFD;

fn code_mark(n: usize) -> Option<char> {
    u32::try_from(n)
        .ok()
        .and_then(|n| CODE_FIRST.checked_add(n))
        .filter(|c| *c <= CODE_LAST)
        .and_then(char::from_u32)
}

fn code_index(c: char) -> Option<usize> {
    (CODE_FIRST..=CODE_LAST)
        .contains(&(c as u32))
        .then(|| (c as u32 - CODE_FIRST) as usize)
}

/// Text outside code: links as their words, and paired emphasis marks gone.
fn unmark(chars: &[char]) -> String {
    let mut linked = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '[' || (c == '!' && chars.get(i + 1) == Some(&'[')) {
            let open = if c == '!' { i + 1 } else { i };
            if let Some(close) = chars[open..]
                .iter()
                .position(|&c| c == ']')
                .map(|p| open + p)
                && chars.get(close + 1) == Some(&'(')
                && let Some(end) = chars[close + 1..]
                    .iter()
                    .position(|&c| c == ')')
                    .map(|p| close + 1 + p)
            {
                linked.extend(&chars[open + 1..close]);
                i = end + 1;
                continue;
            }
        }
        linked.push(c);
        i += 1;
    }
    let text = drop_pairs(&linked, &['*', '*']);
    let text = drop_pairs(&text, &['~', '~']);
    drop_pairs(&text, &['*']).into_iter().collect()
}

/// `text` without each pair of `mark`s that wraps something: an opening one followed by a
/// non-space, a closing one after a non-space. A lone `*` doesn't open inside a word, nor
/// close before one (`2*3*4` stays).
fn drop_pairs(text: &[char], mark: &[char]) -> Vec<char> {
    let n = mark.len();
    let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
    let space = |c: Option<&char>| c.is_none_or(|c| c.is_whitespace());
    let mut drop = vec![false; text.len()];
    let mut open: Option<usize> = None;
    let mut i = 0;
    while i + n <= text.len() {
        // A run of the mark's character longer than the mark isn't this mark (`***`, `*` in `**`).
        let at = text[i..i + n] == *mark
            && text.get(i + n) != Some(&mark[0])
            && (i == 0 || text[i - 1] != mark[0]);
        if !at {
            i += 1;
            continue;
        }
        let (before, after) = (i.checked_sub(1).and_then(|p| text.get(p)), text.get(i + n));
        let lone = n == 1;
        match open {
            Some(o) if !space(before) && !(lone && word(after)) => {
                drop[o..o + n].iter_mut().for_each(|d| *d = true);
                drop[i..i + n].iter_mut().for_each(|d| *d = true);
                open = None;
            }
            _ if !space(after) && !(lone && word(before)) => open = Some(i),
            _ => {}
        }
        i += n;
    }
    text.iter()
        .zip(drop)
        .filter(|(_, d)| !d)
        .map(|(c, _)| *c)
        .collect()
}

/// Hide secrets before anything is logged or shown: boat API keys (`boat_…`), device tokens
/// (`fdt_…`), Claude tokens (`sk-ant-…`), GitHub tokens (`ghp_…`, `gho_…`, `github_pat_…`…),
/// whatever follows `Bearer ` (an `Authorization` header), and preview tokens and tickets in
/// URLs (`_token=…`, `ticket=…`).
pub fn redact(s: &str) -> String {
    // (marker, the secret's characters run to the first that isn't a key's, shortest to hide)
    const MARKERS: &[(&str, bool, usize)] = &[
        ("boat_", true, 8),
        ("fdt_", true, 8),
        ("sk-ant-", true, 8),
        ("ghp_", true, 8),
        ("gho_", true, 8),
        ("ghu_", true, 8),
        ("ghs_", true, 8),
        ("github_pat_", true, 8),
        ("bearer ", false, 1),
        ("_token=", false, 1),
        ("ticket=", false, 1),
    ];
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    loop {
        let next = MARKERS
            .iter()
            .filter_map(|m| lower[at..].find(m.0).map(|i| (at + i, m)))
            .min_by_key(|(i, _)| *i);
        let Some((i, &(marker, key, shortest))) = next else {
            break;
        };
        let start = i + marker.len();
        out.push_str(&s[at..start]);
        let after = &s[start..];
        let len = if key {
            after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
                .unwrap_or(after.len())
        } else {
            after
                .find(|c: char| c == '&' || c == '"' || c == '\'' || c == ',' || c.is_whitespace())
                .unwrap_or(after.len())
        };
        // `boat_` followed by nothing secret-looking (e.g. prose) is left alone.
        if len >= shortest {
            out.push('…');
        } else {
            out.push_str(&after[..len]);
        }
        at = start + len;
    }
    out.push_str(&s[at..]);
    out
}

/// This machine's name, as people see it ("My MacBook Pro", or the host name).
pub fn machine_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let ask = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|n| !n.is_empty())
        };
        let mac = if cfg!(target_os = "macos") {
            ask("/usr/sbin/scutil", &["--get", "ComputerName"])
        } else {
            None
        };
        mac.or_else(|| ask("hostname", &[]))
            .unwrap_or_else(|| "this machine".into())
    })
    .clone()
}

/// When `ms` is, in this machine's time zone, as a line of text says it: "21:07" on the day
/// `now_ms` is in, "tomorrow 09:00" or "yesterday 21:07" beside it, else "2026-11-03 09:00".
/// A bare time is always today's, so it never reads as past when it's tomorrow's.
pub fn local_when(ms: i64, now_ms: i64) -> String {
    when_in(&chrono::Local, ms, now_ms)
}

/// [`local_when`] in `tz`.
pub fn when_in<Tz: TimeZone>(tz: &Tz, ms: i64, now_ms: i64) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (Some(t), Some(now)) = (
        DateTime::from_timestamp_millis(ms),
        DateTime::from_timestamp_millis(now_ms),
    ) else {
        return String::new();
    };
    let (t, now) = (t.with_timezone(tz), now.with_timezone(tz));
    let time = t.format("%H:%M");
    match (t.date_naive() - now.date_naive()).num_days() {
        0 => time.to_string(),
        1 => format!("tomorrow {time}"),
        -1 => format!("yesterday {time}"),
        _ => t.format("%Y-%m-%d %H:%M").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_keys_and_tokens() {
        assert_eq!(redact("Bearer boat_AbC123_xyz-9876 ok"), "Bearer … ok");
        assert_eq!(redact("key boat_AbC123_xyz-9876 ok"), "key boat_… ok");
        assert_eq!(
            redact("token sk-ant-oat01-AbC_123-xyz rejected"),
            "token sk-ant-… rejected"
        );
        assert_eq!(
            redact("gho_abcdef123456 and github_pat_11AB_cdef1234"),
            "gho_… and github_pat_…"
        );
        assert_eq!(
            redact("boat api-key create Flotilla --actions sandbox.create"),
            "boat api-key create Flotilla --actions sandbox.create"
        );
        assert_eq!(
            redact("authorization: bearer fdt_0123456789abcdef_AAAA"),
            "authorization: bearer …"
        );
        assert_eq!(redact("token fdt_0123456789abcdef_AbC-x_9."), "token fdt_…");
        assert_eq!(
            redact("/v1/ws?ticket=abc123&proto=1"),
            "/v1/ws?ticket=…&proto=1"
        );
        assert_eq!(
            redact("https://x.on.boat.dev/?_token=s3cr3t&a=1"),
            "https://x.on.boat.dev/?_token=…&a=1"
        );
        assert_eq!(
            redact("\"url\":\"https://h/?_token=abc\""),
            "\"url\":\"https://h/?_token=…\""
        );
        assert_eq!(redact("the boat_ prefix"), "the boat_ prefix");
        assert_eq!(redact("nothing here"), "nothing here");
    }

    #[test]
    fn a_time_says_its_day_unless_its_today() {
        // Seven hours behind UTC, so late evening here is the next day in UTC.
        let tz = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
        let at = |month: u32, day: u32, h: u32, m: u32| {
            tz.with_ymd_and_hms(2026, month, day, h, m, 0)
                .unwrap()
                .timestamp_millis()
        };
        let now = at(10, 31, 22, 0);
        assert_eq!(when_in(&tz, at(10, 31, 22, 5), now), "22:05");
        assert_eq!(when_in(&tz, at(10, 31, 0, 1), now), "00:01");
        // A day's code made at 22:00 expires tomorrow at 22:00, not "22:00".
        assert_eq!(when_in(&tz, at(11, 1, 22, 0), now), "tomorrow 22:00");
        assert_eq!(when_in(&tz, at(10, 30, 21, 7), now), "yesterday 21:07");
        assert_eq!(when_in(&tz, at(11, 3, 9, 0), now), "2026-11-03 09:00");
        assert_eq!(when_in(&tz, at(10, 28, 9, 0), now), "2026-10-28 09:00");
        // The day is the zone's, not UTC's: 23:30 here is already tomorrow in UTC.
        assert_eq!(when_in(&tz, at(10, 31, 23, 30), at(10, 31, 8, 0)), "23:30");
    }

    #[test]
    fn preview_line_drops_markdown_markers() {
        assert_eq!(
            preview_line("\n## Codeword noted: **PLUM**, ran `uname`.\nmore").as_deref(),
            Some("Codeword noted: PLUM, ran uname.")
        );
        assert_eq!(
            preview_line("- keep snake_case and 2*3").as_deref(),
            Some("keep snake_case and 2*3")
        );
    }

    #[test]
    fn plain_text_drops_markdown_for_previews() {
        // A hand-off's answer in a preview (round 3: "- **Correctness:** No bug…").
        assert_eq!(
            plain_text("- **Correctness:** No bug for the stated behaviour.\n- *Style:* fine"),
            "Correctness: No bug for the stated behaviour. Style: fine"
        );
        assert_eq!(
            plain_text(
                "### Plan\n1. Add `retry()`\n2) See [the docs](https://x.dev/a_b) and ![chart](c.png)"
            ),
            "Plan · Add retry() · See the docs and chart"
        );
        assert_eq!(
            plain_text("> quoted ~~old~~ new\n```rust\nlet a = 2*3;\n```"),
            "quoted old new · let a = 2*3;"
        );
        assert_eq!(
            plain_text("| City | Time |\n|---|:-:|\n| Tokyo | 05:33 |"),
            "City · Time · Tokyo · 05:33"
        );
        // A paragraph's lines join as one; paragraphs apart.
        assert_eq!(
            plain_text("I added the test\nand it passes\n\nNext the docs"),
            "I added the test and it passes · Next the docs"
        );
        // What isn't markdown stays.
        assert_eq!(
            plain_text("keep snake_case, 2 * 3 and #42 and a_b_c"),
            "keep snake_case, 2 * 3 and #42 and a_b_c"
        );
        assert_eq!(plain_text("- [x] done _now_"), "done _now_");
        // Marks only in pairs; identifiers and lone stars stay, even a second time through.
        assert_eq!(
            plain_text("edit __init__.py, then *ptr = 2*3*4 and a ** b"),
            "edit __init__.py, then *ptr = 2*3*4 and a ** b"
        );
        assert_eq!(
            plain_text("**Bold** and *it* and ~~gone~~ and **unclosed"),
            "Bold and it and gone and **unclosed"
        );
        assert_eq!(
            plain_text(&plain_text("Fixed `_token` and `*ptr`")),
            "Fixed _token and *ptr"
        );
        assert_eq!(
            plain_text("**Fixed `retry()`** and *`a*b`*, [`docs`](u)"),
            "Fixed retry() and a*b, docs"
        );
        // A span inside what a link drops (its title) doesn't shift the ones after it.
        assert_eq!(
            plain_text("[docs](url \"Use `v1`\") then `retry()` and `x`"),
            "docs then retry() and x"
        );
        // The store's preview line is final: a client showing it as it comes shows code as written.
        assert_eq!(
            preview_line("Fixed `_token` in `__init__.py`\nmore").as_deref(),
            Some("Fixed _token in __init__.py")
        );
        assert_eq!(
            plain_text("a `` `tick` `` and a lone ` here"),
            "a `tick` and a lone ` here"
        );
        assert_eq!(plain_text("**Größe:** naïve 🚀 *ok*"), "Größe: naïve 🚀 ok");
        assert_eq!(preview_line("```\n\nOK").as_deref(), Some("OK"));
        // Code stays as written.
        assert_eq!(
            plain_text("Added `__init__.py` and ``a`b``"),
            "Added __init__.py and a`b"
        );
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
        assert_eq!(truncate_chars("hi", 5), "hi");
        assert_eq!(ellipsize("hello world", 5), "hello…");
    }
}
