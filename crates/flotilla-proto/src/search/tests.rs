//! T3 Code's `paletteSearch.test.ts`, case for case where Flotilla has the thing (no
//! environments), plus Flotilla's own.

use super::*;

const NOW: i64 = 1_792_713_600_000; // 2026-10-23
const DAY: i64 = DAY_MS;

/// A thread to rank: title, then what else it has.
#[derive(Clone, Default)]
struct T {
    id: Option<String>,
    title: String,
    branch: Option<String>,
    project: Option<String>,
    prs: Vec<(i64, Option<String>, Vec<String>)>,
    activity: Option<i64>,
    opens: Option<Opens>,
}

fn t(title: &str) -> T {
    T {
        title: title.into(),
        activity: Some(NOW - 30 * DAY),
        ..Default::default()
    }
}

impl T {
    fn at(mut self, at: i64) -> T {
        self.activity = Some(at);
        self
    }
    fn opened(mut self, at: i64, score: f64, picks: &[(&str, i64)]) -> T {
        self.opens = Some(Opens {
            at,
            score,
            picks: picks
                .iter()
                .map(|(q, at)| SearchPick {
                    query: q.to_string(),
                    at: *at,
                })
                .collect(),
        });
        self
    }
    fn branch(mut self, b: &str) -> T {
        self.branch = Some(b.into());
        self
    }
    fn project(mut self, p: &str) -> T {
        self.project = Some(p.into());
        self
    }
    fn pr(mut self, n: i64, title: Option<&str>) -> T {
        self.prs.push((n, title.map(str::to_string), vec![]));
        self
    }
    fn subject(&self) -> Subject<'_> {
        Subject {
            id: self.id.as_deref(),
            title: &self.title,
            branch: self.branch.as_deref(),
            project: self.project.as_deref(),
            prs: self
                .prs
                .iter()
                .map(|(n, title, refs)| PrRef {
                    number: *n,
                    title: title.as_deref(),
                    references: refs.clone(),
                })
                .collect(),
            activity: self.activity,
            opens: self.opens.as_ref(),
        }
    }
}

fn score(query: &str, thread: &T) -> Option<ThreadMatch> {
    score_thread(&fold_query(query), &thread.subject(), NOW, false, true)
}

fn rank_titles(query: &str, threads: &[T]) -> Vec<String> {
    let q = fold_query(query);
    rank(threads.to_vec(), |thread, typos| {
        score_thread(&q, &thread.subject(), NOW, typos, true)
    })
    .into_iter()
    .map(|(t, _)| t.title)
    .collect()
}

fn titles(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn whole_words_over_prefixes_over_newer_mid_word_matches() {
    assert_eq!(
        rank_titles(
            "auth",
            &[
                t("Fix oauth redirect").at(NOW),
                t("Authentication cleanup"),
                t("Share auth across worktrees")
            ]
        ),
        titles(&[
            "Share auth across worktrees",
            "Authentication cleanup",
            "Fix oauth redirect"
        ])
    );
}

#[test]
fn camel_case_kebab_path_and_digit_boundaries_start_words() {
    let starts = |title: &str, q: &str| score(q, &t(title)).map(|m| m.ranges);
    assert_eq!(
        starts("useThreadSearch hook", "search"),
        Some(vec![(9, 15)])
    );
    assert_eq!(starts("fix/sidebar-perf", "perf"), Some(vec![(12, 16)]));
    assert_eq!(starts("Upgrade to pnpm12", "12"), Some(vec![(15, 17)]));
    assert_eq!(starts("Fix JSONSchema parsing", "sch"), Some(vec![(8, 11)]));
    assert_eq!(starts("Fix HTTPServer crash", "ser"), Some(vec![(8, 11)]));
    assert_eq!(starts("Rename index.ts", ".ts"), Some(vec![(12, 15)]));
    // Whole only where a word ends: "githuba" doesn't fill "GitHubActions".
    assert!(score("githuba", &t("GitHubActions setup")).unwrap().quality < 1.);
    assert_eq!(
        rank_titles(".ts", &[t("Fix parser.tsx"), t("Rename index.ts")]),
        titles(&["Rename index.ts", "Fix parser.tsx"])
    );
    // Short tokens never match inside a word.
    assert!(score("ci", &t("Decision log")).is_none());
}

#[test]
fn recent_activity_and_frequent_opens_decide_between_equal_matches() {
    assert_eq!(
        rank_titles(
            "goldbug",
            &[
                t("Goldbug epilogue").at(NOW - 20 * DAY),
                t("Goldbug processes").at(NOW - DAY)
            ]
        ),
        titles(&["Goldbug processes", "Goldbug epilogue"])
    );
    assert_eq!(
        rank_titles(
            "goldbug",
            &[
                t("Goldbug processes").at(NOW - DAY),
                t("Goldbug notes")
                    .at(NOW - 40 * DAY)
                    .opened(NOW - DAY, 12., &[])
            ]
        ),
        titles(&["Goldbug notes", "Goldbug processes"])
    );
}

#[test]
fn the_thread_picked_for_a_query_comes_first_when_it_is_typed_again() {
    let threads = [
        t("Release checklist").opened(NOW - 5 * DAY, 1., &[("rel", NOW - 5 * DAY)]),
        t("Relay pairing").at(NOW),
    ];
    assert_eq!(
        rank_titles("rel", &threads),
        titles(&["Release checklist", "Relay pairing"])
    );
    // A pick never lifts a match inside a word above a word match.
    assert_eq!(
        rank_titles(
            "auth",
            &[
                t("Fix oauth redirect").opened(NOW - DAY, 1., &[("auth", NOW - DAY)]),
                t("Share auth across worktrees")
            ]
        ),
        titles(&["Share auth across worktrees", "Fix oauth redirect"])
    );
    // Two letters are too little to go on.
    assert_eq!(
        rank_titles("re", &threads),
        titles(&["Relay pairing", "Release checklist"])
    );
    // Once the query leaves the pick behind, the better match wins again.
    assert_eq!(rank_titles("rela", &threads), titles(&["Relay pairing"]));
    // An old pick fades.
    let stale = [
        t("Release checklist").opened(NOW - 90 * DAY, 1., &[("rel", NOW - 90 * DAY)]),
        t("Relay pairing").at(NOW),
    ];
    assert_eq!(
        rank_titles("rel", &stale),
        titles(&["Relay pairing", "Release checklist"])
    );
    // Opening an old pick some other way doesn't revive it over a newer pick.
    let reopened = [
        t("Release checklist").opened(NOW - DAY, 2., &[("rel", NOW - 30 * DAY)]),
        t("Relay pairing").opened(NOW - DAY, 1., &[("rel", NOW - DAY)]),
    ];
    assert_eq!(
        rank_titles("rel", &reopened),
        titles(&["Relay pairing", "Release checklist"])
    );
    // The latest pick for a query wins over an earlier one with a better title match.
    let repicked = [
        t("Deploy").opened(NOW - DAY, 1., &[("deploy", NOW - DAY)]),
        t("Deploy timeout on Railway").opened(NOW, 1., &[("deploy", NOW)]),
    ];
    assert_eq!(
        rank_titles("deploy", &repicked),
        titles(&["Deploy timeout on Railway", "Deploy"])
    );
    // A thread's exact pick counts even after a newer pick for a longer query.
    let exact = [
        t("Fix browser").opened(NOW, 2., &[("fix browser", NOW), ("fix", NOW - 2 * DAY)]),
        t("Fix sidebar").opened(NOW - DAY, 1., &[("fix", NOW - DAY)]),
    ];
    assert_eq!(
        rank_titles("fix", &exact),
        titles(&["Fix sidebar", "Fix browser"])
    );
    // A pick for a longer query doesn't pass the thread named by the query.
    let named = [
        t("Lakebed perf audit").opened(NOW, 1., &[("lakebed perf", NOW)]),
        t("Lakebed"),
    ];
    assert_eq!(
        rank_titles("lakebed", &named),
        titles(&["Lakebed", "Lakebed perf audit"])
    );
}

#[test]
fn a_branch_pasted_with_flotillas_prefix_is_found() {
    let s = t("Cmd+K search").branch("flotilla/cmdk-search-v2");
    assert!(score("flotilla/cmdk-search-v2", &s).is_some());
    assert!(score("flotilla/cmdk", &s).is_some());
}

#[test]
fn a_title_with_the_words_ranks_above_flotillas_branch_prefix_and_the_project() {
    let busy = |x: T| x.at(NOW).opened(NOW, 6., &[]);
    assert_eq!(
        rank_titles(
            "flotilla connect",
            &[
                busy(t("Diagnose Connect provisioning failures"))
                    .branch("flotilla/diagnose-connect")
                    .project("acme/flotilla"),
                t("Flotilla Connect Tunnel Lifecycle Review").project("acme/flotilla")
            ]
        )[0],
        "Flotilla Connect Tunnel Lifecycle Review"
    );
}

#[test]
fn pull_requests_branches_and_projects_each_hold_some_words() {
    let s = t("Trim model lists")
        .branch("flotilla/trim-mcp-models")
        .project("acme/t3code")
        .pr(22889, Some("Smaller MCP payloads"));
    for q in [
        "#22889",
        "22889",
        "trim mcp",
        "payloads t3code",
        "models trim",
    ] {
        assert!(score(q, &s).is_some(), "{q}");
    }
    assert!(score("trim lakebed", &s).is_none());
}

#[test]
fn one_typo_is_forgiven_only_when_nothing_matches_exactly() {
    let threads = [
        t("Hosting alternatives to Railway"),
        t("Trim MCP model lists"),
    ];
    assert_eq!(
        rank_titles("railwya", &threads),
        titles(&["Hosting alternatives to Railway"])
    );
    assert_eq!(
        rank_titles("trim mpc", &threads),
        titles(&["Trim MCP model lists"])
    );
    assert_eq!(
        rank_titles(
            "drga",
            &[t("Restore drag handles"), t("Organize indicators")]
        ),
        titles(&["Restore drag handles"])
    );
    assert_eq!(
        rank_titles("22889", &[t("Stats").pr(22189, None)]),
        titles(&[])
    );
    // Words with digits still forgive one.
    assert_eq!(
        rank_titles("t3coed", &[t("t3code perf audit"), t("Other")]),
        titles(&["t3code perf audit"])
    );
    assert_eq!(
        rank_titles("oath2", &[t("Fix oauth2 redirect")]),
        titles(&["Fix oauth2 redirect"])
    );
    // A typo beats a mid-word accident: "ilde" is "idle", not the middle of "Builder".
    assert_eq!(
        rank_titles(
            "ilde",
            &[t("Typesafe App Builder"), t("De-risk idle tunnel cleanup")]
        ),
        titles(&["De-risk idle tunnel cleanup", "Typesafe App Builder"])
    );
    // "ing" isn't "Nightly": a three-letter typo keeps the first letter.
    assert_eq!(rank_titles("ing", &[t("Nightly build fix")]), titles(&[]));
    // A short word may miss one letter while it's typed, but not change one.
    assert_eq!(
        rank_titles("srv", &[t("Server restart"), t("Other")]),
        titles(&["Server restart"])
    );
    assert_eq!(
        rank_titles("sidb", &[t("Fix sidebar lag")]),
        titles(&["Fix sidebar lag"])
    );
    assert_eq!(rank_titles("cron", &[t("Config cleanup")]), titles(&[]));
    // Filler words in a phrase aren't required, but alone they are.
    assert_eq!(
        rank_titles("fix the sidebar", &[t("Fix Sidebar Lag")]),
        titles(&["Fix Sidebar Lag"])
    );
    assert_eq!(rank_titles("the", &[t("Fix sidebar lag")]), titles(&[]));
    // A straight apostrophe finds a curly one.
    assert_eq!(
        rank_titles("don't", &[t("Don’t merge yet")]),
        titles(&["Don’t merge yet"])
    );
    // Typos count across camelCase joins.
    assert_eq!(
        rank_titles("githbu", &[t("GitHub API usage"), t("Other")]),
        titles(&["GitHub API usage"])
    );
    assert_eq!(
        rank_titles("macso", &[t("Fix macOS window"), t("Other")]),
        titles(&["Fix macOS window"])
    );
    // "enter" isn't "external" when the right thread is already there.
    assert_eq!(
        rank_titles("cmd enter", &[t("Fix Cmd+Enter"), t("Open links external")]),
        titles(&["Fix Cmd+Enter"])
    );
}

#[test]
fn queries_that_fold_to_nothing_find_nothing() {
    assert_eq!(
        rank_titles("# fix", &[t("Fix sidebar lag")]),
        titles(&["Fix sidebar lag"])
    );
    assert_eq!(rank_titles("#", &[t("Fix sidebar lag")]), titles(&[]));
    assert_eq!(rank_titles("´´´", &[t("Fix sidebar lag")]), titles(&[]));
    assert!(score_terms(&fold_query("# fix"), "Fix sidebar", &[], false).is_none());
    assert_eq!(token_ranges("Fix sidebar", "´´´"), vec![]);
    assert!(MessageMatcher::new("´´´").grade("Fix sidebar").is_none());
}

#[test]
fn words_that_only_meet_across_unrelated_fields_are_left_out() {
    let scattered = t("eMudhra certificate update request")
        .pr(22645, Some("fix model picker"))
        .pr(22649, Some("CI speedups"));
    assert!(score("fix ci", &scattered).is_none());
    // The project explains its own words; one field explains the rest.
    let lakebed = t("Weekly notes")
        .project("lakebed")
        .pr(243, Some("perf: faster cold starts"));
    assert_eq!(
        score("lakebed perf", &lakebed).unwrap().hint,
        Some(Hint {
            label: "PR",
            text: "#243 perf: faster cold starts".into(),
            ranges: vec![(5, 9)]
        })
    );
}

#[test]
fn which_field_matched_is_said_when_the_title_doesnt_show_it() {
    let s = t("Trim model lists")
        .branch("flotilla/cleanup-payloads")
        .pr(22889, Some("Smaller MCP payloads"));
    for q in ["#22889", "pr 22889", "pr22889", "pull request #22889"] {
        assert_eq!(
            score(q, &s).unwrap().hint,
            Some(Hint {
                label: "PR",
                text: "#22889 Smaller MCP payloads".into(),
                ranges: vec![(1, 6)]
            }),
            "{q}"
        );
    }
    // Flotilla's own branch prefix isn't part of what the branch says.
    assert_eq!(
        score("cleanup", &s).unwrap().hint,
        Some(Hint {
            label: "Branch",
            text: "cleanup-payloads".into(),
            ranges: vec![(0, 7)]
        })
    );
    assert_eq!(score("trim", &s).unwrap().hint, None);
}

#[test]
fn letters_and_numbers_typed_together_without_typos_in_numbers() {
    let threads = [
        t("Upgrade to pnpm 12"),
        t("GPT-5 eval"),
        t("iOS 26 support"),
        t("Other"),
    ];
    assert_eq!(
        rank_titles("pnpm12", &threads),
        titles(&["Upgrade to pnpm 12"])
    );
    assert_eq!(rank_titles("gpt5", &threads), titles(&["GPT-5 eval"]));
    assert_eq!(rank_titles("ios26", &threads), titles(&["iOS 26 support"]));
    assert_eq!(rank_titles("pnpm13", &threads), titles(&[]));
}

#[test]
fn whole_words_typed_together_but_not_scattered_letters() {
    let threads = [
        t("VS Code extension settings"),
        t("Fix UI bug in sidebar"),
        t("I plan to open source all of lakebed in the next few weeks"),
    ];
    assert_eq!(
        rank_titles("vscode", &threads),
        titles(&["VS Code extension settings"])
    );
    assert_eq!(
        rank_titles("fixuibug", &threads),
        titles(&["Fix UI bug in sidebar"])
    );
    assert_eq!(rank_titles("lint", &threads), titles(&[]));
    // Short words before a mere start are a coincidence.
    assert_eq!(
        rank_titles("inth", &[t("Show skill names in thread overview")]),
        titles(&[])
    );
    assert_eq!(
        rank_titles("tos", &[t("Move Client State to Server")]),
        titles(&[])
    );
    assert_eq!(
        rank_titles("t3co", &[t("T3 Code vs Gemini")]),
        titles(&["T3 Code vs Gemini"])
    );
    assert_eq!(
        rank_titles("\"sidebar lag\"", &[t("Fix sidebar lag")]),
        titles(&["Fix sidebar lag"])
    );
    // A pasted query with a separate accent mark matches the composed title.
    assert_eq!(
        rank_titles("cafe\u{301}", &[t("Café menu")]),
        titles(&["Café menu"])
    );
    // Three letters of the last word are enough, as it's typed.
    assert_eq!(
        rank_titles("gomod", &[t("Migrate to Go modules")]),
        titles(&["Migrate to Go modules"])
    );
    // Joined words beat an earlier match inside a word.
    assert_eq!(
        score("vscode", &t("Fixvscode and VS Code"))
            .unwrap()
            .quality,
        0.6
    );
    // A strong match elsewhere doesn't hide joined words.
    assert!(
        rank_titles(
            "vscode",
            &[
                t("VS Code remote support"),
                t("Old work").branch("vscode").at(NOW - 400 * DAY)
            ]
        )
        .contains(&"VS Code remote support".to_string())
    );
}

#[test]
fn picked_queries_are_kept_without_splitting_a_character() {
    assert_eq!(
        picked_query(&format!("{}🚀", "a".repeat(31))),
        "a".repeat(31)
    );
    assert_eq!(picked_query("  Upl "), "upl");
    let spaced = "investigate    workspace thread reset bug";
    assert_eq!(picked_query(spaced), picked_query(&fold_query(spaced)));
}

#[test]
fn emoji_and_chinese_and_japanese_characters_match() {
    assert_eq!(
        rank_titles("🚀", &[t("🚀 Launch checklist"), t("Launch notes")]),
        titles(&["🚀 Launch checklist"])
    );
    assert!(score("搜索", &t("实现全文搜索")).is_some());
    assert!(score("検索", &t("新しい検索機能")).is_some());
    // Thai and Lao too (Flotilla's: T3 reads them as spaced).
    assert!(score("ไทย", &t("ภาษาไทย")).is_some());
}

#[test]
fn a_whole_word_on_a_quiet_thread_stays_above_a_mid_word_hit_on_one_opened_daily() {
    let mut opens = record_open(None, NOW - 60 * DAY, None);
    for day in (1..=59).rev() {
        opens = record_open(Some(&opens), NOW - day * DAY, None);
    }
    let busy = T {
        opens: Some(opens),
        ..t("Fix oauth redirect").at(NOW)
    };
    assert_eq!(
        rank_titles(
            "auth",
            &[busy, t("Share auth across worktrees").at(NOW - 90 * DAY)]
        ),
        titles(&["Share auth across worktrees", "Fix oauth redirect"])
    );
}

#[test]
fn a_word_and_the_start_of_the_next_typed_together_mark_each_part() {
    let threads = [t("Fix sidebar lag"), t("Unrelated work")];
    assert_eq!(
        rank_titles("fixsid", &threads),
        titles(&["Fix sidebar lag"])
    );
    assert_eq!(
        rank_titles("fixsid", &[t("Feature: Fix sidebar lag")]),
        titles(&["Feature: Fix sidebar lag"])
    );
    assert_eq!(
        score("fixsid", &threads[0]).unwrap().ranges,
        vec![(0, 3), (4, 7)]
    );
    // Initials aren't words typed together.
    assert_eq!(rank_titles("fsl", &threads), titles(&[]));
}

#[test]
fn nothing_rather_than_an_unrelated_thread_when_no_word_matches() {
    let threads = [
        t("Projectless threads without Git"),
        t("Concern about connection setup"),
        t("Simplify worktree thread creation"),
        t("Start a new conversation"),
        t("Setting up the sentinel"),
        t("I plan to open source all of lakebed in the next few weeks"),
        t("Design notes"),
        t("Link pull request to thread"),
    ];
    for q in ["posthog", "cron", "stripe", "gpu", "fxsb", "lint", "deno"] {
        assert_eq!(rank_titles(q, &threads), titles(&[]), "{q}");
    }
}

#[test]
fn pieces_of_references_with_a_number_match() {
    let s = T {
        id: Some("thread-5fe6f59c-4976".into()),
        prs: vec![(
            22889,
            None,
            vec![
                "acme/t3code#22889".into(),
                "https://github.com/acme/t3code/pull/22889".into(),
            ],
        )],
        ..t("Trim model lists")
    };
    for q in [
        "t3code#22889",
        "github.com/acme/t3code/pull/22889",
        "https://github.com/acme/t3code/pull/22889/files",
        "5fe6f59c",
    ] {
        assert!(score(q, &s).is_some(), "{q}");
    }
    assert!(score("github.com", &s).is_none());
    // A number is never cut short.
    for q in ["github.com/acme/t3code/pull/2288", "t3code#2288"] {
        assert!(score(q, &s).is_none(), "{q}");
    }
}

fn rank_messages(matches: &[(&str, f64, u32)], subjects: &[(&str, T)], query: &str) -> Vec<String> {
    let q = fold_query(query);
    let mut ranked: Vec<(String, MessageRank)> = matches
        .iter()
        .map(|(key, quality, hits)| {
            let s = &subjects.iter().find(|(k, _)| k == key).unwrap().1;
            (
                key.to_string(),
                MessageRank {
                    quality: *quality,
                    hits: *hits,
                    at: None,
                    picked_at: MessageRank::picked(s.opens.as_ref(), &q, NOW),
                    boost: usage_boost(s.activity, s.opens.as_ref(), NOW),
                },
            )
        })
        .collect();
    ranked.sort_by(|a, b| compare_messages(&a.1, &b.1));
    ranked.into_iter().map(|(k, _)| k).collect()
}

#[test]
fn messages_rank_by_word_match_then_picks_then_quality_and_use() {
    let subjects = [
        ("a-word", t("A word")),
        ("a-word-2", t("A word too")),
        ("b-prefix", t("B prefix")),
        ("b-recent", t("B recent").at(NOW)),
        (
            "b-picked",
            t("B picked").opened(NOW - DAY, 1., &[("needle", NOW - DAY)]),
        ),
    ];
    let matches = [
        ("a-word", 1., 1),
        ("a-word-2", 1., 1),
        ("b-prefix", 0.8, 1),
        ("b-recent", 1., 1),
        ("b-picked", 0.8, 1),
    ];
    assert_eq!(
        rank_messages(&matches, &subjects, "needle"),
        titles(&["b-picked", "b-recent", "a-word", "a-word-2", "b-prefix"])
    );
}

#[test]
fn a_thread_that_keeps_using_the_word_ranks_above_a_passing_mention() {
    // 21 hits seven weeks back against one this week, opened today.
    let subjects = [
        (
            "mention",
            t("Mention").at(NOW - 3 * DAY).opened(NOW, 1., &[]),
        ),
        ("about", t("About it").at(NOW - 48 * DAY)),
    ];
    assert_eq!(
        rank_messages(
            &[("mention", 1., 1), ("about", 1., 21)],
            &subjects,
            "tailscale"
        ),
        titles(&["about", "mention"])
    );
}

#[test]
fn matches_inside_words_stay_below_word_matches_however_used() {
    let subjects = [
        ("busy", t("Busy").at(NOW).opened(NOW, 20., &[])),
        ("quiet", t("Quiet")),
    ];
    assert_eq!(
        rank_messages(
            &[("busy", MID_WORD, 1), ("quiet", 1., 1)],
            &subjects,
            "rust"
        ),
        titles(&["quiet", "busy"])
    );
}

fn terms(query: &str, name: &str, keywords: &[&str], typos: bool) -> Option<TermsMatch> {
    score_terms(&fold_query(query), name, keywords, typos)
}

#[test]
fn groups_by_their_best_match_keeping_their_order_on_ties() {
    let groups = vec![
        ("actions", vec!["Open settings", "New thread"]),
        ("threads", vec!["Settings sync rewrite", "Thread settings"]),
        ("projects", vec!["Settings app"]),
    ];
    let ranked = rank_groups(groups, |_, item, typos| terms("settings", item, &[], typos));
    let shown: Vec<(&str, Vec<&str>)> = ranked
        .into_iter()
        .map(|(g, r)| (g, r.into_iter().map(|(i, _)| i).collect()))
        .collect();
    assert_eq!(
        shown,
        [
            ("actions", vec!["Open settings"]),
            ("threads", vec!["Settings sync rewrite", "Thread settings"]),
            ("projects", vec!["Settings app"])
        ]
    );
}

#[test]
fn inside_a_word_only_in_thread_titles_and_typos_only_in_names() {
    assert!(terms("arch", "Search project contents", &[], false).is_none());
    assert!(score("arch", &t("Notes").branch("research-extension-system")).is_none());
    assert_eq!(score("arch", &t("Research notes")).unwrap().quality, 0.3);
    let groups = vec![
        (
            "actions",
            vec![("Open settings", vec!["preferences", "keybindings"])],
        ),
        ("settings", vec![("Keybindings", vec!["General"])]),
    ];
    let ranked = rank_groups(groups, |_, (name, kw), typos| {
        terms("keybindngs", name, kw, typos)
    });
    assert_eq!(
        ranked.iter().map(|(g, _)| *g).collect::<Vec<_>>(),
        ["settings"]
    );
    assert!(terms("duckdb", "Search engine", &["duckduckgo"], true).is_none());
    assert!(terms("rust", "GitHub sharing", &["trusted"], false).is_none());
    // Filler words don't make a short query long enough for keyword starts.
    assert!(terms("the ci", "Context window indicator", &["circle"], false).is_none());
}

#[test]
fn a_thread_named_for_the_query_lists_above_a_command_with_it_as_a_keyword() {
    enum Item {
        Command(&'static str, Vec<&'static str>),
        Thread(T),
    }
    struct M(f64, f64, bool, i64, bool, f64);
    impl Ranked for M {
        fn score(&self) -> f64 {
            self.0
        }
        fn quality(&self) -> f64 {
            self.1
        }
        fn strong(&self) -> bool {
            self.2
        }
        fn picked_at(&self) -> i64 {
            self.3
        }
        fn picked_exactly(&self) -> bool {
            self.4
        }
        fn group_quality(&self) -> f64 {
            self.5
        }
    }
    let order = |command: Item, thread: T| -> Vec<&'static str> {
        let groups = vec![
            ("actions", vec![command]),
            ("threads", vec![Item::Thread(thread)]),
        ];
        rank_groups(groups, |_, item, typos| match item {
            Item::Command(name, kw) => terms("mcp", name, kw, typos)
                .map(|m| M(m.quality, m.quality, m.strong, 0, false, m.group_quality)),
            Item::Thread(t) => score_thread("mcp", &t.subject(), NOW, typos, true).map(|m| {
                M(
                    m.score,
                    m.quality,
                    m.strong,
                    m.picked_at,
                    m.picked_exactly,
                    m.group_quality,
                )
            }),
        })
        .into_iter()
        .map(|(g, _)| g)
        .collect()
    };
    let named = t("Trim MCP model lists");
    let picked = t("Trim MCP model lists").opened(NOW, 1., &[("mcp", NOW)]);
    assert_eq!(
        order(
            Item::Command("Restart agent session", vec!["reload", "mcp"]),
            named.clone()
        ),
        ["threads", "actions"]
    );
    // A command named for the query stays first; a pick reorders threads but never their group.
    assert_eq!(
        order(Item::Command("Reload MCP servers", vec![]), named),
        ["actions", "threads"]
    );
    assert_eq!(
        order(Item::Command("Reload MCP servers", vec![]), picked),
        ["actions", "threads"]
    );
}

#[test]
fn opens_decay_quick_reopens_merge_and_the_latest_picks_are_kept() {
    let first = record_open(None, NOW, Some("  Upl "));
    assert_eq!(
        first,
        Opens {
            at: NOW,
            score: 1.,
            picks: vec![SearchPick {
                query: "upl".into(),
                at: NOW
            }]
        }
    );
    let reopened = record_open(Some(&first), NOW + 60_000, Some("uploadthing"));
    assert_eq!(
        reopened,
        Opens {
            at: NOW,
            score: 1.,
            picks: vec![
                SearchPick {
                    query: "uploadthing".into(),
                    at: NOW + 60_000
                },
                SearchPick {
                    query: "upl".into(),
                    at: NOW
                }
            ]
        }
    );
    let later = record_open(Some(&reopened), NOW + HALF_LIFE_MS, Some(&"x".repeat(50)));
    assert_eq!(later.score, 1.5);
    assert_eq!(
        later
            .picks
            .iter()
            .map(|p| p.query.as_str())
            .collect::<Vec<_>>(),
        ["x".repeat(32).as_str(), "uploadthing", "upl"]
    );
    let repicked = record_open(Some(&later), later.at + DAY, Some("upl"));
    assert_eq!(
        repicked.picks,
        vec![
            SearchPick {
                query: "upl".into(),
                at: later.at + DAY
            },
            SearchPick {
                query: "x".repeat(32),
                at: later.at
            },
            SearchPick {
                query: "uploadthing".into(),
                at: NOW + 60_000
            }
        ]
    );
    // Picks that no longer count are dropped when it's opened again.
    let old = Opens {
        at: NOW - 30 * DAY,
        score: 1.,
        picks: vec![SearchPick {
            query: "old".into(),
            at: NOW - 30 * DAY,
        }],
    };
    assert_eq!(
        record_open(Some(&old), NOW, Some("new")).picks,
        vec![SearchPick {
            query: "new".into(),
            at: NOW
        }]
    );
}

#[test]
fn a_querys_pick_isnt_renewed_when_the_thread_is_picked_for_another() {
    let deploy = [
        t("Deploy notes").opened(NOW, 2., &[("scripts", NOW), ("deploy", NOW - 60 * DAY)]),
        t("Deploy timeout").opened(NOW - DAY, 1., &[("deploy", NOW - DAY)]),
    ];
    assert_eq!(
        rank_titles("deploy", &deploy),
        titles(&["Deploy timeout", "Deploy notes"])
    );
}

fn marked(text: &str, ranges: &[(usize, usize)]) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    ranges
        .iter()
        .map(|&(a, b)| chars[a..b].iter().collect())
        .collect()
}

#[test]
fn marks_map_folded_matches_back_and_skip_short_words() {
    assert_eq!(
        token_ranges("Le Café résumé ci", "CAFE resume ci"),
        vec![(3, 7), (8, 14)]
    );
    // A three-letter word only where it starts a word.
    assert_eq!(token_ranges("drafts, enableFTS", "fts"), vec![(14, 17)]);
    assert_eq!(token_ranges("Open handlers.ts", ".ts"), vec![(13, 16)]);
}

fn grade(text: &str, query: &str) -> Option<(f64, usize)> {
    MessageMatcher::new(query)
        .grade(text)
        .map(|m| ((m.quality * 1000.).round() / 1000., m.offset))
}

fn index(text: &str, of: &str) -> usize {
    text[..text.find(of).unwrap()].chars().count()
}

#[test]
fn short_words_must_start_a_word_and_words_inside_words_rank_lower() {
    let text = format!(
        "Saved the drafts. {}Then enableFTS and Enable FTS.",
        "More notes. ".repeat(40)
    );
    let fts = grade(&text, "fts").unwrap();
    assert_eq!(fts.0, 1.);
    assert!(
        text.chars()
            .skip(fts.1)
            .collect::<String>()
            .starts_with("FTS and")
    );
    assert_eq!(grade("saved two drafts", "fts"), None);
    assert_eq!(grade("We trust the build", "rust"), Some((0.3, 4)));
    assert_eq!(grade("Rust and trust", "rust"), Some((1., 0)));
    assert_eq!(grade("Use the sidebar", "\"sidebar\""), Some((1., 8)));
    // Written together as typed ranks above apart, and the snippet starts there.
    let together = "Run the tests, then vp run dev to check";
    assert_eq!(
        grade(together, "vp run dev"),
        Some((1.25, index(together, "vp run dev")))
    );
    // Near each other ranks above far apart.
    assert_eq!(
        grade("dev notes: vp is slow, run it", "vp run dev")
            .unwrap()
            .0,
        1.1
    );
    let far = format!(
        "run the tests. {}Then dev mode, then vp",
        "More notes. ".repeat(10)
    );
    assert_eq!(grade(&far, "vp run dev").unwrap().0, 1.);
    assert_eq!(
        grade("I don\u{2019}t know why", "don't know").unwrap().0,
        1.25
    );
    assert_eq!(grade("Le Café open late", "cafe open").unwrap().0, 1.25);
    assert_eq!(grade("The cafe open late", "café open").unwrap().0, 1.25);
    // The snippet starts where the words meet, not at an early one.
    let phrase = format!(
        "The plan. {}Since the last fix, the sidebar works",
        "More notes. ".repeat(10)
    );
    assert_eq!(
        grade(&phrase, "fix the sidebar").unwrap().1,
        index(&phrase, "fix,")
    );
    let apart = format!(
        "The lakebed app. {}Lakebed page perf is slow",
        "Notes. ".repeat(30)
    );
    assert!(
        apart
            .chars()
            .skip(grade(&apart, "lakebed perf").unwrap().1)
            .collect::<String>()
            .starts_with("Lakebed page")
    );
    // Only where a word counts: not "fix" in "prefix".
    let prefix = format!("fix {}prefix has CI", "unrelated words ".repeat(40));
    assert_eq!(grade(&prefix, "fix ci").unwrap().1, 0);
    // A commit SHA is one word: "cafe" inside one doesn't match, its start does.
    assert_eq!(grade("see commit 7cafe52bb8", "cafe"), None);
    assert_eq!(grade("see commit 7cafe52bb8", "7cafe52"), Some((1., 11)));
    assert_eq!(grade("7cafe52bb8, then the cafe", "cafe").unwrap().0, 1.);
    // A whole word beats a word's start: "mode" prefers "mode" to "model".
    assert_eq!(grade("model settings", "mode").unwrap().0, 0.8);
    assert_eq!(
        grade("The model and the mode switch", "mode"),
        Some((1., 18))
    );
    assert_eq!(grade("v12rc build", "rc").unwrap().0, 1.);
    assert_eq!(grade("Fix the CLIAdapter state", "ada").unwrap().0, 0.8);
    // Words too short for the index must start a word too.
    assert_eq!(grade("Switch off the connection", "switch on"), None);
    assert_eq!(grade("Upgrade pnpm 512 today", "pnpm 12"), None);
    assert_eq!(grade("Upgrade to pnpm12 today", "pnpm 12").unwrap().0, 1.);
    // A word too short to search only narrows; written together still counts.
    assert_eq!(grade("Fix circle rendering", "fix ci").unwrap().0, 1.);
    assert_eq!(grade("Fix CI failures", "fix ci").unwrap().0, 1.25);
    assert_eq!(grade("Upgrade pnpm 123 today", "pnpm 12").unwrap().0, 1.);
    assert_eq!(grade("Rename the file to index.ts", ".ts").unwrap().0, 1.);
}

#[test]
fn words_are_found_through_case_and_accents_also_after_the_same_letters_inside_a_word() {
    assert_eq!(
        grade("This addresses Résumé indexing", "res").unwrap().1,
        15
    );
    assert_eq!(grade("Le CAFÉ ouvert", "cafe"), Some((1., 3)));
    // An accented whole word beats a plain start earlier on.
    assert_eq!(
        grade("The cafeteria, then the Café", "cafe"),
        Some((1., 24))
    );
    assert_eq!(
        grade("I presume this. Résumé indexing", "resume"),
        Some((1., 16))
    );
    assert_eq!(grade("明天部署到生产环境", "部署").unwrap().1, 2);
    // Vietnamese and combining accents fold too.
    assert_eq!(
        grade("Properties: tiếng Việt support", "tie").unwrap().1,
        12
    );
    assert_eq!(
        grade("Properties: tie\u{301}ng support", "tie").unwrap().1,
        12
    );
    // A letter that grows when lower-cased doesn't shift later places.
    assert_eq!(grade("İ Enable fts and model", "fts").unwrap().1, 9);
    assert_eq!(grade("İ Enable fts and model", "model").unwrap().0, 1.);
    assert_eq!(grade("Trip to İstanbul", "İstanbul"), Some((1., 8)));
}

#[test]
fn message_tokens_split_long_from_short() {
    let tokens = message_tokens("“Retry” the FTS ok 部署 x");
    assert_eq!(tokens.long, ["retry”", "the", "fts", "部署"]);
    assert_eq!(tokens.short, ["ok", "x"]);
    assert!(!finds_messages("df -h ls") && finds_messages("retry") && finds_messages("部署"));
    assert_eq!(message_tokens("don’t").long, ["don't"]);
}

fn snip(text: &str, word: &str) -> String {
    snippet(&text.chars().collect::<Vec<_>>(), index(text, word))
}

#[test]
fn snippets_start_a_few_words_before_the_match_so_one_line_shows_it() {
    let long = format!(
        "{}the NEEDLE sits here {}",
        "lead ".repeat(60),
        "tail ".repeat(60)
    );
    let cut = snip(&long, "NEEDLE");
    assert!(cut.chars().count() <= SNIPPET + 2);
    assert!(
        cut.starts_with("…lead ")
            && cut.contains("lead the NEEDLE sits here")
            && cut.ends_with('…'),
        "{cut}"
    );
    let lead = cut.split("the NEEDLE").next().unwrap();
    assert!((2..=4).contains(&lead.matches("lead ").count()), "{cut}");
    // Whitespace runs together only in what's shown, so the place still lands.
    assert!(snip(&format!("{}un café", "word\n\n  ".repeat(60)), "café").ends_with("un café"));
    // Emphasis and code marks go, as the thread shows the text; underscores stay.
    assert_eq!(
        snippet(
            &"Use **bold** and `code` here".chars().collect::<Vec<_>>(),
            0
        ),
        "Use bold and code here"
    );
    assert_eq!(
        snippet(&"Call __init__ first".chars().collect::<Vec<_>>(), 0),
        "Call __init__ first"
    );
    // Short text still moves a late match into view.
    assert_eq!(
        snip(&format!("{}needle", "word ".repeat(20)), "needle"),
        "…word word word word needle"
    );
    // A megabyte command still gives one short line.
    let huge = format!(
        "cat > big.txt <<'EOF'\n{}\nneedle\nEOF",
        "x".repeat(1 << 20)
    );
    let line = snip(&huge, "needle");
    assert!(
        line.chars().count() <= SNIPPET + 2 && line.contains("needle"),
        "{line}"
    );
}

#[test]
fn marks_are_byte_ranges_on_characters() {
    let line = "…é ✓ the retry loop";
    let r = token_ranges(line, "retry");
    assert_eq!(marked(line, &r), ["retry"]);
    assert_eq!(byte_ranges(line, &r), vec![[14, 19]]);
}

#[test]
fn a_hit_goes_through_json() {
    let hit = SearchHit {
        thread: "t1".into(),
        title: "Fix the retry loop".into(),
        kind: HitKind::Text,
        seq: Some(1234),
        snippet: "…the retry loop".into(),
        marks: vec![[7, 12]],
        at: Some(1_793_300_000_000),
        from: Some("From Review the retry loop".into()),
        hits: 3,
    };
    let json = serde_json::to_value(&hit).unwrap();
    assert_eq!(
        (json["kind"].as_str(), json["hits"].as_u64()),
        (Some("text"), Some(3))
    );
    assert_eq!(serde_json::from_value::<SearchHit>(json).unwrap(), hit);
    let title: SearchHit = serde_json::from_value(
        serde_json::json!({"thread": "t", "title": "T", "kind": "title", "snippet": "T"}),
    )
    .unwrap();
    assert_eq!(
        (
            title.seq,
            title.marks.len(),
            title.at,
            title.from.as_deref(),
            title.hits
        ),
        (None, 0, None, None, 0)
    );
    let text = serde_json::to_string(&title).unwrap();
    assert!(
        !text.contains("from") && !text.contains("hits"),
        "only sent when there is one"
    );
}

#[test]
fn folding_keeps_one_character_for_one() {
    for (c, f) in [
        ('É', 'e'),
        ('ß', 'ß'),
        ('ﬁ', 'ﬁ'),
        ('İ', 'i'),
        ('ς', 'σ'),
        ('’', '\''),
        ('Ł', 'l'),
        ('Ø', 'o'),
        ('Ａ', 'a'),
        ('🚀', '🚀'),
    ] {
        assert_eq!(fold(c), f, "{c}");
    }
    assert_eq!(fold_query("  “Fix   the CAFÉ” "), "fix the cafe");
    assert_eq!(fold_query("\"\""), "");
}

/// A control character (a NUL) separates words and, at either end, comes off with the
/// whitespace before any quotes do, in titles and messages alike (as Swift's `unquoted`).
#[test]
fn control_characters_separate_words_and_come_off_before_quotes() {
    assert_eq!(fold_query("Retry\0Loop"), "retry loop");
    assert_eq!(fold_query("\0\"retry\"\0"), "retry");
    assert_eq!(message_tokens("\0\"retry\"\0").words, ["retry"]);
    assert_eq!(
        message_tokens("retry\0loop\u{1}x").words,
        ["retry", "loop", "x"]
    );
}
