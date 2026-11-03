//! `flotilla doctor`: what isn't set up where the engine runs, and what to run there to fix it.
//! The boat key works and can do what Flotilla does; gh is signed in with the scopes it needs;
//! summaries work; Tailscale serves the server (when it's set up to); the database is writable;
//! remote MCP, an optional part that's off unless it's turned on, is public when it's on.
//! The same checks run when the engine starts (a server logs them), now and then, and when a
//! client asks (`doctor.run`), and every client's Settings shows them.
//!
//! Everything the checks ask the world goes through a [`Probe`], so tests stand in for each
//! failure. A check fails only on an answer that says something is wrong; one that couldn't
//! ask (no network) warns instead.

use crate::github::{GhStatus, TokenError, TokenUser};
use crate::secrets::Source;
use crate::store::Store;
use crate::tailscale;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub use flotilla_proto::{CheckOf, Diagnosis, Doctor, Health};

/// The actions Flotilla's boat key needs (the README's list).
pub const BOAT_ACTIONS: &[&str] = &[
    "sandbox.create",
    "sandbox.read",
    "sandbox.update",
    "sandbox.stop",
    "sandbox.resume",
    "sandbox.fork",
    "sandbox.delete-own",
    "agent.prompt",
    "exec",
    "host",
    "snapshot.read",
    "snapshot.write",
    "environment.read",
    "environment.write",
    "account.read",
];

/// The scope a classic GitHub token needs: repos, pull requests and merges.
pub const GH_SCOPE: &str = "repo";

/// The command that makes a key that can do everything Flotilla does.
pub fn boat_key_command() -> String {
    format!(
        "boat api-key create Flotilla --ttl 365d --actions {}",
        BOAT_ACTIONS.join(",")
    )
}

const SETUP: &str = "flotilla setup";

/// Why boat didn't answer as hoped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// 401 or 403: it says no to the key.
    Refused(String),
    /// No answer, or one that isn't about the key.
    Other(String),
}

/// Why summaries don't work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeError {
    NotFound,
    Failed(String),
}

/// What the checks ask the world. [`System`] asks it for real.
pub trait Probe {
    /// The boat key and where it's kept.
    fn boat_key(&self) -> Result<Option<(String, Source)>, String>;
    /// boat's `/me` with the key.
    fn boat_me(&self, key: &str) -> Result<(), Refusal>;
    /// boat's `/api-keys` with the key.
    fn boat_keys(&self, key: &str) -> Result<Value, Refusal>;
    /// gh's own status (or `GH_TOKEN`'s, with no gh).
    fn gh(&self) -> GhStatus;
    /// The token GitHub calls use.
    fn github_token(&self) -> Result<String, String>;
    fn github_user(&self, token: &str) -> Result<TokenUser, TokenError>;
    /// Whether `GH_TOKEN` is what's used.
    fn github_env(&self) -> Option<&'static str>;
    /// Whether a `claude setup-token` token is kept.
    fn claude_token(&self) -> Result<bool, String>;
    /// A tiny summary with `model`.
    fn summaries(&self, model: &str) -> Result<(), ClaudeError>;
    fn tailscale(&self) -> Result<tailscale::Status, String>;
    /// Who has Tailscale's 443, for the server on `port`.
    fn port_use(&self, port: u16) -> Result<tailscale::Use, String>;
    /// This flavor's thread VMs on the key's boat account, any Flotilla's.
    fn vms(&self, key: &str) -> Result<Vec<crate::boat::Sandbox>, String>;
    /// What Tailscale's Funnel ports do, for remote MCP on `port`.
    fn funnel(&self, _port: u16) -> Result<tailscale::Funnel, String> {
        Err("not checked".into())
    }
    /// Whether remote MCP answers at its public address (`https://box.example.com`), from here.
    fn reachable(&self, _url: &str) -> Result<(), String> {
        Err("not checked".into())
    }
    /// Whether notifications to phones are set up (`crate::push`). Stand-ins: not.
    fn push(&self, _store: Option<&Store>) -> crate::push::Status {
        crate::push::Status::Off
    }
}

/// Where to check.
#[derive(Debug, Clone)]
pub struct At {
    pub db: PathBuf,
    /// Check Tailscale (a server; the app on This Mac doesn't use it).
    pub tailscale: bool,
    /// The server's port, while it's serving: Tailscale must forward to it.
    pub serving: Option<u16>,
    /// Remote MCP's port, while it listens: published, if it's set up to be.
    pub mcp: Option<u16>,
    /// The app on This Mac, not a server: its fixes are its own (the key goes in Settings,
    /// claude signs in as you), not `flotilla setup`.
    pub app: bool,
    /// The database while its server holds it: checks still going when the server stops
    /// leave it alone (they're off its thread). `Gate::default()` where nothing stops.
    pub gate: crate::lock::Gate,
}

/// What the checks read in the database.
#[derive(Default)]
struct Kept {
    model: Option<String>,
    serve: Option<String>,
    push: Option<crate::push::Status>,
    owner: Option<crate::owner::Owner>,
    /// Remote MCP, on a server: None when the app runs the checks, Some(None) when it's off.
    mcp: Option<Option<crate::remote_mcp::Settings>>,
}

/// Read what the checks need from the database first, in one go: the checks after ask the
/// network and can take a while, and the server may let go of the database meanwhile. Nothing
/// (all `None`) once it has.
fn kept(probe: &dyn Probe, at: &At) -> Kept {
    at.gate
        .with(|| {
            let store = Store::open(&at.db).ok();
            let kv = |key: &str| store.as_ref().and_then(|s| s.kv_get(key).ok().flatten());
            Kept {
                model: kv(crate::summarize::MODEL_KEY),
                serve: kv(tailscale::KEY),
                // Phones are paired with a server, not with the app on This Mac.
                push: (!at.app).then(|| probe.push(store.as_ref())),
                owner: store
                    .as_ref()
                    .and_then(|s| crate::owner::Owner::load(s).ok()),
                // A server's, as push is: the app on This Mac has no remote MCP.
                mcp: (!at.app).then(|| store.as_ref().and_then(crate::remote_mcp::load)),
            }
        })
        .unwrap_or_default()
}

/// Run every check.
pub fn run(probe: &dyn Probe, at: &At) -> Doctor {
    let kept = kept(probe, at);
    let model = kept
        .model
        .unwrap_or_else(|| crate::summarize::DEFAULT_MODEL.to_string());
    let boat_check = boat(probe, at.app);
    let key_works = boat_check.health != Health::Fail;
    let mut checks = vec![
        boat_check,
        github(probe),
        claude_for(probe, model.trim(), at.app),
    ];
    if at.tailscale {
        checks.push(tailscale(
            probe,
            kept.serve.as_deref() == Some("on"),
            at.serving,
        ));
    }
    checks.extend(at.gate.with(|| database(&at.db)));
    checks.extend(kept.push.map(push));
    checks.extend(kept.mcp.map(|s| remote_mcp(probe, s.as_ref(), at)));
    if key_works
        && let Some(owner) = &kept.owner
        && let Some(c) = others(probe, owner)
    {
        checks.push(c);
    }
    Doctor {
        checks,
        at_ms: crate::util::now_ms(),
    }
}

/// Another Flotilla's VMs on this boat account, if there are any: information, not a fault
/// (this one leaves them alone). Nothing when they can't be listed: the key's check says why.
fn others(probe: &dyn Probe, owner: &crate::owner::Owner) -> Option<Diagnosis> {
    let (key, _) = probe.boat_key().ok()??;
    let vms = probe.vms(&key).ok()?;
    owner
        .others(&vms)
        .map(|s| says(CheckOf::Others, Health::Ok, s, &[]))
}

/// Dev, with no engine (`FLOTILLA_OFFLINE`): only what needs no network, the database (and
/// push, on a server).
pub fn offline(at: &At) -> Doctor {
    let mut checks: Vec<Diagnosis> = at.gate.with(|| database(&at.db)).into_iter().collect();
    if !at.app {
        checks.extend(
            at.gate
                .with(|| push(crate::push::status(Store::open(&at.db).ok().as_ref()))),
        );
    }
    Doctor {
        checks,
        at_ms: crate::util::now_ms(),
    }
}

/// Every client sees what the checks say: anything that looks like a secret is hidden.
fn says(check: CheckOf, health: Health, says: impl Into<String>, fix: &[&str]) -> Diagnosis {
    let redact = crate::util::redact;
    Diagnosis {
        check,
        health,
        says: redact(&says.into()),
        fix: fix.iter().map(|f| redact(f)).collect(),
    }
}

fn boat(probe: &dyn Probe, app: bool) -> Diagnosis {
    let new_key = boat_key_command();
    let fixes: &[&str] = if app { &[&new_key] } else { &[&new_key, SETUP] };
    match probe.boat_key() {
        Ok(Some((key, source))) => boat_key_for(probe, &key, &source, app),
        Ok(None) => says(CheckOf::BoatKey, Health::Fail, "No boat API key", fixes),
        Err(e) => says(CheckOf::BoatKey, Health::Fail, e, &[]),
    }
}

/// Whether `key` (from `source`) works and can do what Flotilla does.
pub fn boat_key(probe: &dyn Probe, key: &str, source: &Source) -> Diagnosis {
    boat_key_for(probe, key, source, false)
}

fn boat_key_for(probe: &dyn Probe, key: &str, source: &Source, app: bool) -> Diagnosis {
    let new_key = boat_key_command();
    let fixes: &[&str] = if app { &[&new_key] } else { &[&new_key, SETUP] };
    let fail = |s: String| says(CheckOf::BoatKey, Health::Fail, s, fixes);
    let key = key.to_string();
    let from = format!("from {source}");
    match probe.boat_me(&key) {
        Ok(()) => {}
        Err(Refusal::Refused(e)) => return fail(format!("boat.dev refused the key {from} ({e})")),
        Err(Refusal::Other(e)) => {
            return says(
                CheckOf::BoatKey,
                Health::Warn,
                format!("Couldn't check the key {from}: {e}"),
                &[],
            );
        }
    }
    let last4: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let works = format!("Works · ends …{last4} · {from}");
    let keys = match probe.boat_keys(&key) {
        Ok(v) => v,
        Err(Refusal::Refused(e) | Refusal::Other(e)) => {
            return says(
                CheckOf::BoatKey,
                Health::Warn,
                format!("{works}. boat.dev wouldn't list its actions: {e}"),
                fixes,
            );
        }
    };
    let ours = keys["apiKeys"].as_array().into_iter().flatten().find(|k| {
        let (prefix, four) = (
            k["keyPrefix"].as_str().unwrap_or("\u{0}"),
            k["keyLastFour"].as_str().unwrap_or("\u{0}"),
        );
        key.starts_with(prefix) && key.ends_with(four)
    });
    let Some(ours) = ours else {
        return says(
            CheckOf::BoatKey,
            Health::Warn,
            format!("{works}. boat.dev didn't list it, so its actions weren't checked"),
            &[],
        );
    };
    // The day where it's read, not UTC's (a key ending at 02:00 UTC ends the evening before here).
    let ends = ours["expiresAt"].as_str().and_then(crate::util::parse_iso);
    let until = ends.map(|t| {
        t.with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    });
    if ours["expired"].as_bool() == Some(true) {
        return fail(format!(
            "The key {from} expired{}",
            until.map(|u| format!(" on {u}")).unwrap_or_default()
        ));
    }
    let scope = &ours["scope"];
    if !scope.is_null() && scope["grandfathered"].as_bool() != Some(true) {
        let actions: Vec<&str> = scope["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !actions.contains(&"*") {
            let missing: Vec<&str> = BOAT_ACTIONS
                .iter()
                .copied()
                .filter(|a| !actions.contains(a))
                .collect();
            if !missing.is_empty() {
                return fail(format!("The key {from} can't do {}", missing.join(", ")));
            }
        }
        for (field, what) in [("sandboxes", "VMs"), ("environments", "environments")] {
            if !matches!(&scope[field], Value::String(s) if s == "*") && !scope[field].is_null() {
                return fail(format!("The key {from} only reaches some {what}"));
            }
        }
    }
    if ours["expiringSoon"].as_bool() == Some(true) {
        let when = ends
            .map(|t| expires_in(&chrono::Local, t, chrono::Utc::now()))
            .unwrap_or_else(|| "soon".into());
        return says(
            CheckOf::BoatKey,
            Health::Warn,
            format!("{works} · expires {when}"),
            fixes,
        );
    }
    says(CheckOf::BoatKey, Health::Ok, works, &[])
}

/// When a key that ends soon ends, in `tz`, so the warning says why: "today at 08:25",
/// "tomorrow at 08:25", "in 5 days, on 2026-11-06".
fn expires_in<Tz: chrono::TimeZone>(
    tz: &Tz,
    at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let (at, now) = (at.with_timezone(tz), now.with_timezone(tz));
    match (at.date_naive() - now.date_naive()).num_days() {
        ..=0 => format!("today at {}", at.format("%H:%M")),
        1 => format!("tomorrow at {}", at.format("%H:%M")),
        days => format!("in {days} days, on {}", at.format("%Y-%m-%d")),
    }
}

/// Whether gh (or `GH_TOKEN`) is signed in to GitHub with the scope Flotilla needs.
pub fn github(probe: &dyn Probe) -> Diagnosis {
    let login = format!("gh auth login --hostname {}", crate::github::HOST);
    let fail = |s: String, fix: &[&str]| says(CheckOf::GitHub, Health::Fail, s, fix);
    let env = probe.github_env();
    // With GH_TOKEN, gh isn't used: GitHub's answer to the token is all that counts.
    let status = if env.is_some() {
        GhStatus::Unknown
    } else {
        probe.gh()
    };
    let version = match &status {
        GhStatus::SignedIn { version, .. } => version.clone(),
        GhStatus::NotFound => {
            return fail(
                "gh isn't installed".into(),
                &["Install gh: https://cli.github.com", &login],
            );
        }
        GhStatus::TooOld { version } => {
            let (a, b) = crate::github::MIN_GH;
            return fail(
                format!("gh {version} is too old: Flotilla needs {a}.{b} or newer"),
                &["Update gh: https://cli.github.com"],
            );
        }
        GhStatus::NotSignedIn { .. } => {
            return fail(
                format!("gh isn't signed in to {}", crate::github::HOST),
                &[&login],
            );
        }
        GhStatus::Failed(e) => {
            return says(
                CheckOf::GitHub,
                Health::Warn,
                format!("gh didn't answer as expected: {e}"),
                &["gh auth status"],
            );
        }
        GhStatus::Unknown => String::new(),
    };
    let token = match probe.github_token() {
        Ok(t) => t,
        Err(e) => return fail(e, &[&login]),
    };
    let via = match env {
        Some(var) => var.to_string(),
        None if version.is_empty() => "gh".into(),
        None => format!("gh {version}"),
    };
    match probe.github_user(&token) {
        Err(TokenError::Refused) if env.is_some() => fail(format!("GitHub refused {via}"), &[]),
        Err(TokenError::Refused) => fail("GitHub refused gh's sign-in".into(), &[&login]),
        Err(TokenError::Other(e)) => says(
            CheckOf::GitHub,
            Health::Warn,
            format!("Couldn't check with GitHub: {e}"),
            &[],
        ),
        Ok(u) => match u.scopes {
            Some(scopes) if !scopes.iter().any(|s| s == GH_SCOPE) => {
                let refresh = format!(
                    "gh auth refresh --hostname {} --scopes {GH_SCOPE}",
                    crate::github::HOST
                );
                let fix: &[&str] = if env.is_some() { &[] } else { &[&refresh] };
                fail(
                    format!("{}'s token lacks the {GH_SCOPE} scope ({via})", u.login),
                    fix,
                )
            }
            Some(_) => says(
                CheckOf::GitHub,
                Health::Ok,
                format!("Signed in as {} ({via})", u.login),
                &[],
            ),
            None => says(
                CheckOf::GitHub,
                Health::Ok,
                format!("Signed in as {} ({via}, a fine-grained token)", u.login),
                &[],
            ),
        },
    }
}

/// Whether summaries work with `model` ("": they're off).
pub fn claude(probe: &dyn Probe, model: &str) -> Diagnosis {
    claude_for(probe, model, false)
}

fn claude_for(probe: &dyn Probe, model: &str, app: bool) -> Diagnosis {
    // On This Mac claude signs in as you; a server uses a `claude setup-token` token.
    let fix = if app { "claude auth login" } else { SETUP };
    if model.is_empty() {
        return says(CheckOf::Claude, Health::Off, "Off", &[]);
    }
    let token = probe.claude_token();
    let with = match &token {
        Ok(true) => "the setup-token token",
        Ok(false) => "claude's own sign-in",
        Err(e) => return says(CheckOf::Claude, Health::Fail, e.clone(), &[]),
    };
    match probe.summaries(model) {
        Ok(()) => says(
            CheckOf::Claude,
            Health::Ok,
            format!("Work with {model} · {with}"),
            &[],
        ),
        Err(ClaudeError::NotFound) => says(
            CheckOf::Claude,
            Health::Fail,
            "The claude CLI isn't installed",
            &["curl -fsSL https://claude.ai/install.sh | bash", fix],
        ),
        // Too slow to say: not a failure.
        Err(ClaudeError::Failed(e)) if e.contains("took longer than") => says(
            CheckOf::Claude,
            Health::Warn,
            format!("Didn't answer in time with {model} ({with}): {e}"),
            &[],
        ),
        Err(ClaudeError::Failed(e)) => says(
            CheckOf::Claude,
            Health::Fail,
            format!("Fail with {model} ({with}): {e}"),
            &[fix],
        ),
    }
}

/// Tailscale, when the server's set up to use it (`on`): ready (see [`tailscale_ready`]),
/// and while it serves on `serving`, forwarding 443 to it. 443 set up for anything else is
/// someone else's: said, never offered for removal.
pub fn tailscale(probe: &dyn Probe, on: bool, serving: Option<u16>) -> Diagnosis {
    if !on {
        return says(
            CheckOf::Tailscale,
            Health::Off,
            "Not set up",
            &["flotilla setup --tailscale-serve"],
        );
    }
    let (ready, name) = tailscale_ready(probe.tailscale());
    let (Some(name), Some(port)) = (name, serving) else {
        return ready;
    };
    match probe.port_use(port) {
        Ok(tailscale::Use::Ours) => says(
            CheckOf::Tailscale,
            Health::Ok,
            format!("Serving https://{name}"),
            &[],
        ),
        Ok(tailscale::Use::Free) => says(
            CheckOf::Tailscale,
            Health::Fail,
            format!("Tailscale isn't forwarding https://{name} to Flotilla"),
            &[&tailscale::serve_command(port)],
        ),
        Ok(tailscale::Use::Taken(what)) => says(
            CheckOf::Tailscale,
            Health::Fail,
            tailscale::taken(&name, &what),
            &[],
        ),
        Err(e) => says(
            CheckOf::Tailscale,
            Health::Warn,
            format!("Couldn't read what Tailscale serves: {e}"),
            &[],
        ),
    }
}

/// Whether Tailscale can serve this machine, from one `tailscale status`: connected, named
/// and given HTTPS. The verdict, and the name when it can (what `flotilla setup` gives out).
pub fn tailscale_ready(status: Result<tailscale::Status, String>) -> (Diagnosis, Option<String>) {
    let fail = |s: &str, fix: &[&str]| (says(CheckOf::Tailscale, Health::Fail, s, fix), None);
    let status = match status {
        Ok(s) => s,
        Err(e) => {
            return fail(
                &e,
                &[
                    "Install Tailscale: https://tailscale.com/download",
                    "tailscale up",
                ],
            );
        }
    };
    if !status.running {
        return fail("Tailscale isn't connected", &["tailscale up"]);
    }
    let Some(name) = status.name else {
        return fail(
            "Tailscale gives this machine no name: MagicDNS is off",
            &["Turn on MagicDNS: https://login.tailscale.com/admin/dns"],
        );
    };
    if !status.https {
        return fail(
            "The tailnet doesn't give HTTPS certificates",
            &["Turn on HTTPS: https://login.tailscale.com/admin/dns"],
        );
    }
    (
        says(
            CheckOf::Tailscale,
            Health::Ok,
            format!("Ready to serve https://{name}"),
            &[],
        ),
        Some(name),
    )
}

/// Remote MCP: an optional part, off unless it's turned on (and then said so, as information,
/// never a fault). On, whether clients can reach it: published with Tailscale Funnel, at the
/// address it was given, or on this machine only, as it's set up. While the server isn't
/// running, whether it's ready to be.
pub fn remote_mcp(
    probe: &dyn Probe,
    settings: Option<&crate::remote_mcp::Settings>,
    at: &At,
) -> Diagnosis {
    let Some(s) = settings else {
        return says(CheckOf::Mcp, Health::Off, "Off", &[]);
    };
    // A server reads the setting as it starts.
    if at.serving.is_some() && at.mcp.is_none() {
        return says(
            CheckOf::Mcp,
            Health::Fail,
            "On, but the server started before it was: restart it",
            &["flotilla service restart"],
        );
    }
    let serving = at.mcp.is_some();
    if s.funnel {
        let status = probe.tailscale();
        let (ready, name) = tailscale_ready(status.clone());
        let (Some(name), Ok(status)) = (name, status) else {
            return Diagnosis {
                check: CheckOf::Mcp,
                ..ready
            };
        };
        let Some(allowed) = &status.funnel else {
            let fixes = status.funnel_fixes();
            return says(
                CheckOf::Mcp,
                Health::Fail,
                "The tailnet doesn't let this machine use Tailscale Funnel",
                &fixes.iter().map(String::as_str).collect::<Vec<_>>(),
            );
        };
        if !serving {
            return says(
                CheckOf::Mcp,
                Health::Ok,
                "Ready to publish with Tailscale Funnel",
                &[],
            );
        }
        return match probe.funnel(s.port) {
            Ok(f) => match f.ours {
                Some((p, true)) => says(
                    CheckOf::Mcp,
                    Health::Ok,
                    format!("Public at {}/mcp", tailscale::funnel_url(&name, p)),
                    &[],
                ),
                Some((p, false)) => says(
                    CheckOf::Mcp,
                    Health::Fail,
                    format!(
                        "{} is on your tailnet only: Funnel isn't on",
                        tailscale::funnel_url(&name, p)
                    ),
                    &[&tailscale::funnel_command(p, s.port)],
                ),
                None => match f.port(allowed) {
                    Some(p) => says(
                        CheckOf::Mcp,
                        Health::Fail,
                        "Not public: Tailscale Funnel isn't publishing it",
                        &[&tailscale::funnel_command(p, s.port)],
                    ),
                    None => says(CheckOf::Mcp, Health::Fail, f.why_none(&name, allowed), &[]),
                },
            },
            Err(e) => says(
                CheckOf::Mcp,
                Health::Warn,
                format!("Couldn't read what Tailscale serves: {e}"),
                &[],
            ),
        };
    }
    let local = format!("http://{}:{}/mcp", s.host(), s.port);
    match &s.url {
        Some(url) if serving => match probe.reachable(url) {
            Ok(()) => says(
                CheckOf::Mcp,
                Health::Ok,
                format!("Public at {url}/mcp"),
                &[],
            ),
            Err(e) => says(
                CheckOf::Mcp,
                Health::Fail,
                format!("{url}/mcp doesn't reach it ({e}): publish {local} there"),
                &[],
            ),
        },
        Some(url) => says(
            CheckOf::Mcp,
            Health::Ok,
            format!("Ready to serve {url}/mcp"),
            &[],
        ),
        None => says(
            CheckOf::Mcp,
            Health::Ok,
            format!("On at {local}, on this machine only"),
            &[],
        ),
    }
}

/// Whether the database and the uploads beside it can be written.
pub fn database(db: &Path) -> Diagnosis {
    let r = (|| -> crate::Result<()> {
        Store::open(db)?.check_writable()?;
        // Uploads go beside it.
        let blobs = crate::blobs::Blobs::beside(db);
        let probe = blobs.put(b"flotilla doctor")?;
        let _ = blobs.size(&probe.id)?;
        Ok(())
    })();
    match r {
        Ok(()) => says(
            CheckOf::Database,
            Health::Ok,
            format!("Writable · {}", db.display()),
            &[],
        ),
        Err(e) => says(
            CheckOf::Database,
            Health::Fail,
            format!("Can't write {}: {e}", db.display()),
            &[],
        ),
    }
}

/// Notifications to paired phones: optional, so off is information, not a fault.
pub fn push(status: crate::push::Status) -> Diagnosis {
    match status {
        crate::push::Status::Off => says(CheckOf::Push, Health::Off, "Off", &[]),
        crate::push::Status::Relay { url } => {
            let at = reqwest::Url::parse(&url)
                .ok()
                .and_then(|u| {
                    u.host_str()
                        .map(|h| u.port().map_or(h.to_string(), |p| format!("{h}:{p}")))
                })
                .unwrap_or(url);
            says(
                CheckOf::Push,
                Health::Ok,
                format!("Through the relay at {at}"),
                &[],
            )
        }
        crate::push::Status::Ready { key_id, team_id } => says(
            CheckOf::Push,
            Health::Ok,
            format!("Own key {key_id} · team {team_id}"),
            &[],
        ),
        crate::push::Status::Broken(e) => says(
            CheckOf::Push,
            Health::Fail,
            e,
            &[crate::push::SETUP_COMMAND],
        ),
        // Only FLOTILLA_PUSH_RELAY can be one (setup checks what it keeps), which it names.
        crate::push::Status::BadRelay(e) => says(CheckOf::Push, Health::Fail, e, &[]),
    }
}

/// The checks as lines for a terminal or a log.
pub fn lines(d: &Doctor) -> Vec<String> {
    let mut out = Vec::new();
    for c in &d.checks {
        let mark = match c.health {
            Health::Ok => "✓",
            Health::Warn => "!",
            Health::Fail => "✗",
            Health::Off => "-",
        };
        out.push(format!("{mark} {}: {}", c.check.label(), c.says));
        for f in &c.fix {
            out.push(format!("    {f}"));
        }
    }
    out
}

/// The real world: the secrets this process finds, boat.dev, gh and GitHub, the claude CLI,
/// and Tailscale.
pub struct System;

impl Probe for System {
    fn push(&self, store: Option<&Store>) -> crate::push::Status {
        crate::push::status(store)
    }

    fn boat_key(&self) -> Result<Option<(String, Source)>, String> {
        crate::secrets::store()
            .get(crate::secrets::Name::BoatKey)
            .map_err(|e| e.to_string())
    }

    fn boat_me(&self, key: &str) -> Result<(), Refusal> {
        boat_client(key)?.me().map(|_| ()).map_err(refusal)
    }

    fn boat_keys(&self, key: &str) -> Result<Value, Refusal> {
        boat_client(key)?.api_keys().map_err(refusal)
    }

    fn gh(&self) -> GhStatus {
        crate::github::Gh::new().check()
    }

    fn github_token(&self) -> Result<String, String> {
        crate::github::Gh::new().token().map_err(|e| e.to_string())
    }

    fn github_user(&self, token: &str) -> Result<TokenUser, TokenError> {
        crate::github::token_user(token)
    }

    fn github_env(&self) -> Option<&'static str> {
        crate::github::env_token().map(|(_, var)| var)
    }

    fn claude_token(&self) -> Result<bool, String> {
        crate::secrets::claude_token()
            .map(|t| t.is_some())
            .map_err(|e| e.to_string())
    }

    fn summaries(&self, model: &str) -> Result<(), ClaudeError> {
        if crate::summarize::find_claude().is_none() {
            return Err(ClaudeError::NotFound);
        }
        crate::summarize::Summarizer::new(model)
            .and_then(|s| s.test())
            .map_err(|e| ClaudeError::Failed(e.to_string()))
    }

    fn tailscale(&self) -> Result<tailscale::Status, String> {
        tailscale::status()
    }

    fn port_use(&self, port: u16) -> Result<tailscale::Use, String> {
        tailscale::use_of_443(port)
    }

    fn vms(&self, key: &str) -> Result<Vec<crate::boat::Sandbox>, String> {
        let boat = crate::boat::BoatClient::new(key.to_string()).map_err(|e| e.to_string())?;
        boat.list_sandboxes(&[("flotilla", crate::flavor::vm_label())])
            .map_err(|e| e.to_string())
    }

    fn funnel(&self, port: u16) -> Result<tailscale::Funnel, String> {
        tailscale::funnel_now(port)
    }

    /// Its protected resource metadata, through whatever publishes it: it must say it's this
    /// address's MCP.
    fn reachable(&self, url: &str) -> Result<(), String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        let r = client
            .get(format!("{url}/.well-known/oauth-protected-resource"))
            .send()
            .map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("it answered {}", r.status()));
        }
        let v: Value = r
            .json()
            .map_err(|_| "what answered isn't Flotilla".to_string())?;
        if v["resource"].as_str() == Some(&format!("{url}/mcp")) {
            Ok(())
        } else {
            Err("what answered isn't this Flotilla's MCP".into())
        }
    }
}

fn boat_client(key: &str) -> Result<crate::boat::BoatClient, Refusal> {
    crate::boat::BoatClient::new(key.to_string()).map_err(|e| Refusal::Other(e.to_string()))
}

fn refusal(e: crate::BoatError) -> Refusal {
    match e.status() {
        Some(401 | 403) => Refusal::Refused(e.api().map(|a| a.code.clone()).unwrap_or_default()),
        _ => Refusal::Other(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Everything set up and working; each test breaks one part.
    #[derive(Clone)]
    struct Fake {
        key: Result<Option<(String, Source)>, String>,
        me: Result<(), Refusal>,
        keys: Result<Value, Refusal>,
        gh: GhStatus,
        user: Result<TokenUser, TokenError>,
        env: Option<&'static str>,
        token: bool,
        summaries: Result<(), ClaudeError>,
        tailscale: Result<tailscale::Status, String>,
        port_use: Result<tailscale::Use, String>,
        vms: Result<Vec<crate::boat::Sandbox>, String>,
        funnel: Result<tailscale::Funnel, String>,
        reachable: Result<(), String>,
    }

    const KEY: &str = "boat_7qiLX-3secretmlrf";

    fn key_row(actions: &[&str]) -> Value {
        json!({"apiKeys": [
            {"keyPrefix": "boat_other", "keyLastFour": "zzzz", "scope": {"actions": []}},
            {"keyPrefix": "boat_7qiLX-3", "keyLastFour": "mlrf", "expired": false, "expiringSoon": false, "expiresAt": "2027-10-30T09:40:20.906Z",
             "scope": {"actions": actions, "sandboxes": "*", "environments": "*", "grandfathered": false}}
        ]})
    }

    fn good() -> Fake {
        Fake {
            key: Ok(Some((KEY.into(), Source::Keychain("flotilla")))),
            me: Ok(()),
            keys: Ok(key_row(BOAT_ACTIONS)),
            gh: GhStatus::SignedIn {
                login: "acme".into(),
                version: "2.81.0".into(),
                scopes: vec!["repo".into()],
            },
            user: Ok(TokenUser {
                login: "acme".into(),
                scopes: Some(vec!["gist".into(), "read:org".into(), "repo".into()]),
            }),
            env: None,
            token: true,
            summaries: Ok(()),
            tailscale: Ok(tailscale::Status {
                running: true,
                name: Some("laptop.tail1.ts.net".into()),
                https: true,
                id: Some("nLap1".into()),
                funnel: Some(vec![443, 8443, 10000]),
            }),
            port_use: Ok(tailscale::Use::Ours),
            vms: Ok(vec![]),
            funnel: Ok(tailscale::Funnel {
                ours: Some((8443, true)),
                free: vec![10000],
                taken: vec![],
            }),
            reachable: Ok(()),
        }
    }

    impl Probe for Fake {
        fn push(&self, _: Option<&Store>) -> crate::push::Status {
            crate::push::Status::Ready {
                key_id: "ABC123DEFG".into(),
                team_id: "DEF123GHIJ".into(),
            }
        }

        fn boat_key(&self) -> Result<Option<(String, Source)>, String> {
            self.key.clone()
        }
        fn boat_me(&self, _: &str) -> Result<(), Refusal> {
            self.me.clone()
        }
        fn boat_keys(&self, _: &str) -> Result<Value, Refusal> {
            self.keys.clone()
        }
        fn gh(&self) -> GhStatus {
            self.gh.clone()
        }
        fn github_token(&self) -> Result<String, String> {
            Ok("gho_x".into())
        }
        fn github_user(&self, _: &str) -> Result<TokenUser, TokenError> {
            self.user.clone()
        }
        fn github_env(&self) -> Option<&'static str> {
            self.env
        }
        fn claude_token(&self) -> Result<bool, String> {
            Ok(self.token)
        }
        fn summaries(&self, _: &str) -> Result<(), ClaudeError> {
            self.summaries.clone()
        }
        fn tailscale(&self) -> Result<tailscale::Status, String> {
            self.tailscale.clone()
        }
        fn port_use(&self, port: u16) -> Result<tailscale::Use, String> {
            // Only ever asked about the port it serves on.
            assert_eq!(port, 7777);
            self.port_use.clone()
        }
        fn vms(&self, _: &str) -> Result<Vec<crate::boat::Sandbox>, String> {
            self.vms.clone()
        }
        fn funnel(&self, port: u16) -> Result<tailscale::Funnel, String> {
            assert_eq!(port, 8787);
            self.funnel.clone()
        }
        fn reachable(&self, url: &str) -> Result<(), String> {
            assert_eq!(url, "https://box.example.com");
            self.reachable.clone()
        }
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("flotilla-doctor-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A server's checks, with Tailscale set up and serving on 7777.
    fn check(probe: &Fake) -> (Doctor, PathBuf) {
        let dir = scratch();
        let db = dir.join("flotilla.db");
        Store::open(&db)
            .unwrap()
            .kv_set(tailscale::KEY, "on")
            .unwrap();
        (
            run(
                probe,
                &At {
                    db,
                    tailscale: true,
                    serving: Some(7777),
                    mcp: None,
                    app: false,
                    gate: Default::default(),
                },
            ),
            dir,
        )
    }

    fn one(probe: &Fake, of: CheckOf) -> Diagnosis {
        let (d, dir) = check(probe);
        std::fs::remove_dir_all(dir).unwrap();
        d.checks.into_iter().find(|c| c.check == of).unwrap()
    }

    /// Remote MCP's check, as it's set up, while the server serves (or not).
    fn mcp_check(p: &Fake, s: Option<crate::remote_mcp::Settings>, serving: bool) -> Diagnosis {
        let at = At {
            db: PathBuf::new(),
            tailscale: true,
            serving: serving.then_some(7777),
            mcp: serving.then_some(8787),
            app: false,
            gate: Default::default(),
        };
        remote_mcp(p, s.as_ref(), &at)
    }

    /// Off unless it's turned on, and then off is information (no fix, not counted); on, it
    /// says whether it's public and, when it isn't, what to change.
    #[test]
    fn remote_mcp_is_optional_and_says_whether_its_public() {
        use crate::remote_mcp::Settings;
        let p = good();
        let off = mcp_check(&p, None, true);
        assert_eq!(
            (off.health, off.says.as_str(), off.fix.len()),
            (Health::Off, "Off", 0)
        );
        let funnel = Settings {
            funnel: true,
            ..Settings::on(8787)
        };
        assert_eq!(
            mcp_check(&p, Some(funnel.clone()), true).says,
            "Public at https://laptop.tail1.ts.net:8443/mcp"
        );
        assert_eq!(
            mcp_check(&p, Some(funnel.clone()), false).says,
            "Ready to publish with Tailscale Funnel"
        );
        // Funnel's port forwarding on the tailnet only, or not at all.
        let mut q = good();
        q.funnel = Ok(tailscale::Funnel {
            ours: Some((8443, false)),
            free: vec![10000],
            taken: vec![],
        });
        let c = mcp_check(&q, Some(funnel.clone()), true);
        assert_eq!(
            (c.health, c.fix.as_slice()),
            (
                Health::Fail,
                ["tailscale funnel --bg --https=8443 http://127.0.0.1:8787".to_string()].as_slice()
            )
        );
        q.funnel = Ok(tailscale::Funnel {
            ours: None,
            free: vec![10000],
            taken: vec![(8443, "/ → http://127.0.0.1:3773".into())],
        });
        let c = mcp_check(&q, Some(funnel.clone()), true);
        assert_eq!(
            (c.says.as_str(), c.fix.as_slice()),
            (
                "Not public: Tailscale Funnel isn't publishing it",
                ["tailscale funnel --bg --https=10000 http://127.0.0.1:8787".to_string()]
                    .as_slice()
            )
        );
        // The tailnet's policy doesn't allow Funnel here: what to change in the admin console.
        q.tailscale = Ok(tailscale::Status {
            funnel: None,
            ..good().tailscale.unwrap()
        });
        let c = mcp_check(&q, Some(funnel.clone()), true);
        assert_eq!(c.health, Health::Fail);
        assert!(
            c.says
                .contains("doesn't let this machine use Tailscale Funnel"),
            "{}",
            c.says
        );
        assert!(
            c.fix[0].contains("https://login.tailscale.com/f/funnel?node=nLap1")
                && c.fix[1].contains("nodeAttrs"),
            "{:?}",
            c.fix
        );
        // No HTTPS on the tailnet: Tailscale's own fix.
        q.tailscale = Ok(tailscale::Status {
            https: false,
            ..good().tailscale.unwrap()
        });
        assert_eq!(
            mcp_check(&q, Some(funnel), true).fix,
            ["Turn on HTTPS: https://login.tailscale.com/admin/dns"]
        );
        // Published elsewhere: checked at its address.
        let url = Settings {
            url: Some("https://box.example.com".into()),
            ..Settings::on(8787)
        };
        assert_eq!(
            mcp_check(&p, Some(url.clone()), true).says,
            "Public at https://box.example.com/mcp"
        );
        let mut q = good();
        q.reachable = Err("it answered 502 Bad Gateway".into());
        let c = mcp_check(&q, Some(url), true);
        assert_eq!(
            (c.health, c.says.as_str()),
            (
                Health::Fail,
                "https://box.example.com/mcp doesn't reach it (it answered 502 Bad Gateway): publish http://127.0.0.1:8787/mcp there"
            )
        );
        // On this machine only.
        assert_eq!(
            mcp_check(&p, Some(Settings::on(8787)), true).says,
            "On at http://127.0.0.1:8787/mcp, on this machine only"
        );
        // Turned on after the server started: restart it.
        let at = At {
            db: PathBuf::new(),
            tailscale: true,
            serving: Some(7777),
            mcp: None,
            app: false,
            gate: Default::default(),
        };
        let c = remote_mcp(&p, Some(&Settings::on(8787)), &at);
        assert_eq!(
            (c.health, c.fix.as_slice()),
            (
                Health::Fail,
                ["flotilla service restart".to_string()].as_slice()
            )
        );
    }

    #[test]
    fn all_green() {
        let (d, dir) = check(&good());
        assert!(d.ok(), "{:#?}", lines(&d));
        // Remote MCP is off, as it is unless it's turned on: said, and not counted.
        assert!(
            d.checks
                .iter()
                .all(|c| (c.health == Health::Ok || c.check == CheckOf::Mcp) && c.fix.is_empty()),
            "{:#?}",
            lines(&d)
        );
        assert_eq!(
            d.checks.iter().map(|c| c.check).collect::<Vec<_>>(),
            [
                CheckOf::BoatKey,
                CheckOf::GitHub,
                CheckOf::Claude,
                CheckOf::Tailscale,
                CheckOf::Database,
                CheckOf::Push,
                CheckOf::Mcp
            ]
        );
        assert_eq!(lines(&d).last().unwrap(), "- Remote MCP: Off");
        assert_eq!(d.tally(), (6, 6));
        assert_eq!(d.checks[0].says, "Works · ends …mlrf · from the Keychain");
        assert_eq!(d.checks[5].says, "Own key ABC123DEFG · team DEF123GHIJ");
        assert_eq!(d.checks[1].says, "Signed in as acme (gh 2.81.0)");
        assert_eq!(
            d.checks[2].says,
            "Work with claude-haiku-5-5 · the setup-token token"
        );
        assert_eq!(d.checks[3].says, "Serving https://laptop.tail1.ts.net");
        // Never the key itself.
        assert!(!lines(&d).join("\n").contains(KEY));
        // The app on This Mac: no Tailscale, and its own fixes (the key goes in Settings).
        let mut p = good();
        p.key = Ok(None);
        p.summaries = Err(ClaudeError::Failed("Please run /login".into()));
        let d = run(
            &p,
            &At {
                db: dir.join("flotilla.db"),
                tailscale: false,
                serving: None,
                mcp: None,
                app: true,
                gate: Default::default(),
            },
        );
        assert!(
            !d.checks
                .iter()
                .any(|c| c.check == CheckOf::Tailscale || c.check == CheckOf::Mcp)
        );
        assert_eq!(d.checks[0].fix, [boat_key_command()]);
        assert_eq!(d.checks[2].fix, ["claude auth login"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn boat_key_failures_say_what_to_run() {
        let fixes = vec![boat_key_command(), SETUP.to_string()];
        let mut p = good();
        p.key = Ok(None);
        let c = one(&p, CheckOf::BoatKey);
        assert_eq!(
            (c.health, c.says.as_str(), &c.fix),
            (Health::Fail, "No boat API key", &fixes)
        );

        let mut p = good();
        p.key = Ok(Some(("boat_bogus".into(), Source::Env("BOAT_API_KEY"))));
        p.me = Err(Refusal::Refused("unauthorized".into()));
        let c = one(&p, CheckOf::BoatKey);
        assert_eq!(
            (c.health, c.says.as_str(), &c.fix),
            (
                Health::Fail,
                "boat.dev refused the key from BOAT_API_KEY (unauthorized)",
                &fixes
            )
        );

        let mut p = good();
        p.key = Err("Other users can read /k: run `chmod 600 /k`".into());
        let c = one(&p, CheckOf::BoatKey);
        assert_eq!(
            (c.health, c.says.contains("chmod 600")),
            (Health::Fail, true)
        );

        let mut p = good();
        p.keys = Ok(key_row(&["sandbox.create", "sandbox.read", "agent.prompt"]));
        let c = one(&p, CheckOf::BoatKey);
        assert_eq!(c.health, Health::Fail);
        assert!(
            c.says
                .starts_with("The key from the Keychain can't do sandbox.update, sandbox.stop"),
            "{}",
            c.says
        );
        assert_eq!(c.fix, fixes);

        let mut p = good();
        let mut keys = key_row(BOAT_ACTIONS);
        keys["apiKeys"][1]["expired"] = json!(true);
        p.keys = Ok(keys);
        assert_eq!(
            one(&p, CheckOf::BoatKey).says,
            "The key from the Keychain expired on 2027-10-30"
        );

        let mut p = good();
        let mut keys = key_row(BOAT_ACTIONS);
        keys["apiKeys"][1]["scope"]["sandboxes"] = json!(["bx_1"]);
        p.keys = Ok(keys);
        assert_eq!(
            one(&p, CheckOf::BoatKey).says,
            "The key from the Keychain only reaches some VMs"
        );

        // A key that may do anything passes.
        let mut p = good();
        p.keys = Ok(key_row(&["*"]));
        assert_eq!(one(&p, CheckOf::BoatKey).health, Health::Ok);
    }

    /// A key ending soon is flagged with when it ends, so "!" says why: the time where it's
    /// read for today and tomorrow, the day after that.
    #[test]
    fn a_key_ending_soon_says_when() {
        use chrono::TimeZone as _;
        let est = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let utc =
            |d: u32, h: u32, m: u32| chrono::Utc.with_ymd_and_hms(2026, 11, d, h, m, 0).unwrap();
        let now = utc(1, 11, 25);
        assert_eq!(expires_in(&est, utc(1, 13, 25), now), "today at 08:25");
        assert_eq!(expires_in(&est, utc(2, 4, 0), now), "today at 23:00");
        assert_eq!(expires_in(&est, utc(2, 6, 0), now), "tomorrow at 01:00");
        // 02:00 UTC on the 3rd is still the 2nd here.
        assert_eq!(expires_in(&est, utc(3, 2, 0), now), "tomorrow at 21:00");
        assert_eq!(
            expires_in(&est, utc(6, 15, 0), now),
            "in 5 days, on 2026-11-06"
        );

        let mut p = good();
        let mut keys = key_row(BOAT_ACTIONS);
        keys["apiKeys"][1]["expiringSoon"] = json!(true);
        keys["apiKeys"][1]["expiresAt"] =
            json!((chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339());
        p.keys = Ok(keys);
        let c = one(&p, CheckOf::BoatKey);
        assert_eq!(c.health, Health::Warn);
        assert!(
            c.says.contains(" · expires today at ") || c.says.contains(" · expires tomorrow at "),
            "{}",
            c.says
        );
    }

    /// No false alarms: what couldn't be asked warns, never fails.
    #[test]
    fn unknowns_warn() {
        let mut p = good();
        p.me = Err(Refusal::Other("Couldn't reach boat: timed out".into()));
        assert_eq!(one(&p, CheckOf::BoatKey).health, Health::Warn);
        let mut p = good();
        p.keys = Err(Refusal::Refused("api_key_action_forbidden".into()));
        assert_eq!(one(&p, CheckOf::BoatKey).health, Health::Warn);
        let mut p = good();
        p.keys = Ok(json!({"apiKeys": []}));
        assert_eq!(one(&p, CheckOf::BoatKey).health, Health::Warn);
        let mut p = good();
        p.user = Err(TokenError::Other("can't reach GitHub".into()));
        assert_eq!(one(&p, CheckOf::GitHub).health, Health::Warn);
        let mut p = good();
        p.port_use = Err("tailscale serve status failed".into());
        assert_eq!(one(&p, CheckOf::Tailscale).health, Health::Warn);
    }

    #[test]
    fn github_failures_say_what_to_run() {
        let mut p = good();
        p.gh = GhStatus::NotFound;
        let c = one(&p, CheckOf::GitHub);
        assert_eq!(
            (c.health, c.says.as_str()),
            (Health::Fail, "gh isn't installed")
        );
        assert_eq!(
            c.fix,
            [
                "Install gh: https://cli.github.com",
                "gh auth login --hostname github.com"
            ]
        );

        let mut p = good();
        p.gh = GhStatus::NotSignedIn {
            version: "2.81.0".into(),
        };
        assert_eq!(
            one(&p, CheckOf::GitHub).fix,
            ["gh auth login --hostname github.com"]
        );

        let mut p = good();
        p.user = Ok(TokenUser {
            login: "acme".into(),
            scopes: Some(vec!["gist".into()]),
        });
        let c = one(&p, CheckOf::GitHub);
        assert_eq!(
            (c.health, c.says.as_str()),
            (
                Health::Fail,
                "acme's token lacks the repo scope (gh 2.81.0)"
            )
        );
        assert_eq!(
            c.fix,
            ["gh auth refresh --hostname github.com --scopes repo"]
        );

        let mut p = good();
        p.user = Err(TokenError::Refused);
        assert_eq!(one(&p, CheckOf::GitHub).says, "GitHub refused gh's sign-in");

        // GH_TOKEN with no gh: a fine-grained token lists no scopes, and passes.
        let mut p = good();
        p.env = Some("GH_TOKEN");
        p.gh = GhStatus::SignedIn {
            login: "acme".into(),
            version: "GH_TOKEN".into(),
            scopes: vec![],
        };
        p.user = Ok(TokenUser {
            login: "acme".into(),
            scopes: None,
        });
        let c = one(&p, CheckOf::GitHub);
        assert_eq!(
            (c.health, c.says.as_str()),
            (
                Health::Ok,
                "Signed in as acme (GH_TOKEN, a fine-grained token)"
            )
        );
        p.user = Err(TokenError::Refused);
        assert_eq!(one(&p, CheckOf::GitHub).says, "GitHub refused GH_TOKEN");
        // With GH_TOKEN, an old gh beside it doesn't matter, and no network only warns.
        p.gh = GhStatus::TooOld {
            version: "2.40.0".into(),
        };
        p.user = Ok(TokenUser {
            login: "acme".into(),
            scopes: Some(vec!["repo".into()]),
        });
        assert_eq!(
            one(&p, CheckOf::GitHub).says,
            "Signed in as acme (GH_TOKEN)"
        );
        p.user = Err(TokenError::Other("can't reach GitHub".into()));
        assert_eq!(one(&p, CheckOf::GitHub).health, Health::Warn);
    }

    #[test]
    fn claude_failures_say_what_to_run() {
        let mut p = good();
        p.summaries = Err(ClaudeError::NotFound);
        let c = one(&p, CheckOf::Claude);
        assert_eq!(
            (c.health, c.says.as_str()),
            (Health::Fail, "The claude CLI isn't installed")
        );
        assert_eq!(c.fix.last().map(String::as_str), Some("flotilla setup"));

        let mut p = good();
        p.token = false;
        p.summaries = Err(ClaudeError::Failed(
            "Invalid API key · Please run /login".into(),
        ));
        let c = one(&p, CheckOf::Claude);
        assert_eq!(
            c.says,
            "Fail with claude-haiku-5-5 (claude's own sign-in): Invalid API key · Please run /login"
        );
        assert_eq!(c.fix, ["flotilla setup"]);

        // What claude printed is shown to every client: a token in it isn't.
        let mut p = good();
        p.summaries = Err(ClaudeError::Failed(
            "bad token sk-ant-oat01-abcdefghijkl".into(),
        ));
        let c = one(&p, CheckOf::Claude);
        assert!(
            !c.says.contains("abcdefghijkl") && c.says.contains("sk-ant-…"),
            "{}",
            c.says
        );
        let mut p = good();
        p.summaries = Err(ClaudeError::Failed("`claude` took longer than 30s".into()));
        assert_eq!(one(&p, CheckOf::Claude).health, Health::Warn);

        // Turned off in Settings: nothing to fix.
        let dir = scratch();
        let db = dir.join("flotilla.db");
        Store::open(&db)
            .unwrap()
            .kv_set(crate::summarize::MODEL_KEY, "")
            .unwrap();
        let mut p = good();
        p.summaries = Err(ClaudeError::NotFound);
        let d = run(
            &p,
            &At {
                db,
                tailscale: false,
                serving: None,
                mcp: None,
                app: false,
                gate: Default::default(),
            },
        );
        assert_eq!((d.checks[2].health, d.ok()), (Health::Off, true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tailscale_failures_say_what_to_run() {
        let mut p = good();
        p.port_use = Ok(tailscale::Use::Free);
        let c = one(&p, CheckOf::Tailscale);
        assert_eq!(
            (c.health, c.says.as_str()),
            (
                Health::Fail,
                "Tailscale isn't forwarding https://laptop.tail1.ts.net to Flotilla"
            )
        );
        assert_eq!(
            c.fix,
            ["tailscale serve --bg --https=443 http://127.0.0.1:7777"]
        );
        // 443 is someone else's: said, as starting the server says it, with nothing to run.
        // Making Flotilla's forward would replace theirs, and removing it isn't Flotilla's to
        // offer.
        p.port_use = Ok(tailscale::Use::Taken("/ → http://127.0.0.1:8080".into()));
        let c = one(&p, CheckOf::Tailscale);
        assert_eq!(
            (c.health, c.says.as_str()),
            (
                Health::Fail,
                "Tailscale already serves https://laptop.tail1.ts.net (/ → http://127.0.0.1:8080), so Flotilla leaves it alone"
            )
        );
        assert!(c.fix.is_empty(), "{:?}", c.fix);

        let mut p = good();
        p.tailscale = Ok(tailscale::Status {
            running: false,
            name: None,
            https: false,
            id: None,
            funnel: None,
        });
        assert_eq!(one(&p, CheckOf::Tailscale).fix, ["tailscale up"]);
        let mut p = good();
        p.tailscale = Ok(tailscale::Status {
            running: true,
            name: Some("laptop.tail1.ts.net".into()),
            https: false,
            id: None,
            funnel: None,
        });
        assert_eq!(one(&p, CheckOf::Tailscale).health, Health::Fail);
        let mut p = good();
        p.tailscale =
            Err("Couldn't run tailscale (No such file or directory): is it installed?".into());
        assert_eq!(
            one(&p, CheckOf::Tailscale).fix.last().map(String::as_str),
            Some("tailscale up")
        );
        // Ready gives out the name only with an Ok, from the one status it read (setup's).
        let ready = |s: tailscale::Status| tailscale_ready(Ok(s));
        let (d, name) = ready(tailscale::Status {
            running: true,
            name: Some("laptop.tail1.ts.net".into()),
            https: true,
            id: None,
            funnel: None,
        });
        assert_eq!(
            (d.health, d.says.as_str(), name.as_deref()),
            (
                Health::Ok,
                "Ready to serve https://laptop.tail1.ts.net",
                Some("laptop.tail1.ts.net")
            )
        );
        let (d, name) = ready(tailscale::Status {
            running: false,
            name: Some("laptop.tail1.ts.net".into()),
            https: true,
            id: None,
            funnel: None,
        });
        assert_eq!((d.health, name), (Health::Fail, None));
        let (d, name) = ready(tailscale::Status {
            running: true,
            name: Some("laptop.tail1.ts.net".into()),
            https: false,
            id: None,
            funnel: None,
        });
        assert_eq!((d.health, name), (Health::Fail, None));

        // Not set up: off, with how to. Set up but the server isn't running: ready.
        let dir = scratch();
        let db = dir.join("flotilla.db");
        let d = run(
            &good(),
            &At {
                db: db.clone(),
                tailscale: true,
                serving: None,
                mcp: None,
                app: false,
                gate: Default::default(),
            },
        );
        assert_eq!(
            (d.checks[3].health, d.checks[3].fix.as_slice()),
            (
                Health::Off,
                ["flotilla setup --tailscale-serve".to_string()].as_slice()
            )
        );
        Store::open(&db)
            .unwrap()
            .kv_set(tailscale::KEY, "on")
            .unwrap();
        let mut p = good();
        p.port_use = Err("not asked without a port".into());
        let d = run(
            &p,
            &At {
                db,
                tailscale: true,
                serving: None,
                mcp: None,
                app: false,
                gate: Default::default(),
            },
        );
        assert_eq!(
            (d.checks[3].health, d.checks[3].says.as_str()),
            (Health::Ok, "Ready to serve https://laptop.tail1.ts.net")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Another Flotilla's VMs on the account are said, as information: never a failure.
    #[test]
    fn other_flotillas_vms_are_information() {
        let vm = |labels: Value| {
            serde_json::from_value::<crate::boat::Sandbox>(json!({"id": "bx_1", "labels": labels}))
                .unwrap()
        };
        let dir = scratch();
        let db = dir.join("flotilla.db");
        let mine = crate::owner::server_id(&Store::open(&db).unwrap()).unwrap();
        let at = At {
            db,
            tailscale: false,
            serving: None,
            mcp: None,
            app: false,
            gate: Default::default(),
        };
        let mut p = good();
        p.vms = Ok(vec![vm(json!({"flotilla": "1", "flotilla.server": mine}))]);
        assert!(
            !run(&p, &at)
                .checks
                .iter()
                .any(|c| c.check == CheckOf::Others)
        );
        p.vms = Ok(vec![vm(
            json!({"flotilla": "1", "flotilla.server": "srv_box", "flotilla.host": "box"}),
        )]);
        let d = run(&p, &at);
        let c = d
            .checks
            .iter()
            .find(|c| c.check == CheckOf::Others)
            .unwrap();
        assert_eq!(
            (c.health, c.says.as_str()),
            (
                Health::Ok,
                "1 VM on this boat account belongs to Flotilla on box (srv_box). This one leaves them alone"
            )
        );
        assert!(d.ok());
        // Can't list them (or the key doesn't work): no row; the key's check says why.
        p.vms = Err("timed out".into());
        assert!(
            !run(&p, &at)
                .checks
                .iter()
                .any(|c| c.check == CheckOf::Others)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_database_that_cant_be_written() {
        let dir = scratch();
        // Its folder is a file.
        std::fs::write(dir.join("not-a-dir"), "x").unwrap();
        let db = dir.join("not-a-dir/flotilla.db");
        let d = run(
            &good(),
            &At {
                db: db.clone(),
                tailscale: false,
                serving: None,
                mcp: None,
                app: false,
                gate: Default::default(),
            },
        );
        let c = d
            .checks
            .iter()
            .find(|c| c.check == CheckOf::Database)
            .unwrap();
        assert_eq!((c.check, c.health), (CheckOf::Database, Health::Fail));
        assert!(
            c.says.starts_with(&format!("Can't write {}", db.display())),
            "{}",
            c.says
        );
        assert!(!d.ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn push_off_is_information() {
        let off = push(crate::push::Status::Off);
        assert_eq!(
            (off.check, off.health, off.says.as_str()),
            (CheckOf::Push, Health::Off, "Off")
        );
        assert!(off.fix.is_empty());
        let d = Doctor {
            checks: vec![off],
            at_ms: 0,
        };
        assert!(d.ok());
        assert_eq!(lines(&d)[0], "- Push: Off");
        // Through the relay, or with its own key.
        let relay = push(crate::push::Status::Relay {
            url: crate::push::DEFAULT_RELAY.into(),
        });
        assert_eq!(relay.health, Health::Ok);
        assert_eq!(relay.says, "Through the relay at push.useflotilla.com");
        let local = push(crate::push::Status::Relay {
            url: "http://127.0.0.1:8787".into(),
        });
        assert_eq!(local.says, "Through the relay at 127.0.0.1:8787");
        let own = push(crate::push::Status::Ready {
            key_id: "ABC123DEFG".into(),
            team_id: "DEF123GHIJ".into(),
        });
        assert_eq!(
            (own.health, own.says.as_str()),
            (Health::Ok, "Own key ABC123DEFG · team DEF123GHIJ")
        );
        let broken = push(crate::push::Status::Broken(
            "There's an APNs key but no team id".into(),
        ));
        assert_eq!(broken.health, Health::Fail);
        assert_eq!(broken.fix, [crate::push::SETUP_COMMAND]);
        let relay = push(crate::push::Status::BadRelay("not one".into()));
        assert_eq!(relay.health, Health::Fail);
        assert!(relay.fix.is_empty());
        // The app on This Mac pairs no phones: no push check there.
        let dir = scratch();
        let d = run(
            &good(),
            &At {
                db: dir.join("flotilla.db"),
                tailscale: false,
                serving: None,
                mcp: None,
                app: true,
                gate: Default::default(),
            },
        );
        assert!(!d.checks.iter().any(|c| c.check == CheckOf::Push));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
