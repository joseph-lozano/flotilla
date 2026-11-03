//! The local SQLite cache. boat is the source of truth, except for `sent_prompts` (what the
//! user actually typed) and `events` after a VM's disk expires, where this may be the only copy.

use crate::boat;
use crate::error::{Error, Result};
use crate::event::{RawEvent, is_terminal_status};
use crate::github::{Check, PullRequest, SetupScript};
use crate::normalize::{self, MESSAGES_VERSION, Message};
use crate::util::now_iso;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MIGRATIONS: &[&str] = &[
    // 1: the M1 schema. `messages` is derived and lives outside migrations (see ensure_messages).
    r#"
    CREATE TABLE projects (
      id TEXT PRIMARY KEY,               -- boat environment id
      name TEXT NOT NULL,                -- what sandbox create takes
      is_default INTEGER NOT NULL DEFAULT 0,
      updated_at TEXT NOT NULL);

    CREATE TABLE threads (
      id TEXT PRIMARY KEY,               -- uuid; also label flotilla.thread and the create Idempotency-Key
      project_id TEXT, environment_name TEXT, title TEXT NOT NULL,
      sandbox_id TEXT UNIQUE, conversation_id TEXT,
      provider TEXT NOT NULL, model TEXT NOT NULL, effort TEXT, fast INTEGER NOT NULL DEFAULT 0,
      state TEXT NOT NULL,               -- creating|starting|idle|running|stopping|stopped|expired|error
      keep_running INTEGER NOT NULL DEFAULT 0,
      archive_after TEXT, last_stopped_at TEXT, data_expired_at TEXT,
      created_at TEXT NOT NULL, updated_at TEXT NOT NULL);

    -- Acceptance test 8: settings can't change after creation.
    CREATE TRIGGER threads_settings_immutable BEFORE UPDATE OF provider, model, effort, fast ON threads
    WHEN NEW.provider IS NOT OLD.provider OR NEW.model IS NOT OLD.model
      OR NEW.effort IS NOT OLD.effort OR NEW.fast IS NOT OLD.fast
    BEGIN SELECT RAISE(ABORT, 'provider, model, effort and fast are fixed at thread creation'); END;

    -- Stable, nearly schemaless. NEVER wipe it to rebuild from boat: after disk expiry it may be the only copy.
    CREATE TABLE events (
      thread_id TEXT NOT NULL, key TEXT NOT NULL,   -- key = taskId for prompt events, else id
      event_id TEXT NOT NULL, task_id TEXT, type TEXT NOT NULL, timestamp INTEGER NOT NULL,
      raw_json TEXT NOT NULL, settled INTEGER NOT NULL DEFAULT 0, received_at TEXT NOT NULL,
      PRIMARY KEY (thread_id, key));

    CREATE TABLE turns (
      thread_id TEXT NOT NULL, prompt_id TEXT NOT NULL, status TEXT NOT NULL,
      done INTEGER NOT NULL DEFAULT 0, settled INTEGER NOT NULL DEFAULT 0, done_at TEXT, created_at TEXT NOT NULL,
      PRIMARY KEY (thread_id, prompt_id));

    -- What the user actually typed (decision a). Local-only; can't be rebuilt from boat.
    CREATE TABLE sent_prompts (
      thread_id TEXT NOT NULL, id TEXT NOT NULL,    -- promptId or steer id
      kind TEXT NOT NULL,                           -- prompt|steer
      text TEXT NOT NULL, sent_at TEXT NOT NULL,
      PRIMARY KEY (thread_id, id));

    CREATE TABLE sync_state (
      thread_id TEXT PRIMARY KEY,
      cursor TEXT,                       -- at the last settled event
      etag TEXT,                         -- for the first page after cursor
      synced_at TEXT);

    CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
    "#,
    // 2: when this Mac last showed each thread, for "Waiting on you". Local-only.
    r#"
    CREATE TABLE thread_views (thread_id TEXT PRIMARY KEY, viewed_at TEXT NOT NULL);
    "#,
    // 3: archived threads (sorted to the bottom of the sidebar). Local-only.
    r#"
    CREATE TABLE thread_archive (thread_id TEXT PRIMARY KEY, archived_at TEXT NOT NULL);
    "#,
    // 4: model-written title and reply summaries. `source` is what was summarised (the
    // reply's event key), so a stale summary is never shown for a newer reply. Local-only.
    r#"
    CREATE TABLE summaries (
      thread_id TEXT NOT NULL, kind TEXT NOT NULL,  -- title|reply
      source TEXT NOT NULL, text TEXT NOT NULL, created_at TEXT NOT NULL,
      PRIMARY KEY (thread_id, kind));
    "#,
    // 5: "project" now means a GitHub repo. The old `projects` (boat environments) become
    // `environments`. Projects keep their settings and setup scripts here (local only), plus
    // the prepared VM new threads copy, each setup run, and each thread's branch and PR.
    r#"
    ALTER TABLE projects RENAME TO environments;
    ALTER TABLE threads RENAME COLUMN project_id TO environment_id;
    CREATE TABLE projects (
      id TEXT PRIMARY KEY,               -- uuid
      owner TEXT NOT NULL, repo TEXT NOT NULL,
      default_branch TEXT NOT NULL,
      environment_name TEXT NOT NULL,    -- the project's own boat environment (lists the repo)
      created_at TEXT NOT NULL,
      -- The prepared VM new threads copy: stopped, set up at template_sha. Internal only.
      template_sandbox_id TEXT, template_sha TEXT, template_built_at TEXT,
      prev_template_sandbox_id TEXT,
      -- The default branch's head on GitHub, and the ETag of that answer.
      head_sha TEXT, head_etag TEXT, checked_at TEXT,
      UNIQUE (owner, repo));
    CREATE TABLE project_scripts (
      project_id TEXT NOT NULL, position INTEGER NOT NULL,
      command TEXT NOT NULL,
      fresh_only INTEGER NOT NULL DEFAULT 0,  -- runs only when set up from scratch
      PRIMARY KEY (project_id, position));
    CREATE TABLE setup_runs (
      id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
      kind TEXT NOT NULL,                -- update|fresh
      trigger TEXT NOT NULL,             -- added|main|merge|manual|scripts|refresh
      sha TEXT NOT NULL,
      sandbox_id TEXT,
      state TEXT NOT NULL,               -- waiting|running|done|failed|cancelled
      step TEXT,                         -- the stage running, or the one that failed
      error TEXT,
      log TEXT NOT NULL DEFAULT '',
      started_at TEXT NOT NULL, finished_at TEXT);
    ALTER TABLE threads ADD COLUMN repo_project_id TEXT;
    ALTER TABLE threads ADD COLUMN branch TEXT;
    ALTER TABLE threads ADD COLUMN base_sha TEXT;
    CREATE TABLE thread_prs (
      thread_id TEXT PRIMARY KEY,
      number INTEGER NOT NULL, url TEXT NOT NULL, title TEXT NOT NULL,
      state TEXT NOT NULL,               -- OPEN|CLOSED|MERGED
      is_draft INTEGER NOT NULL DEFAULT 0,
      mergeable TEXT, review TEXT, checks_json TEXT,
      additions INTEGER, deletions INTEGER,
      linked_by TEXT NOT NULL DEFAULT 'auto',  -- auto|user
      checked_at TEXT NOT NULL);
    "#,
    // 6: a pull request's branches, for the prompts that ask the agent to fix it.
    r#"
    ALTER TABLE thread_prs ADD COLUMN head TEXT;
    ALTER TABLE thread_prs ADD COLUMN base TEXT;
    "#,
    // 7: when a pull request last changed on GitHub, for reading quiet ones less often.
    r#"
    ALTER TABLE thread_prs ADD COLUMN updated_at TEXT;
    "#,
    // 8: the files sent with a prompt (JSON: path, name and size; never their contents).
    r#"
    ALTER TABLE sent_prompts ADD COLUMN attachments TEXT;
    "#,
    // 9: the client-made ids of requests already acted on (`thread.new`'s thread id,
    // `thread.send`'s `local-` id), so one sent again is a no-op even after a restart.
    r#"
    CREATE TABLE receipts (id TEXT PRIMARY KEY, at TEXT NOT NULL);
    "#,
    // 10: the devices paired with this server and the codes that pair them (`devices`): only
    // the hashes of tokens and codes, never them. Times are ms.
    r#"
    CREATE TABLE devices (
      id TEXT PRIMARY KEY, token_hash TEXT NOT NULL,
      name TEXT NOT NULL, platform TEXT NOT NULL,
      created_ms INTEGER NOT NULL, last_seen_ms INTEGER NOT NULL, last_ip TEXT);
    CREATE TABLE pairing_codes (
      id TEXT PRIMARY KEY, code_hash TEXT NOT NULL UNIQUE,
      made_by TEXT NOT NULL,             -- cli|server|<device id>
      created_ms INTEGER NOT NULL, expires_ms INTEGER NOT NULL,
      consumed_ms INTEGER, revoked_ms INTEGER,
      device_id TEXT);                   -- the device it paired
    "#,
    // 11: thread titles for search (`crate::search`), kept by triggers whoever writes `threads`.
    // Keyed by the thread's id, not its rowid: `INSERT OR REPLACE` and VACUUM can't strand one.
    // Curly apostrophes go straight, as `search::fold` makes a query's.
    r#"
    CREATE VIRTUAL TABLE thread_search USING fts5(thread_id UNINDEXED, title, tokenize='trigram remove_diacritics 1');
    INSERT INTO thread_search (thread_id, title) SELECT id, replace(replace(title, '’', ''''), '‘', '''') FROM threads;
    CREATE TRIGGER thread_search_insert AFTER INSERT ON threads BEGIN
      DELETE FROM thread_search WHERE thread_id = new.id;
      INSERT INTO thread_search (thread_id, title) VALUES (new.id, replace(replace(new.title, '’', ''''), '‘', ''''));
    END;
    CREATE TRIGGER thread_search_title AFTER UPDATE OF title ON threads WHEN old.title IS NOT new.title BEGIN
      DELETE FROM thread_search WHERE thread_id = old.id;
      INSERT INTO thread_search (thread_id, title) VALUES (new.id, replace(replace(new.title, '’', ''''), '‘', ''''));
    END;
    CREATE TRIGGER thread_search_delete AFTER DELETE ON threads BEGIN
      DELETE FROM thread_search WHERE thread_id = old.id;
    END;
    "#,
    // 12: how often and recently each thread was opened, and what for from a search
    // (`flotilla_proto::search::Opens`; `picks` is its JSON), for ranking search.
    "CREATE TABLE thread_opens (thread_id TEXT PRIMARY KEY, opened_at INTEGER NOT NULL, score REAL NOT NULL, picks TEXT NOT NULL DEFAULT '[]');",
    // 13: what the agents spent (`crate::spend`): each hour's tokens by harness, model and
    // speed (`reported`: the harness gave the requests' cost, summed in `reported_usd`), and a
    // hash of every request counted, so none is counted twice. Kept 92 days; a deleted
    // thread's stay (they were spent). `IF NOT EXISTS`: `dev-db.py --agent-usage` may have
    // made them already.
    r#"
    CREATE TABLE IF NOT EXISTS agent_usage (
      hour INTEGER NOT NULL,             -- ms since the epoch, on the hour (UTC)
      provider TEXT NOT NULL, model TEXT NOT NULL, speed TEXT NOT NULL, reported INTEGER NOT NULL,
      input INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
      output INTEGER NOT NULL, reasoning INTEGER NOT NULL, requests INTEGER NOT NULL, reported_usd REAL NOT NULL,
      PRIMARY KEY (hour, provider, model, speed, reported));
    CREATE TABLE IF NOT EXISTS agent_usage_seen (key INTEGER PRIMARY KEY, at INTEGER NOT NULL);
    CREATE INDEX IF NOT EXISTS agent_usage_seen_at ON agent_usage_seen (at);
    "#,
    // 14: scheduled prompts (`flotilla_proto::schedule`): what was set, as JSON where it's a
    // wire type, and how its runs go. `failing`: why its runs fail while they do, so that's
    // said once. `plan_ms`: when its timing or pausing last changed (an edit of its prompt or
    // target leaves it), so a run under way can tell whether it was called off. A prompt a
    // schedule sent names it. Times are ms.
    r#"
    CREATE TABLE schedules (
      id TEXT PRIMARY KEY,
      prompt TEXT NOT NULL,
      target_json TEXT NOT NULL,
      thread_id TEXT,                    -- the thread it sends to, if one
      when_json TEXT NOT NULL,
      zone TEXT NOT NULL,
      paused INTEGER NOT NULL DEFAULT 0,
      next_ms INTEGER, waiting_ms INTEGER,
      last_json TEXT, runs INTEGER NOT NULL DEFAULT 0, failing TEXT,
      made_by TEXT NOT NULL DEFAULT '',
      created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, plan_ms INTEGER NOT NULL DEFAULT 0);
    CREATE INDEX schedules_thread ON schedules (thread_id);
    ALTER TABLE sent_prompts ADD COLUMN schedule TEXT;
    "#,
    // 15: what each paired device may do (`flotilla_proto::Scope`: `full`, or `agent` for an
    // assistant through `flotilla mcp`), and what a pairing code lets the device it pairs do.
    r#"
    ALTER TABLE devices ADD COLUMN scope TEXT NOT NULL DEFAULT 'full';
    ALTER TABLE pairing_codes ADD COLUMN scope TEXT NOT NULL DEFAULT 'full';
    "#,
    // 16: what the agents spent, by context tier too (`crate::prices::RateTable::tier`: the
    // threshold past which a request's prompt is at higher rates; 0 for none), and how many of
    // the cache writes were kept an hour (some of `cache_write`). A key can't be changed in
    // place: the table is made again, its rows at tier 0 and none kept an hour.
    r#"
    CREATE TABLE agent_usage_16 (
      hour INTEGER NOT NULL,             -- ms since the epoch, on the hour (UTC)
      provider TEXT NOT NULL, model TEXT NOT NULL, speed TEXT NOT NULL, reported INTEGER NOT NULL,
      tier INTEGER NOT NULL DEFAULT 0,
      input INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
      cache_write_1h INTEGER NOT NULL DEFAULT 0,
      output INTEGER NOT NULL, reasoning INTEGER NOT NULL, requests INTEGER NOT NULL, reported_usd REAL NOT NULL,
      PRIMARY KEY (hour, provider, model, speed, reported, tier));
    INSERT INTO agent_usage_16 (hour, provider, model, speed, reported, input, cache_read, cache_write, output, reasoning, requests, reported_usd)
      SELECT hour, provider, model, speed, reported, input, cache_read, cache_write, output, reasoning, requests, reported_usd FROM agent_usage;
    DROP TABLE agent_usage;
    ALTER TABLE agent_usage_16 RENAME TO agent_usage;
    "#,
];

/// `messages` and its search index (`crate::search`), derived from `events` together: both are
/// made again whenever [`MESSAGES_VERSION`] changes. A row keeps its `id` as it's rewritten
/// (and through VACUUM, which can renumber implicit rowids), and the index is keyed by it, kept
/// in step by triggers whoever writes (the store, `dev-db.py`, a test).
fn messages_ddl() -> String {
    let (new, old) = (
        crate::search::indexed_sql("new"),
        crate::search::indexed_sql("old"),
    );
    format!(
        r#"
CREATE TABLE messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT, -- message_search's rowid, never reused
  thread_id TEXT NOT NULL, seq INTEGER NOT NULL, task_id TEXT,
  kind TEXT NOT NULL,                  -- prompt|steer|text|tool|notice
  text TEXT, status TEXT, model TEXT,
  tool_name TEXT, tool_summary TEXT, tool_input_json TEXT, tool_result_json TEXT, tool_result_type TEXT,
  is_error INTEGER NOT NULL DEFAULT 0, pending INTEGER NOT NULL DEFAULT 0, event_key TEXT, tool_id TEXT,
  timestamp INTEGER,                   -- ms; the source event's, or sent_at for local-only rows
  attachments TEXT,                    -- a prompt's file names, as a JSON array
  mark INTEGER,                        -- flotilla_proto::history::message_mark, for page marks
  answer_to TEXT,                      -- a prompt or steer that took your answer: the ask's id
  from_flotilla INTEGER NOT NULL DEFAULT 0, -- a prompt or steer of Flotilla's own (`[flotilla] …`)
  UNIQUE (thread_id, seq));
-- Trigrams: any run of three or more characters, ignoring case and accents. Contentless: what
-- it matched is read back from `messages` (crate::search::body_sql).
CREATE VIRTUAL TABLE message_search USING fts5(body, content='', contentless_delete=1, tokenize='trigram remove_diacritics 1');
CREATE TRIGGER message_search_insert AFTER INSERT ON messages BEGIN
  INSERT INTO message_search (rowid, body) SELECT new.id, {new} WHERE {new} IS NOT NULL;
END;
-- A row rewritten with the same text (a tool call finishing, a status changing) keeps its entry.
CREATE TRIGGER message_search_update AFTER UPDATE ON messages WHEN old.id IS NOT new.id OR {old} IS NOT {new} BEGIN
  DELETE FROM message_search WHERE rowid = old.id;
  INSERT INTO message_search (rowid, body) SELECT new.id, {new} WHERE {new} IS NOT NULL;
END;
CREATE TRIGGER message_search_delete AFTER DELETE ON messages BEGIN
  DELETE FROM message_search WHERE rowid = old.id;
END;
{words}"#,
        words = words_ddl()
    )
}

/// The word index beside the trigram one: words, ignoring case, accents and the punctuation
/// between them, so a search finds the words as typed, next to each other (`needle, haystack`
/// for "needle haystack"), however old (`crate::search`). Kept by triggers as `message_search`
/// is; made from `messages` when a database from before it has none.
fn words_ddl() -> String {
    let (new, old) = (
        crate::search::indexed_sql("new"),
        crate::search::indexed_sql("old"),
    );
    format!(
        r#"
CREATE VIRTUAL TABLE IF NOT EXISTS message_words USING fts5(body, content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS message_words_insert AFTER INSERT ON messages BEGIN
  INSERT INTO message_words (rowid, body) SELECT new.id, {new} WHERE {new} IS NOT NULL;
END;
CREATE TRIGGER IF NOT EXISTS message_words_update AFTER UPDATE ON messages WHEN old.id IS NOT new.id OR {old} IS NOT {new} BEGIN
  DELETE FROM message_words WHERE rowid = old.id;
  INSERT INTO message_words (rowid, body) SELECT new.id, {new} WHERE {new} IS NOT NULL;
END;
CREATE TRIGGER IF NOT EXISTS message_words_delete AFTER DELETE ON messages BEGIN
  DELETE FROM message_words WHERE rowid = old.id;
END;
"#
    )
}

pub use flotilla_proto::{
    Activity, Environment, Project, SentPrompt, SetupRun, Thread, ThreadState, ThreadSummary,
};

/// A `devices` row as it's stored (id, token hash, name, platform, created, last seen, IP,
/// scope).
type DeviceRaw = (
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    String,
);

/// What `Store::import` brought.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub threads: usize,
    pub projects: usize,
    /// The Flotilla id the database had, adopted.
    pub adopted: Option<String>,
    /// Where the database that was there went.
    pub replaced: Option<PathBuf>,
    /// This Flotilla's id, and the address it gives devices if it has one: where the source
    /// moved to.
    pub server: String,
    pub url: Option<String>,
}

/// What a rebuild carries into the new database.
struct Carried {
    devices: Vec<DeviceRaw>,
    kv: Vec<(String, String)>,
}

/// The kv entries a rebuild keeps: the server's id (its VMs carry it) and the ids it adopted,
/// the URL it gives out, and whether Tailscale serves it.
const CARRIED_KV: &[&str] = &[
    crate::owner::SERVER_ID_KEY,
    crate::owner::ADOPTED_KEY,
    "server_url",
    crate::tailscale::KEY,
    // How notifications go: the relay (or `off`), and the APNs key's ids (the key is a secret).
    crate::push::RELAY_KEY,
    crate::push::KEY_ID_KEY,
    crate::push::TEAM_ID_KEY,
];

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub key: String,
    pub event_id: String,
    pub settled: bool,
    pub event: RawEvent,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub thread_id: String,
    pub prompt_id: String,
    pub status: String,
    pub done: bool,
    pub settled: bool,
    pub done_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default)]
pub struct SyncState {
    pub cursor: Option<String>,
    pub etag: Option<String>,
    pub synced_at: Option<String>,
}

/// The first non-empty line of a reply, without Markdown heading or list markers.
fn removing_key(project_id: &str) -> String {
    format!("removing:{project_id}")
}

/// Set while a project's forgotten prepared VM hasn't been set up again.
fn template_gone_key(project_id: &str) -> String {
    format!("template_gone:{project_id}")
}

fn start_error_key(thread_id: &str) -> String {
    format!("start_error:{thread_id}")
}

pub use flotilla_model::util::preview_line;

#[cfg(test)]
mod opening {
    use super::*;

    fn db(name: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("flotilla-opening-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("flotilla.db"), dir)
    }

    /// Migration 16: what the agents spent before keeps its rows, at tier 0 with no hour-long
    /// cache writes (so at the base rates); a request since in the same hour below a tier adds
    /// to it, and one past a tier is a row of its own.
    #[test]
    fn spend_kept_before_tiers_stays_at_the_base_rates() {
        let (path, dir) = db("spend16");
        let hour = 1_793_538_000_000_i64;
        {
            let conn = Connection::open(&path).unwrap();
            for sql in &MIGRATIONS[..15] {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", 15).unwrap();
            conn.execute(
                "INSERT INTO agent_usage VALUES (?1, 'claude-code', 'claude-haiku-5-5', 'standard', 0, 2000, 0, 1000, 500, 0, 4, 0)",
                [hour],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let v: i64 = store
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 16);
        let kept: (i64, i64, i64, i64) = store
            .conn()
            .query_row(
                "SELECT tier, cache_write, cache_write_1h, requests FROM agent_usage",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(kept, (0, 1000, 0, 4));
        let rates = crate::prices::RateTable::parse(
            &serde_json::from_str(
                &std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/testdata/prices/litellm.json"
                ))
                .unwrap(),
            )
            .unwrap(),
        );
        let rec = |key: &str, input: u64, write_1h: u64| crate::spend::Record {
            provider: "claude-code",
            at_ms: hour + 60_000,
            model: "claude-haiku-5-5".into(),
            tokens: flotilla_proto::TokenTotals {
                input,
                cache_write: write_1h,
                output: 10,
                ..Default::default()
            },
            cache_write_1h: write_1h,
            speed: flotilla_proto::Speed::Standard,
            reported_usd: None,
            key: key.into(),
        };
        let now = hour + 3_600_000;
        assert_eq!(
            crate::spend::keep(
                &store,
                "t1",
                &[rec("a", 100, 50), rec("b", 120_000, 0)],
                &crate::spend::Cursor::default(),
                &rates,
                now
            )
            .unwrap(),
            2
        );
        let view = crate::spend::view(&store, &rates, None, now).unwrap();
        let rows: Vec<(u64, u64, u64, f64)> = view
            .hours
            .iter()
            .map(|h| {
                (
                    h.requests,
                    h.tokens.input,
                    h.tokens.cache_write,
                    h.cost.unwrap().total(),
                )
            })
            .collect();
        assert_eq!(
            rows.iter().map(|r| (r.0, r.1, r.2)).collect::<Vec<_>>(),
            [(5, 2_100, 1_050), (1, 120_000, 0)]
        );
        // The old row's writes at five minutes' $0.125 a million, the new one's at an hour's $0.20.
        let base = 2_100. * 0.1e-6 + 1_000. * 0.125e-6 + 50. * 0.2e-6 + 510. * 0.5e-6;
        assert!((rows[0].3 - base).abs() < 1e-12, "{rows:?}");
        assert!(
            (rows[1].3 - (120_000. * 0.5e-6 + 10. * 2.5e-6)).abs() < 1e-12,
            "{rows:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Read only, beside a process that has the database: nothing is made, migrated or rebuilt,
    /// and a database at another version is refused rather than read.
    #[test]
    fn reading_only_changes_nothing_and_refuses_another_version() {
        let (path, dir) = db("read");
        let store = Store::open(&path).unwrap();
        store
            .insert_thread(&crate::search::tests_thread("t1", "Fix the retry"))
            .unwrap();
        let reader = Store::open_read_only(&path).unwrap();
        assert_eq!(reader.threads().unwrap().len(), 1);
        assert!(reader.kv_set("x", "y").is_err());
        // Another version's messages: refused, and left as they are.
        store.kv_set("messages_version", "0").unwrap();
        assert!(Store::open_read_only(&path).is_err());
        assert_eq!(
            store.kv_get("messages_version").unwrap().as_deref(),
            Some("0")
        );
        store
            .kv_set("messages_version", &MESSAGES_VERSION.to_string())
            .unwrap();
        // A database not yet migrated as far: refused, and not migrated.
        store
            .conn()
            .pragma_update(None, "user_version", (MIGRATIONS.len() - 1) as i64)
            .unwrap();
        assert!(Store::open_read_only(&path).is_err());
        let v: i64 = store
            .conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, (MIGRATIONS.len() - 1) as i64);
        // None there: none made.
        assert!(Store::open_read_only(&dir.join("none.db")).is_err());
        assert!(!dir.join("none.db").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Opening a database whose messages are another version's, from several connections at
    /// once: none fails, and none comes back before every thread's messages are there again,
    /// so none reads a thread's history half made.
    #[test]
    fn a_messages_rebuild_from_many_connections_at_once_is_whole_for_each() {
        let (path, dir) = db("rebuild");
        let store = Store::open(&path).unwrap();
        let mut want = std::collections::HashMap::new();
        for t in 0..40 {
            let id = format!("t{t}");
            store
                .insert_thread(&crate::search::tests_thread(&id, "A thread"))
                .unwrap();
            let events: Vec<RawEvent> = (0..10)
                .flat_map(|k| {
                    let ts = 1_793_000_000_000i64 + k * 10_000;
                    [
                        RawEvent::from_value(serde_json::json!({"id": format!("p{k}"), "taskId": format!("task-{k}"), "type": "prompt", "timestamp": ts, "conversationId": "c", "data": {"prompt": format!("prompt {k}"), "status": "finished"}})),
                        RawEvent::from_value(serde_json::json!({"id": format!("r{k}"), "taskId": format!("task-{k}"), "type": "response", "timestamp": ts + 1000, "conversationId": "c", "data": {"content": format!("reply {k}"), "model": "m"}})),
                    ]
                })
                .collect();
            store.apply_events(&id, &events, false).unwrap();
            store.rebuild_messages(&id).unwrap();
            want.insert(id.clone(), store.messages(&id).unwrap().len());
        }
        assert!(want.values().all(|n| *n > 0));
        for round in 0..3 {
            store.kv_set("messages_version", "0").unwrap();
            let opened: Vec<_> = (0..4)
                .map(|_| {
                    let (path, want) = (path.clone(), want.clone());
                    std::thread::spawn(move || {
                        let s = Store::open(&path).map_err(|e| e.to_string())?;
                        let short: Vec<String> = want
                            .iter()
                            .filter(|(id, n)| s.messages(id).map(|m| m.len()).unwrap_or(0) != **n)
                            .map(|(id, _)| id.clone())
                            .collect();
                        if short.is_empty() {
                            Ok(())
                        } else {
                            Err(format!("half made: {short:?}"))
                        }
                    })
                })
                .collect();
            for o in opened {
                assert_eq!(o.join().unwrap(), Ok(()), "round {round}");
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Two processes opening an old database at once (a new app and CLI, say) both upgrade it
    /// without either failing on a step the other ran; a new one too.
    #[test]
    fn upgrading_from_many_connections_at_once_runs_each_step_once() {
        for round in 0..10 {
            let (path, dir) = db(&format!("race{round}"));
            // Half the time there already, from before any migration; half the time new.
            if round % 2 == 0 {
                Connection::open(&path)
                    .unwrap()
                    .pragma_update(None, "journal_mode", "WAL")
                    .unwrap();
            }
            let opened: Vec<_> = (0..6)
                .map(|_| {
                    std::thread::spawn({
                        let path = path.clone();
                        move || Store::open(&path).map(|_| ()).map_err(|e| e.to_string())
                    })
                })
                .collect();
            for o in opened {
                assert_eq!(o.join().unwrap(), Ok(()));
            }
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod schedule_tests {
    use flotilla_proto::{Run, RunOutcome, Schedule, Target, When};

    fn schedule(id: &str, next: Option<i64>) -> Schedule {
        Schedule {
            id: id.into(),
            prompt: "Check CI".into(),
            target: Target::Thread("t1".into()),
            when: When::Daily { at: 540 },
            zone: "UTC".into(),
            paused: false,
            next_ms: next,
            waiting_ms: None,
            last: None,
            runs: 0,
            from: "MacBook".into(),
            created_ms: 1,
            updated_ms: 1,
        }
    }

    /// Kept and read back; runs noted without undoing an edit made meanwhile; listed soonest
    /// first, then paused, then done; gone with their thread.
    #[test]
    fn schedules_keep_what_was_set_and_how_their_runs_went() {
        let store = super::Store::open_in_memory().unwrap();
        store
            .insert_thread(&crate::search::tests_thread("t1", "Nightly"))
            .unwrap();
        store
            .put_schedule(&schedule("late", Some(300)), true)
            .unwrap();
        store
            .put_schedule(&schedule("soon", Some(200)), true)
            .unwrap();
        store
            .put_schedule(
                &Schedule {
                    paused: true,
                    next_ms: None,
                    ..schedule("paused", None)
                },
                true,
            )
            .unwrap();
        let once = Schedule {
            when: When::Once { at_ms: 50 },
            target: Target::Thread("t2".into()),
            ..schedule("done", None)
        };
        store.put_schedule(&once, true).unwrap();
        let ids = |store: &super::Store| {
            store
                .schedules()
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&store), ["soon", "late", "paused", "done"]);
        assert_eq!(
            store.schedule("soon").unwrap().unwrap(),
            schedule("soon", Some(200))
        );
        // A run: noted, counted, its next time written as read.
        let sent = Run {
            at_ms: 200,
            outcome: RunOutcome::Sent,
            thread: Some("t1".into()),
            why: None,
        };
        store.record_run("soon", &sent, true, None).unwrap();
        assert!(store.set_schedule_next("soon", Some(400), None, 1).unwrap());
        // Retimed meanwhile (a new updated_ms, and a new plan): the edit's next time stands.
        store
            .put_schedule(
                &Schedule {
                    prompt: "Check CI and docs".into(),
                    next_ms: Some(250),
                    updated_ms: 2,
                    ..schedule("late", None)
                },
                true,
            )
            .unwrap();
        assert!(!store.set_schedule_next("late", Some(999), None, 1).unwrap());
        let late = store.schedule("late").unwrap().unwrap();
        assert_eq!(
            (
                late.prompt.as_str(),
                late.next_ms,
                store.schedule_plan("late").unwrap()
            ),
            ("Check CI and docs", Some(250), Some(2))
        );
        // A prompt-only edit read before the engine claimed a run: the claimed next time stays,
        // and so does the plan.
        assert!(store.set_schedule_next("late", Some(350), None, 2).unwrap());
        store
            .put_schedule(
                &Schedule {
                    prompt: "Check CI".into(),
                    next_ms: Some(250),
                    updated_ms: 3,
                    ..schedule("late", None)
                },
                false,
            )
            .unwrap();
        assert_eq!(
            (
                store.schedule("late").unwrap().unwrap().next_ms,
                store.schedule_plan("late").unwrap()
            ),
            (Some(350), Some(2))
        );
        let soon = store.schedule("soon").unwrap().unwrap();
        assert_eq!(
            (soon.next_ms, soon.runs, soon.last),
            (Some(400), 1, Some(sent.clone()))
        );
        // Putting it again keeps how its runs went.
        store
            .put_schedule(
                &Schedule {
                    prompt: "Check".into(),
                    ..schedule("soon", Some(400))
                },
                false,
            )
            .unwrap();
        assert_eq!(store.schedule("soon").unwrap().unwrap().runs, 1);
        // Failing, then paused for it; a failure from a run claimed before the plan changed
        // doesn't pause the new plan.
        let failed = Run {
            at_ms: 260,
            outcome: RunOutcome::Failed,
            thread: Some("t1".into()),
            why: Some("The thread is archived.".into()),
        };
        let t1 = Target::Thread("t1".into());
        store
            .record_run("late", &failed, false, Some((1, &t1)))
            .unwrap();
        let late = store.schedule("late").unwrap().unwrap();
        assert!(!late.paused && late.next_ms == Some(350));
        // Nor one for a thread it no longer sends to.
        store
            .record_run(
                "late",
                &failed,
                false,
                Some((2, &Target::Thread("t9".into()))),
            )
            .unwrap();
        assert!(!store.schedule("late").unwrap().unwrap().paused);
        store
            .record_run("late", &failed, false, Some((2, &t1)))
            .unwrap();
        let late = store.schedule("late").unwrap().unwrap();
        assert!(late.paused && late.next_ms.is_none());
        // Why its runs fail: news once.
        assert!(store.note_failing("late", Some("archived")).unwrap());
        assert!(!store.note_failing("late", Some("archived")).unwrap());
        assert_eq!(
            store.schedule_failing("late").unwrap().as_deref(),
            Some("archived")
        );
        assert!(store.note_failing("late", None).unwrap());
        // Noted as sent, then busy after all: the run before it back, and waiting, even after an
        // edit of only its prompt; not once it's been paused (or retimed) since it was claimed.
        let resumed = flotilla_proto::Run {
            at_ms: 400,
            ..sent.clone()
        };
        store
            .set_schedule_paused("late", false, Some(500), 4)
            .unwrap();
        store.record_run("late", &resumed, true, None).unwrap();
        store
            .put_schedule(
                &Schedule {
                    prompt: "Check CI, then docs".into(),
                    target: Target::Thread("t3".into()),
                    updated_ms: 5,
                    ..schedule("late", None)
                },
                false,
            )
            .unwrap();
        assert_eq!(
            store.schedule_plan("late").unwrap(),
            Some(4),
            "a new prompt and thread aren't a new plan"
        );
        assert!(store.unsend_run("late", None, 77, 4, &resumed).unwrap());
        let late = store.schedule("late").unwrap().unwrap();
        assert_eq!((late.last, late.waiting_ms), (None, Some(77)));
        store
            .set_schedule_paused("late", false, Some(500), 6)
            .unwrap();
        store.record_run("late", &resumed, true, None).unwrap();
        store.set_schedule_paused("late", true, None, 7).unwrap();
        assert!(!store.unsend_run("late", None, 88, 6, &resumed).unwrap());
        let late = store.schedule("late").unwrap().unwrap();
        assert!(late.paused && late.waiting_ms.is_none() && late.last == Some(resumed));
        // A one-off done a while ago goes.
        store
            .record_run(
                "done",
                &Run {
                    at_ms: 60,
                    ..sent.clone()
                },
                true,
                None,
            )
            .unwrap();
        assert_eq!(store.delete_done_schedules(61).unwrap(), 1);
        // The thread's go with it (the one sent elsewhere stays).
        store.delete_thread("t1").unwrap();
        assert_eq!(ids(&store), ["late"]);
    }

    /// A prompt a schedule sent keeps saying so once boat's id replaces its placeholder.
    #[test]
    fn a_scheduled_prompt_keeps_its_mark() {
        let store = super::Store::open_in_memory().unwrap();
        store
            .insert_sent_prompt("t1", "local-1", "prompt", "Check CI")
            .unwrap();
        store.set_sent_schedule("t1", "local-1", "s1").unwrap();
        store
            .record_sent_prompt("t1", Some("local-1"), "p1", "prompt", "Check CI")
            .unwrap();
        let sent = store.sent_prompts("t1").unwrap();
        assert_eq!(
            (sent[0].id.as_str(), sent[0].schedule.as_deref()),
            ("p1", Some("s1"))
        );
    }
}

#[cfg(test)]
mod preview_tests {
    #[test]
    fn publishing_is_refused_once_a_project_is_being_removed() {
        let store = super::Store::open_in_memory().unwrap();
        let project = super::Project {
            id: "p1".into(),
            owner: "acme".into(),
            repo: "web".into(),
            default_branch: "main".into(),
            environment_name: "flotilla-web".into(),
            created_at: crate::util::now_iso(),
            template_sandbox_id: None,
            template_sha: None,
            template_built_at: None,
            prev_template_sandbox_id: None,
            head_sha: None,
            head_etag: None,
            checked_at: None,
        };
        store.insert_project(&project, &[]).unwrap();
        assert_eq!(store.set_template("p1", "bx_1", "aaa").unwrap(), None);
        store.set_removing("p1", true).unwrap();
        assert!(store.set_template("p1", "bx_2", "bbb").is_err());
        assert_eq!(
            store
                .project("p1")
                .unwrap()
                .unwrap()
                .template_sandbox_id
                .as_deref(),
            Some("bx_1")
        );
        store.set_removing("p1", false).unwrap();
        assert!(store.set_template("p1", "bx_2", "bbb").is_ok());
        // Forgotten only while it's still the one: a run may have published another.
        assert!(!store.forget_template("p1", "bx_1").unwrap());
        assert!(!store.template_gone("p1").unwrap());
        assert!(store.forget_template("p1", "bx_2").unwrap());
        let p = store.project("p1").unwrap().unwrap();
        assert_eq!(
            (
                p.template_sandbox_id,
                p.template_sha,
                p.prev_template_sandbox_id.as_deref()
            ),
            (None, None, Some("bx_1"))
        );
        assert!(store.template_gone("p1").unwrap());
        store.clear_template_gone("p1").unwrap();
        assert!(!store.template_gone("p1").unwrap());
        store.kv_set("template_gone:p1", "1").unwrap();
        store.delete_project("p1").unwrap();
        assert!(!store.template_gone("p1").unwrap());
        assert!(!store.removing("p1").unwrap());
        assert!(store.set_template("p1", "bx_3", "ccc").is_err());
    }

    #[test]
    fn behind_keeps_the_tip_it_counted_from() {
        let store = super::Store::open_in_memory().unwrap();
        // Written before the tip was kept.
        store.kv_set("behind:t1", "aaa:bbb:2").unwrap();
        assert_eq!(
            store.behind("t1").unwrap(),
            Some(("aaa".into(), "bbb".into(), 2))
        );
        assert_eq!(store.behind_tip("t1").unwrap(), "");
        store.set_behind("t1", "aaa", "ccc", 0, "ddd").unwrap();
        assert_eq!(
            store.behind("t1").unwrap(),
            Some(("aaa".into(), "ccc".into(), 0))
        );
        assert_eq!(store.behind_tip("t1").unwrap(), "ddd");
    }

    #[test]
    fn project_environments_are_left_out_of_the_picker() {
        let store = super::Store::open_in_memory().unwrap();
        let env = |id: &str, name: &str| crate::boat::Environment {
            id: id.into(),
            name: name.into(),
            is_default: id == "1",
        };
        // The last two are Flotilla Dev projects', hidden from the release app (as tests are).
        store
            .replace_environments(&[
                env("1", "base"),
                env("2", "flotilla-web-1a2b3c"),
                env("3", "flotilla-dev-web"),
                env("4", "Flotilla-Dev-App"),
            ])
            .unwrap();
        let project = super::Project {
            id: "p1".into(),
            owner: "acme".into(),
            repo: "web".into(),
            default_branch: "main".into(),
            environment_name: "flotilla-web-1a2b3c".into(),
            created_at: crate::util::now_iso(),
            template_sandbox_id: None,
            template_sha: None,
            template_built_at: None,
            prev_template_sandbox_id: None,
            head_sha: None,
            head_etag: None,
            checked_at: None,
        };
        store.insert_project(&project, &[]).unwrap();
        let names: Vec<String> = store
            .user_environments()
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["base"]);
    }

    #[test]
    fn replace_environments_drops_ones_boat_deleted() {
        let env = |id: &str, name: &str| crate::boat::Environment {
            id: id.into(),
            name: name.into(),
            is_default: id == "1",
        };
        let store = super::Store::open_in_memory().unwrap();
        store
            .replace_environments(&[env("1", "base"), env("2", "flotilla-exp")])
            .unwrap();
        store
            .replace_environments(&[env("1", "base"), env("3", "flotilla-web")])
            .unwrap();
        let names: Vec<String> = store
            .environments()
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["base", "flotilla-web"]);
    }
}

/// Whether message row `m`'s text is yours rather than Flotilla's (`?1`: `CHILD_NOTE`, trimmed):
/// not a delivery (`[flotilla] …`), nor a task another thread handed over.
pub(crate) fn yours_sql(m: &str) -> String {
    format!(
        "coalesce({m}.text, '') NOT LIKE '[flotilla]%' AND coalesce({m}.text, '') NOT LIKE '%' || ?1"
    )
}

/// A thread's sidebar row, before its reply, prompt, PR and failed start are filled in.
/// `asked_ms`: when its agent last asked a question that still waits on you.
fn new_summary(
    thread: Thread,
    viewed_at: Option<&str>,
    last_event_ms: Option<i64>,
    turn_created: Option<&str>,
    archived: bool,
    asked_ms: Option<i64>,
) -> ThreadSummary {
    let created_ms = crate::util::iso_to_ms(&thread.created_at).unwrap_or(0);
    let last_activity_ms = last_event_ms
        .unwrap_or(created_ms)
        .max(created_ms)
        .max(asked_ms.unwrap_or(0));
    let turn_started_ms = turn_created.and_then(crate::util::iso_to_ms);
    // Never shown here (e.g. synced from boat): treat as seen when it was created.
    let viewed_ms = viewed_at
        .and_then(crate::util::iso_to_ms)
        .unwrap_or(last_activity_ms);
    let activity = match thread.state {
        ThreadState::Expired => Activity::Expired,
        _ if archived => Activity::Archived,
        _ if turn_started_ms.is_some() || thread.state == ThreadState::Creating => {
            Activity::Working
        }
        // A question waiting on you since it was last shown needs you, as a finished turn does
        // (while the agent still works, it's working: what follows a turn reads it so).
        _ if asked_ms.is_some_and(|a| a > viewed_ms) => Activity::Unread,
        // A VM gone wrong wants attention; a start that failed in front of you doesn't.
        ThreadState::Error if thread.sandbox_id.is_some() => Activity::Unread,
        _ if last_activity_ms > viewed_ms => Activity::Unread,
        _ => Activity::Idle,
    };
    ThreadSummary {
        thread,
        activity,
        last_activity_ms,
        turn_started_ms,
        last_reply: None,
        last_prompt: None,
        pr: None,
        start_failed: false,
        prompted_ms: None,
        opens: None,
    }
}

/// What [`Store::apply_events`] changed.
#[derive(Debug, Clone, Default)]
pub struct ApplyResult {
    pub changed: bool,
    /// Turns first seen in this apply whose prompt event was already terminal.
    pub new_terminal_turns: Vec<String>,
}

pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
    /// How many times this process had rebuilt this database (`backup`) when this connection
    /// was opened: one opened before a rebuild has the file that was moved aside.
    generation: u64,
}

/// How many times each database (by its path) was rebuilt in this process (`Store::backup`).
static REBUILDS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Each database's rebuild gate (by its path): held alone while it's rebuilt (`Store::backup`);
/// held, shared, while a connection to it opens, so none opens a file about to be moved aside
/// without knowing (`Store::stale`), and while a link is made public or private (`public`). A
/// rebuild waiting goes first: no one new is let in meanwhile. One database's never holds up
/// another's.
#[derive(Default)]
struct Gate {
    readers: usize,
    writer: bool,
    writers_waiting: usize,
}

static GATES: std::sync::Mutex<std::collections::BTreeMap<PathBuf, Gate>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());
static GATE_FREED: std::sync::Condvar = std::sync::Condvar::new();

/// The gate held, shared (`gate_read`): let go when dropped. None for a database in memory.
pub(crate) struct GateRead(Option<PathBuf>);

impl Drop for GateRead {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let mut gates = GATES.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(g) = gates.get_mut(p) {
                g.readers -= 1;
            }
            GATE_FREED.notify_all();
        }
    }
}

/// The gate held alone (`gate_write`): let go when dropped.
pub(crate) struct GateWrite(PathBuf);

impl Drop for GateWrite {
    fn drop(&mut self) {
        let mut gates = GATES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(g) = gates.get_mut(&self.0) {
            g.writer = false;
        }
        GATE_FREED.notify_all();
    }
}

pub(crate) fn gate_read(path: Option<&Path>) -> GateRead {
    let Some(path) = path else {
        return GateRead(None);
    };
    let mut gates = GATES.lock().unwrap_or_else(|e| e.into_inner());
    while gates
        .get(path)
        .is_some_and(|g| g.writer || g.writers_waiting > 0)
    {
        gates = GATE_FREED.wait(gates).unwrap_or_else(|e| e.into_inner());
    }
    gates.entry(path.to_path_buf()).or_default().readers += 1;
    GateRead(Some(path.to_path_buf()))
}

pub(crate) fn gate_write(path: &Path) -> GateWrite {
    let mut gates = GATES.lock().unwrap_or_else(|e| e.into_inner());
    gates.entry(path.to_path_buf()).or_default().writers_waiting += 1;
    while gates.get(path).is_some_and(|g| g.writer || g.readers > 0) {
        gates = GATE_FREED.wait(gates).unwrap_or_else(|e| e.into_inner());
    }
    let g = gates.entry(path.to_path_buf()).or_default();
    g.writers_waiting -= 1;
    g.writer = true;
    GateWrite(path.to_path_buf())
}

/// A database's replacement, whole, about to take its place (`Store::backup`).
fn rebuilt_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.rebuilt", path.display()))
}

/// Where a rebuild under way moves the old database (`Store::backup`), written before the moves
/// so a swap cut off can put the old one's WAL files with it, not with the new one.
fn swap_journal(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.rebuilt-old", path.display()))
}

/// A swap cut off between moving the old database aside and putting its replacement in place
/// (`path` gone, `.rebuilt` there): the old one's WAL files that weren't moved go where it went
/// (or are set aside, if that isn't known), so SQLite never reads them into the new one; then
/// the new one is put in place.
fn finish_swap(path: &Path) -> Result<()> {
    let rebuilt = rebuilt_path(path);
    if path.exists() || !rebuilt.exists() {
        return Ok(());
    }
    let journal = swap_journal(path);
    let old = std::fs::read_to_string(&journal)
        .ok()
        .map(|p| PathBuf::from(p.trim()))
        .filter(|p| !p.as_os_str().is_empty());
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", path.display()));
        if side.exists() {
            let to = match &old {
                Some(o) if !PathBuf::from(format!("{}{suffix}", o.display())).exists() => {
                    PathBuf::from(format!("{}{suffix}", o.display()))
                }
                _ => PathBuf::from(format!("{}{suffix}.orphaned", path.display())),
            };
            std::fs::rename(&side, to)?;
        }
    }
    move_db(&rebuilt, path)?;
    let _ = std::fs::remove_file(&journal);
    Ok(())
}

/// Move a database and its WAL files.
fn move_db(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to)?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", from.display()));
        if side.exists() {
            std::fs::rename(&side, PathBuf::from(format!("{}{suffix}", to.display())))?;
        }
    }
    Ok(())
}

fn remove_db(path: &Path) {
    let _ = std::fs::remove_file(path);
    remove_db_sidecars(path);
}

fn remove_db_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn rebuilds(path: Option<&Path>) -> u64 {
    path.map_or(0, |p| {
        REBUILDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(p)
            .copied()
            .unwrap_or(0)
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        // A database that isn't there, with its replacement whole beside it, is a rebuild cut off
        // between its two moves: the replacement takes its place, with the gate held alone (and
        // the state looked at again under it), so no other open is under way meanwhile and none
        // can move a WAL that's the new database's.
        if !path.exists() && rebuilt_path(path).exists() {
            let _alone = gate_write(path);
            finish_swap(path)?;
        }
        let _gate = gate_read(Some(path));
        Self::open_unlocked(path)
    }

    /// `open`, for `backup`, which holds the gate itself.
    fn open_unlocked(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let generation = rebuilds(Some(path));
        let conn = Connection::open(path)?;
        Self::init(conn, Some(path.to_path_buf()), generation)
    }

    /// For reading beside another process that has the database (the CLI's `threads`, `show`
    /// and `search` while the app runs): nothing is made, migrated or rebuilt. A database at
    /// another version than this one's is refused, as its tables may not be what this reads.
    pub fn open_read_only(path: &Path) -> Result<Store> {
        let _gate = gate_read(Some(path));
        let generation = rebuilds(Some(path));
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let store = Store {
            conn,
            generation,
            path: Some(path.to_path_buf()),
        };
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let messages = store.kv_get("messages_version").ok().flatten();
        if version != MIGRATIONS.len() as i64
            || messages.as_deref() != Some(MESSAGES_VERSION.to_string().as_str())
        {
            return Err(crate::Error::Invalid(
                "this database is at another version of Flotilla than this command: run the version that has it open, or quit that one first".into(),
            ));
        }
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?, None, 0)
    }

    fn init(conn: Connection, path: Option<PathBuf>, generation: u64) -> Result<Store> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Switching a new file to WAL needs it alone for a moment, which SQLite doesn't wait
        // for as it waits to write: another opening it at the same time is waited out here.
        let mut tries = 0;
        loop {
            match conn.pragma_update(None, "journal_mode", "WAL") {
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::DatabaseBusy && tries < 100 =>
                {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                r => break r?,
            }
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let store = Store {
            conn,
            generation,
            path,
        };
        store.migrate()?;
        store.ensure_messages()?;
        Ok(store)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Opened before a rebuild in this process: what it writes goes to the database moved
    /// aside, not the one in use.
    pub fn stale(&self) -> bool {
        self.generation != rebuilds(self.path())
    }

    /// Whether this connection can write: takes the write lock, then lets go. Busy (another
    /// connection writing all the while) counts as writable: it's being written.
    pub fn check_writable(&self) -> Result<()> {
        match self.conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;") {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                Ok(())
            }
            r => Ok(r?),
        }
    }

    /// A transaction that will write: it takes the write lock up front (`BEGIN IMMEDIATE`),
    /// waiting out another writer. A deferred one that reads first fails at once instead
    /// (`SQLITE_BUSY_SNAPSHOT`) if another connection writes in between, which the host's
    /// "marked read" and the poller's events do all the time.
    pub(crate) fn write(&self) -> Result<rusqlite::Transaction<'_>> {
        Ok(rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Each step under the write lock, reading where the database is only once it's held: two
    /// processes upgrading at once don't both run a step.
    fn migrate(&self) -> Result<()> {
        let version = |c: &Connection| -> rusqlite::Result<usize> {
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .map(|v| v.max(0) as usize)
        };
        while version(&self.conn)? < MIGRATIONS.len() {
            let tx = self.write()?;
            let v = version(&tx)?;
            if let Some(sql) = MIGRATIONS.get(v) {
                tx.execute_batch(sql)?;
                tx.pragma_update(None, "user_version", (v + 1) as i64)?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// `messages` is derived from `events`: when [`MESSAGES_VERSION`] changes, drop it, recreate
    /// it and rebuild every thread. Rendering changes never need a real migration.
    fn ensure_messages(&self) -> Result<()> {
        // Both the table and its search index at this version, or they're made again.
        let current = |c: &Connection| -> rusqlite::Result<bool> {
            let exists: bool = c.query_row("SELECT count(*) = 2 FROM sqlite_master WHERE type='table' AND name IN ('messages', 'message_search')", [], |r| r.get(0))?;
            let version: Option<String> = c
                .query_row(
                    "SELECT value FROM kv WHERE key = 'messages_version'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(exists && version == Some(MESSAGES_VERSION.to_string()))
        };
        let words = |c: &Connection| -> rusqlite::Result<bool> {
            c.query_row("SELECT count(*) = 1 FROM sqlite_master WHERE type='table' AND name = 'message_words'", [], |r| r.get(0))
        };
        if current(&self.conn)? {
            // The word index, if it came after the rest: made from what's there (unless another
            // process made it while this one waited for the lock).
            if !words(&self.conn)? {
                let tx = self.write()?;
                if !words(&tx)? {
                    tx.execute_batch(&words_ddl())?;
                    let indexed = crate::search::indexed_sql("m");
                    tx.execute(&format!("INSERT INTO message_words (rowid, body) SELECT m.id, {indexed} FROM messages m WHERE {indexed} IS NOT NULL"), [])?;
                }
                tx.commit()?;
            }
            return Ok(());
        }
        // All of it in one transaction: dropped, made, every thread's messages written, and the
        // version said, so no one reads (or makes again) half of it, and a stop part way leaves
        // the old. Another process doing the same is waited out, however long it takes.
        let tx = self.write_waiting()?;
        // Another process made them while this one waited for the lock.
        if current(&tx)? {
            drop(tx);
            return self.ensure_messages();
        }
        tx.execute_batch("DROP TABLE IF EXISTS message_words; DROP TABLE IF EXISTS message_search; DROP TABLE IF EXISTS messages;")?;
        tx.execute_batch(&messages_ddl())?;
        for t in self.threads()? {
            Self::put_messages(&tx, &t.id, &self.normalized(&t.id)?)?;
        }
        tx.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES ('messages_version', ?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![MESSAGES_VERSION.to_string(), crate::util::now_iso()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `write`, waiting past the busy timeout while another connection writes (one rebuilding
    /// every thread's messages, say), for up to ten minutes.
    fn write_waiting(&self) -> Result<rusqlite::Transaction<'_>> {
        let since = std::time::Instant::now();
        loop {
            match self.write() {
                Err(crate::Error::Db(rusqlite::Error::SqliteFailure(e, _)))
                    if e.code == rusqlite::ErrorCode::DatabaseBusy
                        && since.elapsed() < std::time::Duration::from_secs(600) => {}
                r => return r,
            }
        }
    }

    // ---- kv ----

    /// Why a thread's start failed before it had a VM (shown with Try again), if it did.
    pub fn start_error(&self, thread_id: &str) -> Result<Option<String>> {
        self.kv_get(&start_error_key(thread_id))
    }

    pub fn set_start_error(&self, thread_id: &str, error: Option<&str>) -> Result<()> {
        match error {
            Some(e) => self.kv_set(&start_error_key(thread_id), e),
            None => {
                self.conn
                    .execute("DELETE FROM kv WHERE key=?1", [start_error_key(thread_id)])?;
                Ok(())
            }
        }
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// Every non-empty `kv` entry whose key starts with `prefix`.
    pub fn kv_with_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let mut st = self
            .conn
            .prepare("SELECT key, value FROM kv WHERE substr(key, 1, ?2) = ?1 AND value != ''")?;
        let rows = st.query_map(params![prefix, prefix.chars().count() as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![key, value, now_iso()],
        )?;
        Ok(())
    }

    pub fn kv_delete(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM kv WHERE key=?1", [key])?;
        Ok(())
    }

    /// Keep `value` under `prefix` and the next number `counter` (a `kv` entry) gives, which
    /// only grows, with `also` in the same write. Returns the number, as written (20 digits, so
    /// they sort as numbers).
    pub fn kv_append(
        &self,
        counter: &str,
        prefix: &str,
        value: &str,
        also: &[(String, String)],
    ) -> Result<String> {
        let tx = self.write()?;
        let last: Option<String> = tx
            .query_row("SELECT value FROM kv WHERE key=?1", [counter], |r| r.get(0))
            .optional()?;
        let n = last.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) + 1;
        let id = format!("{n:020}");
        for (key, value) in [
            (counter.to_string(), n.to_string()),
            (format!("{prefix}{id}"), value.to_string()),
        ]
        .iter()
        .chain(also)
        {
            tx.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
                params![key, value, now_iso()],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Set several `kv` entries in one write: all of them, or none.
    pub fn kv_set_all(&self, entries: &[(String, String)]) -> Result<()> {
        let tx = self.write()?;
        for (key, value) in entries {
            tx.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
                params![key, value, now_iso()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Note a request's client-made id as acted on: false if it already was.
    pub fn first_receipt(&self, id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO receipts (id, at) VALUES (?1, ?2)",
            params![id, now_iso()],
        )?;
        Ok(n == 1)
    }

    /// Whether a client-made id was kept already, keeping nothing.
    pub fn has_receipt(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM receipts WHERE id=?1", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Forget receipts older than `before` (RFC 3339): a client resends within seconds, not weeks.
    pub fn prune_receipts(&self, before: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM receipts WHERE at < ?1", [before])?;
        Ok(())
    }

    // ---- environments ----

    pub fn upsert_environment(&self, env: &boat::Environment) -> Result<()> {
        self.conn.execute(
            "INSERT INTO environments (id, name, is_default, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, is_default=excluded.is_default, updated_at=excluded.updated_at",
            params![env.id, env.name, env.is_default, now_iso()],
        )?;
        Ok(())
    }

    pub fn delete_environment(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM environments WHERE id=?1", [id])?;
        Ok(())
    }

    /// Make the cached environments match boat's full list: environments deleted in boat go,
    /// so pickers stop offering them. Threads keep their own copy of the name.
    pub fn replace_environments(&self, envs: &[boat::Environment]) -> Result<()> {
        let tx = self.write()?;
        for env in envs {
            self.upsert_environment(env)?;
        }
        let keep: HashSet<&str> = envs.iter().map(|e| e.id.as_str()).collect();
        for env in self.environments()? {
            if !keep.contains(env.id.as_str()) {
                self.delete_environment(&env.id)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The environments a person picks from: boat's, without the ones Flotilla made for its
    /// projects (an implementation detail; a project thread uses its own).
    pub fn user_environments(&self) -> Result<Vec<Environment>> {
        let owned: HashSet<String> = self
            .projects()?
            .into_iter()
            .map(|p| p.environment_name)
            .collect();
        // Flotilla Dev's projects' too, in the release app.
        let dev = format!("{}-", crate::flavor::Flavor::Dev.environment_prefix());
        let other =
            |e: &Environment| !crate::flavor::is_dev() && e.name.to_lowercase().starts_with(&dev);
        Ok(self
            .environments()?
            .into_iter()
            .filter(|e| !owned.contains(&e.name) && !other(e))
            .collect())
    }

    pub fn environments(&self) -> Result<Vec<Environment>> {
        let mut st = self.conn.prepare("SELECT id, name, is_default, updated_at FROM environments ORDER BY is_default DESC, name")?;
        let rows = st.query_map([], |r| {
            Ok(Environment {
                id: r.get(0)?,
                name: r.get(1)?,
                is_default: r.get(2)?,
                updated_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn environment(&self, name_or_id: &str) -> Result<Option<Environment>> {
        Ok(self
            .environments()?
            .into_iter()
            .find(|p| p.id == name_or_id || p.name == name_or_id))
    }

    // ---- projects (GitHub repos) ----

    const PROJECT_COLS: &str = "id, owner, repo, default_branch, environment_name, created_at, template_sandbox_id, template_sha,
        template_built_at, prev_template_sandbox_id, head_sha, head_etag, checked_at";

    fn project_row(r: &Row) -> rusqlite::Result<Project> {
        Ok(Project {
            id: r.get(0)?,
            owner: r.get(1)?,
            repo: r.get(2)?,
            default_branch: r.get(3)?,
            environment_name: r.get(4)?,
            created_at: r.get(5)?,
            template_sandbox_id: r.get(6)?,
            template_sha: r.get(7)?,
            template_built_at: r.get(8)?,
            prev_template_sandbox_id: r.get(9)?,
            head_sha: r.get(10)?,
            head_etag: r.get(11)?,
            checked_at: r.get(12)?,
        })
    }

    /// Save a new project and its scripts together.
    pub fn insert_project(&self, p: &Project, scripts: &[SetupScript]) -> Result<()> {
        let tx = self.write()?;
        tx.execute(
            "INSERT INTO projects (id, owner, repo, default_branch, environment_name, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![p.id, p.owner, p.repo, p.default_branch, p.environment_name, p.created_at],
        )?;
        Self::write_scripts(&tx, &p.id, scripts)?;
        tx.commit()?;
        Ok(())
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {} FROM projects ORDER BY repo COLLATE NOCASE, owner",
            Self::PROJECT_COLS
        ))?;
        let rows = st.query_map([], Self::project_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn project(&self, id: &str) -> Result<Option<Project>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM projects WHERE id=?1", Self::PROJECT_COLS),
                [id],
                Self::project_row,
            )
            .optional()?)
    }

    pub fn project_by_repo(&self, owner: &str, repo: &str) -> Result<Option<Project>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM projects WHERE owner=?1 COLLATE NOCASE AND repo=?2 COLLATE NOCASE", Self::PROJECT_COLS),
                [owner, repo],
                Self::project_row,
            )
            .optional()?)
    }

    /// Remove a project and its scripts and setup history. Its threads keep their branch.
    pub fn delete_project(&self, id: &str) -> Result<()> {
        let tx = self.write()?;
        tx.execute("DELETE FROM project_scripts WHERE project_id=?1", [id])?;
        tx.execute("DELETE FROM setup_runs WHERE project_id=?1", [id])?;
        tx.execute("DELETE FROM projects WHERE id=?1", [id])?;
        tx.execute(
            "DELETE FROM kv WHERE key IN (?1, ?2)",
            [removing_key(id), template_gone_key(id)],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn scripts(&self, project_id: &str) -> Result<Vec<SetupScript>> {
        let mut st = self.conn.prepare(
            "SELECT command, fresh_only FROM project_scripts WHERE project_id=?1 ORDER BY position",
        )?;
        let rows = st.query_map([project_id], |r| {
            Ok(SetupScript {
                command: r.get(0)?,
                fresh_only: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_scripts(&self, project_id: &str, scripts: &[SetupScript]) -> Result<()> {
        let tx = self.write()?;
        Self::write_scripts(&tx, project_id, scripts)?;
        tx.commit()?;
        Ok(())
    }

    fn write_scripts(conn: &Connection, project_id: &str, scripts: &[SetupScript]) -> Result<()> {
        conn.execute(
            "DELETE FROM project_scripts WHERE project_id=?1",
            [project_id],
        )?;
        for (i, sc) in scripts
            .iter()
            .filter(|s| !s.command.trim().is_empty())
            .enumerate()
        {
            conn.execute(
                "INSERT INTO project_scripts (project_id, position, command, fresh_only) VALUES (?1, ?2, ?3, ?4)",
                params![project_id, i as i64, sc.command.trim(), sc.fresh_only],
            )?;
        }
        Ok(())
    }

    /// The default branch's head as GitHub last reported it.
    pub fn set_project_head(&self, id: &str, sha: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET head_sha=?2, checked_at=?3 WHERE id=?1",
            params![id, sha, now_iso()],
        )?;
        Ok(())
    }

    pub fn set_project_checked(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE projects SET checked_at=?2 WHERE id=?1",
            params![id, now_iso()],
        )?;
        Ok(())
    }

    /// The commit a project thread's branch now starts from (after pulling main).
    pub fn set_thread_base(&self, id: &str, sha: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET base_sha=?2 WHERE id=?1",
            params![id, sha],
        )?;
        Ok(())
    }

    /// How far a thread's base is behind main: `(base, head, count)` from `behind:<thread>`.
    /// How many commits the default branch (at `head`) is ahead of a thread that started from
    /// `base`: `(base, head, count)`.
    pub fn behind(&self, thread_id: &str) -> Result<Option<(String, String, i64)>> {
        let v = self
            .kv_get(&format!("behind:{thread_id}"))?
            .unwrap_or_default();
        let parts: Vec<&str> = v.split(':').collect();
        Ok(match parts.as_slice() {
            [base, head, n] | [base, head, n, _] => n
                .parse()
                .ok()
                .map(|n| (base.to_string(), head.to_string(), n)),
            _ => None,
        })
    }

    /// The branch's tip on GitHub the count was taken against (empty: never pushed, so `base`).
    pub fn behind_tip(&self, thread_id: &str) -> Result<String> {
        let v = self
            .kv_get(&format!("behind:{thread_id}"))?
            .unwrap_or_default();
        Ok(v.split(':').nth(3).unwrap_or_default().to_string())
    }

    pub fn set_behind(
        &self,
        thread_id: &str,
        base: &str,
        head: &str,
        count: i64,
        tip: &str,
    ) -> Result<()> {
        self.kv_set(
            &format!("behind:{thread_id}"),
            &format!("{base}:{head}:{count}:{tip}"),
        )
    }

    /// Branch names already taken by a project's threads.
    /// Give a project thread the branch `pick` makes from the project's other threads'
    /// branches, read and written under the write lock, so threads named at the same moment
    /// (one prompt started on several models) never end up on one branch. Returns the name.
    pub fn claim_branch(
        &self,
        thread_id: &str,
        project_id: &str,
        pick: impl Fn(&[String]) -> String,
    ) -> Result<String> {
        let tx = self.write()?;
        let taken: Vec<String> = {
            let mut st = tx.prepare("SELECT branch FROM threads WHERE repo_project_id=?1 AND branch IS NOT NULL AND id != ?2")?;
            let rows = st.query_map(params![project_id, thread_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let name = pick(&taken);
        tx.execute(
            "UPDATE threads SET branch=?2 WHERE id=?1",
            params![thread_id, name],
        )?;
        tx.commit()?;
        Ok(name)
    }

    pub fn project_branches(&self, project_id: &str) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT branch FROM threads WHERE repo_project_id=?1 AND branch IS NOT NULL",
        )?;
        let rows = st.query_map([project_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How a starting thread is getting on (JSON from `lifecycle::StartStages`), kept in `kv`.
    pub fn start_stages(&self, thread_id: &str) -> Result<Option<String>> {
        Ok(self
            .kv_get(&format!("start_stages:{thread_id}"))?
            .filter(|v| !v.is_empty()))
    }

    pub fn set_start_stages(&self, thread_id: &str, json: Option<&str>) -> Result<()> {
        self.kv_set(&format!("start_stages:{thread_id}"), json.unwrap_or(""))
    }

    // ---- pull requests ----

    /// Every thread's pull request: `(pr, linked by hand)`.
    pub fn pull_requests(&self) -> Result<std::collections::HashMap<String, (PullRequest, bool)>> {
        let mut st = self.conn.prepare(
            "SELECT thread_id, number, url, title, state, is_draft, mergeable, review, checks_json, additions, deletions, linked_by, head, base, updated_at FROM thread_prs",
        )?;
        let rows = st.query_map([], |r| {
            let checks: Option<String> = r.get(8)?;
            let pr = PullRequest {
                number: r.get(1)?,
                url: r.get(2)?,
                title: r.get(3)?,
                head: r.get::<_, Option<String>>(12)?.unwrap_or_default(),
                base: r.get::<_, Option<String>>(13)?.unwrap_or_default(),
                state: r.get(4)?,
                is_draft: r.get(5)?,
                mergeable: r.get(6)?,
                review: r.get(7)?,
                checks: checks
                    .and_then(|c| serde_json::from_str::<Vec<Check>>(&c).ok())
                    .unwrap_or_default(),
                additions: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                deletions: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                updated_at: r.get(14)?,
                // Only wanted the moment a merge is seen.
                merged_at: None,
            };
            Ok((
                r.get::<_, String>(0)?,
                (pr, r.get::<_, String>(11)? == "user"),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Save a thread's pull request. Returns the state it had before, if any.
    pub fn set_pull_request(
        &self,
        thread_id: &str,
        pr: &PullRequest,
        linked_by_user: bool,
    ) -> Result<Option<String>> {
        let before: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM thread_prs WHERE thread_id=?1",
                [thread_id],
                |r| r.get(0),
            )
            .optional()?;
        self.conn.execute(
            "INSERT INTO thread_prs (thread_id, number, url, title, state, is_draft, mergeable, review, checks_json, additions, deletions, linked_by, checked_at, head, base, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(thread_id) DO UPDATE SET number=excluded.number, url=excluded.url, title=excluded.title, state=excluded.state,
               is_draft=excluded.is_draft, mergeable=excluded.mergeable, review=excluded.review, checks_json=excluded.checks_json,
               additions=excluded.additions, deletions=excluded.deletions, linked_by=excluded.linked_by, checked_at=excluded.checked_at,
               head=excluded.head, base=excluded.base, updated_at=excluded.updated_at",
            params![
                thread_id,
                pr.number,
                pr.url,
                pr.title,
                pr.state,
                pr.is_draft,
                pr.mergeable,
                pr.review,
                serde_json::to_string(&pr.checks)?,
                pr.additions,
                pr.deletions,
                if linked_by_user { "user" } else { "auto" },
                now_iso(),
                pr.head,
                pr.base,
                pr.updated_at
            ],
        )?;
        Ok(before)
    }

    pub fn clear_pull_request(&self, thread_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM thread_prs WHERE thread_id=?1", [thread_id])?;
        Ok(())
    }

    // ---- setup runs (preparing the VM a project's threads copy) ----

    const RUN_COLS: &str = "id, project_id, kind, trigger, sha, sandbox_id, state, step, error, log, started_at, finished_at";

    fn run_row(r: &Row) -> rusqlite::Result<SetupRun> {
        Ok(SetupRun {
            id: r.get(0)?,
            project_id: r.get(1)?,
            kind: r.get(2)?,
            trigger: r.get(3)?,
            sha: r.get(4)?,
            sandbox_id: r.get(5)?,
            state: r.get(6)?,
            step: r.get(7)?,
            error: r.get(8)?,
            log: r.get(9)?,
            started_at: r.get(10)?,
            finished_at: r.get(11)?,
        })
    }

    pub fn insert_setup_run(&self, run: &SetupRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO setup_runs (id, project_id, kind, trigger, sha, state, log, started_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7)",
            params![run.id, run.project_id, run.kind, run.trigger, run.sha, run.state, run.started_at],
        )?;
        Ok(())
    }

    pub fn setup_run(&self, id: &str) -> Result<Option<SetupRun>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM setup_runs WHERE id=?1", Self::RUN_COLS),
                [id],
                Self::run_row,
            )
            .optional()?)
    }

    /// A project's runs, newest first.
    pub fn setup_runs(&self, project_id: &str, limit: usize) -> Result<Vec<SetupRun>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {} FROM setup_runs WHERE project_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2",
            Self::RUN_COLS
        ))?;
        let rows = st.query_map(params![project_id, limit as i64], Self::run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Runs still marked running (after a quit, these were cut short).
    pub fn unfinished_setup_runs(&self) -> Result<Vec<SetupRun>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {} FROM setup_runs WHERE state='running'",
            Self::RUN_COLS
        ))?;
        let rows = st.query_map([], Self::run_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_run_step(&self, id: &str, step: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE setup_runs SET step=?2 WHERE id=?1",
            params![id, step],
        )?;
        Ok(())
    }

    pub fn set_run_kind(&self, id: &str, kind: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE setup_runs SET kind=?2 WHERE id=?1",
            params![id, kind],
        )?;
        Ok(())
    }

    pub fn set_run_sandbox(&self, id: &str, sandbox_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE setup_runs SET sandbox_id=?2 WHERE id=?1",
            params![id, sandbox_id],
        )?;
        Ok(())
    }

    /// Add output to a run's log, keeping its last ~256 KB.
    pub fn append_run_log(&self, id: &str, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "UPDATE setup_runs SET log = substr(log || ?2, max(1, length(log || ?2) - 262144 + 1)) WHERE id=?1",
            params![id, text],
        )?;
        Ok(())
    }

    /// End a run: `done`, `failed` or `cancelled`.
    pub fn finish_run(&self, id: &str, state: &str, error: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE setup_runs SET state=?2, error=?3, finished_at=?4 WHERE id=?1",
            params![id, state, error, now_iso()],
        )?;
        Ok(())
    }

    /// Publish a finished run's VM as the one new threads copy. Returns the VM that is no
    /// longer needed (two runs back), for the caller to delete.
    /// Publish a setup run's VM. Refused (`NotFound`) once the project is gone or being
    /// removed, so the run deletes its VM rather than leave it with no project.
    pub fn set_template(
        &self,
        project_id: &str,
        sandbox_id: &str,
        sha: &str,
    ) -> Result<Option<String>> {
        let tx = self.write()?;
        let old: Option<String> = tx
            .query_row(
                "SELECT prev_template_sandbox_id FROM projects WHERE id=?1",
                [project_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let changed = tx.execute(
            "UPDATE projects SET prev_template_sandbox_id=template_sandbox_id, template_sandbox_id=?2, template_sha=?3, template_built_at=?4
             WHERE id=?1 AND NOT EXISTS (SELECT 1 FROM kv WHERE key=?5 AND value='1')",
            params![project_id, sandbox_id, sha, now_iso(), removing_key(project_id)],
        )?;
        if changed == 0 {
            return Err(Error::NotFound(format!("project {project_id} (removed)")));
        }
        tx.commit()?;
        Ok(old.filter(|o| o != sandbox_id))
    }

    /// Forget the VM new threads copy, deleted outside Flotilla (or its disk gone), so the
    /// project is set up again (see [`Store::template_gone`]). Only if it's still that VM: a
    /// run may have just published another. Returns whether it was.
    pub fn forget_template(&self, project_id: &str, sandbox_id: &str) -> Result<bool> {
        let tx = self.write()?;
        let n = tx.execute(
            "UPDATE projects SET template_sandbox_id=NULL, template_sha=NULL, template_built_at=NULL WHERE id=?1 AND template_sandbox_id=?2",
            params![project_id, sandbox_id],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES (?1, '1', ?2) ON CONFLICT(key) DO UPDATE SET value='1', updated_at=excluded.updated_at",
                params![template_gone_key(project_id), now_iso()],
            )?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    /// A forgotten prepared VM not set up again yet: the next run gets one try even on a
    /// commit whose setup failed. Starting a run clears it.
    pub fn template_gone(&self, project_id: &str) -> Result<bool> {
        Ok(self.kv_get(&template_gone_key(project_id))?.as_deref() == Some("1"))
    }

    pub fn clear_template_gone(&self, project_id: &str) -> Result<()> {
        self.kv_set(&template_gone_key(project_id), "")
    }

    /// Mark a project as being removed (or not, if removing it failed): its setup runs stop
    /// and can't publish.
    pub fn set_removing(&self, project_id: &str, removing: bool) -> Result<()> {
        self.kv_set(&removing_key(project_id), if removing { "1" } else { "" })
    }

    pub fn removing(&self, project_id: &str) -> Result<bool> {
        Ok(self.kv_get(&removing_key(project_id))?.as_deref() == Some("1"))
    }

    // ---- threads ----

    const THREAD_COLS: &str = "id, environment_id, environment_name, title, sandbox_id, conversation_id, provider, model, effort, fast,
        state, keep_running, archive_after, last_stopped_at, data_expired_at, created_at, updated_at, repo_project_id, branch, base_sha";

    fn thread_row(r: &Row) -> rusqlite::Result<Thread> {
        Ok(Thread {
            id: r.get(0)?,
            environment_id: r.get(1)?,
            environment_name: r.get(2)?,
            title: r.get(3)?,
            sandbox_id: r.get(4)?,
            conversation_id: r.get(5)?,
            provider: r.get(6)?,
            model: r.get(7)?,
            effort: r.get(8)?,
            fast: r.get(9)?,
            state: ThreadState::parse(&r.get::<_, String>(10)?),
            keep_running: r.get(11)?,
            archive_after: r.get(12)?,
            last_stopped_at: r.get(13)?,
            data_expired_at: r.get(14)?,
            created_at: r.get(15)?,
            updated_at: r.get(16)?,
            repo_project_id: r.get(17)?,
            branch: r.get(18)?,
            base_sha: r.get(19)?,
        })
    }

    pub fn insert_thread(&self, t: &Thread) -> Result<()> {
        self.conn.execute(
            &format!("INSERT INTO threads ({}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)", Self::THREAD_COLS),
            params![
                t.id,
                t.environment_id,
                t.environment_name,
                t.title,
                t.sandbox_id,
                t.conversation_id,
                t.provider,
                t.model,
                t.effort,
                t.fast,
                t.state.as_str(),
                t.keep_running,
                t.archive_after,
                t.last_stopped_at,
                t.data_expired_at,
                t.created_at,
                t.updated_at,
                t.repo_project_id,
                t.branch,
                t.base_sha
            ],
        )?;
        Ok(())
    }

    /// Write every mutable field. Provider, model, effort and fast are never written here.
    /// The sandbox and conversation ids are set once and never cleared, so a row read before
    /// one was saved can't erase it. Code that writes after a network call should use the
    /// one-field setters below instead, so it doesn't put back other fields read before it.
    pub fn update_thread(&self, t: &Thread) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET environment_id=?2, environment_name=?3, title=?4, sandbox_id=COALESCE(?5, sandbox_id),
               conversation_id=COALESCE(?6, conversation_id), state=?7, keep_running=?8, archive_after=?9,
               last_stopped_at=?10, data_expired_at=?11, updated_at=?12,
               repo_project_id=COALESCE(?13, repo_project_id), branch=COALESCE(?14, branch), base_sha=COALESCE(?15, base_sha)
             WHERE id=?1",
            params![
                t.id,
                t.environment_id,
                t.environment_name,
                t.title,
                t.sandbox_id,
                t.conversation_id,
                t.state.as_str(),
                t.keep_running,
                t.archive_after,
                t.last_stopped_at,
                t.data_expired_at,
                now_iso(),
                t.repo_project_id,
                t.branch,
                t.base_sha
            ],
        )?;
        Ok(())
    }

    pub fn set_thread_state(&self, id: &str, state: ThreadState) -> Result<()> {
        self.set_thread_field(id, "state", state.as_str())
    }

    pub fn set_thread_title(&self, id: &str, title: &str) -> Result<()> {
        self.set_thread_field(id, "title", title)
    }

    pub fn set_keep_running(&self, id: &str, on: bool) -> Result<()> {
        self.set_thread_field(id, "keep_running", on)
    }

    pub fn set_archive_after(&self, id: &str, at: Option<&str>) -> Result<()> {
        self.set_thread_field(id, "archive_after", at)
    }

    /// Fills in the conversation id if the thread has none yet.
    pub fn set_conversation_id(&self, id: &str, conversation_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET conversation_id=?2, updated_at=?3 WHERE id=?1 AND conversation_id IS NULL",
            params![id, conversation_id, now_iso()],
        )?;
        Ok(())
    }

    /// `column` is always one of the literals above, never input.
    fn set_thread_field(&self, id: &str, column: &str, value: impl rusqlite::ToSql) -> Result<()> {
        self.conn.execute(
            &format!("UPDATE threads SET {column}=?2, updated_at=?3 WHERE id=?1"),
            params![id, value, now_iso()],
        )?;
        Ok(())
    }

    pub fn thread(&self, id: &str) -> Result<Option<Thread>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM threads WHERE id=?1", Self::THREAD_COLS),
                [id],
                Self::thread_row,
            )
            .optional()?)
    }

    /// Newest first.
    pub fn threads(&self) -> Result<Vec<Thread>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {} FROM threads ORDER BY created_at DESC",
            Self::THREAD_COLS
        ))?;
        let rows = st.query_map([], Self::thread_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A thread by id, id prefix, or sandbox id.
    pub fn find_thread(&self, query: &str) -> Result<Thread> {
        let all = self.threads()?;
        if let Some(t) = all
            .iter()
            .find(|t| t.id == query || t.sandbox_id.as_deref() == Some(query))
        {
            return Ok(t.clone());
        }
        let hits: Vec<_> = all
            .into_iter()
            .filter(|t| t.id.starts_with(query))
            .collect();
        match hits.len() {
            0 => Err(Error::NotFound(format!("no thread matches {query:?}"))),
            1 => Ok(hits.into_iter().next().unwrap()),
            n => Err(Error::Invalid(format!(
                "{query:?} matches {n} threads; use a longer prefix"
            ))),
        }
    }

    /// Remove a thread and everything stored for it, including its local-only rows.
    pub fn delete_thread(&self, id: &str) -> Result<()> {
        let tx = self.write()?;
        for table in [
            "messages",
            "events",
            "turns",
            "sent_prompts",
            "sync_state",
            "thread_views",
            "thread_opens",
            "thread_archive",
            "summaries",
            "threads",
        ] {
            let col = if table == "threads" {
                "id"
            } else {
                "thread_id"
            };
            tx.execute(&format!("DELETE FROM {table} WHERE {col}=?1"), [id])?;
        }
        tx.execute("DELETE FROM kv WHERE key=?1", [start_error_key(id)])?;
        // What it was asked by a parent, and its own agents: nothing waits on it now.
        for key in [
            format!("orch:awaiting:{id}"),
            format!("parent:{id}"),
            format!("orch:since:{id}"),
        ] {
            tx.execute("DELETE FROM kv WHERE key=?1", [key])?;
        }
        tx.execute(
            "DELETE FROM kv WHERE substr(key, 1, length(?1)) = ?1",
            [format!("side:{id}:")],
        )?;
        // Its hand-offs' key, outcomes, count and kept messages.
        let (keys, prefixes) = crate::orchestrate::thread_keys(id);
        for key in keys.into_iter().chain([
            crate::lifecycle::stopped_by_user_key(id),
            format!("provisioned:{id}"),
        ]) {
            tx.execute("DELETE FROM kv WHERE key=?1", [key])?;
        }
        for prefix in prefixes
            .into_iter()
            .chain(crate::public::thread_prefixes(id))
        {
            tx.execute(
                "DELETE FROM kv WHERE substr(key, 1, length(?1)) = ?1",
                [prefix],
            )?;
        }
        for key in [
            format!("start_stages:{id}"),
            format!("behind:{id}"),
            format!("fresh_vm:{id}"),
            format!("resume:{id}"),
            format!("archive:{id}"),
            format!("thread_setup:{id}"),
            format!("on_branch:{id}"),
            format!("services:{id}"),
            format!("service_ports:{id}"),
            format!("lineage:{id}"),
            format!("context:{id}"),
            format!("spend_cursor:{id}"),
            crate::lifecycle::unsent_key(id),
            crate::lifecycle::unsent_files_key(id),
        ] {
            tx.execute("DELETE FROM kv WHERE key=?1", [key])?;
        }
        tx.execute("DELETE FROM thread_prs WHERE thread_id=?1", [id])?;
        // What was scheduled for it has nowhere to go.
        tx.execute("DELETE FROM schedules WHERE thread_id=?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    // ---- views and activity ----

    /// Record that the thread is on screen now.
    pub fn mark_viewed(&self, thread_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO thread_views (thread_id, viewed_at) VALUES (?1, ?2)
             ON CONFLICT(thread_id) DO UPDATE SET viewed_at=excluded.viewed_at",
            params![thread_id, now_iso()],
        )?;
        Ok(())
    }

    /// Record that the thread was opened at `at` (ms), from a search for `query` if any
    /// (`flotilla_proto::search::record_open`).
    pub fn record_open(&self, thread_id: &str, at: i64, query: Option<&str>) -> Result<()> {
        if self.thread(thread_id)?.is_none() {
            return Ok(());
        }
        let previous = self.opens()?.remove(thread_id);
        let next = flotilla_proto::search::record_open(previous.as_ref(), at, query);
        self.conn.execute(
            "INSERT INTO thread_opens (thread_id, opened_at, score, picks) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(thread_id) DO UPDATE SET opened_at=excluded.opened_at, score=excluded.score, picks=excluded.picks",
            params![thread_id, next.at, next.score, serde_json::to_string(&next.picks).unwrap_or_else(|_| "[]".into())],
        )?;
        Ok(())
    }

    /// Every thread's opens, by its id.
    pub fn opens(&self) -> Result<HashMap<String, flotilla_proto::search::Opens>> {
        let mut st = self
            .conn
            .prepare("SELECT thread_id, opened_at, score, picks FROM thread_opens")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, at, score, picks) = row?;
            out.insert(
                id,
                flotilla_proto::search::Opens {
                    at,
                    score,
                    picks: serde_json::from_str(&picks).unwrap_or_default(),
                },
            );
        }
        Ok(out)
    }

    /// When you last wrote in each thread (ms), by its id: your latest prompt or steer, not one
    /// Flotilla brought (an answer, a task handed over). Threads you never wrote in aren't there.
    pub fn prompted(&self) -> Result<HashMap<String, i64>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT t.id, (SELECT m.timestamp FROM messages m WHERE m.thread_id = t.id AND m.kind IN ('prompt', 'steer') AND {} ORDER BY m.seq DESC LIMIT 1) FROM threads t",
            yours_sql("m")
        ))?;
        let rows = st.query_map(
            [flotilla_model::orchestrate::CHILD_NOTE.trim_start()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)),
        )?;
        let mut out = HashMap::new();
        for row in rows {
            if let (id, Some(at)) = row? {
                out.insert(id, at);
            }
        }
        Ok(out)
    }

    /// Show the thread as unread until it's next opened.
    pub fn mark_unread(&self, thread_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO thread_views (thread_id, viewed_at) VALUES (?1, '1970-01-01T00:00:00.000Z')
             ON CONFLICT(thread_id) DO UPDATE SET viewed_at=excluded.viewed_at",
            [thread_id],
        )?;
        Ok(())
    }

    pub fn is_archived(&self, thread_id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM thread_archive WHERE thread_id=?1",
                [thread_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Archive or unarchive. Coming back from Archived is noted (`tidy::unarchived_key`):
    /// archiving on its own counts from then.
    pub fn set_archived(&self, thread_id: &str, archived: bool) -> Result<()> {
        if archived {
            self.conn.execute(
                "INSERT INTO thread_archive (thread_id, archived_at) VALUES (?1, ?2) ON CONFLICT(thread_id) DO NOTHING",
                params![thread_id, now_iso()],
            )?;
        } else if self
            .conn
            .execute("DELETE FROM thread_archive WHERE thread_id=?1", [thread_id])?
            > 0
        {
            self.kv_set(
                &crate::tidy::unarchived_key(thread_id),
                &crate::util::now_ms().to_string(),
            )?;
        }
        Ok(())
    }

    /// When the newest message went to the thread (a prompt or a steer, sent from here or
    /// read from boat), in ms.
    pub fn last_prompt_ms(&self, thread_id: &str) -> Result<Option<i64>> {
        Ok(self.conn.query_row("SELECT MAX(timestamp) FROM messages WHERE thread_id=?1 AND kind IN ('prompt', 'steer')", [thread_id], |r| r.get(0))?)
    }

    pub fn set_summary(&self, thread_id: &str, kind: &str, source: &str, text: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO summaries (thread_id, kind, source, text, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(thread_id, kind) DO UPDATE SET source=excluded.source, text=excluded.text, created_at=excluded.created_at",
            params![thread_id, kind, source, text, now_iso()],
        )?;
        Ok(())
    }

    /// `(source, text)` of a stored summary.
    pub fn summary(&self, thread_id: &str, kind: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source, text FROM summaries WHERE thread_id=?1 AND kind=?2",
                params![thread_id, kind],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// `(event key, text)` of the agent's latest reply.
    pub fn latest_reply(&self, thread_id: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT event_key, text FROM messages WHERE thread_id=?1 AND kind='text' AND text IS NOT NULL ORDER BY seq DESC LIMIT 1",
                [thread_id],
                |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get(1)?)),
            )
            .optional()?)
    }

    /// The question the thread's agent last asked that still waits on you (an ask message's
    /// text: its questions, a line each).
    pub fn waiting_question(&self, thread_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT text FROM messages WHERE thread_id=?1 AND kind='ask' AND status='waiting' AND text IS NOT NULL ORDER BY seq DESC LIMIT 1",
                [thread_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The text of the thread's first prompt, as sent from this Mac or as boat has it.
    pub fn first_prompt(&self, thread_id: &str) -> Result<Option<String>> {
        let sent: Option<String> = self
            .conn
            .query_row(
                "SELECT text FROM sent_prompts WHERE thread_id=?1 AND kind='prompt' ORDER BY sent_at LIMIT 1",
                [thread_id],
                |r| r.get(0),
            )
            .optional()?;
        if sent.is_some() {
            return Ok(sent);
        }
        Ok(self
            .conn
            .query_row("SELECT text FROM messages WHERE thread_id=?1 AND kind='prompt' ORDER BY seq LIMIT 1", [thread_id], |r| r.get(0))
            .optional()?)
    }

    /// Every thread with its activity group, for the sidebar. Newest activity first.
    pub fn thread_summaries(&self) -> Result<Vec<ThreadSummary>> {
        let mut st = self.conn.prepare(
            "SELECT t.id, v.viewed_at,
               (SELECT MAX(timestamp) FROM events e WHERE e.thread_id = t.id),
               (SELECT MIN(MIN(COALESCE(p.sent_at, u.created_at), u.created_at)) FROM turns u
                  LEFT JOIN sent_prompts p ON p.thread_id = u.thread_id AND p.id = u.prompt_id
                  WHERE u.thread_id = t.id AND u.settled = 0),
               a.thread_id IS NOT NULL,
               (SELECT text FROM messages m WHERE m.thread_id = t.id AND m.kind = 'text' ORDER BY seq DESC LIMIT 1),
               (SELECT s.text FROM summaries s WHERE s.thread_id = t.id AND s.kind = 'reply' AND s.source =
                  (SELECT m.event_key FROM messages m WHERE m.thread_id = t.id AND m.kind = 'text' ORDER BY seq DESC LIMIT 1)),
               (SELECT text FROM messages m WHERE m.thread_id = t.id AND m.kind IN ('prompt', 'steer') ORDER BY seq DESC LIMIT 1),
               (SELECT q.tool_result_json FROM messages m JOIN messages q ON q.thread_id = m.thread_id AND q.kind = 'ask' AND q.tool_id = m.answer_to
                  WHERE m.thread_id = t.id AND m.seq = (SELECT MAX(seq) FROM messages l WHERE l.thread_id = t.id AND l.kind IN ('prompt', 'steer'))),
               (SELECT MIN(sent_at) FROM sent_prompts p WHERE p.thread_id = t.id AND p.id LIKE 'local-%'),
               (SELECT MAX(timestamp) FROM messages m WHERE m.thread_id = t.id AND m.kind = 'ask' AND m.status = 'waiting')
             FROM threads t LEFT JOIN thread_views v ON v.thread_id = t.id
               LEFT JOIN thread_archive a ON a.thread_id = t.id",
        )?;
        type Row = (
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            bool,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
        );
        let rows: Vec<Row> = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let threads: std::collections::HashMap<String, Thread> = self
            .threads()?
            .into_iter()
            .map(|t| (t.id.clone(), t))
            .collect();
        let mut prs = self.pull_requests()?;
        let failed: HashSet<String> = {
            let mut st = self.conn.prepare(
                "SELECT substr(key, 13) FROM kv WHERE key LIKE 'start_error:%' AND value != ''",
            )?;
            let ids = st.query_map([], |r| r.get::<_, String>(0))?;
            ids.collect::<rusqlite::Result<_>>()?
        };
        let (mut prompted, mut opens) = (self.prompted()?, self.opens()?);
        let mut out: Vec<ThreadSummary> = rows
            .into_iter()
            .filter_map(
                |(
                    id,
                    viewed_at,
                    last_event_ms,
                    turn_created,
                    archived,
                    last_reply,
                    reply_summary,
                    last_prompt,
                    answered,
                    sending_since,
                    asked_ms,
                )| {
                    let thread = threads.get(&id)?.clone();
                    // A message still on its way (a VM booting for it, say) is the turn starting:
                    // working from the moment it was sent, unless the start failed.
                    let sending = sending_since.filter(|_| !failed.contains(&id));
                    let mut s = new_summary(
                        thread,
                        viewed_at.as_deref(),
                        last_event_ms,
                        turn_created.as_deref().or(sending.as_deref()),
                        archived,
                        asked_ms,
                    );
                    // A model-written summary of the latest reply, else its first line.
                    s.last_reply =
                        reply_summary.or_else(|| last_reply.as_deref().and_then(preview_line));
                    // Your answer to a question: what you chose (the question is the agent's).
                    let answered = answered
                        .and_then(|j| serde_json::from_str::<Vec<Vec<String>>>(&j).ok())
                        .map(|a| {
                            a.iter()
                                .map(|x| x.join(", "))
                                .collect::<Vec<_>>()
                                .join("; ")
                        });
                    s.last_prompt = answered
                        .or(last_prompt)
                        .filter(|p| p != normalize::PROMPT_NOT_KEPT)
                        .map(|p| crate::util::one_line(&p))
                        .filter(|p| !p.is_empty());
                    s.pr = prs.remove(&s.thread.id);
                    s.start_failed = failed.contains(&s.thread.id);
                    s.prompted_ms = prompted.remove(&s.thread.id);
                    s.opens = opens.remove(&s.thread.id);
                    Some(s)
                },
            )
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
        Ok(out)
    }

    // ---- events ----

    /// The apply step of a poll, in one transaction: upsert every non-partial event by key,
    /// record turns from prompt events, and (when `complete`, i.e. this was a full read of
    /// everything after the cursor) delete unsettled rows boat no longer has.
    pub fn apply_events(
        &self,
        thread_id: &str,
        events: &[RawEvent],
        complete: bool,
    ) -> Result<ApplyResult> {
        let tx = self.write()?;
        let mut result = ApplyResult::default();
        let mut seen = HashSet::new();
        let now = now_iso();
        for e in events.iter().filter(|e| !e.is_partial()) {
            let key = e.key();
            let raw = serde_json::to_string(&e.raw)?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT raw_json FROM events WHERE thread_id=?1 AND key=?2",
                    params![thread_id, key],
                    |r| r.get(0),
                )
                .optional()?;
            match existing {
                None => {
                    tx.execute(
                        "INSERT INTO events (thread_id, key, event_id, task_id, type, timestamp, raw_json, settled, received_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
                        params![thread_id, key, e.id, e.task_id, e.kind, e.timestamp, raw, now],
                    )?;
                    result.changed = true;
                }
                Some(old) if old != raw => {
                    tx.execute(
                        "UPDATE events SET event_id=?3, task_id=?4, type=?5, timestamp=?6, raw_json=?7, received_at=?8
                         WHERE thread_id=?1 AND key=?2",
                        params![thread_id, key, e.id, e.task_id, e.kind, e.timestamp, raw, now],
                    )?;
                    result.changed = true;
                }
                Some(_) => {}
            }
            if e.is_prompt() {
                let status = e.prompt_status().unwrap_or("unknown");
                let old: Option<String> = tx
                    .query_row(
                        "SELECT status FROM turns WHERE thread_id=?1 AND prompt_id=?2",
                        params![thread_id, key],
                        |r| r.get(0),
                    )
                    .optional()?;
                match old {
                    None => {
                        tx.execute(
                            "INSERT INTO turns (thread_id, prompt_id, status, created_at) VALUES (?1, ?2, ?3, ?4)",
                            params![thread_id, key, status, now],
                        )?;
                        if is_terminal_status(status) {
                            result.new_terminal_turns.push(key.clone());
                        }
                    }
                    Some(s) if s != status => {
                        tx.execute(
                            "UPDATE turns SET status=?3 WHERE thread_id=?1 AND prompt_id=?2",
                            params![thread_id, key, status],
                        )?;
                    }
                    Some(_) => {}
                }
            }
            seen.insert(key);
        }
        if complete {
            let unsettled: Vec<String> = {
                let mut st =
                    tx.prepare("SELECT key FROM events WHERE thread_id=?1 AND settled=0")?;
                st.query_map([thread_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for key in unsettled.into_iter().filter(|k| !seen.contains(k)) {
                // The only deletion ever made from `events`: superseded or removed unsettled rows.
                tx.execute(
                    "DELETE FROM events WHERE thread_id=?1 AND key=?2 AND settled=0",
                    params![thread_id, key],
                )?;
                result.changed = true;
            }
        }
        tx.commit()?;
        Ok(result)
    }

    pub fn events(&self, thread_id: &str) -> Result<Vec<StoredEvent>> {
        let mut st = self.conn.prepare(
            "SELECT key, event_id, settled, raw_json FROM events WHERE thread_id=?1 ORDER BY timestamp, event_id",
        )?;
        let rows = st.query_map([thread_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (key, event_id, settled, raw) = row?;
            out.push(StoredEvent {
                key,
                event_id,
                settled,
                event: RawEvent::from_json(&raw)?,
            });
        }
        Ok(out)
    }

    pub fn event(&self, thread_id: &str, key: &str) -> Result<Option<RawEvent>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT raw_json FROM events WHERE thread_id=?1 AND key=?2",
                params![thread_id, key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match raw {
            Some(r) => Some(RawEvent::from_json(&r)?),
            None => None,
        })
    }

    pub fn settle_all_events(&self, thread_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE events SET settled=1 WHERE thread_id=?1 AND settled=0",
            [thread_id],
        )?;
        Ok(())
    }

    /// Where the cursor goes once every turn has settled: the newest stored event by
    /// `(timestamp, event_id)`, never a `qp-` placeholder (partials are never stored). boat
    /// ignores a cursor naming an id it doesn't have and restarts from the first event, so the
    /// cursor must sit on a real event.
    ///
    /// The exception is a tool call still pending after its turn ended: an interrupted turn
    /// keeps its in-flight call, and boat fills in the result later, during a later turn. The
    /// cursor stays just before it so polls keep re-reading it until then.
    pub fn cursor_event(&self, thread_id: &str) -> Result<Option<(i64, String)>> {
        let mut st = self.conn.prepare("SELECT timestamp, event_id, raw_json FROM events WHERE thread_id=?1 ORDER BY timestamp, event_id")?;
        let rows = st.query_map([thread_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut cursor = None;
        for row in rows {
            let (ts, id, raw) = row?;
            if RawEvent::from_json(&raw)?.has_pending_tool() {
                break;
            }
            if !id.starts_with("qp-") {
                cursor = Some((ts, id));
            }
        }
        Ok(cursor)
    }

    // ---- turns ----

    fn turn_row(r: &Row) -> rusqlite::Result<Turn> {
        Ok(Turn {
            thread_id: r.get(0)?,
            prompt_id: r.get(1)?,
            status: r.get(2)?,
            done: r.get(3)?,
            settled: r.get(4)?,
            done_at: r.get(5)?,
            created_at: r.get(6)?,
        })
    }

    /// Record a turn this process just started (`POST /prompt`).
    pub fn insert_turn(&self, thread_id: &str, prompt_id: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO turns (thread_id, prompt_id, status, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(thread_id, prompt_id) DO NOTHING",
            params![thread_id, prompt_id, status, now_iso()],
        )?;
        Ok(())
    }

    pub fn turns(&self, thread_id: &str) -> Result<Vec<Turn>> {
        let mut st = self.conn.prepare(
            "SELECT thread_id, prompt_id, status, done, settled, done_at, created_at FROM turns WHERE thread_id=?1 ORDER BY created_at",
        )?;
        let rows = st.query_map([thread_id], Self::turn_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn unsettled_turns(&self, thread_id: &str) -> Result<Vec<Turn>> {
        Ok(self
            .turns(thread_id)?
            .into_iter()
            .filter(|t| !t.settled)
            .collect())
    }

    pub fn update_turn(&self, t: &Turn) -> Result<()> {
        self.conn.execute(
            "UPDATE turns SET status=?3, done=?4, settled=?5, done_at=?6 WHERE thread_id=?1 AND prompt_id=?2",
            params![t.thread_id, t.prompt_id, t.status, t.done, t.settled, t.done_at],
        )?;
        Ok(())
    }

    // ---- sent prompts ----

    pub fn insert_sent_prompt(
        &self,
        thread_id: &str,
        id: &str,
        kind: &str,
        text: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sent_prompts (thread_id, id, kind, text, sent_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(thread_id, id) DO NOTHING",
            params![thread_id, id, kind, text, now_iso()],
        )?;
        Ok(())
    }

    /// Record a sent prompt or steer under the id boat gave it. With `local`, the placeholder
    /// written when Send was pressed takes that id, keeping its text and place in the thread
    /// (and an answer it took goes with it: `crate::asks::moved`); otherwise, or if the
    /// placeholder is gone, a new row.
    pub fn record_sent_prompt(
        &self,
        thread_id: &str,
        local: Option<&str>,
        id: &str,
        kind: &str,
        text: &str,
    ) -> Result<()> {
        if let Some(local) = local {
            let tx = self.write()?;
            let claimed = tx.execute(
                "UPDATE sent_prompts SET id=?3, kind=?4 WHERE thread_id=?1 AND id=?2",
                params![thread_id, local, id, kind],
            )?;
            if claimed > 0 {
                crate::asks::moved(self, thread_id, local, id)?;
                tx.commit()?;
                return Ok(());
            }
        }
        self.insert_sent_prompt(thread_id, id, kind, text)
    }

    /// The files sent (or being sent) with a prompt.
    pub fn set_sent_attachments(
        &self,
        thread_id: &str,
        id: &str,
        files: &[crate::attach::Attached],
    ) -> Result<()> {
        let json = if files.is_empty() {
            None
        } else {
            Some(serde_json::to_string(files)?)
        };
        self.conn.execute(
            "UPDATE sent_prompts SET attachments=?3 WHERE thread_id=?1 AND id=?2",
            params![thread_id, id, json],
        )?;
        Ok(())
    }

    /// Drop a placeholder whose send failed.
    /// Mark a placeholder ("sending") as not sent: boat never got it (a quit cut the send
    /// short). It keeps its text and place; the thread offers Send again and Discard.
    pub fn mark_unsent(&self, thread_id: &str, id: &str) -> Result<()> {
        let unsent = format!(
            "{}{}",
            crate::lifecycle::UNSENT_PREFIX,
            id.trim_start_matches(crate::lifecycle::LOCAL_PREFIX)
        );
        self.conn.execute(
            "UPDATE sent_prompts SET id=?3 WHERE thread_id=?1 AND id=?2",
            params![thread_id, id, unsent],
        )?;
        Ok(())
    }

    /// The files of every prompt boat hasn't taken: still sending, or not sent.
    pub fn waiting_attachments(&self) -> Result<Vec<crate::attach::Attached>> {
        let mut st = self.conn.prepare("SELECT attachments FROM sent_prompts WHERE attachments IS NOT NULL AND (id LIKE 'local-%' OR id LIKE 'unsent-%')")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            out.extend(
                serde_json::from_str::<Vec<crate::attach::Attached>>(&json?).unwrap_or_default(),
            );
        }
        Ok(out)
    }

    pub fn delete_sent_prompt(&self, thread_id: &str, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM sent_prompts WHERE thread_id=?1 AND id=?2",
            params![thread_id, id],
        )?;
        Ok(())
    }

    pub fn sent_prompts(&self, thread_id: &str) -> Result<Vec<SentPrompt>> {
        let mut st = self.conn.prepare("SELECT id, kind, text, sent_at, attachments, schedule FROM sent_prompts WHERE thread_id=?1 ORDER BY sent_at")?;
        let rows = st.query_map([thread_id], |r| {
            let files: Option<String> = r.get(4)?;
            Ok(SentPrompt {
                id: r.get(0)?,
                kind: r.get(1)?,
                text: r.get(2)?,
                sent_at: r.get(3)?,
                attachments: files
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
                schedule: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Mark a prompt (still its placeholder, or boat's id) as one a schedule sent.
    pub fn set_sent_schedule(&self, thread_id: &str, id: &str, schedule: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sent_prompts SET schedule=?3 WHERE thread_id=?1 AND id=?2",
            params![thread_id, id, schedule],
        )?;
        Ok(())
    }

    // ---- scheduled prompts ----

    /// Make a schedule, or replace what was set for the one with its id. How its runs went
    /// (its last run, count, whether it's failing) and whether it's paused stay. Its next run
    /// and a run waiting on its thread are written as given only when it's new or `retimed`;
    /// otherwise they stay as they are now, so an edit never brings back a run the engine has
    /// claimed since the edit was read.
    pub fn put_schedule(&self, s: &flotilla_proto::Schedule, retimed: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO schedules (id, prompt, target_json, thread_id, when_json, zone, paused, next_ms, waiting_ms, last_json, runs, made_by, created_ms, updated_ms, plan_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
             ON CONFLICT(id) DO UPDATE SET prompt=excluded.prompt, target_json=excluded.target_json, thread_id=excluded.thread_id,
               when_json=excluded.when_json, zone=excluded.zone,
               next_ms=CASE WHEN ?15 THEN excluded.next_ms ELSE schedules.next_ms END,
               waiting_ms=CASE WHEN ?15 THEN excluded.waiting_ms ELSE schedules.waiting_ms END,
               plan_ms=CASE WHEN ?15 THEN excluded.updated_ms ELSE schedules.plan_ms END,
               updated_ms=excluded.updated_ms",
            params![
                s.id,
                s.prompt,
                serde_json::to_string(&s.target)?,
                s.target.thread(),
                serde_json::to_string(&s.when)?,
                s.zone,
                s.paused,
                s.next_ms,
                s.waiting_ms,
                s.last.as_ref().map(serde_json::to_string).transpose()?,
                s.runs,
                s.from,
                s.created_ms,
                s.updated_ms,
                retimed,
            ],
        )?;
        Ok(())
    }

    fn schedule_row(r: &Row) -> rusqlite::Result<Option<flotilla_proto::Schedule>> {
        let json = |i: usize| -> rusqlite::Result<Option<String>> { r.get(i) };
        let (Some(target), Some(when)) = (
            json(2)?.and_then(|j| serde_json::from_str(&j).ok()),
            json(3)?.and_then(|j| serde_json::from_str(&j).ok()),
        ) else {
            // Written by a newer version: left out, not failing the list.
            return Ok(None);
        };
        Ok(Some(flotilla_proto::Schedule {
            id: r.get(0)?,
            prompt: r.get(1)?,
            target,
            when,
            zone: r.get(4)?,
            paused: r.get(5)?,
            next_ms: r.get(6)?,
            waiting_ms: r.get(7)?,
            last: json(8)?.and_then(|j| serde_json::from_str(&j).ok()),
            runs: r.get(9)?,
            from: r.get(10)?,
            created_ms: r.get(11)?,
            updated_ms: r.get(12)?,
        }))
    }

    const SCHEDULE_COLUMNS: &'static str = "id, prompt, target_json, when_json, zone, paused, next_ms, waiting_ms, last_json, runs, made_by, created_ms, updated_ms";

    pub fn schedule(&self, id: &str) -> Result<Option<flotilla_proto::Schedule>> {
        let row = self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM schedules WHERE id=?1",
                    Self::SCHEDULE_COLUMNS
                ),
                [id],
                Self::schedule_row,
            )
            .optional()?;
        Ok(row.flatten())
    }

    /// Every schedule: those due soonest first (one waiting on its thread before them), then
    /// paused ones, newest first, then one-offs done, latest first.
    pub fn schedules(&self) -> Result<Vec<flotilla_proto::Schedule>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {} FROM schedules", Self::SCHEDULE_COLUMNS))?;
        let mut all: Vec<flotilla_proto::Schedule> = st
            .query_map([], Self::schedule_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        all.sort_by_key(|s| match (s.paused, s.waiting_ms, s.next_ms) {
            (false, Some(w), _) => (0, w, s.created_ms),
            (false, None, Some(n)) => (0, n, s.created_ms),
            (true, ..) => (1, -s.updated_ms, s.created_ms),
            (false, None, None) => (
                2,
                -s.last.as_ref().map_or(s.updated_ms, |l| l.at_ms),
                s.created_ms,
            ),
        });
        Ok(all)
    }

    /// Pause it (no next time, nothing waiting), or let it run again from `next_ms`; either
    /// way its failures start afresh. Whether there was one.
    pub fn set_schedule_paused(
        &self,
        id: &str,
        paused: bool,
        next_ms: Option<i64>,
        updated_ms: i64,
    ) -> Result<bool> {
        Ok(self
            .conn
            .execute("UPDATE schedules SET paused=?2, next_ms=?3, waiting_ms=NULL, failing=NULL, updated_ms=?4, plan_ms=?4 WHERE id=?1", params![id, paused, next_ms, updated_ms])?
            > 0)
    }

    pub fn delete_schedule(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM schedules WHERE id=?1", [id])?
            > 0)
    }

    /// When it runs next and whether a run waits on its thread, unless it was changed since
    /// `seen` (its `updated_ms` as read): then what was set then stands. Whether it was written.
    pub fn set_schedule_next(
        &self,
        id: &str,
        next_ms: Option<i64>,
        waiting_ms: Option<i64>,
        seen: i64,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE schedules SET next_ms=?2, waiting_ms=?3 WHERE id=?1 AND updated_ms=?4",
            params![id, next_ms, waiting_ms, seen],
        )? > 0)
    }

    /// Its plan's revision (`plan_ms`): what a run read as it was claimed.
    pub fn schedule_plan(&self, id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT plan_ms FROM schedules WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Note a run (or one that didn't happen) as its last; `ran` counts it. `pause`: pause it,
    /// as its plan and target were then (`plan_ms`, the target it ran for; it can't run as it
    /// is), unless either changed since: an old run's failure never pauses a schedule since
    /// retimed, paused or sent elsewhere.
    pub fn record_run(
        &self,
        id: &str,
        run: &flotilla_proto::Run,
        ran: bool,
        pause: Option<(i64, &flotilla_proto::Target)>,
    ) -> Result<()> {
        let (plan, target) = match pause {
            Some((plan, target)) => (Some(plan), Some(serde_json::to_string(target)?)),
            None => (None, None),
        };
        self.conn.execute(
            "UPDATE schedules SET last_json=?2, runs=runs+?3,
               paused=CASE WHEN plan_ms IS ?4 AND target_json IS ?5 THEN 1 ELSE paused END,
               next_ms=CASE WHEN plan_ms IS ?4 AND target_json IS ?5 THEN NULL ELSE next_ms END,
               waiting_ms=CASE WHEN plan_ms IS ?4 AND target_json IS ?5 THEN NULL ELSE waiting_ms END
             WHERE id=?1",
            params![id, serde_json::to_string(run)?, i64::from(ran), plan, target],
        )?;
        Ok(())
    }

    /// Why its runs fail now (`None`: they work): whether that's news, in one write, so two
    /// runs failing at once for one reason say it once.
    pub fn note_failing(&self, id: &str, why: Option<&str>) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE schedules SET failing=?2 WHERE id=?1 AND failing IS NOT ?2",
            params![id, why],
        )? > 0)
    }

    /// A run noted as `sent` that found its thread busy after all: `last` (the run before it)
    /// back, the count back, and it waits from `waiting_ms` (or as it already waits). Only while
    /// its plan is as it was claimed (`plan`: `plan_ms` then) and that run is still its last: a
    /// schedule paused, retimed or run again since keeps what happened since (an edit of its
    /// prompt or target doesn't call the run off: it waits, and goes as it now says).
    pub fn unsend_run(
        &self,
        id: &str,
        last: Option<&flotilla_proto::Run>,
        waiting_ms: i64,
        plan: i64,
        sent: &flotilla_proto::Run,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE schedules SET last_json=?2, runs=MAX(runs-1, 0), waiting_ms=COALESCE(waiting_ms, ?3) WHERE id=?1 AND plan_ms=?4 AND last_json=?5",
            params![id, last.map(serde_json::to_string).transpose()?, waiting_ms, plan, serde_json::to_string(sent)?],
        )? > 0)
    }

    /// Why its runs fail, while they do.
    pub fn schedule_failing(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT failing FROM schedules WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .flatten())
    }

    /// A schedule that sends somewhere: not paused, with a run to come or one waiting for its
    /// turn (a one-off that's done sends nowhere).
    const SENDING: &'static str = "NOT paused AND (next_ms IS NOT NULL OR waiting_ms IS NOT NULL)";

    /// The threads a schedule sends to (`SENDING`).
    pub fn scheduled_threads(&self) -> Result<HashSet<String>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT DISTINCT thread_id FROM schedules WHERE thread_id IS NOT NULL AND {}",
            Self::SENDING
        ))?;
        let ids = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        Ok(ids)
    }

    /// Archive the thread for sitting idle, in one write with the check that no schedule sends
    /// to it (one made or resumed since it was found idle keeps it). Whether it's archived.
    pub fn archive_unless_scheduled(&self, thread_id: &str) -> Result<bool> {
        let n = self.conn.execute(
            &format!(
                "INSERT INTO thread_archive (thread_id, archived_at) SELECT ?1, ?2
                 WHERE NOT EXISTS (SELECT 1 FROM schedules WHERE thread_id=?1 AND {})
                 ON CONFLICT(thread_id) DO NOTHING",
                Self::SENDING
            ),
            params![thread_id, now_iso()],
        )?;
        Ok(n > 0 || self.is_archived(thread_id)?)
    }

    /// One-offs that ran (or didn't) before `before_ms`: done, and gone from the list.
    pub fn delete_done_schedules(&self, before_ms: i64) -> Result<usize> {
        let done: Vec<String> = self
            .schedules()?
            .into_iter()
            .filter(|s| s.done() && s.last.as_ref().map_or(s.updated_ms, |l| l.at_ms) < before_ms)
            .map(|s| s.id)
            .collect();
        for id in &done {
            self.delete_schedule(id)?;
        }
        Ok(done.len())
    }

    // ---- sync state ----

    pub fn sync_state(&self, thread_id: &str) -> Result<SyncState> {
        Ok(self
            .conn
            .query_row(
                "SELECT cursor, etag, synced_at FROM sync_state WHERE thread_id=?1",
                [thread_id],
                |r| {
                    Ok(SyncState {
                        cursor: r.get(0)?,
                        etag: r.get(1)?,
                        synced_at: r.get(2)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn set_sync_state(&self, thread_id: &str, s: &SyncState) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_state (thread_id, cursor, etag, synced_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(thread_id) DO UPDATE SET cursor=excluded.cursor, etag=excluded.etag, synced_at=excluded.synced_at",
            params![thread_id, s.cursor, s.etag, s.synced_at],
        )?;
        Ok(())
    }

    // ---- messages ----

    /// Re-derive the thread's messages from its events and sent prompts. Only rows that
    /// changed are written (a running turn changes its last few): a row whose mark is the
    /// same holds the same message, and its search entry stays as it is.
    pub fn rebuild_messages(&self, thread_id: &str) -> Result<()> {
        // Read under the write lock: another rebuild (the server's after an answer, the
        // reader's after a question) can't land between what this reads and what it writes.
        let tx = self.write()?;
        let messages = self.normalized(thread_id)?;
        Self::put_messages(&tx, thread_id, &messages)?;
        tx.commit()?;
        Ok(())
    }

    /// The thread's messages as its events, sent prompts and the questions its agent asked
    /// make them now.
    fn normalized(&self, thread_id: &str) -> Result<Vec<Message>> {
        let events: Vec<RawEvent> = self
            .events(thread_id)?
            .into_iter()
            .map(|e| e.event)
            .collect();
        let sent = self.sent_prompts(thread_id)?;
        let asks = crate::asks::of_thread(self, thread_id);
        Ok(normalize::normalize_with(thread_id, &events, &sent, &asks))
    }

    /// Write `messages` as the thread's, on `tx` (a transaction that writes): only what changed.
    fn put_messages(tx: &Connection, thread_id: &str, messages: &[Message]) -> Result<()> {
        let held: HashMap<i64, Option<i64>> = {
            let mut st = tx.prepare("SELECT seq, mark FROM messages WHERE thread_id=?1")?;
            st.query_map([thread_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        {
            // An update, not a replace, for a seq already there: it keeps its id, and the
            // search index's triggers see it (a REPLACE's delete fires none).
            let mut st = tx.prepare(
                "INSERT INTO messages (thread_id, seq, task_id, kind, text, status, model, tool_name, tool_summary,
                   tool_input_json, tool_result_json, tool_result_type, is_error, pending, event_key, timestamp, tool_id, attachments, mark, answer_to, from_flotilla)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
                 ON CONFLICT (thread_id, seq) DO UPDATE SET task_id=excluded.task_id, kind=excluded.kind, text=excluded.text,
                   status=excluded.status, model=excluded.model, tool_name=excluded.tool_name, tool_summary=excluded.tool_summary,
                   tool_input_json=excluded.tool_input_json, tool_result_json=excluded.tool_result_json,
                   tool_result_type=excluded.tool_result_type, is_error=excluded.is_error, pending=excluded.pending,
                   event_key=excluded.event_key, timestamp=excluded.timestamp, tool_id=excluded.tool_id,
                   attachments=excluded.attachments, mark=excluded.mark, answer_to=excluded.answer_to, from_flotilla=excluded.from_flotilla",
            )?;
            for m in messages {
                let mark = flotilla_proto::history::message_mark(m) as i64;
                if held.get(&m.seq) == Some(&Some(mark)) {
                    continue;
                }
                st.execute(params![
                    thread_id,
                    m.seq,
                    m.task_id,
                    m.kind,
                    m.text,
                    m.status,
                    m.model,
                    m.tool_name,
                    m.tool_summary,
                    m.tool_input_json,
                    m.tool_result_json,
                    m.tool_result_type,
                    m.is_error,
                    m.pending,
                    m.event_key,
                    m.timestamp,
                    m.tool_id,
                    if m.attachments.is_empty() {
                        None
                    } else {
                        Some(serde_json::to_string(&m.attachments)?)
                    },
                    mark,
                    m.answer_to,
                    m.from_flotilla
                ])?;
            }
        }
        let now: HashSet<i64> = messages.iter().map(|m| m.seq).collect();
        for seq in held.keys().filter(|s| !now.contains(s)) {
            tx.execute(
                "DELETE FROM messages WHERE thread_id=?1 AND seq=?2",
                params![thread_id, seq],
            )?;
        }
        Ok(())
    }

    /// Whether a thread has a message of `kind` with `status` (a question still waiting, say).
    pub fn has_message(&self, thread_id: &str, kind: &str, status: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM messages WHERE thread_id=?1 AND kind=?2 AND status=?3 LIMIT 1",
                params![thread_id, kind, status],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn messages(&self, thread_id: &str) -> Result<Vec<Message>> {
        self.messages_between(thread_id, 0, i64::MAX)
    }

    /// A thread's messages with seqs from `from` up to (not including) `to`.
    pub fn messages_between(&self, thread_id: &str, from: i64, to: i64) -> Result<Vec<Message>> {
        let mut st = self.conn.prepare(
            "SELECT seq, task_id, kind, text, status, model, tool_name, tool_summary, tool_input_json, tool_result_json,
               tool_result_type, is_error, pending, event_key, timestamp, tool_id, attachments, answer_to, from_flotilla FROM messages
             WHERE thread_id=?1 AND seq >= ?2 AND seq < ?3 ORDER BY seq",
        )?;
        let rows = st.query_map(params![thread_id, from, to], |r| {
            let files: Option<String> = r.get(16)?;
            Ok(Message {
                seq: r.get(0)?,
                task_id: r.get(1)?,
                kind: r.get(2)?,
                text: r.get(3)?,
                status: r.get(4)?,
                model: r.get(5)?,
                tool_name: r.get(6)?,
                tool_summary: r.get(7)?,
                tool_input_json: r.get(8)?,
                tool_result_json: r.get(9)?,
                tool_result_type: r.get(10)?,
                is_error: r.get(11)?,
                pending: r.get(12)?,
                event_key: r.get(13)?,
                timestamp: r.get(14)?,
                tool_id: r.get(15)?,
                attachments: files
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
                cut: Vec::new(),
                answer_to: r.get(17)?,
                from_flotilla: r.get(18)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Each of a thread's messages' (seq, mark), in seq order, without reading the messages:
    /// what a thread's view needs to mark its pages. A row written without a mark gets one now.
    pub fn message_marks(&self, thread_id: &str) -> Result<Vec<(i64, u64)>> {
        let mut st = self
            .conn
            .prepare("SELECT seq, mark FROM messages WHERE thread_id=?1 ORDER BY seq")?;
        let rows: Vec<(i64, Option<i64>)> = st
            .query_map([thread_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (seq, mark) in rows {
            let mark = match mark {
                Some(m) => m as u64,
                None => self
                    .messages_between(thread_id, seq, seq + 1)?
                    .first()
                    .map_or(0, flotilla_proto::history::message_mark),
            };
            out.push((seq, mark));
        }
        Ok(out)
    }

    /// Move the database (and any WAL files) aside to `.bak`, never overwriting an earlier
    /// backup, and return the backup's path. `sent_prompts` is local-only, so the old file is
    /// kept rather than deleted. No other connection may have the file open.
    ///
    /// What isn't boat's to rebuild comes along into the new one: the paired devices (so a
    /// rebuild doesn't unpair them all), the server's id and URL, and the links made public
    /// (`public`), which boat still serves. The new one is made whole beside it first
    /// (`.rebuilt`), then the two moves: cut off between them, the next open finishes it, so a
    /// crash never leaves a database without what was carried.
    pub fn backup(path: &Path) -> Result<PathBuf> {
        // No revoke from outside the server between reading the devices and carrying them.
        let _devices = crate::devices::hold(path)?;
        // No connection opens, and no link goes public or private, until it's done.
        let _gate = gate_write(path);
        Self::prepare_rebuilt(path)?;
        let mut bak = PathBuf::from(format!("{}.bak", path.display()));
        if bak.exists() {
            bak = PathBuf::from(format!(
                "{}.bak-{}",
                path.display(),
                chrono::Utc::now().format("%Y%m%dT%H%M%S")
            ));
        }
        std::fs::write(swap_journal(path), bak.to_string_lossy().as_bytes())?;
        move_db(path, &bak)?;
        // Every connection open before now has the old file (`stale`).
        *REBUILDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(path.to_path_buf())
            .or_default() += 1;
        finish_swap(path)?;
        Ok(bak)
    }

    /// The database that replaces `path`'s, with what's carried, whole at `.rebuilt` (built at
    /// `.rebuilding`, then moved, so a `.rebuilt` is never half made).
    pub(crate) fn prepare_rebuilt(path: &Path) -> Result<PathBuf> {
        let carried = {
            let store = Store::open_unlocked(path)?;
            store.checkpoint()?;
            store.carried()?
        };
        let building = PathBuf::from(format!("{}.rebuilding", path.display()));
        let rebuilt = rebuilt_path(path);
        remove_db(&building);
        remove_db(&rebuilt);
        {
            let store = Store::open_unlocked(&building)?;
            store.carry(&carried)?;
            // One file, no WAL beside it: what's carried can't be left behind in a move, and
            // nothing of another database's WAL can be taken for its own (it's back in WAL mode
            // once opened).
            let mode: String = store
                .conn
                .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
            if !mode.eq_ignore_ascii_case("delete") {
                return Err(crate::Error::Invalid(format!(
                    "couldn't make the rebuilt database one file (journal mode {mode})"
                )));
            }
        }
        remove_db_sidecars(&building);
        move_db(&building, &rebuilt)?;
        Ok(rebuilt)
    }

    /// `flotilla import`: `source` (another Flotilla's database: a copy of a Mac's) becomes the
    /// database at `target`. All of it comes, what's only here included: what was typed, unread
    /// and archived, summaries, projects, settings, hand-offs. Not who may connect: devices and
    /// codes paired with another machine stay behind.
    ///
    /// The target keeps what's its own (its id, adopted ids, URL, Tailscale choice and paired
    /// devices), and adopts the source's id, so the VMs that database's threads and projects
    /// run on are its own now (`owner`). A target with threads or projects of its own is put
    /// aside only with `replace`. The caller holds the target's lock; the source must not be
    /// in use.
    pub fn import(source: &Path, target: &Path, replace: bool) -> Result<Imported> {
        let same = |a: &Path, b: &Path| {
            a.canonicalize()
                .ok()
                .zip(b.canonicalize().ok())
                .is_some_and(|(a, b)| a == b)
        };
        if !source.is_file() {
            return Err(Error::NotFound(format!(
                "{} isn't a file",
                source.display()
            )));
        }
        if same(source, target) {
            return Err(Error::Invalid(
                "That's the database being imported into".into(),
            ));
        }
        // Read only: the copy is what's changed, never the source.
        let from = Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = from.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let flotilla = from.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('threads', 'kv')",
            [],
            |r| r.get::<_, i64>(0),
        )? == 2;
        if version < 1 || !flotilla {
            return Err(Error::Invalid(format!(
                "{} isn't a Flotilla database",
                source.display()
            )));
        }
        if version as usize > MIGRATIONS.len() {
            return Err(Error::Invalid(format!(
                "{} is from a newer Flotilla: update this one first",
                source.display()
            )));
        }
        let copy = PathBuf::from(format!("{}.importing", target.display()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", copy.display()));
        }
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        from.execute("VACUUM INTO ?1", params![copy.to_string_lossy()])?;
        drop(from);
        let result = (|| {
            // No revoke from outside the server between reading the target's devices and
            // carrying them.
            let _devices = crate::devices::hold(target)?;
            let into = Store::open(&copy)?;
            let threads = into.threads()?.len();
            let projects = into.projects()?.len();
            // Its own id is adopted; who connected to it, and where it was reached, aren't ours.
            let source_id = into
                .kv_get(crate::owner::SERVER_ID_KEY)?
                .filter(|id| !id.is_empty());
            let mut adopted = crate::owner::adopted(&into)?;
            adopted.extend(source_id.clone());
            {
                let tx = into.write()?;
                tx.execute("DELETE FROM devices", [])?;
                tx.execute("DELETE FROM pairing_codes", [])?;
                tx.execute(
                    "DELETE FROM kv WHERE substr(key, 1, ?2) = ?1",
                    params![
                        crate::devices::SIGN_IN_PENDING,
                        crate::devices::SIGN_IN_PENDING.len() as i64
                    ],
                )?;
                // Nor is where it moved before: it's here now.
                for key in CARRIED_KV.iter().chain([&crate::owner::MOVED_KEY]) {
                    tx.execute("DELETE FROM kv WHERE key = ?1", params![key])?;
                }
                tx.commit()?;
            }
            let mut replaced = None;
            if target.exists() {
                let mine = Store::open(target)?;
                let (has_threads, has_projects) = (mine.threads()?.len(), mine.projects()?.len());
                if (has_threads > 0 || has_projects > 0) && !replace {
                    return Err(Error::Invalid(format!(
                        "{} already has {has_threads} thread(s) and {has_projects} project(s): pass --replace to put it aside and import anyway",
                        target.display()
                    )));
                }
                mine.checkpoint()?;
                let mut carried = mine.carried()?;
                let own_id = mine.kv_get(crate::owner::SERVER_ID_KEY)?;
                adopted.extend(crate::owner::adopted(&mine)?);
                adopted.retain(|a| Some(a) != own_id.as_ref());
                carried.kv.retain(|(k, _)| k != crate::owner::ADOPTED_KEY);
                into.carry(&carried)?;
                drop(mine);
                replaced = Some(PathBuf::from(format!(
                    "{}.before-import-{}",
                    target.display(),
                    chrono::Utc::now().format("%Y%m%dT%H%M%S")
                )));
            }
            crate::owner::adopt(&into, &adopted)?;
            let server = crate::owner::server_id(&into)?;
            let url = into.kv_get("server_url")?.filter(|u| !u.is_empty());
            into.checkpoint()?;
            drop(into);
            if let Some(aside) = &replaced {
                for suffix in ["", "-wal", "-shm"] {
                    let side = PathBuf::from(format!("{}{suffix}", target.display()));
                    if side.exists() {
                        std::fs::rename(&side, format!("{}{suffix}", aside.display()))?;
                    }
                }
            }
            std::fs::rename(&copy, target)?;
            for suffix in ["-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", copy.display()));
            }
            Ok(Imported {
                threads,
                projects,
                adopted: source_id,
                replaced,
                server,
                url,
            })
        })();
        if result.is_err() {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", copy.display()));
            }
        }
        result
    }

    /// What a rebuild keeps (see [`Store::backup`]).
    fn carried(&self) -> Result<Carried> {
        let devices = self.devices_raw()?;
        let mut kv = Vec::new();
        for key in CARRIED_KV {
            if let Some(v) = self.kv_get(key)? {
                kv.push((key.to_string(), v));
            }
        }
        // What's public on boat stays known, and ends on time (`public`); a sign-in not finished
        // still revokes the device it made.
        // The paired phones' registrations go with their devices.
        for prefix in crate::public::CARRIED_PREFIXES
            .iter()
            .chain([&crate::devices::SIGN_IN_PENDING, &crate::push::TOKEN_PREFIX])
        {
            kv.extend(self.kv_with_prefix(prefix)?);
        }
        Ok(Carried { devices, kv })
    }

    fn carry(&self, c: &Carried) -> Result<()> {
        let tx = self.write()?;
        for d in &c.devices {
            tx.execute(
                "INSERT OR REPLACE INTO devices (id, token_hash, name, platform, created_ms, last_seen_ms, last_ip, scope) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![d.0, d.1, d.2, d.3, d.4, d.5, d.6, d.7],
            )?;
        }
        for (k, v) in &c.kv {
            tx.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![k, v, now_iso()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn devices_raw(&self) -> Result<Vec<DeviceRaw>> {
        let mut st = self.conn.prepare("SELECT id, token_hash, name, platform, created_ms, last_seen_ms, last_ip, scope FROM devices")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Flush the WAL into the main file so the database is a single file (before a rename).
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }
}
