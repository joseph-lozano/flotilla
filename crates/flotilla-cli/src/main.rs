//! `flotilla`: `flotilla serve`, and `setup` and `doctor` for the machine that serves; and
//! commands that run and read threads from here.
//!
//! Secrets (the boat key, the summarizer's Claude token) come from the environment, a file
//! (`--boat-key-file`), systemd's credentials, or where `flotilla setup` keeps them: the
//! Keychain on macOS, `secrets/` in the data directory elsewhere (or `--secrets-dir`). gh and
//! claude are found on PATH.
//!
//! `pair` and `devices` write the database directly, whoever holds it, so they work even when
//! the server doesn't answer (the server notices within half a second). Every other command but
//! `serve` works one of three ways, by who holds the database's lock:
//! - no one: this command does the work itself (holding the lock while it runs, so an app or
//!   server can't start an engine beside it);
//! - a server, or the app on This Mac (which runs one inside itself): the command goes
//!   through its Unix socket as requests and views;
//! - an app that couldn't have its socket, or another command: only `threads` and `show`,
//!   which only read, work.
//!
//! With `--server <url>`, they go to that server instead, as the agent device `flotilla connect`
//! paired: the server refuses what an agent may not do. `flotilla mcp` serves the same to an
//! assistant over MCP (`assistant`).

mod assistant;
mod remote;
mod schedule;
mod service;
mod setup;
mod shown;

use clap::{Parser, Subcommand, ValueEnum};
use flotilla_core::lifecycle::{self, Core, NewThread, Sent, days_left};
use flotilla_core::normalize::Message;
use flotilla_core::store::{Store, Thread, ThreadState};
use flotilla_core::util::{ellipsize, local_when, one_line};
use flotilla_core::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "flotilla",
    about = "Flotilla: coding agents, each in a VM of its own. This serves the apps, and runs threads from the command line",
    version
)]
struct Cli {
    /// Flotilla's data directory: its database (flotilla.db), uploaded files and, off a Mac,
    /// secrets/ [default: where the app keeps its]
    #[arg(long, global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// The database itself, in place of --data-dir (its folder is the data directory)
    /// [default: flotilla.db in the data directory]
    #[arg(long, global = true, value_name = "FILE")]
    db: Option<PathBuf>,
    /// The boat API key is in this file, which only you may read
    #[arg(long, global = true, value_name = "FILE")]
    boat_key_file: Option<PathBuf>,
    /// The token `claude setup-token` made is in this file, which only you may read
    #[arg(long, global = true, value_name = "FILE")]
    claude_token_file: Option<PathBuf>,
    /// The APNs key (Apple's .p8, for notifications to phones) is in this file, which only you
    /// may read
    #[arg(long, global = true, value_name = "FILE")]
    apns_key_file: Option<PathBuf>,
    /// Keep secrets in files here, readable only by you [default: the Keychain on macOS,
    /// secrets/ in the data directory elsewhere]
    #[arg(long, global = true, value_name = "DIR")]
    secrets_dir: Option<PathBuf>,
    /// Use the Flotilla server at this address, as the agent device `flotilla connect` paired
    /// (for the commands that work with threads, and `mcp`)
    #[arg(long, global = true, value_name = "URL")]
    server: Option<String>,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Admin(Admin),
    #[command(flatten)]
    Engine(Cmd),
    #[command(flatten)]
    Assistant(AssistantCmd),
}

/// Flotilla for other agents: an assistant (Claude Code, OpenClaw, Hermes) starts and follows
/// threads through MCP, as an agent device that can't delete, merge or change settings.
#[derive(Subcommand)]
enum AssistantCmd {
    /// Serve MCP on stdin and stdout for an assistant: through the Flotilla server on this
    /// machine, or with --server, the one `flotilla connect` paired this machine with
    Mcp,
    /// Pair this machine with a Flotilla server as an agent device, for `flotilla mcp --server`
    Connect {
        /// The server's address (https://box.tail1234.ts.net), or a pairing link
        address: String,
        /// The pairing code: from `flotilla pair --agent` on the server, or Add device in
        /// Settings [default: asked for, or the link's]
        code: Option<String>,
        /// What Settings → Devices calls it [default: Agent on <this machine>]
        #[arg(long)]
        name: Option<String>,
    },
    /// Sign this machine's agent device out of a server, and forget its token
    Disconnect {
        /// The server's address
        address: String,
    },
}

/// What sets up and serves this machine: these work on the database here, never through a
/// server.
#[derive(Subcommand)]
enum Admin {
    /// Run the engine as a server: paired devices connect over HTTP, and this machine's own
    /// user (this CLI, or the app with FLOTILLA_SERVER=unix:…) over a Unix socket. With no
    /// device paired yet, it prints a pairing link on a terminal (in a log, how to make one).
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// The Unix socket for this machine's user, which needs no pairing [default:
        /// run/server.sock in the data directory]
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Allow a --host other than this machine. Flotilla speaks plain HTTP, so pairing codes
        /// and device tokens cross the network unencrypted
        #[arg(long)]
        insecure: bool,
        /// Reach the server from your tailnet over HTTPS: runs `tailscale serve --bg
        /// --https=443 http://127.0.0.1:<port>`, and pairing links use this machine's ts.net name
        #[arg(long)]
        tailscale_serve: bool,
        /// The address devices reach this server at, for pairing links, when a proxy is in
        /// front (Caddy): https://flotilla.example.com
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Turn on remote MCP, for assistants in other clouds, on this port (kept: the next
        /// start opens it too; `flotilla setup --no-mcp` turns it off). It serves only MCP, to
        /// agent devices, so it alone can be published
        #[arg(long, value_name = "PORT")]
        mcp_port: Option<u16>,
        /// Where remote MCP listens [default: 127.0.0.1]
        #[arg(long, value_name = "IP", requires = "mcp_port")]
        mcp_host: Option<String>,
        /// The public address remote MCP is published at, by a tunnel or proxy in front:
        /// https://mcp.example.com
        #[arg(long, value_name = "URL", requires = "mcp_port")]
        mcp_url: Option<String>,
        /// Dev: no engine (no boat, no GitHub), the database as it is
        #[arg(long, hide = true)]
        offline: bool,
        /// A demo: no boat, GitHub, model or VMs; threads play scripted replies, and one standing
        /// code pairs any device. Every start puts its own --data-dir back to the demo's threads,
        /// keeping the devices paired
        #[arg(long, conflicts_with_all = ["offline", "mcp_port", "tailscale_serve"])]
        demo: bool,
    },
    /// Set up this machine to serve Flotilla: the boat key, gh, claude for summaries and
    /// Tailscale, then a pairing link. Asks on a terminal; otherwise takes what it's given
    /// (--boat-key-file, BOAT_API_KEY, --claude-token-file…) and checks the rest
    Setup {
        /// Reach the server from your tailnet over HTTPS (`flotilla serve` forwards to it)
        #[arg(long, conflicts_with = "no_tailscale_serve")]
        tailscale_serve: bool,
        /// Answer on this machine only
        #[arg(long)]
        no_tailscale_serve: bool,
        #[arg(long)]
        skip_github: bool,
        #[arg(long)]
        skip_claude: bool,
        /// Ask nothing, even on a terminal
        #[arg(long)]
        non_interactive: bool,
        /// The port `flotilla serve` will listen on, for the pairing link
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// How long the pairing code works: 90s, 10m, 2h
        #[arg(long, default_value = "15m")]
        ttl: String,
        /// Send notifications to paired phones with this APNs key (the AuthKey_<KEY ID>.p8
        /// Apple gives you): it's kept with the other secrets
        #[arg(long, value_name = "P8", requires = "apns_team_id")]
        apns_key: Option<PathBuf>,
        /// The key's id [default: from the .p8's name]
        #[arg(long, value_name = "KEY ID")]
        apns_key_id: Option<String>,
        /// Your Apple developer team's id
        #[arg(long, value_name = "TEAM ID")]
        apns_team_id: Option<String>,
        /// Without an APNs key, send notifications through this push relay [default:
        /// https://push.useflotilla.com]: it sees when a phone gets one, never what it says
        #[arg(
            long,
            value_name = "URL",
            num_args = 0..=1,
            default_missing_value = flotilla_core::push::DEFAULT_RELAY,
            conflicts_with = "no_push_relay"
        )]
        push_relay: Option<String>,
        /// Send no notifications through a relay
        #[arg(long)]
        no_push_relay: bool,
        /// Turn on remote MCP, for assistants in other clouds (Grok, Muse): a listener of its
        /// own that serves only MCP to agent devices. Off unless turned on
        #[arg(long, conflicts_with = "no_mcp")]
        mcp: bool,
        /// Turn remote MCP off
        #[arg(long)]
        no_mcp: bool,
        /// Remote MCP's port [default: 8787]
        #[arg(long, value_name = "PORT", conflicts_with = "no_mcp")]
        mcp_port: Option<u16>,
        /// Where remote MCP listens [default: 127.0.0.1]
        #[arg(long, value_name = "IP", conflicts_with = "no_mcp")]
        mcp_host: Option<String>,
        /// Publish remote MCP (only it) to the internet with Tailscale Funnel, on 8443 or 10000
        #[arg(long, conflicts_with_all = ["no_mcp", "mcp_url"])]
        mcp_funnel: bool,
        /// Remote MCP's public address, when something else publishes it (exe.dev, a Cloudflare
        /// Tunnel): https://mcp.example.com
        #[arg(long, value_name = "URL", conflicts_with = "no_mcp")]
        mcp_url: Option<String>,
    },
    /// What isn't set up here, and what to run to fix it (a running server's own checks)
    Doctor,
    /// Make a one-time code (a link and a QR code) that pairs another device with this server
    Pair {
        /// How long the code works: 90s, 10m, 2h [default: 5m; at most 24h]
        #[arg(long)]
        ttl: Option<String>,
        /// A code that pairs only an agent device (`flotilla connect`, for an assistant): it
        /// can read, start and follow threads, not delete, merge or change settings
        #[arg(long, conflicts_with_all = ["list", "revoke"])]
        agent: bool,
        /// With --agent: make the agent device now and print its token, once, for an assistant
        /// that sends it as a header to remote MCP (Authorization: Bearer …)
        #[arg(long, requires = "agent", conflicts_with = "ttl")]
        token: bool,
        /// The device's name, with --token [default: Remote MCP]
        #[arg(long, requires = "token")]
        name: Option<String>,
        /// List the codes not used yet
        #[arg(long, conflicts_with_all = ["ttl", "revoke"])]
        list: bool,
        /// Revoke an unused code, by its id (from --list) or the code itself
        #[arg(long, value_name = "ID")]
        revoke: Option<String>,
    },
    /// The devices paired with this server: `list` (the default), or `revoke <id>`
    Devices {
        #[command(subcommand)]
        cmd: Option<DevicesCmd>,
    },
    /// Run the server as a service: a systemd user unit on Linux that starts at boot (with
    /// lingering on) and comes back if it fails
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
    /// Copy another Flotilla's database (a Mac's flotilla.db, with the blobs/ folder beside it)
    /// into this server's data directory: threads, what you typed, unread, projects and
    /// settings. This server takes over the VMs those threads and projects run on, so connect
    /// the app on the Mac to this server and quit it first; it won't run them again.
    Import {
        /// The database to import
        path: PathBuf,
        /// Put this server's own threads and projects aside (kept as flotilla.db.before-import-…)
        #[arg(long)]
        replace: bool,
    },
}

/// What works with threads: done here, or through the server that has the database.
#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// List boat environments
    Envs,
    /// Show the model catalog
    Models { provider: Option<String> },
    /// Show plan limits
    Limits,
    /// List threads
    Threads,
    /// Full sync from boat
    Sync,
    /// Create a thread, send its first prompt, then follow it
    New {
        /// boat environment name; defaults to the default environment
        #[arg(long = "env", alias = "project")]
        environment: Option<String>,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        fast: bool,
        #[arg(long)]
        title: Option<String>,
        /// Machine type, e.g. small
        #[arg(long = "type")]
        machine_type: Option<String>,
        /// Wait for the sandbox to be ready before the first prompt (boat doesn't need it)
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        no_follow: bool,
        prompt: String,
    },
    /// Steer if a turn is active, otherwise prompt (resuming if stopped), then follow
    Send {
        thread: String,
        text: String,
        #[arg(long)]
        no_follow: bool,
    },
    /// Poll at 1 Hz and print messages until the turn settles
    Follow { thread: String },
    /// Print the thread's messages
    Show {
        thread: String,
        /// Include tool input and result JSON
        #[arg(long)]
        verbose: bool,
    },
    /// Find threads by title, and messages by what was said: every word, in any case
    Search {
        #[arg(required = true, value_parser = clap::builder::NonEmptyStringValueParser::new())]
        query: Vec<String>,
        /// At most this many
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Steer the running turn
    Steer { thread: String, text: String },
    /// Interrupt the running turn
    Interrupt { thread: String },
    /// Stop the thread's VM
    Stop {
        thread: String,
        #[arg(long)]
        no_wait: bool,
    },
    /// Resume the thread's VM
    Resume { thread: String },
    /// Rename the thread
    Rename { thread: String, title: String },
    /// Stop the thread's VM (if up) and sort it to the bottom
    Archive { thread: String },
    /// Bring an archived thread back (its VM resumes on the next send)
    Unarchive { thread: String },
    /// Keep the VM running after turns (2 h on the trial) or not (30 min)
    KeepRunning { thread: String, state: OnOff },
    /// Open a private preview URL for a port in the VM
    Preview { thread: String, port: u16 },
    /// Rename the DB to .bak, then rebuild everything from boat
    Rebuild {
        #[arg(long)]
        yes: bool,
    },
    /// Delete the thread's sandbox and local rows
    Delete {
        thread: String,
        #[arg(long)]
        yes: bool,
    },
    /// Show a project thread's services (`.agents/services.yml`), or: `ensure` (start what
    /// isn't running and wait for it), `restart <name>`, `stop <name>`, `check`
    Services {
        thread: String,
        action: Option<String>,
        name: Option<String>,
    },
    /// Prompts sent later, once or again and again, to a thread or a new thread each run
    #[command(subcommand)]
    Schedule(schedule::ScheduleCmd),
}

#[derive(Subcommand, Clone)]
pub(crate) enum ServiceCmd {
    /// Write the unit (it names where the secrets are, and a PATH that finds gh, claude and
    /// tailscale), enable it and start it. Run it again after changing any of those
    Install {
        /// The port `flotilla serve` listens on (127.0.0.1)
        #[arg(long, default_value_t = 7777)]
        port: u16,
        /// The address devices reach the server at, when a proxy is in front (Caddy)
        #[arg(long, value_name = "URL")]
        url: Option<String>,
    },
    /// Whether it's running, and its latest log lines
    Status,
    Start,
    Stop,
    Restart,
    /// Stop it and remove the unit (the data stays)
    Uninstall,
}

#[derive(Subcommand, Clone)]
pub(crate) enum DevicesCmd {
    /// Every paired device, and whether it's connected (when the server runs)
    List,
    /// Forget a device: its token stops working and its connections close
    Revoke { id: String },
}

/// GitHub through the gh CLI signed in on this Mac, as the app uses it (for `.agents/archive`).
fn github() -> flotilla_core::github::GitHub {
    flotilla_core::github::GitHub::new(std::sync::Arc::new(flotilla_core::github::Gh::new()))
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum OnOff {
    On,
    Off,
}

fn main() -> ExitCode {
    // Debug builds work on Flotilla Dev's database, VMs and environments, as the app does.
    flotilla_core::flavor::init(cfg!(debug_assertions));
    // gh and claude are wherever PATH says (the app, opened from the Dock, looks further).
    flotilla_core::tools::path_only(true);
    let cli = Cli::parse();
    let paths = match Paths::resolve(cli.data_dir, cli.db, cli.secrets_dir) {
        Ok(p) => p,
        Err(e) => return finish(Err(e)),
    };
    let mut secrets = flotilla_core::secrets::SecretStore::system(paths.secrets.clone());
    for (name, file) in [
        (flotilla_core::secrets::Name::BoatKey, &cli.boat_key_file),
        (
            flotilla_core::secrets::Name::ClaudeToken,
            &cli.claude_token_file,
        ),
        (flotilla_core::secrets::Name::ApnsKey, &cli.apns_key_file),
    ] {
        if let Some(f) = file {
            secrets.files.insert(name, f.clone());
        }
    }
    flotilla_core::secrets::configure(secrets);
    let cmd =
        match cli.cmd {
            Command::Admin(_) if cli.server.is_some() => return finish(Err(
                "--server is for the commands that work with threads: run this one on the server"
                    .into(),
            )),
            Command::Admin(a) => return admin(a, &paths, cli.boat_key_file, cli.claude_token_file),
            Command::Assistant(AssistantCmd::Mcp) => {
                return finish(assistant::mcp(&paths, cli.server.as_deref()));
            }
            Command::Assistant(_) if cli.server.is_some() => {
                return finish(Err(
                    "Give the server's address on its own: flotilla connect <address>".into(),
                ));
            }
            Command::Assistant(AssistantCmd::Connect {
                address,
                code,
                name,
            }) => return finish(assistant::connect(&paths, &address, code, name)),
            Command::Assistant(AssistantCmd::Disconnect { address }) => {
                return finish(assistant::disconnect(&paths, &address));
            }
            Command::Engine(cmd) => cmd,
        };
    // Another server, as this machine's agent device there.
    if let Some(server) = &cli.server {
        return finish(
            assistant::door_for(&paths, server)
                .and_then(|(url, token)| remote::run_as(cmd, &url, Some(token))),
        );
    }
    let db = &paths.db;
    // A server (or the app's own) has the database: go through it.
    let holder = flotilla_core::lock::holder(db);
    if let Some(address) = holder.as_ref().and_then(|h| h.address.clone()) {
        return match remote::run(cmd, &address) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    // Something without a socket has it: only reading is safe.
    let lock = match holder {
        Some(h) if !matches!(cmd, Cmd::Threads | Cmd::Show { .. } | Cmd::Search { .. }) => {
            eprintln!(
                "error: {} (the {}, pid {}). Quit it to use this command, or run `{}` and start the app with FLOTILLA_SERVER.",
                flotilla_core::lock::BUSY,
                h.kind,
                h.pid,
                paths.command("serve")
            );
            return ExitCode::FAILURE;
        }
        Some(_) => None,
        None => match flotilla_core::lock::Lock::take(db, &flotilla_core::lock::Holder::new("cli"))
        {
            Ok(lock) => Some(lock),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    // Moved to another Flotilla (`flotilla import`): that one runs its threads now.
    if let Some(moved) = flotilla_core::owner::moved(db)
        && !matches!(cmd, Cmd::Threads | Cmd::Show { .. } | Cmd::Search { .. })
    {
        eprintln!("error: {}", moved.reason());
        return ExitCode::FAILURE;
    }
    // Beside another process, `threads`, `show` and `search` read without changing anything.
    let open: fn(&Path) -> flotilla_core::Result<Store> = if lock.is_some() {
        Store::open
    } else {
        Store::open_read_only
    };
    match run(cmd, &paths, open) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            if (e.is("limit_reached") || e.is("member_limit_reached"))
                && let Ok(store) = Store::open(db)
            {
                let up: Vec<_> = store
                    .threads()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| {
                        matches!(
                            t.state,
                            ThreadState::Idle | ThreadState::Running | ThreadState::Starting
                        )
                    })
                    .collect();
                if !up.is_empty() {
                    eprintln!(
                        "running threads (stop one with `{}`):",
                        paths.command("stop <id>")
                    );
                    for t in up {
                        eprintln!("  {}", thread_line(&t));
                    }
                }
            }
            if let Some(s) = e.boat().and_then(|b| b.retry_after_seconds()) {
                eprintln!("boat says retry after {s}s");
            }
            ExitCode::FAILURE
        }
    }
}

/// `serve`, `setup`, `doctor`, `service`, `import`, `pair` and `devices`.
fn admin(
    a: Admin,
    paths: &Paths,
    boat_key_file: Option<PathBuf>,
    claude_token_file: Option<PathBuf>,
) -> ExitCode {
    match a {
        Admin::Serve {
            host,
            port,
            socket,
            insecure,
            tailscale_serve,
            url,
            mcp_port,
            mcp_host,
            mcp_url,
            offline,
            demo,
        } => serve(
            paths,
            Serving {
                host,
                port,
                socket,
                insecure,
                tailscale: tailscale_serve,
                url,
                mcp: mcp_port.map(|port| (port, mcp_host, mcp_url)),
                offline,
                demo,
            },
        ),
        Admin::Setup {
            tailscale_serve,
            no_tailscale_serve,
            skip_github,
            skip_claude,
            non_interactive,
            port,
            ttl,
            apns_key,
            apns_key_id,
            apns_team_id,
            push_relay,
            no_push_relay,
            mcp,
            no_mcp,
            mcp_port,
            mcp_host,
            mcp_funnel,
            mcp_url,
        } => {
            let ttl = match parse_ttl(&ttl) {
                Ok(t) => t,
                Err(e) => return finish(Err(e)),
            };
            if let Err(e) = paths.make() {
                return finish(Err(e));
            }
            let tailscale = match (tailscale_serve, no_tailscale_serve) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            };
            finish(setup::setup(setup::Options {
                paths,
                boat_key_file,
                claude_token_file,
                tailscale,
                skip_github,
                skip_claude,
                non_interactive,
                port,
                ttl,
                apns: apns_key.map(|key| setup::Apns {
                    key,
                    key_id: apns_key_id,
                    team_id: apns_team_id.unwrap_or_default(),
                }),
                push_relay: match (push_relay, no_push_relay) {
                    (Some(url), _) => Some(Some(url)),
                    (_, true) => Some(None),
                    _ => None,
                },
                mcp: setup::McpFlags {
                    on: mcp,
                    off: no_mcp,
                    port: mcp_port,
                    host: mcp_host,
                    funnel: mcp_funnel,
                    url: mcp_url,
                },
            }))
        }
        Admin::Doctor => match setup::doctor(&paths.db) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => finish(Err(e)),
        },
        Admin::Service { cmd } => {
            if paths.given == Given::Db {
                return finish(Err(
                    "The service keeps its database in a data directory: give --data-dir, not --db"
                        .into(),
                ));
            }
            let dir = absolute(&paths.dir);
            if matches!(cmd, ServiceCmd::Install { .. })
                && let Err(e) = paths.make()
            {
                return finish(Err(e));
            }
            let action = match cmd {
                ServiceCmd::Install { port, url } => service::Action::Install(service::Install {
                    secrets_dir: absolute(
                        &paths.secrets.clone().unwrap_or_else(|| dir.join("secrets")),
                    ),
                    data_dir: dir,
                    boat_key_file: boat_key_file.as_deref().map(absolute),
                    claude_token_file: claude_token_file.as_deref().map(absolute),
                    port,
                    url,
                }),
                ServiceCmd::Status => service::Action::Status,
                ServiceCmd::Start => service::Action::Start,
                ServiceCmd::Stop => service::Action::Stop,
                ServiceCmd::Restart => service::Action::Restart,
                ServiceCmd::Uninstall => service::Action::Uninstall { data_dir: dir },
            };
            finish(service::run(action))
        }
        Admin::Import { path, replace } => {
            finish(paths.make().and_then(|()| import(&path, paths, replace)))
        }
        Admin::Pair {
            agent: true,
            token: true,
            name,
            ..
        } => finish(agent_token(paths, name)),
        Admin::Pair {
            ttl,
            agent,
            list,
            revoke,
            ..
        } => finish(pair(paths, ttl, agent, list, revoke)),
        Admin::Devices { cmd } => finish(devices(paths, cmd.unwrap_or(DevicesCmd::List))),
    }
}

/// Where this Flotilla's data is, from `--data-dir`, `--db` and `--secrets-dir`: worked out
/// once, for every command.
pub(crate) struct Paths {
    /// The data directory: the database, its blobs/ and lock, run/ (the server's socket) and,
    /// off a Mac, secrets/.
    pub dir: PathBuf,
    pub db: PathBuf,
    /// Where secrets are kept in files; None: the Keychain.
    pub secrets: Option<PathBuf>,
    pub given: Given,
    /// `--secrets-dir` was given (else they're where the data is).
    secrets_given: bool,
}

/// Which flag named the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Given {
    Default,
    DataDir,
    Db,
}

impl Paths {
    /// `--db`, else flotilla.db in `--data-dir`, else the app's; its folder is the data
    /// directory. Both at once would name two places, so they're refused.
    fn resolve(
        data_dir: Option<PathBuf>,
        db: Option<PathBuf>,
        secrets_dir: Option<PathBuf>,
    ) -> std::result::Result<Paths, String> {
        let (db, given) = match (db, data_dir) {
            (Some(_), Some(_)) => return Err(
                "Give --data-dir or --db, not both: the database's folder is the data directory"
                    .into(),
            ),
            (Some(db), None) => (db, Given::Db),
            (None, Some(dir)) => (dir.join("flotilla.db"), Given::DataDir),
            (None, None) => (lifecycle::default_db_path(), Given::Default),
        };
        let dir = db
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let secrets_given = secrets_dir.is_some();
        // Kept in files beside the database where there's no Keychain.
        let secrets =
            secrets_dir.or_else(|| (!cfg!(target_os = "macos")).then(|| dir.join("secrets")));
        Ok(Paths {
            dir,
            db,
            secrets,
            given,
            secrets_given,
        })
    }

    /// How a command this prints names the same data: ` --data-dir X` or ` --db X`, and
    /// ` --secrets-dir S` if that was given; or nothing.
    pub fn flag(&self) -> String {
        let mut flag = match self.given {
            Given::Default => String::new(),
            Given::DataDir => format!(" --data-dir {}", path_word(&self.dir)),
            Given::Db => format!(" --db {}", path_word(&self.db)),
        };
        if self.secrets_given
            && let Some(s) = &self.secrets
        {
            flag += &format!(" --secrets-dir {}", path_word(s));
        }
        flag
    }

    /// `flotilla <what>` on this data, for what this prints.
    pub fn command(&self, what: &str) -> String {
        format!("flotilla {what}{}", self.flag())
    }

    /// The data directory, made for its owner alone if it isn't there yet: it holds what was
    /// typed, device tokens' hashes and (off a Mac) the secrets.
    fn make(&self) -> std::result::Result<(), String> {
        if self.dir.exists() {
            return Ok(());
        }
        private_dir(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))
    }
}

/// A path as an option's value: one shell word, and never read as an option itself (`-x` is
/// `./-x`).
fn path_word(p: &Path) -> String {
    let s = p.display().to_string();
    shell_word(&if s.starts_with('-') {
        format!("./{s}")
    } else {
        s
    })
}

/// `s` as one shell word: as it is if that's safe, else in single quotes.
pub(crate) fn shell_word(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:@,%".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Whether this machine's service runs on this data, so `flotilla service start` and `stop`
/// are about it. A database named by `--db` is never the service's.
fn service_serves(paths: &Paths) -> bool {
    paths.given != Given::Db && service::serves(&absolute(&paths.dir))
}

/// A search's hits, one a line: the thread, then a title's line, or where its best message is
/// (its seq), what it is, its line, and its thread's title (with how many matched as well).
pub(crate) fn print_hits(hits: &[flotilla_proto::SearchHit], took: std::time::Duration) {
    for h in hits {
        println!("{}", hit_line(h));
    }
    eprintln!(
        "{} hit{} in {:.1} ms",
        hits.len(),
        if hits.len() == 1 { "" } else { "s" },
        took.as_secs_f64() * 1000.
    );
}

fn hit_line(h: &flotilla_proto::SearchHit) -> String {
    use flotilla_proto::HitKind;
    let thread: String = h.thread.chars().take(8).collect();
    let kind = match h.kind {
        HitKind::Title => "title",
        HitKind::Prompt => "prompt",
        HitKind::Steer => "steer",
        HitKind::Text => "reply",
        HitKind::Tool => "tool",
    };
    match h.seq {
        // A title's line is the title.
        None => format!("{thread}  {kind:<14} {}", h.snippet),
        Some(seq) => {
            let from = h
                .from
                .as_deref()
                .map(|f| format!(" · {f}"))
                .unwrap_or_default();
            let more = if h.hits > 1 {
                format!(" · {} matches", h.hits)
            } else {
                String::new()
            };
            format!(
                "{thread}  {:<7} {kind:<6} {}  [{}{from}{more}]",
                format!("#{seq}"),
                h.snippet,
                h.title
            )
        }
    }
}

fn finish(r: std::result::Result<(), String>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Serving {
    host: String,
    port: u16,
    socket: Option<PathBuf>,
    insecure: bool,
    tailscale: bool,
    url: Option<String>,
    /// `--mcp-port`, `--mcp-host`, `--mcp-url`: remote MCP turned on (and kept).
    mcp: Option<(u16, Option<String>, Option<String>)>,
    offline: bool,
    demo: bool,
}

/// Remote MCP's listener, if it's on (given now, else as kept): where, and the address a
/// tunnel or proxy publishes it at. Off, there's none.
fn remote_mcp_listen(
    db: &Path,
    given: Option<(u16, Option<String>, Option<String>)>,
) -> std::result::Result<Option<flotilla_core::remote_mcp::Settings>, String> {
    use flotilla_core::remote_mcp::{self, Settings};
    let store = Store::open(db).map_err(|e| e.to_string())?;
    let kept = remote_mcp::load(&store);
    let Some((port, host, url)) = given else {
        return Ok(kept);
    };
    let url = url.as_deref().map(remote_mcp::check_url).transpose()?;
    // Given now: kept, as `--tailscale-serve` is (a Funnel it was published with stays so).
    let s = Settings {
        port,
        host,
        funnel: kept.is_some_and(|k| k.funnel) && url.is_none(),
        url,
    };
    remote_mcp::save(&store, Some(&s)).map_err(|e| e.to_string())?;
    Ok(Some(s))
}

/// `flotilla serve`.
fn serve(paths: &Paths, a: Serving) -> ExitCode {
    let db = &paths.db;
    let tcp = match flotilla_server::Listen::tcp(&a.host, a.port, a.insecure) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let loopback = matches!(&tcp, flotilla_server::Listen::Tcp(addr) if addr.ip().is_loopback());
    if a.tailscale && !loopback {
        eprintln!("error: --tailscale-serve forwards to 127.0.0.1: leave --host as it is");
        return ExitCode::FAILURE;
    }
    if !loopback {
        eprintln!("warning: {}", flotilla_server::INSECURE_WARNING);
    }
    if let Some(url) = &a.url
        && !(url.starts_with("https://") || url.starts_with("http://"))
    {
        eprintln!("error: --url {url}: give the whole address, https://…");
        return ExitCode::FAILURE;
    }
    let socket = match a.socket {
        Some(s) => s,
        None => {
            // Only its owner may enter the socket's directory.
            let run = paths.dir.join("run");
            if let Err(e) = private_dir(&run) {
                eprintln!("error: {}: {e}", run.display());
                return ExitCode::FAILURE;
            }
            run.join("server.sock")
        }
    };
    // A demo resets its database: never the app's, nor one named by accident.
    if a.demo && paths.given == Given::Default {
        eprintln!("error: --demo needs a data directory of its own: give --data-dir");
        return ExitCode::FAILURE;
    }
    let mode = if a.demo {
        flotilla_engine::host::Mode::Demo
    } else if a.offline {
        flotilla_engine::host::Mode::Offline
    } else {
        flotilla_engine::host::Mode::Engine
    };
    // `--tailscale-serve` is kept (as `flotilla setup --tailscale-serve` keeps it), so the setup
    // checks know to look, and the next start does it too.
    // A demo publishes nothing of its own accord: not what a database it resets kept.
    let tailscale = a.tailscale
        || !a.demo
            && Store::open(db)
                .ok()
                .and_then(|s| s.kv_get(flotilla_core::tailscale::KEY).ok().flatten())
                .as_deref()
                == Some("on");
    if a.tailscale
        && let Ok(store) = Store::open(db)
    {
        let _ = store.kv_set(flotilla_core::tailscale::KEY, "on");
    }
    if tailscale && !loopback {
        eprintln!(
            "error: Tailscale Serve forwards to 127.0.0.1: leave --host as it is, or run `{}`",
            paths.command("setup --no-tailscale-serve")
        );
        return ExitCode::FAILURE;
    }
    let mcp = match (!a.demo).then(|| remote_mcp_listen(db, a.mcp)).transpose() {
        Ok(m) => m.flatten(),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut listen = vec![tcp, flotilla_server::Listen::Unix(socket.clone())];
    let mut funneled = None;
    let mut mcp_url = None;
    if let Some(m) = &mcp {
        let ip: std::net::IpAddr = match m
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
        {
            Ok(ip) => ip,
            Err(_) => {
                eprintln!("error: remote MCP's host {} isn't an IP address", m.host());
                return ExitCode::FAILURE;
            }
        };
        if !ip.is_loopback() {
            eprintln!(
                "warning: remote MCP listens on {ip}: plain HTTP, so publish it only through HTTPS in front (exe.dev's proxy, a tunnel)"
            );
        }
        mcp_url = m.url.clone();
        listen.push(flotilla_server::Listen::Mcp(
            std::net::SocketAddr::new(ip, m.port),
            mcp_url.clone(),
        ));
    }
    let config = flotilla_server::Config {
        db: db.clone(),
        listen,
        mode,
        role: flotilla_engine::host::Role::Server,
    };
    let mut forwarding = None;
    let r = flotilla_server::serve(config, |server| {
        let addr = server.tcp().ok_or("not listening on TCP")?;
        let url = if tailscale {
            match flotilla_core::tailscale::serve(addr.port()) {
                Ok(url) => {
                    forwarding = Some(addr.port());
                    url
                }
                Err(e) => {
                    eprintln!("warning: no Tailscale HTTPS: {e}");
                    local_url(addr)
                }
            }
        } else {
            local_url(addr)
        };
        // A proxy in front: devices reach it there.
        let url = a
            .url
            .clone()
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or(url);
        // Pairing links give this out: kept, so `flotilla pair` knows it too.
        let store = Store::open(db).map_err(|e| e.to_string())?;
        store
            .kv_set(flotilla_engine::host::SERVER_URL_KEY, &url)
            .map_err(|e| e.to_string())?;
        println!("Flotilla is serving {}", db.display());
        println!("  Devices: {url}");
        println!("  This Mac's user: unix:{}", socket.display());
        // Published with Funnel once it holds the database and listens: a second `serve` that
        // can't start never touches the first one's.
        if let Some(at) = server
            .mcp()
            .filter(|_| mcp.as_ref().is_some_and(|m| m.funnel))
        {
            match flotilla_core::tailscale::funnel(at.port()) {
                Ok(url) => {
                    funneled = Some(at.port());
                    server.publish_mcp(&url);
                    mcp_url = Some(url);
                }
                Err(e) => eprintln!("warning: remote MCP isn't public: {e}"),
            }
        }
        if let Some(at) = server.mcp() {
            match &mcp_url {
                Some(u) => println!("  Remote MCP: {u}/mcp (listening on {at})"),
                None => println!("  Remote MCP: http://{at}/mcp"),
            }
        }
        let now = flotilla_core::util::now_ms();
        if a.demo {
            // For the reviewer's notes: it's no secret, so it's said wherever this goes.
            let code = flotilla_engine::demo::code(&store).ok_or("the demo has no code")?;
            println!();
            println!(
                "Demo: pairs any number of devices with {}, and doesn't expire.",
                flotilla_proto::pair::display_code(&code)
            );
            let scheme = flotilla_core::flavor::get().url_scheme();
            println!("  {}", flotilla_proto::pair::link(scheme, &url, &code));
        } else if store.devices(now).map_err(|e| e.to_string())?.is_empty() {
            println!();
            // A code goes only to a terminal: a service's log (the journal, `docker logs`)
            // keeps what it's given, and anyone who can read it could pair.
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                let code = store
                    .mint_code(flotilla_core::devices::CODE_TTL, "server", now)
                    .map_err(|e| e.to_string())?;
                print_pairing(db, &url, &code);
            } else {
                println!(
                    "No device is paired yet: `{}` makes a link.",
                    paths.command("pair")
                );
            }
        }
        Ok(())
    });
    if let Some(port) = forwarding
        && let Err(e) = flotilla_core::tailscale::off(port)
    {
        eprintln!("warning: {e}");
    }
    if let Some(port) = funneled
        && let Err(e) = flotilla_core::tailscale::funnel_off(port)
    {
        eprintln!("warning: {e}");
    }
    finish(r)
}

/// `dir`, made (or kept) so only its owner can enter it.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// The server's own URL, for pairing links: its address, or this machine's name when it
/// listens everywhere.
fn local_url(addr: std::net::SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        let name = std::process::Command::new("hostname")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|n| !n.is_empty());
        if let Some(name) = name {
            return format!("http://{name}:{}", addr.port());
        }
    }
    format!("http://{addr}")
}

/// The link for a code, a QR code of it, and the code to type.
pub(crate) fn print_pairing(db: &Path, url: &str, code: &flotilla_core::devices::Code) {
    let link =
        flotilla_proto::pair::link(flotilla_core::flavor::get().url_scheme(), url, &code.code);
    println!("Pair a device: open this link on it, or scan the code.");
    println!("  {link}");
    match qrcode::QrCode::new(link.as_bytes()) {
        Ok(qr) => println!(
            "{}",
            qr.render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build()
        ),
        Err(e) => eprintln!("warning: no QR code: {e}"),
    }
    let now = flotilla_core::util::now_ms();
    println!(
        "  Or enter {url} and {} · expires {} ({})",
        flotilla_proto::pair::display_code(&code.code),
        local_when(code.expires_ms, now),
        time_left(code.expires_ms - now)
    );
    if url.contains("://127.0.0.1") || url.contains("://[::1]") || url.contains("://localhost") {
        // The app lets devices in from its Settings; a server is told on its command line.
        let how = match flotilla_core::lock::holder(db) {
            Some(h) if h.kind == "app" => "turn on Over Tailscale in the app's Settings → Devices",
            _ => "--tailscale-serve",
        };
        println!(
            "  This link only works on this machine: reach it from your other devices with {how}."
        );
    }
}

/// How long a code has left: "5 min", "2 h", "23 h 59 min".
fn time_left(ms: i64) -> String {
    let min = ((ms + 59_999) / 60_000).max(1);
    match (min / 60, min % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// A line of `flotilla pair --list`: when it was made and when it expires (or did), each with
/// its day unless it's today's, in `tz`.
fn code_line<Tz: chrono::TimeZone>(
    c: &flotilla_core::devices::OpenCode,
    now: i64,
    tz: &Tz,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let when = |ms| flotilla_core::util::when_in(tz, ms, now);
    let expiry = if c.expires_ms <= now {
        format!("expired {}", when(c.expires_ms))
    } else {
        format!("expires {}", when(c.expires_ms))
    };
    let agent = if c.scope == flotilla_core::devices::Scope::Agent {
        "  (agent)"
    } else {
        ""
    };
    format!(
        "{}  made by {:<16}  {:<16}  {expiry}{agent}",
        c.id,
        c.made_by,
        when(c.created_ms)
    )
}

/// `90s`, `10m`, `2h`, or seconds.
fn parse_ttl(s: &str) -> std::result::Result<Duration, String> {
    let s = s.trim();
    let (n, unit) = s
        .find(|c: char| !c.is_ascii_digit())
        .map_or((s, ""), |i| (&s[..i], &s[i..]));
    let n: u64 = n
        .parse()
        .map_err(|_| format!("{s:?} isn't a duration (90s, 10m, 2h)"))?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err(format!("{s:?} isn't a duration (90s, 10m, 2h)")),
    };
    if secs == 0 || Duration::from_secs(secs) > flotilla_core::devices::CODE_TTL_MAX {
        return Err("A code works for at most 24h".into());
    }
    Ok(Duration::from_secs(secs))
}

/// `p` from the root, as a unit file needs it.
fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `flotilla import`.
fn import(source: &Path, paths: &Paths, replace: bool) -> std::result::Result<(), String> {
    let db = &paths.db;
    if source
        .canonicalize()
        .ok()
        .is_some_and(|s| db.canonicalize().ok() == Some(s))
    {
        return Err("That's the database being imported into".into());
    }
    // A Flotilla still running on the source (the app, on this machine) would keep writing it,
    // and keep running its threads beside this server. Held until the source says it moved,
    // so none starts on it meanwhile. A copy somewhere only readable can't be locked, and
    // nothing runs on it.
    let source_lock =
        match flotilla_core::lock::Lock::take(source, &flotilla_core::lock::Holder::new("cli")) {
            Ok(lock) => Some(lock),
            Err(flotilla_core::lock::Busy::Held(h)) => {
                let who = h
                    .map(|h| format!(" (the {}, pid {})", h.kind, h.pid))
                    .unwrap_or_default();
                return Err(format!(
                    "Flotilla is running on {}{who}: quit it first",
                    source.display()
                ));
            }
            Err(flotilla_core::lock::Busy::Io(_)) => None,
        };
    let _lock = flotilla_core::lock::Lock::take(db, &flotilla_core::lock::Holder::new("cli"))
        .map_err(|e| match e {
            flotilla_core::lock::Busy::Held(h) => {
                let who = h
                    .map(|h| format!(" (the {}, pid {})", h.kind, h.pid))
                    .unwrap_or_default();
                let how = if service_serves(paths) {
                    " (`flotilla service stop`)"
                } else {
                    ""
                };
                format!(
                    "A Flotilla is running on {}{who}: stop it first{how}",
                    db.display()
                )
            }
            e => e.to_string(),
        })?;
    let moved_before = flotilla_core::owner::moved(source);
    let got = Store::import(source, db, replace).map_err(|e| e.to_string())?;
    // Skills and attachments waiting to be sent are files beside the database.
    let from = flotilla_core::blobs::Blobs::beside(source);
    let (blobs, skipped) = flotilla_core::blobs::Blobs::beside(db).copy_from(&from);
    println!(
        "Imported {} into {}: {} thread(s), {} project(s), {blobs} file(s).",
        source.display(),
        db.display(),
        got.threads,
        got.projects
    );
    if skipped > 0 {
        println!(
            "  {skipped} file(s) in {} didn't read whole, and weren't copied.",
            from.dir().display()
        );
    }
    if let Some(aside) = &got.replaced
        && replace
    {
        println!("  What was here is now {}.", aside.display());
    }
    if let Some(id) = &got.adopted {
        println!(
            "  This server now runs the VMs that database's threads and projects use (it was Flotilla {id})."
        );
    }
    if let Some(before) = moved_before.filter(|m| m.server != got.server) {
        println!(
            "  It had moved to {} before: stop that server, or both will run these threads.",
            before.place()
        );
    }
    // The source says so, and no engine starts on it again. One on another machine learns it
    // when its app connects here.
    if source_lock.is_some()
        && flotilla_core::owner::mark_moved(
            source,
            &flotilla_core::owner::Moved::to(&got.server, got.url.clone()),
        )
        .is_ok()
    {
        println!(
            "  {} now says it moved here: no Flotilla runs its threads from it again.",
            source.display()
        );
    }
    println!(
        "  Connect the app that used it to this server: once it has, it won't run those threads itself."
    );
    drop(source_lock);
    match service_serves(paths) {
        true => println!(
            "Start the server: `flotilla service start`, or `{}`.",
            paths.command("serve")
        ),
        false => println!("Start the server: `{}`.", paths.command("serve")),
    }
    Ok(())
}

/// `flotilla pair`: straight into the database, so it works even when the server doesn't
/// answer.
fn pair(
    paths: &Paths,
    ttl: Option<String>,
    agent: bool,
    list: bool,
    revoke: Option<String>,
) -> std::result::Result<(), String> {
    let db = &paths.db;
    let store = Store::open(db).map_err(|e| e.to_string())?;
    let now = flotilla_core::util::now_ms();
    if list {
        let codes = store.unused_codes().map_err(|e| e.to_string())?;
        if codes.is_empty() {
            println!("No unused pairing codes.");
        }
        for c in codes {
            println!("{}", code_line(&c, now, &chrono::Local));
        }
        return Ok(());
    }
    if let Some(id) = revoke {
        return match store.revoke_code(&id, now).map_err(|e| e.to_string())? {
            true => {
                println!("Revoked {id}.");
                Ok(())
            }
            false => Err(format!(
                "There's no unused code {id} (see `{}`)",
                paths.command("pair --list")
            )),
        };
    }
    let ttl = ttl
        .as_deref()
        .map(parse_ttl)
        .transpose()?
        .unwrap_or(flotilla_core::devices::CODE_TTL);
    let url = store
        .kv_get(flotilla_engine::host::SERVER_URL_KEY)
        .map_err(|e| e.to_string())?;
    let running = flotilla_core::lock::holder(db).is_some_and(|h| h.address.is_some());
    let Some(url) = url else {
        return Err(format!(
            "Nothing lets other devices in on this database: start `{}`, or turn on Settings → Devices → Let other devices connect in the app",
            paths.command("serve")
        ));
    };
    let scope = if agent {
        flotilla_core::devices::Scope::Agent
    } else {
        flotilla_core::devices::Scope::Full
    };
    let code = store
        .mint_code_as(ttl, "cli", scope, now)
        .map_err(|e| e.to_string())?;
    if agent {
        // An assistant pairs with the code, not the link.
        println!(
            "Agent pairing code: {}  (expires {})",
            flotilla_core::devices::display_code(&code.code),
            local_when(code.expires_ms, now)
        );
        println!(
            "On the assistant's machine: flotilla connect {url} {}",
            flotilla_core::devices::display_code(&code.code)
        );
    } else {
        print_pairing(db, &url, &code);
    }
    if !running {
        println!(
            "  No server is running on {} now: start `{}` before using it.",
            db.display(),
            paths.command("serve")
        );
    }
    Ok(())
}

/// `flotilla pair --agent --token`: an agent device made now, and its token printed this once,
/// for an assistant that sends it as a header to remote MCP.
fn agent_token(paths: &Paths, name: Option<String>) -> std::result::Result<(), String> {
    use flotilla_core::devices::Scope;
    let store = Store::open(&paths.db).map_err(|e| e.to_string())?;
    let now = flotilla_core::util::now_ms();
    let name = name.unwrap_or_else(|| "Remote MCP".into());
    let code = store
        .mint_code_as(std::time::Duration::from_secs(60), "cli", Scope::Agent, now)
        .map_err(|e| e.to_string())?;
    let paired = store
        .pair_device_as(
            &code.code,
            &name,
            flotilla_server::mcp::PLATFORM,
            Scope::Agent,
            None,
            now,
        )
        .map_err(|e| e.to_string())?;
    eprintln!(
        "Agent device {} ({name}). Its token, shown this once (send it as Authorization: Bearer <token>; revoke with `{}`):",
        paired.device,
        paths.command(&format!("devices revoke {}", paired.device))
    );
    println!("{}", paired.token);
    Ok(())
}

/// `flotilla devices`.
fn devices(paths: &Paths, cmd: DevicesCmd) -> std::result::Result<(), String> {
    let db = &paths.db;
    match cmd {
        DevicesCmd::List => {
            // A running server knows which are connected.
            let connected = match flotilla_core::lock::holder(db).and_then(|h| h.address) {
                Some(address) => remote::connected_devices(&address).ok(),
                None => None,
            };
            let store = Store::open(db).map_err(|e| e.to_string())?;
            let now = flotilla_core::util::now_ms();
            let list = store.devices(now).map_err(|e| e.to_string())?;
            if list.is_empty() {
                println!(
                    "No devices are paired. Pair one with `{}`.",
                    paths.command("pair")
                );
            }
            for d in list {
                let seen = match &connected {
                    Some(c) if c.contains(&d.id) => "connected now".to_string(),
                    _ => format!("last seen {}", local_when(d.last_seen_ms, now)),
                };
                let ip = d
                    .last_ip
                    .map(|ip| format!(" from {ip}"))
                    .unwrap_or_default();
                let kind = if d.scope == flotilla_core::devices::Scope::Agent {
                    "agent"
                } else {
                    &d.platform
                };
                println!(
                    "{}  {:<24} {:<8} {seen}{ip}",
                    d.id,
                    ellipsize(&d.name, 24),
                    kind
                );
            }
            Ok(())
        }
        DevicesCmd::Revoke { id } => {
            // A rebuild waits for this, so it can't carry the device into its new database.
            let _held = flotilla_core::devices::hold(db).map_err(|e| e.to_string())?;
            match Store::open(db)
                .and_then(|s| s.revoke_device(&id))
                .map_err(|e| e.to_string())?
            {
                true => {
                    println!("Revoked {id}: its token no longer works, and its connections close.");
                    Ok(())
                }
                false => Err(format!(
                    "There's no device {id} (see `{}`)",
                    paths.command("devices")
                )),
            }
        }
    }
}

/// The model catalog, one provider (or all) at a time.
pub(crate) fn print_catalog(catalog: &flotilla_core::catalog::Catalog, provider: Option<&str>) {
    for p in &catalog.providers {
        if provider.is_some_and(|want| catalog.provider(want).map(|x| &x.key) != Some(&p.key)) {
            continue;
        }
        println!(
            "{}{}",
            p.key,
            p.default_model
                .as_deref()
                .map(|d| format!("  (default {d})"))
                .unwrap_or_default()
        );
        for m in &p.models {
            let effort = m
                .effort
                .as_ref()
                .map(|e| {
                    format!(
                        "effort {} [default {}]",
                        e.supported.join("|"),
                        e.default.as_deref().unwrap_or("-")
                    )
                })
                .unwrap_or_else(|| "no effort".into());
            println!(
                "  {:<34} {:<44} {}",
                m.id,
                effort,
                if m.fast_mode { "fast" } else { "" }
            );
        }
    }
}

/// `open`: how `threads`, `show` and `search` open the database (read only beside another
/// process that has it).
fn run(cmd: Cmd, paths: &Paths, open: fn(&Path) -> Result<Store>) -> Result<()> {
    let db = &paths.db;
    match cmd {
        Cmd::Envs => {
            let core = Core::open(db)?;
            if let Err(e) = core.sync_environments() {
                eprintln!("warning: couldn't refresh environments ({e}); showing cached");
            }
            for p in core.store.environments()? {
                println!(
                    "{}  {}{}",
                    p.id,
                    p.name,
                    if p.is_default { "  (default)" } else { "" }
                );
            }
        }
        Cmd::Models { provider } => {
            let core = Core::open(db)?;
            print_catalog(&core.catalog()?, provider.as_deref());
        }
        Cmd::Limits => {
            let core = Core::open(db)?;
            let l = core.limits()?;
            println!(
                "tier {}  active {}/{}  can start: {}",
                l.access_tier.as_deref().unwrap_or("?"),
                l.active_sandboxes.unwrap_or(-1),
                l.max_active_sandboxes.unwrap_or(-1),
                l.can_start
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "?".into())
            );
            if let Some(r) = l.start_blocked_reason.or(l.blocked_reason) {
                println!("blocked: {r}");
            }
            if let Some(c) = l.raw.get("currentLimits") {
                println!("current limits: {c}");
            }
        }
        Cmd::Threads => {
            let store = open(db)?;
            let threads = store.threads()?;
            if threads.is_empty() {
                println!("no threads (try `{}`)", paths.command("sync"));
            }
            for t in threads {
                println!("{}", thread_line(&t));
            }
        }
        Cmd::Sync => {
            let mut core = Core::open(db)?;
            let r = core.full_sync()?;
            println!(
                "{} environments, {} flotilla sandboxes, {} threads created, {} expired, {} polled",
                r.environments,
                r.sandboxes,
                r.created.len(),
                r.expired.len(),
                r.polled
            );
            if let Some(others) = &r.others {
                println!("{others}");
            }
            for e in r.errors {
                eprintln!("warning: {e}");
            }
        }
        Cmd::New {
            environment,
            provider,
            model,
            effort,
            fast,
            title,
            machine_type,
            wait,
            no_follow,
            prompt,
        } => {
            let mut core = Core::open(db)?;
            let environment = match environment {
                Some(p) => p,
                None => {
                    core.sync_environments()?;
                    core.store
                        .environments()?
                        .into_iter()
                        .find(|p| p.is_default)
                        .map(|p| p.name)
                        .ok_or_else(|| {
                            Error::Invalid("no default environment; pass --env".into())
                        })?
                }
            };
            let req = NewThread {
                environment,
                project_id: None,
                provider,
                model,
                effort,
                fast,
                title,
                machine_type,
                prompt,
                attachments: vec![],
                wait_ready: wait,
                on_branch: None,
                id: None,
                parent: None,
                branch_tag: None,
            };
            let started = Instant::now();
            let t = core.new_thread(&req)?;
            println!(
                "created {}  sandbox {}  ({:.1}s)",
                t.id,
                t.sandbox_id.as_deref().unwrap_or("-"),
                started.elapsed().as_secs_f64()
            );
            if !no_follow {
                follow(&mut core, &t.id, false)?;
            }
        }
        Cmd::Send {
            thread,
            text,
            no_follow,
        } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            match core.send(&t.id, &text)? {
                Sent::Prompt(id) => println!("prompted ({id})"),
                Sent::Steer(id) => println!("steered ({id})"),
            }
            if !no_follow {
                follow(&mut core, &t.id, false)?;
            }
        }
        Cmd::Follow { thread } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            follow(&mut core, &t.id, true)?;
        }
        Cmd::Search { query, limit } => {
            let started = Instant::now();
            let hits = open(db)?.search(&query.join(" "), limit)?;
            print_hits(&hits, started.elapsed());
        }
        Cmd::Show { thread, verbose } => {
            let store = open(db)?;
            let t = store.find_thread(&thread)?;
            println!("{}", thread_line(&t));
            for m in store.messages(&t.id)? {
                print_message(&m, verbose, "");
            }
        }
        Cmd::Steer { thread, text } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            match core.steer(&t.id, &text)? {
                Sent::Steer(id) => println!("steered ({id})"),
                Sent::Prompt(id) => {
                    println!("the turn had ended, so this was sent as a new prompt ({id})")
                }
            }
        }
        Cmd::Interrupt { thread } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            core.interrupt(&t.id)?;
            println!("interrupted");
        }
        Cmd::Stop { thread, no_wait } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            let t = core.stop(&t.id, !no_wait)?;
            println!("{}", thread_line(&t));
        }
        // They run where the engine runs: a server (or the app) running here.
        Cmd::Schedule(schedule::ScheduleCmd::List) => {
            let store = Store::open(db)?;
            let all = store.schedules()?;
            if all.is_empty() {
                println!("nothing scheduled");
            }
            let now = flotilla_core::util::now_ms();
            for s in &all {
                let to = match &s.target {
                    flotilla_proto::Target::Thread(t) => store
                        .thread(t)?
                        .map(|t| t.title)
                        .unwrap_or_else(|| t.clone()),
                    flotilla_proto::Target::New(f) => {
                        format!("new thread ({} {})", f.provider, f.model)
                    }
                };
                println!("{}", schedule::line(s, &to, now));
            }
        }
        Cmd::Schedule(_) => {
            return Err(Error::Invalid(format!(
                "scheduled prompts are set on a running server: start `{}` (or the app) first",
                "flotilla serve"
            )));
        }
        Cmd::Services {
            thread,
            action,
            name,
        } => {
            use flotilla_core::services;
            let core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            let started = Instant::now();
            let notify = || {};
            let need_name = || {
                name.clone()
                    .ok_or_else(|| Error::Invalid("which service?".into()))
            };
            match action.as_deref() {
                None => {}
                Some("ensure") => services::ensure(
                    &core.boat,
                    &core.store,
                    &t.id,
                    &services::Ensure {
                        include_stopped: true,
                        restart: None,
                    },
                    &notify,
                )?,
                Some("restart") => services::ensure(
                    &core.boat,
                    &core.store,
                    &t.id,
                    &services::Ensure {
                        include_stopped: false,
                        restart: Some(need_name()?),
                    },
                    &notify,
                )?,
                Some("stop") => services::stop(&core.boat, &core.store, &t.id, &need_name()?)?,
                Some("check") => {
                    services::check(&core.boat, &core.store, &t.id)?;
                }
                Some(other) => {
                    return Err(Error::Invalid(format!(
                        "unknown services action {other:?}: ensure, restart, stop or check"
                    )));
                }
            }
            if action.is_some() {
                println!("({:.1}s)", started.elapsed().as_secs_f64());
            }
            match services::state(&core.store, &t.id)? {
                None => println!("no services"),
                Some(s) => {
                    println!("{} on {}", s.file.as_deref().unwrap_or("?"), s.sandbox);
                    if let Some(e) = &s.error {
                        println!("error: {e}");
                    }
                    for r in &s.services {
                        let how = match (r.state.as_str(), r.exit_code, &r.error) {
                            ("exited", Some(c), _) => format!("exited {c}"),
                            ("failed", _, Some(e)) => format!("failed: {e}"),
                            (st, _, _) => st.to_string(),
                        };
                        // The preview URL carries boat's sign-in token: show its origin only.
                        let url = r
                            .preview
                            .as_deref()
                            .map(services::origin)
                            .unwrap_or_default();
                        println!("  {:<12} :{:<5} {:<15} {url}", r.name, r.port, how);
                        for line in flotilla_core::setup::tail(&r.output, 3) {
                            println!("      | {line}");
                        }
                    }
                }
            }
        }
        Cmd::Resume { thread } => {
            let core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            let started = Instant::now();
            let t = core.resume(&t.id)?;
            println!(
                "{}  ({:.1}s)",
                thread_line(&t),
                started.elapsed().as_secs_f64()
            );
        }
        Cmd::Rename { thread, title } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            let t = core.rename(&t.id, &title)?;
            println!("{}", thread_line(&t));
        }
        Cmd::Archive { thread } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            core.archive(&t.id, Some(&github()))?;
            println!("archived {}", t.id);
        }
        Cmd::Unarchive { thread } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            core.unarchive(&t.id)?;
            println!("unarchived {}", t.id);
        }
        Cmd::KeepRunning { thread, state } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            let t = core.set_keep_running(&t.id, matches!(state, OnOff::On))?;
            println!(
                "{}  auto-stop at {}",
                thread_line(&t),
                t.archive_after.as_deref().unwrap_or("-")
            );
        }
        Cmd::Preview { thread, port } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            println!("{}", core.preview(&t.id, port)?);
        }
        Cmd::Rebuild { yes } => {
            if !yes {
                return Err(Error::Invalid(
                    "rebuild renames the database aside and re-downloads everything; pass --yes"
                        .into(),
                ));
            }
            if db.exists() {
                let bak = Store::backup(db)?;
                println!("moved the old database to {}", bak.display());
            }
            let mut core = Core::open(db)?;
            let started = Instant::now();
            let r = core.full_sync()?;
            println!(
                "rebuilt in {:.1}s: {} environments, {} threads, {} polled",
                started.elapsed().as_secs_f64(),
                r.environments,
                r.created.len(),
                r.polled
            );
            for e in r.errors {
                eprintln!("warning: {e}");
            }
            for t in core.store.threads()? {
                println!("{}", thread_line(&t));
            }
        }
        Cmd::Delete { thread, yes } => {
            let mut core = Core::open(db)?;
            let t = core.store.find_thread(&thread)?;
            if !yes {
                return Err(Error::Invalid(format!(
                    "this deletes sandbox {} for good; pass --yes",
                    t.sandbox_id.as_deref().unwrap_or("-")
                )));
            }
            core.delete(&t.id, Some(&github()))?;
            println!(
                "deleted {} ({})",
                t.id,
                t.sandbox_id.as_deref().unwrap_or("no sandbox")
            );
        }
    }
    Ok(())
}

pub(crate) fn thread_line(t: &Thread) -> String {
    let state = match (t.state, days_left(t)) {
        (ThreadState::Stopped, Some(d)) => format!("stopped ({d}d left)"),
        (s, _) => s.as_str().to_string(),
    };
    let settings = format!(
        "{}/{}{}{}",
        t.provider,
        t.model,
        t.effort
            .as_deref()
            .map(|e| format!("/{e}"))
            .unwrap_or_default(),
        if t.fast { "/fast" } else { "" }
    );
    format!(
        "{}  {:<18} {:<28} {:<12} {}{}",
        &t.id[..8.min(t.id.len())],
        state,
        settings,
        t.sandbox_id.as_deref().unwrap_or("-"),
        t.title,
        if t.keep_running {
            "  [keep running]"
        } else {
            ""
        }
    )
}

pub(crate) fn print_message(m: &Message, verbose: bool, text_prefix: &str) {
    print!("{}", message_text(m, verbose, text_prefix));
}

/// A message's lines, as `show` and `follow` print it.
pub(crate) fn message_text(m: &Message, verbose: bool, text_prefix: &str) -> String {
    let status = m
        .status
        .as_deref()
        .map(|s| format!("  [{s}]"))
        .unwrap_or_default();
    match m.kind.as_str() {
        "prompt" => format!(
            "\n> {}{status}\n",
            m.text.as_deref().unwrap_or("").replace('\n', "\n  ")
        ),
        "steer" => format!(">> steer: {}{status}\n", m.text.as_deref().unwrap_or("")),
        "text" => format!("{text_prefix}{}\n", m.text.as_deref().unwrap_or("")),
        "tool" => {
            let mut s = format!(
                "  · {} {}{}{}{status}\n",
                m.tool_name.as_deref().unwrap_or("tool"),
                m.tool_summary.as_deref().unwrap_or(""),
                if m.is_error { "  [error]" } else { "" },
                if m.pending { "  [pending]" } else { "" }
            );
            if verbose {
                s += &format!(
                    "      input:  {}\n",
                    m.tool_input_json.as_deref().unwrap_or("-")
                );
                s += &format!(
                    "      result: {}\n",
                    ellipsize(&one_line(m.tool_result_json.as_deref().unwrap_or("-")), 400)
                );
            }
            s
        }
        _ => format!("  ! {}\n", m.text.as_deref().unwrap_or("")),
    }
}

/// Poll at 1 Hz and print messages as they settle; show partial text as it grows; print each
/// tool line once it's no longer pending; return when the turn settles.
fn follow(core: &mut Core, thread_id: &str, from_start: bool) -> Result<()> {
    let mut shown = shown::Shown::default();
    if !from_start {
        shown.seed(&core.store.messages(thread_id)?);
    }
    let mut errors = 0;
    let mut out = std::io::stdout();
    loop {
        let tick = Instant::now();
        let outcome = match core.poll(thread_id) {
            Ok(o) => {
                errors = 0;
                o
            }
            Err(e) => {
                errors += 1;
                eprintln!("\nwarning: poll failed ({e})");
                if errors >= 30 {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        for w in &outcome.warnings {
            eprintln!("\nwarning: {w}");
        }
        let blocked = shown.settle(&mut out, &core.store.messages(thread_id)?)?;
        if !blocked && let Some(live) = core.live(thread_id) {
            shown.stream(&mut out, &live.partials)?;
        }
        if !outcome.active {
            shown.finish(&mut out)?;
            let t = core
                .store
                .thread(thread_id)?
                .ok_or_else(|| Error::NotFound(thread_id.into()))?;
            println!("\n[{}]", thread_line(&t));
            return Ok(());
        }
        if let Some(rest) = Duration::from_secs(1).checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    /// `flotilla serve` opens remote MCP's listener only when it's turned on: given now (and
    /// kept), or kept from before. With nothing said, there's none.
    #[test]
    fn serve_opens_remote_mcp_only_when_its_on() {
        let dir = std::env::temp_dir().join(format!("flotilla-serve-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("flotilla.db");
        assert_eq!(remote_mcp_listen(&db, None).unwrap(), None);
        let on = remote_mcp_listen(
            &db,
            Some((8799, None, Some("https://mcp.example.com/".into()))),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (on.port, on.url.as_deref(), on.funnel),
            (8799, Some("https://mcp.example.com"), false)
        );
        // Kept: the next start opens it too.
        assert_eq!(remote_mcp_listen(&db, None).unwrap(), Some(on));
        assert!(remote_mcp_listen(&db, Some((8799, None, Some("ftp://x".into())))).is_err());
        flotilla_core::remote_mcp::save(&Store::open(&db).unwrap(), None).unwrap();
        assert_eq!(remote_mcp_listen(&db, None).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `pair --list` at 22:00, seven hours behind UTC: a day's code made now expires
    /// tomorrow, not at a bare "22:00" that reads as gone; one past its time says it expired.
    #[test]
    fn pair_list_says_which_day_and_what_expired() {
        let tz = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
        let at = |month: u32, day: u32, h: u32, m: u32| {
            tz.with_ymd_and_hms(2026, month, day, h, m, 0)
                .unwrap()
                .timestamp_millis()
        };
        let now = at(10, 31, 22, 0);
        let code = |made: i64, expires: i64| flotilla_core::devices::OpenCode {
            id: "pc_1".into(),
            made_by: "cli".into(),
            created_ms: made,
            expires_ms: expires,
            scope: flotilla_core::devices::Scope::Full,
        };
        assert_eq!(
            code_line(&code(now, at(11, 1, 22, 0)), now, &tz),
            "pc_1  made by cli               22:00             expires tomorrow 22:00"
        );
        assert_eq!(
            code_line(&code(at(10, 31, 21, 0), at(10, 31, 21, 5)), now, &tz),
            "pc_1  made by cli               21:00             expired 21:05"
        );
        assert_eq!(
            code_line(&code(at(10, 30, 21, 0), at(10, 30, 21, 5)), now, &tz),
            "pc_1  made by cli               yesterday 21:00   expired yesterday 21:05"
        );
        assert_eq!(
            code_line(&code(now, at(10, 31, 22, 5)), now, &tz),
            "pc_1  made by cli               22:00             expires 22:05"
        );
    }

    /// `--data-dir` works on every command, before or after it, as the server docs say.
    #[test]
    fn every_command_takes_the_data_dir() {
        use clap::Parser as _;
        for args in [
            &["flotilla", "search", "palindrome", "--data-dir", "/srv/fl"][..],
            &["flotilla", "threads", "--data-dir", "/srv/fl"],
            &["flotilla", "--data-dir", "/srv/fl", "show", "2f69"],
            &["flotilla", "doctor", "--data-dir", "/srv/fl"],
            &["flotilla", "serve", "--data-dir", "/srv/fl"],
            &[
                "flotilla",
                "devices",
                "revoke",
                "abc",
                "--data-dir",
                "/srv/fl",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert_eq!(
                Paths::resolve(cli.data_dir, cli.db, cli.secrets_dir)
                    .unwrap()
                    .db,
                PathBuf::from("/srv/fl/flotilla.db"),
                "{args:?}"
            );
        }
        // The database and the data directory are one place: naming both is refused, wherever
        // each goes.
        for args in [
            &[
                "flotilla",
                "--db",
                "/x/flotilla.db",
                "--data-dir",
                "/y",
                "threads",
            ][..],
            &[
                "flotilla",
                "--db",
                "/x/flotilla.db",
                "serve",
                "--data-dir",
                "/y",
            ],
            &[
                "flotilla",
                "setup",
                "--data-dir",
                "/y",
                "--db",
                "/x/flotilla.db",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            let e = Paths::resolve(cli.data_dir, cli.db, cli.secrets_dir)
                .err()
                .unwrap_or_else(|| panic!("{args:?} was taken"));
            assert!(e.contains("not both"), "{args:?}: {e}");
        }
        // With no words, search says how to use it.
        let e = Cli::try_parse_from(["flotilla", "search"]).err().unwrap();
        assert_eq!(e.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        assert!(e.to_string().contains("Usage: flotilla search"), "{e}");
        assert!(Cli::try_parse_from(["flotilla", "search", ""]).is_err());
    }

    /// One reading of the flags: the data directory is the database's folder, secrets are in
    /// it off a Mac (or `--secrets-dir`), and a command printed names the data as it was named.
    #[test]
    fn paths_are_worked_out_once() {
        let p = |data_dir: Option<&str>, db: Option<&str>, secrets: Option<&str>| {
            Paths::resolve(
                data_dir.map(PathBuf::from),
                db.map(PathBuf::from),
                secrets.map(PathBuf::from),
            )
            .unwrap()
        };
        let beside =
            |dir: &str| (!cfg!(target_os = "macos")).then(|| PathBuf::from(dir).join("secrets"));

        let d = p(Some("/srv/fl"), None, None);
        assert_eq!(
            (d.dir.as_path(), d.db.as_path(), d.given),
            (
                Path::new("/srv/fl"),
                Path::new("/srv/fl/flotilla.db"),
                Given::DataDir
            )
        );
        assert_eq!(d.secrets, beside("/srv/fl"));
        assert_eq!(d.flag(), " --data-dir /srv/fl");

        // A path with a space, or a quote, is one word to the shell; one that starts with a
        // dash isn't taken for an option; secrets given elsewhere are named too.
        let d = p(Some("/srv/Flotilla Dev/it's"), None, None);
        assert_eq!(
            d.command("pair"),
            r#"flotilla pair --data-dir '/srv/Flotilla Dev/it'\''s'"#
        );
        assert_eq!(
            p(Some("-review"), None, None).flag(),
            " --data-dir ./-review"
        );
        assert_eq!(
            p(None, Some("-review.db"), None).flag(),
            " --db ./-review.db"
        );
        assert_eq!(
            p(Some("/srv/fl"), None, Some("/srv/private")).command("serve"),
            "flotilla serve --data-dir /srv/fl --secrets-dir /srv/private"
        );
        for args in [
            &["flotilla", "threads", "--data-dir", "./-review"][..],
            &["flotilla", "threads", "--db", "./-review.db"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }

        let d = p(None, Some("/tmp/demo/demo.db"), None);
        assert_eq!(
            (d.dir.as_path(), d.db.as_path(), d.given),
            (
                Path::new("/tmp/demo"),
                Path::new("/tmp/demo/demo.db"),
                Given::Db
            )
        );
        assert_eq!(d.secrets, beside("/tmp/demo"));
        assert_eq!(d.flag(), " --db /tmp/demo/demo.db");

        // A bare file name is in this folder.
        let d = p(None, Some("demo.db"), Some("/run/keys"));
        assert_eq!(
            (d.dir.as_path(), d.secrets),
            (Path::new("."), Some(PathBuf::from("/run/keys")))
        );

        let d = p(None, None, None);
        assert_eq!(
            (d.db.clone(), d.given),
            (lifecycle::default_db_path(), Given::Default)
        );
        assert_eq!(d.dir, d.db.parent().unwrap());
        assert_eq!(
            d.secrets,
            (!cfg!(target_os = "macos")).then(|| d.dir.join("secrets"))
        );
        assert_eq!(d.flag(), "");
    }

    /// A title hit names its kind once; a message's says where it is and in which thread.
    #[test]
    fn hits_read_once_each() {
        let hit = |kind, seq| flotilla_proto::SearchHit {
            thread: "2f69665b-8f93".into(),
            title: "Palindrome function".into(),
            kind,
            seq,
            snippet: "Palindrome function".into(),
            marks: vec![],
            at: None,
            from: None,
            hits: if seq.is_some() { 3 } else { 0 },
        };
        assert_eq!(
            hit_line(&hit(flotilla_proto::HitKind::Title, None)),
            "2f69665b  title          Palindrome function"
        );
        assert_eq!(
            hit_line(&hit(flotilla_proto::HitKind::Text, Some(12))),
            "2f69665b  #12     reply  Palindrome function  [Palindrome function · 3 matches]"
        );
    }

    #[test]
    fn time_left_reads_in_minutes_then_hours() {
        assert_eq!(time_left(300_000), "5 min");
        assert_eq!(time_left(1), "1 min");
        assert_eq!(time_left(2 * 3_600_000), "2 h");
        assert_eq!(time_left(24 * 3_600_000 - 60_000), "23 h 59 min");
    }
}
