# Run Flotilla on a server

> The docs on the website cover this too: [Run a server](https://useflotilla.com/docs/main/server.html) and [The agent API](https://useflotilla.com/docs/main/agent-api.html).

Agents run in their boat.dev VMs, so a turn keeps going with your laptop shut. Flotilla itself runs inside the app while it's open. Put it on a machine that's always on, and the rest keeps going too: hand-offs happen, parents wake, setup runs, schedules send, and your phone stays connected. Your Macs and Linux desktops (and later your phone) connect to it, and all of them see the same threads, unread and settings.

A server needs very little: outbound HTTPS to boat.dev and GitHub, a few MB of memory, and one SQLite database. Any Linux box will do. It listens on `127.0.0.1` only; reach it through [Tailscale](#reach-it-over-tailscale) (the easy way) or [Caddy](#reach-it-from-the-internet-with-caddy). Assistants in other clouds can use it through [remote MCP](#let-assistants-in-other-clouds-use-it-remote-mcp), if you turn it on.

## Install

Download the `flotilla` command into `~/.local/bin`. It's linked statically, so it runs on any Linux:

```sh
mkdir -p ~/.local/bin
curl -fL https://github.com/joseph-lozano/flotilla/releases/latest/download/flotilla-cli-linux-$(uname -m).tar.gz |
  tar -xz --strip-components=1 -C ~/.local/bin flotilla-cli-linux-$(uname -m)/flotilla
```

Or build it from source, and put it somewhere lasting:

```sh
git clone https://github.com/joseph-lozano/flotilla.git
cd flotilla
cargo build --release -p flotilla-cli
cp target/release/flotilla ~/.local/bin/
```

On Debian or Ubuntu, building needs `build-essential cmake pkg-config`. For one static binary that runs on any x86-64 Linux, add `musl-tools`, run `rustup target add x86_64-unknown-linux-musl`, and build with `--target x86_64-unknown-linux-musl`.

The server also uses:

- the [GitHub CLI](https://cli.github.com), for projects and pull requests;
- the [Claude Code CLI](https://docs.claude.com/en/docs/claude-code), optional, for thread titles and summaries;
- [Tailscale](https://tailscale.com/download), optional, to reach it from your devices.

## Set up

Give the server a boat.dev key of its own, so you can tell its use apart and revoke it alone. On any machine where the `boat` CLI is signed in:

```sh
boat api-key create "Flotilla server" --ttl 365d --actions sandbox.create,sandbox.read,sandbox.update,sandbox.stop,sandbox.resume,sandbox.fork,sandbox.delete-own,agent.prompt,exec,host,snapshot.read,snapshot.write,environment.read,environment.write,account.read
```

Then, on the server:

```sh
flotilla setup --tailscale-serve
```

It asks for the boat key, signs gh in (`gh auth login`), sets up titles and summaries with `claude setup-token`, and checks everything. The key and the Claude token are kept in `~/.local/share/Flotilla/secrets/`, readable only by you.

To set up without questions, put the key in a file only you can read, and pass it:

```sh
flotilla setup --non-interactive --boat-key-file ~/boat-key --tailscale-serve
```

`flotilla doctor` says what isn't set up, and what to run to fix it. Every device's **Settings → Server setup** shows the same checks.

### Notifications to iPhones

Only Flotilla's makers can send notifications to the Flotilla iPhone app, so a server without their APNs key sends them through Flotilla's push relay, `https://push.useflotilla.com`, which runs on Cloudflare. There's nothing to set up: the server tells the phone it uses the relay, the phone registers with it, and gives the server what it got with a key of its own.

- **What the relay keeps.** Your phone's notification token, a random id with a hashed secret, which app and APNs environment it's for, and when it was registered and last used. Each notification is encrypted with the phone's key, which the relay never has, so it can't read titles, threads or anything else. It keeps no addresses and no logs.
- **When it deletes them.** When you unpair (forget the server, or pair with another), when Apple reports the app gone (on the next notification after it's deleted), or after 90 days unused. Cloudflare can restore deleted data for 30 days.
- **Off.** `flotilla setup --no-push-relay`, then restart the server. It never sends the relay anything, and the phone gets no notifications. `--push-relay` turns it back on; `--push-relay https://push.example.com` uses another relay (the code is in `relay/`). `FLOTILLA_PUSH_RELAY` (a URL, or `off`) overrides both.
- **Your own key.** A server with the app's own APNs key (`flotilla setup --apns-key AuthKey_<KEY ID>.p8 --apns-team-id <TEAM ID>`) sends to Apple directly and never uses the relay. That only works for an app built under the key's Apple team.

The doctor's **Push** line says which: `Through the relay at push.useflotilla.com`, `Own key <KEY ID> · team <TEAM ID>`, or `Off`.

## Run it as a service

```sh
flotilla service install
```

This writes a systemd user service, `flotilla.service`, and starts it. It restarts if it fails, and finds gh, claude and tailscale where they were when you installed it: run `install` again after moving any of them. To start it at boot and keep it running after you log out, turn on lingering once:

```sh
sudo loginctl enable-linger $USER
```

| Command | |
| - | - |
| `flotilla service status` | Whether it's running, and its latest log lines |
| `flotilla service restart` | After `flotilla setup`, or updating `flotilla` |
| `flotilla service stop`, `start` | |
| `flotilla service uninstall` | Stops it and removes the service. Your data stays. |
| `journalctl --user -u flotilla -f` | Follow its log |

Flotilla keeps its data in `~/.local/share/Flotilla` (or `$XDG_DATA_HOME/Flotilla`). Use `--data-dir` with every command to keep it elsewhere.

Back up `flotilla.db` now and then: the history of a deleted VM's thread exists nowhere else. `sqlite3 flotilla.db ".backup flotilla-backup.db"` is safe while it runs.

## Pair your devices

```sh
flotilla pair
```

prints a link and a QR code. Open the link on your Mac or [Linux desktop](#the-linux-desktop-app), or use **Flotilla → Connect to a Server…** (on Linux, **Settings → Server → Connect to a server…**) and type the address and code. A code works once, for 5 minutes; `--ttl 2h` (up to `24h`) gives it longer. `flotilla pair --list` and `--revoke` manage codes not used yet.

Each app pairs with one server. Connecting it to another takes the first one's place: its token is deleted, and the first server is asked to sign the device out. On a Mac or Linux desktop, **Settings → Server** switches between that server and **This Mac**.

Each device gets its own token. `flotilla devices` lists them, and `flotilla devices revoke <id>` signs one out at once. **Settings → Devices** does the same from any paired device, and **Add device** makes a code there.

## Scheduled prompts

The server sends prompts later: once, or at most once a day: daily, on weekdays, weekly or on a cron expression with one time of day, in the time zone of the device that set them. Each run goes to one thread as a follow-up, or starts a new thread. Use **Send later** (the clock by Send) in either app. The same works from the server:

```sh
flotilla schedule add --thread 7f3e --daily 09:00 "Check the overnight CI runs"
flotilla schedule add --provider claude-code --model claude-haiku-5-5 --weekdays 08:30 "Triage new bugs"
flotilla schedule list
flotilla schedule rm 2a87
```

Times that pass while the server is off are skipped to the next one. A one-off goes late instead, as does an every-24-hours schedule set before runs had to be a day apart. A run that needs a VM start waits while boat has fewer than 5 starts left this hour. Flotilla notes that run as not run and notifies you once. On a Mac without a server, schedules run only while the app is open.

## Let other agents use it

Claude Code, OpenClaw, Hermes Agent and other assistants can start threads, read them, send follow-ups, wait for a turn to end and answer an agent's questions, through `flotilla mcp`, an MCP server. Each one connects as an **agent device**: the server lets it read and search threads, see projects and models, start threads, send, answer, interrupt, archive and open previews, and nothing else. It can't delete threads, merge pull requests, change settings, keys or projects, or pair and revoke devices, and its reading a thread doesn't mark it read. It shows in **Settings → Devices** as an Agent, where Revoke signs it out.

On the machine that runs Flotilla (`flotilla serve`, or the app), add it to the assistant as it is:

```sh
claude mcp add flotilla -- flotilla mcp
```

From another machine, pair it with the server first. On the server, make a code that pairs only an agent (or use **Add device** in Settings on any paired device):

```sh
flotilla pair --agent
```

Then, where the assistant runs (with `flotilla` installed there too):

```sh
flotilla connect https://box.tail1234.ts.net
```

It asks for the code and keeps the device's token in the login Keychain on a Mac, or in `secrets/` in the data directory elsewhere (`--secrets-dir` to keep it elsewhere). Then add the server to the assistant:

| Assistant | |
| - | - |
| Claude Code | `claude mcp add flotilla -- flotilla mcp --server https://box.tail1234.ts.net` (add `--scope user` for every project) |
| OpenClaw | `openclaw mcp add flotilla --command flotilla --arg mcp --arg --server --arg https://box.tail1234.ts.net` |
| Hermes Agent | in `~/.hermes/config.yaml`, under `mcp_servers:` add `flotilla:` with `command: "flotilla"` and `args: ["mcp", "--server", "https://box.tail1234.ts.net"]` |

Use `flotilla`'s full path if the assistant's PATH doesn't have it (`flotilla connect` prints the command with it). The tools:

| Tool | |
| - | - |
| `list_threads` | The threads, a line each; with `query`, the ones whose titles or messages match |
| `get_thread` | A thread's status, model, project, branch, pull request, preview links, and a question it's waiting on |
| `read_thread` | Its messages, newest last, tool calls folded; `before` pages back, `seq` gives one message whole |
| `list_projects`, `list_models` | What a thread can start in, and on |
| `start_thread` | A new thread on a model, in a project or a blank VM |
| `send_message` | A follow-up, or a steer while a turn runs |
| `wait_for_turn` | Waits for the turn to end (45 s unless told, at most 10 minutes), then gives its answer |
| `answer_question` | Answers the questions an agent is waiting on |
| `interrupt_turn`, `archive_thread`, `open_preview` | Stop the turn, put the thread away, a private link to a port in its VM |

`flotilla disconnect https://box.tail1234.ts.net` signs the device out and forgets its token. `--server` works for the CLI's own thread commands too (`flotilla --server https://box.tail1234.ts.net threads`), as the same agent device. [design/agent-api.md](../design/agent-api.md) has why it works this way.

## Let assistants in other clouds use it (remote MCP)

Grok Bot, grok.com and Muse run in their own clouds: they can't run `flotilla mcp` or reach your tailnet. Remote MCP gives them the same tools, as the same kind of agent device, over HTTPS. It's **off unless you turn it on**. With it off there's no listener and no public port, and nothing else changes.

When it's on, it's a listener of its own (port 8787 unless you say otherwise) that serves only MCP and the sign-in page. Publish that port alone; the rest of the server stays on your tailnet.

### Turn it on, and publish it

**With Tailscale Funnel** (a box at home, on your tailnet):

```sh
flotilla setup --mcp-funnel
flotilla service restart
```

`flotilla serve` publishes only remote MCP with Funnel as it starts, at `https://box.tail1234.ts.net:8443/mcp` (or `:10000` if 8443 is something else's), and takes it down as it stops. The server itself stays private on 443. Funnel needs:
- HTTPS on for your tailnet ([DNS settings](https://login.tailscale.com/admin/dns));
- the `funnel` attribute for this machine in the [tailnet policy file](https://login.tailscale.com/admin/acls/file):
  ```json
  "nodeAttrs": [{ "target": ["autogroup:member"], "attr": ["funnel"] }]
  ```
- on Linux, your user allowed to change Tailscale's settings once: `sudo tailscale set --operator=$USER`.

`flotilla doctor` says whether it's public, and if not, what to change.

**On exe.dev**, where one port of a VM can be public:

```sh
flotilla setup --mcp --mcp-url https://box.exe.xyz
flotilla service restart
ssh exe.dev share port box 8787
ssh exe.dev share set-public box
```

The VM's other ports need an exe.dev login, so the main API stays private. If exe.dev's proxy can't reach `127.0.0.1` on the VM, add `--mcp-host 0.0.0.0`. See [exe.dev's proxy](https://exe.dev/docs/proxy.md).

**With a Cloudflare Tunnel**, or any HTTPS proxy in front:

```sh
flotilla setup --mcp --mcp-url https://mcp.example.com
cloudflared tunnel create flotilla-mcp
cloudflared tunnel route dns flotilla-mcp mcp.example.com
cloudflared tunnel run --url http://127.0.0.1:8787 flotilla-mcp
```

Use a named tunnel: xAI says quick tunnels (`trycloudflare.com`) don't carry the event streams MCP answers with.

`flotilla setup` on a terminal offers remote MCP too (No unless you say yes). `--mcp-port` changes the port, and `--no-mcp` turns it off.

### Connect an assistant

Each assistant gets an agent device of its own, as `flotilla connect` makes for `flotilla mcp`. It can do what an agent may and no more, it shows in **Settings → Devices**, and **Revoke** there signs it out at once. There are two ways in.

**Sign in from the assistant** (Grok Bot, grok.com and Muse need this): add the address. The assistant opens Flotilla's sign-in page, which asks for a pairing code. Make one with **Add device** in **Settings → Devices**, or `flotilla pair --agent` on the server. Type it and press **Allow**. The device is named for the assistant.

**Or give it a token** (Muse Code, Claude Code, any client that sends a header):

```sh
flotilla pair --agent --token --name "Muse Code"
```

This prints the token once. Send it as `Authorization: Bearer <token>`.

| Assistant | |
| - | - |
| Grok Bot | In a chat with your Bot: "Add the MCP server https://box.tail1234.ts.net:8443/mcp and call it Flotilla". Press **Authenticate** on the card it shows, then sign in as above. |
| grok.com | [grok.com/connectors](https://grok.com/connectors) → **New Connector** → **Custom**. Paste `https://box.tail1234.ts.net:8443/mcp`, then sign in as above. |
| Muse | **Settings → Connectors → Add custom connector**, or tell Muse: "Create a custom connector for the MCP server at https://box.tail1234.ts.net:8443/mcp (streamable HTTP, OAuth)". Then sign in as above. |
| Muse Code | In `~/.config/muse/settings.json`, under `"mcpServers"`: `"flotilla": {"type": "streamable-http", "url": "https://box.tail1234.ts.net:8443/mcp", "headers": {"Authorization": "Bearer <token>"}, "tool_timeout_sec": 600}`. Or leave out `headers` and run `muse mcp login flotilla`. Start a new session after. |
| Claude Code | `claude mcp add --transport http flotilla https://box.tail1234.ts.net:8443/mcp --header "Authorization: Bearer <token>"`. Or leave out `--header` and sign in from `/mcp`. |

The tools are the twelve above. `wait_for_turn` waits at most 100 seconds for an assistant that takes no progress, under the two minutes Grok Bot gives a call; it says to call again while the thread works.

What faces the internet:
- Only agent devices' tokens get in, checked on every request.
- Wrong tokens, pairing codes, sign-ins and each device's requests are limited for each address, and in all.
- Requests from web pages are refused.
- A sign-in that isn't finished within two minutes revokes the device it made, even if the server stops or restarts meanwhile.

## The Linux desktop app

The app you use on a Mac runs on Linux desktops too, on X11 or Wayland. It runs threads itself while it's open (**This computer**), or connects to a server. Download it for Debian, Ubuntu and their relatives:

```sh
curl -fLO https://github.com/joseph-lozano/flotilla/releases/latest/download/Flotilla-$(dpkg --print-architecture).deb
sudo apt install ./Flotilla-$(dpkg --print-architecture).deb
```

On any other distribution, use the AppImage from the [latest release](https://github.com/joseph-lozano/flotilla/releases/latest): `chmod +x` it and run it. Or build the `.deb` yourself:

```sh
sudo apt install build-essential cmake pkg-config libfontconfig-dev libxkbcommon-x11-dev dpkg-dev imagemagick
script/bundle-linux
sudo apt install ./target/release/flotilla_0.1.0_$(dpkg --print-architecture).deb
```

This installs `flotilla-app`, its icon, and `com.useflotilla.flotilla.desktop`, which opens `flotilla://` pairing links. apt brings what it needs to run: Vulkan or EGL from your graphics driver, `xdg-utils` and `desktop-file-utils`. `script/bundle-linux --dev` builds Flotilla Dev, which installs beside it.

**Pair it** with a link from `flotilla pair`: open it on the Linux machine (or run `xdg-open '<link>'`), check the address, and press Connect. The app keeps its token in `~/.config/Flotilla/flotilla-device/`, readable only by you.

**This computer** keeps its threads in `~/.local/share/Flotilla`, and the boat key you paste in a file there only you can read. That's also where a server on the same machine keeps its data, and only one Flotilla can use a database at a time: on a machine running `flotilla service`, connect the app to that server instead.

- Ctrl takes ⌘'s place in shortcuts, and F9 shows or hides the sidebar. There's no menu bar: everything in the Mac's menus is in **Settings**.
- File pickers (attaching files, adding skills) and following the desktop's light or dark style come through the desktop portal, `xdg-desktop-portal`, which GNOME and KDE run. Without it, pick Light or Dark in **Settings → Appearance**.
- Opening Flotilla again brings its window forward, and a link opened while it runs goes to that window.

## Reach it over Tailscale

With Tailscale on the server and your devices, and HTTPS turned on for your tailnet ([DNS settings](https://login.tailscale.com/admin/dns)), `flotilla setup --tailscale-serve` is all it takes. The server forwards `https://<machine>.<tailnet>.ts.net` to itself while it runs, and pairing links use that address. Only your tailnet can reach it.

Flotilla uses Tailscale's port 443 and nothing else, unless you publish remote MCP with Funnel (8443 or 10000). If something else already serves those ports on that machine, Flotilla leaves it alone and says so. When it stops, it removes only its own forwards.

On Linux, let your user change Tailscale's settings once: `sudo tailscale set --operator=$USER`.

## Reach it from the internet with Caddy

On a public server, put [Caddy](https://caddyserver.com) in front for HTTPS, and tell Flotilla its public address:

```sh
flotilla setup --no-tailscale-serve
flotilla service install --url https://flotilla.example.com
```

```caddyfile
# /etc/caddy/Caddyfile
flotilla.example.com {
	request_body {
		max_size 64MiB
	}
	reverse_proxy 127.0.0.1:7777
}
```

Caddy gets a certificate and passes WebSockets through on its own. `max_size` is the largest file Flotilla takes; Caddy refuses anything bigger before it reaches the server. Only paired devices get in: pairing codes are single use and short lived, and wrong ones are limited for each address.

## Docker

```sh
docker build -t flotilla .
docker volume create flotilla
docker run -it --rm -v flotilla:/data flotilla setup --no-tailscale-serve
docker run -d --name flotilla --restart unless-stopped -v flotilla:/data -p 127.0.0.1:7777:7777 flotilla serve --host 0.0.0.0 --insecure --url https://flotilla.example.com
docker exec flotilla flotilla pair
```

Everything the server keeps is in the `flotilla` volume: the database, uploaded files, the boat key, the Claude token, and gh's sign-in. None of it is in the image. `--insecure` is because the server listens on the container's own network; publishing it on `127.0.0.1` keeps it on this machine. Put Caddy (as above) or Tailscale in front: with Tailscale on the host, run `tailscale serve --bg --https=443 http://127.0.0.1:7777` and pass `--url https://<machine>.<tailnet>.ts.net`.

The image has gh but not the claude CLI. For titles and summaries, build it with `--build-arg CLAUDE=1`, or turn them off in **Settings**.

`docker exec flotilla flotilla doctor` checks the setup.

## Move your Mac's threads to the server

Your Mac's threads, what you typed, unread, projects and settings can move to the server:

1. Connect the Mac to the server first (**Flotilla → Connect to a Server…**), so it opens on the server from now on, then quit Flotilla.
2. Copy its data to the server:
   ```sh
   cd ~/Library/Application\ Support/Flotilla
   ssh server mkdir -p mac && scp -r flotilla.db blobs server:mac/
   ```
3. On the server:
   ```sh
   flotilla service stop
   flotilla import ~/mac/flotilla.db
   flotilla service start
   ```
4. Open Flotilla on the Mac: the threads are there, on the server.

The server takes over the VMs those threads and projects run on, and the Mac learns that the first time it connects: **This Mac** won't run them again, and says where they went. Devices paired with the Mac pair again with the server. If the server already has threads of its own, `--replace` puts them aside first.

On a machine that runs the server itself, the app finds it holding its data and offers **Connect to it**: no pairing needed.

A boat key deletes only the VMs it made, so the server's key can't delete the Mac's. Deleting one of those threads on the server says so: delete its VM in boat's dashboard, or with the Mac's key, then delete the thread again.

## More than one Flotilla on one boat account

The app on your Mac and a server can share a boat.dev account. Each one keeps to the VMs it made, so a hand-off or a setup run never happens twice. The other's VMs show in **Settings → Server setup** as "Other Flotillas", for information.
