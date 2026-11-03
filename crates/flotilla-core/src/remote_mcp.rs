//! Remote MCP's setting: whether `flotilla serve` opens its MCP listener (`flotilla-server`'s
//! `mcp`), where, and how it's published. Off unless it's turned on (`flotilla setup --mcp`,
//! or `flotilla serve --mcp-port`): with nothing kept, there's no listener and no public port.

use crate::store::Store;
use serde::{Deserialize, Serialize};

/// Where it's kept, in kv.
pub const KEY: &str = "remote_mcp";
/// The port it listens on unless told: 3000–9999, which exe.dev's proxy can forward.
pub const DEFAULT_PORT: u16 = 8787;

/// Remote MCP, turned on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The port on this machine (127.0.0.1 unless `host` says).
    pub port: u16,
    /// The address it listens on, when not 127.0.0.1 (a proxy that reaches this machine over
    /// its network, as exe.dev's may).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Published with Tailscale Funnel: `flotilla serve` turns it on as it starts, and off as
    /// it stops (on 8443 or 10000, never 443, which the main API's private serve has).
    #[serde(default)]
    pub funnel: bool,
    /// Published another way (exe.dev, a Cloudflare Tunnel, a proxy): the address clients use,
    /// `https://box.example.com`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Settings {
    pub fn on(port: u16) -> Settings {
        Settings {
            port,
            host: None,
            funnel: false,
            url: None,
        }
    }

    /// The address it listens on.
    pub fn host(&self) -> &str {
        self.host.as_deref().unwrap_or("127.0.0.1")
    }
}

/// What's kept: None when it's off (or what's kept doesn't read).
pub fn load(store: &Store) -> Option<Settings> {
    store
        .kv_get(KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
}

/// Keep `settings`, or turn it off (None).
pub fn save(store: &Store, settings: Option<&Settings>) -> crate::Result<()> {
    match settings {
        Some(s) => store.kv_set(KEY, &serde_json::to_string(s).unwrap_or_default()),
        None => store.kv_delete(KEY),
    }
}

/// `url` as an address clients can use: `https://host` (or `http://` to this machine: the host
/// exactly `localhost` or a loopback address), with a port at most: no user, path, query or
/// fragment. As it's written once read (the host in lower case), with no slash after it.
pub fn check_url(url: &str) -> Result<String, String> {
    let refuse = || {
        Err(format!(
            "{url} isn't an address clients can use: give https://host, with no path"
        ))
    };
    let given = url.trim();
    // The parser drops tabs and newlines inside an address; here they make it wrong.
    if given.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return refuse();
    }
    let Ok(u) = reqwest::Url::parse(given) else {
        return refuse();
    };
    let host = u.host_str().unwrap_or("");
    let ip = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<std::net::IpAddr>();
    let this_machine = host == "localhost" || ip.is_ok_and(|ip| ip.is_loopback());
    let scheme_ok = u.scheme() == "https" || (u.scheme() == "http" && this_machine);
    if !scheme_ok
        || host.is_empty()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return refuse();
    }
    Ok(u.as_str().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_unless_kept_and_kept_as_given() {
        let dir = std::env::temp_dir().join(format!("flotilla-remote-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("flotilla.db")).unwrap();
        assert_eq!(load(&store), None);
        let s = Settings {
            funnel: true,
            ..Settings::on(8787)
        };
        save(&store, Some(&s)).unwrap();
        assert_eq!(load(&store), Some(s));
        save(&store, None).unwrap();
        assert_eq!(load(&store), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_public_address_is_https_and_only_a_host() {
        assert_eq!(
            check_url("https://box.example.com/").unwrap(),
            "https://box.example.com"
        );
        assert_eq!(
            check_url("https://box.tail1234.ts.net:8443").unwrap(),
            "https://box.tail1234.ts.net:8443"
        );
        assert!(check_url("http://127.0.0.1:8787").is_ok());
        assert!(check_url("http://box.example.com").is_err());
        assert!(check_url("https://box.example.com/mcp").is_err());
        assert!(check_url("box.example.com").is_err());
    }

    /// Plain HTTP only to this machine: the host exactly `localhost` or a loopback address, not
    /// a name that starts like one. No user, query or fragment, whatever the scheme.
    #[test]
    fn plain_http_is_only_for_this_machine() {
        for ok in [
            "http://localhost:8787",
            "http://LOCALHOST:8787/",
            "http://127.0.0.1",
            "http://127.0.0.2:9000",
            "http://[::1]:9000",
        ] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            check_url("http://LOCALHOST:8787/").unwrap(),
            "http://localhost:8787"
        );
        for bad in [
            "http://localhost.attacker.example",
            "http://localhost.attacker.example:8787",
            "http://127.0.0.1.attacker.example",
            "http://127.0.0.1.nip.io",
            "http://[::1].attacker.example",
            "http://localhost%2eattacker.example",
            "http://127.0.0.1:8787@attacker.example",
            "http://user@127.0.0.1",
            "https://user:secret@box.example.com",
            "https://box.example.com?x=1",
            "https://box.example.com#here",
            "https://box.example.com/?",
            "http://10.0.0.5:8787",
            "http://local\thost:8787",
            "http://localhost:8787\n/x",
            "http://localhost:8787/\r\nx",
            "ftp://box.example.com",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }
}
