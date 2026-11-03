//! Long-running services (dev servers, watchers) a repo declares for its threads' VMs, a copy
//! of Amp's `.amp/services.yaml` (ampcode.com/docs/orbs/portals). Never started from
//! `.agents/setup` or `.agents/resume`.
//!
//! - **The file:** `.agents/services.yml`, or `.amp/services.yaml` when that's all a repo has,
//!   so repos set up for Amp work as they are. A non-empty `services` map of name → `command`
//!   (run through a login shell; a service with a preview listens on `$PORT`), `cwd`, `port`,
//!   `env` (`${services.<name>.publicURL}` is another service's preview URL), `health` (an
//!   HTTP path answering 2xx/3xx; else the port accepting a connection means ready), `portal`
//!   and `portals` (the preview links) and `platforms` (a service without `linux` is skipped).
//!   `review` is ignored.
//! - **Ensure** (Amp's word): once a project thread's VM is up, after its first prompt and
//!   after every wake, Flotilla starts what isn't running, as boat commands it can follow and
//!   stop, and waits for each to be ready. boat's stop keeps the VM's memory, so services
//!   pause with it and carry on after a wake (seen live); ensure then finds them running and
//!   only starts the ones that died. `~/.cache/flotilla/services.json` in the VM tells the
//!   agent where they are.
//!
//! Each thread's services are kept in `kv` as `services:<thread>`; the ports Flotilla chose,
//! which stay the same for the thread, as `service_ports:<thread>`.

use crate::boat::{BoatClient, CommandStatus};
use crate::error::{Error, Result};
use crate::setup::sh_quote;
use crate::store::{Store, Thread, ThreadState};
use crate::util::now_iso;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub const FILE: &str = ".agents/services.yml";
pub const AMP_FILE: &str = ".amp/services.yaml";
/// How long a starting service gets to answer before it's "not responding".
pub const READY_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// Ports Flotilla picks for services that don't name one start here.
const FIRST_PORT: u16 = 4100;
/// How much of a service's output to keep.
const OUTPUT_CHARS: usize = 4000;
/// Set by Flotilla for every service; a service's `env` can't set them.
const RESERVED: &[&str] = &[
    "PORT",
    "PUBLIC_URL",
    "FLOTILLA_THREAD_ID",
    "FLOTILLA_BRANCH",
    "FLOTILLA",
];
const LOG_DIR: &str = "~/.cache/flotilla/logs";
const STATE_FILE: &str = "~/.cache/flotilla/services.json";

// ---- the file ----

#[derive(Deserialize)]
struct RawFile {
    services: Option<BTreeMap<String, RawService>>,
}

#[derive(Deserialize)]
struct RawService {
    command: Option<String>,
    cwd: Option<String>,
    port: Option<i64>,
    #[serde(default)]
    env: BTreeMap<String, Scalar>,
    health: Option<String>,
    portal: Option<RawPortal>,
    #[serde(default)]
    portals: Vec<RawLink>,
    platforms: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Scalar {
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Scalar {
    fn text(&self) -> String {
        match self {
            Scalar::Text(s) => s.clone(),
            Scalar::Int(i) => i.to_string(),
            Scalar::Float(f) => f.to_string(),
            Scalar::Bool(b) => b.to_string(),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawPortal {
    On(bool),
    Link {
        url: Option<String>,
        title: Option<String>,
    },
}

/// A `portals` entry: a link, or a folder of links (flattened here).
#[derive(Deserialize)]
struct RawLink {
    url: Option<String>,
    title: Option<String>,
    #[serde(default)]
    links: Vec<RawLink>,
}

pub use flotilla_proto::{Link, ServiceRun, ThreadServices};

/// One service, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub command: String,
    pub cwd: Option<String>,
    pub port: Option<u16>,
    /// As written; `${services.<name>.publicURL}` is filled in at start.
    pub env: Vec<(String, String)>,
    pub health: Option<String>,
    /// The primary link first.
    pub links: Vec<Link>,
    /// Services whose preview URL its `env` uses: they start first.
    pub needs: Vec<String>,
}

impl Service {
    pub fn has_preview(&self) -> bool {
        !self.links.is_empty()
    }
}

/// A checked services file: the services that run on Linux, in start order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub file: String,
    pub services: Vec<Service>,
}

fn valid_name(n: &str) -> bool {
    let mut chars = n.chars();
    n.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_env_name(n: &str) -> bool {
    let mut chars = n.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The names in `${services.<name>.publicURL}` references, in order.
fn references(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(i) = rest.find("${services.") {
        rest = &rest[i + "${services.".len()..];
        if let Some(end) = rest.find(".publicURL}") {
            let name = &rest[..end];
            if !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn link_ok(url: &str) -> bool {
    url.starts_with('/') || url.starts_with("http://") || url.starts_with("https://")
}

fn flatten(
    links: Vec<RawLink>,
    service: &str,
    out: &mut Vec<Link>,
) -> std::result::Result<(), String> {
    for l in links {
        match l.url {
            Some(url) => {
                if !link_ok(&url) {
                    return Err(format!(
                        "service {service}: a portal url must be a path starting with / or an http(s) URL, not {url:?}"
                    ));
                }
                out.push(Link {
                    title: l.title.unwrap_or_else(|| service.to_string()),
                    url,
                });
            }
            None if !l.links.is_empty() => flatten(l.links, service, out)?,
            None => {
                return Err(format!(
                    "service {service}: each entry in portals needs a url"
                ));
            }
        }
    }
    Ok(())
}

/// Read and check a services file. `file` is its path, for messages.
pub fn parse(file: &str, text: &str) -> std::result::Result<Spec, String> {
    let raw: RawFile = serde_saphyr::from_str(text).map_err(|e| format!("{file}: {e}"))?;
    let services = raw
        .services
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{file}: needs a non-empty top-level `services` map"))?;
    let mut checked = Vec::new();
    let mut declared_ports: BTreeMap<u16, String> = BTreeMap::new();
    for (name, s) in services {
        let at = |what: &str| format!("{file}: service {name}: {what}");
        if !valid_name(&name) {
            return Err(format!(
                "{file}: service name {name:?} must be lowercase letters, digits and hyphens, start with a letter or digit, and be at most 32 characters"
            ));
        }
        if let Some(platforms) = &s.platforms {
            if platforms.is_empty() {
                return Err(at(
                    "platforms can't be empty (leave it out to run everywhere)",
                ));
            }
            if let Some(p) = platforms
                .iter()
                .find(|p| !matches!(p.as_str(), "linux" | "darwin"))
            {
                return Err(at(&format!("unknown platform {p:?} (linux or darwin)")));
            }
            // boat's VMs are Linux.
            if !platforms.iter().any(|p| p == "linux") {
                continue;
            }
        }
        let command = s
            .command
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .ok_or_else(|| at("needs a command"))?;
        let cwd = match s
            .cwd
            .map(|c| c.trim().trim_end_matches('/').to_string())
            .filter(|c| !c.is_empty() && c != ".")
        {
            Some(c) if c.starts_with('/') || c.split('/').any(|part| part == "..") => {
                return Err(at("cwd must be a folder inside the repo"));
            }
            c => c,
        };
        let port = match s.port {
            None => None,
            Some(p) if (1..=65535).contains(&p) => Some(p as u16),
            Some(p) => return Err(at(&format!("port {p} isn't between 1 and 65535"))),
        };
        if let Some(p) = port
            && let Some(other) = declared_ports.insert(p, name.clone())
        {
            return Err(format!(
                "{file}: services {other} and {name} both use port {p}"
            ));
        }
        let mut env = Vec::new();
        for (k, v) in &s.env {
            if !valid_env_name(k) {
                return Err(at(&format!(
                    "{k:?} isn't a valid environment variable name"
                )));
            }
            if RESERVED.contains(&k.as_str()) {
                return Err(at(&format!(
                    "Flotilla sets {k} itself; it can't be set in env"
                )));
            }
            env.push((k.clone(), v.text()));
        }
        let health = match s.health.map(|h| h.trim().to_string()) {
            Some(h)
                if !h.starts_with('/')
                    || h.chars().any(|c| {
                        c.is_whitespace() || matches!(c, '\'' | '"' | '\\' | '`' | '$')
                    }) =>
            {
                return Err(at("health must be a URL path starting with /"));
            }
            h => h,
        };
        let mut links = Vec::new();
        match s.portal {
            None | Some(RawPortal::On(false)) => {}
            Some(RawPortal::On(true)) => links.push(Link {
                title: name.clone(),
                url: "/".into(),
            }),
            Some(RawPortal::Link { url, title }) => {
                let url = url.unwrap_or_else(|| "/".into());
                if !link_ok(&url) {
                    return Err(at(&format!(
                        "portal url must be a path starting with / or an http(s) URL, not {url:?}"
                    )));
                }
                links.push(Link {
                    title: title.unwrap_or_else(|| name.clone()),
                    url,
                });
            }
        }
        flatten(s.portals, &name, &mut links).map_err(|e| format!("{file}: {e}"))?;
        let needs: Vec<String> = env
            .iter()
            .flat_map(|(_, v)| references(v))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        checked.push(Service {
            name,
            command,
            cwd,
            port,
            env,
            health,
            links,
            needs,
        });
    }
    // Every service may exclude Linux: then there's nothing to run here (Amp allows it).
    // References: to a service there, with a preview; then a start order without cycles.
    for s in &checked {
        for n in &s.needs {
            match checked.iter().find(|o| &o.name == n) {
                None => {
                    return Err(format!(
                        "{file}: service {}: env refers to service {n}, which isn't in the file (or doesn't run on Linux)",
                        s.name
                    ));
                }
                Some(o) if !o.has_preview() => {
                    return Err(format!(
                        "{file}: service {}: env uses {n}'s publicURL, but {n} has no portal",
                        s.name
                    ));
                }
                Some(_) if n == &s.name => {
                    return Err(format!(
                        "{file}: service {} refers to its own publicURL",
                        s.name
                    ));
                }
                _ => {}
            }
        }
    }
    let services = start_order(checked).map_err(|cycle| {
        format!(
            "{file}: services refer to each other's publicURL in a loop: {}",
            cycle.join(", ")
        )
    })?;
    Ok(Spec {
        file: file.to_string(),
        services,
    })
}

/// Services in an order where each comes after the ones it `needs`, keeping the file's
/// (alphabetical) order otherwise. The services left in a cycle, if any.
fn start_order(services: Vec<Service>) -> std::result::Result<Vec<Service>, Vec<String>> {
    let mut left = services;
    let mut out: Vec<Service> = Vec::new();
    while !left.is_empty() {
        let Some(i) = left
            .iter()
            .position(|s| s.needs.iter().all(|n| out.iter().any(|o| &o.name == n)))
        else {
            return Err(left.into_iter().map(|s| s.name).collect());
        };
        out.push(left.remove(i));
    }
    Ok(out)
}

/// Each service's port: its own, or one kept from before for the thread, or the next free one
/// from 4100, so a thread's ports stay the same across restarts and wakes.
pub fn assign_ports(services: &[Service], kept: &BTreeMap<String, u16>) -> BTreeMap<String, u16> {
    let mut out: BTreeMap<String, u16> = services
        .iter()
        .filter_map(|s| s.port.map(|p| (s.name.clone(), p)))
        .collect();
    let mut used: BTreeSet<u16> = out.values().copied().collect();
    for s in services.iter().filter(|s| s.port.is_none()) {
        if let Some(&p) = kept.get(&s.name)
            && used.insert(p)
        {
            out.insert(s.name.clone(), p);
        }
    }
    let mut next = FIRST_PORT;
    let unassigned: Vec<&Service> = services
        .iter()
        .filter(|s| !out.contains_key(&s.name))
        .collect();
    for s in unassigned {
        while used.contains(&next) {
            next += 1;
        }
        used.insert(next);
        out.insert(s.name.clone(), next);
    }
    out
}

/// `https://host` from a preview URL (`https://host/?_token=…`).
pub fn origin(url: &str) -> String {
    let after = url.find("://").map(|i| i + 3).unwrap_or(0);
    let end = url[after..]
        .find(['/', '?', '#'])
        .map(|i| after + i)
        .unwrap_or(url.len());
    url[..end].to_string()
}

/// A link's full URL on a service's preview: a path keeps the preview's sign-in token.
pub fn link_url(preview: &str, link: &Link) -> String {
    if !link.url.starts_with('/') {
        return link.url.clone();
    }
    let token = preview
        .split_once('?')
        .map(|(_, q)| q)
        .filter(|q| !q.is_empty());
    match token {
        Some(q) if link.url.contains('?') => format!("{}{}&{q}", origin(preview), link.url),
        Some(q) => format!("{}{}?{q}", origin(preview), link.url),
        None => format!("{}{}", origin(preview), link.url),
    }
}

/// A service's environment: Flotilla's variables, then its own `env` with the publicURL
/// references filled in from `urls` (preview origins by service).
pub fn service_env(
    s: &Service,
    port: u16,
    urls: &BTreeMap<String, String>,
    thread_id: &str,
    branch: Option<&str>,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("FLOTILLA".to_string(), "1".to_string()),
        ("FLOTILLA_THREAD_ID".to_string(), thread_id.to_string()),
    ];
    if let Some(b) = branch {
        env.push(("FLOTILLA_BRANCH".into(), b.to_string()));
    }
    env.push(("PORT".into(), port.to_string()));
    if let Some(u) = urls.get(&s.name) {
        env.push(("PUBLIC_URL".into(), u.clone()));
    }
    for (k, v) in &s.env {
        let mut v = v.clone();
        for n in references(&v.clone()) {
            v = v.replace(
                &format!("${{services.{n}.publicURL}}"),
                urls.get(&n).map(String::as_str).unwrap_or(""),
            );
        }
        env.push((k.clone(), v));
    }
    env
}

/// The boat command that runs a service: a login shell in its folder with its environment,
/// its output also going to `~/.cache/flotilla/logs/service-<name>.log`. Its exit code is the
/// service's.
pub fn start_command(s: &Service, env: &[(String, String)]) -> String {
    let exports: String = env
        .iter()
        .map(|(k, v)| format!("export {k}={}; ", sh_quote(v)))
        .collect();
    let cd = s
        .cwd
        .as_deref()
        .map(|c| format!("cd {} && ", sh_quote(c)))
        .unwrap_or_default();
    let log = format!("{LOG_DIR}/service-{}.log", s.name);
    let inner = format!(
        "mkdir -p {LOG_DIR} && {cd}{{ {exports}set -o pipefail; {{ {}\n}} 2>&1 | tee {log}; }}",
        s.command
    );
    format!("bash -lc {}", sh_quote(&inner))
}

/// One shell script that asks each `(name, port, health)` whether it's ready, printing
/// `<name> ok` or `<name> no` per line.
pub fn probe_command(checks: &[(String, u16, Option<String>)]) -> String {
    let mut script = String::from(
        "http_ok() { if command -v curl >/dev/null 2>&1; then c=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 \"$1\" 2>/dev/null); \
         else c=$(python3 -c 'import sys,urllib.request as u\ntry: print(u.urlopen(sys.argv[1],timeout=3).status)\nexcept u.HTTPError as e: print(e.code)\nexcept Exception: print(0)' \"$1\" 2>/dev/null); fi; \
         case \"$c\" in 2??|3??) return 0;; esac; return 1; }\n",
    );
    for (name, port, health) in checks {
        let test = match health {
            Some(path) => format!(
                "http_ok {}",
                sh_quote(&format!("http://127.0.0.1:{port}{path}"))
            ),
            None => format!("(exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null"),
        };
        script.push_str(&format!(
            "if {test}; then echo {n}' ok'; else echo {n}' no'; fi\n",
            n = sh_quote(name)
        ));
    }
    format!("bash -c {}", sh_quote(&script))
}

// ---- what's running ----

fn key(thread_id: &str) -> String {
    format!("services:{thread_id}")
}

fn ports_key(thread_id: &str) -> String {
    format!("service_ports:{thread_id}")
}

/// A thread's services as last seen, if its repo declares any. A port made private again
/// after it was public has a new token (`crate::public`): its links take it here.
pub fn state(store: &Store, thread_id: &str) -> Result<Option<ThreadServices>> {
    let Some(mut s) = store
        .kv_get(&key(thread_id))?
        .and_then(|j| serde_json::from_str::<ThreadServices>(&j).ok())
    else {
        return Ok(None);
    };
    for run in s.services.iter_mut().filter(|r| r.preview.is_some()) {
        if let Some(url) = crate::public::private_url(store, thread_id, &s.sandbox, run.port)
            && run.preview.as_deref() != Some(url.as_str())
        {
            retoken(run, &url);
        }
    }
    Ok(Some(s))
}

/// The service's preview is now `preview`: its links on that preview take its token (or lose
/// theirs, if it has none).
fn retoken(run: &mut ServiceRun, preview: &str) {
    let here = origin(preview);
    let token = preview
        .split_once('?')
        .map(|(_, q)| q)
        .filter(|q| !q.is_empty());
    run.preview = Some(preview.to_string());
    for l in run.links.iter_mut().filter(|l| origin(&l.url) == here) {
        let (base, query) = l.url.split_once('?').unwrap_or((l.url.as_str(), ""));
        let kept: Vec<&str> = query
            .split('&')
            .filter(|p| !p.is_empty() && !p.starts_with("_token="))
            .chain(token)
            .collect();
        l.url = if kept.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{}", kept.join("&"))
        };
    }
}

fn save(store: &Store, thread_id: &str, s: Option<&ThreadServices>) -> Result<()> {
    store.kv_set(
        &key(thread_id),
        &s.and_then(|s| serde_json::to_string(s).ok())
            .unwrap_or_default(),
    )
}

fn kept_ports(store: &Store, thread_id: &str) -> BTreeMap<String, u16> {
    store
        .kv_get(&ports_key(thread_id))
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

/// Fold one look at a service's command into `run`. True when it changed what's shown.
fn absorb(run: &mut ServiceRun, st: &CommandStatus) -> bool {
    // New output counts even once the kept tail is full (its length no longer changes).
    let got = !st.stdout.is_empty() || !st.stderr.is_empty();
    let before = run.state.clone();
    run.output.push_str(&st.stdout);
    run.output.push_str(&st.stderr);
    if run.output.len() > OUTPUT_CHARS {
        let cut = (run.output.len() - OUTPUT_CHARS..run.output.len())
            .find(|i| run.output.is_char_boundary(*i))
            .unwrap_or(0);
        run.output = run.output[cut..].to_string();
    }
    run.out_at = st
        .stdout_next_offset
        .unwrap_or(run.out_at + st.stdout.len() as u64);
    run.err_at = st
        .stderr_next_offset
        .unwrap_or(run.err_at + st.stderr.len() as u64);
    if !st.running && run.live() {
        run.state = "exited".into();
        run.exit_code = st.exit_code;
    }
    got || run.state != before
}

/// Whether a run's command is still going on `sandbox` (a failed look counts as not).
fn running(boat: &BoatClient, sandbox: &str, run: &mut ServiceRun) -> bool {
    let Some(pid) = run.pid else { return false };
    match boat.command_status(sandbox, pid, run.out_at, run.err_at) {
        Ok(st) => {
            absorb(run, &st);
            st.running
        }
        Err(_) => false,
    }
}

/// Read the services file in the VM: `None` when the repo has none.
fn read_file(boat: &BoatClient, sandbox: &str, dir: &str) -> Result<Option<(String, String)>> {
    let script = format!(
        "for f in {} {}; do if [ -f \"$f\" ]; then printf '%s\\n' \"$f\"; cat -- \"$f\"; exit 0; fi; done; exit 3",
        sh_quote(FILE),
        sh_quote(AMP_FILE)
    );
    let r = boat.exec(
        sandbox,
        &format!("bash -c {}", sh_quote(&script)),
        Some(dir),
        20,
    )?;
    match r.exit_code {
        Some(0) => {
            let (file, text) = r.stdout.split_once('\n').unwrap_or((r.stdout.as_str(), ""));
            Ok(Some((file.trim().to_string(), text.to_string())))
        }
        Some(3) => Ok(None),
        _ => Err(Error::Invalid(format!(
            "couldn't read the services file: {}",
            crate::setup::tail(&r.stderr, 3).join(" / ")
        ))),
    }
}

/// Tell the agent where the services are: `~/.cache/flotilla/services.json` in the VM. Best
/// effort.
fn write_state_file(boat: &BoatClient, sandbox: &str, s: &ThreadServices) {
    let list: Vec<serde_json::Value> = s
        .services
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "port": r.port,
                "url": r.preview.as_deref().map(origin),
                "state": r.state,
                "log": format!("{LOG_DIR}/service-{}.log", r.name),
            })
        })
        .collect();
    let json = serde_json::json!({ "file": s.file, "services": list }).to_string();
    let script = format!(
        "mkdir -p ~/.cache/flotilla && printf '%s\\n' {} > {STATE_FILE}",
        sh_quote(&json)
    );
    let _ = boat.exec(sandbox, &format!("bash -c {}", sh_quote(&script)), None, 15);
}

/// What an ensure does beyond starting what isn't running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ensure {
    /// Start services you stopped too (otherwise they stay stopped).
    pub include_stopped: bool,
    /// Stop this one first and start it again.
    pub restart: Option<String>,
}

/// Start the thread's services that aren't running and wait (up to 2 min) for them to be
/// ready, as Amp's `amp orb services ensure`. Nothing to do when the repo declares none, the
/// thread isn't in a project, or its VM isn't up. `notify` is called whenever what's shown
/// changes.
pub fn ensure(
    boat: &BoatClient,
    store: &Store,
    thread_id: &str,
    opts: &Ensure,
    notify: &dyn Fn(),
) -> Result<()> {
    let Some(t) = store.thread(thread_id)? else {
        return Ok(());
    };
    let (Some(sandbox), Some(project_id)) = (t.sandbox_id.clone(), t.repo_project_id.clone())
    else {
        return Ok(());
    };
    if !matches!(t.state, ThreadState::Idle | ThreadState::Running) {
        return Ok(());
    }
    let Some(p) = store.project(&project_id)? else {
        return Ok(());
    };
    let mut prev = state(store, thread_id)?
        .filter(|s| s.sandbox == sandbox)
        .unwrap_or_default();
    let Some((file, text)) = read_file(boat, &sandbox, p.dir())? else {
        if state(store, thread_id)?.is_some() {
            save(store, thread_id, None)?;
            notify();
        }
        return Ok(());
    };
    let spec = match parse(&file, &text) {
        Ok(spec) => spec,
        Err(e) => {
            prev.sandbox = sandbox;
            prev.file = Some(file);
            prev.error = Some(e);
            save(store, thread_id, Some(&prev))?;
            notify();
            return Ok(());
        }
    };
    let ports = assign_ports(&spec.services, &kept_ports(store, thread_id));
    store.kv_set(
        &ports_key(thread_id),
        &serde_json::to_string(&ports).unwrap_or_default(),
    )?;

    // Services the file no longer has: stop them.
    for old in prev
        .services
        .iter()
        .filter(|r| !spec.services.iter().any(|s| s.name == r.name))
    {
        if let Some(pid) = old.pid {
            let _ = boat.stop_command(&sandbox, pid);
        }
    }
    let mut runs: Vec<ServiceRun> = Vec::new();
    let mut urls: BTreeMap<String, String> = BTreeMap::new();
    for s in &spec.services {
        let port = ports[&s.name];
        let mut run = prev
            .services
            .iter()
            .find(|r| r.name == s.name && r.port == port)
            .cloned()
            .unwrap_or_else(|| ServiceRun {
                name: s.name.clone(),
                port,
                ..Default::default()
            });
        // A port it moved off: whatever ran there is stopped below with the rest.
        if let Some(old) = prev
            .services
            .iter()
            .find(|r| r.name == s.name && r.port != port)
            && let Some(pid) = old.pid
        {
            let _ = boat.stop_command(&sandbox, pid);
        }
        if s.has_preview() && run.preview.is_none() {
            // Not fatal: the service runs without a preview. Public, if you made it so.
            run.preview = crate::public::host(boat, store, thread_id, &sandbox, port).ok();
        }
        if let Some(preview) = &run.preview {
            urls.insert(s.name.clone(), origin(preview));
            run.links = s
                .links
                .iter()
                .map(|l| Link {
                    title: l.title.clone(),
                    url: link_url(preview, l),
                })
                .collect();
        }
        runs.push(run);
    }
    for (s, run) in spec.services.iter().zip(runs.iter_mut()) {
        let restart = opts.restart.as_deref() == Some(s.name.as_str());
        if restart && let Some(pid) = run.pid.take() {
            let _ = boat.stop_command(&sandbox, pid);
        }
        if !restart && running(boat, &sandbox, run) {
            continue;
        }
        if run.state == "stopped" && !opts.include_stopped && !restart {
            continue;
        }
        let env = service_env(s, run.port, &urls, thread_id, t.branch.as_deref());
        *run = ServiceRun {
            name: run.name.clone(),
            port: run.port,
            preview: run.preview.clone(),
            links: run.links.clone(),
            ..Default::default()
        };
        run.started_at = Some(now_iso());
        match boat.exec_detached(&sandbox, &start_command(s, &env), Some(p.dir())) {
            Ok(pid) => {
                run.pid = Some(pid);
                run.state = "starting".into();
            }
            Err(e) => {
                run.state = "failed".into();
                run.error = Some(e.to_string());
            }
        }
    }
    let mut current = ThreadServices {
        sandbox: sandbox.clone(),
        file: Some(file),
        error: None,
        services: runs,
    };
    save(store, thread_id, Some(&current))?;
    notify();
    write_state_file(boat, &sandbox, &current);
    wait_ready(boat, store, thread_id, &spec, &mut current, notify)?;
    write_state_file(boat, &sandbox, &current);
    Ok(())
}

/// Look at starting (and not-responding) services until each is ready or exits, up to the
/// time limit; those still silent then are "not responding" and left running.
fn wait_ready(
    boat: &BoatClient,
    store: &Store,
    thread_id: &str,
    spec: &Spec,
    current: &mut ThreadServices,
    notify: &dyn Fn(),
) -> Result<()> {
    let sandbox = current.sandbox.clone();
    let started = Instant::now();
    loop {
        let waiting: Vec<usize> = current
            .services
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r.state.as_str(), "starting" | "not_responding"))
            .map(|(i, _)| i)
            .collect();
        if waiting.is_empty() {
            return Ok(());
        }
        let mut changed = false;
        for &i in &waiting {
            let run = &mut current.services[i];
            if let Some(pid) = run.pid
                && let Ok(st) = boat.command_status(&sandbox, pid, run.out_at, run.err_at)
            {
                changed |= absorb(run, &st);
            }
        }
        let checks: Vec<(String, u16, Option<String>)> = waiting
            .iter()
            .map(|&i| &current.services[i])
            .filter(|r| r.live())
            .map(|r| {
                (
                    r.name.clone(),
                    r.port,
                    spec.services
                        .iter()
                        .find(|s| s.name == r.name)
                        .and_then(|s| s.health.clone()),
                )
            })
            .collect();
        if !checks.is_empty()
            && let Ok(r) = boat.exec(&sandbox, &probe_command(&checks), None, 30)
        {
            for line in r.stdout.lines() {
                if let Some(name) = line.strip_suffix(" ok")
                    && let Some(run) = current
                        .services
                        .iter_mut()
                        .find(|s| s.name == name && s.live())
                    && run.state != "ready"
                {
                    run.state = "ready".into();
                    changed = true;
                }
            }
        }
        let timed_out = started.elapsed() >= READY_TIMEOUT;
        if timed_out {
            for r in current
                .services
                .iter_mut()
                .filter(|r| r.state == "starting")
            {
                r.state = "not_responding".into();
                changed = true;
            }
        }
        if changed {
            save(store, thread_id, Some(current))?;
            notify();
        }
        if timed_out {
            return Ok(());
        }
        sleep(Duration::from_millis(1500));
    }
}

/// Stop one service; ensure leaves it stopped until it's restarted.
pub fn stop(boat: &BoatClient, store: &Store, thread_id: &str, name: &str) -> Result<()> {
    let Some(mut s) = state(store, thread_id)? else {
        return Ok(());
    };
    let Some(run) = s.services.iter_mut().find(|r| r.name == name) else {
        return Err(Error::NotFound(format!("service {name}")));
    };
    if let Some(pid) = run.pid.take() {
        boat.stop_command(&s.sandbox, pid)?;
    }
    run.state = "stopped".into();
    save(store, thread_id, Some(&s))?;
    write_state_file(boat, &s.sandbox.clone(), &s);
    Ok(())
}

/// A cheap look at the running services (no readiness probes): catches one that exited.
/// Returns whether anything shown changed.
pub fn check(boat: &BoatClient, store: &Store, thread_id: &str) -> Result<bool> {
    let Some(mut s) = state(store, thread_id)? else {
        return Ok(false);
    };
    let sandbox = s.sandbox.clone();
    let states = |s: &ThreadServices| {
        s.services
            .iter()
            .map(|r| r.state.clone())
            .collect::<Vec<_>>()
    };
    let before = states(&s);
    let mut changed = false;
    for run in s.services.iter_mut().filter(|r| r.live()) {
        let Some(pid) = run.pid else { continue };
        if let Ok(st) = boat.command_status(&sandbox, pid, run.out_at, run.err_at) {
            changed |= absorb(run, &st);
        }
    }
    if changed {
        save(store, thread_id, Some(&s))?;
    }
    // The agent reads services.json: tell it when one stopped (log lines alone don't need it).
    if states(&s) != before {
        write_state_file(boat, &sandbox, &s);
    }
    Ok(changed)
}

/// Whether a thread has services worth a periodic look: some running on its current VM.
pub fn watched(store: &Store, t: &Thread) -> bool {
    matches!(t.state, ThreadState::Idle | ThreadState::Running)
        && state(store, &t.id).ok().flatten().is_some_and(|s| {
            Some(&s.sandbox) == t.sandbox_id.as_ref() && s.services.iter().any(ServiceRun::live)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMP: &str = r#"
services:
  web:
    command: npm run dev -- --port $PORT
    cwd: apps/web
    health: /health
    portal: true
    env:
      API_URL: ${services.api.publicURL}/v1
      DEBUG: 1
  api:
    command: cargo run
    port: 8080
    portal:
      url: /docs
      title: API docs
    portals:
      - folder: More
        links:
          - url: /admin
            title: Admin
  worker:
    command: ./worker
    platforms: [darwin]
    review: false
"#;

    #[test]
    fn amp_files_parse_in_start_order() {
        let spec = parse(".amp/services.yaml", AMP).unwrap();
        let names: Vec<&str> = spec.services.iter().map(|s| s.name.as_str()).collect();
        // worker is macOS-only; web needs api's URL, so api starts first.
        assert_eq!(names, ["api", "web"]);
        let web = &spec.services[1];
        assert_eq!(web.cwd.as_deref(), Some("apps/web"));
        assert_eq!(web.health.as_deref(), Some("/health"));
        assert_eq!(web.needs, ["api"]);
        assert!(web.env.contains(&("DEBUG".into(), "1".into())));
        let api = &spec.services[0];
        assert_eq!(api.port, Some(8080));
        assert_eq!(
            api.links,
            [
                Link {
                    title: "API docs".into(),
                    url: "/docs".into()
                },
                Link {
                    title: "Admin".into(),
                    url: "/admin".into()
                }
            ]
        );
    }

    #[test]
    fn bad_files_say_what_is_wrong() {
        let bad = |yaml: &str| parse(FILE, yaml).unwrap_err();
        assert!(bad("services: {}").contains("non-empty"));
        assert!(bad("nothing: 1").contains("non-empty"));
        assert!(bad("services:\n  Web:\n    command: x").contains("lowercase"));
        assert!(bad("services:\n  web:\n    cwd: .").contains("needs a command"));
        assert!(
            bad("services:\n  web:\n    command: x\n    env:\n      PORT: 1")
                .contains("sets PORT itself")
        );
        assert!(
            bad("services:\n  web:\n    command: x\n    port: 70000")
                .contains("between 1 and 65535")
        );
        assert!(bad("services:\n  a:\n    command: x\n    port: 3000\n  b:\n    command: y\n    port: 3000").contains("both use port 3000"));
        assert!(
            bad("services:\n  web:\n    command: x\n    cwd: ../elsewhere")
                .contains("inside the repo")
        );
        assert!(
            bad("services:\n  web:\n    command: x\n    health: ready").contains("starting with /")
        );
        assert!(
            bad("services:\n  web:\n    command: x\n    env:\n      U: ${services.api.publicURL}")
                .contains("isn't in the file")
        );
        assert!(bad("services:\n  api:\n    command: y\n  web:\n    command: x\n    env:\n      U: ${services.api.publicURL}").contains("has no portal"));
        let cycle = "services:\n  a:\n    command: x\n    portal: true\n    env:\n      U: ${services.b.publicURL}\n  b:\n    command: y\n    portal: true\n    env:\n      U: ${services.a.publicURL}";
        assert!(bad(cycle).contains("in a loop: a, b"));
        assert!(
            bad("services:\n  web:\n    command: x\n    platforms: [windows]")
                .contains("unknown platform")
        );
        assert!(bad("services:\n  web:\n    command: [").starts_with(".agents/services.yml: "));
    }

    #[test]
    fn a_file_with_only_macos_services_runs_nothing_here() {
        let spec = parse(
            FILE,
            "services:\n  app:\n    command: open -a App\n    platforms: [darwin]",
        )
        .unwrap();
        assert!(spec.services.is_empty());
    }

    #[test]
    fn output_past_the_kept_tail_still_counts() {
        let mut run = ServiceRun {
            state: "ready".into(),
            output: "x".repeat(OUTPUT_CHARS),
            out_at: 4000,
            ..Default::default()
        };
        let st: CommandStatus = serde_json::from_value(
            serde_json::json!({"running": true, "stdout": "y", "stdoutNextOffset": 4001}),
        )
        .unwrap();
        assert!(absorb(&mut run, &st));
        assert_eq!(run.out_at, 4001);
        assert!(run.output.ends_with('y') && run.output.len() == OUTPUT_CHARS);
    }

    #[test]
    fn ports_stay_put_and_skip_declared_ones() {
        let svc = |name: &str, port: Option<u16>| Service {
            name: name.into(),
            command: "x".into(),
            cwd: None,
            port,
            env: vec![],
            health: None,
            links: vec![],
            needs: vec![],
        };
        let services = [
            svc("api", Some(4100)),
            svc("web", None),
            svc("worker", None),
        ];
        let first = assign_ports(&services, &BTreeMap::new());
        assert_eq!(
            first,
            BTreeMap::from([
                ("api".into(), 4100),
                ("web".into(), 4101),
                ("worker".into(), 4102)
            ])
        );
        // web keeps its port even when worker is gone and a new one arrives.
        let again = assign_ports(
            &[svc("api", Some(4100)), svc("new", None), svc("web", None)],
            &first,
        );
        assert_eq!(again["web"], 4101);
        assert_eq!(again["new"], 4102);
    }

    #[test]
    fn env_and_links_use_the_previews() {
        let spec = parse(FILE, AMP).unwrap();
        let urls = BTreeMap::from([(
            "api".to_string(),
            "https://p8080.example.boat.dev".to_string(),
        )]);
        let env = service_env(&spec.services[1], 4100, &urls, "t1", Some("feat"));
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("PORT"), Some("4100"));
        assert_eq!(get("FLOTILLA_THREAD_ID"), Some("t1"));
        assert_eq!(get("FLOTILLA_BRANCH"), Some("feat"));
        assert_eq!(get("PUBLIC_URL"), None);
        assert_eq!(get("API_URL"), Some("https://p8080.example.boat.dev/v1"));
        let preview = "https://p8080.example.boat.dev/?_token=abc";
        assert_eq!(origin(preview), "https://p8080.example.boat.dev");
        assert_eq!(
            link_url(
                preview,
                &Link {
                    title: "d".into(),
                    url: "/docs".into()
                }
            ),
            "https://p8080.example.boat.dev/docs?_token=abc"
        );
        assert_eq!(
            link_url(
                preview,
                &Link {
                    title: "d".into(),
                    url: "/q?a=1".into()
                }
            ),
            "https://p8080.example.boat.dev/q?a=1&_token=abc"
        );
        assert_eq!(
            link_url(
                preview,
                &Link {
                    title: "x".into(),
                    url: "https://example.com".into()
                }
            ),
            "https://example.com"
        );
    }

    #[test]
    fn commands_run_in_a_login_shell_with_the_env() {
        let spec = parse(FILE, AMP).unwrap();
        let web = &spec.services[1];
        let cmd = start_command(web, &[("PORT".into(), "4100".into())]);
        assert!(cmd.starts_with("bash -lc '"));
        assert!(cmd.contains("cd '\\''apps/web'\\'' && "));
        assert!(cmd.contains("export PORT='\\''4100'\\''; "));
        assert!(cmd.contains("npm run dev -- --port $PORT"));
        assert!(cmd.contains("tee ~/.cache/flotilla/logs/service-web.log"));
        let probe = probe_command(&[
            ("web".into(), 4100, Some("/health".into())),
            ("api".into(), 8080, None),
        ]);
        assert!(probe.contains("http://127.0.0.1:4100/health"));
        assert!(probe.contains("/dev/tcp/127.0.0.1/8080"));
    }

    #[test]
    fn a_link_made_private_again_takes_its_new_token() {
        let mut run = ServiceRun {
            name: "web".into(),
            port: 4100,
            preview: Some("https://sub-4100.on.boat.dev?_token=old".into()),
            links: vec![
                Link {
                    title: "web".into(),
                    url: "https://sub-4100.on.boat.dev/?_token=old".into(),
                },
                Link {
                    title: "Admin".into(),
                    url: "https://sub-4100.on.boat.dev/q?a=1&_token=old".into(),
                },
                Link {
                    title: "Docs".into(),
                    url: "https://example.com/?_token=theirs".into(),
                },
            ],
            ..Default::default()
        };
        retoken(&mut run, "https://sub-4100.on.boat.dev?_token=new");
        assert_eq!(
            run.preview.as_deref(),
            Some("https://sub-4100.on.boat.dev?_token=new")
        );
        let urls: Vec<&str> = run.links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://sub-4100.on.boat.dev/?_token=new",
                "https://sub-4100.on.boat.dev/q?a=1&_token=new",
                "https://example.com/?_token=theirs"
            ]
        );
        // Public: no token at all.
        retoken(&mut run, "https://sub-4100.on.boat.dev");
        let urls: Vec<&str> = run.links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://sub-4100.on.boat.dev/",
                "https://sub-4100.on.boat.dev/q?a=1",
                "https://example.com/?_token=theirs"
            ]
        );
        // Read back, the kept services take the newest private URL boat gave.
        let store = Store::open_in_memory().unwrap();
        let s = ThreadServices {
            sandbox: "bx_1".into(),
            services: vec![ServiceRun {
                name: "web".into(),
                port: 4100,
                preview: Some("https://sub-4100.on.boat.dev?_token=old".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        save(&store, "t1", Some(&s)).unwrap();
        store
            .kv_set(
                "preview_url:t1:4100",
                r#"{"sandbox":"bx_1","url":"https://sub-4100.on.boat.dev?_token=new"}"#,
            )
            .unwrap();
        assert_eq!(
            state(&store, "t1").unwrap().unwrap().services[0]
                .preview
                .as_deref(),
            Some("https://sub-4100.on.boat.dev?_token=new")
        );
        // Another VM's URL isn't this one's.
        store
            .kv_set(
                "preview_url:t1:4100",
                r#"{"sandbox":"bx_0","url":"https://other-4100.on.boat.dev?_token=x"}"#,
            )
            .unwrap();
        assert_eq!(
            state(&store, "t1").unwrap().unwrap().services[0]
                .preview
                .as_deref(),
            Some("https://sub-4100.on.boat.dev?_token=old")
        );
    }

    #[test]
    fn thread_services_round_trip_through_kv() {
        let store = Store::open_in_memory().unwrap();
        assert!(state(&store, "t1").unwrap().is_none());
        let s = ThreadServices {
            sandbox: "bx_1".into(),
            file: Some(FILE.into()),
            error: None,
            services: vec![ServiceRun {
                name: "web".into(),
                port: 4100,
                pid: Some(7),
                state: "ready".into(),
                ..Default::default()
            }],
        };
        save(&store, "t1", Some(&s)).unwrap();
        assert_eq!(state(&store, "t1").unwrap(), Some(s));
    }
}
