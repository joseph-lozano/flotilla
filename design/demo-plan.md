# Flotilla: the complete demo (plan)

Written 2026-10-30, for after P11 (header), P10 (services) and the design pass; updated the same evening to the UI after Joseph's corrections (filter dropdown, rows, header, sidebar rail, `.agents/resume` on wakes only). Narration: `script/demo/full-demo.tsv`; takes: `script/demo/full-demo.sh`.

**Goal:** about 11–12 minutes, narrated (Holler 0.6B's Kit voice, through `script/demo/narrate.py`), showing the whole app, not just projects. The old 4-minute video (`~/Movies/Flotilla/flotilla-projects-demo.mp4`) covered projects only and shows resume behaviour that has since changed.

**How it's made:** the same way as the old video. Scripted takes drive the real app against a scratch database and the real boat account (`script/demo/projects-demo.sh` is the model). Waits for VMs, agents, CI and setup are cut with `rec:pause` / `rec:resume`. Narration is voiced per caption and mixed in at each caption's time.

## Chapters

| # | Chapter | Length | Running | Shows |
| - | - | - | - | - |
| 0 | Cold open | 0:15 | 0:15 | Several agents working at once; a PR's merge button filling up |
| 1 | The window | 0:40 | 0:55 | Sidebar groups, rows, resizing and hiding the sidebar, New thread page |
| 2 | A first thread | 1:05 | 2:00 | Agent and model picker, effort, a VM per thread, streaming text and tools, the model-written title |
| 3 | Steer and stop | 0:45 | 2:45 | Steering a running turn, interrupting it |
| 4 | Many at once | 0:50 | 3:35 | A second agent, Working group with timers, "Thread finished" toast, unread dot and one-line summary, hover card, ⌘1–⌘9 |
| 5 | Reading and replying | 0:30 | 4:05 | Copy, Cite into the composer, code-block copy, long prompts folded, jump to latest, ↑ to recall a prompt |
| 6 | VMs that sleep | 0:50 | 4:55 | Idle stop (5/10/30 min), time left in the header, a stopped VM waking on send, Keep running (the cup), Stop/Resume VM, Preview |
| 7 | Keeping it tidy | 0:40 | 5:35 | Rename (double-click, Generate), Archive with Undo ⌘Z, unarchive by sending, Delete |
| 8 | Projects: add a repo | 1:00 | 6:35 | GitHub through gh, Add project (cached repo list), `.agents/setup`, the first setup run, the project's setup page, the project filter appearing |
| 9 | A project thread | 1:00 | 7:35 | Project and branch pickers, the model-named branch, start stages, services starting, Preview ▾ |
| 10 | The pull request | 1:20 | 8:55 | PR bar on the composer, checks, red conflict icon and "1 behind", Resolve, Ready for review, Hold to merge, setup following main |
| 11 | When things break | 0:50 | 9:45 | A failing check and Fix; a broken setup on main, its toast, Fix in a new thread, the thread starting anyway |
| 12 | Start from a branch or PR | 0:35 | 10:20 | Branch picker (`#N`, a PR link), "New thread on this branch" |
| 13 | Hooks and services | 0:45 | 11:05 | The four `.agents/` files; resume while a woken VM waits for it (or narrated over a wake: a resume that ran shows nothing), archive on archive, services' output and Restart |
| 14 | Settings, briefly | 0:25 | 11:30 | Defaults, titles and previews model, branch prefix, appearance, billing link, Rebuild from boat |
| 15 | Close | 0:20 | 11:50 | The fleet in the sidebar, one line on what Flotilla is |

**Total: 11:50.** Spoken at ~150 words a minute, that's roughly 1,750 words of narration.

## Chapter detail

### 0. Cold open (0:15)
- **On screen:** a sidebar with five or six threads, three working (green dots, timers), two agents' logos; cut to a PR bar where "Hold to merge" fills and the PR turns merged.
- **Gist:** "Coding agents, each in its own cloud VM, each on its own branch, all in one window."
- **Setup:** the threads from chapters 2–4 already running (record the open last, from the same data).

### 1. The window (0:40)
- **On screen:** Unread and Idle at the top, Working and Archived pinned at the bottom with counts; a row's lines (title and time, the reply's summary, the agent's logo and model; no VM words unless it needs you); dragging the sidebar's edge wider, hiding it to the rail with ⌃⌘S and back; the New thread page with the pixel sails. The project filter only exists once there's a project, so it's shown in chapter 8.
- **Gist:** "Every thread is a conversation with an agent, running in a boat.dev VM. The sidebar sorts them by what needs you."
- **Setup:** a handful of finished threads (real ones, started before recording), at least one unread.

### 2. A first thread (1:05)
- **On screen:** New thread; the searchable agent-and-model picker (Claude Code, Codex, pi, OpenCode, Kimi, Mistral, grouped with logos); effort and fast; No project; type a small task; the thread opens at once ("sending"), the VM starts, text and tool calls stream in; the title is rewritten by the model; the header's second line (the VM's time left), and the model in the composer's footer.
- **Gist:** "Pick any agent and model boat offers. The thread opens straight away; the VM takes a few seconds."
- **Setup:** none. Agent: Claude Code with Haiku 5.5 (cheap, fast). Task with a couple of tool calls (e.g. write and run a tiny script).

### 3. Steer and stop (0:45)
- **On screen:** while a turn runs, the composer says "Steer the running turn…"; send a steer and see the agent take it; then Stop interrupts a turn.
- **Gist:** "You can steer an agent mid-turn without stopping it, or stop it."
- **Setup:** a task long enough to steer (a few tool calls). Same thread as chapter 2 or a new one.

### 4. Many at once (0:50)
- **On screen:** start a second thread on another agent (Codex, a small model); the Working group with timers; switch away; a "Thread finished" toast with Open; the unread dot and the one-line summary; the hover card (agent, VM state in words, auto-stop, project, times); ⌘1–⌘9 between threads.
- **Gist:** "Run as many as you like. Flotilla tells you when one finishes and what it said, in a few words."
- **Setup:** the account needs the second agent's credentials in boat (see Needs). Titles and previews on (Haiku 5.5 via `claude -p`).

### 5. Reading and replying (0:30)
- **On screen:** hover a reply for Copy and the time; right-click → Cite (quote lands in the composer); a code block's copy button and language label; a long prompt folded with "Show full message"; the jump-to-latest button; ↑ in an empty composer brings back the last prompt.
- **Gist:** "The small things from T3 Code: cite, copy, recall."
- **Setup:** a thread with a code block and a long prompt (from chapter 2's or a seeded one).

### 6. VMs that sleep (0:50)
- **On screen:** Settings → "Stop idle VMs after" (5/10/30 min); the header's "9 min left"; a thread whose VM was stopped with Stop VM (its pip and the hover card say so); send to it and it wakes; the Keep running cup; Preview… asks for a port (the browser itself isn't recorded).
- **Gist:** "Idle VMs stop on their own, so you pay nothing while you're away, and wake when you send."
- **Setup:** stop a VM with Stop VM rather than waiting the 5 minutes. Preview opens in the browser, which the app's own recorder can't capture (see Risks).

### 7. Keeping it tidy (0:40)
- **On screen:** double-click a title to rename; Generate asks the model for a name; archive from the row's hover button, "Archived · Undo ⌘Z"; sending to an archived thread brings it back; Delete removes the VM and the thread after a confirmation.
- **Gist:** "Archive keeps the history and stops the VM. Delete removes both."
- **Setup:** throwaway threads to archive and delete.

### 8. Projects: add a repo (1:00)
- **On screen:** Settings → GitHub ("Signed in as … with the GitHub CLI"); Add project with the list filtered to the demo repo before it appears; the `.agents/setup` note; Add; the project's setup page: stages, the log tail, "Ready on main@… took 22s"; setup history; Run setup again and Rebuild from scratch.
- **Gist:** "A project is a GitHub repo. Flotilla prepares a VM for it, with your setup already run, and keeps it current as main moves."
- **Setup:** the playground repo (`acme/flotilla-playground`) with `.agents/setup`, `.agents/resume`, `.agents/archive` and `.agents/services.yml`; the scratch DB with no projects; `step:search-repos` so no other repo names show.

### 9. A project thread (1:00)
- **On screen:** New thread with the project picked; the branch picker on "New branch"; the prompt; the start card: name the branch (the model's name appears), copy the prepared VM, switch branch, setup (seconds when nothing changed); services start; Preview ▾ shows `web` and `info` ready; a still of the Retry visualizer page.
- **Gist:** "A project thread starts from a copy of that VM, on its own branch, in seconds. Its dev server starts on its own."
- **Setup:** P10 services in the repo (a tiny web server on `$PORT`). Agent: Haiku 5.5. A task that ends in a draft PR (e.g. add jitter to `retry`).

### 10. The pull request (1:20)
- **On screen:** the PR bar joins the composer: #N, title, checks, +/−; meanwhile a teammate's commit on main conflicts: the red conflict icon, "Conflicts with main" and "1 behind"; Resolve fills the composer with the prompt (nothing sent until Send); send; the agent rebases and pushes; checks go green, "1 behind" goes; Ready for review; press and hold Merge, the bar fills; merged toast; the project's setup runs for the new main.
- **Gist:** "Flotilla watches the PR. When something needs doing, one button writes the prompt for the agent. Merging is a press and hold."
- **Setup:** push the conflicting commit to main from the shell right after the PR opens (as in the old take 3). Hold timing from P11.

### 11. When things break (0:50)
- **On screen:** a PR with a failing check (red "1 failing"); Fix writes a prompt with the failing check; then a commit that breaks `.agents/setup` on main: the setup run fails, the toast with "Fix in a new thread", the prompt with the commit, the failing script, exit code and the end of its log; send; the new thread starts even though setup fails, and its header says so.
- **Gist:** "Broken checks and broken setup each come with a ready-made prompt for an agent."
- **Setup:** stage the failing check by pushing a commit that breaks a test to the PR's branch from the shell ("a teammate's typo"); stage the broken setup with a commit that calls a missing npm script (as in the old take 5). The agent's fix also gives chapter 12 an open PR.

### 12. Start from a branch or PR (0:35)
- **On screen:** New thread's branch picker: open PRs and branches, "another thread is on it"; type `#N` or paste a PR link; pick one; right-click a thread → "New thread on this branch".
- **Gist:** "Any branch or open PR can be a starting point, including one a thread is already on."
- **Setup:** chapter 11's open fix PR.

### 13. Hooks and services (0:45)
- **On screen:** a caption card listing `.agents/setup`, `resume`, `archive` and `services.yml`; Stop a project thread's VM and send: the problem line shows `.agents/resume` running during its (up to 10 s) wait, or the wake is narrated (a resume that ran shows nothing); Archive it: `.agents/archive` runs (narrate the PlanetScale branch example), then the VM stops; Preview ▾ → a service's Output and Restart.
- **Gist:** "Four files in `.agents/` tell Flotilla how to set up, wake, clean up, and serve your repo. Same convention as Amp's orbs."
- **Setup:** `.agents/archive` that removes something visible per thread (a marker it reports). The file listing can't come from the app; show it as a caption card or a short still.

### 14. Settings, briefly (0:25)
- **On screen:** a scroll through Settings: boat API key (in the keychain), GitHub, Projects and Branch prefix, Appearance (System/Light/Dark, after the design pass), New threads defaults, Stop idle VMs after, Titles and previews (model, Test again), Plan and billing link, Rebuild from boat.
- **Gist:** "Everything lives on your Mac: the key in the keychain, the history in a local database."
- **Setup:** none.

### 15. Close (0:20)
- **On screen:** the sidebar full of threads across agents, the project filter open; the pixel sails.
- **Gist:** "Flotilla. A fleet of agents, each in its own boat."
- **Setup:** the state left by the chapters above.

## Risks, and how to stage them

- **VM starts:** about 12–15 starts across the takes (threads in chapters 2, 4, 7, 9 and 11, setup runs in 8, 10 and 11, the wakes in 6 and 13), more with retakes. The paid limits are 12 a minute and 60 an hour; record over two sessions if retakes pile up.
- **Costs:** agents on cheap models (Haiku 5.5; a small Codex model), short tasks. Titles, previews and branch names also use Haiku 5.5 through `claude -p`.
- **Long waits:** agent turns, setup runs (~22 s), CI (~30–60 s), merges. All cut with `rec:pause` and `until:` steps. The 5-minute idle stop is shown in Settings and narrated, and the stopped state is made with Stop VM.
- **The browser:** Preview and "Open PR" open the default browser, outside the window the app records. Either show only the in-app side (Preview ▾ with `web` ready) or splice in a still of the page.
- **Staged moments:** the conflict (a commit pushed to main), the failing check (a commit pushed to the PR's branch), the broken setup (a commit to main) are all pushed from the shell at the right point, as in the old takes. Each changes the playground, so the takes run in order and the repo is put back afterwards.
- **What the agent decides:** agents can surprise (the old take 3 had the agent add the changelog line itself, so the planned failing check never failed). Keep fallbacks: Resolve if Fix can't be staged, and the reverse.
- **Private names:** filter Add project's list before it loads, start from a scratch database, and keep other repos and threads off screen.
- **Seeded threads:** chapters 1, 4 and 5 need a few finished threads first. Start them for real before recording (a few extra VM starts) rather than faking a database.

## Left out on purpose

- **The `flotilla` CLI:** a developer tool, not the app.
- **Link a different PR, Copy thread ID and Copy VM ID:** niche; the menu is seen in passing.
- **Address review:** GitHub won't let a PR's author request changes on it, so it needs a second GitHub account to stage. Mentioned in the narration of chapter 10 only.
- **boat environments in Settings:** projects make their own; No-project threads use the default.
- **Trial-plan limits:** Flotilla is paid-plan only now.
- **Recording, captions and narration tooling:** the demo is about the app.
- **Not built yet** (from the plan): the "archive after merge" setting, retrying a setup run cut short by quitting, and Amp's pre-clone script.

## Answered

1. **The second agent:** Codex. The boat account has a ChatGPT subscription connected; chapter 4 uses a small Codex model.
2. **Screen Recording:** not available while Joseph is away; Preview and Open PR stay in-app, with a still of the Retry visualizer page where it helps.

## The final cut (2026-10-30)

`~/Movies/Flotilla/flotilla-demo.mp4`, 9 min 34 s, 23 chapters in this order (`script/demo/full-demo.sh final`). Part 3 added the cold open, Find your threads, The PR up close, Setup your way, The setup log, See it change, A fleet at work and Day and Night to the plan's chapters, to bring the length near ten minutes without padding.

| Chapter | File | Length |
| - | - | - |
| Cold open | ch00 | 13.1 s |
| The window | ch01 | 33.6 s |
| A first thread | ch02 | 42.8 s |
| Steer and stop | ch03 | 29.1 s |
| Many at once | ch04 | 41.7 s |
| Reading and replying | ch05 | 14.4 s |
| VMs that sleep | ch06 | 24.8 s |
| Keeping it tidy | ch07 | 25.9 s |
| Find your threads | ch17 | 11.0 s |
| Add a repo | ch08 | 42.7 s |
| A project thread | ch09 | 35.5 s |
| The pull request | ch10 | 48.0 s |
| The PR, up close | ch19 | 15.2 s |
| When things break | ch11 | 34.9 s |
| Setup, your way | ch20 | 14.8 s |
| The setup log | ch22 | 7.8 s |
| Start from a branch or PR | ch12 | 14.2 s |
| Hooks and services | ch13 | 46.2 s |
| See it change | ch16 | 25.3 s |
| A fleet at work | ch18 | 19.7 s |
| Day and Night | ch21 | 9.6 s |
| Settings | ch14 | 14.9 s |
| Close | ch15 | 8.5 s |

Notes:
- The visualizer stills in See it change (and chapter 9) are local builds of the playground served on 127.0.0.1: the app's recorder can't capture the browser.
- A fleet at work shows the fleet's fresh draft PRs rather than agents mid-turn: Haiku and GPT-6 Luna finish small tasks in a minute or two.
- Settings (chapters 6 and 14) stops above the environment list: a scratch DB lists environments of projects it doesn't own.
