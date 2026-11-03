//! The menu `/`, `$` and `@` open in a composer: the skills and commands the thread's agent can
//! run, or the files and folders in its repo, narrowed by what's typed after the sign. Picking
//! one puts it in the prompt the way that agent's harness reads it.

use flotilla_model::mentions::{file_mention, leads_prompt};
use flotilla_proto::{Entry, Kind, Mentions};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::TextareaState;
use gpui_kit::component::{Icon, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;

/// Rows shown at most; typing more narrows them.
const ROWS: usize = 8;

/// What a composer can offer for one harness: a thread's skills, commands and paths, ready to
/// search as you type.
pub struct Offer {
    provider: String,
    root: String,
    entries: Vec<Entry>,
    /// The repo's files, then its folders (ending in `/`).
    paths: Vec<String>,
}

impl Offer {
    pub fn new(provider: &str, m: &Mentions) -> Offer {
        let mut dirs = BTreeSet::new();
        for f in &m.files {
            let mut end = 0;
            while let Some(i) = f[end..].find('/') {
                end += i + 1;
                dirs.insert(&f[..end]);
            }
        }
        let mut paths = m.files.clone();
        paths.extend(dirs.into_iter().map(str::to_string));
        Offer {
            provider: provider.to_string(),
            root: m.root.clone(),
            entries: m.entries.clone(),
            paths,
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    fn skills(&self, query: &str) -> Vec<Item> {
        let q = query.to_lowercase();
        let mut found: Vec<(u8, &Entry, String)> = self
            .entries
            .iter()
            .filter_map(|e| {
                let insert = e.insert(&self.provider)?.to_string();
                let name = e.name.to_lowercase();
                let score = if name.starts_with(&q) {
                    0
                } else if name.contains(&q) {
                    1
                } else if q.chars().count() >= 3 && e.description.to_lowercase().contains(&q) {
                    2
                } else {
                    return None;
                };
                Some((score, e, insert))
            })
            .collect();
        found.sort_by(|a, b| (a.0, &a.1.name).cmp(&(b.0, &b.1.name)));
        let mut seen = BTreeSet::new();
        found
            .into_iter()
            .filter(|(_, _, insert)| seen.insert(insert.clone()))
            .take(ROWS)
            .map(|(_, e, insert)| Item {
                icon: if e.kind == "skill" {
                    IconName::Sparkles
                } else {
                    IconName::SquareSlash
                },
                // Codex's text for a name two skills share links the file: show the name.
                label: if self.provider == "codex" {
                    format!("${}", e.name)
                } else {
                    insert.clone()
                },
                detail: e.description.clone(),
                insert,
                leads: leads_prompt(&self.provider),
                folder: false,
            })
            .collect()
    }

    fn files(&self, query: &str) -> Vec<Item> {
        // A full path (as Codex and OpenCode get them) searches from the repo.
        let query = query
            .strip_prefix(&format!("{}/", self.root.trim_end_matches('/')))
            .filter(|_| !self.root.is_empty())
            .unwrap_or(query);
        let q = query.to_lowercase();
        let mut found: Vec<(usize, usize, &String)> = self
            .paths
            .iter()
            .filter_map(|p| {
                let lower = p.to_lowercase();
                // Not the folder being gone into itself (a file typed out in full stays).
                if p.ends_with('/') && lower == q {
                    return None;
                }
                let name = lower
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or(&lower);
                if q.is_empty() {
                    // Nothing typed: the top of the repo first, folders before files.
                    let depth = lower.trim_end_matches('/').matches('/').count();
                    return Some((depth * 2 + usize::from(!p.ends_with('/')), 0, p));
                }
                let score = if name.starts_with(&q) {
                    0
                } else if name.contains(&q) {
                    1
                } else if lower.contains(&q) {
                    2
                } else if in_order(&q, &lower) {
                    3
                } else {
                    return None;
                };
                Some((score, p.len(), p))
            })
            .collect();
        found.sort();
        found
            .into_iter()
            .take(ROWS)
            .map(|(_, _, p)| {
                let folder = p.ends_with('/');
                let trimmed = p.trim_end_matches('/');
                let (dir, name) = match trimmed.rfind('/') {
                    Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
                    None => ("", trimmed),
                };
                let insert = file_mention(&self.provider, &self.root, p);
                Item {
                    icon: if folder {
                        IconName::Folder
                    } else {
                        crate::attachments::icon(Kind::of_name(name))
                    },
                    label: if folder {
                        format!("{name}/")
                    } else {
                        name.to_string()
                    },
                    detail: dir.to_string(),
                    // One quoted for its spaces can't be gone on into: it's picked as it is.
                    folder: folder && !insert.contains('"'),
                    insert,
                    leads: false,
                }
            })
            .collect()
    }
}

/// Whether `q`'s characters appear in `s` in order.
fn in_order(q: &str, s: &str) -> bool {
    let mut rest = s.chars();
    q.chars().all(|c| rest.any(|x| x == c))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    icon: IconName,
    label: String,
    detail: String,
    insert: String,
    /// It works only at the very start of the prompt.
    leads: bool,
    /// A folder: picking it goes on into it.
    folder: bool,
}

/// What picking a row does: put `text` in place of the word at `range`, or, when it must
/// `lead`, at the very start of the prompt (taking the word away).
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub range: Range<usize>,
    pub text: String,
    pub leads: bool,
    /// A folder, left open (no space after it) to go on into.
    pub folder: bool,
}

impl Choice {
    /// Make the edit in the composer, leaving the caret after what was typed.
    pub fn apply(
        &self,
        input: &mut TextareaState,
        window: &mut Window,
        cx: &mut Context<TextareaState>,
    ) {
        if self.leads && self.range.start > 0 {
            // Claude Code and OpenCode run a skill or command only at the very start.
            let text = input.value().to_string();
            let start = if text[..self.range.start].ends_with(' ') {
                self.range.start - 1
            } else {
                self.range.start
            };
            input.set_selected_range(start..self.range.end, cx);
            input.replace("", window, cx);
            input.set_selected_range(0..0, cx);
            input.replace(self.text.clone(), window, cx);
            let at = start + self.text.len();
            input.set_selected_range(at..at, cx);
        } else {
            input.set_selected_range(self.range.clone(), cx);
            input.replace(self.text.clone(), window, cx);
        }
    }
}

/// The word the caret ends, if it starts with a sign: where it starts, the sign, and what
/// follows. A caret inside a word opens nothing (what's after it would be left dangling). A
/// `/` or `$` word takes only name characters, so a path or a price doesn't open it.
fn word(text: &str, cursor: usize) -> Option<(usize, char, &str)> {
    let before = text.get(..cursor)?;
    if text[cursor..]
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace())
    {
        return None;
    }
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let w = &before[start..];
    let sign = w.chars().next()?;
    let query = &w[sign.len_utf8()..];
    match sign {
        '@' => Some((start, sign, query)),
        '/' | '$'
            if query
                .chars()
                .all(|c| c.is_alphanumeric() || "-_:.".contains(c)) =>
        {
            Some((start, sign, query))
        }
        _ => None,
    }
}

#[derive(Default)]
pub struct Menu {
    /// While open: the word's range, its sign, and what matches.
    open: Option<(Range<usize>, char, Vec<Item>)>,
    selected: usize,
    /// Escape closed the menu on the word starting here: it stays shut until another starts.
    dismissed: Option<usize>,
}

impl Menu {
    /// Follow the composer's text and caret: open on a word starting with `/`, `$` or `@`,
    /// narrow as it's typed, close once the caret leaves it. `None`: nothing read yet. Returns
    /// whether it just opened.
    pub fn update(&mut self, text: &str, cursor: usize, offer: Option<&Offer>) -> bool {
        let was_open = self.open.is_some();
        // `$` is Codex's sign for a skill: elsewhere it's just text (a price, a variable).
        let Some((start, sign, query)) = word(text, cursor)
            .filter(|(_, sign, _)| *sign != '$' || offer.is_some_and(|o| o.provider == "codex"))
        else {
            self.open = None;
            self.dismissed = None;
            return false;
        };
        if self.dismissed == Some(start) {
            self.open = None;
            return false;
        }
        self.dismissed = None;
        let items = match (offer, sign) {
            (None, _) => Vec::new(),
            (Some(o), '@') => o.files(query),
            (Some(o), _) => o.skills(query),
        };
        // A skill name that matches nothing is just text (a price, a fraction): no menu.
        if items.is_empty() && !query.is_empty() && sign != '@' && offer.is_some() {
            self.open = None;
            return false;
        }
        // New matches start from the top.
        if self.open.as_ref().map(|(r, _, it)| (r.start, it)) != Some((start, &items)) {
            self.selected = 0;
        }
        self.selected = self.selected.min(items.len().saturating_sub(1));
        self.open = Some((start..cursor, sign, items));
        !was_open
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Whether there's something to pick (Enter, Tab and the arrows go to the menu).
    pub fn has_items(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|(_, _, items)| !items.is_empty())
    }

    /// Move the highlight, wrapping around.
    pub fn step(&mut self, by: isize) {
        if let Some((_, _, items)) = &self.open
            && !items.is_empty()
        {
            self.selected = (self.selected as isize + by).rem_euclid(items.len() as isize) as usize;
        }
    }

    /// What picking the highlighted row, or row `i`, does.
    pub fn choice(&self, i: Option<usize>) -> Option<Choice> {
        let (range, _, items) = self.open.as_ref()?;
        items.get(i.unwrap_or(self.selected)).map(|it| Choice {
            range: range.clone(),
            text: if it.folder {
                it.insert.clone()
            } else {
                format!("{} ", it.insert)
            },
            leads: it.leads,
            folder: it.folder,
        })
    }

    /// Escape: shut until the caret leaves this word.
    pub fn dismiss(&mut self) {
        if let Some((range, _, _)) = self.open.take() {
            self.dismissed = Some(range.start);
        }
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    /// The menu, if open. `waiting`: what to say while there's nothing to show because nothing
    /// has been read yet. `pick` gets the row clicked.
    pub fn render(
        &self,
        waiting: &str,
        theme: &Theme,
        pick: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Option<AnyElement> {
        let (_, sign, items) = self.open.as_ref()?;
        let pick = Rc::new(pick);
        let empty = match (items.is_empty(), *sign) {
            (false, _) => None,
            (true, _) if !waiting.is_empty() => Some(waiting.to_string()),
            (true, '@') => Some("No matching files".to_string()),
            (true, _) => Some("No skills or commands".to_string()),
        };
        Some(
            v_flex()
                .id("mention-menu")
                .occlude()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_md()
                .text_sm()
                .children(empty.map(|e| {
                    div()
                        .px_2()
                        .py_1()
                        .text_color(theme.muted_foreground)
                        .child(e)
                }))
                .children(items.iter().enumerate().map(|(i, it)| {
                    let pick = pick.clone();
                    h_flex()
                        .id(("mention", i))
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(i == self.selected, |el| el.bg(theme.primary.opacity(0.16)))
                        .hover(|s| s.bg(theme.list_hover))
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            pick(i, window, cx)
                        })
                        .child(
                            Icon::new(it.icon)
                                .size(px(14.))
                                .text_color(theme.muted_foreground),
                        )
                        .child(
                            div()
                                .flex_none()
                                .max_w(relative(0.6))
                                .truncate()
                                .child(it.label.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(it.detail.clone()),
                        )
                })),
        )
        .map(|el| el.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::{Choice, Item, Menu, Offer, word};
    use flotilla_proto::{Entry, Mentions};

    fn offer(provider: &str) -> Offer {
        let m = Mentions {
            entries: vec![
                entry(
                    "skill",
                    "review-pr",
                    "Review a pull request",
                    "claude-code",
                    "/review-pr",
                ),
                entry("command", "test", "Write tests", "claude-code", "/test"),
                entry(
                    "skill",
                    "deploy",
                    "Ship it",
                    "codex",
                    "[$deploy](/home/user/.agents/skills/deploy/SKILL.md)",
                ),
            ],
            root: "/home/user/app".into(),
            files: vec![
                "README.md".into(),
                "src/main.rs".into(),
                "src/ui/menu.rs".into(),
                "docs/review.md".into(),
            ],
        };
        Offer::new(provider, &m)
    }

    fn entry(kind: &str, name: &str, description: &str, harness: &str, insert: &str) -> Entry {
        Entry {
            kind: kind.into(),
            name: name.into(),
            description: description.into(),
            harness: harness.into(),
            insert: insert.into(),
            shadowed: false,
        }
    }

    fn choice(range: std::ops::Range<usize>, text: &str) -> Option<Choice> {
        Some(Choice {
            range,
            text: text.into(),
            leads: true,
            folder: false,
        })
    }

    fn labels(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn words_that_open_it() {
        assert_eq!(word("fix @src/ma", 11), Some((4, '@', "src/ma")));
        assert_eq!(word("/rev", 4), Some((0, '/', "rev")));
        assert_eq!(word("use $dep", 8), Some((4, '$', "dep")));
        assert_eq!(word("see /usr/bin", 12), None);
        assert_eq!(word("email me@x.com", 14), None);
        assert_eq!(word("done ", 5), None);
        assert_eq!(word("café @a", 8), Some((6, '@', "a")));
        assert_eq!(word("/rev", 2), None);
        assert_eq!(word("/rev more", 4), Some((0, '/', "rev")));
    }

    #[test]
    fn skills_for_the_harness_in_its_syntax() {
        let o = offer("claude-code");
        assert_eq!(labels(&o.skills("")), ["/review-pr", "/test"]);
        assert_eq!(labels(&o.skills("pull")), ["/review-pr"]);
        let codex = offer("codex").skills("");
        assert_eq!(labels(&codex), ["$deploy"]);
        assert_eq!(
            codex[0].insert,
            "[$deploy](/home/user/.agents/skills/deploy/SKILL.md)"
        );
        assert!(!codex[0].leads);
    }

    #[test]
    fn files_and_folders_ranked_by_name() {
        let o = offer("codex");
        let found = o.files("menu");
        assert_eq!(labels(&found), ["menu.rs"]);
        assert_eq!(found[0].detail, "src/ui");
        assert_eq!(found[0].insert, "@/home/user/app/src/ui/menu.rs");
        assert_eq!(labels(&o.files("rev")), ["review.md"]);
        assert_eq!(
            labels(&o.files("")),
            [
                "docs/",
                "src/",
                "README.md",
                "ui/",
                "review.md",
                "main.rs",
                "menu.rs"
            ]
        );
        assert_eq!(labels(&o.files("smr"))[0], "main.rs");
        assert_eq!(offer("claude-code").files("main")[0].insert, "@src/main.rs");
        // Into a folder picked as a full path.
        assert_eq!(labels(&o.files("/home/user/app/src/ui/")), ["menu.rs"]);
        // A file typed out in full is still there to pick.
        assert_eq!(labels(&o.files("readme.md")), ["README.md"]);
    }

    #[test]
    fn follows_the_caret_and_escape() {
        let o = offer("claude-code");
        let mut m = Menu::default();
        assert!(m.update("/", 1, Some(&o)));
        assert!(m.has_items());
        m.step(1);
        assert_eq!(m.choice(None), choice(0..1, "/test "));
        assert!(!m.update("/", 1, Some(&o)));
        assert_eq!(m.choice(None), choice(0..1, "/test "));
        // The same rows keep the highlight; fewer start from the top.
        assert!(!m.update("/r", 2, Some(&o)));
        assert_eq!(m.choice(None), choice(0..2, "/review-pr "));
        // Descriptions count from three letters.
        assert!(!m.update("/tes", 4, Some(&o)));
        assert_eq!(m.choice(None), choice(0..4, "/test "));
        assert!(!m.update("/re", 3, Some(&o)));
        assert_eq!(m.choice(None), choice(0..3, "/review-pr "));
        m.dismiss();
        assert!(!m.update("/rev", 4, Some(&o)));
        assert!(!m.is_open());
        assert!(!m.update("/rev ", 5, Some(&o)));
        assert!(m.update("/rev @", 6, Some(&o)));
        assert!(!m.update("/rev @zzz", 9, Some(&o)) && m.is_open() && !m.has_items());
        assert!(!m.update("costs $5", 8, Some(&o)) && !m.is_open());
        assert!(m.update("@", 1, None) && m.is_open() && !m.has_items());
    }

    #[test]
    fn dollar_opens_skills_on_codex_only() {
        let mut m = Menu::default();
        assert!(!m.update("$", 1, Some(&offer("claude-code"))) && !m.is_open());
        assert!(!m.update("$", 1, None) && !m.is_open());
        let codex = offer("codex");
        assert!(m.update("$", 1, Some(&codex)) && m.is_open());
        assert_eq!(
            m.choice(None).map(|c| c.text),
            Some("[$deploy](/home/user/.agents/skills/deploy/SKILL.md) ".into())
        );
        // `/` still opens on Claude Code.
        assert!(Menu::default().update("/", 1, Some(&offer("claude-code"))));
    }
}
