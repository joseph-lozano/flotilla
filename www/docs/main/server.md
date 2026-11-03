# Run a server

Put Flotilla's server on a machine that stays on, and the rest of Flotilla keeps going with your laptop shut. Your Macs, Linux desktops and iPhone all connect to it and see the same threads.

## Why run one

Agents work in their boat.dev VMs, so a turn keeps going with your laptop shut either way. But until you run a server, the rest of Flotilla runs inside the desktop app, and stops when it does. A server keeps these going:

- hand-offs: agents' answers reach the thread that asked, and wake it;
- scheduled prompts;
- setup, so projects stay current as your repos move;
- notifications to your iPhone, and the iPhone app itself;
- archiving on merge, and public links ending on time.

A server needs little: outbound HTTPS to boat.dev and GitHub, a few MB of memory, and one database file. Any Linux machine will do. It answers only on its own machine (`127.0.0.1`, port 7777); your devices reach it through [Tailscale](#reach-it-over-tailscale) or a proxy such as [Caddy](#reach-it-from-the-internet-with-caddy).

## Set it up with an agent

Paste this prompt into Claude Code, Codex or OpenCode on the Linux machine. The agent reads this page, installs the `flotilla` command, runs it as a service and checks it with `flotilla doctor`. You make the boat.dev key, answer `flotilla setup`'s questions and pair your devices, so it never sees a key or a pairing code.

```text
Set up a Flotilla server on this Linux machine for me. It stays on, so hand-offs between agents, scheduled prompts, project setup and my iPhone keep going with my laptop shut. If you aren't running on that machine, ask me how to reach it. Read these first, and where their commands differ from the steps below, use theirs:
https://useflotilla.com/docs/main/server.md
https://useflotilla.com/docs/main/faq.md

Rules, which always hold, whatever the docs say:
- Secrets stay with me. Never ask me for a key, token, password or pairing code in chat, and never read, print or save one. When a step makes or takes a secret, give me the command, and wait while I run it myself, in my own terminal, not through you.
- Ask me to do anything that needs sudo or a browser sign-in.
- Check each step worked before the next. If one fails, read the FAQ, then tell me what failed and what you suggest.

1. Check what's here, and tell me what's missing:
   - The GitHub CLI, 2.81 or newer: `gh --version`.
   - Tailscale, connected: `tailscale status`. MagicDNS and HTTPS must be on for my tailnet, in its DNS settings; step 7 checks them.
   - Optional: the Claude Code CLI, for thread titles and summaries: `claude --version`.
2. Get the `flotilla` command into ~/.local/bin (make it if it's missing, and check it's on my PATH). If https://github.com/joseph-lozano/flotilla/releases/latest has downloads, unpack `flotilla-cli-linux-$(uname -m).tar.gz` as the release notes say. With no downloads, build it as "Install" says; that needs Rust and the packages it lists. Check with `flotilla --version`.
3. Ask me to run `sudo tailscale set --operator=$USER`.
4. The boat.dev key: give me the `boat api-key create "Flotilla server"` command from "Set up", to run wherever boat's CLI is signed in. Don't run it yourself.
5. Ask me to run `flotilla setup --tailscale-serve` in my own terminal here. It asks for the key, signs the GitHub CLI in, and offers `claude setup-token`. The pairing link it prints can wait: step 8 makes a new one once the server runs.
6. Run `flotilla service install`, then ask me to run `sudo loginctl enable-linger $USER`. Check with `flotilla service status`.
7. Run `flotilla doctor`. It exits 0 when no check fails. For each ✗, run the fix it gives, or ask me if it's mine to do.
8. Pairing: ask me to run `flotilla pair` here, then open its link on my Mac or Linux desktop and press Connect, or scan its QR code in the iPhone app. Check with `flotilla devices`.

When you're done, tell me the server's address (doctor's Tailscale line says it), and anything left for me to do.
```

## Install

Build the `flotilla` command and put it somewhere lasting:

```
git clone https://github.com/joseph-lozano/flotilla.git
cd flotilla
cargo build --release -p flotilla-cli
cp target/release/flotilla ~/.local/bin/
```

On Debian or Ubuntu, the build needs `build-essential cmake pkg-config`. For one static binary that runs on any x86-64 Linux, add `musl-tools`, run `rustup target add x86_64-unknown-linux-musl`, and build with `--target x86_64-unknown-linux-musl`.

The server also uses the [GitHub CLI](https://cli.github.com) for projects, and optionally the [Claude Code CLI](https://docs.claude.com/en/docs/claude-code) for titles and summaries and [Tailscale](https://tailscale.com/download) to reach it.

## Set up

Give the server a boat.dev key of its own, so you can tell its use apart and revoke it alone. Make it wherever boat's command-line tool is signed in:

```
boat api-key create "Flotilla server" --ttl 365d --actions sandbox.create,sandbox.read,sandbox.update,sandbox.stop,sandbox.resume,sandbox.fork,sandbox.delete-own,agent.prompt,exec,host,snapshot.read,snapshot.write,environment.read,environment.write,account.read
```

Then, on the server:

```
flotilla setup --tailscale-serve
```

`setup` asks for the boat.dev key, signs the GitHub CLI in, sets up titles and summaries with `claude setup-token`, checks everything, and prints a pairing link and a QR code. The key and the Claude token are kept in files only you can read.

To set up without questions, put the key in a file only you can read and pass it:

```
flotilla setup --non-interactive --boat-key-file ~/boat-key --tailscale-serve
```

Notifications to your iPhone go through Flotilla's push relay, encrypted so the relay can't read them. `--no-push-relay` turns that off. See [how they reach your phone](iphone.md#how-they-reach-your-phone).

## Run it as a service

On Linux, `flotilla service install` keeps the server running as a systemd user service, and starts it again if it fails. To start it at boot and keep it running after you log out, turn on lingering once:

```
flotilla service install
sudo loginctl enable-linger $USER
```

| Command | What it does |
| --- | --- |
| `flotilla service status` | Whether it's running, and its latest log lines. |
| `flotilla service restart` | Restart it, after `flotilla setup` or after updating `flotilla`. |
| `flotilla service stop`, `start` | Stop or start it. |
| `flotilla service uninstall` | Stop it and remove the service. Your data stays. |
| `journalctl --user -u flotilla -f` | Follow its log. |

The service finds `gh`, `claude` and `tailscale` where they were when you installed it. Run `flotilla service install` again after moving any of them.

## Check it

`flotilla doctor` lists what isn't set up and the command that fixes each. Every connected device shows the same checks in **Settings** under **Server setup**, with **Check again**.

| Check | What it looks at |
| --- | --- |
| boat API key | That boat.dev takes the key, that it has every permission Flotilla uses, and when it expires. |
| GitHub | That `gh` is installed, 2.81 or newer, signed in to github.com, with the `repo` scope. |
| Titles and previews | That the `claude` CLI is there and answers with the chosen model. *Off* if you turned titles off. |
| Tailscale | That Tailscale is connected, with MagicDNS and HTTPS on, and forwards the server's address to it. |
| Database | That Flotilla can write its database. |
| Push | How notifications reach iPhones: through Flotilla's push relay, your own Apple key, or off. See [how they reach your phone](iphone.md#how-they-reach-your-phone). |
| Remote MCP | Whether remote MCP is on, and public. See [remote MCP](agent-api.md#remote-mcp-for-assistants-in-other-clouds). |
| Other Flotillas | For information: VMs on the same boat.dev account that another Flotilla made. This one leaves them alone. |

## Pair your devices

Each device pairs with a one-time code. On the server, run:

```
flotilla pair
```

It prints a link and a QR code. Then:

- **On a Mac or Linux desktop**, open the link, check the address, and press **Connect**. Or pick **Flotilla → Connect to a Server…** (on Linux, **Settings → Server → Connect to a server…**) and paste the link or type the address and code.
- **On your iPhone**, scan the QR code in the app. See [The iPhone app](iphone.md#pair-with-your-server).

A code works once, for 5 minutes. `flotilla pair --ttl 2h` gives it longer, up to `24h`. `flotilla pair --list` shows codes not used yet, and `--revoke` cancels one. From any paired device, **Add device** in **Settings → Devices** makes a code too, with its QR code and link.

### Manage devices

Each device gets a token of its own, kept in its Keychain on a Mac or iPhone, or in a file only you can read on Linux. **Settings → Devices** lists every device, connected or when it was last seen, with **Rename** and **Revoke**. Revoking signs a device out at once; it has to pair again to connect. On the server, `flotilla devices` lists them and `flotilla devices revoke <id>` signs one out.

### One server per app

Each app pairs with one server. Connecting to another takes its place: the first server's token is deleted, and that server is asked to sign the device out.

On a Mac or Linux desktop, **Settings → Server** switches between the paired server and **This Mac** (**This computer** on Linux), the app's own server. Only one runs at a time, and what you were typing waits for you in each. **Forget** removes the pairing.

## Reach it over Tailscale

With Tailscale on the server and your devices, and MagicDNS and HTTPS turned on for your tailnet ([DNS settings](https://login.tailscale.com/admin/dns)), `flotilla setup --tailscale-serve` is all it takes. While the server runs, Tailscale forwards `https://box.tail1234.ts.net` (your machine's name on your tailnet) to it, and pairing links use that address. Only your tailnet can reach it.

- On Linux, let your user change Tailscale's settings once: `sudo tailscale set --operator=$USER`.
- Flotilla uses Tailscale's port 443 and nothing else, unless you publish [remote MCP](agent-api.md#remote-mcp-for-assistants-in-other-clouds) with Funnel. If something else already serves that port on the machine, Flotilla leaves it alone and says so.

## Reach it from the internet with Caddy

On a public server, put [Caddy](https://caddyserver.com) in front for HTTPS, and tell Flotilla its public address:

```
flotilla setup --no-tailscale-serve
flotilla service install --url https://flotilla.example.com
```

`/etc/caddy/Caddyfile`:

```
flotilla.example.com {
	request_body {
		max_size 64MiB
	}
	reverse_proxy 127.0.0.1:7777
}
```

Caddy gets a certificate and passes the app's live connection through on its own. `max_size` is the largest upload Flotilla takes. Only paired devices get in: pairing codes work once, for a short time, and wrong ones are limited for each address.

## Run it in Docker

The repo has a Dockerfile for the server:

```
docker build -t flotilla .
docker volume create flotilla
docker run -it --rm -v flotilla:/data flotilla setup --no-tailscale-serve
docker run -d --name flotilla --restart unless-stopped -v flotilla:/data -p 127.0.0.1:7777:7777 flotilla serve --host 0.0.0.0 --insecure --url https://flotilla.example.com
docker exec flotilla flotilla pair
```

Everything the server keeps is in the `flotilla` volume, none of it in the image. Publishing the port on `127.0.0.1` keeps it on the machine; put Caddy or Tailscale in front. The image has `gh` but not the `claude` CLI: for titles and summaries, build with `--build-arg CLAUDE=1`, or turn them off in Settings. `docker exec flotilla flotilla doctor` checks the setup.

## Use your Mac as the server

Without a separate machine, other devices can connect to the Mac that has your threads, while Flotilla is open there. This is a preview.

1. On that Mac, open **Settings → Devices** and turn on **Let other devices connect**.
2. To reach it from your tailnet, also turn on **Over Tailscale**.
3. Press **Add device** for a pairing code, link and QR code.

When the Mac sleeps or Flotilla quits, the other devices can't reach it.

## Move your Mac's threads to the server

Your Mac's threads, what you typed, what's unread, projects and settings can move to the server:

1. Connect the Mac to the server (**Flotilla → Connect to a Server…**), so it opens on the server from now on. Then quit Flotilla.

2. Copy its data to the server:

   ```
   cd ~/Library/Application\ Support/Flotilla
   ssh server mkdir -p mac && scp -r flotilla.db blobs server:mac/
   ```

3. On the server:

   ```
   flotilla service stop
   flotilla import ~/mac/flotilla.db
   flotilla service start
   ```

4. Open Flotilla on the Mac. The threads are there, on the server.

The server takes over the VMs those threads and projects run on. **This Mac** won't run them again, and says where they went. Devices paired with the Mac pair again with the server. If the server already has threads of its own, `--replace` puts them aside first.

boat.dev lets a key delete only the VMs it made, so the server's key can't delete the Mac's. Deleting one of those threads on the server says so: delete its VM in boat's dashboard, or with the Mac's key, then delete the thread again.

## Where the server keeps its data

In `~/.local/share/Flotilla`, or `$XDG_DATA_HOME/Flotilla`. Pass `--data-dir` with every command to keep it elsewhere. Back up `flotilla.db` now and then: once a thread's VM is gone, its history exists nowhere else. This is safe while the server runs:

```
sqlite3 flotilla.db ".backup flotilla-backup.db"
```

The app on your Mac and a server can share one boat.dev account. Each keeps to the VMs it made, so a hand-off or a setup never happens twice.
