//! Search: thread titles and what was said in threads, by T3 Code's rules (its
//! `packages/shared/src/paletteSearch.ts`, ported rule for rule). The server
//! (`flotilla_core::search`), the Mac and Linux app's ⌘K and the iPhone's Threads search
//! (ios/FlotillaKit `Search`, line for line; `mobile/fixtures/conformance/search.json`, recorded
//! from these, keeps the two the same) all follow it.
//!
//! - **Folding** ([`fold`]): case, accents (a letter's marks), curly apostrophes; one character
//!   in, one out, so places in folded text are places in the text.
//! - **Word starts** ([`starts_word`]): after a non-letter, at a camelCase or digit boundary, at
//!   the last capital of an acronym (`JSONSchema`), at any character of a script written
//!   without spaces; not after an apostrophe inside a word (`Julius’s`).
//! - **Threads** ([`score_thread`]): every word must match the title, a pull request, the branch
//!   or the project. A whole word beats a word's start; in a title only, a word of four letters
//!   or more may match inside a word, lowest of all. Words typed together count (`vscode` for
//!   "VS Code"). With nothing at a word start anywhere, one typo is forgiven ([`rank`]). Use
//!   (when you last wrote, how often you open it) and picks (what you opened for this query)
//!   reorder matches of about the same quality, never across them.
//! - **Messages** ([`MessageMatcher`]): words of three characters or more are looked up, shorter
//!   ones only narrow ([`message_tokens`]). A word of four or more may match inside a word, at
//!   low quality; a shorter one must start a word (`fts` isn't in `drafts`). Hex ids aren't
//!   matched from inside (`cafe` isn't in `7cafe52bb8`). The words as typed, together, rank
//!   above them apart, and near each other above far apart. Each thread answers once, with its
//!   best message and how many matched as well ([`compare_messages`]).
//! - **Snippets** ([`snippet`]) are one line from a few words before the match, at most
//!   [`SNIPPET`] characters, with what matched marked ([`token_ranges`]).

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};

/// Hits a search answers with when it isn't told how many.
pub const LIMIT: u32 = 50;
/// The most a search answers with.
pub const MAX_LIMIT: u32 = 200;
/// Threads a palette lists for a query, at most.
pub const THREAD_ROWS: usize = 20;
/// Threads found by their messages a palette lists, at most.
pub const MESSAGE_ROWS: usize = 10;
/// A snippet's length in characters, ellipses aside.
pub const SNIPPET: usize = 238;
/// Characters of a snippet before the match, at most (then on to a word's start).
const LEAD: usize = 24;

/// What a hit is: a thread's title, or one of its messages (as `Message::kind` says it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitKind {
    Title,
    Prompt,
    Steer,
    /// A reply.
    Text,
    /// A tool call: its command, or its path, pattern or URL.
    Tool,
}

impl HitKind {
    /// A message's kind as a hit's; None for what isn't searched (notices).
    pub fn of_message(kind: &str) -> Option<HitKind> {
        match kind {
            "prompt" => Some(HitKind::Prompt),
            "steer" => Some(HitKind::Steer),
            "text" => Some(HitKind::Text),
            "tool" => Some(HitKind::Tool),
            _ => None,
        }
    }

    /// What you wrote (a prompt or a steer), not the agent.
    pub fn yours(self) -> bool {
        matches!(self, HitKind::Prompt | HitKind::Steer)
    }
}

/// One thing a search found: a thread by its title (or PR, branch, project), or a thread by
/// its messages, as its best one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub thread: String,
    /// The thread's title.
    pub title: String,
    pub kind: HitKind,
    /// The message's seq (none for a title): where its thread opens.
    #[serde(default)]
    pub seq: Option<i64>,
    /// A line around the match ([`snippet`]); for a title, the title.
    pub snippet: String,
    /// What matched in `snippet`: byte ranges, each on a character's boundary, in order and
    /// apart.
    #[serde(default)]
    pub marks: Vec<[u32; 2]>,
    /// When it was said (ms); for a title, when its thread was last active.
    #[serde(default)]
    pub at: Option<i64>,
    /// Who it's from when Flotilla brought it, not you: another thread's answer ("From <its
    /// title>"), the answers of agents started in the thread ("2 agents answered"), a task another
    /// thread handed over ("From <its title>"). As the thread shows it, in place of "You".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// How many of the thread's messages match as well as this one (a message hit).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hits: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

// ---- characters ----

/// A letter or a digit, of any script (`\p{L}\p{N}`).
pub fn alnum(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric();
    }
    matches!(
        c.general_category_group(),
        GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number
    )
}

fn lower(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_lowercase()
    } else {
        c.general_category() == GeneralCategory::LowercaseLetter
    }
}

fn upper(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_uppercase()
    } else {
        c.general_category() == GeneralCategory::UppercaseLetter
    }
}

fn digit(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_digit()
    } else {
        c.general_category_group() == GeneralCategoryGroup::Number
    }
}

fn letter(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic()
    } else {
        c.general_category_group() == GeneralCategoryGroup::Letter
    }
}

fn mark(c: char) -> bool {
    !c.is_ascii() && c.general_category_group() == GeneralCategoryGroup::Mark
}

/// Emoji and other symbols (`\p{So}`): they can be searched for, so they start words too.
fn symbol(c: char) -> bool {
    !c.is_ascii() && c.general_category() == GeneralCategory::OtherSymbol
}

fn apostrophe(c: char) -> bool {
    matches!(c, '\'' | '’')
}

/// A script written without spaces (Chinese, Japanese, Thai, Lao): each character starts a word.
fn unspaced(c: char) -> bool {
    matches!(c as u32, 0x0E00..=0x0EFF | 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFF66..=0xFF9F | 0x20000..=0x3FFFF)
}

/// Two Chinese, Japanese or Korean characters already make a common word.
fn cjk_word(c: char) -> bool {
    !matches!(c as u32, 0x0E00..=0x0EFF) && unspaced(c) || matches!(c as u32, 0xAC00..=0xD7AF)
}

/// A character as search compares it: lower case, without its accent (its decomposition's
/// marks, a stroke: `ł` is `l`), `ς` as `σ`, curly apostrophes straight (iPhone keyboards type
/// `’`). One character in,
/// one out: one that folds to more or fewer (`ß`, `ﬁ`) stays as it is, lower-cased.
pub fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut folded = c
        .nfkd()
        .filter(|m| !mark(*m))
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'ς' => 'σ',
            '‘' | '’' => '\'',
            c => unstroked(c),
        });
    match (folded.next(), folded.next()) {
        (Some(one), None) => one,
        _ => {
            let mut low = c.to_lowercase();
            match (low.next(), low.next()) {
                (Some(one), None) => one,
                _ => c,
            }
        }
    }
}

/// Letters with a stroke, lower case, and the letter each is found by: they have no
/// decomposition to lose it by (Flotilla's: T3 keeps them). The server's message index maps
/// them too, in either case, as SQLite's own folding doesn't.
pub const STROKES: [(char, char); 6] = [
    ('đ', 'd'),
    ('ħ', 'h'),
    ('ı', 'i'),
    ('ł', 'l'),
    ('ø', 'o'),
    ('ŧ', 't'),
];

/// A lower-case letter without its stroke ([`STROKES`]); any other character as it is.
pub fn unstroked(c: char) -> char {
    STROKES
        .iter()
        .find(|(s, _)| *s == c)
        .map_or(c, |(_, to)| *to)
}

fn fold_all(text: &[char]) -> Vec<char> {
    text.iter().map(|&c| fold(c)).collect()
}

/// Whether a word starts at `c`, after `prev` and `before` (none: the text's start), with `next`
/// after it: after a non-letter, at a camelCase or digit boundary, at the last capital of an
/// acronym (`JSONSchema`), or at or after a character of a script without spaces. A possessive
/// or contraction (`Julius’s`) doesn't start one. The one rule for titles and messages.
pub fn starts_word(before: Option<char>, prev: Option<char>, c: char, next: Option<char>) -> bool {
    let after_apostrophe = prev.is_some_and(apostrophe) && before.is_some_and(alnum);
    (alnum(c) || symbol(c))
        && !after_apostrophe
        && match prev {
            None => true,
            Some(p) => {
                !alnum(p)
                    || (lower(p) && upper(c))
                    || (upper(p) && upper(c) && next.is_some_and(lower))
                    || digit(p) != digit(c)
                    || unspaced(c)
                    || unspaced(p)
            }
        }
}

/// Text as titles are matched: folded, and where its words start.
struct Field {
    text: Vec<char>,
    starts: Vec<bool>,
}

impl Field {
    fn of(source: &str) -> Field {
        let src: Vec<char> = source.chars().collect();
        Field {
            starts: word_starts(&src),
            text: fold_all(&src),
        }
    }

    fn word_end(&self, at: usize) -> usize {
        let mut end = at + 1;
        while end < self.text.len() && !self.starts[end] && alnum(self.text[end]) {
            end += 1;
        }
        end
    }
}

fn word_starts(src: &[char]) -> Vec<bool> {
    let at = |i: Option<usize>| i.and_then(|i| src.get(i)).copied();
    (0..src.len())
        .map(|i| {
            starts_word(
                at(i.checked_sub(2)),
                at(i.checked_sub(1)),
                src[i],
                at(Some(i + 1)),
            )
        })
        .collect()
}

/// Where `needle` is in `hay` at or after `from`.
fn index_of(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    if needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len())
        .find(|&i| hay[i] == first && hay[i..i + needle.len()] == *needle)
}

/// A pasted query's quotes ("fix sidebar"), and the spaces around them.
const QUOTES: &[char] = &['"', '\'', '`', '“', '”', '‘', '’'];

fn unquoted(query: &str) -> &str {
    query.trim_matches(separates).trim_matches(QUOTES)
}

/// What separates a query's words: whitespace, and control characters (a NUL), which nothing
/// has to find.
pub fn separates(c: char) -> bool {
    c.is_whitespace() || c.is_control()
}

/// A query as titles are matched against it: folded, its words one space apart; empty when
/// there's nothing to match.
pub fn fold_query(query: &str) -> String {
    let composed: String = unquoted(query).nfc().collect();
    let folded: String = composed.chars().map(fold).collect();
    folded
        .split(separates)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---- titles ----

/// A match of a whole word.
const WHOLE_WORD: f64 = 1.;
/// A title that is the whole query lifts a whole word to this.
const WHOLE_NAME: f64 = 1.5;
/// Inside a word.
pub const MID_WORD: f64 = 0.3;
const TYPO: f64 = 0.45;
/// Whole words typed together are exact: with typos, above mid-word hits, below a word start.
const JOINED_WORDS: f64 = 0.6;

/// A token's match in a field: how good, where, and for words typed together, each word's part.
#[derive(Debug, Clone)]
struct TokenMatch {
    score: f64,
    start: usize,
    end: usize,
    letters: Vec<(usize, usize)>,
}

impl TokenMatch {
    fn at(score: f64, start: usize, end: usize) -> TokenMatch {
        TokenMatch {
            score,
            start,
            end,
            letters: vec![],
        }
    }
}

/// One substitution, insertion, deletion or swap of neighbours; with `swap_only`, only a swap.
fn within_one_edit(left: &[char], right: &[char], swap_only: bool) -> bool {
    if left.len().abs_diff(right.len()) > if swap_only { 0 } else { 1 } {
        return false;
    }
    let mut i = 0;
    while i < left.len() && i < right.len() && left[i] == right[i] {
        i += 1;
    }
    let (l, r) = (&left[i..], &right[i..]);
    let from = |s: &'_ [char], k: usize| -> Vec<char> { s.get(k..).unwrap_or(&[]).to_vec() };
    let swapped = l.len() > 1
        && Some(&l[0]) == r.get(1)
        && Some(&l[1]) == r.first()
        && from(l, 2) == from(r, 2);
    if swap_only {
        return swapped;
    }
    swapped || l == r || from(l, 1) == from(r, 1) || from(l, 1) == r || l == from(r, 1).as_slice()
}

/// `token` in `field`: a whole word, else a word's start (the more of the word, the better), else
/// (with `mid_word`, four characters or more) inside a word. In the title (`primary`), whole
/// words typed together. With `typos` and nothing better, one edit of a word's start, keeping
/// its first letter (unless the first two are swapped in a longer word); short tokens forgive
/// only a swap or a missing letter, and numbers never (pull requests must stay apart).
fn match_token(
    field: &Field,
    token: &[char],
    typos: bool,
    primary: bool,
    mid_word: bool,
) -> Option<TokenMatch> {
    let first = *token.first()?;
    let mut best: Option<TokenMatch> = None;
    // A token that opens with punctuation (".ts") brings its own boundary.
    let own_boundary = !alnum(first) && !symbol(first);
    let len = field.text.len();
    let mut found = index_of(&field.text, token, 0);
    while let Some(i) = found {
        if field.starts[i] || own_boundary {
            // Whole when it ends where a word ends: "t3c" is a start of "t3code"; ".ts" is whole
            // in "index.ts" but a start in "parser.tsx".
            let end = i + token.len();
            let whole = end >= len || field.starts[end] || !alnum(field.text[end]);
            let mut run_end = end;
            while run_end < len && alnum(field.text[run_end]) {
                run_end += 1;
            }
            let score = if whole {
                WHOLE_WORD
            } else {
                0.7 + 0.2 * token.len() as f64 / (run_end - i) as f64
            };
            if best.as_ref().is_none_or(|b| score > b.score) {
                best = Some(TokenMatch::at(score, i, end));
            }
            if score == WHOLE_WORD {
                return best;
            }
        } else if mid_word && token.len() >= 4 && best.is_none() {
            best = Some(TokenMatch::at(MID_WORD, i, i + token.len()));
        }
        found = index_of(&field.text, token, i + 1);
    }
    // Words typed together are exact, so they count without typos: a strong match elsewhere
    // mustn't hide "VS Code" for "vscode".
    if best.as_ref().is_none_or(|b| b.score <= MID_WORD)
        && primary
        && let Some(joined) = joined_words(field, token)
    {
        return Some(joined);
    }
    if best.as_ref().is_some_and(|b| b.score > MID_WORD) || !typos || token.len() < 3 {
        return best;
    }
    // Numbers skip typos to keep pull requests apart; words with digits ("t3code") don't.
    if token.iter().any(|&c| letter(c)) {
        for i in 0..len {
            if !field.starts[i] {
                continue;
            }
            // People rarely mistype the first letter, so a typo keeps it, unless it swaps the
            // first two of a longer word ("ing" isn't "nig…").
            let f = field.text[i];
            let kept = f == token[0]
                || (token.len() > 3 && f == token[1] && field.text.get(i + 1) == Some(&token[0]));
            if !kept {
                continue;
            }
            // The whole run, across camelCase joins: "githbu" is "GitHub".
            let mut run_end = i + 1;
            while run_end < len && alnum(field.text[run_end]) {
                run_end += 1;
            }
            let word = &field.text[i..run_end];
            // Short tokens forgive a swapped pair or one missing letter ("srv" for "serv…"); any
            // other edit of a three- or four-letter word matches too much ("cron", "con…").
            let short = token.len() <= 4;
            // The same length first, so "githbu" marks all of "GitHub".
            let lengths: &[usize] = if short {
                &[token.len(), token.len() + 1]
            } else {
                &[token.len(), token.len() - 1, token.len() + 1]
            };
            for &n in lengths {
                if n > word.len() {
                    continue;
                }
                if within_one_edit(token, &word[..n], short && n == token.len()) {
                    return Some(TokenMatch::at(TYPO, i, i + n));
                }
            }
        }
    }
    best
}

/// Whole words typed together ("vscode" for "VS Code", "fixsid" for "Fix sidebar"), each run a
/// word or (the last) a word's start. Short words before a short start are noise inside a title
/// ("inth" in "in thread"): allowed at its first word ("t3co"), with three letters of the last
/// ("gomod"), all of it ("vscode"), or every word before three letters or more.
fn joined_words(field: &Field, token: &[char]) -> Option<TokenMatch> {
    let len = field.text.len();
    let first_word = field.starts.iter().position(|&s| s);
    for start in 0..len {
        if !field.starts[start] || field.text[start] != token[0] {
            continue;
        }
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let (mut at, mut rest) = (start, token);
        while !rest.is_empty() && at < len {
            let end = field.word_end(at);
            if rest.len() <= end - at && field.text[at..].starts_with(rest) {
                runs.push((at, at + rest.len()));
                rest = &[];
            } else if rest.starts_with(&field.text[at..end]) {
                runs.push((at, end));
                rest = &rest[end - at..];
                at = end;
                while at < len && !field.starts[at] {
                    at += 1;
                }
            } else {
                break;
            }
        }
        let Some(&last) = runs.last() else { continue };
        if !rest.is_empty() || runs.len() < 2 {
            continue;
        }
        if last.1 == field.word_end(last.0)
            || last.1 - last.0 >= 3
            || Some(start) == first_word
            || runs[..runs.len() - 1].iter().all(|r| r.1 - r.0 >= 3)
        {
            return Some(TokenMatch {
                score: JOINED_WORDS,
                start,
                end: last.1,
                letters: runs,
            });
        }
    }
    None
}

/// A field a query is matched against, and how much a match there counts.
#[derive(Debug, Clone, Default)]
struct Weighted<'a> {
    value: &'a str,
    weight: f64,
    /// Only a whole word counts (a short query against keywords).
    whole_words: bool,
    /// Only a word's start counts, without typos: hidden keywords that matched loosely would list
    /// rows you can't see why for.
    keyword: bool,
    /// Where the thread lives (its project): it helps a match, but a word start there doesn't
    /// make it strong, so it never turns off joined words or typos for the title.
    context: bool,
    /// A token of four characters or more may match inside a word (thread titles only: inside a
    /// command or a branch it reads as junk, "arch" in "Search").
    mid_word: bool,
}

/// How a query matched some fields.
#[derive(Debug, Clone)]
struct FieldsMatch {
    /// Roughly 0.3 (inside a word) to 1.5 (the whole title).
    quality: f64,
    /// Every token matched at a word start, without a typo.
    strong: bool,
    /// What matched in the first field (character ranges), to mark it.
    ranges: Vec<(usize, usize)>,
}

/// Words a phrase-style query ("fix the sidebar") carries that a name may leave out.
const FILLER: &[&str] = &[
    "a", "an", "and", "for", "in", "of", "on", "the", "to", "with",
];

/// Sorted, overlapping ranges joined.
fn merged(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_by_key(|r| r.0);
    let mut out: Vec<(usize, usize)> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.0 <= last.1 => last.1 = last.1.max(r.1),
            _ => out.push(r),
        }
    }
    out
}

/// Every token of `query` (folded) must match some field; filler words only where they match
/// unless the query is all filler. Quality is the mean of each token's best weighted match, plus
/// a bonus when the first field is the whole query or holds it as a phrase.
fn match_fields(query: &str, fields: &[Weighted], typos: bool) -> Option<FieldsMatch> {
    let tokens: Vec<Vec<char>> = query
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().collect())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let primary_set = fields.first().is_some_and(|f| !f.value.is_empty());
    let prepared: Vec<(Field, &Weighted)> = fields
        .iter()
        .filter(|f| !f.value.is_empty())
        .map(|f| (Field::of(f.value), f))
        .collect();
    let is_filler = |t: &[char]| FILLER.iter().any(|f| f.chars().eq(t.iter().copied()));
    let filler_optional = tokens.iter().any(|t| !is_filler(t));
    let (mut ranges, mut total, mut counted, mut strong) = (Vec::new(), 0., 0usize, true);
    for token in &tokens {
        let (mut best, mut best_primary, mut at_word_start): (f64, Option<TokenMatch>, bool) =
            (0., None, false);
        for (k, (field, w)) in prepared.iter().enumerate() {
            let primary = primary_set && k == 0;
            if best >= w.weight && !primary && at_word_start {
                continue;
            }
            let Some(m) = match_token(field, token, typos && !w.keyword, primary, w.mid_word)
            else {
                continue;
            };
            if m.score
                < if w.whole_words {
                    WHOLE_WORD
                } else if w.keyword {
                    0.7
                } else {
                    0.
                }
            {
                continue;
            }
            if m.score >= 0.7 && !w.context {
                at_word_start = true;
            }
            if m.score * w.weight > best {
                best = m.score * w.weight;
            }
            if primary {
                best_primary = Some(m);
            }
        }
        if best == 0. {
            if filler_optional && is_filler(token) {
                continue;
            }
            return None;
        }
        strong &= at_word_start;
        if let Some(m) = best_primary {
            if m.letters.is_empty() {
                ranges.push((m.start, m.end))
            } else {
                ranges.extend(m.letters)
            }
        }
        total += best;
        counted += 1;
    }
    let mut quality = total / counted as f64;
    if let Some((primary, _)) = prepared.first().filter(|_| primary_set) {
        let q: Vec<char> = query.chars().collect();
        if primary.text == q {
            quality += WHOLE_NAME - WHOLE_WORD;
        } else if tokens.len() > 1 && index_of(&primary.text, &q, 0).is_some() {
            quality += 0.25;
        }
    }
    Some(FieldsMatch {
        quality,
        strong,
        ranges: merged(ranges),
    })
}

// ---- use and picks ----

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Recent use and opens both halve every two weeks.
pub const HALF_LIFE_MS: i64 = 14 * DAY_MS;
/// Opens closer together than this count once, so flipping between threads isn't frequency.
pub const OPEN_DEDUPE_MS: i64 = 10 * 60 * 1000;
/// Picks kept per thread.
const PICKS: usize = 3;
/// A picked query is kept this long (in UTF-16 units, as T3 has it).
const PICK_LENGTH: usize = 32;
/// Picks count this long after they're made; opening the thread another way doesn't renew them.
const PICK_WINDOW_MS: i64 = 21 * DAY_MS;
/// A query's start counts for a pick from three letters: "n" is too little to go on.
const PICK_PREFIX: usize = 3;

/// How often and how recently a thread was opened, and what it was last opened for from a
/// search: kept by the server ([`record_open`]), so every device ranks by the same.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Opens {
    /// The last open that counted (ms).
    pub at: i64,
    /// Opens, each halving every [`HALF_LIFE_MS`] (as of `at`).
    pub score: f64,
    /// The latest queries it was picked for, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub picks: Vec<SearchPick>,
}

// A score is never NaN.
impl Eq for Opens {}

/// A query a thread was opened for from a search, as [`picked_query`] has it, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPick {
    pub query: String,
    pub at: i64,
}

/// `query` as a pick is kept and compared: folded, then cut to [`PICK_LENGTH`] without splitting
/// a character.
pub fn picked_query(query: &str) -> String {
    let folded = fold_query(query);
    let (mut out, mut units) = (String::new(), 0);
    for c in folded.chars() {
        units += c.len_utf16();
        if units > PICK_LENGTH {
            break;
        }
        out.push(c);
    }
    out.trim().to_string()
}

fn decay(age_ms: i64) -> f64 {
    2f64.powf(-(age_ms.max(0) as f64) / HALF_LIFE_MS as f64)
}

/// The opens after one at `at`; `query`, the search it was picked from, if any.
pub fn record_open(previous: Option<&Opens>, at: i64, query: Option<&str>) -> Opens {
    let pick = picked_query(query.unwrap_or(""));
    // Picks that no longer count aren't kept.
    let earlier: Vec<SearchPick> = previous
        .map(|p| {
            p.picks
                .iter()
                .filter(|e| at - e.at <= PICK_WINDOW_MS)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let picks = if pick.is_empty() {
        earlier
    } else {
        let mut picks = vec![SearchPick {
            query: pick.clone(),
            at,
        }];
        picks.extend(earlier.into_iter().filter(|e| e.query != pick));
        picks.truncate(PICKS);
        picks
    };
    match previous {
        Some(p) if at - p.at < OPEN_DEDUPE_MS => Opens { picks, ..p.clone() },
        Some(p) => Opens {
            at,
            score: p.score * decay(at - p.at) + 1.,
            picks,
        },
        None => Opens {
            at,
            score: 1.,
            picks,
        },
    }
}

/// The latest pick of the thread for `query` (folded), while it counts: an exact one, else the
/// latest the query starts (from three letters).
fn active_pick<'a>(opens: Option<&'a Opens>, query: &str, now: i64) -> Option<&'a SearchPick> {
    let query = picked_query(query);
    let prefix = query.chars().count() >= PICK_PREFIX;
    let (mut exact, mut latest): (Option<&SearchPick>, Option<&SearchPick>) = (None, None);
    for pick in opens.map(|o| o.picks.as_slice()).unwrap_or(&[]) {
        if now - pick.at > PICK_WINDOW_MS {
            continue;
        }
        if pick.query == query && exact.is_none_or(|e| pick.at > e.at) {
            exact = Some(pick);
        }
        if (pick.query == query || (prefix && pick.query.starts_with(&query)))
            && latest.is_none_or(|l| pick.at > l.at)
        {
            latest = Some(pick);
        }
    }
    exact.or(latest)
}

/// Use reorders matches of about the same quality, but never makes a mid-word hit on a busy
/// thread better than a whole word on a quiet one.
const USE_LIMIT: f64 = 2.5;

/// How much use lifts a thread: 1 for a stale one never opened, about 2 for one written in
/// today, up to 2.5 for one opened often. `activity`: when you last wrote in it (ms).
pub fn usage_boost(activity: Option<i64>, opens: Option<&Opens>, now: i64) -> f64 {
    let recency = activity.map_or(0., |at| decay(now - at));
    let opened = opens.map_or(0., |o| o.score * decay(now - o.at));
    (1. + recency + 0.5 * (1. + opened).log2()).min(USE_LIMIT)
}

// ---- threads ----

/// A thread as search sees it.
#[derive(Debug, Clone, Default)]
pub struct Subject<'a> {
    /// Matched by the whole query only, as references are.
    pub id: Option<&'a str>,
    pub title: &'a str,
    pub branch: Option<&'a str>,
    /// Its project (`owner/repo`).
    pub project: Option<&'a str>,
    pub prs: Vec<PrRef<'a>>,
    /// When you last wrote in it, else when it began (ms).
    pub activity: Option<i64>,
    pub opens: Option<&'a Opens>,
}

/// A thread's pull request as search sees it.
#[derive(Debug, Clone, Default)]
pub struct PrRef<'a> {
    pub number: i64,
    pub title: Option<&'a str>,
    /// Values such as its URL, matched by the whole query only.
    pub references: Vec<String>,
}

/// Why a thread matched when its title shows nothing: the field, and what matched in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    /// "PR", "Branch", "Project", or "ID or link".
    pub label: &'static str,
    pub text: String,
    /// Character ranges of `text`.
    pub ranges: Vec<(usize, usize)>,
}

/// How a thread matched a query.
#[derive(Debug, Clone)]
pub struct ThreadMatch {
    /// Quality times use: what results sort by.
    pub score: f64,
    pub quality: f64,
    /// Every token at a word start.
    pub strong: bool,
    /// What matched in the title (character ranges).
    pub ranges: Vec<(usize, usize)>,
    /// Why it matched when the title shows nothing.
    pub hint: Option<Hint>,
    /// When it was picked for this query, while that counts (ms; 0: never).
    pub picked_at: i64,
    /// Picked for exactly this query.
    pub picked_exactly: bool,
    /// Below the whole-name class: every new thread is titled "New thread" for a while.
    pub group_quality: f64,
}

/// Branches Flotilla names itself start with this: "flotilla connect" mustn't match nearly
/// every thread.
const BRANCH_PREFIX: &str = "flotilla/";

/// `branch` without Flotilla's own prefix (only before more text, so typing "flotilla/" doesn't
/// empty the list for a keystroke).
fn branch_shown(branch: &str) -> &str {
    match branch.strip_prefix(BRANCH_PREFIX) {
        Some(rest) if rest.starts_with(|c: char| !c.is_whitespace()) => rest,
        _ => branch,
    }
}

/// The query without a leading `pr`, `pull request`, `pull` (before a number), `#`, or
/// Flotilla's branch prefix.
fn field_query(query: &str) -> String {
    let mut q = query;
    for prefix in ["pull request", "pull", "pr"] {
        if let Some(rest) = q.strip_prefix(prefix) {
            let rest = rest.trim_start();
            if rest
                .strip_prefix('#')
                .unwrap_or(rest)
                .starts_with(|c: char| c.is_ascii_digit())
            {
                q = rest;
                break;
            }
        }
    }
    let q = q.strip_prefix('#').map_or(q, str::trim_start);
    branch_shown(q).to_string()
}

/// A reference (an id, a link) to search the whole query in.
const REFERENCE_MIN: usize = 8;

/// The query in an id or link: a long start of one, or (holding a digit) anywhere, its number
/// never cut short (".../pull/2288" isn't PR 22889). Ids and links are ASCII.
fn match_reference(query: &str, subject: &Subject) -> bool {
    if query.chars().count() < REFERENCE_MIN {
        return false;
    }
    let anywhere = query.chars().any(|c| c.is_ascii_digit());
    let not_cut = |text: &str, end: usize| !text[end..].starts_with(|c: char| c.is_ascii_digit());
    let refs = subject.id.into_iter().map(str::to_string).chain(
        subject
            .prs
            .iter()
            .flat_map(|p| p.references.iter().cloned()),
    );
    refs.into_iter().any(|r| {
        let r = r.to_lowercase();
        match r.find(query) {
            // A pasted link may go on past the reference: ".../pull/22889/files".
            _ if anywhere && query.starts_with(&r) && not_cut(query, r.len()) => true,
            Some(at) if anywhere => not_cut(&r, at + query.len()),
            Some(0) => true,
            _ => false,
        }
    })
}

/// The field that explains a match the title doesn't show: the first that holds the whole
/// query, else, past the words the project holds, the first that holds the rest.
fn hint(query: &str, subject: &Subject, typos: bool) -> Option<Hint> {
    let mut fields: Vec<(&'static str, String)> = subject
        .prs
        .iter()
        .map(|p| {
            (
                "PR",
                format!(
                    "#{}{}",
                    p.number,
                    p.title.map(|t| format!(" {t}")).unwrap_or_default()
                ),
            )
        })
        .collect();
    if let Some(b) = subject.branch {
        fields.push(("Branch", branch_shown(b).to_string()));
    }
    if let Some(p) = subject.project {
        fields.push(("Project", p.to_string()));
    }
    let holds = |text: &str, q: &str| {
        match_fields(
            q,
            &[Weighted {
                value: text,
                weight: 1.,
                ..Default::default()
            }],
            typos,
        )
        .map(|m| m.ranges)
    };
    let hint_for = |q: &str| {
        fields.iter().find_map(|(label, text)| {
            holds(text, q).map(|ranges| Hint {
                label,
                text: text.clone(),
                ranges,
            })
        })
    };
    if let Some(h) = hint_for(query) {
        return Some(h);
    }
    let rest: Vec<&str> = query
        .split(' ')
        .filter(|t| !subject.project.is_some_and(|p| holds(p, t).is_some()))
        .collect();
    let rest = rest.join(" ");
    if !rest.is_empty() && rest != query {
        hint_for(&rest)
    } else {
        None
    }
}

/// How `subject` matches `query` (folded, [`fold_query`]), or None. Titles and pull request
/// numbers count fully, pull request titles and branches less, the project least. A row whose
/// title shows nothing says why ([`Hint`]); one whose words only meet across unrelated fields is
/// left out. `picks`: whether picks count (off where one would show unexplained).
pub fn score_thread(
    query: &str,
    subject: &Subject,
    now: i64,
    typos: bool,
    picks: bool,
) -> Option<ThreadMatch> {
    let pr_numbers: Vec<String> = subject
        .prs
        .iter()
        .map(|p| format!("#{}", p.number))
        .collect();
    let mut fields = vec![Weighted {
        value: subject.title,
        weight: 1.,
        mid_word: true,
        ..Default::default()
    }];
    for (p, number) in subject.prs.iter().zip(&pr_numbers) {
        fields.push(Weighted {
            value: number,
            weight: 1.,
            ..Default::default()
        });
        if let Some(t) = p.title {
            fields.push(Weighted {
                value: t,
                weight: 0.5,
                ..Default::default()
            });
        }
    }
    if let Some(b) = subject.branch {
        fields.push(Weighted {
            value: branch_shown(b),
            weight: 0.6,
            ..Default::default()
        });
    }
    if let Some(p) = subject.project {
        fields.push(Weighted {
            value: p,
            weight: 0.25,
            context: true,
            ..Default::default()
        });
    }
    let fq = field_query(query);
    let by_fields = match_fields(&fq, &fields, typos);
    let by_reference = by_fields.is_none() && match_reference(query, subject);
    let m = match by_fields {
        Some(m) => m,
        None if by_reference => FieldsMatch {
            quality: 0.6,
            strong: true,
            ranges: vec![],
        },
        None => return None,
    };
    let hint = if !m.ranges.is_empty() {
        None
    } else if by_reference {
        Some(Hint {
            label: "ID or link",
            text: query.to_string(),
            ranges: vec![],
        })
    } else {
        let h = hint(&fq, subject, typos);
        // Several words, none in a field that explains them: a coincidence.
        if h.is_none() && fq.contains(' ') {
            return None;
        }
        h
    };
    let pick = if picks {
        active_pick(subject.opens, query, now)
    } else {
        None
    };
    Some(ThreadMatch {
        score: m.quality * usage_boost(subject.activity, subject.opens, now),
        quality: m.quality,
        group_quality: m.quality.min(WHOLE_NAME - 0.01),
        strong: m.strong,
        ranges: m.ranges,
        hint,
        picked_at: pick.map_or(0, |p| p.at),
        picked_exactly: pick.is_some_and(|p| p.query == picked_query(query)),
    })
}

/// What ranking reads of a match.
pub trait Ranked {
    fn score(&self) -> f64;
    fn quality(&self) -> f64;
    fn strong(&self) -> bool;
    fn picked_at(&self) -> i64 {
        0
    }
    fn picked_exactly(&self) -> bool {
        false
    }
    fn group_quality(&self) -> f64 {
        self.quality()
    }
}

impl Ranked for ThreadMatch {
    fn score(&self) -> f64 {
        self.score
    }
    fn quality(&self) -> f64 {
        self.quality
    }
    fn strong(&self) -> bool {
        self.strong
    }
    fn picked_at(&self) -> i64 {
        self.picked_at
    }
    fn picked_exactly(&self) -> bool {
        self.picked_exactly
    }
    fn group_quality(&self) -> f64 {
        self.group_quality
    }
}

/// Whole words and starts share a class, so "Compact" doesn't lift a command over "Compaction".
fn match_class(quality: f64) -> u8 {
    if quality >= WHOLE_NAME {
        3
    } else if quality >= 0.7 {
        2
    } else if quality >= 0.4 {
        1
    } else {
        0
    }
}

/// How two matches in a group order: by word match (whole or a start, above typos and the
/// inside of words), then an exact pick, then class, then any pick, then score. Picks reorder
/// word matches but never lift a typo or a mid-word hit over them, and a pick for a longer query
/// never passes a whole name for this one.
pub fn compare<M: Ranked>(l: &M, r: &M) -> Ordering {
    let word = |m: &M| match_class(m.quality()).min(2);
    let exact = |m: &M| if m.picked_exactly() { m.picked_at() } else { 0 };
    word(r)
        .cmp(&word(l))
        .then(exact(r).cmp(&exact(l)))
        .then(match_class(r.quality()).cmp(&match_class(l.quality())))
        .then(r.picked_at().cmp(&l.picked_at()))
        .then(r.score().partial_cmp(&l.score()).unwrap_or(Ordering::Equal))
}

/// A group's class, by its best match: groups order by it, so use can't lift a group of
/// project-only matches over title matches, nor across classes.
pub fn group_class<'a, M: Ranked + 'a>(matches: impl IntoIterator<Item = &'a M>) -> u8 {
    matches
        .into_iter()
        .map(|m| match_class(m.group_quality()))
        .max()
        .unwrap_or(0)
}

/// Groups of items ranked: each group's items by [`compare`], the groups by [`group_class`],
/// keeping their order on ties. Typos are tried only when nothing matches at a word start
/// anywhere (they're noise beside real matches).
pub fn rank_groups<G, T, M: Ranked>(
    groups: Vec<(G, Vec<T>)>,
    score: impl Fn(&G, &T, bool) -> Option<M>,
) -> Vec<(G, Vec<(T, M)>)> {
    let pass = |typos: bool| -> Vec<Vec<Option<M>>> {
        groups
            .iter()
            .map(|(g, items)| items.iter().map(|t| score(g, t, typos)).collect())
            .collect()
    };
    let mut found = pass(false);
    if !found.iter().flatten().flatten().any(|m| m.strong()) {
        found = pass(true);
    }
    // Each group's class and place, for ordering them.
    type Placed<G, T, M> = (u8, usize, G, Vec<(T, M)>);
    let mut out: Vec<Placed<G, T, M>> = Vec::new();
    for (order, ((g, items), matches)) in groups.into_iter().zip(found).enumerate() {
        let mut ranked: Vec<(T, M)> = items
            .into_iter()
            .zip(matches)
            .filter_map(|(t, m)| m.map(|m| (t, m)))
            .collect();
        if ranked.is_empty() {
            continue;
        }
        ranked.sort_by(|(_, l), (_, r)| compare(l, r));
        out.push((group_class(ranked.iter().map(|(_, m)| m)), order, g, ranked));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    out.into_iter().map(|(_, _, g, r)| (g, r)).collect()
}

/// [`rank_groups`] for a single list.
pub fn rank<T, M: Ranked>(items: Vec<T>, score: impl Fn(&T, bool) -> Option<M>) -> Vec<(T, M)> {
    rank_groups(vec![((), items)], |_, t, typos| score(t, typos))
        .pop()
        .map(|(_, r)| r)
        .unwrap_or_default()
}

/// How a command (its name, then its keywords) matched.
#[derive(Debug, Clone)]
pub struct TermsMatch {
    pub quality: f64,
    /// Lower than `quality` when only keywords matched: a match in hidden keywords mustn't put
    /// its group above one whose name matches.
    pub group_quality: f64,
    pub strong: bool,
    /// What matched in the name (character ranges).
    pub ranges: Vec<(usize, usize)>,
}

impl Ranked for TermsMatch {
    fn score(&self) -> f64 {
        self.quality
    }
    fn quality(&self) -> f64 {
        self.quality
    }
    fn strong(&self) -> bool {
        self.strong
    }
    fn group_quality(&self) -> f64 {
        self.group_quality
    }
}

const KEYWORD_QUALITY: f64 = 0.65;
/// Below this, keyword starts match too much ("co", "pro"); a short query still matches a
/// keyword it equals ("pr").
const KEYWORD_QUERY_MIN: usize = 4;

/// `query` (folded) against a name and its keywords. Keywords count less, so they can't lift a
/// group above a matching name; a short query needs whole keywords.
pub fn score_terms(query: &str, name: &str, keywords: &[&str], typos: bool) -> Option<TermsMatch> {
    // Filler words ("the ci") don't make a short query long.
    let meaningful: Vec<&str> = query.split(' ').filter(|t| !FILLER.contains(t)).collect();
    let meaningful = meaningful.join(" ");
    let short = if meaningful.is_empty() {
        query
    } else {
        &meaningful
    }
    .chars()
    .count()
        < KEYWORD_QUERY_MIN;
    let mut fields = vec![Weighted {
        value: name,
        weight: 1.,
        ..Default::default()
    }];
    // An alias typed in full ("pr") is deliberate, above a mere start of the name.
    fields.extend(keywords.iter().map(|k| Weighted {
        value: k,
        weight: if short { 0.8 } else { 0.6 },
        whole_words: short,
        keyword: true,
        ..Default::default()
    }));
    let m = match_fields(query, &fields, typos)?;
    Some(TermsMatch {
        quality: m.quality,
        group_quality: if m.ranges.is_empty() {
            m.quality.min(KEYWORD_QUALITY)
        } else {
            m.quality
        },
        strong: m.strong,
        ranges: m.ranges,
    })
}

/// Character ranges as byte ranges of `text`, for marks.
pub fn byte_ranges(text: &str, ranges: &[(usize, usize)]) -> Vec<[u32; 2]> {
    let mut bytes: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    bytes.push(text.len());
    ranges
        .iter()
        .filter_map(|&(a, b)| Some([*bytes.get(a)? as u32, *bytes.get(b)? as u32]))
        .filter(|[a, b]| a < b)
        .collect()
}

// ---- messages ----

/// A two-letter substring of a message is noise rather than a match: words this long (in
/// characters) are looked up in the index (it holds every run of three).
pub const INDEXED: usize = 3;
const MESSAGE_TOKEN_MIN: usize = INDEXED;
/// A message token shorter than this must start a word: "fts" isn't in "drafts".
const WORD_START_MIN: usize = 4;
/// A word's start, not all of it ("mode" in "model").
pub const PREFIX: f64 = 0.8;
/// The words written together, as typed.
const PHRASE_BONUS: f64 = 0.25;
/// The words near each other: within about what a row shows.
const NEAR_BONUS: f64 = 0.1;
const NEAR_SPAN: usize = 60;

/// A query's words for messages: lower-cased, apostrophes straight (as the index stores them),
/// accents kept (the index folds them). `long` ones (three characters or more, or two Chinese,
/// Japanese or Korean ones) are looked up and must all match; `short` ones only narrow. No long
/// ones: no message search.
#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
    pub long: Vec<String>,
    pub short: Vec<String>,
    /// Every word, in the order typed.
    pub words: Vec<String>,
}

pub fn message_tokens(query: &str) -> Tokens {
    let q: String = unquoted(query)
        .replace(['‘', '’'], "'")
        .nfc()
        .collect::<String>()
        .to_lowercase();
    let words: Vec<String> = q
        .split(separates)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let searchable = |t: &String| {
        let n = t.chars().count();
        n >= MESSAGE_TOKEN_MIN || (n == 2 && t.chars().any(cjk_word))
    };
    Tokens {
        long: words.iter().filter(|t| searchable(t)).cloned().collect(),
        short: words.iter().filter(|t| !searchable(t)).cloned().collect(),
        words,
    }
}

/// Whether a query searches messages, not only titles.
pub fn finds_messages(query: &str) -> bool {
    !message_tokens(query).long.is_empty()
}

fn starts_word_at(text: &[char], i: usize) -> bool {
    let c = text[i];
    // A token that opens with punctuation (".ts") brings its own boundary.
    if !alnum(c) && !symbol(c) {
        return true;
    }
    let at = |k: Option<usize>| k.and_then(|k| text.get(k)).copied();
    starts_word(
        at(i.checked_sub(2)),
        at(i.checked_sub(1)),
        c,
        at(Some(i + 1)),
    )
}

fn ends_word_at(text: &[char], end: usize) -> bool {
    end >= text.len() || !alnum(text[end]) || starts_word_at(text, end)
}

/// A commit SHA or a UUID's part: seven or more hex digits with a digit among them.
fn hex_id(run: &[char]) -> bool {
    run.len() >= 7
        && run.iter().all(char::is_ascii_hexdigit)
        && run.iter().any(char::is_ascii_digit)
}

/// Where `token` best sits in `folded` (a fold of `text`): every place it's a whole word, else a
/// word's start, else inside a word, and which of those. Hits past the start of a hex id don't
/// count ("cafe" in "7cafe52bb8"), though a pasted SHA's start does.
fn find_message_token(text: &[char], folded: &[char], token: &[char]) -> Option<(Vec<usize>, f64)> {
    let (mut quality, mut at) = (0., Vec::new());
    // The letters-and-digits run around a hit, kept for later hits in it: finding it again per
    // hit is quadratic in a long run ("0000" in a hex dump).
    let mut run: Option<(usize, usize, bool)> = None;
    let mut found = index_of(folded, token, 0);
    while let Some(i) = found {
        if run.is_none_or(|(_, end, _)| i + token.len() > end) {
            let mut start = i;
            while start > 0 && alnum(text[start - 1]) {
                start -= 1;
            }
            let mut end = i + token.len();
            while end < text.len() && alnum(text[end]) {
                end += 1;
            }
            run = Some((start, end, hex_id(&text[start..end])));
        }
        let (start, _, hex) = run.unwrap_or_default();
        if !(hex && start < i) {
            let q = if !starts_word_at(text, i) {
                MID_WORD
            } else if ends_word_at(text, i + token.len()) {
                1.
            } else {
                PREFIX
            };
            if q > quality {
                quality = q;
                at = vec![i];
            } else if q == quality {
                at.push(i);
            }
        }
        found = index_of(folded, token, i + 1);
    }
    (!at.is_empty()).then_some((at, quality))
}

/// The words side by side as whole words, as typed, whatever whitespace is between them: where
/// they start in `text` (both lower-cased), or None (or for one word).
fn phrase_at(words: &[Vec<char>], text: &[char]) -> Option<usize> {
    let first = words.first().filter(|_| words.len() > 1)?;
    let mut found = index_of(text, first, 0);
    while let Some(i) = found {
        if i == 0 || !alnum(text[i - 1]) {
            let mut end = Some(i + first.len());
            for w in &words[1..] {
                end = end.and_then(|e| {
                    let mut k = e;
                    while k < text.len() && text[k].is_whitespace() {
                        k += 1;
                    }
                    (k > e && text[k..].starts_with(w)).then_some(k + w.len())
                });
            }
            if let Some(e) = end
                && (e >= text.len() || !alnum(text[e]))
            {
                return Some(i);
            }
        }
        found = index_of(text, first, i + 1);
    }
    None
}

/// The shortest stretch holding one place of each word, from each word's places: its start and
/// length.
fn shortest_span(places: &[Vec<usize>]) -> (usize, usize) {
    let mut hits: Vec<(usize, usize)> = places
        .iter()
        .enumerate()
        .flat_map(|(w, at)| at.iter().map(move |&i| (i, w)))
        .collect();
    hits.sort_by_key(|h| h.0);
    let mut counts = vec![0usize; places.len()];
    let (mut covered, mut left) = (0, 0);
    let mut best = (hits.first().map_or(0, |h| h.0), usize::MAX);
    for &(index, w) in &hits {
        counts[w] += 1;
        if counts[w] == 1 {
            covered += 1;
        }
        while covered == places.len() {
            let (first, fw) = hits[left];
            if index - first < best.1 {
                best = (first, index - first);
            }
            counts[fw] -= 1;
            if counts[fw] == 0 {
                covered -= 1;
            }
            left += 1;
        }
    }
    best
}

/// How a message matched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MessageMatch {
    /// 1 when every looked-up word is a whole word, [`PREFIX`] when one only starts a word,
    /// [`MID_WORD`] when one is only inside words; plus [`PHRASE_BONUS`] for the words written
    /// together as typed, else [`NEAR_BONUS`] for them near each other.
    pub quality: f64,
    /// Where a snippet should start to show it (a character of the text).
    pub offset: usize,
}

struct MessageToken {
    token: Vec<char>,
    /// Folded, marks gone: lower-casing "İ" adds one.
    folded: Vec<char>,
    non_ascii: bool,
    must_start_word: bool,
    /// Long enough to grade by: a short one only narrows, so typing the next word ("nvme s")
    /// doesn't reorder results.
    grades: bool,
}

/// A query made ready to grade messages by (the server grades thousands for one query).
pub struct MessageMatcher {
    tokens: Vec<MessageToken>,
    phrase: Vec<Vec<char>>,
    folded_phrase: Vec<Vec<char>>,
    long: usize,
    non_ascii: bool,
}

fn fold_token(t: &str) -> Vec<char> {
    t.chars().map(fold).filter(|c| !mark(*c)).collect()
}

/// Every character lower-cased, one for one (what's not one-for-one is folded).
fn lowered(text: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len());
    for &c in text {
        let mut l = c.to_lowercase();
        match (l.next(), l.next()) {
            (Some(one), None) => out.push(one),
            _ => return fold_all(text),
        }
    }
    out
}

fn straight(text: &[char]) -> Vec<char> {
    text.iter()
        .map(|&c| if matches!(c, '‘' | '’') { '\'' } else { c })
        .collect()
}

impl MessageMatcher {
    pub fn new(query: &str) -> MessageMatcher {
        let Tokens { long, short, words } = message_tokens(query);
        let tokens = long
            .iter()
            .map(|t| (t, true))
            .chain(short.iter().map(|t| (t, false)))
            .map(|(t, grades)| MessageToken {
                token: t.chars().collect(),
                folded: fold_token(t),
                non_ascii: !t.is_ascii(),
                must_start_word: t.chars().count() < WORD_START_MIN,
                grades,
            })
            .collect();
        MessageMatcher {
            tokens,
            phrase: words.iter().map(|w| w.chars().collect()).collect(),
            folded_phrase: words.iter().map(|w| fold_token(w)).collect(),
            long: long.len(),
            non_ascii: !query.is_ascii(),
        }
    }

    /// Whether there's anything to look up.
    pub fn searches(&self) -> bool {
        self.long > 0
    }

    /// How `text` matches, or None: a word missing, or a short one never starting a word ("fts"
    /// in "drafts"). Case and accents are ignored, as the index ignores them.
    pub fn grade(&self, text: &str) -> Option<MessageMatch> {
        let text: Vec<char> = text.chars().collect();
        self.grade_chars(&text)
    }

    pub fn grade_chars(&self, text: &[char]) -> Option<MessageMatch> {
        let plain = lowered(text);
        let text_non_ascii = text.iter().any(|c| !c.is_ascii());
        let mut folded: Option<Vec<char>> = None;
        let (mut quality, mut places) = (1f64, Vec::with_capacity(self.tokens.len()));
        for t in &self.tokens {
            let mut found = find_message_token(text, &plain, &t.token);
            // Accents typed or left out: "cafe" finds "Café", also after "cafeteria".
            if found.as_ref().is_none_or(|f| f.1 != 1.)
                && !t.folded.is_empty()
                && (t.non_ascii || text_non_ascii)
            {
                found = find_message_token(
                    text,
                    folded.get_or_insert_with(|| fold_all(text)),
                    &t.folded,
                );
            }
            let (at, q) = found?;
            if q == MID_WORD && t.must_start_word {
                return None;
            }
            if t.grades {
                quality = quality.min(q);
            }
            places.push(at);
        }
        // Written together as typed: the snippet starts there. Apostrophes are straight in the
        // query, and straightening keeps places.
        let together = phrase_at(&self.phrase, &straight(&plain)).or_else(|| {
            (self.non_ascii || text_non_ascii)
                .then(|| {
                    phrase_at(
                        &self.folded_phrase,
                        &straight(folded.get_or_insert_with(|| fold_all(text))),
                    )
                })
                .flatten()
        });
        if let Some(offset) = together {
            return Some(MessageMatch {
                quality: quality + PHRASE_BONUS,
                offset,
            });
        }
        // Else the shortest stretch holding every word: the snippet starts there, and words
        // close together rank above words far apart.
        let (offset, length) = shortest_span(&places);
        Some(MessageMatch {
            quality: if self.long > 1 && length <= NEAR_SPAN {
                quality + NEAR_BONUS
            } else {
                quality
            },
            offset,
        })
    }
}

/// One line of `text` to show for a match at character `at`: from a few words before it,
/// Markdown's `**` and backticks left out (a thread doesn't show them either), whitespace run
/// together, at most [`SNIPPET`] characters, with `…` where it was cut.
pub fn snippet(text: &[char], at: usize) -> String {
    let at = at.min(text.len());
    let mut start = at.saturating_sub(LEAD);
    if start > 0
        && let Some(space) = text[start..at].iter().position(|c| c.is_whitespace())
    {
        start += space + 1;
    }
    // Running whitespace together shortens it, so read more of it.
    let raw_end = text.len().min(start + 2 * (SNIPPET + 2));
    let mut shown: Vec<char> = Vec::new();
    let mut k = start;
    while k < raw_end {
        let c = text[k];
        if c == '`' || (c == '*' && text.get(k + 1) == Some(&'*') && k + 1 < raw_end) {
            k += if c == '*' { 2 } else { 1 };
            continue;
        }
        if c.is_whitespace() {
            if shown.last().is_some_and(|l| *l != ' ') {
                shown.push(' ');
            }
        } else {
            shown.push(c);
        }
        k += 1;
    }
    while shown.last() == Some(&' ') {
        shown.pop();
    }
    let end = shown.len().min(SNIPPET);
    let cut = end < shown.len() || raw_end < text.len();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        shown[..end].iter().collect::<String>(),
        if cut { "…" } else { "" }
    )
}

/// Where in `text` (character ranges) a long word of `query` is, ignoring case and accents, to
/// mark it: shorter words would light up half a line, and a three-letter one counts only where
/// it starts a word.
pub fn token_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let mut tokens: Vec<(Vec<char>, bool)> = Vec::new();
    for t in message_tokens(query).long {
        let folded: Vec<char> = fold_query(&t).chars().collect();
        // Some fold to nothing ("´´´"), which would match everywhere.
        if folded.is_empty() {
            continue;
        }
        let short = t.chars().count() < WORD_START_MIN;
        match tokens.iter_mut().find(|(f, _)| *f == folded) {
            Some(entry) => entry.1 = short,
            None => tokens.push((folded, short)),
        }
    }
    if tokens.is_empty() {
        return vec![];
    }
    let field = Field::of(text);
    let mut ranges = Vec::new();
    for (token, word_start) in &tokens {
        let own_boundary = !alnum(token[0]) && !symbol(token[0]);
        let mut found = index_of(&field.text, token, 0);
        while let Some(i) = found {
            if *word_start && !own_boundary && !field.starts[i] {
                found = index_of(&field.text, token, i + 1);
                continue;
            }
            ranges.push((i, i + token.len()));
            found = index_of(&field.text, token, i + token.len());
        }
    }
    merged(ranges)
}

/// A thread's messages for a query, as message results rank: its best message's quality, how
/// many matched that well, when that was said, and the thread's pick and use.
#[derive(Debug, Clone, Copy, Default)]
pub struct MessageRank {
    pub quality: f64,
    pub hits: u32,
    /// When its best message was said (ms).
    pub at: Option<i64>,
    /// When the thread was picked for this query (ms; 0: never).
    pub picked_at: i64,
    /// [`usage_boost`].
    pub boost: f64,
}

impl MessageRank {
    /// A thread that keeps using the word outweighs a recent passing mention: hits count for
    /// more than use (21 hits about 5.5, one 2).
    fn weight(&self) -> f64 {
        (1. + (1. + self.hits as f64).log2()) * self.boost
    }

    /// The thread's pick for `query` (folded), if `picks`.
    pub fn picked(opens: Option<&Opens>, query: &str, now: i64) -> i64 {
        active_pick(opens, query, now).map_or(0, |p| p.at)
    }
}

/// How message results order: word matches (whole or a start) first; among them a pick, then
/// the closer match, then hits and use, then the newest.
pub fn compare_messages(l: &MessageRank, r: &MessageRank) -> Ordering {
    (r.quality >= PREFIX)
        .cmp(&(l.quality >= PREFIX))
        .then(r.picked_at.cmp(&l.picked_at))
        .then(r.quality.partial_cmp(&l.quality).unwrap_or(Ordering::Equal))
        .then(
            r.weight()
                .partial_cmp(&l.weight())
                .unwrap_or(Ordering::Equal),
        )
        .then(r.at.cmp(&l.at))
}

#[cfg(test)]
mod tests;
