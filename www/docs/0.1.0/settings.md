# Settings

Open Settings with ⌘,, or the gear at the bottom of the sidebar. The sections appear in this order. Settings marked *this device* stay on the Mac or Linux desktop you set them on; the rest are the server's, shared by every device connected to it.

On Linux, **This Mac** reads **This computer**. If the server can't be reached, Settings shows only Server, Devices, Appearance and Notifications. The iPhone's settings are on [The iPhone app](iphone.md#settings).

## Server

Which Flotilla server this app uses: **This Mac**, the server built into the app, or the server you paired with. The one in use has a tick; **Use** switches to the other at once. The paired server has **Forget**, which removes the pairing. **Connect to a server…** pairs with one; it reads **Connect to a different server…** once you have one, which it replaces. See [Pair your devices](server.md#pair-your-devices).

## Devices

The devices paired with the server in use, each with its kind (macOS, Linux, iOS, or Agent for an [assistant](agent-api.md)), and whether it's connected or when it was last seen. Every device has **Rename**; every device but this one has **Revoke**, which disconnects it at once. **Add device** makes a pairing code, with its QR code and link.

On **This Mac**, **Let other devices connect** lets other devices pair with this Mac while Flotilla is open, and **Over Tailscale** lets them reach it from your tailnet. Both are off by default. See [Use your Mac as the server](server.md#use-your-mac-as-the-server).

## Setup

Called **Server setup** when you use a paired server. The checks Flotilla needs to pass, with how many pass, such as *6/7*. It opens on its own when a check fails or warns. Each check says what it found and gives the commands that fix it, each with **Copy**. **Check again** runs them all again. The checks are listed under [Check it](server.md#check-it).

On **This Mac**, the **boat API key** check has a field to paste a new key, and **Replace**.

## Projects

Your projects, each with its state, such as *Ready on main@1a2b3c4* or *Setup failed at* a step. **Add project** adds a GitHub repo, and each project's **Settings** opens its setup scripts, setup history and **Remove project…**. See [Projects and pull requests](projects.md).

**Branch prefix** puts new threads' branches in a folder: `you` becomes `you/`. It applies to new branches only. Flotilla drops characters git doesn't allow, and says so.

## Appearance

**System** (the default) follows your system's light or dark setting; **Light** and **Dark** stay put. This device.

## Notifications

**Banners and the Dock's unread count** (on Linux, **Notifications and the launcher's unread count**). On by default. This device. If your system blocks Flotilla's banners, Settings says so, with **Open System Settings**.

## New threads

The model new threads start on, with its **Effort** and **Fast** where the model has them.

## Instructions and skills for every agent

**Instructions for every agent** is text every agent gets, such as *Use pnpm, not npm. Run the tests before you finish.* It saves as you type, and applies from each thread's next prompt.

**Skills for every agent** are skills, folders with a `SKILL.md`, that Flotilla puts in every thread's VM. **Add skill** lists the skills it finds in `~/.claude/skills`, `~/.agents/skills` and `~/.codex/skills`, or **Choose a folder…** picks one skill or a folder of them. Each skill shows where it came from; the refresh button updates it from its folder, and **×** removes it.

## Stop idle VMs after

**5 min**, **10 min** (the default) or **30 min**. A change applies from each thread's next turn. See [Idle VMs](threads.md#idle-vms-stop-wake-and-keep-running).

## Archiving

Flotilla can archive threads for you. You can undo either kind of archive.

- **Archive a thread when its pull request merges**: On by default. See [Archive on merge](projects.md#archive-on-merge).
- **Archive threads idle for**: **Never** (the default), 3, 7, 14 or 30 days. A thread a schedule sends to isn't archived for being idle.

## Titles and previews

The model that titles threads and sums up each reply in a line, or **Off**. It runs through the Claude Code CLI where the server runs. Picking a model tests it; **Test again** runs the test again, and the line under it says whether it's working.

## boat environments

Your boat.dev environments, with the default marked. Shown only when you have some.

## Plan and billing

**Open boat.dev billing** opens your boat.dev plan.

## Local data

Called **Data on the server** when you use a paired server. Where the database is. **Rebuild from boat…** downloads every thread from boat.dev again, keeping the old database as a backup.
