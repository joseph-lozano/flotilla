//! `flotilla setup` and `flotilla doctor`, run on the machine that serves Flotilla. Clients
//! never enter secrets: the boat key, GitHub's sign-in and the summarizer's Claude token are set
//! up here, and every client's Settings shows what `doctor` finds, with what to run here.
//!
//! `setup` walks through the boat key (pasted, or `--boat-key-file`, or `BOAT_API_KEY`; checked,
//! then kept), `gh auth login`, `claude setup-token` (`--claude-token-file`, or
//! `CLAUDE_CODE_OAUTH_TOKEN`), Tailscale Serve (`--tailscale-serve`), notifications to phones
//! (`--apns-key`, or the push relay: `--push-relay`, `--no-push-relay`), offers remote MCP (off
//! unless it's asked for: `--mcp`, `--mcp-funnel`), then prints a pairing link. Without a terminal, or with `--non-interactive`, it asks nothing: what it isn't given
//! it checks as it is, and says what to run.

use flotilla_core::doctor::{self, Diagnosis, Health, System};
use flotilla_core::secrets::{self, Name, Source};
use flotilla_core::store::Store;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Options<'a> {
    pub paths: &'a crate::Paths,
    pub boat_key_file: Option<PathBuf>,
    pub claude_token_file: Option<PathBuf>,
    /// `--tailscale-serve` (true) or `--no-tailscale-serve` (false).
    pub tailscale: Option<bool>,
    pub skip_github: bool,
    pub skip_claude: bool,
    pub non_interactive: bool,
    pub port: u16,
    pub ttl: std::time::Duration,
    /// `--apns-key`, `--apns-key-id`, `--apns-team-id`.
    pub apns: Option<Apns>,
    /// `--push-relay <URL>` (Some(Some)) or `--no-push-relay` (Some(None)).
    pub push_relay: Option<Option<String>>,
    pub mcp: McpFlags,
}

/// What `setup` was told about remote MCP.
#[derive(Debug, Default, Clone)]
pub struct McpFlags {
    /// `--mcp`.
    pub on: bool,
    /// `--no-mcp`.
    pub off: bool,
    pub port: Option<u16>,
    pub host: Option<String>,
    /// `--mcp-funnel`.
    pub funnel: bool,
    pub url: Option<String>,
}

impl McpFlags {
    /// Whether anything said to turn it on.
    fn said_on(&self) -> bool {
        self.on || self.port.is_some() || self.host.is_some() || self.funnel || self.url.is_some()
    }
}

/// An APNs key to keep, for notifications to phones.
pub struct Apns {
    pub key: PathBuf,
    pub key_id: Option<String>,
    pub team_id: String,
}

/// `flotilla setup`.
pub fn setup(o: Options) -> Result<(), String> {
    let ask = Asker {
        on: !o.non_interactive && std::io::stdin().is_terminal(),
    };
    let path = &o.paths.db;
    println!("Setting up Flotilla in {}", o.paths.dir.display());
    let store = Store::open(path).map_err(|e| format!("Couldn't open {}: {e}", path.display()))?;
    let db = doctor::database(path);
    if db.health == Health::Fail {
        return Err(db.says);
    }

    println!();
    println!("1. boat API key");
    boat_key(&o, &ask)?;

    println!();
    println!("2. GitHub");
    if o.skip_github {
        println!("   Skipped.");
    } else {
        github(&ask);
    }

    println!();
    println!("3. Titles and previews (claude)");
    let model = flotilla_core::summarize::configured_model(&store).unwrap_or_default();
    if o.skip_claude {
        println!("   Skipped.");
    } else {
        claude(&o, &ask, &model);
    }

    println!();
    println!("4. Tailscale");
    let name = tailscale(&o, &ask, &store);

    println!();
    println!("5. Notifications to phones");
    push(&o, &store)?;

    println!();
    println!("6. Remote MCP (optional)");
    remote_mcp(&o, &ask, &store)?;

    // Pairing links give this out until `flotilla serve` says where it is.
    let url = match &name {
        Some(name) => format!("https://{name}"),
        None => store
            .kv_get(flotilla_engine::host::SERVER_URL_KEY)
            .ok()
            .flatten()
            .filter(|u| !u.starts_with("https://"))
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", o.port)),
    };
    store
        .kv_set(flotilla_engine::host::SERVER_URL_KEY, &url)
        .map_err(|e| e.to_string())?;

    println!();
    println!("Checking…");
    let d = doctor::run(
        &System,
        &doctor::At {
            db: path.clone(),
            tailscale: true,
            serving: None,
            mcp: None,
            app: false,
            gate: Default::default(),
        },
    );
    for line in doctor::lines(&d) {
        println!("  {line}");
    }

    println!();
    let running = flotilla_core::lock::holder(path);
    let serve = start_command(o.paths, cfg!(target_os = "linux"));
    match running {
        Some(h) if h.kind == "app" => println!(
            "The app is open on this database (pid {}): quit it, then start the server: {serve}",
            h.pid
        ),
        Some(h) => println!(
            "A server is running here (pid {}): restart it to use what changed (`flotilla service restart`, if it's the service).",
            h.pid
        ),
        None => println!("Start the server: {serve}"),
    }
    let code = store
        .mint_code(o.ttl, "setup", flotilla_core::util::now_ms())
        .map_err(|e| e.to_string())?;
    println!();
    crate::print_pairing(path, &url, &code);
    if d.checks
        .iter()
        .any(|c| c.check == doctor::CheckOf::BoatKey && c.health == Health::Fail)
    {
        return Err("The boat key doesn't work yet".into());
    }
    Ok(())
}

/// How to start the server on this data. On Linux it runs best as a service, which starts at
/// boot; a service keeps its database as flotilla.db in a data directory, so a database named
/// with `--db` can only be served.
fn start_command(paths: &crate::Paths, linux: bool) -> String {
    if linux && paths.given != crate::Given::Db {
        format!(
            "{} (or {})",
            paths.command("service install"),
            paths.command("serve")
        )
    } else {
        paths.command("serve")
    }
}

/// The boat key: given now (a file, or `BOAT_API_KEY`), else the one kept, else pasted. It's
/// checked before it's kept.
fn boat_key(o: &Options, ask: &Asker) -> Result<(), String> {
    let s = secrets::store();
    let given = match &o.boat_key_file {
        Some(f) => Some((
            secrets::read_file(f, Name::BoatKey).map_err(|e| e.to_string())?,
            Source::File(f.clone()),
        )),
        None => std::env::var(Name::BoatKey.env())
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .map(|k| (k, Source::Env(Name::BoatKey.env()))),
    };
    let kept = s.kept(Name::BoatKey).map_err(|e| e.to_string())?;
    let mut candidate = match (given, kept) {
        (Some(g), _) => Some(g),
        (None, Some((k, source))) => {
            let ends: String = k
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if !ask.on
                || ask.yes(
                    &format!("   A key ending …{ends} is kept in {source}. Keep it?"),
                    true,
                )
            {
                Some((k, source))
            } else {
                None
            }
        }
        (None, None) => None,
    };
    for _ in 0..3 {
        let (key, source) = match candidate.take() {
            Some(c) => c,
            None if ask.on => {
                println!("   Make a key with everything Flotilla uses:");
                println!("     {}", doctor::boat_key_command());
                let key = ask.secret("   Paste it here (it isn't shown): ")?;
                if key.is_empty() {
                    continue;
                }
                (key, Source::Pasted)
            }
            None => {
                return Err(format!(
                    "No boat API key: pass {} (a file only you can read), or set {}",
                    Name::BoatKey.flag(),
                    Name::BoatKey.env()
                ));
            }
        };
        let d = doctor::boat_key(&System, &key, &source);
        say(&d);
        if d.health == Health::Fail {
            if ask.on && !matches!(source, Source::File(_)) {
                continue;
            }
            return Err("The boat key doesn't work".into());
        }
        if matches!(source, Source::Keychain(_) | Source::Dir(_)) {
            return Ok(());
        }
        let at = s
            .set(Name::BoatKey, &key)
            .map_err(|e| format!("Couldn't keep the key: {e}"))?;
        println!("   Kept in {at}.");
        return Ok(());
    }
    Err("The boat key doesn't work".into())
}

/// The APNs key, if one's given: checked, then kept with the secrets, its ids in the database.
/// The relay to send through without one, or none, if that's given. Otherwise what's there.
fn push(o: &Options, store: &Store) -> Result<(), String> {
    if let Some(relay) = &o.push_relay {
        let kept = match relay {
            Some(url) => flotilla_core::push::relay_url(url)?,
            None => "off".to_string(),
        };
        store
            .kv_set(flotilla_core::push::RELAY_KEY, &kept)
            .map_err(|e| e.to_string())?;
    }
    if let Some(a) = &o.apns {
        let text = std::fs::read_to_string(&a.key)
            .map_err(|e| format!("Couldn't read {}: {e}", a.key.display()))?;
        let key_id = a
            .key_id
            .clone()
            .or_else(|| flotilla_core::push::key_id_of_file(&a.key))
            .ok_or("Say the key's id: --apns-key-id")?;
        let kept = flotilla_core::push::keepable(&text)?;
        // Checked whole before anything is kept.
        flotilla_core::push::Config::new(&kept, &key_id, &a.team_id)?;
        let at = secrets::store()
            .set(Name::ApnsKey, &kept)
            .map_err(|e| format!("Couldn't keep the APNs key: {e}"))?;
        store
            .kv_set(flotilla_core::push::KEY_ID_KEY, key_id.trim())
            .map_err(|e| e.to_string())?;
        store
            .kv_set(flotilla_core::push::TEAM_ID_KEY, a.team_id.trim())
            .map_err(|e| e.to_string())?;
        println!("   Kept in {at}. You can delete {} now.", a.key.display());
    }
    say(&doctor::push(flotilla_core::push::status(Some(store))));
    Ok(())
}

/// gh signed in with the scope Flotilla needs: `gh auth login` (or refresh) run here if not.
fn github(ask: &Asker) {
    let mut d = doctor::github(&System);
    say(&d);
    // gh's own sign-in can be fixed from here; the rest is up to you.
    let fixable = |d: &Diagnosis| {
        d.health == Health::Fail && d.fix.first().is_some_and(|f| f.starts_with("gh auth "))
    };
    if ask.on && fixable(&d) && ask.yes("   Run it now?", true) {
        let fix = d.fix[0].clone();
        if run_here(&fix) {
            d = doctor::github(&System);
            say(&d);
        }
    }
}

/// Summaries: a token given now (checked, then kept), else what's there; `claude setup-token`
/// run here if that doesn't work.
fn claude(o: &Options, ask: &Asker, model: &str) {
    if model.is_empty() {
        println!("   Off in Settings: nothing to set up.");
        return;
    }
    let given = match &o.claude_token_file {
        Some(f) => match secrets::read_file(f, Name::ClaudeToken) {
            Ok(t) => Some(t),
            Err(e) => {
                println!("   ✗ {e}");
                return;
            }
        },
        None => std::env::var(Name::ClaudeToken.env())
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
    };
    if let Some(token) = given {
        keep_claude_token(&token, model);
        return;
    }
    let d = doctor::claude(&System, model);
    say(&d);
    if d.health != Health::Fail || !ask.on || flotilla_core::summarize::find_claude().is_none() {
        return;
    }
    if !ask.yes("   Make a token with `claude setup-token` now?", true) {
        return;
    }
    if !run_here("claude setup-token") {
        return;
    }
    match ask.secret("   Paste the token it printed (it isn't shown): ") {
        Ok(t) if !t.is_empty() => keep_claude_token(&t, model),
        _ => println!("   Nothing pasted: summaries stay as they are."),
    }
}

fn keep_claude_token(token: &str, model: &str) {
    match flotilla_core::summarize::Summarizer::with_token(model, Some(token.to_string()))
        .and_then(|s| s.test())
    {
        Ok(()) => match secrets::set_claude_token(token) {
            Ok(()) => println!(
                "   ✓ Summaries work with {model}. The token is kept in {}.",
                secrets::store().kept_in(Name::ClaudeToken)
            ),
            Err(e) => println!("   ✗ Couldn't keep the token: {e}"),
        },
        Err(e) => println!("   ✗ Summaries fail with that token: {e}"),
    }
}

/// Whether to forward the tailnet's HTTPS to the server: this machine's tailnet name when it
/// does.
fn tailscale(o: &Options, ask: &Asker, store: &Store) -> Option<String> {
    let status = flotilla_core::tailscale::status();
    let on = match o.tailscale {
        Some(on) => on,
        None if store
            .kv_get(flotilla_core::tailscale::KEY)
            .ok()
            .flatten()
            .as_deref()
            == Some("on") =>
        {
            true
        }
        None => match &status {
            Ok(s) if ask.on && s.running && s.name.is_some() => ask.yes(
                &format!(
                    "   Reach the server from your tailnet at https://{}?",
                    s.name.as_deref().unwrap_or_default()
                ),
                false,
            ),
            Ok(_) => false,
            Err(_) => {
                println!("   Not installed: skipped.");
                return None;
            }
        },
    };
    let _ = store.kv_set(flotilla_core::tailscale::KEY, if on { "on" } else { "off" });
    if !on {
        println!("   Off: the server answers on this machine only.");
        return None;
    }
    // The checks' own verdict, as `flotilla doctor` gives it, and the name from what it read:
    // read now, after any question.
    let (d, name) = doctor::tailscale_ready(flotilla_core::tailscale::status());
    say(&d);
    name
}

/// Remote MCP as it's to be, from the flags and what's kept: off unless something turns it on
/// (a flag now, or kept from before). The flags change only what they say.
fn mcp_wanted(
    f: &McpFlags,
    kept: Option<flotilla_core::remote_mcp::Settings>,
) -> Result<Option<flotilla_core::remote_mcp::Settings>, String> {
    use flotilla_core::remote_mcp::{DEFAULT_PORT, Settings, check_url};
    if f.off {
        return Ok(None);
    }
    if !f.said_on() {
        return Ok(kept);
    }
    let mut s = kept.unwrap_or_else(|| Settings::on(DEFAULT_PORT));
    if let Some(p) = f.port {
        s.port = p;
    }
    if let Some(h) = &f.host {
        h.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .map_err(|_| format!("--mcp-host {h} isn't an IP address"))?;
        s.host = Some(h.clone()).filter(|h| h != "127.0.0.1");
    }
    if f.funnel {
        (s.funnel, s.url) = (true, None);
    }
    if let Some(u) = &f.url {
        (s.funnel, s.url) = (false, Some(check_url(u)?));
    }
    Ok(Some(s))
}

/// Remote MCP: off unless asked for. On a terminal with nothing said, it's offered (No unless
/// you say yes), and then how to publish it: Tailscale Funnel when this machine can, else the
/// address something else publishes it at, or none (this machine only).
fn remote_mcp(o: &Options, ask: &Asker, store: &Store) -> Result<(), String> {
    use flotilla_core::remote_mcp::{self, DEFAULT_PORT, Settings};
    let kept = remote_mcp::load(store);
    let mut wanted = mcp_wanted(&o.mcp, kept.clone())?;
    if wanted.is_none()
        && !o.mcp.off
        && ask.on
        && ask.yes(
            "   Let assistants in other clouds (Grok, Muse) reach this server's MCP over HTTPS?",
            false,
        )
    {
        let mut s = Settings::on(DEFAULT_PORT);
        match flotilla_core::tailscale::status() {
            Ok(t) if t.running && t.name.is_some() && t.funnel.is_some() => s.funnel = ask.yes(
                "   Publish it with Tailscale Funnel (only it: the rest stays on your tailnet)?",
                true,
            ),
            _ => {}
        }
        if !s.funnel {
            let url = ask.line("   Its public address, if a tunnel or proxy publishes it (https://…), or Enter for this machine only: ")?;
            if !url.is_empty() {
                s.url = Some(remote_mcp::check_url(&url)?);
            }
        }
        wanted = Some(s);
    }
    remote_mcp::save(store, wanted.as_ref()).map_err(|e| e.to_string())?;
    match &wanted {
        None => println!("   Off."),
        Some(s) => {
            let at = doctor::At {
                db: o.paths.db.clone(),
                tailscale: true,
                serving: None,
                mcp: None,
                app: false,
                gate: Default::default(),
            };
            say(&doctor::remote_mcp(&System, Some(s), &at));
            if kept.as_ref() != Some(s) {
                println!(
                    "       Agent devices only: `{}` makes one, or an assistant signs in with a pairing code.",
                    o.paths.command("pair --agent --token")
                );
            }
        }
    }
    Ok(())
}

/// `flotilla doctor`: a running server's checks (it knows where it serves; the app on This
/// Mac runs one too, with its own fixes), else this machine's as they are.
pub fn doctor(db: &Path) -> Result<bool, String> {
    let server = flotilla_core::lock::holder(db).and_then(|h| h.address);
    let d = match server {
        Some(address) => crate::remote::doctor(&address)?,
        None => doctor::run(
            &System,
            &doctor::At {
                db: db.to_path_buf(),
                tailscale: true,
                serving: None,
                mcp: None,
                app: false,
                gate: Default::default(),
            },
        ),
    };
    for line in doctor::lines(&d) {
        println!("{line}");
    }
    Ok(d.ok())
}

fn say(d: &Diagnosis) {
    let mark = match d.health {
        Health::Ok => "✓",
        Health::Warn => "!",
        Health::Fail => "✗",
        Health::Off => "-",
    };
    println!("   {mark} {}", d.says);
    for f in &d.fix {
        println!("       {f}");
    }
}

/// Run a command line here, on this terminal; whether it worked.
fn run_here(line: &str) -> bool {
    let parts: Vec<&str> = line.split_whitespace().collect();
    let Some((program, args)) = parts.split_first() else {
        return false;
    };
    println!("   $ {line}");
    match Command::new(program).args(args).status() {
        Ok(s) => s.success(),
        Err(e) => {
            println!("   ✗ Couldn't run {program}: {e}");
            false
        }
    }
}

/// Questions on the terminal, when there is one.
struct Asker {
    on: bool,
}

impl Asker {
    fn yes(&self, question: &str, default: bool) -> bool {
        print!("{question} {} ", if default { "[Y/n]" } else { "[y/N]" });
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() {
            return default;
        }
        match line.trim().to_lowercase().as_str() {
            "" => default,
            a => a.starts_with('y'),
        }
    }

    /// A line typed, as it's typed.
    fn line(&self, prompt: &str) -> Result<String, String> {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        Ok(line.trim().to_string())
    }

    /// A line typed without it showing.
    fn secret(&self, prompt: &str) -> Result<String, String> {
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let quiet = Command::new("stty")
            .arg("-echo")
            .stdin(std::process::Stdio::inherit())
            .status()
            .is_ok_and(|s| s.success());
        let mut line = String::new();
        let read = std::io::stdin().lock().read_line(&mut line);
        if quiet {
            let _ = Command::new("stty")
                .arg("echo")
                .stdin(std::process::Stdio::inherit())
                .status();
            println!();
        }
        read.map_err(|e| e.to_string())?;
        Ok(line.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Remote MCP stays as it is (off, for a server that never turned it on) unless a flag
    /// says otherwise; each flag changes only what it says.
    #[test]
    fn remote_mcp_is_off_unless_asked_for() {
        use flotilla_core::remote_mcp::Settings;
        let none = McpFlags::default();
        assert_eq!(mcp_wanted(&none, None).unwrap(), None);
        let kept = Settings {
            funnel: true,
            ..Settings::on(9000)
        };
        assert_eq!(
            mcp_wanted(&none, Some(kept.clone())).unwrap(),
            Some(kept.clone())
        );
        assert_eq!(
            mcp_wanted(
                &McpFlags {
                    off: true,
                    ..McpFlags::default()
                },
                Some(kept.clone())
            )
            .unwrap(),
            None
        );
        assert_eq!(
            mcp_wanted(
                &McpFlags {
                    on: true,
                    ..McpFlags::default()
                },
                None
            )
            .unwrap(),
            Some(Settings::on(8787))
        );
        let funnel = mcp_wanted(
            &McpFlags {
                funnel: true,
                ..McpFlags::default()
            },
            None,
        )
        .unwrap()
        .unwrap();
        assert!(funnel.funnel && funnel.url.is_none() && funnel.port == 8787);
        // A port changes the port, and keeps how it's published.
        assert_eq!(
            mcp_wanted(
                &McpFlags {
                    port: Some(8790),
                    ..McpFlags::default()
                },
                Some(kept.clone())
            )
            .unwrap(),
            Some(Settings {
                port: 8790,
                ..kept.clone()
            })
        );
        // An address published elsewhere takes Funnel's place.
        let url = mcp_wanted(
            &McpFlags {
                url: Some("https://mcp.example.com/".into()),
                ..McpFlags::default()
            },
            Some(kept),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (url.funnel, url.url.as_deref()),
            (false, Some("https://mcp.example.com"))
        );
        assert!(
            mcp_wanted(
                &McpFlags {
                    url: Some("http://mcp.example.com".into()),
                    ..McpFlags::default()
                },
                None
            )
            .is_err()
        );
        assert!(
            mcp_wanted(
                &McpFlags {
                    host: Some("box".into()),
                    ..McpFlags::default()
                },
                None
            )
            .is_err()
        );
        assert_eq!(
            mcp_wanted(
                &McpFlags {
                    host: Some("0.0.0.0".into()),
                    ..McpFlags::default()
                },
                None
            )
            .unwrap()
            .unwrap()
            .host(),
            "0.0.0.0"
        );
    }

    /// The command setup ends with names the data as it was named, and offers the service only
    /// where it could run on it: a database named by `--db` isn't a data directory's.
    #[test]
    fn the_start_command_fits_how_the_data_was_named() {
        let paths = |data_dir: Option<&str>, db: Option<&str>| {
            crate::Paths::resolve(data_dir.map(PathBuf::from), db.map(PathBuf::from), None).unwrap()
        };
        assert_eq!(
            start_command(&paths(Some("/srv/fl"), None), true),
            "flotilla service install --data-dir /srv/fl (or flotilla serve --data-dir /srv/fl)"
        );
        assert_eq!(
            start_command(&paths(Some("/srv/fl"), None), false),
            "flotilla serve --data-dir /srv/fl"
        );
        assert_eq!(
            start_command(&paths(None, Some("/srv/fl/custom.db")), true),
            "flotilla serve --db /srv/fl/custom.db"
        );
        assert_eq!(
            start_command(&paths(None, None), true),
            "flotilla service install (or flotilla serve)"
        );
        // Secrets kept elsewhere: the server is told where.
        let elsewhere =
            crate::Paths::resolve(Some("/srv/fl".into()), None, Some("/srv/private".into()))
                .unwrap();
        assert_eq!(
            start_command(&elsewhere, true),
            "flotilla service install --data-dir /srv/fl --secrets-dir /srv/private (or flotilla serve --data-dir /srv/fl --secrets-dir /srv/private)"
        );
    }
}
