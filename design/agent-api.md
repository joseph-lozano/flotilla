# Flotilla for other agents: `flotilla mcp`, and remote MCP

Joseph, 2026-11-02: "one more feature that the remote server could do is an MCP or some other kind of API … for Claude Code, or another personal assistant, like OpenClaw, or Hermes, to be able to kick off threads, read threads, et cetera. It doesn't need complete control." Then: "it needs to work with grokbot and muse", "this must work just as well on a private home Linux box on Tailscale, not only on exe.dev", and "remote MCP must be strictly optional".

## What the cloud assistants need (vendor docs and reports, 2026-11-01)

Grok Bot and Muse run in their providers' clouds, so they can't run `flotilla mcp` (it would run on their machine, with neither the binary nor the tailnet) and can't reach a tailnet. They take a remote MCP server at a public `https` URL.

| Client | Transport | Auth | Redirect URIs | Public address? |
| - | - | - | - | - |
| **Grok Bot**, custom MCP servers ([xAI: computer and apps](https://docs.x.ai/grok-bot/computer-and-apps); [Cursor forum, Sep–Oct 2026](https://forum.cursor.com/t/grok-bot-custom-mcp-oauth-fails-before-sign-in-redirect-uri-not-allowed/171877); [wiki](https://www.grokbotwiki.com/guides/grok-bot-plugins-mcp)) | Streamable HTTP (or SSE) at an `https` URL. No settings form: you ask the Bot to add the server. Calls wait up to 2 minutes, longer while the server sends progress. On reused sessions it may omit `MCP-Protocol-Version`. | OAuth 2.1 with PKCE and dynamic client registration; no field for a static client id. `AddMcpServer.headers` exists, but staff say "Connectors in Grok Bot authenticate only via OAuth" and there's "nowhere to enter a secret there safely". xAI's docs: API keys go in a masked secret card. | Registers `cursor://anysphere.cursor-mcp/oauth/callback`, `https://www.cursor.com/agents/mcp/oauth/callback`, `http://localhost:8787/callback`. Cursor's client metadata (`https://cursor.com/oauth/mcp-client.json`) adds `https://www.cursor.com/bot/mcp/oauth/callback` and `https://grok.com/connectors-oauth-exchange-code/`. | Yes. The Bot's computer is in the cloud; OAuth discovery runs from Cursor's servers. |
| **grok.com** custom connectors ([xAI: connectors](https://docs.x.ai/grok/connectors)) | Streamable HTTP. xAI's tunnel guide says Cloudflare quick tunnels don't carry SSE. | OAuth ("complete any required authentication"), with dynamic registration ([report](https://github.com/alexanderkrauck/mailindex-mcp/pull/8)). | `https://grok.com/connectors-oauth-exchange-code/` (reported; xAI doesn't publish it). | Yes: "must be reachable over the public internet". Private addresses are refused. |
| **Muse** app, custom connectors ([Meta Help Center](https://www.meta.com/help/artificial-intelligence/1687253048996149/)) | Streamable HTTP. | OAuth 2.1 with PKCE. Meta documents only "ask Muse to create a Custom Connector". Reports: dynamic registration ([kagura-ai#2487](https://github.com/kagura-ai/memory-cloud/issues/2487)), or a client id typed in (a client-metadata URL, [vercel-mcp#239](https://github.com/Robinson-AI-Systems/vercel-mcp/pull/239)). No static header. It reportedly asks for no `offline_access`, so tokens that expire in an hour stop working. | `https://muse.ai/api/hatch/oauth/callback` and `https://platform.muse.ai/oauth/credential-test/callback`; earlier `https://agent.meta.ai/api/hatch/oauth/callback`. | Yes: it runs in Meta's cloud. |
| **Muse Code** ([Muse Code SDK: MCP servers](https://meta-models.github.io/muse-code-sdk/next/guides/extend/mcp-servers/)) | `"type": "streamable-http"` with `url` and `headers` under `mcpServers` in `~/.config/muse/settings.json` (or stdio). `tool_timeout_sec` defaults to 300. | A static `Authorization` header (literal: `${VAR}` isn't expanded; it skips OAuth), or OAuth with `muse mcp login` (dynamic registration, or `--oauth-client-id`). | A loopback port: `http://127.0.0.1:<port>`. | No: it runs on your machine. On the tailnet, `flotilla mcp` (stdio) works too. |

So **three of the four need OAuth** (Grok Bot, grok.com, Muse), and Muse Code takes a header. Flotilla serves both: an agent device's token as a bearer header, and OAuth 2.1 for the rest. Claude Code, OpenClaw and Hermes take either.

## Remote MCP: the design

**A listener of its own, off unless it's turned on.** `flotilla setup --mcp` (or `--mcp-funnel`, `--mcp-url`), or `flotilla serve --mcp-port`, keeps a setting (`remote_mcp` in kv), and `flotilla serve` opens the listener only when it's there. With nothing kept there's no listener and no public port, and the apps, pairing, the private server and `flotilla mcp` work as they did. `flotilla setup` offers it on a terminal, No unless you say yes; `--no-mcp` turns it off.

**It serves MCP and OAuth, and nothing else.** `POST /mcp`, the OAuth metadata, `/register`, `/authorize` and `/token`. No WebSocket, pairing, uploads, history or health, so only this port is published and the main API stays on the tailnet (443 through `tailscale serve`).

**The same twelve tools, held to the same scope.** Each session is a `flotilla_mcp::Mcp`, as `flotilla mcp`'s is. A device's sessions share one connection to the server, made through a pipe inside the process with that device's token, so the server sees an agent device and holds it to the scope below. Revoking the device ends its sessions at once, and its next request is refused. Only agent devices' tokens are taken; a full device's is refused (403).

**Streamable HTTP** (2025-06-18, and 2025-03-26's clients):
- `initialize` starts a session (`Mcp-Session-Id`, bound to the device).
- A request with no session is 400; an unknown one, or another device's, is 404 (initialize again).
- A missing `MCP-Protocol-Version` is taken (Grok Bot's reused sessions); an unknown one is 400.
- A tool call answers as an event stream when the client takes one: progress, a keep-alive comment every 15 s for proxies, then the answer. Otherwise it answers as JSON.
- `DELETE` ends a session. `GET` is 405: nothing is sent unasked.
- Sessions idle for 30 minutes end.

**Long waits.** `wait_for_turn` waits at most 100 s for a call that asks for no progress, under Grok Bot's two minutes. With progress, it waits as long as asked (up to 600 s). A client that drops the connection cancels its call.

**OAuth 2.1, approved by a person with a pairing code.**
- Discovery: a 401 carries `WWW-Authenticate: Bearer resource_metadata=…` (RFC 9728). The protected resource and authorization server metadata (RFC 8414) are at the usual well-known paths.
- Registration (RFC 7591) keeps nothing. The client id is the client's name and redirect URIs, encoded, which is all dynamic registration lets anyone say anyway, so it survives restarts and needs no table. Redirects: `https`, `http` to the client's own machine (any port, RFC 8252), or an app's own scheme (`cursor://…`). Never a script, data, file or fragment.
- The authorization code grant, public clients, PKCE S256 only.
- `/authorize` shows "Allow Grok to use Flotilla?", what it can't do, where access goes (the redirect's host), and a field for a pairing code. Make the code with Add device in Settings, or `flotilla pair --agent`. The right code pairs an agent device named for the client, and the client gets a code (two minutes, once) for its token.
- The token is the device's: in Settings → Devices as an agent, revocable there. It doesn't expire (devices lapse after 90 days unused), so Muse's lack of `offline_access` doesn't matter.
- A refresh gives the same token back while the device is paired.
- A code that's never swapped, or swapped with the wrong verifier, client or redirect, revokes the device it made.

Approving in the Mac or iPhone app instead was considered. It needs a new request kind, a card in both apps, and a way to tie a browser page to a tap, while other threads are changing the apps' cards. The code is what pairing already uses, and works on every device.

**Hardening for the internet:**
- Rate limits for each address and in all: wrong tokens (30 a minute), pairing codes (shared with `POST /v1/pair`'s), registrations, token requests, page views. Each device gets 240 requests a minute and 8 calls in flight; 128 requests are in flight at most in all.
- Sessions are bounded: 16 a device, 256 in all, 32 devices connected.
- Bodies: 1 MiB for MCP, 16 KiB for OAuth forms.
- Tokens are hashed in the database and checked in constant time, as devices' are. Codes are kept by their hash, and the PKCE check is constant time.
- Nothing secret is logged. Tokens never go in a URL; only OAuth's one-time, PKCE-bound code does.
- No CORS. Requests from web pages are refused (403), except the approval page's own form.
- Responses are `no-store`, `nosniff` and `no-referrer`. The approval page can't be framed and runs no script.

**Publishing it.** Only this port, one of three ways:
- **Tailscale Funnel** (a home Linux box): `flotilla setup --mcp-funnel`. `flotilla serve` runs `tailscale funnel --bg --https=8443 http://127.0.0.1:8787` as it starts (10000 if 8443 is someone else's; never 443, which the private serve has), and takes it down as it stops. Ports someone else set up are left alone.
- **exe.dev**: one public port per VM. Make it the MCP port.
- **A Cloudflare Tunnel**, or any HTTPS proxy: `--mcp-url` says the public address.

**Doctor.** "Remote MCP" is one of the checks, on servers:
- Off, it says "Off": no fix, not counted, never a warning.
- On, it says whether it's public: "Public at https://box.tail1234.ts.net:8443/mcp".
- When it isn't public, it says why and what to run. That covers a tailnet whose policy doesn't allow Funnel for this machine (the admin console's link, and the `nodeAttrs` line), Funnel's port on the tailnet only, a port someone else has, and a public address that doesn't reach it.
- The setup's count (Mac, Linux, iPhone) counts only checks that are on, so an optional part that's off doesn't make it look unfinished.

Tests:
- `remote_mcp_speaks_streamable_http_to_agent_devices_only`: sessions, JSON and streams, errors, refusals, the scope, revocation.
- `oauth_signs_a_client_in_with_a_pairing_code_typed_by_a_person`: a stand-in client, end to end.
- `the_mcp_listener_serves_nothing_of_the_main_api`.
- `remote_mcp_listens_only_when_asked` and `serve_opens_remote_mcp_only_when_its_on`: off opens nothing.
- `remote_mcp_is_off_unless_asked_for` (setup) and `remote_mcp_is_optional_and_says_whether_its_public` (doctor).
- `funnel_publishes_only_remote_mcp` (a stand-in `tailscale`), and `optional_parts_that_are_off_leave_the_count_whole` (Rust and Swift).

## `flotilla mcp`: the decision

**A stdio MCP server in the `flotilla` binary, `flotilla mcp`, connecting as an agent device that the server holds to a limited scope.** It reaches this machine's Flotilla through its Unix socket, or a server elsewhere as a paired device (`flotilla connect`). Remote MCP, above, carries the same tools over HTTP for clients that can't run it.

### What the assistants take (their docs, 2026-11-01)

| Assistant | stdio MCP | Remote MCP over HTTP | Where it's set |
| - | - | - | - |
| Claude Code | yes: `claude mcp add <name> -- <command> [args]` | streamable HTTP with headers (`--transport http … --header "Authorization: Bearer …"`), or OAuth | `~/.claude.json`, `.mcp.json`, or `--mcp-config` |
| OpenClaw | yes: `openclaw mcp add <name> --command … --arg …` | SSE or streamable HTTP, with `headers` (bearer) or OAuth (`openclaw mcp login`) | `mcp.servers` in its config |
| Hermes Agent (Nous Research) | yes: `command`, `args`, `env` | HTTP `url` with `headers`, or OAuth | `mcp_servers` in `~/.hermes/config.yaml` |

All three run stdio servers, so `flotilla mcp` serves every one of them, on the server's own machine and on any machine that can reach the server. All three can also use remote MCP, with a bearer header or OAuth.

### Why stdio came first

- **It covers every case.** On another tailnet machine, `flotilla mcp --server https://box.tail1234.ts.net` does what an HTTP endpoint would. It needs one binary there (the static musl build runs on any x86-64 Linux), and `flotilla connect` once.
- **HTTP isn't cheap here.** The server has no MCP sessions of its own. `/mcp` would need, for each MCP session, a client of the server inside the server (its views, their deltas, a thread and runtime each), sessions that expire, and HTTP requests held open for minutes through `tailscale serve` and Caddy. It would also be a second way in on the side that faces the network, to review as auth.
- **It was an add-on later.** The protocol is apart from its transport (`flotilla_mcp::Mcp::handle` takes a message and gives its reply), so remote MCP (above) is a listener that keeps an `Mcp` per `Mcp-Session-Id` over the same agent devices.

## The scope: what an agent device may do

The server enforces it, not the MCP layer. A device paired as an agent (`flotilla_proto::Scope::Agent`) is held to it on every request, view and upload, whatever its client sends.

| It can | Request or view |
| - | - |
| List and search threads | `sidebar` view; `search` |
| Read a thread's messages, paged, and a message whole | `thread` view; `GET /v1/threads/:id/history` and `/full` |
| See projects, environments and models | `sidebar` (projects, environments) and `catalog` views |
| See what the threads list carries with it: the account's VM and start counts, the agents' usage limits, whether GitHub is signed in | `sidebar` view |
| Start a thread: a prompt, a project or none, a model and effort from the catalog | `thread.new` (no files, not as another thread's hand-off) |
| Send a follow-up, a steer while a turn runs | `thread.send` (no files) |
| Wait for a turn to settle | the `sidebar` view, as it changes |
| Answer its agent's question | `thread.answer` |
| Interrupt a turn | `thread.interrupt` |
| Archive a thread | `thread.archive` |
| Open a preview link | `thread.preview`; the `thread` view's services |
| Sign itself out | `devices.revoke` of its own id only |

**It can't:** delete or forget threads, merge or link pull requests, stop or resume VMs, rename, mark read or unread, unarchive, change settings, instructions or skills, set the boat key, add, remove or set up projects, read settings, devices, schedules, what the agents spent or a VM's files, schedule prompts, pair, rename or revoke other devices, register for push, rebuild or sync, or upload files.

**Archive and interrupt are in.** An assistant that starts threads needs to stop a turn that went wrong, and to put away a thread once it's done. Neither loses work: an interrupted thread takes the next message as its next turn, and an archived one keeps its history and disk, and unarchives in the apps (its VM wakes on the next send). Idle threads get archived on their own after a while anyway.

**Its reading marks nothing read.** A thread the apps have open is on screen: marked read as things arrive, polled every second, and sent streaming text. An agent's subscription to a thread is none of those, so your unread stays yours.

### How it's enforced

- **The device's scope is kept with it** (migration 15: `devices.scope`, `pairing_codes.scope`). `flotilla pair --agent` makes a code that pairs only an agent. A device pairing may also ask for an agent's scope (`PairRequest.scope`), so an ordinary code (Add device in Settings) pairs an agent when `flotilla connect` asks. The device gets the narrower of the two. A rebuild carries the scope with the device, so it can't widen.
- **The server maps the token to `Peer::Agent`.** The host keeps each session's scope. It refuses every request outside `Request::agent_refusal` with "Agents can't …", before anything is done. It never subscribes a view outside `ViewKey::agent_may`, whether asked for or held in a hello. It sends an agent no notes (a schedule's failure, a setup run, alerts: those are for the apps), and keeps none for it while it's away. `PUT /v1/blobs` refuses an agent's token (403).
- **The threads list is one view for every client**, kept once with its revisions and deltas, so an agent sees all of it, the account's counts and limits included. They're low in sensitivity and tell an assistant whether a start will go; holding them back would mean a second sidebar view kept beside the first.
- **On this machine's socket** (the CLI's way in, which needs no token), a connection's hello can narrow it to an agent's scope (`Hello.scope`), and `flotilla mcp` always asks. Anything running as your user could run every `flotilla` command anyway, so this is for safety, not security. To have the server hold an assistant on this machine to the scope as a matter of security, pair it like any other (`flotilla connect http://127.0.0.1:7777`).
- **Settings → Devices** on the Mac, Linux and iPhone lists an agent like any device, with "Agent" where the platform goes. Revoke it there, or with `flotilla devices revoke`, and its connections close at once.

Tests: `an_agent_may_send_only_its_few_requests` (every request kind against the policy), `an_agent_is_refused_everything_outside_its_scope` and `an_agent_sees_only_its_views_and_its_reading_marks_nothing_read` (the host), `an_agent_device_is_held_to_its_scope_by_the_server` (a real server over TCP, uploads and a hand-written hello included), and `an_assistant_connects_as_an_agent_and_is_held_to_it` (the binary, end to end).

## The tools

Twelve tools, plain-text answers. Ids show by their first 8 characters, and any unique start of an id is taken.

| Tool | Takes | Gives |
| - | - | - |
| `list_threads` | `query?`, `limit?` (20, at most 100), `include_archived?` | A line a thread, newest activity first: id, status (starting, working, idle, failed, archived, expired), title, model, project, PR, how long ago, unread; the latest reply's first line under it. With `query`: the threads whose title or messages match, with the matching line and its seq. |
| `get_thread` | `thread` | Its card: status, model, project and branch, PR (state, checks, review, conflicts, link), preview links, a question waiting on you (numbered, with options), the last prompt and reply, how many messages. |
| `read_thread` | `thread`, `before?`, `limit?` (30, at most 200), `seq?`, `tools?` | Its messages newest last: `[seq] you:`, `[seq] agent:` (cut at 1,500 characters, saying `read_thread seq=…` for the rest), a run of tool calls as one line (`[14–20] 7 tool calls: Bash ×4, Edit ×3; last: …`), notices and questions. A header says where older ones start. `seq`: one message whole (a tool call's input and output fetched whole, up to 40,000 characters). `tools`: a line for each tool call. |
| `list_projects` | | The projects (owner/repo, id, default branch), and No project (a blank VM in the default environment). |
| `list_models` | `provider?` | Each harness with its default and model ids (the first 24); with `provider`, each model with its efforts. |
| `start_thread` | `prompt`, `model`, `provider?`, `effort?`, `project?`, `title?` | The new thread's id, at once. A model several harnesses run starts on the one boat lists first (Claude's on claude-code, OpenAI's on codex) unless `provider` says. |
| `send_message` | `thread`, `text` | Whether it went as the next turn or steered the running one. |
| `wait_for_turn` | `thread`, `timeout_seconds?` (45, at most 600) | Once the turn ends: its status, its last reply whole (up to 6,000 characters), a question waiting, the PR. Or that it's still working after the wait, with its latest tool call: call it again. Sends progress while it waits if the client asks. |
| `answer_question` | `thread`, `answers` (one per question: a string, or a list for several picks) | That the answer went (as a message, as the apps send one). |
| `interrupt_turn` | `thread` | Done. |
| `archive_thread` | `thread` | Done. |
| `open_preview` | `thread`, `port` | A private link to that port in the VM. |

### Why these, and why so

- **Few tools, named for what an assistant wants to do**, each with a one-paragraph description and annotations (read-only, destructive, idempotent), so a model picks right and a client can ask before the ones that act.
- **Summaries first, the whole on request.** A long thread is thousands of messages, mostly tool calls. A list is a line a thread; reading folds tool runs; a reply is cut with the seq that reads it whole; tool output only comes when one message is asked for. Each reply is cut at 1,500 characters, so a thread's 30 newest messages stay well under Claude Code's 25,000-token cap on a tool's output (the live run's two-turn thread read at about 900 characters).
- **Text, not JSON.** The reader is a model; JSON's keys and escapes cost tokens and say nothing more. Errors come back as the tool's result (`isError`) saying what to do ("No thread …: list_threads gives their ids"), so a model can fix its call.
- **A turn takes minutes; a tool call shouldn't.** `start_thread` and `send_message` return at once; `wait_for_turn` waits as long as it's told (default 45 s, under OpenClaw's and Hermes's usual tool timeouts), then says to call again. Claude Code waits up to hours, so it can pass 600.
- **Cancelling and many calls at once.** Each request runs on its own thread over one shared connection, so waits on two threads run together; `notifications/cancelled` ends a wait.

## Setting it up

On the machine where the Flotilla app or `flotilla serve` runs, nothing more is needed: `flotilla mcp` goes through its socket. For a server elsewhere:

1. On the server (or from any paired device: Settings → Devices → Add device), make a code: `flotilla pair --agent`.
2. On the assistant's machine: `flotilla connect https://box.tail1234.ts.net` and type the code. The token is kept in the login Keychain on a Mac, `secrets/` in the data directory elsewhere (or `--secrets-dir`); it's never printed.
3. Add the MCP server to the assistant:
   - **Claude Code:** `claude mcp add flotilla -- flotilla mcp --server https://box.tail1234.ts.net` (add `--scope user` for every project).
   - **OpenClaw:** `openclaw mcp add flotilla --command flotilla --arg mcp --arg --server --arg https://box.tail1234.ts.net`
   - **Hermes Agent**, in `~/.hermes/config.yaml`:
     ```yaml
     mcp_servers:
       flotilla:
         command: "flotilla"
         args: ["mcp", "--server", "https://box.tail1234.ts.net"]
     ```
   Leave out `--server …` on the server's own machine. If the assistant's tool timeout is short, `wait_for_turn` takes a shorter `timeout_seconds`.
4. To stop: `flotilla disconnect https://box.tail1234.ts.net`, or Revoke in Settings → Devices.

`--server` works for the CLI's thread commands too (`flotilla --server https://box.tail1234.ts.net threads`), as the same agent device.

## Follow-ups

- Approving a remote MCP client with a tap in the Mac or iPhone app, instead of typing a code.
- Remote MCP from the app on This Mac (it runs only with `flotilla serve`).
- Files: an agent can't upload, so it can't attach files to a prompt.
- Threads it started: an agent can archive any thread. Holding it to the ones it started would need the server to note which device started each.
- A per-device scope you choose (read-only, say) if one scope turns out to be too much or too little.
