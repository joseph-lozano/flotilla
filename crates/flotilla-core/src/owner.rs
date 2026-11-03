//! Whose VMs are whose, when more than one Flotilla uses a boat account (the app on a Mac and
//! a server on a Linux box, say). Each database is one Flotilla, with its own id (kv
//! `server_id`); every VM it makes is labelled with that id (`flotilla.server`), and it lists,
//! syncs, orchestrates and cleans up only its own. Another's are left alone, so a hand-off,
//! a wake or a setup run never happens twice.
//!
//! boat sets labels only when a VM is made (a `PATCH` with labels is refused), so a VM made
//! before Flotilla labelled its own can't be labelled later. Such a VM belongs to the
//! database that already knows it: its thread (`flotilla.thread`) or its project
//! (`flotilla.setup`) is in that database. A database that doesn't know it never takes it in.
//!
//! `flotilla import` moves a database to a server: the server keeps its own id and adopts the
//! one the database had (kv `servers_adopted`), so the VMs that database's threads and
//! projects run on are the server's from then on.
//!
//! A move is one way. The database left behind says where it went (kv `moved_to`), and no
//! engine starts on it again, so two Flotillas never drive the same VMs. It's marked by the
//! import itself when the database is on the same machine, and otherwise by the app, the first
//! time it connects to a server that adopted its database's id (the server's welcome lists
//! them). Importing the server's database back is the move the other way.

use crate::Result;
use crate::boat::Sandbox;
use crate::store::Store;
use rusqlite::{Connection, OpenFlags, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// The label on every VM a Flotilla makes: its id.
pub const SERVER_LABEL: &str = "flotilla.server";
/// Which machine that Flotilla runs on, to say whose a VM is ("on box").
pub const HOST_LABEL: &str = "flotilla.host";
/// The kv entry holding this database's Flotilla id.
pub const SERVER_ID_KEY: &str = "server_id";
/// The ids of the Flotillas whose databases were imported into this one.
pub const ADOPTED_KEY: &str = "servers_adopted";
/// Where this database's threads went, once it was imported into another Flotilla.
pub const MOVED_KEY: &str = "moved_to";

/// Where a database's threads and projects went (`flotilla import`): that Flotilla runs them
/// now, so no engine runs on this database again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    /// The Flotilla it went to.
    pub server: String,
    /// Where that one is reached, if it was known.
    #[serde(default)]
    pub url: Option<String>,
    pub at: String,
}

impl Moved {
    pub fn to(server: &str, url: Option<String>) -> Moved {
        Moved {
            server: server.to_string(),
            url: url.filter(|u| !u.is_empty()),
            at: crate::util::now_iso(),
        }
    }

    /// Where it went, for a person: its address, else its id.
    pub fn place(&self) -> String {
        self.url
            .clone()
            .unwrap_or_else(|| format!("Flotilla {}", self.server))
    }

    /// Why no engine starts on it.
    pub fn reason(&self) -> String {
        format!(
            "This database moved to {} with `flotilla import`, which runs its threads now",
            self.place()
        )
    }
}

/// Where the database at `db` moved, if it did. Reads only, and never makes a database.
pub fn moved(db: &Path) -> Option<Moved> {
    read_kv(db, MOVED_KEY).and_then(|v| serde_json::from_str(&v).ok())
}

/// The Flotilla id the database at `db` has, if it has one yet. Reads only.
pub fn recorded_id(db: &Path) -> Option<String> {
    read_kv(db, SERVER_ID_KEY).filter(|id| !id.is_empty())
}

/// Say that the database at `db` moved to `to`. It must exist: none is made.
pub fn mark_moved(db: &Path, to: &Moved) -> Result<()> {
    existing(db)?.execute(
        "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        rusqlite::params![
            MOVED_KEY,
            serde_json::to_string(to)?,
            crate::util::now_iso()
        ],
    )?;
    Ok(())
}

/// A connection to the database at `db` if there is one: opened without creating it (or
/// migrating it), so asking about a database never makes one.
fn existing(db: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

fn read_kv(db: &Path, key: &str) -> Option<String> {
    existing(db)
        .ok()?
        .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

/// This database's Flotilla id, made the first time it's asked for.
pub fn server_id(store: &Store) -> Result<String> {
    if let Some(id) = store.kv_get(SERVER_ID_KEY)?.filter(|id| !id.is_empty()) {
        return Ok(id);
    }
    let id = format!("srv_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    // Two first askers at once (the host and the engine): the first one written wins.
    store.conn().execute(
        "INSERT OR IGNORE INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![SERVER_ID_KEY, id, crate::util::now_iso()],
    )?;
    Ok(store.kv_get(SERVER_ID_KEY)?.unwrap_or(id))
}

/// The ids this database adopted (see the module's note).
pub fn adopted(store: &Store) -> Result<Vec<String>> {
    Ok(store
        .kv_get(ADOPTED_KEY)?
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default())
}

/// Adopt `ids` too.
pub fn adopt(store: &Store, ids: &[String]) -> Result<()> {
    let mut all = adopted(store)?;
    for id in ids {
        if !id.is_empty() && !all.contains(id) {
            all.push(id.clone());
        }
    }
    store.kv_set(
        ADOPTED_KEY,
        &serde_json::to_string(&all).unwrap_or_else(|_| "[]".into()),
    )
}

/// This machine's name as a label value: letters, digits, `.`, `_` and `-`, at most 63.
pub fn host_label() -> String {
    let name: String = crate::util::machine_name()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(63)
        .collect();
    if name.trim_matches('-').is_empty() {
        "unknown".into()
    } else {
        name
    }
}

/// Whose a VM is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Whose {
    Mine,
    /// Another Flotilla's: its id and host, from the labels.
    Other {
        server: String,
        host: Option<String>,
    },
    /// Made before Flotilla labelled its VMs, and this database doesn't know it.
    Older,
}

/// What a database owns, read once: its id, the ids it adopted, and the threads and projects
/// it knows.
#[derive(Debug, Clone)]
pub struct Owner {
    pub id: String,
    adopted: HashSet<String>,
    threads: HashSet<String>,
    projects: HashSet<String>,
}

impl Owner {
    pub fn load(store: &Store) -> Result<Owner> {
        Ok(Owner {
            id: server_id(store)?,
            adopted: adopted(store)?.into_iter().collect(),
            threads: store.threads()?.into_iter().map(|t| t.id).collect(),
            projects: store.projects()?.into_iter().map(|p| p.id).collect(),
        })
    }

    /// The labels every VM this Flotilla makes carries.
    pub fn labels(&self) -> Vec<(String, String)> {
        vec![
            (SERVER_LABEL.to_string(), self.id.clone()),
            (HOST_LABEL.to_string(), host_label()),
        ]
    }

    pub fn whose(&self, sb: &Sandbox) -> Whose {
        let knows = || {
            sb.label("flotilla.thread")
                .is_some_and(|t| self.threads.contains(&t))
                || sb
                    .label(crate::setup::SETUP_LABEL)
                    .is_some_and(|p| self.projects.contains(&p))
        };
        match sb.label(SERVER_LABEL).filter(|s| !s.is_empty()) {
            Some(s) if s == self.id => Whose::Mine,
            // An imported database's: its threads and projects came along.
            Some(s) if self.adopted.contains(&s) && knows() => Whose::Mine,
            Some(server) => Whose::Other {
                server,
                host: sb.label(HOST_LABEL),
            },
            None if knows() => Whose::Mine,
            None => Whose::Older,
        }
    }

    pub fn mine(&self, sb: &Sandbox) -> bool {
        self.whose(sb) == Whose::Mine
    }

    /// Delete VM `id`, a VM this database has a record of, if it's this Flotilla's. `Ok(false)`:
    /// it isn't (a copy of this database elsewhere has records of another's VMs), or it's gone.
    pub fn delete_own(
        &self,
        boat: &crate::boat::BoatClient,
        id: &str,
    ) -> std::result::Result<bool, crate::BoatError> {
        let gone = |e: &crate::BoatError| e.status() == Some(404);
        match boat.get_sandbox(id) {
            Ok(vm) if self.mine(&vm) => match boat.delete_sandbox(id) {
                Ok(()) => Ok(true),
                Err(e) if gone(&e) => Ok(false),
                Err(e) => Err(e),
            },
            Ok(_) => Ok(false),
            Err(e) if gone(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The VMs among `vms` that aren't this Flotilla's, as a sentence for the setup checks
    /// ("3 VMs on this boat account belong to Flotilla on box (2) and an older Flotilla
    /// (1)"), or None when every one is.
    pub fn others(&self, vms: &[Sandbox]) -> Option<String> {
        let mut by: BTreeMap<String, usize> = BTreeMap::new();
        let mut total = 0;
        for sb in vms {
            let who = match self.whose(sb) {
                Whose::Mine => continue,
                Whose::Other {
                    server,
                    host: Some(h),
                } => format!("Flotilla on {h} ({server})"),
                Whose::Other { server, host: None } => format!("Flotilla {server}"),
                Whose::Older => "an older Flotilla".to_string(),
            };
            *by.entry(who).or_default() += 1;
            total += 1;
        }
        if total == 0 {
            return None;
        }
        let whose: Vec<String> = if by.len() == 1 {
            by.into_keys().collect()
        } else {
            by.into_iter()
                .map(|(who, n)| format!("{who}: {n}"))
                .collect()
        };
        let vms = if total == 1 {
            "1 VM on this boat account belongs".to_string()
        } else {
            format!("{total} VMs on this boat account belong")
        };
        Some(format!(
            "{vms} to {}. This one leaves them alone",
            join_and(&whose)
        ))
    }
}

fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vm(id: &str, labels: serde_json::Value) -> Sandbox {
        serde_json::from_value(json!({"id": id, "state": "idle", "labels": labels})).unwrap()
    }

    fn thread(id: &str) -> crate::store::Thread {
        crate::store::Thread {
            id: id.into(),
            environment_id: None,
            environment_name: None,
            title: "t".into(),
            sandbox_id: None,
            conversation_id: None,
            provider: "claude-code".into(),
            model: "haiku".into(),
            effort: None,
            fast: false,
            state: crate::store::ThreadState::Idle,
            keep_running: false,
            archive_after: None,
            last_stopped_at: None,
            data_expired_at: None,
            created_at: crate::util::now_iso(),
            updated_at: crate::util::now_iso(),
            branch: None,
            repo_project_id: None,
            base_sha: None,
        }
    }

    #[test]
    fn the_id_is_made_once_and_kept() {
        let store = Store::open_in_memory().unwrap();
        let id = server_id(&store).unwrap();
        assert!(id.starts_with("srv_") && id.len() == 16, "{id}");
        assert_eq!(server_id(&store).unwrap(), id);
    }

    #[test]
    fn a_vm_is_mine_by_my_label_or_by_knowing_an_unlabelled_one() {
        let store = Store::open_in_memory().unwrap();
        store.kv_set(SERVER_ID_KEY, "srv_me").unwrap();
        store.insert_thread(&thread("t-known")).unwrap();
        let owner = Owner::load(&store).unwrap();
        // Mine by label, whether or not this database knows the thread yet (a rebuild).
        assert_eq!(
            owner.whose(&vm(
                "bx_1",
                json!({"flotilla": "1", "flotilla.thread": "t-new", "flotilla.server": "srv_me"})
            )),
            Whose::Mine
        );
        // Another's, even for a thread this database knows (a copied database).
        assert_eq!(
            owner.whose(&vm("bx_2", json!({"flotilla": "1", "flotilla.thread": "t-known", "flotilla.server": "srv_box", "flotilla.host": "box"}))),
            Whose::Other { server: "srv_box".into(), host: Some("box".into()) }
        );
        // Made before labels: mine only if this database knows it.
        assert_eq!(
            owner.whose(&vm(
                "bx_3",
                json!({"flotilla": "1", "flotilla.thread": "t-known"})
            )),
            Whose::Mine
        );
        assert_eq!(
            owner.whose(&vm(
                "bx_4",
                json!({"flotilla": "1", "flotilla.thread": "t-other"})
            )),
            Whose::Older
        );
        assert_eq!(
            owner.whose(&vm(
                "bx_5",
                json!({"flotilla.setup": "p-other", "flotilla.run": "r"})
            )),
            Whose::Older
        );
        assert_eq!(
            owner.labels()[0],
            (SERVER_LABEL.to_string(), "srv_me".to_string())
        );
    }

    #[test]
    fn an_adopted_id_brings_only_what_this_database_knows() {
        let store = Store::open_in_memory().unwrap();
        store.kv_set(SERVER_ID_KEY, "srv_box").unwrap();
        store.insert_thread(&thread("t-moved")).unwrap();
        adopt(&store, &["srv_mac".into()]).unwrap();
        adopt(&store, &["srv_mac".into(), "".into()]).unwrap();
        assert_eq!(adopted(&store).unwrap(), ["srv_mac"]);
        let owner = Owner::load(&store).unwrap();
        assert_eq!(
            owner.whose(&vm(
                "bx_1",
                json!({"flotilla.thread": "t-moved", "flotilla.server": "srv_mac"})
            )),
            Whose::Mine
        );
        // One the Mac made after the copy is still the Mac's.
        assert!(matches!(
            owner.whose(&vm(
                "bx_2",
                json!({"flotilla.thread": "t-later", "flotilla.server": "srv_mac"})
            )),
            Whose::Other { .. }
        ));
    }

    #[test]
    fn others_say_whose_they_are() {
        let store = Store::open_in_memory().unwrap();
        store.kv_set(SERVER_ID_KEY, "srv_me").unwrap();
        let owner = Owner::load(&store).unwrap();
        let mine = vm("bx_0", json!({"flotilla.server": "srv_me"}));
        assert_eq!(owner.others(std::slice::from_ref(&mine)), None);
        let box1 = vm(
            "bx_1",
            json!({"flotilla.server": "srv_box", "flotilla.host": "box"}),
        );
        assert_eq!(
            owner.others(&[mine.clone(), box1.clone()]).unwrap(),
            "1 VM on this boat account belongs to Flotilla on box (srv_box). This one leaves them alone"
        );
        let old = vm("bx_2", json!({"flotilla.thread": "t"}));
        assert_eq!(
            owner.others(&[mine, box1.clone(), box1, old]).unwrap(),
            "3 VMs on this boat account belong to Flotilla on box (srv_box): 2 and an older Flotilla: 1. This one leaves them alone"
        );
    }

    /// `flotilla import`: everything comes but who may connect; the server keeps its own id,
    /// devices and settings for this machine, and adopts the Mac's id, so the Mac's VMs for
    /// those threads are its now.
    #[test]
    fn an_import_moves_the_threads_and_their_vms_but_not_the_devices() {
        let dir = std::env::temp_dir().join(format!("flotilla-import-{}", uuid::Uuid::new_v4()));
        let (mac_db, box_db) = (dir.join("mac/flotilla.db"), dir.join("box/flotilla.db"));
        let now = crate::util::now_ms();
        let mac_token = {
            let mac = Store::open(&mac_db).unwrap();
            mac.kv_set(SERVER_ID_KEY, "srv_mac").unwrap();
            mac.kv_set("server_url", "http://127.0.0.1:7777").unwrap();
            mac.kv_set(crate::tailscale::KEY, "off").unwrap();
            mac.kv_set(crate::summarize::MODEL_KEY, "claude-sonnet-5-5")
                .unwrap();
            mac.insert_thread(&thread("t-mac")).unwrap();
            mac.insert_sent_prompt("t-mac", "p1", "prompt", "what I typed")
                .unwrap();
            mac.mark_unread("t-mac").unwrap();
            let c = mac
                .mint_code(std::time::Duration::from_secs(300), "cli", now)
                .unwrap();
            let token = mac
                .pair_device(&c.code, "Phone", "ios", None, now)
                .unwrap()
                .token;
            mac.mint_code(std::time::Duration::from_secs(300), "cli", now)
                .unwrap();
            token
        };
        let box_token = {
            let b = Store::open(&box_db).unwrap();
            b.kv_set(SERVER_ID_KEY, "srv_box").unwrap();
            b.kv_set("server_url", "https://box.t.ts.net").unwrap();
            b.kv_set(crate::tailscale::KEY, "on").unwrap();
            let c = b
                .mint_code(std::time::Duration::from_secs(300), "cli", now)
                .unwrap();
            b.pair_device(&c.code, "MacBook", "macos", None, now)
                .unwrap()
                .token
        };

        // A skill's file, kept as uploads are.
        let skill = crate::blobs::Blobs::beside(&mac_db)
            .put(b"a packed skill")
            .unwrap();
        let got = Store::import(&mac_db, &box_db, false).unwrap();
        // `flotilla import` copies the files beside it too.
        let (to, from) = (
            crate::blobs::Blobs::beside(&box_db),
            crate::blobs::Blobs::beside(&mac_db),
        );
        std::fs::write(from.dir().join(".half-written"), b"x").unwrap();
        assert_eq!(to.copy_from(&from), (1, 0));
        assert_eq!(to.read(&skill.id).unwrap(), b"a packed skill");
        assert_eq!(
            (
                got.threads,
                got.projects,
                got.adopted.as_deref(),
                got.replaced.is_some()
            ),
            (1, 0, Some("srv_mac"), true)
        );
        let b = Store::open(&box_db).unwrap();
        assert_eq!(
            b.thread("t-mac").unwrap().map(|t| t.id).as_deref(),
            Some("t-mac")
        );
        assert_eq!(b.sent_prompts("t-mac").unwrap()[0].text, "what I typed");
        assert_eq!(
            b.kv_get(crate::summarize::MODEL_KEY).unwrap().as_deref(),
            Some("claude-sonnet-5-5")
        );
        // Its own id, URL, Tailscale and devices; not the Mac's.
        assert_eq!(server_id(&b).unwrap(), "srv_box");
        assert_eq!(
            b.kv_get("server_url").unwrap().as_deref(),
            Some("https://box.t.ts.net")
        );
        assert_eq!(
            b.kv_get(crate::tailscale::KEY).unwrap().as_deref(),
            Some("on")
        );
        assert!(b.authenticate(&box_token, None, now).is_ok());
        assert!(b.authenticate(&mac_token, None, now).is_err());
        assert!(b.unused_codes().unwrap().is_empty());
        // The Mac's VMs for its threads are the box's now.
        let owner = Owner::load(&b).unwrap();
        assert!(owner.mine(&vm(
            "bx_1",
            json!({"flotilla.thread": "t-mac", "flotilla.server": "srv_mac"})
        )));
        // The source is as it was.
        let mac = Store::open(&mac_db).unwrap();
        assert_eq!(mac.devices(now).unwrap().len(), 1);
        assert_eq!(
            mac.kv_get(SERVER_ID_KEY).unwrap().as_deref(),
            Some("srv_mac")
        );
        drop((b, mac));

        // Now the box has threads: only --replace puts them aside.
        let e = Store::import(&mac_db, &box_db, false).unwrap_err();
        assert!(e.to_string().contains("--replace"), "{e}");
        let again = Store::import(&mac_db, &box_db, true).unwrap();
        assert!(again.replaced.as_ref().is_some_and(|p| p.exists()));
        assert_eq!(
            adopted(&Store::open(&box_db).unwrap()).unwrap(),
            ["srv_mac"]
        );
        // Not a database, or itself: refused.
        std::fs::write(dir.join("notes.txt"), "hello").unwrap();
        assert!(Store::import(&dir.join("notes.txt"), &box_db, true).is_err());
        assert!(Store::import(&box_db, &box_db, true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A database left behind by an import says where it went, read without making or
    /// migrating anything; the copy the server keeps doesn't say it moved.
    #[test]
    fn a_moved_database_says_where_and_its_copy_doesnt() {
        let dir = std::env::temp_dir().join(format!("flotilla-moved-{}", uuid::Uuid::new_v4()));
        let (mac_db, box_db, none) = (
            dir.join("mac/flotilla.db"),
            dir.join("box/flotilla.db"),
            dir.join("none/flotilla.db"),
        );
        {
            let mac = Store::open(&mac_db).unwrap();
            mac.kv_set(SERVER_ID_KEY, "srv_mac").unwrap();
            mac.insert_thread(&thread("t-mac")).unwrap();
            let b = Store::open(&box_db).unwrap();
            b.kv_set(SERVER_ID_KEY, "srv_box").unwrap();
            b.kv_set("server_url", "https://box.t.ts.net").unwrap();
        }
        assert_eq!(
            (recorded_id(&mac_db).as_deref(), moved(&mac_db)),
            (Some("srv_mac"), None)
        );
        // No database there: nothing to say, and none is made.
        assert_eq!((recorded_id(&none), moved(&none)), (None, None));
        assert!(mark_moved(&none, &Moved::to("srv_box", None)).is_err());
        assert!(!none.exists());

        // An earlier move, then the import here: the box's copy carries no marker, and says
        // which Flotilla the source went to.
        mark_moved(&mac_db, &Moved::to("srv_old", None)).unwrap();
        assert_eq!(
            moved(&mac_db).map(|m| m.place()).as_deref(),
            Some("Flotilla srv_old")
        );
        let got = Store::import(&mac_db, &box_db, false).unwrap();
        assert_eq!(
            (got.server.as_str(), got.url.as_deref()),
            ("srv_box", Some("https://box.t.ts.net"))
        );
        assert_eq!(moved(&box_db), None);
        mark_moved(&mac_db, &Moved::to(&got.server, got.url)).unwrap();
        let m = moved(&mac_db).unwrap();
        assert_eq!(
            (m.server.as_str(), m.place().as_str()),
            ("srv_box", "https://box.t.ts.net")
        );
        assert!(
            m.reason().contains("https://box.t.ts.net"),
            "{}",
            m.reason()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_labels_are_label_safe() {
        assert!(
            host_label()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        );
    }
}
