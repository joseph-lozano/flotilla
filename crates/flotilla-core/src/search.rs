//! Search over thread titles and messages, with SQLite FTS5 trigram indexes:
//! `message_search` over each message's text (kept in step with `messages` by the triggers in
//! `store::messages_ddl`, keyed by the row's id) and `thread_search` over titles (migration
//! 11's triggers on `threads`). What a query means and how hits read is
//! `flotilla_proto::search`.
//!
//! A query never reaches FTS5's syntax: each word goes in as a quoted string (`"…"`, a `"`
//! doubled), so `OR`, `NEAR`, `*`, `^`, `-` and `column:` are only text to find.

use crate::error::Result;
use crate::store::Store;
use crate::util::iso_to_ms;
use flotilla_proto::search::{self, HitKind, SearchHit};
use rusqlite::{OptionalExtension, params};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// The text a message row `r` (`new`, `old` or a table alias) is found by, in SQL: a prompt's,
/// steer's or reply's text; a tool call's command when it has one as text, else its summary (a
/// path, pattern or URL); nothing for a notice.
pub(crate) fn body_sql(r: &str) -> String {
    format!(
        "CASE {r}.kind WHEN 'prompt' THEN {r}.text WHEN 'steer' THEN {r}.text WHEN 'text' THEN {r}.text \
         WHEN 'tool' THEN coalesce(CASE WHEN json_valid({r}.tool_input_json) THEN CASE WHEN json_type({r}.tool_input_json, '$.command') = 'text' \
         THEN json_extract({r}.tool_input_json, '$.command') END END, {r}.tool_summary) END"
    )
}

/// What the index holds for a message row: its [`body_sql`], with curly apostrophes straight
/// and letters with a stroke without it, in either case (as [`search::fold`] makes a query's;
/// the tokenizers fold case and accents, but not these).
pub(crate) fn indexed_sql(r: &str) -> String {
    let mut sql = format!("replace(replace({}, '’', ''''), '‘', '''')", body_sql(r));
    for (stroked, to) in search::STROKES {
        for c in [stroked]
            .into_iter()
            .chain(stroked.to_uppercase().filter(|u| !u.is_ascii()))
        {
            sql = format!("replace({sql}, '{c}', '{to}')");
        }
    }
    sql
}

/// A query's words as the index holds them: letters with a stroke without it ([`indexed_sql`]).
fn unstroked(words: &[String]) -> Vec<String> {
    words
        .iter()
        .map(|w| w.chars().map(search::unstroked).collect())
        .collect()
}

/// The newest index hits a search looks among: a common word stops here instead of reading
/// every message, so time stays flat as history grows (T3's).
const CANDIDATES: usize = 10_000;
/// Hits read (their text graded) at most, of those that pass the filters: all of them. T3
/// reads 2,000; one row a thread needs more to fill its rows for a common word ("the" found 5
/// of 50 threads in the 100,000-message database at 2,000, 13 at 10,000, in 24 ms).
const READ: usize = CANDIDATES;
/// Without the index (two Chinese, Japanese or Korean characters, too short for trigrams): how
/// many of the newest messages are scanned.
const SCAN: usize = 20_000;
/// What of a query is used: its first bytes and words (a paired device's query is bounded work).
const QUERY_BYTES: usize = 1024;
const QUERY_WORDS: usize = 16;

/// A query for FTS5: each word quoted (a `"` doubled), all of them required.
fn fts_query(words: &[String]) -> Option<String> {
    let quoted: Vec<String> = words
        .iter()
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    (!quoted.is_empty()).then(|| quoted.join(" "))
}

/// A query for the word index (`message_words`): the words as typed, in order, next to each
/// other whatever punctuation is between, the last maybe the start of a longer word (still being
/// typed). None if they have no letters or digits to look up.
fn words_phrase(words: &[String]) -> Option<String> {
    let phrase = words.join(" ");
    phrase
        .chars()
        .any(char::is_alphanumeric)
        .then(|| format!("\"{}\" *", phrase.replace('"', "\"\"")))
}

/// A message as its thread shows it: who it's from, when Flotilla brought it rather than you
/// (`SearchHit::from`), and its text. Flotilla's own deliveries lose the marker, headings and
/// ids the agent reads: another thread's answer is the answer, from that thread; the answers of
/// agents started in the thread are each one's title, how it ended and its answer; a thread it
/// asked for that couldn't start is why; a task another thread (`parent`) handed this one is the
/// task, from that thread. Anything else is as written. Links read as their text, as a thread
/// shows them.
fn shown(kind: HitKind, text: &str, parent: Option<&str>) -> (Option<String>, String) {
    use flotilla_model::{orchestrate, side};
    let (from, body): (Option<String>, Cow<str>) = 'shown: {
        // Flotilla's own reads the same as a prompt or as a steer (into a running turn).
        if kind != HitKind::Prompt && !(kind == HitKind::Steer && orchestrate::by_flotilla(text)) {
            break 'shown (None, text.into());
        }
        if let (Some(members), Some(body)) = (side::parse_report(text), side::report_text(text)) {
            let from = match members.as_slice() {
                [(title, _)] => format!("{title} answered"),
                _ => format!("{} agents answered", members.len()),
            };
            break 'shown (Some(from), body.into());
        }
        if let Some((title, _, how, answer)) = orchestrate::parse_answer(text) {
            break 'shown (
                Some(format!("From {title}")),
                if answer.is_empty() { how } else { answer }.into(),
            );
        }
        if let Some((title, _, why)) = orchestrate::parse_couldnt_start(text) {
            break 'shown (Some(format!("Couldn't start {title}")), why.into());
        }
        if let Some(task) = orchestrate::task_of(text) {
            break 'shown (
                Some(format!("From {}", parent.unwrap_or("another thread"))),
                task.into(),
            );
        }
        (None, text.into())
    };
    (from, link_text(&body))
}

/// Markdown links and images as their text: `[the plan](https://…)` reads "the plan".
fn link_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let (before, at) = rest.split_at(open);
        let bang = before.ends_with('!');
        let label_end = at[1..].find([']', '\n']).map(|i| i + 1);
        let link = label_end
            .filter(|&e| at[e..].starts_with("]("))
            .and_then(|e| {
                let url = &at[e + 2..];
                url.find([')', ' ', '\t', '\n', '\r'])
                    .filter(|&k| url[k..].starts_with(')'))
                    .map(|k| (e, e + 2 + k + 1))
            });
        match link {
            Some((label, end)) => {
                out.push_str(if bang {
                    &before[..before.len() - 1]
                } else {
                    before
                });
                out.push_str(&at[1..label]);
                rest = &at[end..];
            }
            None => {
                out.push_str(before);
                out.push('[');
                rest = &at[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// What a message is found by: who it's from (as shown) and its text.
fn searched(from: Option<&str>, body: &str) -> Vec<char> {
    match from {
        Some(from) => format!("{from} {body}").chars().collect(),
        None => body.chars().collect(),
    }
}

/// A thread as search reads it.
struct Candidate {
    title: String,
    branch: Option<String>,
    project: Option<String>,
    prs: Vec<(i64, String, Vec<String>)>,
    created_ms: i64,
    /// When you last wrote in it.
    prompted: Option<i64>,
    opens: Option<search::Opens>,
}

impl Candidate {
    fn subject<'a>(&'a self, id: &'a str) -> search::Subject<'a> {
        search::Subject {
            id: Some(id),
            title: &self.title,
            branch: self.branch.as_deref(),
            project: self.project.as_deref(),
            prs: self
                .prs
                .iter()
                .map(|(n, t, refs)| search::PrRef {
                    number: *n,
                    title: Some(t.as_str()),
                    references: refs.clone(),
                })
                .collect(),
            // What you last wrote, else when it began.
            activity: Some(self.prompted.unwrap_or(self.created_ms)),
            opens: self.opens.as_ref(),
        }
    }
}

/// A message found, read.
struct Row {
    id: i64,
    thread: String,
    seq: i64,
    kind: String,
    at: Option<i64>,
    body: Option<String>,
}

/// A thread's best message for a query, and how many matched as well.
struct Best {
    row: Row,
    from: Option<String>,
    body: String,
    found: search::MessageMatch,
    hits: u32,
}

impl Store {
    /// Threads for `query` (`flotilla_proto::search`): those whose title (or PR, branch or
    /// project) matches, ranked as the palette ranks them; then, of the rest, those whose
    /// messages match, one hit a thread (its best message). At most `limit` (default
    /// `search::LIMIT`, at most `search::MAX_LIMIT`), a fifth of it kept for messages.
    pub fn search(&self, query: &str, limit: Option<u32>) -> Result<Vec<SearchHit>> {
        self.search_at(query, limit, crate::util::now_ms())
    }

    pub fn search_at(&self, query: &str, limit: Option<u32>, now: i64) -> Result<Vec<SearchHit>> {
        let limit = limit.unwrap_or(search::LIMIT).clamp(1, search::MAX_LIMIT) as usize;
        let query: Vec<&str> = flotilla_proto::history::floor(query, QUERY_BYTES)
            .split(flotilla_proto::search::separates)
            .filter(|w| !w.is_empty())
            .take(QUERY_WORDS)
            .collect();
        let query = query.join(" ");
        let folded = search::fold_query(&query);
        let threads = self.candidates()?;
        // Ties keep this order: the newest thread first, then by id, the same every time.
        let mut order: Vec<(&String, &Candidate)> = threads.iter().collect();
        order.sort_by(|(a, x), (b, y)| y.created_ms.cmp(&x.created_ms).then(a.cmp(b)));
        let ranked = search::rank(order, |(id, c), typos| {
            search::score_thread(&folded, &c.subject(id), now, typos, true)
        });
        let mut last = self.conn().prepare(
            "SELECT timestamp FROM messages WHERE thread_id=?1 ORDER BY seq DESC LIMIT 1",
        )?;
        let mut titles = Vec::new();
        for ((id, c), m) in ranked.into_iter().take(limit) {
            let at: Option<i64> = last.query_row([id], |r| r.get(0)).optional()?.flatten();
            titles.push(SearchHit {
                thread: id.clone(),
                title: c.title.clone(),
                kind: HitKind::Title,
                seq: None,
                snippet: c.title.clone(),
                marks: search::byte_ranges(&c.title, &m.ranges),
                at: at.or(Some(c.created_ms)),
                from: None,
                hits: 0,
            });
        }
        let shown: HashSet<&str> = titles.iter().map(|h| h.thread.as_str()).collect();
        let messages = self.message_hits(&query, &folded, &threads, &shown, now, limit)?;
        // Titles first, but a fifth of the room stays for messages.
        let room = messages.len().min(limit / 5);
        titles.truncate(limit - room);
        titles.extend(messages);
        titles.truncate(limit);
        Ok(titles)
    }

    /// Every thread as search reads it, by its id.
    fn candidates(&self) -> Result<HashMap<String, Candidate>> {
        let projects: HashMap<String, String> = {
            let mut st = self
                .conn()
                .prepare("SELECT id, owner || '/' || repo FROM projects")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut prs = self.pull_requests()?;
        let (mut prompted, mut opens) = (self.prompted()?, self.opens()?);
        let mut st = self
            .conn()
            .prepare("SELECT id, title, branch, created_at, repo_project_id FROM threads")?;
        type Thread = (String, String, Option<String>, String, Option<String>);
        let rows: Vec<Thread> = st
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows
            .into_iter()
            .map(|(id, title, branch, created, project)| {
                let project = project.and_then(|p| projects.get(&p).cloned());
                let prs = prs.remove(&id).map(|(pr, _)| {
                    let mut refs = vec![pr.url.clone()];
                    refs.extend(project.as_ref().map(|p| format!("{p}#{}", pr.number)));
                    (pr.number, pr.title, refs)
                });
                let c = Candidate {
                    title,
                    branch,
                    project,
                    prs: prs.into_iter().collect(),
                    created_ms: iso_to_ms(&created).unwrap_or(0),
                    prompted: prompted.remove(&id),
                    opens: opens.remove(&id),
                };
                (id, c)
            })
            .collect())
    }

    /// Of the threads not `shown` by their titles, those whose messages match, one hit a thread:
    /// its best message (the closest match, then yours, then the newest said), with how many
    /// matched as well. Ranked by word matches first, then a pick for this query, then how close,
    /// then hits and use, then the newest.
    fn message_hits(
        &self,
        query: &str,
        folded: &str,
        threads: &HashMap<String, Candidate>,
        listed: &HashSet<&str>,
        now: i64,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let tokens = search::message_tokens(query);
        let matcher = search::MessageMatcher::new(query);
        if !matcher.searches() {
            return Ok(Vec::new());
        }
        let parents = crate::orchestrate::parents(self);
        let parent = |thread: &str| {
            parents
                .get(thread)
                .and_then(|p| threads.get(p))
                .map(|c| c.title.as_str())
        };
        let mut rows = self.message_rows(&tokens, listed)?;
        let mut seen: HashSet<i64> = rows.iter().map(|r| r.id).collect();
        // A delivery is also found by who it's from ("From <a thread>"), which its text may not
        // have: if a word could be from that, Flotilla's deliveries in hand-offs' threads (an
        // answer brought to the parent, the task handed to the child) are read too.
        let froms: String = std::iter::once("From")
            .chain(
                parents
                    .values()
                    .filter_map(|p| threads.get(p))
                    .map(|c| c.title.as_str()),
            )
            .collect::<Vec<_>>()
            .join(" ");
        if !parents.is_empty()
            && tokens
                .long
                .iter()
                .any(|w| search::MessageMatcher::new(w).grade(&froms).is_some())
        {
            let mut st = self.conn().prepare(&format!(
                "SELECT m.id, m.thread_id, m.seq, m.kind, m.timestamp, {body} FROM messages m
                 WHERE m.kind IN ('prompt', 'steer') AND (m.text LIKE '[flotilla] %' OR m.text LIKE '%' || ?1)
                   AND m.thread_id IN (SELECT substr(key, {start}) FROM kv WHERE key LIKE 'parent:%' AND value != '' UNION SELECT value FROM kv WHERE key LIKE 'parent:%' AND value != '')
                 ORDER BY m.id DESC LIMIT {READ}",
                body = body_sql("m"),
                start = "parent:".len() + 1
            ))?;
            let more: Vec<Row> = st
                .query_map([flotilla_model::orchestrate::CHILD_NOTE], read_row)?
                .collect::<rusqlite::Result<_>>()?;
            rows.extend(
                more.into_iter()
                    .filter(|r| !listed.contains(r.thread.as_str()) && seen.insert(r.id)),
            );
        }
        // Each thread's best message, and how many matched that well: a thread that keeps using
        // the word ranks above a passing mention, and weaker hits ("Trust" for "rust") never add
        // to a whole word's count.
        let mut best: HashMap<String, Best> = HashMap::new();
        for row in rows {
            let (Some(kind), Some(text)) = (HitKind::of_message(&row.kind), row.body.as_deref())
            else {
                continue;
            };
            if !threads.contains_key(&row.thread) {
                continue;
            }
            let (from, body) = shown(kind, text, parent(&row.thread));
            let Some(found) = matcher.grade_chars(&searched(from.as_deref(), &body)) else {
                continue;
            };
            let better = |b: &Best| {
                found
                    .quality
                    .partial_cmp(&b.found.quality)
                    .unwrap_or(Ordering::Equal)
                    .then(
                        kind.yours()
                            .cmp(&HitKind::of_message(&b.row.kind).is_some_and(HitKind::yours)),
                    )
                    .then(row.at.cmp(&b.row.at))
                    .is_gt()
            };
            match best.get_mut(&row.thread) {
                Some(b) => {
                    let hits = if found.quality > b.found.quality {
                        1
                    } else {
                        b.hits + u32::from(found.quality == b.found.quality)
                    };
                    if better(b) {
                        *b = Best {
                            row,
                            from,
                            body,
                            found,
                            hits,
                        };
                    } else {
                        b.hits = hits;
                    }
                }
                None => {
                    best.insert(
                        row.thread.clone(),
                        Best {
                            row,
                            from,
                            body,
                            found,
                            hits: 1,
                        },
                    );
                }
            }
        }
        let mut ranked: Vec<(search::MessageRank, Best)> = best
            .into_values()
            .map(|b| {
                let c = &threads[&b.row.thread];
                let rank = search::MessageRank {
                    quality: b.found.quality,
                    hits: b.hits,
                    at: b.row.at,
                    picked_at: search::MessageRank::picked(c.opens.as_ref(), folded, now),
                    // Use counts what you last wrote, not when it began.
                    boost: search::usage_boost(c.prompted, c.opens.as_ref(), now),
                };
                (rank, b)
            })
            .collect();
        ranked
            .sort_by(|(l, a), (r, b)| search::compare_messages(l, r).then(b.row.id.cmp(&a.row.id)));
        ranked.truncate(limit);
        Ok(ranked
            .into_iter()
            .map(|(_, b)| {
                let body: Vec<char> = b.body.chars().collect();
                // The match's place in what's shown, past who it's from.
                let lead = b.from.as_ref().map_or(0, |f| f.chars().count() + 1);
                let snippet = search::snippet(&body, b.found.offset.saturating_sub(lead));
                let marks = search::byte_ranges(&snippet, &search::token_ranges(&snippet, query));
                let kind = HitKind::of_message(&b.row.kind).unwrap_or(HitKind::Text);
                SearchHit {
                    title: threads[&b.row.thread].title.clone(),
                    thread: b.row.thread,
                    kind,
                    seq: Some(b.row.seq),
                    snippet,
                    marks,
                    at: b.row.at,
                    from: b.from,
                    hits: b.hits,
                }
            })
            .collect())
    }

    /// The messages a search reads, the newest first: the newest [`CANDIDATES`] the index has
    /// for the long words, in threads not `shown`, at most [`READ`]. Past those, the older ones
    /// with the words as typed, next to each other (few have them), however old: from the word
    /// index, whatever punctuation is between, and from the trigrams, as one string (scripts
    /// without spaces). Without the index (a two-character Chinese, Japanese or Korean word),
    /// the newest [`SCAN`] messages. The short words are left to the matcher, which folds case
    /// and accents as SQL can't.
    fn message_rows(&self, tokens: &search::Tokens, shown: &HashSet<&str>) -> Result<Vec<Row>> {
        let shown = serde_json::to_string(&shown.iter().collect::<Vec<_>>())
            .unwrap_or_else(|_| "[]".into());
        let body = body_sql("m");
        let (long, words) = (unstroked(&tokens.long), unstroked(&tokens.words));
        let looked_up = long.iter().all(|t| t.chars().count() >= search::INDEXED);
        let Some(fts) = fts_query(&long).filter(|_| looked_up) else {
            let mut st = self.conn().prepare(&format!(
                "SELECT m.id, m.thread_id, m.seq, m.kind, m.timestamp, {body} FROM messages m
                 WHERE {body} IS NOT NULL AND m.id > (SELECT coalesce(max(id), 0) FROM messages) - {SCAN}
                   AND m.thread_id NOT IN (SELECT value FROM json_each(?1))
                 ORDER BY m.id DESC LIMIT {SCAN}"
            ))?;
            return Ok(st
                .query_map([&shown], read_row)?
                .collect::<rusqlite::Result<_>>()?);
        };
        // Where the newest CANDIDATES end, if the index has more.
        let cut: Option<i64> = self.conn().query_row(&format!("SELECT rowid FROM message_search WHERE message_search MATCH ?1 ORDER BY rowid DESC LIMIT 1 OFFSET {}", CANDIDATES - 1), [&fts], |r| r.get(0)).optional()?;
        let select = |index: &str, older: bool| {
            format!(
                "SELECT m.id, m.thread_id, m.seq, m.kind, m.timestamp, {body} FROM {index} JOIN messages m ON m.id = {index}.rowid
                 WHERE {index} MATCH ?1 AND {index}.rowid {cmp} ?2
                   AND m.thread_id NOT IN (SELECT value FROM json_each(?3))
                 ORDER BY {index}.rowid DESC LIMIT {READ}",
                cmp = if older { "<" } else { ">=" },
            )
        };
        let mut st = self.conn().prepare(&select("message_search", false))?;
        let mut rows: Vec<Row> = st
            .query_map(params![fts, cut.unwrap_or(0), shown], read_row)?
            .collect::<rusqlite::Result<_>>()?;
        if let Some(cut) = cut
            && words.len() > 1
        {
            let mut seen: HashSet<i64> = rows.iter().map(|r| r.id).collect();
            for (index, q) in [
                ("message_words", words_phrase(&words)),
                ("message_search", fts_query(&[words.join(" ")])),
            ] {
                let Some(q) = q else { continue };
                let mut st = self.conn().prepare(&select(index, true))?;
                let more: Vec<Row> = st
                    .query_map(params![q, cut, shown], read_row)?
                    .collect::<rusqlite::Result<_>>()?;
                rows.extend(more.into_iter().filter(|r| seen.insert(r.id)));
            }
        }
        Ok(rows)
    }
}

fn read_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        thread: r.get(1)?,
        seq: r.get(2)?,
        kind: r.get(3)?,
        at: r.get(4)?,
        body: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_proto::{Message, Thread};

    const NOW: i64 = 1_793_000_000_000;
    const MIN: i64 = 60_000;

    fn thread(id: &str, title: &str) -> Thread {
        Thread {
            title: title.into(),
            ..super::tests_thread(id, title)
        }
    }

    /// Write `rows` (seq, kind, text or tool command, ms) as the thread's messages, as
    /// `rebuild_messages` would.
    fn write(store: &Store, id: &str, rows: &[(i64, &str, &str, i64)]) {
        let tx = store.write().unwrap();
        tx.execute("DELETE FROM messages WHERE thread_id=?1", [id])
            .unwrap();
        for (seq, kind, text, at) in rows {
            let m = if *kind == "tool" {
                Message {
                    seq: *seq,
                    kind: "tool".into(),
                    tool_name: Some("Bash".into()),
                    tool_summary: Some(crate::util::ellipsize(text, 120)),
                    tool_input_json: Some(serde_json::json!({ "command": text }).to_string()),
                    timestamp: Some(*at),
                    ..Default::default()
                }
            } else {
                Message {
                    seq: *seq,
                    kind: kind.to_string(),
                    text: Some(text.to_string()),
                    timestamp: Some(*at),
                    ..Default::default()
                }
            };
            tx.execute(
                "INSERT INTO messages (thread_id, seq, kind, text, tool_name, tool_summary, tool_input_json, timestamp, mark) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![id, m.seq, m.kind, m.text, m.tool_name, m.tool_summary, m.tool_input_json, m.timestamp, flotilla_proto::history::message_mark(&m) as i64],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    /// `n` rows of `text` from `seq` on, all said at `at`.
    fn many(
        n: i64,
        kind: &'static str,
        text: &'static str,
        at: i64,
    ) -> Vec<(i64, &'static str, &'static str, i64)> {
        (0..n).map(|k| (k, kind, text, at)).collect()
    }

    fn with(threads: &[(&str, &str)]) -> Store {
        let store = Store::open_in_memory().unwrap();
        for (id, title) in threads {
            store.insert_thread(&thread(id, title)).unwrap();
        }
        store
    }

    fn found(store: &Store, q: &str) -> Vec<(String, HitKind, Option<i64>)> {
        store
            .search_at(q, None, NOW)
            .unwrap()
            .into_iter()
            .map(|h| (h.thread, h.kind, h.seq))
            .collect()
    }

    fn threads(store: &Store, q: &str, limit: Option<u32>) -> Vec<String> {
        store
            .search_at(q, limit, NOW)
            .unwrap()
            .into_iter()
            .map(|h| h.thread)
            .collect()
    }

    /// A control character (a NUL) in a query only separates words: no error, and what the
    /// rest says is found.
    #[test]
    fn a_nul_in_a_query_finds_what_the_rest_says() {
        let store = with(&[("a", "Retry loop")]);
        write(
            &store,
            "a",
            &[(0, "text", "the retry loop backs off", NOW - MIN)],
        );
        for q in ["retry\0", "re\0try", "\0", "retry\u{1}loop"] {
            assert!(store.search_at(q, None, NOW).is_ok(), "{q:?}");
        }
        assert_eq!(found(&store, "retry\0loop"), found(&store, "retry loop"));
        assert!(!found(&store, "retry\0loop").is_empty());
        assert!(found(&store, "\0").is_empty());
    }

    #[test]
    fn one_hit_a_thread_its_best_match_then_yours_then_the_newest() {
        let store = with(&[("both", "Both"), ("reply", "Reply only")]);
        write(
            &store,
            "both",
            &[
                (0, "text", "needle from the answer", NOW - 3 * MIN),
                (1, "prompt", "older needle question", NOW - 5 * MIN),
                (2, "prompt", "newer needle question", NOW - 4 * MIN),
                (3, "notice", "needle notice", NOW),
            ],
        );
        write(
            &store,
            "reply",
            &[(0, "text", "needle in an answer", NOW - 9 * MIN)],
        );
        let mut hits = store.search_at("NEEDLE", Some(20), NOW).unwrap();
        hits.sort_by(|a, b| a.thread.cmp(&b.thread));
        let got: Vec<(&str, HitKind, &str, u32)> = hits
            .iter()
            .map(|h| (h.thread.as_str(), h.kind, h.snippet.as_str(), h.hits))
            .collect();
        assert_eq!(
            got,
            [
                ("both", HitKind::Prompt, "newer needle question", 3),
                ("reply", HitKind::Text, "needle in an answer", 1)
            ],
            "notices aren't searched"
        );
        assert_eq!(store.search_at("needle", Some(1), NOW).unwrap().len(), 1);
        assert!(found(&store, "ne%le").is_empty());
    }

    /// A letter with a stroke is found by the letter (as titles are), either way round, and an
    /// accent in a short word (or with a two-character Chinese one) is ignored as in a long one.
    #[test]
    fn strokes_and_accents_in_short_words_are_found_as_in_titles() {
        for (body, q) in [
            ("Visit Łódź tomorrow", "lodz"),
            ("Visit Lodz tomorrow", "łódź"),
            ("Visit Łódź tomorrow", "lodz tomorrow"),
            ("a smørrebrød recipe", "smorrebrod"),
            ("a smorrebrod recipe", "smørrebrød"),
            ("Đorđe said so", "dorde"),
            ("Dorde said so", "đorđe"),
            ("Ħal Far road", "hal far"),
            ("the ŧest file", "test file"),
            ("Ça marche bien", "ça marche"),
            ("Ça marche bien", "ca marche"),
            ("Où est le fichier", "ou est"),
            ("Où est le fichier", "où est"),
            ("日本語 Café", "日本 cafe"),
            ("日本語 Cafe", "日本 café"),
            ("日本語 Émile", "日本 émile"),
        ] {
            let store = with(&[("t", "Unrelated")]);
            write(&store, "t", &[(0, "text", body, NOW - MIN)]);
            assert_eq!(
                found(&store, q),
                [("t".into(), HitKind::Text, Some(0))],
                "{q:?} in {body:?}"
            );
            // And a message without it isn't.
            write(
                &store,
                "t",
                &[(0, "text", "nothing like it here at all", NOW - MIN)],
            );
            assert!(found(&store, q).is_empty(), "{q:?}");
        }
    }

    #[test]
    fn words_inside_identifiers_and_short_words_only_narrow() {
        let store = with(&[("p", "Parent")]);
        write(
            &store,
            "p",
            &[(
                0,
                "prompt",
                "Rename ThreadSearchService before shipping",
                NOW,
            )],
        );
        assert_eq!(
            store
                .search_at("search servi", None, NOW)
                .unwrap()
                .iter()
                .map(|h| h.snippet.as_str())
                .collect::<Vec<_>>(),
            ["Rename ThreadSearchService before shipping"]
        );
        // "ab" alone can't use the index; with a longer word it narrows what that found.
        assert!(found(&store, "ab").is_empty());
        assert_eq!(found(&store, "renam be").len(), 1);
        assert!(found(&store, "renam ab").is_empty());
    }

    #[test]
    fn of_equal_matches_the_newest_said_first_not_the_newest_written() {
        // An imported history lands after newer messages in row order.
        let store = with(&[("recent", "Recent"), ("imported", "Imported")]);
        write(
            &store,
            "recent",
            &[(0, "prompt", "kestrel deploy notes", NOW - 30 * MIN)],
        );
        write(
            &store,
            "imported",
            &[(0, "prompt", "kestrel deploy notes", NOW - 59 * MIN)],
        );
        assert_eq!(threads(&store, "kestrel", None), ["recent", "imported"]);
    }

    #[test]
    fn a_thread_about_the_word_ranks_above_a_passing_mention_counting_its_best_matches_only() {
        let store = with(&[("about", "About"), ("mention", "Mention")]);
        write(
            &store,
            "about",
            &many(12, "text", "osprey step", NOW - 58 * MIN),
        );
        write(
            &store,
            "mention",
            &[(0, "prompt", "also check osprey", NOW - 55 * MIN)],
        );
        let hits = store.search_at("osprey", None, NOW).unwrap();
        assert_eq!(
            hits.iter()
                .map(|h| (h.thread.as_str(), h.hits))
                .collect::<Vec<_>>(),
            [("about", 12), ("mention", 1)]
        );
        // "lark" inside "Clark" is weaker: it never adds to a whole word's count.
        let store = with(&[("clarks", "Notes"), ("larks", "Plans")]);
        let mut rows = vec![(0, "prompt", "Lark once", NOW - 57 * MIN)];
        rows.extend((1..=31).map(|k| (k, "text", "Clark the plan", NOW - 57 * MIN)));
        write(&store, "clarks", &rows);
        write(
            &store,
            "larks",
            &many(8, "text", "Lark again", NOW - 57 * MIN),
        );
        let hits = store.search_at("lark", None, NOW).unwrap();
        assert_eq!(
            hits.iter()
                .map(|h| (h.thread.as_str(), h.hits))
                .collect::<Vec<_>>(),
            [("larks", 8), ("clarks", 1)]
        );
    }

    #[test]
    fn a_match_behind_hundreds_of_narrower_hits_is_found() {
        let store = with(&[("parent", "Parent"), ("older", "Older")]);
        write(
            &store,
            "older",
            &many(450, "text", "pnpm 11 pnpm", NOW - 60 * MIN),
        );
        write(
            &store,
            "parent",
            &[
                (
                    0,
                    "prompt",
                    "a needle in a long haystack of other words",
                    NOW - 50 * MIN,
                ),
                (
                    1,
                    "prompt",
                    "moved the repo to pnpm 12 today",
                    NOW - 40 * MIN,
                ),
            ],
        );
        assert_eq!(threads(&store, "pnpm 12", None), ["parent"]);
    }

    #[test]
    fn a_thread_picked_for_the_query_stays_when_the_limit_cuts() {
        let store = with(&[("busy", "Busy"), ("picked", "Picked")]);
        write(
            &store,
            "busy",
            &[
                (0, "prompt", "plover notes", NOW - 57 * MIN),
                (1, "prompt", "more plover notes", NOW - 56 * MIN),
            ],
        );
        write(
            &store,
            "picked",
            &[(0, "prompt", "one plover mention", NOW - 59 * MIN)],
        );
        assert_eq!(threads(&store, "plover", Some(1)), ["busy"]);
        store
            .record_open("picked", NOW - MIN, Some("plover"))
            .unwrap();
        assert_eq!(threads(&store, "plover", Some(1)), ["picked"]);
        // Its pick is the same for everyone that asks: the server keeps it.
        assert_eq!(store.opens().unwrap()["picked"].picks[0].query, "plover");
    }

    #[test]
    fn a_curly_apostrophe_finds_a_straight_one_and_back() {
        let store = with(&[("curly", "Curly"), ("straight", "Straight")]);
        write(
            &store,
            "curly",
            &[(0, "prompt", "the gannet\u{2019}s nest", NOW)],
        );
        write(
            &store,
            "straight",
            &[(0, "prompt", "a kittiwake's nest", NOW)],
        );
        assert_eq!(threads(&store, "gannet's", None), ["curly"]);
        assert_eq!(threads(&store, "kittiwake\u{2019}s", None), ["straight"]);
    }

    #[test]
    fn messages_are_matched_as_they_read_with_links_as_their_text() {
        let store = with(&[("labeled", "Labeled"), ("address", "Address only")]);
        write(
            &store,
            "labeled",
            &[(0, "text", "See [the heron plan](https://x.dev/a) now", NOW)],
        );
        write(
            &store,
            "address",
            &[(0, "prompt", "Read [this](https://heron.dev/b)", NOW)],
        );
        let hits = store.search_at("heron", None, NOW).unwrap();
        assert_eq!(
            hits.iter()
                .map(|h| (h.thread.as_str(), h.snippet.as_str()))
                .collect::<Vec<_>>(),
            [("labeled", "See the heron plan now")]
        );
        assert_eq!(
            link_text("![a chart](x.png) and [x] (y) and [a](b c)"),
            "a chart and [x] (y) and [a](b c)"
        );
    }

    #[test]
    fn the_newest_ten_thousand_index_hits_are_read_but_the_words_as_typed_however_old() {
        let store = with(&[("old", "Old"), ("busy", "Busy")]);
        write(
            &store,
            "old",
            &[(0, "prompt", "kestrel ci is red", NOW - 59 * MIN)],
        );
        write(
            &store,
            "busy",
            &many(
                CANDIDATES as i64 - 1,
                "text",
                "kestrel packages",
                NOW - 55 * MIN,
            ),
        );
        assert_eq!(threads(&store, "kestrel ci", None), ["old"]);
        write(
            &store,
            "busy",
            &many(
                CANDIDATES as i64,
                "text",
                "kestrel packages",
                NOW - 55 * MIN,
            ),
        );
        // Past the newest 10,000: the words apart aren't read (T3 stops there)...
        assert!(threads(&store, "ci kestrel", None).is_empty());
        // ...but the words as typed, together, are found however old (Flotilla's).
        assert_eq!(threads(&store, "kestrel ci", None), ["old"]);
    }

    #[test]
    fn korean_japanese_two_character_chinese_and_whole_short_words() {
        let store = with(&[
            ("s", "Scripts"),
            ("drafts", "Drafts"),
            ("addresses", "Addresses"),
        ]);
        write(
            &store,
            "s",
            &[
                (0, "prompt", "검색 기능을 고쳐 주세요", NOW),
                (1, "prompt", "がんばって検索します", NOW),
                (2, "prompt", "明天部署到生产环境", NOW),
                (3, "text", "the FTS index is ready", NOW),
                (4, "text", "Configure enableFTS today", NOW),
                (5, "prompt", "Review Résumé indexing", NOW),
            ],
        );
        write(
            &store,
            "drafts",
            &[(0, "text", "saved two drafts and shifts", NOW)],
        );
        write(
            &store,
            "addresses",
            &[(0, "prompt", "This addresses Résumé indexing", NOW)],
        );
        for q in ["검색 기능", "がんばって", "部署", "fts", "enable"] {
            assert_eq!(threads(&store, q, None), ["s"], "{q}");
        }
        // "res" inside "addresses" doesn't hide the accented word after it.
        let mut res = threads(&store, "res", None);
        res.sort();
        assert_eq!(res, ["addresses", "s"]);
        // A word beats the same letters inside a longer word, even in an older message.
        let store = with(&[("rust", "Notes"), ("trust", "Build")]);
        write(
            &store,
            "rust",
            &[(0, "text", "Rust rewrite notes", NOW - 9 * MIN)],
        );
        write(&store, "trust", &[(0, "prompt", "We trust the build", NOW)]);
        assert_eq!(threads(&store, "rust", None), ["rust", "trust"]);
    }

    #[test]
    fn titles_rank_as_the_palette_ranks_them_then_messages_with_room_kept_for_them() {
        let store = with(&[
            ("mid", "Fix oauth redirect"),
            ("start", "Clean up auth tokens"),
            ("message", "Unrelated"),
        ]);
        write(
            &store,
            "message",
            &[(0, "prompt", "the auth flow broke again", NOW)],
        );
        let got: Vec<(String, HitKind)> = store
            .search_at("auth", Some(3), NOW)
            .unwrap()
            .into_iter()
            .map(|h| (h.thread, h.kind))
            .collect();
        assert_eq!(
            got,
            [
                ("start".into(), HitKind::Title),
                ("mid".into(), HitKind::Title),
                ("message".into(), HitKind::Prompt)
            ]
        );
        assert_eq!(
            threads(&store, "auth", Some(1)),
            ["start"],
            "with one slot, the best title keeps it"
        );
        // Nothing matches exactly: one typo is forgiven.
        assert_eq!(threads(&store, "tokesn", None), ["start"]);
        // Titled threads are left out in the query, before the read stops: a thread found only by
        // an older message still comes, behind a titled one with all but one of the newest hits.
        let store = with(&[("message", "Unrelated"), ("titled", "Grebe")]);
        write(
            &store,
            "message",
            &[(0, "prompt", "a grebe note", NOW - 59 * MIN)],
        );
        write(
            &store,
            "titled",
            &many(CANDIDATES as i64 - 1, "text", "grebe again", NOW - 30 * MIN),
        );
        assert_eq!(threads(&store, "grebe", Some(5)), ["titled", "message"]);
    }

    #[test]
    fn equal_titles_come_in_the_same_order_every_time() {
        let store = Store::open_in_memory().unwrap();
        for (id, title, created) in [
            ("b", "Fix alpha", "2026-10-30T09:00:00.000Z"),
            ("a", "Fix bravo", "2026-10-30T09:00:00.000Z"),
            ("c", "Fix charlie", "2026-10-31T09:00:00.000Z"),
        ] {
            store
                .insert_thread(&Thread {
                    created_at: created.into(),
                    ..thread(id, title)
                })
                .unwrap();
        }
        // The newest first, then by id; a fresh map each search mustn't reorder them.
        for _ in 0..20 {
            assert_eq!(threads(&store, "fix", None), ["c", "a", "b"]);
            assert_eq!(threads(&store, "fix", Some(1)), ["c"]);
        }
    }

    #[test]
    fn opens_rank_threads_and_are_kept_per_thread() {
        let store = with(&[("a", "Goldbug epilogue"), ("b", "Goldbug notes")]);
        assert_eq!(threads(&store, "goldbug", None).len(), 2);
        for k in 0..6 {
            store
                .record_open("b", NOW - (k + 1) * 86_400_000, None)
                .unwrap();
        }
        assert_eq!(threads(&store, "goldbug", None), ["b", "a"], "opened often");
        // Opens of a thread that's gone aren't kept; a deleted thread takes its own.
        store.record_open("gone", NOW, Some("x")).unwrap();
        store.delete_thread("b").unwrap();
        assert!(store.opens().unwrap().is_empty());
        // The sidebar carries them, and when you last wrote.
        store.record_open("a", NOW, Some("gold")).unwrap();
        write(
            &store,
            "a",
            &[
                (0, "prompt", "hello", NOW - MIN),
                (1, "prompt", "[flotilla] Agents answered (1):", NOW),
            ],
        );
        let s = store
            .thread_summaries()
            .unwrap()
            .into_iter()
            .find(|s| s.thread.id == "a")
            .unwrap();
        assert_eq!(
            (s.prompted_ms, s.opens.map(|o| o.picks[0].query.clone())),
            (Some(NOW - MIN), Some("gold".into()))
        );
    }

    #[test]
    fn a_delivery_is_found_by_who_its_from_among_many_prompts_and_many_hand_offs() {
        let store = with(&[("p", "Palindrome function"), ("c", "Review palindrome.py")]);
        crate::orchestrate::set_parent(&store, "c", "p").unwrap();
        let task = flotilla_model::orchestrate::child_prompt("concisely describe the bug");
        write(&store, "c", &[(0, "prompt", &task, 1)]);
        let prompts: Vec<(i64, &str, &str, i64)> = (0..READ as i64 + 10)
            .map(|k| {
                (
                    k,
                    "prompt",
                    "an ordinary prompt, nothing handed over",
                    10 + k,
                )
            })
            .collect();
        write(&store, "p", &prompts);
        {
            let tx = store.write().unwrap();
            for k in 0..20_000 {
                tx.execute(
                    "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, '')",
                    params![format!("parent:child-{k}"), format!("parent-{k}")],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        assert_eq!(
            found(&store, "palindrome concisely"),
            [("c".into(), HitKind::Prompt, Some(0))]
        );
        assert_eq!(
            found(&store, "from palindrome")
                .iter()
                .filter(|h| h.2.is_some())
                .count(),
            1
        );
    }

    #[test]
    fn flotillas_own_deliveries_read_as_the_thread_shows_them() {
        use flotilla_model::orchestrate;
        let store = with(&[("p", "Palindrome function"), ("c", "Review palindrome.py")]);
        crate::orchestrate::set_parent(&store, "c", "p").unwrap();
        let id = "6a532dfb-d90c-4b9a-9f0e-0123456789ab";
        let report = format!(
            "[flotilla] Agents answered (2):\n\n### \"Review slug.py (gpt-6.1-sol)\" ({id}) finished:\n\nUnicode dashes are deleted.\n\n### \"Review slug.py (claude)\" (b2) failed without an answer: no key."
        );
        let answer = orchestrate::wake_text(
            "c",
            "Review palindrome.py",
            Some("flotilla/review"),
            Some("- **Correctness**: No bug for the stated behavior."),
            "finished",
        );
        let task = orchestrate::child_prompt(
            "Say concisely what you would change: correctness, edge cases.",
        );
        write(
            &store,
            "p",
            &[
                (0, "prompt", &report, 1000),
                (1, "prompt", &answer, 2000),
                (
                    2,
                    "prompt",
                    "[flotilla] Couldn't start \"X\" (c9): boat said 402",
                    2500,
                ),
            ],
        );
        write(&store, "c", &[(0, "prompt", &task, 3000)]);
        let hit = |q: &str| {
            let hits: Vec<SearchHit> = store
                .search_at(q, None, NOW)
                .unwrap()
                .into_iter()
                .filter(|h| h.kind != HitKind::Title)
                .collect();
            assert_eq!(hits.len(), 1, "{q}: {hits:?}");
            let h = hits.into_iter().next().unwrap();
            (h.from.clone().unwrap_or_default(), h.snippet.clone(), h)
        };
        let (from, snippet, _) = hit("unicode dashes");
        assert_eq!(from, "2 agents answered");
        assert!(snippet.contains("Unicode dashes are deleted. Review slug.py (claude) failed without an answer: no key."), "{snippet}");
        assert!(
            !snippet.contains("[flotilla]")
                && !snippet.contains("###")
                && !snippet.contains(id)
                && !snippet.contains("(b2)"),
            "{snippet}"
        );
        assert_eq!(hit("agents answered gpt-6.1-sol").0, "2 agents answered");
        for q in [id, "6a532dfb", "###"] {
            assert!(
                store
                    .search_at(q, None, NOW)
                    .unwrap()
                    .iter()
                    .all(|h| h.kind == HitKind::Title || h.seq == Some(2)),
                "{q}"
            );
        }
        let (from, snippet, h) = hit("correctness bug");
        assert_eq!(
            (from.as_str(), snippet.as_str(), h.thread.as_str()),
            (
                "From Review palindrome.py",
                "- Correctness: No bug for the stated behavior.",
                "p"
            )
        );
        let (from, snippet, h) = hit("concisely");
        assert_eq!(
            (from.as_str(), snippet.as_str(), h.thread.as_str()),
            (
                "From Palindrome function",
                "Say concisely what you would change: correctness, edge cases.",
                "c"
            )
        );
        assert_eq!(hit("palindrome concisely").2.seq, Some(0));
        assert_eq!(hit("from palindrome function").2.thread, "c");
        assert_eq!(
            hit("from review palindrome").2.thread,
            "p",
            "the answer, from the thread it was handed to"
        );
        let (from, snippet, _) = hit("402");
        assert_eq!(
            (from.as_str(), snippet.as_str()),
            ("Couldn't start X", "boat said 402")
        );
        assert!(
            store.search_at("c9", None, NOW).unwrap().is_empty(),
            "not by its id"
        );
        // Into a running turn, it went as a steer: it reads the same.
        let steered = "[flotilla] Agents answered (1):\n\n### \"Sleep then ready\" (f72a1419-dbb3-47cd-a19c-d30eb2255730) finished:\n\nready and waiting";
        let answer = orchestrate::wake_text(
            "c",
            "Review palindrome.py",
            None,
            Some("All lint clean."),
            "finished",
        );
        write(
            &store,
            "p",
            &[
                (0, "prompt", "hello", 1000),
                (1, "steer", steered, 2000),
                (2, "steer", &answer, 2500),
            ],
        );
        let (from, snippet, h) = hit("ready and waiting");
        assert_eq!(
            (from.as_str(), h.kind, h.thread.as_str()),
            ("Sleep then ready answered", HitKind::Steer, "p")
        );
        assert!(
            !snippet.contains("[flotilla]") && !snippet.contains("f72a1419"),
            "{snippet}"
        );
        assert_eq!(hit("lint clean").0, "From Review palindrome.py");
        let (_, _, h) = hit("from review palindrome");
        assert_eq!(
            (h.thread.as_str(), h.seq),
            ("p", Some(2)),
            "a steered answer, by the thread it came from"
        );
    }

    #[test]
    fn fts_syntax_in_a_query_is_only_text() {
        let store = with(&[("a", "A")]);
        write(
            &store,
            "a",
            &[(
                0,
                "text",
                "use NEAR(a b) OR c* and \"quotes\" -x col:val ^start",
                1000,
            )],
        );
        for q in [
            "NEAR(a b)",
            "OR c*",
            "col:val",
            "^start",
            "-x",
            "\"quotes\"",
            "\"",
            "\"\"\"",
            "a\"b",
            "*",
            "AND",
            "NOT",
        ] {
            let hits = store.search_at(q, None, NOW);
            assert!(hits.is_ok(), "{q}: {hits:?}");
        }
        assert_eq!(found(&store, "NEAR(a").len(), 1);
        assert_eq!(
            found(&store, "c* col:val"),
            vec![("a".into(), HitKind::Text, Some(0))]
        );
        assert_eq!(found(&store, "start OR zebra"), vec![]);
        // A huge query is cut to its first words, on a character.
        let huge = format!("{} {}", "start ".repeat(5000), "é".repeat(5000));
        assert_eq!(
            found(&store, &huge),
            vec![("a".into(), HitKind::Text, Some(0))]
        );
    }

    #[test]
    fn the_index_follows_edits_deletes_and_renames() {
        let store = with(&[("a", "Fix the retry loop"), ("b", "Café menu")]);
        write(
            &store,
            "a",
            &[
                (
                    0,
                    "prompt",
                    "Look at the retry module and run the tests",
                    1000,
                ),
                (1, "tool", "cargo test --workspace -- retry", 2000),
                (2, "text", "The retry loop now backs off.", 3000),
                (3, "notice", "error: retry failed", 3500),
            ],
        );
        write(
            &store,
            "b",
            &[
                (0, "prompt", "Write the café’s menu in Markdown", 5000),
                (1, "text", "Here's the **menu**: soup, bread.", 6000),
            ],
        );
        let entries = || -> i64 {
            store
                .conn()
                .query_row("SELECT count(*) FROM message_search", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(entries(), 5);
        store
            .conn()
            .execute(
                "UPDATE messages SET text='The loop now waits.' WHERE thread_id='a' AND seq=2",
                [],
            )
            .unwrap();
        assert_eq!(found(&store, "backs"), vec![]);
        assert_eq!(
            found(&store, "waits"),
            vec![("a".into(), HitKind::Text, Some(2))]
        );
        store
            .conn()
            .execute(
                "UPDATE messages SET status='done' WHERE thread_id='a' AND seq=2",
                [],
            )
            .unwrap();
        assert_eq!(found(&store, "waits").len(), 1);
        store
            .conn()
            .execute("DELETE FROM messages WHERE thread_id='a' AND seq=1", [])
            .unwrap();
        assert_eq!(found(&store, "workspace"), vec![]);
        assert_eq!(entries(), 4);
        store.delete_thread("b").unwrap();
        assert_eq!(found(&store, "menu"), vec![]);
        assert_eq!(entries(), 2);
        store.set_thread_title("a", "Backoff").unwrap();
        assert_eq!(
            found(&store, "backoff"),
            vec![("a".into(), HitKind::Title, None)]
        );
        assert_eq!(
            found(&store, "retry loop")
                .iter()
                .filter(|h| h.1 == HitKind::Title)
                .count(),
            0
        );
    }

    #[test]
    fn a_tool_is_found_by_its_whole_command_and_snippets_stay_short() {
        let store = with(&[("a", "A"), ("b", "B")]);
        let long = format!(
            "python3 - <<'EOF'\n{}\nprint('needle')\nEOF",
            "x = 1\n".repeat(50_000)
        );
        write(&store, "a", &[(0, "tool", &long, 1000)]);
        write(
            &store,
            "b",
            &[(0, "tool", "日本語のコメント ✓ grep -rn コメント src/", 2000)],
        );
        let hits = store.search_at("needle", None, NOW).unwrap();
        assert_eq!(hits.len(), 1, "found past the summary's 120 characters");
        let h = &hits[0];
        assert!(
            h.snippet.chars().count() <= search::SNIPPET + 2 && h.snippet.contains("needle"),
            "{}",
            h.snippet
        );
        for [a, b] in &h.marks {
            assert_eq!(&h.snippet[*a as usize..*b as usize], "needle");
        }
        let hits = store.search_at("コメント", None, NOW).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].marks.len(), 2);
        assert!(
            hits[0]
                .marks
                .iter()
                .all(|[a, b]| hits[0].snippet.is_char_boundary(*a as usize)
                    && hits[0].snippet.is_char_boundary(*b as usize))
        );
    }
}

/// The index through the store's own write path: events in, `rebuild_messages`, whatever
/// happens to the thread after.
#[cfg(test)]
mod rebuilds {
    use crate::event::RawEvent;
    use crate::store::Store;
    use flotilla_proto::HitKind;
    use rusqlite::params;

    fn turn(k: usize, reply: &str, command: Option<&str>) -> Vec<RawEvent> {
        let task = format!("task-{k}");
        let ts = 1_793_000_000_000i64 + k as i64 * 10_000;
        let mut data = serde_json::json!({"content": reply, "model": "m"});
        if let Some(cmd) = command {
            let result =
                serde_json::json!({"type": "bash_output", "command": cmd, "content": "ok"})
                    .to_string();
            data["tools"] = serde_json::json!([{"use": {"id": format!("toolu_{k}"), "name": "Bash", "input": {"command": cmd}, "type": "tool_use"}, "result": {"content": result, "is_error": false, "tool_use_id": format!("toolu_{k}"), "type": "tool_result"}}]);
        }
        vec![
            RawEvent::from_value(
                serde_json::json!({"id": format!("p{k}"), "taskId": task, "type": "prompt", "timestamp": ts, "conversationId": "c", "data": {"prompt": format!("prompt number {k}"), "status": "finished"}}),
            ),
            RawEvent::from_value(
                serde_json::json!({"id": format!("r{k}"), "taskId": task, "type": "response", "timestamp": ts + 1000, "conversationId": "c", "data": data}),
            ),
        ]
    }

    fn store_with(turns: usize) -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_thread(&super::tests_thread("t1", "Long thread"))
            .unwrap();
        let events: Vec<RawEvent> = (0..turns)
            .flat_map(|k| {
                turn(
                    k,
                    &format!("reply {k}: all good"),
                    (k % 3 == 0).then_some("cargo test --release"),
                )
            })
            .collect();
        store.apply_events("t1", &events, false).unwrap();
        store.rebuild_messages("t1").unwrap();
        store
    }

    /// The index holds exactly the searchable messages, each by its row's id.
    fn in_step(store: &Store) {
        let conn = store.conn();
        let want: Vec<i64> = {
            let mut st = conn
                .prepare(&format!(
                    "SELECT id FROM messages m WHERE {} IS NOT NULL ORDER BY id",
                    super::body_sql("m")
                ))
                .unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        let held: Vec<i64> = {
            let mut st = conn
                .prepare("SELECT rowid FROM message_search ORDER BY rowid")
                .unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            held, want,
            "the index holds exactly the searchable messages"
        );
        let words: Vec<i64> = {
            let mut st = conn
                .prepare("SELECT rowid FROM message_words ORDER BY rowid")
                .unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(words, want, "the word index too");
        // And each by its own text: every message is found by a word of its own.
        let mut st = conn
            .prepare(&format!(
                "SELECT id, {} FROM messages m WHERE {} IS NOT NULL",
                super::body_sql("m"),
                super::body_sql("m")
            ))
            .unwrap();
        let rows: Vec<(i64, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for (id, body) in rows {
            let word = body
                .split_whitespace()
                .max_by_key(|w| w.len())
                .unwrap()
                .replace('"', "");
            let found: bool = conn.query_row("SELECT count(*) > 0 FROM message_search WHERE message_search MATCH ?1 AND rowid = ?2", params![format!("\"{word}\""), id], |r| r.get(0)).unwrap();
            assert!(found, "message {id} isn't found by {word:?}");
        }
    }

    fn ids(store: &Store) -> Vec<(i64, i64)> {
        let mut st = store
            .conn()
            .prepare("SELECT seq, id FROM messages WHERE thread_id='t1' ORDER BY seq")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn a_rebuild_rewrites_only_what_changed_and_the_index_follows() {
        let store = store_with(30);
        in_step(&store);
        let before = ids(&store);
        // Nothing new: nothing written.
        store.rebuild_messages("t1").unwrap();
        assert_eq!(ids(&store), before);
        // A reply edited (boat rewrites a response), and a turn added: those rows only.
        let mut edited = turn(4, "reply 4: now with exponential backoff", None);
        edited.truncate(2);
        store.apply_events("t1", &edited[1..], false).unwrap();
        store
            .apply_events(
                "t1",
                &turn(30, "reply 30: a brand new turn", Some("make lint")),
                false,
            )
            .unwrap();
        store.rebuild_messages("t1").unwrap();
        in_step(&store);
        let after = ids(&store);
        let changed: Vec<i64> = before
            .iter()
            .zip(&after)
            .filter(|(a, b)| a != b)
            .map(|(a, _)| a.0)
            .collect();
        assert!(
            changed.is_empty(),
            "rows keep their ids as they're rewritten: {changed:?}"
        );
        assert_eq!(after.len(), before.len() + 3);
        let hits = store.search("exponential backoff", None).unwrap();
        assert_eq!(
            hits.iter().map(|h| (h.kind, h.seq)).collect::<Vec<_>>(),
            [(HitKind::Text, Some(seq_of(&store, "%exponential%")))]
        );
        assert!(
            store
                .search("reply 4: all good", None)
                .unwrap()
                .iter()
                .all(|h| h.seq != Some(14)),
            "the old text is gone"
        );
        assert_eq!(store.search("make lint", None).unwrap().len(), 1);
    }

    #[test]
    fn a_message_put_in_earlier_shifts_the_rest_and_the_index_follows() {
        let store = store_with(10);
        // An event boat reports late, timed before the rest: everything after moves down one.
        let late = RawEvent::from_value(
            serde_json::json!({"id": "late", "taskId": "task-0", "type": "response", "timestamp": 1_793_000_000_500i64, "conversationId": "c", "data": {"content": "a late remark about zebras", "model": "m"}}),
        );
        store.apply_events("t1", &[late], false).unwrap();
        store.rebuild_messages("t1").unwrap();
        in_step(&store);
        let hits = store.search("zebras", None).unwrap();
        assert_eq!(hits.iter().map(|h| h.seq).collect::<Vec<_>>(), [Some(1)]);
        // Every later message is found where it is now.
        for k in 1..10usize {
            let hits = store.search(&format!("prompt number {k}"), None).unwrap();
            let seqs: Vec<Option<i64>> = hits
                .iter()
                .filter(|h| h.kind == HitKind::Prompt)
                .map(|h| h.seq)
                .collect();
            assert_eq!(seqs, [Some(seq_of(&store, &format!("prompt number {k}")))]);
        }
        assert_eq!(seq_of(&store, "%zebras%"), 1);
    }

    /// Where the message with text like `like` is.
    fn seq_of(store: &Store, like: &str) -> i64 {
        store
            .conn()
            .query_row("SELECT seq FROM messages WHERE text LIKE ?1", [like], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn a_new_messages_version_and_an_import_keep_the_index_right() {
        let dir = std::env::temp_dir().join(format!("flotilla-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        {
            let store = Store::open(&db).unwrap();
            store
                .insert_thread(&super::tests_thread("t1", "Long thread"))
                .unwrap();
            let events: Vec<RawEvent> = (0..20)
                .flat_map(|k| turn(k, &format!("reply {k}: fine"), Some("cargo build")))
                .collect();
            store.apply_events("t1", &events, false).unwrap();
            store.rebuild_messages("t1").unwrap();
            // Holes in the ids, as deletes leave them, for VACUUM to close if it could.
            store
                .conn()
                .execute("DELETE FROM messages WHERE seq % 7 = 3", [])
                .unwrap();
            in_step(&store);
            // An older messages version: dropped and made again on open, index too.
            store.kv_set("messages_version", "5").unwrap();
        }
        let store = Store::open(&db).unwrap();
        in_step(&store);
        assert_eq!(
            store
                .search("reply 13: fine", None)
                .unwrap()
                .iter()
                .filter(|h| h.kind == HitKind::Text)
                .count(),
            1
        );
        // A database from before the word index: it's made from the messages on open.
        store.conn().execute_batch("DROP TRIGGER message_words_insert; DROP TRIGGER message_words_update; DROP TRIGGER message_words_delete; DROP TABLE message_words;").unwrap();
        drop(store);
        let store = Store::open(&db).unwrap();
        in_step(&store);
        store
            .conn()
            .execute("DELETE FROM messages WHERE seq % 5 = 1", [])
            .unwrap();
        drop(store);
        // `flotilla import` copies with VACUUM INTO: ids (and so the index) come through.
        let target = dir.join("imported").join("flotilla.db");
        Store::import(&db, &target, false).unwrap();
        let copy = Store::open(&target).unwrap();
        in_step(&copy);
        let title: i64 = copy
            .conn()
            .query_row(
                "SELECT count(*) FROM thread_search WHERE thread_search MATCH '\"long\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
pub(crate) fn tests_thread(id: &str, title: &str) -> flotilla_proto::Thread {
    flotilla_proto::Thread {
        id: id.into(),
        environment_id: None,
        environment_name: Some("base".into()),
        title: title.into(),
        sandbox_id: None,
        conversation_id: None,
        provider: "claude-code".into(),
        model: "haiku".into(),
        effort: None,
        fast: false,
        state: flotilla_proto::ThreadState::Idle,
        keep_running: false,
        archive_after: None,
        last_stopped_at: None,
        data_expired_at: None,
        created_at: "2026-10-31T09:00:00.000Z".into(),
        updated_at: "2026-10-31T09:00:00.000Z".into(),
        repo_project_id: None,
        branch: None,
        base_sha: None,
    }
}
