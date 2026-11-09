Set up a Flotilla server on this Linux machine for me. It stays on, so hand-offs between agents, scheduled prompts, project setup and my iPhone keep going with my laptop shut. If you aren't running on that machine, ask me how to reach it. Read these first, and where their commands differ from the steps below, use theirs:
https://useflotilla.com/docs/0.1.0/server.md
https://useflotilla.com/docs/0.1.0/faq.md

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
