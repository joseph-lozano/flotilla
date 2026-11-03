//! Finding the programs the engine runs: gh and claude. The app, opened from the Dock, gets a
//! bare PATH, so it also asks the login shell and tries the usual install places. The CLI and
//! a server have a real PATH (the shell's, or a systemd unit's), so they look only there
//! ([`path_only`]): what isn't on PATH isn't found, as with any command. `FLOTILLA_GH` and
//! `FLOTILLA_CLAUDE` name one exactly.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

static PATH_ONLY: AtomicBool = AtomicBool::new(false);

/// Look only on PATH from now on (the CLI and a server).
pub fn path_only(on: bool) {
    PATH_ONLY.store(on, Ordering::Relaxed);
}

/// Whether to look beyond PATH (the app).
pub fn beyond_path() -> bool {
    !PATH_ONLY.load(Ordering::Relaxed)
}

/// `name` in a folder on PATH.
pub fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// The program `var` names, if it's set: `Some(None)` when it names nothing there.
pub fn named(var: &str) -> Option<Option<PathBuf>> {
    let p = PathBuf::from(std::env::var_os(var)?);
    Some(p.is_file().then_some(p))
}

/// Where `name` is for someone setting up a service: on PATH, in the usual install places (a
/// version manager's shims before its versioned folders, so an upgrade doesn't strand it), or
/// wherever the user's interactive login shell finds it.
pub fn find_anywhere(name: &str) -> Option<PathBuf> {
    if let Some(p) = on_path(name) {
        return Some(p);
    }
    let home = dirs::home_dir().unwrap_or_default();
    let usual = [
        home.join(".local/share/mise/shims"),
        home.join(".local/bin"),
        home.join(".asdf/shims"),
        home.join(".nix-profile/bin"),
        home.join(".claude/local"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/snap/bin"),
    ];
    if let Some(p) = usual.iter().map(|d| d.join(name)).find(|p| p.is_file()) {
        return Some(p);
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let mut child = std::process::Command::new(shell)
        .args(["-ilc", &format!("command -v {name}")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // A shell whose start-up waits on something isn't waited for.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while child.try_wait().ok()?.is_none() {
        if std::time::Instant::now() > until {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().ok()?;
    // Start-up can print other lines; the answer is the last absolute path.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .find(|p| p.is_file())
}
