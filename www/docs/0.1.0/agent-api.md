# The agent API

Other assistants can use Flotilla too. Claude Code, OpenClaw, Hermes Agent and other MCP clients can start threads, follow them, send follow-ups and answer their agents' questions, with `flotilla mcp`. Assistants in other clouds, such as Grok and Muse, use remote MCP.

## What an assistant can do

Each assistant connects as an **agent device**, with less power than your own devices.

| It can | It can't |
| --- | --- |
| Read and search threads, see projects and models, start threads, send follow-ups and steers, answer agents' questions, interrupt a turn, archive a thread, and open private previews. | Delete threads, merge pull requests, send files, make a port public or private, change settings, keys or projects, or pair and revoke other devices. Its reading a thread doesn't mark it read. |

It shows in **Settings → Devices** as an *Agent*, where **Revoke** signs it out at once.

## On the machine that runs Flotilla

Where Flotilla runs, the server or the Mac with the app open, add `flotilla mcp` to the assistant as it is. For Claude Code:

```
claude mcp add flotilla -- flotilla mcp
```

## From another machine

Pair the assistant's machine with the server as an agent device first.

1. On the server, make a code that pairs only an agent device:

   ```
   flotilla pair --agent
   ```

   Or use **Add device** in **Settings → Devices** on any paired device. Either way, what `flotilla connect` pairs is an agent device; a code from `pair --agent` can't pair anything more.

2. Where the assistant runs, with `flotilla` installed there too:

   ```
   flotilla connect https://box.tail1234.ts.net
   ```

   It asks for the code. `--name` sets what Settings calls the device.

3. Add the server to the assistant:

   | Assistant | Setup |
   | --- | --- |
   | Claude Code | `claude mcp add flotilla -- flotilla mcp --server https://box.tail1234.ts.net`, plus `--scope user` for every project. |
   | OpenClaw | `openclaw mcp add flotilla --command flotilla --arg mcp --arg --server --arg https://box.tail1234.ts.net` |
   | Hermes Agent | In `~/.hermes/config.yaml`, under `mcp_servers:`, add `flotilla:` with `command: "flotilla"` and `args: ["mcp", "--server", "https://box.tail1234.ts.net"]`. |

If the assistant can't find `flotilla`, use its full path; `flotilla connect` prints the command with it. `flotilla disconnect https://box.tail1234.ts.net` signs the device out and forgets its token. The CLI's own thread commands take `--server` too, as the same agent device.

## The tools

Each assistant gets these twelve tools.

| Tool | What it does |
| --- | --- |
| `list_threads` | The threads, a line each; with `query`, the ones whose titles or messages match. |
| `get_thread` | A thread's state, model, project, branch, pull request, preview links, and any question it's waiting on. |
| `read_thread` | Its messages, newest last, with tool calls folded. `before` pages back, and `seq` gives one message whole. |
| `list_projects`, `list_models` | What a thread can start in, and on. |
| `start_thread` | A new thread on a model, in a project or a blank VM. |
| `send_message` | A follow-up, or a steer while a turn runs. |
| `wait_for_turn` | Waits for the turn to end, 45 seconds unless told otherwise and at most 10 minutes, then gives its answer. |
| `answer_question` | Answers the questions an agent is waiting on. |
| `interrupt_turn`, `archive_thread`, `open_preview` | Stops the turn, puts the thread away, or gives a private link to a port in its VM. |

## Remote MCP, for assistants in other clouds

Grok Bot, grok.com and Muse run in their own clouds, so they can't run `flotilla mcp` or reach your tailnet. Remote MCP gives them the same tools, as the same kind of agent device, over HTTPS.

It's **off unless you turn it on**, and it runs on a [server](server.md), not on This Mac. When it's on, it's a listener of its own (port 8787 unless you change it) that serves only MCP and its sign-in page, so you publish that port alone and the rest of the server stays private.

### Turn it on with Tailscale Funnel

For a server on your tailnet:

```
flotilla setup --mcp-funnel
flotilla service restart
```

While the server runs, Funnel publishes remote MCP alone at `https://box.tail1234.ts.net:8443/mcp` (or port 10000 if 8443 is taken). The server itself stays private on 443. Funnel needs:

- HTTPS on for your tailnet ([DNS settings](https://login.tailscale.com/admin/dns));

- the `funnel` attribute for the machine in your tailnet's policy file:

  ```
  "nodeAttrs": [{ "target": ["autogroup:member"], "attr": ["funnel"] }]
  ```

- on Linux, your user allowed to change Tailscale's settings: `sudo tailscale set --operator=$USER`.

`flotilla doctor` says whether it's public, and if not, what to change.

### Or publish it another way

Behind a Cloudflare Tunnel, exe.dev or any HTTPS proxy, tell Flotilla the public address and point the proxy at port 8787:

```
flotilla setup --mcp --mcp-url https://mcp.example.com
flotilla service restart
```

With Cloudflare, use a named tunnel, not a quick tunnel: quick tunnels don't carry the event streams MCP answers with. If the proxy can't reach `127.0.0.1`, add `--mcp-host 0.0.0.0`. `--mcp-port` changes the port, and `flotilla setup --no-mcp` turns remote MCP off.

### Connect an assistant

Each assistant gets an agent device of its own. There are two ways in.

**Sign in from the assistant.** Grok Bot, grok.com and Muse need this. Add the address; the assistant opens Flotilla's sign-in page, which asks for a pairing code. Make one with **Add device** in **Settings → Devices**, or `flotilla pair --agent` on the server, type it, and press **Allow**.

**Or give it a token.** For a client that sends a header, such as Muse Code or Claude Code:

```
flotilla pair --agent --token --name "Muse Code"
```

This prints the token once. The client sends it as `Authorization: Bearer <token>`.

| Assistant | Setup |
| --- | --- |
| Grok Bot | In a chat with your Bot: *Add the MCP server https://box.tail1234.ts.net:8443/mcp and call it Flotilla*. Press **Authenticate** on the card it shows, then sign in. |
| grok.com | [grok.com/connectors](https://grok.com/connectors) → **New Connector** → **Custom**. Paste the address, then sign in. |
| Muse | **Settings → Connectors → Add custom connector**, or tell Muse: *Create a custom connector for the MCP server at https://box.tail1234.ts.net:8443/mcp (streamable HTTP, OAuth)*. Then sign in. |
| Muse Code | In `~/.config/muse/settings.json`, under `"mcpServers"`: `"flotilla": {"type": "streamable-http", "url": "https://box.tail1234.ts.net:8443/mcp", "headers": {"Authorization": "Bearer <token>"}, "tool_timeout_sec": 600}`. Or leave out `headers` and run `muse mcp login flotilla`. Start a new session after. |
| Claude Code | `claude mcp add --transport http flotilla https://box.tail1234.ts.net:8443/mcp --header "Authorization: Bearer <token>"`. Or leave out `--header` and sign in from `/mcp`. |

Over remote MCP, `wait_for_turn` waits at most 100 seconds, under the two minutes Grok Bot gives a call; it says to call again while the thread works.

### What faces the internet

Remote MCP is the one part of Flotilla you can put on the internet, so it keeps strangers out:

- Only agent devices' tokens get in, checked on every request.
- Wrong tokens, pairing codes, sign-ins and each device's requests are limited for each address, and in all.
- Requests from web pages are refused.
- A sign-in that isn't finished within two minutes revokes the device it made.
