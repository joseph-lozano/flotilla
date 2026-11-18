# Flotilla docs

Flotilla runs coding agents in the cloud. Each agent works alone in its own VM on [boat.dev](https://boat.dev), and you run the whole fleet from your Mac, your Linux desktop or your iPhone.

## What Flotilla is

You give an agent a task in a **thread**: one conversation with one agent, Claude Code, Codex or OpenCode, on the model you pick. Every thread gets a VM of its own, with its own files and processes, so agents never share a working copy, a port or your laptop.

- Start as many threads as you like. The sidebar puts the ones that need you first, and a small model sums up each reply in a line.
- Idle VMs stop after a few minutes and wake when you send, so a quiet thread uses no VM time.
- Add a GitHub repo as a **project**, and each thread starts with the repo set up on a branch of its own. The thread's pull request sits above the message box, with its checks, buttons that write the next prompt, and **Hold to merge**.
- Agents can hand work to other agents, and ask you questions you answer with a tap.

## How the pieces fit

Three things work together. You install the first; the other two are services you sign up for.

| Piece | What it does |
| --- | --- |
| Flotilla | The desktop app for the Mac and Linux, the iPhone app, and Flotilla's server, which keeps your threads and talks to boat.dev and GitHub. The desktop app has a server built in, **This Mac**, which runs while the app is open. You can also run the server on a Linux machine that stays on. |
| boat.dev | Runs the VMs and the agents in them. You need an account on a paid plan, and boat.dev bills VM time. |
| Your model providers | Bill the tokens the agents use, as they would anyway. |

Agents work in their VMs, so a turn keeps going with your laptop shut. The rest of Flotilla, such as hand-offs between agents, scheduled prompts and your iPhone, needs the server to be running: see [Run a server](server.md).

## Words these docs use

A few words come up on every page.

- **Thread**: One conversation with one agent, in a VM of its own.
- **Turn**: One round of work: the agent's answer to a message, from when you send until it stops and waits for you.
- **Steer**: A message sent while a turn runs. It goes into that turn without stopping it.
- **VM**: The cloud machine on boat.dev a thread runs in. It stops when idle and wakes when you send.
- **Project**: A GitHub repo you've added. Its threads start with the repo set up, each on a branch of its own.
- **Setup**: The scripts that install and build a project, so threads start ready to work.
- **Hand-off**: Work an agent gives to another agent, in its VM or in a new thread.
- **Server**: The part of Flotilla that keeps your threads and talks to boat.dev and GitHub: inside the desktop app (**This Mac**), or on a machine that stays on.

## Find your way

New to Flotilla? Start with [Getting started](getting-started.md). The guides show how to do things; the reference pages list every setting and shortcut, and the common commands.

- [Getting started](getting-started.md): Install the app, connect boat.dev, start a thread
- [Threads](threads.md): Models, follow-ups, search, archiving, idle VMs, usage
- [Projects and pull requests](projects.md): Repos, setup scripts, branches, Fix, Hold to merge
- [Previews and services](previews.md): Open a port, run dev servers, share a public link
- [Agents working together](agents.md): Reviews by another model, work in a VM of its own
- [Questions from agents](questions.md): Answer from the Mac, Linux or your phone
- [Scheduled prompts](schedules.md): Send later, once or every day
- [Run a server](server.md): Keep going with your laptop shut, pair devices
- [The iPhone app](iphone.md): Pair, follow threads, answer, hold to merge
- [The agent API](agent-api.md): Let Claude Code, OpenClaw or Grok use Flotilla
- [Settings](settings.md): Every section, as you see it
- [Keyboard shortcuts](shortcuts.md): On the Mac and Linux
- [The CLI](cli.md): The flotilla command, from a terminal
- [FAQ and troubleshooting](faq.md): Costs, where data lives, common messages
