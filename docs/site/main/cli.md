# The CLI

The `flotilla` command runs the server, sets it up and pairs devices. It also starts, follows and manages threads from a terminal. This page lists the common commands; `flotilla --help` and `flotilla <command> --help` have every option.

## Get it

Download it into `~/.local/bin`, and check that's on your `PATH`:

```
mkdir -p ~/.local/bin
# Linux
curl -fL https://github.com/joseph-lozano/flotilla/releases/latest/download/flotilla-cli-linux-$(uname -m).tar.gz |
  tar -xz --strip-components=1 -C ~/.local/bin flotilla-cli-linux-$(uname -m)/flotilla
# Mac
curl -fL https://github.com/joseph-lozano/flotilla/releases/latest/download/flotilla-cli-macos-universal.tar.gz |
  tar -xz --strip-components=1 -C ~/.local/bin flotilla-cli-macos-universal/flotilla
```

Or build it from the repo:

```
cargo build --release -p flotilla-cli
cp target/release/flotilla ~/.local/bin/
```

It works with the Flotilla on the same machine: through the server or the app when one is running, or on its own when neither is. With `--server <address>`, the thread commands go to another server instead, as an [agent device](agent-api.md).

## Server

Run these on the machine that serves Flotilla.

| Command | What it does |
| --- | --- |
| `flotilla serve` | Run the server. `--tailscale-serve` reaches it over your tailnet; `--url` gives its address behind a proxy. |
| `flotilla setup` | Set up this machine: the boat.dev key, GitHub, titles and summaries, Tailscale, remote MCP, iPhone notifications. Then print a pairing link. |
| `flotilla doctor` | What isn't set up, and what to run to fix it. |
| `flotilla service install` | Run the server as a systemd user service on Linux. Also `status`, `start`, `stop`, `restart`, `uninstall`. |
| `flotilla pair` | Make a one-time pairing code, with a link and a QR code. `--agent` for an assistant, `--ttl` for longer than 5 minutes, `--list`, `--revoke`. |
| `flotilla devices` | List paired devices. `flotilla devices revoke <id>` signs one out. |
| `flotilla import <path>` | Bring a Mac's threads to this server. See [Move your Mac's threads](server.md#move-your-macs-threads-to-the-server). |

## Threads

Commands that take a thread take its id, or the first few characters of it.

| Command | What it does |
| --- | --- |
| `flotilla threads` | List threads. |
| `flotilla search <words>` | Find threads by title, and messages by what was said. |
| `flotilla new --provider <agent> --model <id> "<prompt>"` | Start a thread and follow it. `--effort` and `--fast` as in the app; `--no-follow` to return at once. |
| `flotilla send <thread> "<text>"` | Send a follow-up, waking the VM if it's stopped, or steer a running turn. Then follow. |
| `flotilla follow <thread>` | Print messages as they arrive, until the turn ends. |
| `flotilla show <thread>` | Print the thread's messages. |
| `flotilla steer`, `interrupt` | Steer or stop the running turn. |
| `flotilla stop`, `resume` | Stop or wake the thread's VM. |
| `flotilla keep-running <thread> on\|off` | Keep the VM up while it's idle, or not. |
| `flotilla rename`, `archive`, `unarchive`, `delete` | As in the thread menu. |
| `flotilla preview <thread> <port>` | Print a private link to a port in the VM. |
| `flotilla services <thread>` | Show the thread's services. `ensure`, `restart <name>`, `stop <name>` manage them. |

`--provider` is `claude-code`, `codex` or `opencode`. `flotilla models` lists the models boat.dev offers for each.

## Scheduled prompts

`flotilla schedule add`, `list`, `pause`, `resume`, `run` and `rm`. See [Scheduled prompts](schedules.md#from-the-command-line).

## Assistants

For assistants that use Flotilla over MCP.

| Command | What it does |
| --- | --- |
| `flotilla mcp` | Serve Flotilla's tools to an assistant over MCP, on standard input and output. |
| `flotilla connect <address>` | Pair this machine with a server as an agent device. |
| `flotilla disconnect <address>` | Sign that agent device out, and forget its token. |

See [The agent API](agent-api.md).

## boat.dev

These read from boat.dev, or rebuild from it.

| Command | What it does |
| --- | --- |
| `flotilla models` | The models, by agent. |
| `flotilla envs` | Your boat.dev environments. |
| `flotilla limits` | Your plan's limits. |
| `flotilla sync` | Bring everything up to date from boat.dev. |
| `flotilla rebuild` | Rebuild the database from boat.dev, keeping the old one as a backup. |

## Options for every command

These work with any command.

| Option | What it does |
| --- | --- |
| `--data-dir <dir>` | Use Flotilla's data in this folder, not the default. |
| `--boat-key-file <file>` | Read the boat.dev key from this file, which only you may read. `BOAT_API_KEY` works too. |
| `--secrets-dir <dir>` | Keep keys in files here, not in the Keychain (on a Mac) or the data folder (elsewhere). |
| `--server <address>` | Work with the server at this address, as the agent device `flotilla connect` paired. |
