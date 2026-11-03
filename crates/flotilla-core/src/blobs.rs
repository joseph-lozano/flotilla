//! Files clients upload to the engine's machine: attachments and packed skills. Each is kept
//! by its SHA-256 (`b_<hex>`) in `blobs/` beside the database, in a folder only its owner can
//! enter, so the same file sent twice is kept once. The engine reads a prompt's files from
//! here, never from a path a client names.
//!
//! **How long they're kept** ([`Blobs::sweep`]):
//! - a skill's, while it's one of the skills every agent gets;
//! - an attachment boat hasn't taken yet (its prompt still sending, or not sent: Send again and
//!   Try again send it from here), up to [`KEEP_UNSENT`] after it was last uploaded;
//! - anything else (boat has the prompt, or nothing refers to it yet: an upload whose request
//!   hasn't come), [`CLAIM_WITHIN`] after it was last uploaded.

use crate::error::{Error, Result};
use crate::store::Store;
use sha2::Digest;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// An upload nothing needs any more goes this long after it was last uploaded.
pub const CLAIM_WITHIN: Duration = Duration::from_secs(60 * 60);
/// A file a prompt boat hasn't taken refers to is kept this long after it was last uploaded.
pub const KEEP_UNSENT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub use flotilla_proto::wire::Blob;

/// Whether `id` is a blob's id: `b_` and 64 lowercase hex digits (so it's safe in a path).
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("b_").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The blobs folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blobs {
    dir: PathBuf,
}

impl Blobs {
    pub fn new(dir: PathBuf) -> Blobs {
        Blobs { dir }
    }

    /// The folder beside the database `db`.
    pub fn beside(db: &Path) -> Blobs {
        Blobs::new(
            db.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
                .join("blobs"),
        )
    }

    /// The store's (a scratch folder for one in memory).
    pub fn of(store: &Store) -> Blobs {
        match store.path() {
            Some(db) => Blobs::beside(db),
            None => Blobs::new(
                std::env::temp_dir().join(format!("flotilla-blobs-{}", std::process::id())),
            ),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err(Error::Invalid(format!("{id:?} isn't an uploaded file")));
        }
        Ok(self.dir.join(&id[2..]))
    }

    /// Keep `bytes`: its id. Kept already, it counts as uploaded now (for how long it's kept).
    pub fn put(&self, bytes: &[u8]) -> Result<Blob> {
        if bytes.is_empty() {
            return Err(Error::Invalid("An empty file".into()));
        }
        let hex: String = sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let path = self.dir.join(&hex);
        self.make_dir()?;
        let fresh = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now()));
        if fresh.is_err() {
            // Written aside, then moved in: a half-written file never has the name.
            let part = self.dir.join(format!(
                ".{hex}.{}.{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            write_private(&part, bytes)?;
            std::fs::rename(&part, &path)?;
        }
        Ok(Blob {
            id: format!("b_{hex}"),
            size: bytes.len() as u64,
        })
    }

    /// Keep a copy of every file `from` keeps (`flotilla import`): how many came, and how many
    /// didn't read whole (their contents no longer match their names).
    pub fn copy_from(&self, from: &Blobs) -> (usize, usize) {
        let (mut copied, mut skipped) = (0, 0);
        for entry in std::fs::read_dir(&from.dir).into_iter().flatten().flatten() {
            // Files are named by their SHA-256; anything else (a half-written `.…`) isn't one.
            let id = format!("b_{}", entry.file_name().to_string_lossy());
            if !valid_id(&id) {
                continue;
            }
            match std::fs::read(entry.path())
                .map_err(Error::from)
                .and_then(|b| self.put(&b))
            {
                Ok(b) if b.id == id => copied += 1,
                _ => skipped += 1,
            }
        }
        (copied, skipped)
    }

    fn make_dir(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.dir)?;
            if std::fs::metadata(&self.dir)?.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&self.dir)?;
        Ok(())
    }

    /// Its size, if it's kept.
    pub fn size(&self, id: &str) -> Result<Option<u64>> {
        match std::fs::metadata(self.path(id)?) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Its bytes; `NotFound` once it's gone.
    pub fn read(&self, id: &str) -> Result<Vec<u8>> {
        match std::fs::read(self.path(id)?) {
            Ok(b) => Ok(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::NotFound(format!("{id} is no longer kept")))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Delete what's no longer needed (see the module's notes), as of `now`. Returns how many
    /// went.
    pub fn sweep(&self, store: &Store, now: SystemTime) -> Result<usize> {
        let refs = References::read(store)?;
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Ok(0);
        };
        let mut gone = 0;
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(age) = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| now.duration_since(t).ok())
            else {
                continue;
            };
            let id = format!("b_{name}");
            let keep = if name.starts_with('.') {
                // An upload cut short.
                age < CLAIM_WITHIN
            } else if !valid_id(&id) || refs.skills.contains(&id) {
                // Not ours to sweep, or a skill's.
                true
            } else if refs.unsent.contains(&id) {
                age < KEEP_UNSENT
            } else {
                age < CLAIM_WITHIN
            };
            if !keep && std::fs::remove_file(e.path()).is_ok() {
                gone += 1;
            }
        }
        Ok(gone)
    }
}

/// What the database still needs.
struct References {
    skills: HashSet<String>,
    /// Attachments of prompts boat hasn't taken.
    unsent: HashSet<String>,
}

impl References {
    fn read(store: &Store) -> Result<References> {
        let skills = crate::skills::chosen(store)
            .into_iter()
            .map(|s| s.blob)
            .collect();
        let unsent = store
            .waiting_attachments()?
            .into_iter()
            .map(|a| a.blob)
            .filter(|b| !b.is_empty())
            .collect();
        Ok(References { skills, unsent })
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::{Attached, Kind};

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("flotilla-blobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ids_are_checked() {
        let hex = "0123456789abcdef".repeat(4);
        assert!(valid_id(&format!("b_{hex}")));
        for bad in [
            String::new(),
            hex.clone(),
            format!("b_{}", &hex[1..]),
            format!("b_{}", hex.to_uppercase()),
            format!("b_../{}", &hex[3..]),
            "b_".into(),
        ] {
            assert!(!valid_id(&bad), "{bad}");
        }
        let blobs = Blobs::new(scratch());
        assert!(blobs.read("b_../../etc/passwd").is_err());
        assert!(blobs.size("../x").is_err());
    }

    #[test]
    fn put_keeps_one_copy_privately() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let blobs = Blobs::new(root.join("blobs"));
        let a = blobs.put(b"hello").unwrap();
        assert_eq!(
            a.id,
            "b_2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(blobs.put(b"hello").unwrap(), a);
        assert_eq!(blobs.read(&a.id).unwrap(), b"hello");
        assert_eq!(blobs.size(&a.id).unwrap(), Some(5));
        assert_eq!(
            std::fs::metadata(blobs.dir()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(blobs.dir().join(&a.id[2..]))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(blobs.put(b"").is_err());
        let gone = format!("b_{}", "0".repeat(64));
        assert!(matches!(blobs.read(&gone), Err(Error::NotFound(_))));
        assert_eq!(blobs.size(&gone).unwrap(), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// What's kept and what goes, by what refers to it and how long ago it was uploaded.
    #[test]
    fn sweep_expires_what_nothing_needs() {
        let root = scratch();
        let store = Store::open(&root.join("flotilla.db")).unwrap();
        let blobs = Blobs::of(&store);
        assert_eq!(blobs.dir(), root.join("blobs"));
        let file = |b: &Blob| Attached {
            path: "/tmp/x".into(),
            name: "x".into(),
            size: b.size,
            kind: Kind::Text,
            blob: b.id.clone(),
            from: None,
        };
        let [sending, unsent, taken, loose, skill] =
            ["sending", "unsent", "taken", "loose", "skill"]
                .map(|s| blobs.put(s.as_bytes()).unwrap());
        for (id, b) in [
            ("local-1", &sending),
            ("unsent-2", &unsent),
            ("prompt_3", &taken),
        ] {
            store.insert_sent_prompt("t1", id, "prompt", "hi").unwrap();
            store.set_sent_attachments("t1", id, &[file(b)]).unwrap();
        }
        crate::skills::put(&store, crate::skills::tests_skill("review", &skill.id)).unwrap();
        let exists = |b: &Blob| blobs.size(&b.id).unwrap().is_some();

        // Just uploaded: everything stays.
        let now = SystemTime::now();
        assert_eq!(blobs.sweep(&store, now).unwrap(), 0);
        // An hour on, what boat has and what nothing refers to go.
        assert_eq!(
            blobs
                .sweep(&store, now + CLAIM_WITHIN + Duration::from_secs(1))
                .unwrap(),
            2
        );
        assert!(exists(&sending) && exists(&unsent) && exists(&skill));
        assert!(!exists(&taken) && !exists(&loose));
        // A week on, prompts never sent let go of theirs; a skill's stays while it's chosen.
        assert_eq!(
            blobs
                .sweep(&store, now + KEEP_UNSENT + Duration::from_secs(1))
                .unwrap(),
            2
        );
        assert!(exists(&skill));
        crate::skills::remove(&store, "review").unwrap();
        assert_eq!(
            blobs
                .sweep(&store, now + CLAIM_WITHIN + Duration::from_secs(1))
                .unwrap(),
            1
        );
        // Uploading it again counts from then.
        let again = blobs.put(b"skill").unwrap();
        assert_eq!(
            blobs.sweep(&store, now + Duration::from_secs(1)).unwrap(),
            0
        );
        assert!(exists(&again));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
