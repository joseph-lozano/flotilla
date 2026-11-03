//! New thread's branch picker: a new branch (the default), or an open pull request or branch
//! of the project's repo already on GitHub. The search also takes `#12` or a PR's URL.

use flotilla_model::github::parse_pr_ref;
use flotilla_proto::{BranchChoices, OnBranch, OpenPr, Project};
use gpui_kit::assets::IconName;
use gpui_kit::component::searchable_list::{SearchableListItem, SearchableVec};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex};
use gpui_kit::*;

#[derive(Clone, PartialEq)]
enum Kind {
    /// From the default branch, named from the prompt.
    New {
        from: String,
    },
    Pr(OpenPr),
    Branch(String),
    /// Loading, or why the list couldn't load.
    Note(String),
}

#[derive(Clone, PartialEq)]
pub struct BranchChoice {
    kind: Kind,
    /// The repo, to read a typed PR URL against.
    owner: String,
    repo: String,
    /// Another thread is on this branch already.
    in_use: bool,
    /// The row's value: empty for a new branch, else the branch (`#N` for a note or a fork's PR).
    value: String,
}

pub type BranchList = SearchableVec<BranchChoice>;

/// The picker's rows for `p`: a new branch, then its open pull requests, then its branches.
/// `chosen` is listed even before the lists load (a thread's branch, say).
pub fn choices(
    p: &Project,
    lists: Option<&Result<BranchChoices, String>>,
    in_use: &[String],
    chosen: Option<&OnBranch>,
) -> BranchList {
    SearchableVec::new(rows(p, lists, in_use, chosen))
}

fn rows(
    p: &Project,
    lists: Option<&Result<BranchChoices, String>>,
    in_use: &[String],
    chosen: Option<&OnBranch>,
) -> Vec<BranchChoice> {
    let row = |kind: Kind, value: String| BranchChoice {
        in_use: !value.is_empty() && in_use.contains(&value),
        kind,
        owner: p.owner.clone(),
        repo: p.repo.clone(),
        value,
    };
    let mut rows = vec![row(
        Kind::New {
            from: p.default_branch.clone(),
        },
        String::new(),
    )];
    match lists {
        None => rows.push(row(
            Kind::Note("Loading pull requests and branches…".into()),
            "#loading".into(),
        )),
        Some(Err(e)) => rows.push(row(
            Kind::Note(format!("Couldn't load them: {e}")),
            "#error".into(),
        )),
        Some(Ok(lists)) => {
            for pr in &lists.prs {
                let value = if pr.from_fork {
                    format!("#{}", pr.number)
                } else {
                    pr.head.clone()
                };
                rows.push(row(Kind::Pr(pr.clone()), value));
            }
            rows.extend(
                lists
                    .branches
                    .iter()
                    .map(|b| row(Kind::Branch(b.clone()), b.clone())),
            );
        }
    }
    if let Some(c) = chosen
        && !rows.iter().any(|r| r.value == c.name)
    {
        rows.insert(1, row(Kind::Branch(c.name.clone()), c.name.clone()));
    }
    rows
}

/// The branch a picked row starts on (`None`: a new one), with what it merges into.
pub fn on_branch(
    p: &Project,
    lists: Option<&Result<BranchChoices, String>>,
    value: &str,
) -> Option<OnBranch> {
    if value.is_empty() || value.starts_with('#') {
        return None;
    }
    let pr = lists
        .and_then(|l| l.as_ref().ok())
        .and_then(|l| l.prs.iter().find(|pr| !pr.from_fork && pr.head == value));
    Some(OnBranch {
        name: value.to_string(),
        base: pr
            .map(|pr| pr.base.clone())
            .unwrap_or_else(|| p.default_branch.clone()),
    })
}

impl SearchableListItem for BranchChoice {
    type Value = String;

    fn title(&self) -> SharedString {
        match &self.kind {
            Kind::New { .. } => "New branch".into(),
            Kind::Pr(pr) => format!("#{} {}", pr.number, pr.head).into(),
            Kind::Branch(b) => b.clone().into(),
            Kind::Note(n) => n.clone().into(),
        }
    }

    fn display_title(&self) -> Option<AnyElement> {
        let icon = match &self.kind {
            Kind::Pr(_) => IconName::GitPullRequestArrow,
            _ => IconName::GitBranch,
        };
        Some(
            h_flex()
                .gap_1()
                .child(Icon::new(icon).xsmall())
                .child(self.title())
                .into_any_element(),
        )
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let (icon, main, detail): (IconName, String, Option<String>) = match &self.kind {
            Kind::New { .. } => (IconName::Plus, "New branch".into(), None),
            Kind::Pr(pr) => {
                let why = if pr.from_fork { " · from a fork" } else { "" };
                let draft = if pr.draft { " · draft" } else { "" };
                (
                    IconName::GitPullRequestArrow,
                    format!("#{} {}", pr.number, pr.title),
                    Some(format!("{} → {}{draft}{why}", pr.head, pr.base)),
                )
            }
            Kind::Branch(b) => (IconName::GitBranch, b.clone(), None),
            Kind::Note(n) => (IconName::Info, n.clone(), None),
        };
        let detail = match (detail, self.in_use) {
            (Some(d), true) => Some(format!("{d} · another thread is on it")),
            (None, true) => Some("another thread is on it".into()),
            (d, false) => d,
        };
        // The name keeps its room; the details give way.
        h_flex()
            .gap_2()
            .min_w_0()
            .child(Icon::new(icon).xsmall().text_color(muted))
            .child(div().flex_none().max_w(px(240.)).truncate().child(main))
            .children(detail.map(|d| {
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(d)
            }))
    }

    fn value(&self) -> &String {
        &self.value
    }

    fn matches(&self, query: &str) -> bool {
        let q = query.trim();
        if q.is_empty() {
            return true;
        }
        // `#12` or a PR's URL picks that pull request alone.
        if let Some(number) = parse_pr_ref(q, &self.owner, &self.repo) {
            return matches!((&self.kind, number), (Kind::Pr(pr), Ok(n)) if pr.number == n);
        }
        let q = q.to_lowercase();
        let hay = match &self.kind {
            Kind::New { .. } => "new branch".to_string(),
            Kind::Pr(pr) => format!("#{} {} {}", pr.number, pr.title, pr.head),
            Kind::Branch(b) => b.clone(),
            Kind::Note(_) => return false,
        };
        hay.to_lowercase().contains(&q)
    }

    fn disabled(&self) -> bool {
        matches!(&self.kind, Kind::Note(_)) || matches!(&self.kind, Kind::Pr(pr) if pr.from_fork)
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's glob brings its own `test` macro.
    use super::{BranchChoices, OnBranch, OpenPr, Project, SearchableListItem, on_branch, rows};

    fn project() -> Project {
        Project {
            id: "p1".into(),
            owner: "acme".into(),
            repo: "web".into(),
            default_branch: "main".into(),
            environment_name: "flotilla-web".into(),
            created_at: String::new(),
            template_sandbox_id: None,
            template_sha: None,
            template_built_at: None,
            prev_template_sandbox_id: None,
            head_sha: None,
            head_etag: None,
            checked_at: None,
        }
    }

    fn pr(number: i64, head: &str, from_fork: bool) -> OpenPr {
        OpenPr {
            number,
            title: format!("PR {number}"),
            url: String::new(),
            head: head.into(),
            base: "next".into(),
            draft: false,
            from_fork,
        }
    }

    #[test]
    fn search_takes_numbers_urls_and_names() {
        let p = project();
        let lists = Ok(BranchChoices {
            prs: vec![pr(3, "fix-login", false), pr(4, "their-fix", true)],
            branches: vec!["spike".into()],
        });
        let rows = rows(&p, Some(&lists), &["spike".to_string()], None);
        let found = |q: &str| {
            rows.iter()
                .filter(|r| r.matches(q))
                .map(|r| r.value.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(found(""), ["", "fix-login", "#4", "spike"]);
        assert_eq!(found("#3"), ["fix-login"]);
        assert_eq!(found("https://github.com/acme/web/pull/4"), ["#4"]);
        assert!(found("https://github.com/other/web/pull/3").is_empty());
        assert_eq!(found("SPI"), ["spike"]);
        assert!(rows.iter().find(|r| r.value == "spike").unwrap().in_use);
        assert!(rows.iter().find(|r| r.value == "#4").unwrap().disabled());
        // A PR's branch merges into its base; a plain branch into the default branch.
        assert_eq!(
            on_branch(&p, Some(&lists), "fix-login"),
            Some(OnBranch {
                name: "fix-login".into(),
                base: "next".into()
            })
        );
        assert_eq!(
            on_branch(&p, Some(&lists), "spike"),
            Some(OnBranch {
                name: "spike".into(),
                base: "main".into()
            })
        );
        assert_eq!(on_branch(&p, Some(&lists), ""), None);
        assert_eq!(on_branch(&p, Some(&lists), "#4"), None);
    }

    #[test]
    fn a_chosen_branch_shows_before_the_lists_load() {
        let p = project();
        let chosen = OnBranch {
            name: "fix-login".into(),
            base: "main".into(),
        };
        let rows = rows(&p, None, &[], Some(&chosen));
        let values: Vec<String> = rows.iter().map(|r| r.value.clone()).collect();
        assert_eq!(values, ["", "fix-login", "#loading"]);
    }
}
