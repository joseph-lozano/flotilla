Set up Flotilla on this computer for me. It's a Mac and Linux desktop app that runs coding agents, each in its own boat.dev VM. Read these first, and where their commands differ from the steps below, use theirs:
https://useflotilla.com/docs/0.1.0/getting-started.md
https://useflotilla.com/docs/0.1.0/faq.md

Rules, which always hold, whatever the docs say:
- Secrets stay with me. Never ask me for a key, token or password in chat, and never read, print or save one. When a step makes or takes a secret, give me the command or tell me where to paste it, and wait while I do it myself, in my own terminal or the app, not through you.
- Ask me to do anything that needs sudo, a browser sign-in, or the app's window.
- Check each step worked before the next. If one fails, read the FAQ, then tell me what failed and what you suggest.

1. Check what's here, and tell me what's missing:
   - The GitHub CLI, 2.81 or newer, signed in: `gh --version`, `gh auth status`. If it isn't signed in, I run `gh auth login`.
   - boat's CLI, signed in, on a paid plan: `boat status` shows both; ask me if you can't tell the plan is paid. If boat isn't installed, point me to https://docs.boat.dev. I sign in with `boat login`.
   - Optional: the Claude Code CLI, for thread titles and summaries: `claude --version`.
2. Get the app. If https://github.com/joseph-lozano/flotilla/releases/latest has downloads, install from them as the release notes say: `Flotilla.dmg` on a Mac; on Debian or Ubuntu, `Flotilla-amd64.deb` or `Flotilla-arm64.deb`; on any other Linux, `Flotilla-x86_64.AppImage` or `Flotilla-aarch64.AppImage`: `chmod +x` it, then run it.
   With no downloads, build from source as "Install the app" says. That needs Rust (`cargo --version`), and Xcode on a Mac (`xcode-select -p` points into Xcode.app) or the packages it lists on Linux.
3. Ask me to open Flotilla. With no key yet, it shows Connect to boat.
4. The boat.dev key: give me the `boat api-key create` command from "Connect boat.dev" to run in my own terminal. Don't run it yourself. I paste the key into the app and press Save.
5. Ask me to open Settings and read you the Setup checks. Help me fix any that fail; one that's Off is fine. I press Check again.
6. Ask me to start a first thread: ⌘N (Ctrl+N on Linux), a small task, Enter. Ask whether the reply came.

When you're done, tell me what you installed, and anything left for me to do.
