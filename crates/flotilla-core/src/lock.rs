//! One engine per database. Whatever runs one (`flotilla serve`, the app's own host, a CLI
//! command that polls or writes) holds an exclusive lock on a file beside the database for as
//! long as it runs, so a second can't start beside it and carry out every hand-off twice. The
//! file also says who holds it, and where a server listens, so the CLI can go through that
//! server instead of writing the database behind its back.
//!
//! The lock is the OS's (`flock`): it goes when the process does, however it ends, and the
//! moment it's dropped.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

/// What the loser of the lock says.
pub const BUSY: &str = "Another Flotilla is already running on this database";

/// Who holds a database's lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    /// `server`, `app` or `cli`.
    pub kind: String,
    /// Where a server listens: `ws://127.0.0.1:7777` or `unix:/path/to/socket`.
    #[serde(default)]
    pub address: Option<String>,
}

impl Holder {
    pub fn new(kind: &str) -> Holder {
        Holder {
            pid: std::process::id(),
            kind: kind.to_string(),
            address: None,
        }
    }
}

/// The lock, held until dropped.
#[derive(Debug)]
pub struct Lock {
    file: File,
    path: PathBuf,
}

/// The lock file for `db`.
pub fn path_for(db: &Path) -> PathBuf {
    let mut name = db
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "flotilla.db".into());
    name.push(".lock");
    db.with_file_name(name)
}

impl Lock {
    /// Take `db`'s lock for `holder`, or say who has it.
    pub fn take(db: &Path, holder: &Holder) -> Result<Lock, Busy> {
        let path = path_for(db);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Busy::Io(format!("{}: {e}", path.display())))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(Busy::Held(read_holder(&path))),
            Err(TryLockError::Error(e)) => {
                return Err(Busy::Io(format!("{}: {e}", path.display())));
            }
        }
        let lock = Lock { file, path };
        lock.write(holder)
            .map_err(|e| Busy::Io(format!("{}: {e}", lock.path.display())))?;
        Ok(lock)
    }

    /// Say who holds it now (a server's address, once it listens).
    pub fn write(&self, holder: &Holder) -> std::io::Result<()> {
        let mut f = &self.file;
        f.set_len(0)?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(serde_json::to_string(holder).unwrap_or_default().as_bytes())?;
        f.flush()
    }
}

impl Drop for Lock {
    /// Unlocked, not just closed: a process another thread is starting has a copy of the file
    /// until it runs its program, and a closed lock holds until the last copy closes. That
    /// would refuse the next taker for no one.
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Why the lock wasn't taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Busy {
    /// Someone has it (and who, if the file says).
    Held(Option<Holder>),
    Io(String),
}

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Busy::Held(_) => f.write_str(BUSY),
            Busy::Io(e) => write!(f, "Couldn't lock the database: {e}"),
        }
    }
}

/// The database, for what a lock's holder does off its own thread (the setup checks, a name
/// it suggests): open while the holder runs, and closed as it stops, before it lets go of the
/// lock. Work still under way then (a slow network check, say) can't use the database after,
/// so it never opens it once another engine may hold it, or makes its folder again once it's
/// gone. Closing waits for whatever is using the database through it right then.
#[derive(Debug, Clone, Default)]
pub struct Gate(Arc<RwLock<bool>>);

impl Gate {
    /// `f`, which uses the database, unless the gate is closed (`None`). It can't close while
    /// `f` runs. Not to be called again from inside `f`.
    pub fn with<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let closed = self.0.read().unwrap_or_else(PoisonError::into_inner);
        (!*closed).then(f)
    }

    /// Close it for good, once what's using the database through it now is done.
    pub fn close(&self) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = true;
    }
}

/// Who holds `db`'s lock, if anyone does now.
pub fn holder(db: &Path) -> Option<Holder> {
    let path = path_for(db);
    let file = File::open(&path).ok()?;
    match file.try_lock_shared() {
        // No one: let go at once.
        Ok(()) => {
            let _ = file.unlock();
            None
        }
        Err(TryLockError::WouldBlock) => Some(read_holder(&path).unwrap_or_else(|| Holder {
            pid: 0,
            kind: "unknown".into(),
            address: None,
        })),
        Err(TryLockError::Error(_)) => None,
    }
}

fn read_holder(path: &Path) -> Option<Holder> {
    let mut text = String::new();
    File::open(path).ok()?.read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_gate_runs_nothing_and_closing_waits_for_whats_running() {
        let gate = Gate::default();
        assert_eq!(gate.with(|| 1), Some(1));
        let (started, running) = std::sync::mpsc::channel();
        let (finish, finished) = std::sync::mpsc::channel::<()>();
        let inside = gate.clone();
        let user = std::thread::spawn(move || {
            inside.with(|| {
                started.send(()).unwrap();
                finished.recv().unwrap();
            })
        });
        running.recv().unwrap();
        let closing = gate.clone();
        let closer = std::thread::spawn(move || closing.close());
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !closer.is_finished(),
            "closed while the database was in use"
        );
        finish.send(()).unwrap();
        assert_eq!(user.join().unwrap(), Some(()));
        closer.join().unwrap();
        assert_eq!(gate.with(|| 2), None);
    }

    #[test]
    fn one_holder_at_a_time_and_the_file_says_who() {
        let dir = std::env::temp_dir().join(format!("flotilla-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("flotilla.db");
        assert_eq!(holder(&db), None);
        let lock = Lock::take(&db, &Holder::new("server")).unwrap();
        // A second taker (another open file, as another process would have) loses.
        let busy = Lock::take(&db, &Holder::new("app")).unwrap_err();
        assert!(matches!(&busy, Busy::Held(Some(h)) if h.kind == "server"));
        assert_eq!(busy.to_string(), BUSY);
        lock.write(&Holder {
            address: Some("ws://127.0.0.1:7777".into()),
            ..Holder::new("server")
        })
        .unwrap();
        assert_eq!(
            holder(&db).and_then(|h| h.address).as_deref(),
            Some("ws://127.0.0.1:7777")
        );
        drop(lock);
        assert_eq!(holder(&db), None);
        assert!(Lock::take(&db, &Holder::new("app")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Processes started meanwhile (a Keychain lookup, `git`) hold a copy of the lock's file
    /// for a moment: one let go is still free to take at once.
    #[test]
    fn a_lock_let_go_is_free_at_once_while_processes_start() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let dir = std::env::temp_dir().join(format!("flotilla-lock-spawn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = dir.join("flotilla.db");
        let (stop, started) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU32::new(0)),
        );
        let (s, n) = (stop.clone(), started.clone());
        let starting = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                std::process::Command::new("true")
                    .status()
                    .expect("start `true`");
                n.fetch_add(1, Ordering::Relaxed);
            }
        });
        let mut refused = 0;
        let mut tries = 0;
        while started.load(Ordering::Relaxed) < 20 && !starting.is_finished() && tries < 1_000_000 {
            tries += 1;
            for kind in ["server", "app"] {
                match Lock::take(&db, &Holder::new(kind)) {
                    Ok(lock) => drop(lock),
                    Err(_) => refused += 1,
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        starting.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            started.load(Ordering::Relaxed) >= 20,
            "processes started: {}",
            started.load(Ordering::Relaxed)
        );
        assert_eq!(refused, 0, "refused {refused} of {} takes", tries * 2);
    }
}
