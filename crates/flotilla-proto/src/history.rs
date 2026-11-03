//! Long threads, cheap to load and send.
//!
//! - **Pages.** A thread's messages fall into pages of [`PAGE`] seqs: page `k` holds seqs
//!   `k * PAGE .. (k + 1) * PAGE`. Its view ([`crate::ThreadDetail`]) carries only the newest
//!   whole pages, at least [`WINDOW`] messages (so 100 to 149), and how many come before them
//!   (`older`). Older pages come one at a time over HTTP ([`HISTORY_PATH`]), as T3 pages its
//!   history.
//! - **Marks.** The view also carries every page's mark: a fingerprint of everything in it, as
//!   the server holds it. A client keeps a page it has (fetched, or seen in the view before the
//!   view moved on past it) only while its mark matches, so a message that changes outside the
//!   view, the view moving on as turns arrive, and a resync after a gap all come out right
//!   without the server keeping anything per client. [`History`] is that bookkeeping.
//! - **Caps.** A tool's input or result longer than [`CAP`] bytes goes out cut ([`cap`]),
//!   still valid JSON when it was, its longest strings shortened and ended with `…`, and the
//!   message says so ([`Cut`]). The whole string comes on demand ([`FULL_PATH`]).

use crate::{Message, ThreadDetail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Seqs per page.
pub const PAGE: i64 = 50;
/// A thread's view holds at least this many of its newest messages (in whole pages).
pub const WINDOW: i64 = 100;
/// The longest tool input or result sent whole, in bytes (T3's 32 KB per detail string).
pub const CAP: usize = 32 * 1024;
/// Bump when what a mark covers, or how a message is cut, changes: marks from before then
/// no longer match, so clients fetch their pages again.
pub const MARKS_VERSION: u64 = 1;

/// `GET /v1/threads/<id>/history?before=<seq>`: the page that ends at `before` (a multiple of
/// [`PAGE`]), as a [`HistoryPage`].
pub const HISTORY_PATH: &str = "/v1/threads/{id}/history";
/// `GET /v1/threads/<id>/full?seq=<seq>&field=<field>&mark=<mark>`: a cut string whole, as
/// `text/plain`; 409 if it's changed since.
pub const FULL_PATH: &str = "/v1/threads/{id}/full";

/// A page's mark, in a thread's view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageMark {
    pub page: i64,
    pub mark: String,
}

/// A string a message carries cut short: which field, how long it is whole, and its mark (to
/// ask for it whole, and be sure it's the one shown).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cut {
    /// `tool_input_json` or `tool_result_json`.
    pub field: String,
    pub bytes: u64,
    pub mark: String,
}

/// One page of a thread's older messages, cut as the view's are, and its mark as it was read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub thread: String,
    pub page: i64,
    pub mark: String,
    pub messages: Vec<Message>,
}

/// Where a thread with `count` messages starts its view: the first seq of its newest whole
/// pages holding at least [`WINDOW`] (0 when there are no more than that).
pub fn window_start(count: i64) -> i64 {
    ((count - WINDOW).max(0) / PAGE) * PAGE
}

/// The page a seq is on.
pub fn page_of(seq: i64) -> i64 {
    seq.div_euclid(PAGE)
}

/// A 64-bit fingerprint of bytes, the same on every machine and run (not cryptographic: it
/// tells content apart, it doesn't resist someone forging a match).
#[derive(Debug, Clone)]
pub struct Fingerprint(u64);

const K: u64 = 0x9e37_79b9_7f4a_7c15;

impl Default for Fingerprint {
    fn default() -> Self {
        Fingerprint(0x243f_6a88_85a3_08d3 ^ MARKS_VERSION.wrapping_mul(K))
    }
}

impl Fingerprint {
    pub fn new() -> Fingerprint {
        Fingerprint::default()
    }

    fn word(&mut self, w: u64) {
        self.0 = (self.0 ^ w).wrapping_mul(K).rotate_left(29);
    }

    /// Bytes, with their length (so `"ab","c"` isn't `"a","bc"`).
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.word(b.len() as u64 ^ 0xa5a5_a5a5_0000_0000);
        let (chunks, rest) = b.as_chunks::<8>();
        for c in chunks {
            self.word(u64::from_le_bytes(*c));
        }
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.word(u64::from_le_bytes(buf));
        }
        self
    }

    pub fn int(&mut self, n: i64) -> &mut Self {
        self.word(n as u64);
        self
    }

    /// An optional string: none isn't `""`.
    pub fn opt(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            Some(s) => self.int(1).bytes(s.as_bytes()),
            None => self.int(0),
        }
    }

    pub fn finish(&self) -> u64 {
        // murmur3's finalizer, so every input bit reaches every output bit.
        let mut h = self.0;
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
        h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        h ^ (h >> 33)
    }
}

/// A mark as it travels: 16 hex digits.
pub fn hex(mark: u64) -> String {
    format!("{mark:016x}")
}

/// A message's fingerprint: everything it carries, whole (so its cut form is covered too).
pub fn message_mark(m: &Message) -> u64 {
    let mut f = Fingerprint::new();
    f.int(m.seq)
        .opt(m.task_id.as_deref())
        .bytes(m.kind.as_bytes())
        .opt(m.text.as_deref())
        .opt(m.status.as_deref())
        .opt(m.model.as_deref());
    f.opt(m.tool_name.as_deref())
        .opt(m.tool_id.as_deref())
        .opt(m.tool_summary.as_deref())
        .opt(m.tool_input_json.as_deref())
        .opt(m.tool_result_json.as_deref());
    f.opt(m.tool_result_type.as_deref())
        .int(m.is_error as i64)
        .int(m.pending as i64)
        .opt(m.event_key.as_deref());
    match m.timestamp {
        Some(t) => f.int(1).int(t),
        None => f.int(0),
    };
    f.int(m.attachments.len() as i64);
    for a in &m.attachments {
        f.bytes(a.as_bytes());
    }
    // Only where it's set: every other message keeps the mark it had.
    if let Some(ask) = &m.answer_to {
        f.int(2).bytes(ask.as_bytes());
    }
    // A page kept from before Flotilla's own were marked is read again.
    if m.from_flotilla {
        f.int(3);
    }
    f.finish()
}

/// A page's mark from its messages' marks, in seq order.
pub fn page_mark(page: i64, marks: impl IntoIterator<Item = (i64, u64)>) -> String {
    let mut f = Fingerprint::new();
    f.int(page);
    for (seq, mark) in marks {
        f.int(seq).int(mark as i64);
    }
    hex(f.finish())
}

/// Every page's mark, from each message's (seq, mark) in seq order.
pub fn page_marks(marks: &[(i64, u64)]) -> Vec<PageMark> {
    let mut out: Vec<PageMark> = Vec::new();
    let mut i = 0;
    while i < marks.len() {
        let page = page_of(marks[i].0);
        let end = marks[i..]
            .iter()
            .position(|(s, _)| page_of(*s) != page)
            .map_or(marks.len(), |n| i + n);
        out.push(PageMark {
            page,
            mark: page_mark(page, marks[i..end].iter().copied()),
        });
        i = end;
    }
    out
}

/// A string's mark, as a [`Cut`] carries it.
pub fn string_mark(s: &str) -> String {
    hex(Fingerprint::new().bytes(s.as_bytes()).finish())
}

/// The longest prefix of `s` of at most `max` bytes that ends on a character's boundary.
pub fn floor(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

const ELLIPSIS: &str = "…";

/// Strings this short are never cut when lists can be shortened instead.
const FLOOR: usize = 64;

/// `text` cut to at most `cap` bytes, or None when it fits. JSON stays JSON: its longest
/// strings are shortened, all to one length, each ended with `…`, so short ones (a command, a
/// path) stay whole; if that isn't enough (a list of thousands of short lines), long lists
/// keep only their first entries, all cut to one count. A JSON string (a tool's plain-text
/// output) stays a string, cut; other JSON too big even then becomes a string of its text,
/// cut, so a client reading it as JSON never gets half an object. Text that isn't JSON is cut
/// as text.
pub fn cap_text(text: &str, cap: usize) -> Option<String> {
    if text.len() <= cap {
        return None;
    }
    let raw = || cut_as_string(text, cap);
    let value: Value = match serde_json::from_str(text) {
        Ok(v @ (Value::Object(_) | Value::Array(_))) => v,
        Ok(Value::String(s)) => return Some(cut_as_string(&s, cap)),
        Ok(_) => return Some(raw()),
        Err(_) => {
            return Some(format!(
                "{}{ELLIPSIS}",
                floor(text, cap.saturating_sub(ELLIPSIS.len()))
            ));
        }
    };
    let mut lens = Vec::new();
    strings(&value, &mut lens);
    lens.sort_unstable_by(|a, b| b.cmp(a));
    let whole = serde_json::to_string(&value).map_or(text.len(), |s| s.len());
    let mut excess = whole.saturating_sub(cap);
    // Strings alone, in a few rounds: escapes (a newline is two bytes written) make a cut
    // save less than its length, and each round asks for what the last one fell short by.
    for _ in 0..16 {
        let Some(limit) = water(&lens, excess).filter(|l| *l >= FLOOR) else {
            break;
        };
        let mut cut = value.clone();
        shorten(&mut cut, limit);
        let out = serde_json::to_string(&cut).unwrap_or_default();
        if out.len() <= cap {
            return Some(out);
        }
        excess += out.len() - cap;
    }
    // Then lists too: strings at the floor, and the most entries any list may keep that fits.
    let mut base = value;
    shorten(&mut base, FLOOR);
    let fits = |n: usize| -> Option<String> {
        let mut cut = base.clone();
        trim_lists(&mut cut, n);
        serde_json::to_string(&cut).ok().filter(|o| o.len() <= cap)
    };
    let (mut lo, mut hi) = (0usize, longest_list(&base));
    if fits(0).is_none() {
        return Some(raw());
    }
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid).is_some() {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    fits(lo).or_else(|| Some(raw()))
}

/// `s`, cut and ended with `…`, as a JSON string of at most `cap` bytes.
fn cut_as_string(s: &str, cap: usize) -> String {
    let room = cap.saturating_sub(2 + ELLIPSIS.len());
    serde_json::to_string(&Value::String(format!(
        "{}{ELLIPSIS}",
        floor_escaped(s, room)
    )))
    .unwrap_or_default()
}

/// The most entries in any list.
fn longest_list(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.iter().map(longest_list).max().unwrap_or(0).max(a.len()),
        Value::Object(o) => o.values().map(longest_list).max().unwrap_or(0),
        _ => 0,
    }
}

/// Keep at most `n` entries of every list.
fn trim_lists(v: &mut Value, n: usize) {
    match v {
        Value::Array(a) => {
            a.truncate(n);
            a.iter_mut().for_each(|v| trim_lists(v, n));
        }
        Value::Object(o) => o.values_mut().for_each(|v| trim_lists(v, n)),
        _ => {}
    }
}

/// How many bytes a character takes inside a JSON string, as serde_json writes it.
fn escaped(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// A string's length inside JSON, quotes aside.
fn escaped_len(s: &str) -> usize {
    s.chars().map(escaped).sum()
}

/// The longest prefix of `s` that takes at most `max` bytes written inside JSON.
fn floor_escaped(s: &str, max: usize) -> &str {
    let mut used = 0;
    for (i, c) in s.char_indices() {
        used += escaped(c);
        if used > max {
            return &s[..i];
        }
    }
    s
}

/// Every string's length as written, keys aside.
fn strings(v: &Value, out: &mut Vec<usize>) {
    match v {
        Value::String(s) => out.push(escaped_len(s)),
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        Value::Object(o) => o.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// The longest length (bytes written) to cut strings to that saves `excess` bytes, counting the `…`
/// each cut one gains; None if even cutting every string to nothing wouldn't.
fn water(lens: &[usize], excess: usize) -> Option<usize> {
    let saved = |limit: usize| -> usize {
        lens.iter()
            .take_while(|&&l| l > limit + ELLIPSIS.len())
            .map(|&l| l - limit - ELLIPSIS.len())
            .sum()
    };
    if saved(0) < excess {
        return None;
    }
    let (mut lo, mut hi) = (0usize, lens.first().copied().unwrap_or(0));
    // The largest limit that still saves enough.
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if saved(mid) >= excess {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    Some(lo)
}

/// Cut every string longer (as written) than `limit` + `…` to `limit` bytes written, and end
/// it with `…`.
fn shorten(v: &mut Value, limit: usize) {
    match v {
        Value::String(s) if escaped_len(s) > limit + ELLIPSIS.len() => {
            *s = format!("{}{ELLIPSIS}", floor_escaped(s, limit))
        }
        Value::Array(a) => a.iter_mut().for_each(|v| shorten(v, limit)),
        Value::Object(o) => o.values_mut().for_each(|v| shorten(v, limit)),
        _ => {}
    }
}

thread_local! {
    /// Strings cut lately, by their mark and length. A thread's view is computed again on every
    /// change while a turn runs, and cutting a big output means parsing it whole.
    static CUTS: std::cell::RefCell<std::collections::HashMap<(String, usize), String>> = Default::default();
}

/// A message as it goes out: its tool input and result cut to [`CAP`], each one cut noted.
pub fn cap(m: &Message) -> Message {
    let mut out = m.clone();
    out.cut.clear();
    for field in ["tool_input_json", "tool_result_json"] {
        let slot = match field {
            "tool_input_json" => &mut out.tool_input_json,
            _ => &mut out.tool_result_json,
        };
        let Some(text) = slot.as_deref().filter(|t| t.len() > CAP) else {
            continue;
        };
        let mark = string_mark(text);
        let key = (mark.clone(), text.len());
        let cut = CUTS.with(|c| c.borrow().get(&key).cloned()).or_else(|| {
            let cut = cap_text(text, CAP)?;
            CUTS.with(|c| {
                let mut c = c.borrow_mut();
                if c.len() >= 64 {
                    c.clear();
                }
                c.insert(key, cut.clone());
            });
            Some(cut)
        });
        if let Some(cut) = cut {
            out.cut.push(Cut {
                field: field.into(),
                bytes: text.len() as u64,
                mark,
            });
            *slot = Some(cut);
        }
    }
    out
}

/// The whole string a cut stands for, from the message as the server holds it, if it's still
/// the one cut (by its mark).
pub fn uncut<'a>(m: &'a Message, field: &str, mark: &str) -> Option<&'a str> {
    let text = match field {
        "tool_input_json" => m.tool_input_json.as_deref(),
        "tool_result_json" => m.tool_result_json.as_deref(),
        _ => None,
    }?;
    (string_mark(text) == mark).then_some(text)
}

/// A client's older pages of one thread, beside its view: which it holds and whether each is
/// still good, which to fetch, and the messages to show (the pages held, then the view's).
///
/// - A page leaving the view as it moves on is kept from the view's copy if its mark didn't
///   change meanwhile, so nothing on screen goes.
/// - A page whose mark changed is shown as it was until its fetch comes back.
/// - Pages the user hasn't scrolled to aren't fetched.
#[derive(Debug, Clone, Default)]
pub struct History {
    /// The view as last seen: where it starts, its marks, its messages.
    start: i64,
    marks: BTreeMap<i64, String>,
    window: Vec<Message>,
    seen: bool,
    /// Older pages held: page → (its mark when fetched or kept, its messages).
    held: BTreeMap<i64, (String, Vec<Message>)>,
    /// The oldest page wanted (the user scrolled up to it); None: just the view.
    want: Option<i64>,
    /// Pages asked for, with the view's mark for each when asked: not asked again until that
    /// changes (or the ask fails).
    asked: BTreeMap<i64, String>,
}

impl History {
    pub fn new() -> History {
        History::default()
    }

    /// The first page in the view.
    fn first(&self) -> i64 {
        page_of(self.start)
    }

    /// A new view of the thread.
    pub fn view(&mut self, d: &ThreadDetail) {
        let marks: BTreeMap<i64, String> =
            d.pages.iter().map(|p| (p.page, p.mark.clone())).collect();
        let first = page_of(d.older);
        if self.seen {
            // Pages that left the view, kept from its copy when they didn't change meanwhile.
            for page in self.first()..first {
                let (Some(was), Some(now)) = (self.marks.get(&page), marks.get(&page)) else {
                    continue;
                };
                let messages: Vec<Message> = self
                    .window
                    .iter()
                    .filter(|m| page_of(m.seq) == page)
                    .cloned()
                    .collect();
                if was == now && messages.len() as i64 == PAGE {
                    self.held.insert(page, (now.clone(), messages));
                    self.want = Some(self.want.map_or(page, |w| w.min(page)));
                }
            }
        }
        // Nothing held that's in the view now, or that's gone.
        self.held.retain(|p, _| *p < first && marks.contains_key(p));
        self.asked
            .retain(|p, _| *p < first && marks.contains_key(p));
        self.want = self.want.filter(|w| *w < first);
        self.start = d.older;
        self.marks = marks;
        self.window = d.messages.clone();
        self.seen = true;
    }

    /// A fetched page. One fetched at an older mark that answers late never replaces the
    /// page held at the mark the view has now.
    pub fn page(&mut self, p: HistoryPage) {
        let Some(now) = self.marks.get(&p.page) else {
            return;
        };
        let current = self.held.get(&p.page).is_some_and(|(m, _)| m == now);
        if p.page < self.first() && (p.mark == *now || !current) {
            self.held.insert(p.page, (p.mark, p.messages));
        }
    }

    /// A fetch failed: it can be asked for again.
    pub fn failed(&mut self, page: i64) {
        self.asked.remove(&page);
    }

    /// The pages shown above the view: held ones, from the view down, while unbroken.
    fn shown(&self) -> impl Iterator<Item = i64> + '_ {
        let want = self.want.unwrap_or(self.first());
        (want..self.first())
            .rev()
            .take_while(|p| self.held.contains_key(p))
    }

    /// The messages to show, in seq order: the older pages held, then the view's.
    pub fn messages(&self) -> Vec<Message> {
        let pages: Vec<i64> = self.shown().collect();
        let mut out: Vec<Message> =
            Vec::with_capacity(pages.len() * PAGE as usize + self.window.len());
        for p in pages.iter().rev() {
            out.extend(self.held[p].1.iter().cloned());
        }
        out.extend(self.window.iter().cloned());
        out
    }

    /// The oldest seq shown.
    fn lowest(&self) -> i64 {
        self.shown().last().map_or(self.start, |p| p * PAGE)
    }

    /// There are older messages than those shown.
    pub fn more_above(&self) -> bool {
        self.seen && self.lowest() > 0
    }

    /// Some page wanted isn't here yet.
    pub fn loading(&self) -> bool {
        let want = self.want.unwrap_or(self.first());
        (want..self.first()).any(|p| !self.held.contains_key(&p))
    }

    /// The user scrolled near the top: want the page above what's shown. False if there's
    /// none, or the one wanted last hasn't come yet.
    pub fn more(&mut self) -> bool {
        if !self.more_above() || self.loading() {
            return false;
        }
        self.want = Some(page_of(self.lowest()) - 1);
        true
    }

    /// Open at the message `seq` (a search hit): want every page from the view down to the one
    /// that holds it, so it shows once they're in (only an unbroken run shows). False when
    /// nothing more is wanted: it's in the view or wanted already, or there's no such page.
    pub fn reach(&mut self, seq: i64) -> bool {
        if !self.seen || seq < 0 || seq >= self.start {
            return false;
        }
        let page = page_of(seq);
        if !self.marks.contains_key(&page) || self.want.is_some_and(|w| w <= page) {
            return false;
        }
        self.want = Some(page);
        true
    }

    /// Whether the message `seq` is among those shown.
    pub fn shows(&self, seq: i64) -> bool {
        self.seen
            && if seq >= self.start {
                self.window.iter().any(|m| m.seq == seq)
            } else {
                seq >= self.lowest()
            }
    }

    /// The pages to fetch now, nearest first: wanted, and missing or changed, and not already
    /// asked for at the mark the view has.
    pub fn wanted(&mut self) -> Vec<i64> {
        self.wanted_up_to(usize::MAX)
    }

    /// As [`wanted`](Self::wanted), at most `room` of them (fetches a client still has room
    /// for): the rest stay wanted, for when these come in.
    pub fn wanted_up_to(&mut self, room: usize) -> Vec<i64> {
        let Some(want) = self.want else { return vec![] };
        let mut out = Vec::new();
        for p in (want..self.first()).rev() {
            if out.len() >= room {
                break;
            }
            let Some(mark) = self.marks.get(&p) else {
                continue;
            };
            let good = self.held.get(&p).is_some_and(|(m, _)| m == mark);
            if good || self.asked.get(&p) == Some(mark) {
                continue;
            }
            self.asked.insert(p, mark.clone());
            out.push(p);
        }
        out
    }

    /// The `before` to fetch a page with.
    pub fn before(page: i64) -> i64 {
        (page + 1) * PAGE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(seq: i64, text: &str) -> Message {
        Message {
            seq,
            kind: "text".into(),
            text: Some(text.into()),
            ..Default::default()
        }
    }

    /// A thread of `n` messages, each "v{version}" (some changed), as the server's view of it.
    fn thread(n: i64, text: impl Fn(i64) -> String) -> (Vec<Message>, ThreadDetail) {
        let all: Vec<Message> = (0..n).map(|s| msg(s, &text(s))).collect();
        let marks: Vec<(i64, u64)> = all.iter().map(|m| (m.seq, message_mark(m))).collect();
        let older = window_start(n);
        let d = ThreadDetail {
            id: "t".into(),
            thread: None,
            messages: all[older as usize..].to_vec(),
            older,
            pages: page_marks(&marks),
            sent: vec![],
            start_error: None,
            stages: None,
            project: None,
            resume: None,
            archive: None,
            setup_failure: None,
            services: None,
            public: vec![],
            lineage: None,
            parent: None,
            children: vec![],
            context: None,
        };
        (all, d)
    }

    fn fetch(all: &[Message], page: i64) -> HistoryPage {
        let messages: Vec<Message> = all
            .iter()
            .filter(|m| page_of(m.seq) == page)
            .cloned()
            .collect();
        let mark = page_mark(page, messages.iter().map(|m| (m.seq, message_mark(m))));
        HistoryPage {
            thread: "t".into(),
            page,
            mark,
            messages,
        }
    }

    fn seqs(h: &History) -> Vec<i64> {
        h.messages().iter().map(|m| m.seq).collect()
    }

    #[test]
    fn the_window_is_the_newest_whole_pages() {
        assert_eq!(window_start(0), 0);
        assert_eq!(window_start(100), 0);
        assert_eq!(window_start(149), 0);
        assert_eq!(window_start(150), 50);
        assert_eq!(window_start(199), 50);
        assert_eq!(window_start(200), 100);
        assert_eq!(window_start(3146), 3000);
        for n in 0..1000 {
            let held = n - window_start(n);
            assert!(
                held == n || (WINDOW..WINDOW + PAGE).contains(&held),
                "{n}: {held}"
            );
        }
    }

    #[test]
    fn scrolling_up_fetches_a_page_at_a_time() {
        let (all, d) = thread(260, |s| format!("m{s}"));
        let mut h = History::new();
        h.view(&d);
        assert_eq!(d.older, 150);
        assert_eq!(seqs(&h), (150..260).collect::<Vec<_>>());
        assert!(h.more_above() && !h.loading());
        assert!(h.wanted().is_empty(), "nothing fetched until asked");
        assert!(h.more());
        assert!(!h.more(), "one page at a time");
        assert_eq!(h.wanted(), [2]);
        assert!(h.wanted().is_empty(), "asked once");
        h.page(fetch(&all, 2));
        assert_eq!(seqs(&h), (100..260).collect::<Vec<_>>());
        assert!(h.more());
        assert_eq!(h.wanted(), [1]);
        h.page(fetch(&all, 1));
        assert!(h.more());
        assert_eq!(h.wanted(), [0]);
        h.page(fetch(&all, 0));
        assert_eq!(seqs(&h), (0..260).collect::<Vec<_>>());
        assert!(!h.more_above());
        assert!(!h.more());
    }

    #[test]
    fn reaching_an_old_message_fetches_down_to_its_page_a_few_at_a_time() {
        // 3,146 messages: the view holds 3000.., the hit is on page 3.
        let (all, d) = thread(3146, |s| format!("m{s}"));
        let mut h = History::new();
        assert!(!h.reach(170), "nothing to reach before the view");
        h.view(&d);
        assert!(h.shows(3100) && !h.shows(170) && !h.shows(3146));
        assert!(!h.reach(3050), "in the view already");
        assert!(!h.reach(4000) && !h.reach(-1));
        assert!(h.reach(170));
        assert!(!h.reach(170) && !h.reach(200), "wanted already");
        assert!(h.loading());
        // Four at a time, nearest first; more as they come in.
        assert_eq!(h.wanted_up_to(4), [59, 58, 57, 56]);
        assert_eq!(h.wanted_up_to(0), Vec::<i64>::new());
        for p in [59, 58, 57, 56] {
            h.page(fetch(&all, p));
        }
        assert_eq!(seqs(&h).first(), Some(&2800));
        assert!(!h.shows(170));
        let mut fetched = 4;
        loop {
            let pages = h.wanted_up_to(4);
            if pages.is_empty() {
                break;
            }
            fetched += pages.len();
            // One that answers out of order still shows only once the run is unbroken.
            for p in pages.iter().rev() {
                h.page(fetch(&all, *p));
            }
        }
        assert_eq!(fetched, 57, "pages 59 down to 3");
        assert!(h.shows(170) && !h.loading());
        assert_eq!(seqs(&h).first(), Some(&150));
        // Further up is the usual one page at a time.
        assert!(h.more());
        assert_eq!(h.wanted(), [2]);
        // Reaching one below what's wanted wants the rest down to it.
        assert!(h.reach(10));
        assert_eq!(h.wanted(), [1, 0]);
    }

    #[test]
    fn a_page_leaving_the_view_is_kept_and_one_that_changed_is_fetched_again() {
        let (_, d) = thread(199, |s| format!("m{s}"));
        let mut h = History::new();
        h.view(&d);
        assert_eq!(d.older, 50);
        // The view moves on a page: page 1 was in it, unchanged, so it's kept.
        let (all, d) = thread(200, |s| format!("m{s}"));
        h.view(&d);
        assert_eq!(d.older, 100);
        assert_eq!(seqs(&h), (50..200).collect::<Vec<_>>());
        assert!(h.wanted().is_empty(), "kept, not fetched");
        // A message on that page changes (outside the view): shown as it was until fetched.
        let (all2, d2) = thread(200, |s| {
            if s == 60 {
                "changed".into()
            } else {
                format!("m{s}")
            }
        });
        assert_eq!(d2.messages, d.messages);
        h.view(&d2);
        assert_eq!(seqs(&h), (50..200).collect::<Vec<_>>());
        assert_eq!(h.messages()[10].text.as_deref(), Some("m60"));
        assert_eq!(h.wanted(), [1]);
        // A fetch made before the change answers late: held, but it's still wanted again only
        // once the view changes; the right one replaces it.
        h.page(fetch(&all, 1));
        assert!(h.wanted().is_empty());
        h.page(fetch(&all2, 1));
        assert_eq!(h.messages()[10].text.as_deref(), Some("changed"));
        // The other way round: the current one first, then the late one, which doesn't replace it.
        h.page(fetch(&all, 1));
        assert_eq!(h.messages()[10].text.as_deref(), Some("changed"));
        assert!(h.wanted().is_empty());
        // A page that changed while in the view and left it is fetched, not kept.
        let (_, d3) = thread(249, |s| {
            if s == 120 {
                "x".into()
            } else if s == 60 {
                "changed".into()
            } else {
                format!("m{s}")
            }
        });
        let mut h2 = History::new();
        h2.view(&d2);
        h2.view(&d3);
        assert_eq!(d3.older, 100);
        let (_, d4) = thread(250, |s| {
            if s == 120 {
                "y".into()
            } else if s == 60 {
                "changed".into()
            } else {
                format!("m{s}")
            }
        });
        h2.view(&d4);
        assert_eq!(d4.older, 150);
        assert!(
            h2.messages().iter().all(|m| m.seq >= 150),
            "page 2 changed on the way out: not kept"
        );
        assert!(h2.more());
        assert_eq!(h2.wanted(), [2]);
    }

    #[test]
    fn a_resync_after_a_gap_keeps_only_what_still_matches() {
        let (_, d) = thread(199, |s| format!("m{s}"));
        let mut h = History::new();
        h.view(&d);
        // Views missed: the thread grew, and seq 70 changed while it was still in the view.
        let (all2, d2) = thread(310, |s| {
            if s == 70 {
                "changed".into()
            } else {
                format!("m{s}")
            }
        });
        h.view(&d2);
        assert_eq!(d2.older, 200);
        // Page 2 was whole in it and is unchanged: kept. Page 3 has grown since (its mark
        // moved) and page 1 changed: neither kept, and page 3, between, is fetched.
        assert_eq!(seqs(&h).first(), Some(&200), "shown only while unbroken");
        assert_eq!(h.wanted(), [3]);
        h.page(fetch(&all2, 3));
        assert_eq!(seqs(&h).first(), Some(&100));
        assert!(h.more());
        assert_eq!(h.wanted(), [1]);
        h.page(fetch(&all2, 1));
        assert_eq!(h.messages()[20].text.as_deref(), Some("changed"));
        // The thread shrinking past what's held drops it.
        let (_, d3) = thread(120, |s| format!("m{s}"));
        h.view(&d3);
        assert_eq!(seqs(&h), (0..120).collect::<Vec<_>>());
        assert!(!h.more_above());
        assert!(h.wanted().is_empty());
    }

    #[test]
    fn a_failed_fetch_can_be_asked_again() {
        let (_, d) = thread(400, |s| format!("m{s}"));
        let mut h = History::new();
        h.view(&d);
        assert!(h.more());
        assert_eq!(h.wanted(), [5]);
        assert!(h.wanted().is_empty());
        h.failed(5);
        assert_eq!(h.wanted(), [5]);
    }

    #[test]
    fn marks_tell_content_apart() {
        let a = msg(3, "hello");
        let mut b = a.clone();
        assert_eq!(message_mark(&a), message_mark(&b));
        b.text = Some("hellO".into());
        assert_ne!(message_mark(&a), message_mark(&b));
        b.text = Some("hello".into());
        b.pending = true;
        assert_ne!(message_mark(&a), message_mark(&b));
        let mut c = a.clone();
        c.status = Some(String::new());
        assert_ne!(message_mark(&a), message_mark(&c), "none isn't empty");
        // Same on every run: fixed, not seeded.
        assert_eq!(string_mark("abc"), string_mark("abc"));
        assert_eq!(string_mark(""), hex(Fingerprint::new().bytes(b"").finish()));
        assert_ne!(string_mark("ab"), string_mark("ba"));
    }

    #[test]
    fn a_long_string_is_cut_on_a_character_boundary() {
        // Multibyte characters across the cut: never split, never over.
        for pad in 0..4 {
            let text = format!("{}{}", "a".repeat(pad), "é日✓🚢".repeat(5000));
            let cut = cap_text(&text, CAP).unwrap();
            assert!(cut.len() <= CAP, "{}", cut.len());
            assert!(cut.ends_with('…'));
            assert!(text.starts_with(cut.trim_end_matches('…')));
        }
        assert_eq!(cap_text("short", CAP), None);
        let exactly = "x".repeat(CAP);
        assert_eq!(cap_text(&exactly, CAP), None);
        assert!(cap_text(&format!("{exactly}y"), CAP).unwrap().len() <= CAP);
    }

    #[test]
    fn json_stays_json_with_its_short_strings_whole() {
        let log: String = (0..20_000)
            .map(|i| format!("line {i} \"quoted\"\n"))
            .collect();
        let text = serde_json::json!({"type": "bash_output", "command": "cat big.log", "content": log, "error": false}).to_string();
        let cut = cap_text(&text, CAP).unwrap();
        assert!(cut.len() <= CAP, "{}", cut.len());
        let v: Value = serde_json::from_str(&cut).unwrap();
        assert_eq!(v["command"], "cat big.log");
        assert_eq!(v["type"], "bash_output");
        assert_eq!(v["error"], false);
        let content = v["content"].as_str().unwrap();
        assert!(content.ends_with('…') && log.starts_with(content.trim_end_matches('…')));
        assert!(
            content.len() > CAP / 2,
            "most of the room goes to the long one: {}",
            content.len()
        );
        // Many short strings (a 2,000-line read): too many to fit even cut, so the list keeps
        // its first lines, whole, and the rest of the object stays.
        let lines: Vec<Value> = (0..2000).map(|i| serde_json::json!({"number": i, "content": format!("    value_{i} = compute({i})  # 日本語のコメント {i}")})).collect();
        let text = serde_json::json!({"type": "file_content", "filePath": "/x/big.py", "totalLines": 2000, "lines": lines}).to_string();
        let cut = cap_text(&text, CAP).unwrap();
        assert!(cut.len() <= CAP);
        let v: Value = serde_json::from_str(&cut).unwrap();
        let kept = v["lines"].as_array().unwrap();
        assert!(kept.len() > 200 && kept.len() < 2000, "{}", kept.len());
        assert_eq!(kept[0], lines[0]);
        assert_eq!(kept.last(), lines.get(kept.len() - 1));
        assert_eq!(v["filePath"], "/x/big.py");
        assert_eq!(v["totalLines"], 2000);
        // A long list of numbers: its first entries.
        let numbers = serde_json::to_string(&(0..20_000).collect::<Vec<_>>()).unwrap();
        let cut = cap_text(&numbers, CAP).unwrap();
        let v: Vec<i64> = serde_json::from_str(&cut).unwrap();
        assert!(cut.len() <= CAP && v.len() > 1000 && v[..3] == [0, 1, 2]);
        // An object too big in its keys alone (or many short fields): still JSON, a string of
        // its text, cut.
        for text in [
            Value::Object(
                (0..20_000)
                    .map(|i| (format!("k{i}"), Value::from(i)))
                    .collect(),
            )
            .to_string(),
            Value::Object(
                (0..2000)
                    .map(|i| {
                        (
                            format!("field_{i}"),
                            Value::from(format!("value \"{i}\" here")),
                        )
                    })
                    .collect(),
            )
            .to_string(),
        ] {
            let cut = cap_text(&text, CAP).unwrap();
            assert!(cut.len() <= CAP, "{}", cut.len());
            let v: Value = serde_json::from_str(&cut).unwrap();
            let shown = v.as_str().unwrap();
            assert!(shown.ends_with('…') && text.starts_with(shown.trim_end_matches('…')));
        }
        // A tool's plain-text output, kept as a JSON string: still one, cut, its escapes and
        // characters whole.
        let output: String = (0..3000)
            .map(|i| format!("line {i}: \"é\"\t✓\\\n"))
            .collect();
        let text = Value::String(output.clone()).to_string();
        let cut = cap_text(&text, CAP).unwrap();
        assert!(cut.len() <= CAP && cut.len() > CAP - 16, "{}", cut.len());
        let shown: String = serde_json::from_str(&cut).unwrap();
        assert!(shown.ends_with('…') && output.starts_with(shown.trim_end_matches('…')));
        // Not JSON at all.
        let plain = "plain ".repeat(10_000);
        assert!(cap_text(&plain, CAP).unwrap().len() <= CAP);
    }

    #[test]
    fn a_capped_message_says_what_was_cut_and_gives_it_back() {
        let big = serde_json::json!({"content": "z".repeat(100_000)}).to_string();
        let m = Message {
            seq: 4,
            kind: "tool".into(),
            tool_input_json: Some(r#"{"command":"cat"}"#.into()),
            tool_result_json: Some(big.clone()),
            ..Default::default()
        };
        let c = cap(&m);
        assert_eq!(c.tool_input_json, m.tool_input_json);
        assert!(c.tool_result_json.as_ref().unwrap().len() <= CAP);
        assert_eq!(
            c.cut,
            [Cut {
                field: "tool_result_json".into(),
                bytes: big.len() as u64,
                mark: string_mark(&big)
            }]
        );
        assert_eq!(
            uncut(&m, "tool_result_json", &c.cut[0].mark),
            Some(big.as_str())
        );
        assert_eq!(uncut(&m, "tool_result_json", "0000000000000000"), None);
        assert_eq!(uncut(&m, "text", &c.cut[0].mark), None);
        // Capping again changes nothing.
        assert_eq!(cap(&cap(&m)).tool_result_json, c.tool_result_json);
    }
}
