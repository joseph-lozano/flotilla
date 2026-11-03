//! `flotilla service install|status|start|stop|restart|uninstall`: the server as a systemd user
//! service on Linux, so it runs while no one is logged in (with lingering on), starts on boot
//! and comes back if it fails.
//!
//! The unit holds no secret: it names where they are (`--secrets-dir`, or the files given),
//! and its PATH finds gh, claude and tailscale where they were found at install, since the
//! server looks for them on PATH only. On a Mac the app runs Flotilla while it's open, so
//! there's no service there (yet).

// The unit is written only on Linux, and tested everywhere.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// What `install` writes into the unit.
#[derive(Debug, Clone)]
pub struct Install {
    pub data_dir: PathBuf,
    pub secrets_dir: PathBuf,
    pub boat_key_file: Option<PathBuf>,
    pub claude_token_file: Option<PathBuf>,
    pub port: u16,
    pub url: Option<String>,
}

impl Install {
    /// The command that installs this again, as it was given.
    pub fn command(&self) -> String {
        let word = |p: &Path| crate::shell_word(&p.display().to_string());
        let mut c = format!(
            "flotilla service install --data-dir {} --secrets-dir {}",
            word(&self.data_dir),
            word(&self.secrets_dir)
        );
        if let Some(f) = &self.boat_key_file {
            c += &format!(" --boat-key-file {}", word(f));
        }
        if let Some(f) = &self.claude_token_file {
            c += &format!(" --claude-token-file {}", word(f));
        }
        if self.port != 7777 {
            c += &format!(" --port {}", self.port);
        }
        if let Some(u) = &self.url {
            c += &format!(" --url {}", crate::shell_word(u));
        }
        c
    }
}

#[derive(Debug, Clone)]
pub enum Action {
    Install(Install),
    Status,
    Start,
    Stop,
    Restart,
    Uninstall { data_dir: PathBuf },
}

/// The unit's name: `flotilla.service`, or `flotilla-dev.service` for Flotilla Dev.
pub fn unit_name() -> &'static str {
    if flotilla_core::flavor::is_dev() {
        "flotilla-dev.service"
    } else {
        "flotilla.service"
    }
}

/// `s` as one systemd word: quoted, with `\`, `"` and `%` (a specifier) escaped.
fn quote(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

/// `s` as one word of `ExecStart`, where systemd also expands `$NAME` and `${NAME}`: `$` is
/// `$$` there.
fn exec_word(s: &str) -> String {
    quote(&s.replace('$', "$$"))
}

/// The folders on the unit's PATH: where each tool was found, then the usual ones.
pub fn path_for(found: &[Option<PathBuf>], home: &Path) -> String {
    let mut dirs: Vec<PathBuf> = found
        .iter()
        .flatten()
        .filter_map(|p| p.parent().map(Path::to_path_buf))
        .collect();
    dirs.extend([
        home.join(".local/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ]);
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs.iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

/// The unit for running `exe` as the server.
pub fn unit_text(exe: &Path, i: &Install, path: &str) -> String {
    let mut args = vec![
        exec_word(&exe.display().to_string()),
        "serve".to_string(),
        "--data-dir".into(),
        exec_word(&i.data_dir.display().to_string()),
    ];
    args.extend([
        "--secrets-dir".into(),
        exec_word(&i.secrets_dir.display().to_string()),
    ]);
    if let Some(f) = &i.boat_key_file {
        args.extend([
            "--boat-key-file".into(),
            exec_word(&f.display().to_string()),
        ]);
    }
    if let Some(f) = &i.claude_token_file {
        args.extend([
            "--claude-token-file".into(),
            exec_word(&f.display().to_string()),
        ]);
    }
    args.extend(["--port".into(), i.port.to_string()]);
    if let Some(u) = &i.url {
        args.extend(["--url".into(), exec_word(u)]);
    }
    let dev = if flotilla_core::flavor::is_dev() {
        "1"
    } else {
        "0"
    };
    format!(
        "# Written by `flotilla service install`; run it again to change this.
[Unit]
Description={name} server
Documentation=https://github.com/joseph-lozano/flotilla/blob/main/docs/server.md

[Service]
Type=exec
ExecStart={exec}
Environment={path}
Environment=FLOTILLA_DEV={dev}
UMask=0077
Restart=on-failure
RestartSec=5
# Stopping waits for boat requests under way, and turns Tailscale's forward off.
TimeoutStopSec=120
NoNewPrivileges=true

[Install]
WantedBy=default.target
",
        name = flotilla_core::flavor::app_name(),
        exec = args.join(" "),
        path = quote(&format!("PATH={path}")),
    )
}

/// Whether the service installed here runs the server on `dir` (an absolute path), so
/// `flotilla service start` and `stop` are about it. Never off Linux.
pub fn serves(dir: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::unit_path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|unit| unit_serves(&unit, dir))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        false
    }
}

/// Whether `unit` (as `unit_text` writes it) runs the server on `dir`.
fn unit_serves(unit: &str, dir: &Path) -> bool {
    let arg = format!(
        " serve --data-dir {} ",
        exec_word(&dir.display().to_string())
    );
    unit.lines()
        .any(|l| l.starts_with("ExecStart=") && l.contains(&arg))
}

pub fn run(action: Action) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::run(action)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = action;
        Err("`flotilla service` runs Flotilla as a systemd user service, on Linux. On a Mac, the Flotilla app runs it while it's open".into())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    pub(super) fn unit_path() -> Result<PathBuf, String> {
        let config =
            dirs::config_dir().ok_or("No config folder ($XDG_CONFIG_HOME or ~/.config)")?;
        Ok(config.join("systemd/user").join(unit_name()))
    }

    /// `systemctl --user <args>`: whether it succeeded, and what it said.
    fn systemctl(args: &[&str]) -> Result<(bool, String), String> {
        let mut cmd = Command::new("systemctl");
        cmd.arg("--user").args(args);
        // An SSH session may not have it; the user's manager is there all the same.
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata("/proc/self")
                .map(|m| m.uid())
                .map_err(|e| e.to_string())?;
            cmd.env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
        }
        let out = cmd
            .output()
            .map_err(|e| format!("Couldn't run systemctl ({e}): is this a systemd system?"))?;
        let said = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        Ok((out.status.success(), said.trim().to_string()))
    }

    fn must(args: &[&str]) -> Result<(), String> {
        match systemctl(args)? {
            (true, _) => Ok(()),
            (false, said) => Err(format!(
                "`systemctl --user {}` failed: {said}",
                args.join(" ")
            )),
        }
    }

    fn active() -> bool {
        systemctl(&["is-active", "--quiet", unit_name()]).is_ok_and(|(ok, _)| ok)
    }

    fn user() -> String {
        std::env::var("USER")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "$USER".into())
    }

    pub fn run(action: Action) -> Result<(), String> {
        match action {
            Action::Install(i) => install(i),
            Action::Status => {
                let (_, said) = systemctl(&["status", "--no-pager", "--lines=15", unit_name()])?;
                println!("{said}");
                if let Ok(p) = unit_path() {
                    println!();
                    println!("Unit: {}", p.display());
                }
                println!("Logs: journalctl --user -u {} -f", unit_name());
                Ok(())
            }
            Action::Start => must(&["start", unit_name()]),
            Action::Stop => must(&["stop", unit_name()]),
            Action::Restart => must(&["restart", unit_name()]),
            Action::Uninstall { data_dir } => {
                let path = unit_path()?;
                if !path.exists() {
                    return Err(format!(
                        "{} isn't installed ({} doesn't exist)",
                        unit_name(),
                        path.display()
                    ));
                }
                must(&["disable", "--now", unit_name()])?;
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                must(&["daemon-reload"])?;
                println!(
                    "Removed {}. Flotilla's data is still in {}.",
                    unit_name(),
                    data_dir.display()
                );
                Ok(())
            }
        }
    }

    fn install(i: Install) -> Result<(), String> {
        let exe = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .map_err(|e| format!("Where is this flotilla? {e}"))?;
        let db = i.data_dir.join("flotilla.db");
        // Another Flotilla on this database (not this service's) would keep it from starting.
        if let Some(h) = flotilla_core::lock::holder(&db)
            && !active()
        {
            return Err(format!(
                "{} (the {}, pid {}): stop it first, then install the service",
                flotilla_core::lock::BUSY,
                h.kind,
                h.pid
            ));
        }
        let home = dirs::home_dir().unwrap_or_default();
        let tools: Vec<(&str, Option<PathBuf>)> = ["gh", "claude", "tailscale"]
            .into_iter()
            .map(|t| (t, flotilla_core::tools::find_anywhere(t)))
            .collect();
        let path = path_for(
            &tools.iter().map(|(_, p)| p.clone()).collect::<Vec<_>>(),
            &home,
        );
        let unit = unit_path()?;
        if let Some(dir) = unit.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::create_dir_all(&i.data_dir)
            .map_err(|e| format!("{}: {e}", i.data_dir.display()))?;
        std::fs::write(&unit, unit_text(&exe, &i, &path))
            .map_err(|e| format!("{}: {e}", unit.display()))?;
        must(&["daemon-reload"])?;
        must(&["enable", unit_name()])?;
        // Starts it, or puts a changed unit into effect.
        must(&["restart", unit_name()])?;
        println!("Installed {} ({})", unit_name(), unit.display());
        println!(
            "  Runs: {} serve --data-dir {}",
            exe.display(),
            i.data_dir.display()
        );
        for (tool, found) in &tools {
            match found {
                Some(p) => println!("  {tool}: {}", p.display()),
                None => println!(
                    "  {tool}: not found. Install it (or put it on PATH), then run `{}` again",
                    i.command()
                ),
            }
        }
        if exe.components().any(|c| c.as_os_str() == "target") || exe.starts_with("/tmp") {
            println!(
                "  Note: the service runs this binary where it is. Copy flotilla somewhere lasting (~/.local/bin) and install from there."
            );
        }
        if flotilla_core::secrets::store()
            .get(flotilla_core::secrets::Name::BoatKey)
            .ok()
            .flatten()
            .is_none()
        {
            let word = |p: &Path| crate::shell_word(&p.display().to_string());
            println!(
                "  No boat key yet: run `flotilla setup --data-dir {} --secrets-dir {}`, then `flotilla service restart`.",
                word(&i.data_dir),
                word(&i.secrets_dir)
            );
        }
        // Started at boot, and kept after logging out, only with lingering on.
        let (_, linger) = Command::new("loginctl")
            .args(["show-user", &user(), "--property=Linger"])
            .output()
            .map(|o| {
                (
                    o.status.success(),
                    String::from_utf8_lossy(&o.stdout).trim().to_string(),
                )
            })
            .unwrap_or_default();
        if linger != "Linger=yes" {
            println!(
                "  It stops when you log out, and won't start at boot, until you run: sudo loginctl enable-linger {}",
                user()
            );
        }
        // Up when it holds the database and listens; down if it isn't active a moment on, or
        // never gets there.
        let started = Instant::now();
        loop {
            if flotilla_core::lock::holder(&db)
                .is_some_and(|h| h.kind == "server" && h.address.is_some())
            {
                println!(
                    "Flotilla is running. Logs: journalctl --user -u {} -f",
                    unit_name()
                );
                println!(
                    "Pair a device: flotilla pair --data-dir {}",
                    crate::shell_word(&i.data_dir.display().to_string())
                );
                return Ok(());
            }
            let waited = started.elapsed();
            if waited > Duration::from_secs(20) || waited > Duration::from_secs(3) && !active() {
                let (_, log) = Command::new("journalctl")
                    .args(["--user", "-u", unit_name(), "-n", "20", "--no-pager"])
                    .output()
                    .map(|o| (true, String::from_utf8_lossy(&o.stdout).to_string()))
                    .unwrap_or_default();
                return Err(format!("{} didn't start. Its log:\n{log}", unit_name()));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install() -> Install {
        Install {
            data_dir: "/home/you/.local/share/Flotilla".into(),
            secrets_dir: "/home/you/.local/share/Flotilla/secrets".into(),
            boat_key_file: None,
            claude_token_file: None,
            port: 7777,
            url: None,
        }
    }

    #[test]
    fn the_unit_runs_the_server_with_a_path_that_finds_its_tools() {
        let home = Path::new("/home/you");
        let found = [
            Some(PathBuf::from("/home/you/.local/share/mise/shims/gh")),
            Some(PathBuf::from("/home/you/.local/bin/claude")),
            None,
        ];
        let path = path_for(&found, home);
        assert_eq!(
            path,
            "/home/you/.local/share/mise/shims:/home/you/.local/bin:/usr/local/bin:/usr/bin:/bin"
        );
        let unit = unit_text(
            Path::new("/home/you/.local/bin/flotilla"),
            &install(),
            &path,
        );
        assert!(unit.contains(r#"ExecStart="/home/you/.local/bin/flotilla" serve --data-dir "/home/you/.local/share/Flotilla" --secrets-dir "/home/you/.local/share/Flotilla/secrets" --port 7777"#), "{unit}");
        assert!(
            unit.contains(&format!("Environment=\"PATH={path}\"")),
            "{unit}"
        );
        assert!(
            unit.contains("Restart=on-failure")
                && unit.contains("WantedBy=default.target")
                && unit.contains("UMask=0077"),
            "{unit}"
        );
    }

    #[test]
    fn odd_paths_and_urls_stay_one_word() {
        let mut i = install();
        i.data_dir = "/srv/Flotilla Dev/100%".into();
        i.boat_key_file = Some("/etc/flotilla/boat\"key".into());
        i.url = Some("https://flotilla.example.com".into());
        let unit = unit_text(Path::new("/usr/local/bin/flotilla"), &i, "/usr/bin");
        assert!(
            unit.contains(r#"--data-dir "/srv/Flotilla Dev/100%%""#),
            "{unit}"
        );
        assert!(
            unit.contains(r#"--boat-key-file "/etc/flotilla/boat\"key""#),
            "{unit}"
        );
        assert!(
            unit.contains(r#"--url "https://flotilla.example.com""#),
            "{unit}"
        );
    }

    /// A unit serves the data directory it names, and no other; installing again is said with
    /// every option it was given, each one word to the shell.
    #[test]
    fn a_unit_knows_its_data_and_how_it_was_installed() {
        let unit = unit_text(Path::new("/usr/local/bin/flotilla"), &install(), "/usr/bin");
        assert!(unit_serves(
            &unit,
            Path::new("/home/you/.local/share/Flotilla")
        ));
        assert!(!unit_serves(
            &unit,
            Path::new("/home/you/.local/share/Flot")
        ));
        assert!(!unit_serves(&unit, Path::new("/srv/fl")));
        assert_eq!(
            install().command(),
            "flotilla service install --data-dir /home/you/.local/share/Flotilla --secrets-dir /home/you/.local/share/Flotilla/secrets"
        );
        let mut i = install();
        i.data_dir = "/srv/Flotilla Dev/100%".into();
        i.boat_key_file = Some("/etc/flotilla/boat'key".into());
        i.port = 7790;
        i.url = Some("https://flotilla.example.com".into());
        assert!(unit_serves(
            &unit_text(Path::new("/usr/local/bin/flotilla"), &i, "/usr/bin"),
            &i.data_dir
        ));
        // systemd expands `${USER}` in ExecStart: a folder named that is written so it isn't,
        // and it's that folder the unit serves, not the user's.
        let mut dollar = install();
        dollar.data_dir = "/srv/${USER}".into();
        let unit = unit_text(Path::new("/usr/local/bin/flotilla"), &dollar, "/usr/bin");
        assert!(unit.contains(r#"--data-dir "/srv/$${USER}""#), "{unit}");
        assert!(
            unit_serves(&unit, Path::new("/srv/${USER}"))
                && !unit_serves(&unit, Path::new("/srv/you"))
        );
        // A unit from before, which systemd read as the user's folder, isn't taken for it.
        assert!(!unit_serves(
            r#"ExecStart="/usr/local/bin/flotilla" serve --data-dir "/srv/${USER}" --port 7777"#,
            Path::new("/srv/${USER}")
        ));
        assert_eq!(
            i.command(),
            r#"flotilla service install --data-dir '/srv/Flotilla Dev/100%' --secrets-dir /home/you/.local/share/Flotilla/secrets --boat-key-file '/etc/flotilla/boat'\''key' --port 7790 --url https://flotilla.example.com"#
        );
    }
}
