//! Tailscale, for reaching a server from your tailnet over HTTPS: `tailscale serve` forwards
//! the tailnet's HTTPS on port 443 to the server's plain HTTP on 127.0.0.1, as T3's
//! `--tailscale-serve` does. The tailnet's name for this machine comes from `tailscale status`.
//!
//! Remote MCP alone (never the main API) can be published to the internet with Tailscale
//! Funnel, on one of Funnel's other ports ([`FUNNEL_PORTS`]: 443 stays the tailnet's): see
//! [`funnel`]. The tailnet's policy must allow Funnel for this machine (its `funnel`
//! attribute), which `tailscale status` says.

use serde_json::Value;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The HTTPS port Tailscale serves on.
pub const PORT: u16 = 443;
/// `tailscale serve` waits, with a link to follow, when the tailnet hasn't turned HTTPS on.
const PATIENCE: Duration = Duration::from_secs(20);
/// Whether the server forwards through Tailscale (`flotilla setup --tailscale-serve`), in kv.
pub const KEY: &str = "tailscale_serve";
/// Where Funnel may publish remote MCP, in order of choice: Funnel's ports but 443, which the
/// main API's private forward has.
pub const FUNNEL_PORTS: [u16; 2] = [8443, 10000];
/// The ports Funnel takes when the tailnet doesn't say which.
const FUNNEL_DEFAULT: [u16; 3] = [443, 8443, 10000];
/// Where the tailnet's policy says what may use Funnel.
pub const POLICY_URL: &str = "https://login.tailscale.com/admin/acls/file";

/// This machine on the tailnet, as `tailscale status --json` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Connected to the tailnet.
    pub running: bool,
    /// Its name (`box.tail1234.ts.net`), when MagicDNS gives it one.
    pub name: Option<String>,
    /// The tailnet gives it HTTPS certificates.
    pub https: bool,
    /// Its node id, for the admin console's links.
    pub id: Option<String>,
    /// The ports the tailnet lets it publish to the internet with Funnel; None when its policy
    /// doesn't let it (no `funnel` attribute).
    pub funnel: Option<Vec<u16>>,
}

impl Status {
    pub fn read(json: &str) -> Result<Status, String> {
        let v: Value = serde_json::from_str(json)
            .map_err(|e| format!("`tailscale status --json` didn't read: {e}"))?;
        let me = &v["Self"];
        let name = me["DNSName"]
            .as_str()
            .map(|n| n.trim_end_matches('.').to_string())
            .filter(|n| !n.is_empty());
        let https = v["CertDomains"].as_array().is_some_and(|d| !d.is_empty());
        let id = me["ID"]
            .as_str()
            .filter(|i| !i.is_empty())
            .map(str::to_string);
        // Its capabilities: CapMap's keys, or Capabilities (older Tailscale).
        let mapped: Vec<&str> = me["CapMap"]
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let listed = me["Capabilities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        let caps: Vec<&str> = mapped.into_iter().chain(listed).collect();
        let funnel = caps
            .iter()
            .any(|c| *c == "funnel" || *c == "https://tailscale.com/cap/funnel")
            .then(|| {
                let ports: Vec<u16> = caps
                    .iter()
                    .filter_map(|c| c.strip_prefix("https://tailscale.com/cap/funnel-ports?ports="))
                    .flat_map(ports_of)
                    .collect();
                if ports.is_empty() {
                    FUNNEL_DEFAULT.to_vec()
                } else {
                    ports
                }
            });
        Ok(Status {
            running: v["BackendState"].as_str() == Some("Running"),
            name,
            https,
            id,
            funnel,
        })
    }

    /// What to do so this machine may use Funnel: the admin console's page for it, then the
    /// policy file (its `funnel` attribute).
    pub fn funnel_fixes(&self) -> Vec<String> {
        let mut fix = vec![];
        if let Some(id) = &self.id {
            fix.push(format!(
                "Allow Funnel for this machine: https://login.tailscale.com/f/funnel?node={id}"
            ));
        }
        fix.push(format!("Or give it the funnel attribute in the tailnet policy file ({POLICY_URL}): \"nodeAttrs\": [{{\"target\": [\"autogroup:member\"], \"attr\": [\"funnel\"]}}]"));
        fix
    }
}

/// `443,8443` or `8000-8100`: the ports.
fn ports_of(list: &str) -> Vec<u16> {
    list.split(',')
        .flat_map(|p| match p.trim().split_once('-') {
            Some((a, b)) => match (a.parse::<u16>(), b.parse::<u16>()) {
                (Ok(a), Ok(b)) if a <= b && b - a < 1000 => (a..=b).collect(),
                _ => vec![],
            },
            None => p.trim().parse().ok().into_iter().collect(),
        })
        .collect()
}

/// `tailscale status`.
pub fn status() -> Result<Status, String> {
    Status::read(&run(&["status", "--json"])?)
}

/// Whether `proxy` (a `tailscale serve` target) is the server on this machine's `port`.
fn is_local_port(proxy: &str, port: u16) -> bool {
    let rest = proxy.trim_end_matches('/');
    let rest = rest.strip_prefix("http://").unwrap_or(rest);
    ["127.0.0.1", "localhost"]
        .iter()
        .any(|h| rest == format!("{h}:{port}"))
}

/// The command that forwards the tailnet's HTTPS to `http://127.0.0.1:<port>`.
pub fn serve_command(port: u16) -> String {
    format!("tailscale serve --bg --https={PORT} http://127.0.0.1:{port}")
}

/// Who has Tailscale's port 443, from `tailscale serve status --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Use {
    Free,
    /// Only `/`, forwarded to the server on this port.
    Ours,
    /// Anything else: a forward elsewhere, files, other paths, a TCP forward.
    Taken(String),
}

/// Who has 443 now, for the server on `port`: the one reading of `tailscale serve status`
/// that starting, stopping and the setup checks share.
pub fn use_of_443(port: u16) -> Result<Use, String> {
    port_use(&run(&["serve", "status", "--json"])?, port)
}

/// What's said when 443 is someone else's ([`Use::Taken`]): what's there, and that it stays.
/// Never how to remove it: that's for whoever set it up.
pub fn taken(name: &str, what: &str) -> String {
    format!("Tailscale already serves https://{name} ({what}), so Flotilla leaves it alone")
}

/// Who has 443, for the server on `port`, from `tailscale serve status --json`.
pub fn port_use(json: &str, port: u16) -> Result<Use, String> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| format!("`tailscale serve status --json` didn't read: {e}"))?;
    Ok(use_on(&v, PORT, port))
}

/// Who has Tailscale's HTTPS port `https`, for a server on this machine's `port`.
fn use_on(v: &Value, https: u16, port: u16) -> Use {
    let tcp = &v["TCP"][https.to_string()];
    if !tcp["TCPForward"].is_null() || !tcp["TerminateTLS"].is_null() {
        return Use::Taken(format!(
            "a TCP forward to {}",
            tcp["TCPForward"].as_str().unwrap_or("somewhere")
        ));
    }
    let suffix = format!(":{https}");
    let handlers: Vec<(&String, &Value)> = v["Web"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(host, _)| host.ends_with(&suffix))
        .flat_map(|(_, w)| w["Handlers"].as_object().into_iter().flatten())
        .collect();
    match handlers.as_slice() {
        [] => Use::Free,
        [(mount, h)]
            if mount.as_str() == "/"
                && h["Proxy"]
                    .as_str()
                    .is_some_and(|to| is_local_port(to, port)) =>
        {
            Use::Ours
        }
        _ => Use::Taken(
            handlers
                .iter()
                .map(|(mount, h)| {
                    format!(
                        "{mount} → {}",
                        h["Proxy"]
                            .as_str()
                            .or(h["Path"].as_str())
                            .unwrap_or("something")
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

/// What Tailscale does with Funnel's ports ([`FUNNEL_PORTS`]), for remote MCP on `port`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Funnel {
    /// The port that forwards to it, and whether through Funnel (else the tailnet only).
    pub ours: Option<(u16, bool)>,
    /// Ports nothing uses.
    pub free: Vec<u16>,
    /// Ports something else has, and what: left alone.
    pub taken: Vec<(u16, String)>,
}

impl Funnel {
    /// The port to publish on: the one that forwards to it already, else the first free one
    /// the tailnet allows.
    pub fn port(&self, allowed: &[u16]) -> Option<u16> {
        self.ours
            .map(|(p, _)| p)
            .or_else(|| self.free.iter().copied().find(|p| allowed.contains(p)))
    }

    /// Why there's no port to publish on: what has each (never how to remove it).
    pub fn why_none(&self, name: &str, allowed: &[u16]) -> String {
        let usable: Vec<u16> = FUNNEL_PORTS
            .iter()
            .copied()
            .filter(|p| allowed.contains(p))
            .collect();
        if usable.is_empty() {
            return format!(
                "The tailnet lets this machine use Funnel only on {}, and 443 is the private server's",
                allowed
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        let what: Vec<String> = self
            .taken
            .iter()
            .filter(|(p, _)| usable.contains(p))
            .map(|(p, w)| format!("https://{name}:{p} ({w})"))
            .collect();
        format!(
            "Tailscale already serves {}, so Flotilla leaves them alone",
            what.join(" and ")
        )
    }
}

/// What Funnel's ports do now, for remote MCP on `port`.
pub fn funnel_now(port: u16) -> Result<Funnel, String> {
    funnel_use(&run(&["serve", "status", "--json"])?, port)
}

/// What Funnel's ports do, for remote MCP on `port`, from `tailscale serve status --json`.
pub fn funnel_use(json: &str, port: u16) -> Result<Funnel, String> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| format!("`tailscale serve status --json` didn't read: {e}"))?;
    let public = |https: u16| {
        v["AllowFunnel"].as_object().is_some_and(|m| {
            m.iter().any(|(host, on)| {
                host.ends_with(&format!(":{https}")) && on.as_bool() == Some(true)
            })
        })
    };
    let mut f = Funnel::default();
    for p in FUNNEL_PORTS {
        match use_on(&v, p, port) {
            Use::Ours if f.ours.is_none() => f.ours = Some((p, public(p))),
            Use::Ours | Use::Free => f.free.push(p),
            Use::Taken(what) => f.taken.push((p, what)),
        }
    }
    Ok(f)
}

/// The command that publishes remote MCP (on this machine's `port`) at Funnel's `https` port.
pub fn funnel_command(https: u16, port: u16) -> String {
    format!("tailscale funnel --bg --https={https} http://127.0.0.1:{port}")
}

/// Its address on Funnel's `https` port: `https://box.tail1234.ts.net:8443`.
pub fn funnel_url(name: &str, https: u16) -> String {
    if https == PORT {
        format!("https://{name}")
    } else {
        format!("https://{name}:{https}")
    }
}

/// Publish remote MCP (on this machine's `port`) to the internet with Funnel, on 8443 or 10000:
/// its address. A port that forwards elsewhere is someone else's, left as it is; 443 is never
/// touched.
pub fn funnel(port: u16) -> Result<String, String> {
    let s = status()?;
    let name = s
        .name
        .clone()
        .ok_or("Tailscale doesn't give this machine a name: is MagicDNS on?")?;
    if !s.https {
        return Err("The tailnet doesn't give HTTPS certificates: turn on HTTPS at https://login.tailscale.com/admin/dns".into());
    }
    let Some(allowed) = &s.funnel else {
        return Err(format!(
            "The tailnet doesn't let this machine use Funnel. {}",
            s.funnel_fixes().join(". ")
        ));
    };
    let f = funnel_now(port)?;
    if let Some((p, true)) = f.ours {
        return Ok(funnel_url(&name, p));
    }
    let p = f.port(allowed).ok_or_else(|| f.why_none(&name, allowed))?;
    run(&[
        "funnel",
        "--bg",
        &format!("--https={p}"),
        &format!("http://127.0.0.1:{port}"),
    ])?;
    Ok(funnel_url(&name, p))
}

/// Stop publishing remote MCP on `port`: only a Funnel port that forwards to it, nothing else.
pub fn funnel_off(port: u16) -> Result<(), String> {
    if let Some((p, _)) = funnel_now(port)?.ours {
        run(&["funnel", &format!("--https={p}"), "off"])?;
    }
    Ok(())
}

/// Forward the tailnet's HTTPS to `http://127.0.0.1:<port>`: the URL it's reached at. 443 set
/// up for anything else is someone else's: it's left as it is, and this fails. Other ports are
/// never touched.
pub fn serve(port: u16) -> Result<String, String> {
    let s = status()?;
    let name = s
        .name
        .ok_or("Tailscale doesn't give this machine a name: is MagicDNS on?")?;
    if let Use::Taken(what) = use_of_443(port)? {
        return Err(taken(&name, &what));
    }
    run(&[
        "serve",
        "--bg",
        &format!("--https={PORT}"),
        &format!("http://127.0.0.1:{port}"),
    ])?;
    Ok(format!("https://{name}"))
}

/// Stop forwarding 443 to the server on `port`, if that's still all 443 does: anything someone
/// else set up there since, and every other port, stay. (Never `tailscale serve reset`.)
pub fn off(port: u16) -> Result<(), String> {
    match use_of_443(port)? {
        Use::Ours => run(&["serve", &format!("--https={PORT}"), "off"]).map(|_| ()),
        _ => Ok(()),
    }
}

/// `tailscale <args>`'s output, or what it said went wrong.
fn run(args: &[&str]) -> Result<String, String> {
    let mut child = Command::new(program())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't run tailscale ({e}): is it installed?"))?;
    let until = Instant::now() + PATIENCE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                let said = String::from_utf8_lossy(&out.stdout).to_string()
                    + &String::from_utf8_lossy(&out.stderr);
                return Err(format!(
                    "`tailscale {}` didn't finish: {}",
                    args.join(" "),
                    said.trim()
                ));
            }
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).to_string()
            + &String::from_utf8_lossy(&out.stdout);
        return Err(format!(
            "`tailscale {}` failed: {}",
            args.join(" "),
            said.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `tailscale` on PATH; the Mac app's own CLI when it isn't (the app from the App Store puts
/// none there); `FLOTILLA_TAILSCALE` names one exactly.
fn program() -> std::path::PathBuf {
    if let Some(p) = crate::tools::named("FLOTILLA_TAILSCALE") {
        return p.unwrap_or_else(|| "/nonexistent/tailscale".into());
    }
    crate::tools::on_path("tailscale")
        .or_else(|| {
            ["/Applications/Tailscale.app/Contents/MacOS/Tailscale"]
                .into_iter()
                .map(std::path::PathBuf::from)
                .find(|p| p.is_file())
        })
        .unwrap_or_else(|| "tailscale".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_status() {
        let up = r#"{"BackendState":"Running","CertDomains":["laptop.tail1.ts.net"],"Self":{"DNSName":"laptop.tail1.ts.net.","ID":"nAbC123"}}"#;
        assert_eq!(
            Status::read(up).unwrap(),
            Status {
                running: true,
                name: Some("laptop.tail1.ts.net".into()),
                https: true,
                id: Some("nAbC123".into()),
                funnel: None
            }
        );
        let down = r#"{"BackendState":"Stopped","CertDomains":null,"Self":{"DNSName":""}}"#;
        assert_eq!(
            Status::read(down).unwrap(),
            Status {
                running: false,
                name: None,
                https: false,
                id: None,
                funnel: None
            }
        );
        // Funnel allowed, on the ports its policy says (CapMap, or Capabilities on older Tailscale).
        let funnel = r#"{"BackendState":"Running","CertDomains":["b.t.ts.net"],"Self":{"DNSName":"b.t.ts.net.","CapMap":{"https":null,"funnel":null,"https://tailscale.com/cap/funnel-ports?ports=443,8443,10000":null}}}"#;
        assert_eq!(
            Status::read(funnel).unwrap().funnel,
            Some(vec![443, 8443, 10000])
        );
        let older = r#"{"BackendState":"Running","Self":{"Capabilities":["https","https://tailscale.com/cap/funnel","https://tailscale.com/cap/funnel-ports?ports=8000-8002"]}}"#;
        assert_eq!(
            Status::read(older).unwrap().funnel,
            Some(vec![8000, 8001, 8002])
        );
        let no = r#"{"BackendState":"Running","Self":{"CapMap":{"https":null,"https://tailscale.com/cap/funnel-ports?ports=443":null}}}"#;
        assert_eq!(Status::read(no).unwrap().funnel, None);
        assert!(Status::read("nope").is_err());

        assert!(
            is_local_port("http://127.0.0.1:7777", 7777)
                && !is_local_port("http://127.0.0.1:7777", 7778)
        );
        assert!(
            is_local_port("127.0.0.1:7777/", 7777) && is_local_port("http://localhost:7777", 7777)
        );
        assert!(!is_local_port("http://10.0.0.2:7777", 7777));
    }

    /// 443 is ours only when all it does is forward `/` to this server.
    #[test]
    fn who_has_443() {
        let web = |handlers: &str| {
            format!(
                r#"{{"TCP":{{"443":{{"HTTPS":true}}}},"Web":{{"laptop.t.ts.net:443":{{"Handlers":{{{handlers}}}}}}}}}"#
            )
        };
        assert_eq!(port_use("{}", 7777).unwrap(), Use::Free);
        assert_eq!(port_use(r#"{"Web":{"laptop.t.ts.net:8443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3773"}}}}}"#, 7777).unwrap(), Use::Free);
        assert_eq!(
            port_use(&web(r#""/":{"Proxy":"http://127.0.0.1:7777"}"#), 7777).unwrap(),
            Use::Ours
        );
        assert!(matches!(
            port_use(&web(r#""/":{"Proxy":"http://127.0.0.1:9000"}"#), 7777).unwrap(),
            Use::Taken(_)
        ));
        // Files, not a forward.
        assert_eq!(
            port_use(&web(r#""/":{"Path":"/srv/www"}"#), 7777).unwrap(),
            Use::Taken("/ → /srv/www".into())
        );
        // Ours at /, and someone's at another path.
        assert!(matches!(port_use(&web(r#""/":{"Proxy":"http://127.0.0.1:7777"},"/api":{"Proxy":"http://127.0.0.1:8080"}"#), 7777).unwrap(), Use::Taken(_)));
        assert!(matches!(
            port_use(r#"{"TCP":{"443":{"TCPForward":"127.0.0.1:22"}}}"#, 7777).unwrap(),
            Use::Taken(_)
        ));
        // Forwards on other ports (a machine may keep its own on 4000, 4026 and 8443) aren't 443's: beside
        // a free 443 they leave it free, and beside someone else's they're not named with it.
        let others = r#""laptop.t.ts.net:4000":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:4000"}}},"laptop.t.ts.net:4026":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:4026"}}},"laptop.t.ts.net:8443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3773"}}}"#;
        assert_eq!(
            port_use(&format!(r#"{{"Web":{{{others}}}}}"#), 4000).unwrap(),
            Use::Free
        );
        let theirs = format!(
            r#"{{"Web":{{{others},"laptop.t.ts.net:443":{{"Handlers":{{"/":{{"Proxy":"http://127.0.0.1:9000"}}}}}}}}}}"#
        );
        assert_eq!(
            port_use(&theirs, 7777).unwrap(),
            Use::Taken("/ → http://127.0.0.1:9000".into())
        );
    }

    /// `FLOTILLA_TAILSCALE` is set by one test at a time.
    static STAND_IN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A stand-in `tailscale` that logs what it's asked, and answers `serve status --json`
    /// with `json` and `status --json` with what's in `status.json` (Funnel allowed, unless a
    /// test writes otherwise).
    #[cfg(unix)]
    fn fake(dir: &std::path::Path, json: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("serve.json"), json).unwrap();
        std::fs::write(dir.join("status.json"), r#"{"BackendState":"Running","CertDomains":["box.t.ts.net"],"Self":{"DNSName":"box.t.ts.net.","ID":"nBox1","CapMap":{"https":null,"funnel":null,"https://tailscale.com/cap/funnel-ports?ports=443,8443,10000":null}}}"#).unwrap();
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{d}/asked'\ncase \"$*\" in\n  'status --json') cat '{d}/status.json' ;;\n  'serve status --json') cat '{d}/serve.json' ;;\nesac\n",
            d = dir.display()
        );
        let p = dir.join("tailscale");
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// Only 443, and only while it's Flotilla's: the machine's other forwards (4000, 8443) are never
    /// touched, a 443 forward to something else is neither replaced nor removed, and nothing
    /// ever resets Tailscale's settings.
    #[cfg(unix)]
    #[test]
    fn only_its_own_forward_is_made_and_removed() {
        let _one = STAND_IN.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("flotilla-ts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let others = r#""box.t.ts.net:4000":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:4000"}}},"box.t.ts.net:8443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3773"}}}"#;
        let p = fake(&dir, &format!(r#"{{"Web":{{{others}}}}}"#));
        // SAFETY: tests in this module run one at a time on this variable, and nothing else reads it.
        unsafe { std::env::set_var("FLOTILLA_TAILSCALE", &p) };
        let asked = || std::fs::read_to_string(dir.join("asked")).unwrap_or_default();

        assert_eq!(serve(7777).unwrap(), "https://box.t.ts.net");
        assert!(
            asked().contains("serve --bg --https=443 http://127.0.0.1:7777"),
            "{}",
            asked()
        );
        // Ours now: stopping removes it, and only it.
        std::fs::write(dir.join("serve.json"), format!(r#"{{"Web":{{{others},"box.t.ts.net:443":{{"Handlers":{{"/":{{"Proxy":"http://127.0.0.1:7777"}}}}}}}}}}"#)).unwrap();
        off(7777).unwrap();
        assert!(
            asked().lines().any(|l| l == "serve --https=443 off"),
            "{}",
            asked()
        );

        // Someone else's 443: not replaced, not removed.
        std::fs::remove_file(dir.join("asked")).unwrap();
        std::fs::write(dir.join("serve.json"), format!(r#"{{"Web":{{{others},"box.t.ts.net:443":{{"Handlers":{{"/":{{"Proxy":"http://127.0.0.1:9000"}}}}}}}}}}"#)).unwrap();
        let e = serve(7777).unwrap_err();
        assert_eq!(
            e,
            "Tailscale already serves https://box.t.ts.net (/ → http://127.0.0.1:9000), so Flotilla leaves it alone"
        );
        off(7777).unwrap();
        assert!(
            !asked().contains("--bg") && !asked().contains(" off"),
            "{}",
            asked()
        );
        assert!(!asked().contains("reset"));
        unsafe { std::env::remove_var("FLOTILLA_TAILSCALE") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Funnel's ports for remote MCP: whose each is, and which to use.
    #[test]
    fn who_has_funnels_ports() {
        let web = |port: u16, to: &str| {
            format!(r#""box.t.ts.net:{port}":{{"Handlers":{{"/":{{"Proxy":"{to}"}}}}}}"#)
        };
        assert_eq!(
            funnel_use("{}", 8787).unwrap(),
            Funnel {
                ours: None,
                free: vec![8443, 10000],
                taken: vec![]
            }
        );
        // The main API's private 443 is no Funnel port's business.
        let private = format!(r#"{{"Web":{{{}}}}}"#, web(443, "http://127.0.0.1:7777"));
        assert_eq!(funnel_use(&private, 8787).unwrap().free, vec![8443, 10000]);
        // Ours on 8443, through Funnel; on the tailnet only.
        let ours = format!(
            r#"{{"Web":{{{},{}}},"AllowFunnel":{{"box.t.ts.net:8443":true}}}}"#,
            web(443, "http://127.0.0.1:7777"),
            web(8443, "http://127.0.0.1:8787")
        );
        assert_eq!(funnel_use(&ours, 8787).unwrap().ours, Some((8443, true)));
        let tailnet = format!(r#"{{"Web":{{{}}}}}"#, web(8443, "http://127.0.0.1:8787"));
        assert_eq!(
            funnel_use(&tailnet, 8787).unwrap().ours,
            Some((8443, false))
        );
        // 8443 someone else's: 10000.
        let theirs = format!(r#"{{"Web":{{{}}}}}"#, web(8443, "http://127.0.0.1:3773"));
        let f = funnel_use(&theirs, 8787).unwrap();
        assert_eq!(
            (f.port(&[443, 8443, 10000]), f.taken.len()),
            (Some(10000), 1)
        );
        // Both someone else's: none, and why.
        let both = format!(
            r#"{{"Web":{{{},{}}}}}"#,
            web(8443, "http://127.0.0.1:3773"),
            web(10000, "http://127.0.0.1:9000")
        );
        let f = funnel_use(&both, 8787).unwrap();
        assert_eq!(f.port(&[443, 8443, 10000]), None);
        assert_eq!(
            f.why_none("box.t.ts.net", &[443, 8443, 10000]),
            "Tailscale already serves https://box.t.ts.net:8443 (/ → http://127.0.0.1:3773) and https://box.t.ts.net:10000 (/ → http://127.0.0.1:9000), so Flotilla leaves them alone"
        );
        // A tailnet that allows only 443.
        assert_eq!(
            Funnel {
                free: vec![8443, 10000],
                ..Funnel::default()
            }
            .port(&[443]),
            None
        );
        assert_eq!(
            funnel_url("box.t.ts.net", 8443),
            "https://box.t.ts.net:8443"
        );
    }

    /// Funnel publishes remote MCP alone, on 8443 (or 10000), and stopping takes down only that:
    /// 443's private forward and anyone else's ports are never touched, and a tailnet that
    /// doesn't allow Funnel gets no command run at all.
    #[cfg(unix)]
    #[test]
    fn funnel_publishes_only_remote_mcp() {
        let _one = STAND_IN.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("flotilla-funnel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let private = r#""box.t.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:7777"}}}"#;
        let p = fake(&dir, &format!(r#"{{"Web":{{{private}}}}}"#));
        // SAFETY: STAND_IN keeps this test alone on the variable, and nothing else reads it.
        unsafe { std::env::set_var("FLOTILLA_TAILSCALE", &p) };
        let asked = || std::fs::read_to_string(dir.join("asked")).unwrap_or_default();

        assert_eq!(funnel(8787).unwrap(), "https://box.t.ts.net:8443");
        assert!(
            asked()
                .lines()
                .any(|l| l == "funnel --bg --https=8443 http://127.0.0.1:8787"),
            "{}",
            asked()
        );
        // Published: asked again, it's left as it is; stopped, only 8443 goes.
        std::fs::write(dir.join("serve.json"), format!(r#"{{"Web":{{{private},"box.t.ts.net:8443":{{"Handlers":{{"/":{{"Proxy":"http://127.0.0.1:8787"}}}}}}}},"AllowFunnel":{{"box.t.ts.net:8443":true}}}}"#)).unwrap();
        std::fs::remove_file(dir.join("asked")).unwrap();
        assert_eq!(funnel(8787).unwrap(), "https://box.t.ts.net:8443");
        assert!(!asked().contains("--bg"), "{}", asked());
        funnel_off(8787).unwrap();
        assert_eq!(
            asked()
                .lines()
                .filter(|l| !l.ends_with("--json"))
                .collect::<Vec<_>>(),
            ["funnel --https=8443 off"]
        );

        // 8443 someone else's: 10000, and 8443 left alone.
        std::fs::remove_file(dir.join("asked")).unwrap();
        std::fs::write(dir.join("serve.json"), format!(r#"{{"Web":{{{private},"box.t.ts.net:8443":{{"Handlers":{{"/":{{"Proxy":"http://127.0.0.1:3773"}}}}}}}}}}"#)).unwrap();
        assert_eq!(funnel(8787).unwrap(), "https://box.t.ts.net:10000");
        funnel_off(8787).unwrap();
        assert!(
            asked().contains("funnel --bg --https=10000 http://127.0.0.1:8787")
                && !asked().contains("8443 off")
                && !asked().contains("--https=443"),
            "{}",
            asked()
        );

        // A tailnet that doesn't allow Funnel here: what to change, and nothing run.
        std::fs::remove_file(dir.join("asked")).unwrap();
        std::fs::write(dir.join("serve.json"), "{}").unwrap();
        std::fs::write(dir.join("status.json"), r#"{"BackendState":"Running","CertDomains":["box.t.ts.net"],"Self":{"DNSName":"box.t.ts.net.","ID":"nBox1","CapMap":{"https":null}}}"#).unwrap();
        let e = funnel(8787).unwrap_err();
        assert!(
            e.contains("https://login.tailscale.com/f/funnel?node=nBox1")
                && e.contains("nodeAttrs"),
            "{e}"
        );
        assert!(!asked().contains("funnel"), "{}", asked());
        assert!(!asked().contains("reset"));
        unsafe { std::env::remove_var("FLOTILLA_TAILSCALE") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
