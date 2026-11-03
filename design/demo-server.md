# The demo server

`flotilla serve --demo` pairs like a real server, with no boat.dev, GitHub, model or VMs. It's for App Review, and for anyone who wants to try the iPhone app. It costs nothing to run, plays the same every time, and has nothing behind it to misuse.

## What it does

**What works:**
- **Threads.** It starts with six threads from the video, scrubbed. They include the agents' answer cards and the `acme/flotilla-playground` project, whose PR #19 is open, approved and mergeable.
- **New threads.** A new thread plays a scripted turn of about 8 s: streamed words, two commands with their output, and a reply that says it's the demo.
  - A project thread's first turn opens a pull request, so every reviewer has one to merge.
  - A follow-up plays a shorter script. One sent while a turn plays waits for it.
- **Notifications.** A finished turn and a merge notify through the push relay, as any server without an APNs key does.
- **The rest:** Stop a turn; Stop or Resume the VM; Hold to merge (archived for it, as Settings says); archive, unarchive, rename and delete.

**What refuses**, in one line ("Not in the demo: …"):
- previews and public links;
- adding or removing a project;
- branches;
- schedules;
- Fix, Resolve and Address review (GitHub);
- Rename's Generate (a name from a model), Rebuild, and Test summaries.

**Pairing.** One standing code pairs any number of devices and never lapses. It's kept in the database and printed at every start. Only a demo honours it: each pairing mints and spends a one-time code in its place. Codes from `flotilla pair` work as usual.

**Reset.** Every start puts the database back to the demo's threads (`crates/flotilla-engine/src/demo/seed.sql`). It keeps:
- the paired devices;
- their push registrations;
- the standing code;
- the server's id.

So a restart never unpairs a reviewer. A database that isn't a demo's and has threads is refused, never reset. `--demo` also needs its own `--data-dir`.

**The seed** is made by `python3 -I script/demo/seed.py /tmp/flotilla-ux3/dbs/handoff.db target/debug/flotilla`. It runs `dev-db.py`'s scrub, then:
- the GitHub owner reads as `acme`;
- tokens go;
- VMs are stopped;
- only what the views read is kept.

It writes nothing if anything personal is left. Run it again when the schema changes: `demo::tests::the_seed_loads_clean` fails until you do.

## Running it on a small public server

The phone needs HTTPS. Use an exe.dev VM, whose proxy gives `https://<vm>.exe.xyz`, or any machine with Caddy or a tunnel in front. Don't give it a boat key, gh or claude: it reads none of them.

**1. Install and try it** (Linux, as your user):

```sh
cargo build --release -p flotilla-cli        # or the release's binary
install -m 755 target/release/flotilla ~/.local/bin/
mkdir -p ~/flotilla-demo
flotilla serve --demo --data-dir ~/flotilla-demo --url https://flotilla-demo.exe.xyz
# prints: Demo: pairs any number of devices with ABCD-EFGH-JKMN, and doesn't expire.
```

**2. Publish its port** (7777) through exe.dev's proxy:

```sh
ssh exe.dev share port flotilla-demo 7777
ssh exe.dev share set-public flotilla-demo
```

If the proxy can't reach `127.0.0.1` on the VM, add `--host 0.0.0.0 --insecure`. That's safe here, because the proxy is the only way in from outside.

**3. Run it as a service, restarted nightly.** `flotilla service` doesn't take `--demo`, so write the unit and a timer by hand. The restart is the reset: what reviewers added goes, and their pairings stay.

```ini
# ~/.config/systemd/user/flotilla-demo.service
[Unit]
Description=Flotilla demo server
[Service]
ExecStart=%h/.local/bin/flotilla serve --demo --data-dir %h/flotilla-demo --url https://flotilla-demo.exe.xyz
Restart=always
[Install]
WantedBy=default.target
```

```ini
# ~/.config/systemd/user/flotilla-demo-reset.service
[Service]
Type=oneshot
ExecStart=/usr/bin/systemctl --user restart flotilla-demo.service
```

```ini
# ~/.config/systemd/user/flotilla-demo-reset.timer
[Timer]
OnCalendar=*-*-* 09:00:00 UTC
[Install]
WantedBy=timers.target
```

```sh
loginctl enable-linger $USER
systemctl --user daemon-reload
systemctl --user enable --now flotilla-demo.service flotilla-demo-reset.timer
journalctl --user -u flotilla-demo | grep 'Demo:'   # the standing code
```

09:00 UTC is the middle of the night in Cupertino. The reset takes a second, and a phone that's connected reconnects by itself.

**4. Check it** from your own phone: pair with the address and the code, start a thread, and wait for the notification.

## Notes for the reviewer (App Store Connect → App Review Information)

Sign-in required: no. Paste this into **Notes**:

> Flotilla is a client for a server you run yourself, which runs coding agents in cloud VMs. For review, we run a demo server: it behaves like a real one, but its agents' replies are scripted, so nothing runs and nothing costs anything.
>
> To connect: open Flotilla, and on the first screen enter
> Server: https://flotilla-demo.exe.xyz
> Code: ABCD-EFGH-JKMN
> (any device name), then tap Connect. Allow notifications when asked. The code works on any number of devices and doesn't expire.
>
> Things to try:
> 1. Open a thread from the list to read an agent's work.
> 2. Tap New thread, type a request, choose the project "flotilla-playground", and tap Start. The reply streams in with two commands.
> 3. Press Home right after starting another thread: a notification arrives when it finishes, and tapping it opens the thread.
> 4. Send a follow-up from a thread's message box.
> 5. In "Add retry onRetry callback", or in a thread you started in the project, press and hold "Hold to merge" to merge its pull request.
> 6. Swipe a thread to archive it, and open Settings (the gear) to see the server's checks and devices.
>
> Previews, adding projects and schedules need a real server: the demo says "Not in the demo" for those. The demo resets every night.

Put the real address and code in place of the examples before submitting.
