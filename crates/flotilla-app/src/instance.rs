//! Linux: one app per database, and the links it's handed. On a Mac, LaunchServices gives the
//! running app the `flotilla://` links opened anywhere (`on_open_urls`). On Linux, `xdg-open`
//! runs the app again with the link as its argument. So the first start holds `run/app.lock`
//! beside its database and listens on `run/app.sock`; a later one hands it its links (or none:
//! come forward) and exits once the first says its window took them. A first app that's busy or
//! quitting doesn't say so: the later one keeps asking (that app, or whichever starts next), and
//! is the app itself if the database comes free first.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a later start waits for the first to listen (it may be starting too).
const WAIT: Duration = Duration::from_secs(5);
/// How long the first waits for its window to take what was handed over.
const TAKE: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(5)
};
/// How long a later start keeps asking an app that doesn't take its links (busy, or quitting
/// while its engine finishes) before it gives up.
const GOING: Duration = Duration::from_secs(120);
/// The most a later start may send.
const MAX: u64 = 64 * 1024;
/// What the first answers once its window took the links.
const TAKEN: &[u8] = b"taken\n";

/// Links handed to the window, and how many it has taken (`took`), in order.
static SENT: AtomicU64 = AtomicU64::new(0);
static TOOK: AtomicU64 = AtomicU64::new(0);

/// The window took a link handed over (it opened it, or came forward).
pub fn took() {
    TOOK.fetch_add(1, Ordering::SeqCst);
}

/// Hand `link` to the window through `deliver`, counted so a later start can be told it was
/// taken. The command line's links go this way too.
pub fn send(link: String, deliver: &impl Fn(String)) {
    SENT.fetch_add(1, Ordering::SeqCst);
    deliver(link);
}

pub enum Start {
    /// This is the app on its database, listening while the process runs (unless it couldn't).
    First,
    /// Another app has this database: it took the links, or didn't for two minutes. This start
    /// is done either way; a second app on the database wouldn't help.
    Other,
}

/// The links on a command line (`flotilla-app flotilla://pair?u=…#CODE`, as `xdg-open` runs
/// it). The window checks each before it opens one.
pub fn links_in(args: impl IntoIterator<Item = String>) -> Vec<String> {
    args.into_iter()
        .filter(|a| {
            let lower = a.trim().to_ascii_lowercase();
            [
                flotilla_proto::pair::SCHEME,
                flotilla_proto::pair::DEV_SCHEME,
            ]
            .iter()
            .any(|s| lower.starts_with(&format!("{s}://")))
        })
        .collect()
}

/// `run/app.lock` and `run/app.sock` beside the database.
fn paths(db: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let run = db
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("run");
    (run.join("app.lock"), run.join("app.sock"), run)
}

/// Be the app on `db`, passing each link a later start hands over to `deliver` (an empty one:
/// it had none, so only come forward); the window calls [`took`] for each. Or, if another app
/// has `db`, hand it `links`.
pub fn start(db: &Path, links: &[String], deliver: impl Fn(String) + Send + 'static) -> Start {
    let (lock_path, socket, run) = paths(db);
    // Only this user may reach the socket: `run/` is 0700, as the server keeps it.
    if let Err(e) = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .and_then(|_| std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)))
    {
        eprintln!("[instance] couldn't make {}: {e}", run.display());
        return Start::First;
    }
    let lock = match File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[instance] couldn't open {}: {e}", lock_path.display());
            return Start::First;
        }
    };
    match lock.try_lock() {
        Ok(()) => {
            listen(lock, &socket, deliver);
            Start::First
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let until = Instant::now() + GOING;
            loop {
                match hand_over(&socket, links) {
                    Ok(()) => return Start::Other,
                    // No socket can be made at that path (too long): no app listens there.
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => return Start::First,
                    Err(e) if Instant::now() >= until => {
                        eprintln!(
                            "[instance] another Flotilla has {} but didn't take the links: {e}",
                            lock_path.display()
                        );
                        return Start::Other;
                    }
                    Err(_) => {}
                }
                // It's busy or quitting: be the app once it's gone, or ask again (it, or
                // whichever app took its place).
                if lock.try_lock().is_ok() {
                    listen(lock, &socket, deliver);
                    return Start::First;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        Err(std::fs::TryLockError::Error(e)) => {
            eprintln!("[instance] couldn't lock {}: {e}", lock_path.display());
            Start::First
        }
    }
}

/// Listen on `socket` for the rest of the process, holding `lock` (no other app can be there,
/// so a socket file left there is stale).
fn listen(lock: File, socket: &Path, deliver: impl Fn(String) + Send + 'static) {
    let _ = std::fs::remove_file(socket);
    let listener = match UnixListener::bind(socket) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[instance] couldn't listen on {}: {e}", socket.display());
            return;
        }
    };
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    std::thread::Builder::new()
        .name("flotilla-instance".into())
        .spawn(move || {
            let _held = lock;
            for mut stream in listener.incoming().flatten() {
                let links = read_links(&stream);
                let want = SENT.load(Ordering::SeqCst) + links.len() as u64;
                for link in links {
                    send(link, &deliver);
                }
                // Say so once the window took them; a window that's gone never does.
                let until = Instant::now() + TAKE;
                while TOOK.load(Ordering::SeqCst) < want && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if TOOK.load(Ordering::SeqCst) >= want {
                    let _ = stream.write_all(TAKEN);
                }
            }
        })
        .map(drop)
        .unwrap_or_else(|e| eprintln!("[instance] couldn't start listening: {e}"));
}

/// What a later start sent: its links, or one empty string when it had none.
fn read_links(stream: &UnixStream) -> Vec<String> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut text = String::new();
    if stream.take(MAX).read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let links: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    if links.is_empty() {
        vec![String::new()]
    } else {
        links
    }
}

/// Send `links` to the app listening on `socket` (waiting for it to start listening), and wait
/// for it to say its window took them.
fn hand_over(socket: &Path, links: &[String]) -> std::io::Result<()> {
    let until = Instant::now() + WAIT;
    let mut stream = loop {
        match UnixStream::connect(socket) {
            Ok(s) => break s,
            Err(e) if Instant::now() >= until => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    stream.write_all(links.join("\n").as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    stream.set_read_timeout(Some(TAKE + Duration::from_secs(2)))?;
    let mut answer = Vec::new();
    let _ = stream.take(TAKEN.len() as u64).read_to_end(&mut answer);
    if answer == TAKEN {
        Ok(())
    } else {
        Err(std::io::Error::other("its window didn't take them"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pairing_links_are_taken_from_the_command_line() {
        let args = [
            "flotilla-app",
            "--verbose",
            "flotilla://pair?u=https%3A%2F%2Fbox#ABCD",
            "FLOTILLA-DEV://pair?u=x",
            "https://example.com",
        ]
        .map(String::from);
        assert_eq!(
            links_in(args),
            [
                "flotilla://pair?u=https%3A%2F%2Fbox#ABCD",
                "FLOTILLA-DEV://pair?u=x"
            ]
        );
    }

    #[test]
    fn a_second_start_hands_its_links_to_the_first_and_one_without_asks_it_to_come_forward() {
        let dir = std::env::temp_dir().join(format!("flotilla-instance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        // The window: takes each link as it comes.
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(matches!(
            start(&db, &[], move |l| {
                tx.send(l).unwrap();
                took();
            }),
            Start::First
        ));
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            (mode(&dir.join("run")), mode(&dir.join("run/app.sock"))),
            (0o700, 0o600)
        );

        let link = "flotilla://pair?u=https%3A%2F%2Fbox#ABCDEFGHJKMN".to_string();
        assert!(matches!(
            start(&db, std::slice::from_ref(&link), |_| panic!(
                "a second listener"
            )),
            Start::Other
        ));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), link);
        assert!(matches!(
            start(&db, &[], |_| panic!("a second listener")),
            Start::Other
        ));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "");

        // A first whose window is busy (blocked for a while, then takes everything at once): a
        // later start asks again until it's taken, rather than becoming a second app.
        let busy = dir.join("busy/flotilla.db");
        let (queued, queue) = std::sync::mpsc::channel::<String>();
        let (opened, seen) = std::sync::mpsc::channel();
        let free_at = Instant::now() + Duration::from_secs(1);
        std::thread::spawn(move || {
            for link in queue {
                std::thread::sleep(free_at.saturating_duration_since(Instant::now()));
                opened.send(link).unwrap();
                took();
            }
        });
        assert!(matches!(
            start(&busy, &[], move |l| queued.send(l).unwrap()),
            Start::First
        ));
        let asked = Instant::now();
        assert!(matches!(
            start(&busy, std::slice::from_ref(&link), |_| panic!(
                "a second listener"
            )),
            Start::Other
        ));
        assert!(asked.elapsed() < Duration::from_secs(10));
        assert_eq!(seen.recv_timeout(Duration::from_secs(5)).unwrap(), link);

        // Another database is another app.
        let (tx2, _rx2) = std::sync::mpsc::channel();
        assert!(matches!(
            start(&dir.join("other/flotilla.db"), &[], move |l| tx2
                .send(l)
                .unwrap()),
            Start::First
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
