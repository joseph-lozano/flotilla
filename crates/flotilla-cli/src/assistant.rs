//! Flotilla for other agents, from the command line: `flotilla connect` pairs this machine with
//! a server as an agent device, `flotilla mcp` serves MCP to an assistant (Claude Code,
//! OpenClaw, Hermes) through this machine's server or that one, and `flotilla disconnect`
//! signs it out. See `design/agent-api.md`.
//!
//! An agent device's token is kept as other secrets are here: in files only you can read under
//! `--secrets-dir` (and in the data directory's `secrets/` off a Mac), else in the login
//! Keychain. One per server, by its address. It's never printed.

use crate::Paths;
use flotilla_client::Target;
use flotilla_mcp::{Door, Link, Mcp};
use flotilla_proto::{Scope, Secret};
use std::path::PathBuf;
use std::sync::Arc;

/// The Keychain service agent tokens are kept under (apart from the app's own device tokens,
/// so connecting an assistant never replaces the app's pairing).
#[cfg(target_os = "macos")]
fn service() -> &'static str {
    match flotilla_model::flavor::get() {
        flotilla_model::flavor::Flavor::Release => "flotilla-agent",
        flotilla_model::flavor::Flavor::Dev => "flotilla-dev-agent",
    }
}

/// A server's address as kept: what's safe for a Keychain account or a file name (its bytes
/// in hex, so no two addresses share one).
fn account(url: &str) -> String {
    let hex: String = url.bytes().map(|b| format!("{b:02x}")).collect();
    format!("agent-{hex}")
}

/// Where the token for `url` is kept.
enum Kept {
    File(PathBuf),
    #[cfg(target_os = "macos")]
    Keychain(String),
}

fn kept(paths: &Paths, url: &str) -> Kept {
    match &paths.secrets {
        Some(dir) => Kept::File(dir.join(account(url))),
        #[cfg(target_os = "macos")]
        None => Kept::Keychain(account(url)),
        #[cfg(not(target_os = "macos"))]
        None => Kept::File(paths.dir.join("secrets").join(account(url))),
    }
}

fn save(paths: &Paths, url: &str, token: &str) -> Result<(), String> {
    match kept(paths, url) {
        Kept::File(path) => {
            let (dir, name) = (path.parent().unwrap_or(&paths.dir), account(url));
            flotilla_model::secrets::write_private(dir, &name, token).map_err(|e| e.to_string())
        }
        #[cfg(target_os = "macos")]
        Kept::Keychain(acct) => {
            flotilla_model::secrets::keychain::set(service(), &acct, token, "an agent token")
                .map_err(|e| e.to_string())
        }
    }
}

fn load(paths: &Paths, url: &str) -> Result<Option<String>, String> {
    match kept(paths, url) {
        Kept::File(path) if !path.exists() => Ok(None),
        Kept::File(path) => flotilla_model::secrets::read_private(&path, None)
            .map(Some)
            .map_err(|e| e.to_string()),
        #[cfg(target_os = "macos")]
        Kept::Keychain(acct) => match flotilla_model::secrets::keychain::password(service(), &acct)
        {
            Ok(t) => Ok(Some(t)),
            Err(flotilla_model::secrets::Error::Missing(_)) => Ok(None),
            Err(e) => Err(e.to_string()),
        },
    }
}

fn forget(paths: &Paths, url: &str) {
    match kept(paths, url) {
        Kept::File(path) => {
            let _ = std::fs::remove_file(path);
        }
        #[cfg(target_os = "macos")]
        Kept::Keychain(acct) => {
            let _ = std::process::Command::new("/usr/bin/security")
                .args(["delete-generic-password", "-s", service(), "-a", &acct])
                .output();
        }
    }
}

/// A server's address (or a pairing link) as its URL, and the code a link carries.
fn read(server: &str) -> Result<(String, Option<String>), String> {
    let t = flotilla_proto::pair::read(server)?;
    Ok((t.server, t.code))
}

/// This command, as `claude mcp add` should run it: its own path (an assistant's PATH may not
/// have it), with the flags that find the token.
fn command(paths: &Paths, what: &str) -> String {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| crate::shell_word(&p.display().to_string()))
        .unwrap_or_else(|| "flotilla".into());
    // Absolute: the assistant runs it from wherever it is.
    let secrets = match paths.secrets.as_deref().filter(|_| paths.secrets_given) {
        Some(s) => format!(
            " --secrets-dir {}",
            crate::shell_word(
                &std::path::absolute(s)
                    .unwrap_or_else(|_| s.to_path_buf())
                    .display()
                    .to_string()
            )
        ),
        None => String::new(),
    };
    format!("{exe}{secrets} {what}")
}

/// `flotilla connect <server> [code]`: pair this machine with `server` as an agent device.
pub fn connect(
    paths: &Paths,
    server: &str,
    code: Option<String>,
    name: Option<String>,
) -> Result<(), String> {
    let (url, in_link) = read(server)?;
    let code = match code.or(in_link) {
        Some(c) => c,
        None if std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
            eprint!(
                "Pairing code (from `flotilla pair --agent` on the server, or Add device in Settings): "
            );
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            line
        }
        None => {
            return Err(format!(
                "Give the pairing code: `flotilla connect {url} ABCD-EFGH-JKMN`"
            ));
        }
    };
    let name = name.unwrap_or_else(|| format!("Agent on {}", flotilla_core::util::machine_name()));
    let paired =
        flotilla_client::pair_as(&url, &code, &name, std::env::consts::OS, Some(Scope::Agent))?;
    save(paths, &url, &paired.token.0)?;
    println!(
        "Connected to {url} as an agent device, {name:?} ({}).",
        paired.device
    );
    println!(
        "It can read, start and follow threads; it can't delete, merge, or change settings, projects or devices."
    );
    println!(
        "\nAdd it to Claude Code:\n  claude mcp add flotilla -- {}",
        command(paths, &format!("mcp --server {url}"))
    );
    println!("Remove it with `flotilla disconnect {url}`, or revoke it in Settings → Devices.");
    Ok(())
}

/// `flotilla disconnect <server>`: sign this machine's agent device out, and forget its token.
pub fn disconnect(paths: &Paths, server: &str) -> Result<(), String> {
    let (url, _) = read(server)?;
    let Some(token) = load(paths, &url)? else {
        return Err(format!("This machine isn't connected to {url}"));
    };
    let device = flotilla_model::devices::token_device(&token).map(str::to_string);
    let signed_out = device
        .as_deref()
        .is_some_and(|d| sign_out(&url, &token, d).is_ok());
    forget(paths, &url);
    if signed_out {
        println!("Signed out of {url}.");
    } else {
        println!(
            "Forgot the token for {url}. The server couldn't be told: revoke the device in Settings → Devices there."
        );
    }
    Ok(())
}

/// Ask the server to revoke this device: done once it closes the connection as revoked.
fn sign_out(url: &str, token: &str, device: &str) -> Result<(), String> {
    use flotilla_proto::{ClientMsg, Connection, Request, ServerMsg};
    let (tx, rx) = std::sync::mpsc::channel();
    let info = flotilla_client::Info {
        app: "flotilla-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        token: Some(Secret(token.into())),
        scope: Some(Scope::Agent),
    };
    let client =
        flotilla_client::Client::connect(Target::parse(url)?, info, move |m| tx.send(m).is_ok());
    client.send(ClientMsg::Req {
        id: 1,
        req: Request::RevokeDevice(device.into()),
    });
    let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
            Ok(ServerMsg::Res { id: 1, result }) => return result.map(|_| ()),
            Ok(ServerMsg::Connection(Connection::Refused { reason })) => {
                return if reason == flotilla_engine::host::REVOKED {
                    Ok(())
                } else {
                    Err(reason)
                };
            }
            Ok(_) => {}
            Err(_) => return Err("the server didn't answer".into()),
        }
    }
}

/// `--server`: its URL, and this machine's agent token for it.
pub fn door_for(paths: &Paths, server: &str) -> Result<(String, Secret), String> {
    let (url, _) = read(server)?;
    let token = load(paths, &url)?.ok_or_else(|| {
        format!("This machine isn't connected to {url}: run `flotilla connect {url}` first")
    })?;
    Ok((url, Secret(token)))
}

/// Where `flotilla mcp` connects: `server` with this machine's agent token for it, or this
/// machine's server through its socket (where the server holds it to an agent's scope too).
/// With no server running here yet, the socket it will have: the connection waits for it.
fn door(paths: &Paths, server: Option<&str>) -> Result<Door, String> {
    if let Some(server) = server {
        let (url, token) = door_for(paths, server)?;
        return Ok(Door {
            target: Target::parse(&url)?,
            token: Some(token),
        });
    }
    let address = flotilla_core::lock::holder(&paths.db).and_then(|h| h.address);
    let target = match address {
        Some(a) if a.starts_with("unix:") => Target::parse(&a)?,
        Some(a) => {
            return Err(format!(
                "The Flotilla server here listens only on {a}: pass --server with an address you've connected to with `flotilla connect`"
            ));
        }
        None => Target::Unix(paths.dir.join("run").join("server.sock")),
    };
    Ok(Door {
        target,
        token: None,
    })
}

/// `flotilla mcp`: serve MCP on stdin and stdout until stdin closes.
pub fn mcp(paths: &Paths, server: Option<&str>) -> Result<(), String> {
    let door = door(paths, server)?;
    let mcp = Mcp::new(Link::connect(door, "flotilla-mcp"));
    flotilla_mcp::serve_stdio(Arc::new(mcp)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_servers_token_is_kept_apart_under_a_safe_name() {
        let a = account("https://box.tail1234.ts.net");
        assert!(flotilla_model::secrets::safe(&a), "{a}");
        assert_ne!(a, account("https://box-tail1234.ts.net"));
        assert_ne!(
            account("http://127.0.0.1:7777"),
            account("http://127.0.0.1:7778")
        );
    }

    #[test]
    fn a_token_is_kept_in_the_secrets_folder_and_forgotten() {
        let dir = std::env::temp_dir().join(format!("flotilla-agent-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths =
            Paths::resolve(Some(dir.join("data")), None, Some(dir.join("secrets"))).unwrap();
        let url = "https://box.tail1234.ts.net";
        assert_eq!(load(&paths, url).unwrap(), None);
        save(&paths, url, "fdt_0123456789abcdef_secret").unwrap();
        assert_eq!(
            load(&paths, url).unwrap().as_deref(),
            Some("fdt_0123456789abcdef_secret")
        );
        assert_eq!(load(&paths, "https://other.tail1234.ts.net").unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("secrets").join(account(url)))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // `flotilla mcp --server` finds it by the address however it's typed.
        let door = door(&paths, Some("https://box.tail1234.ts.net/")).unwrap();
        assert!(door.token.is_some() && door.target == Target::parse(url).unwrap());
        assert!(door_err(&paths, "https://unknown.tail1234.ts.net").contains("flotilla connect"));
        forget(&paths, url);
        assert_eq!(load(&paths, url).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn door_err(paths: &Paths, server: &str) -> String {
        door(paths, Some(server)).unwrap_err()
    }
}
