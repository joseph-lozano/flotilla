# FAQ and troubleshooting

Short answers to common questions, and what to do about the messages you're most likely to see.

## What does it cost?

Flotilla itself is free and open source, under the MIT License. You pay for what runs:

- **boat.dev** bills VM time, on a paid plan. Each thread is a VM, and so is each delegate an agent starts. Your plan includes a number of VM starts (waking a stopped VM is one); starts beyond them are *extra starts*, if you've turned those on in boat.dev.
- **Your model providers** bill the agents' tokens, as they would anyway. The **Agents** tab in [Usage](threads.md#usage-and-limits) shows what the last 30 days would cost at API prices.

Idle VMs stop on their own, so a quiet thread uses no VM time. **Keep running** keeps a VM up, and billing, until you turn it off.

## Where does my data live?

With the server you use. On **This Mac**, that's your Mac: threads in `~/Library/Application Support/Flotilla`, and your boat.dev key in the Keychain. On Linux, threads are in `~/.local/share/Flotilla` and the key is in a file there only you can read. On a server, both live on the server, with the files you attach and the skills you add. Each device keeps only its own sign-in token.

Your code lives in each thread's VM on boat.dev, and on GitHub once an agent pushes it.

## Which agents can I use?

Claude Code, Codex and OpenCode, with the models boat.dev offers for them. New models show up on their own.

## What happens when I shut my laptop?

Running turns carry on in their VMs, and idle VMs still stop on time. Without a server, the rest of Flotilla is in the app, so it stops until you open the app again:

- answers from agents that were handed work wait, and arrive when the app opens;
- setup waits;
- a repeating scheduled prompt whose time passes is skipped, and a one-off runs late;
- your iPhone can't connect.

With a [server](server.md), all of it keeps going.

## Can I use more than one computer?

Yes. Run a [server](server.md) and pair each Mac and Linux desktop with it; they all see the same threads. Or, as a preview, let other devices connect to the Mac that has your threads: see [Use your Mac as the server](server.md#use-your-mac-as-the-server).

## Messages you might see

What these messages mean, and what to do.

- **Hand-off limit reached (20 since your last message). Send a message to continue.**: Agents in this thread, and the threads it handed work to, have handed off 20 tasks since you last wrote. Read what they did, then send a message, even *carry on*, to let them go on. See [Limits](agents.md#limits).
- **Can't start VMs**: boat.dev won't start another VM now, usually because a start limit is used up. [Usage](threads.md#usage-and-limits) shows which limit and when VMs can start again.
- **boat can't see *owner/repo* yet**: Give boat.dev access to the repo in boat's dashboard, under **Environment → GitHub repositories**, then add the project again.
- **gh isn't signed in to github.com, or gh is too old**: Run `gh auth login --hostname github.com`, or update the GitHub CLI to 2.81 or newer, then press **Check again** in Settings.
- **Setup failed at a step**: A project's setup scripts or `.agents/setup` failed. Press **Fix in a new thread** to have an agent fix it, or open the project's settings for the log. See [When setup fails](projects.md#when-setup-fails).
- **Hold to merge isn't there**: It shows only when the pull request is open, not a draft, mergeable, with no failing or pending checks, and no review needed. The bar says what's in the way, such as *Conflicts with main* or *Changes requested*, with the button that helps.
- **Flotilla can't show banners: they're off in System Settings.**: Press **Open System Settings** and allow notifications for Flotilla.
- **Flotilla is already running on this Mac as a server.**: On Linux, *this computer*. Only one Flotilla can use a database at a time, and a server on the same machine is using it. Press **Connect to it**: no pairing needed.
- **Another Flotilla is already running on this database**: From the CLI: most commands go through the server or the app when one is running, but this one can't. Quit the app, or stop the server with `flotilla service stop`, and run it again.

## What do the setup checks mean?

`flotilla doctor`, and **Setup** in Settings, check what Flotilla needs and give the command that fixes each problem. The checks are listed in [Check it](server.md#check-it). A check that's *Off*, such as Push or Remote MCP when you haven't set them up, isn't counted. If the boat API key or the database check fails, the app shows the checks in place of New thread until it's fixed.

## Linux notes

A few things work differently on a Linux desktop.

- File pickers, and following the desktop's light or dark style, come through the desktop portal (`xdg-desktop-portal`), which GNOME and KDE run. Without it, pick Light or Dark in Settings.
- Opening Flotilla again brings its window forward, and a pairing link opened while it runs goes to that window.
- A thread's notifications need a notification service, which most desktops have.

## Still stuck?

Open an issue on [GitHub](https://github.com/joseph-lozano/flotilla/issues), with what you did, what you expected, and what `flotilla doctor` or **Setup** in Settings says.
