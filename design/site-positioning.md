# useflotilla.com: positioning and copy rules

Written 2026-10-31 for the site redesign, from README.md, PRODUCT.md, ios/README.md, the 90-second video's script and the old site. Nothing here comes from users or metrics: there are none yet.

## Positioning statement

For developers who already run Claude Code, Codex or OpenCode and want several going at once, Flotilla is a native app for the Mac (and Linux, with an iPhone app coming) that runs each agent alone in its own cloud VM on boat.dev. Agents running on your own laptop share its files, ports and processes, and they stop when the lid closes. In Flotilla every thread gets a whole machine, its pull request sits on the thread until you hold to merge, agents can hand work to each other, and because each agent works in its own VM, it keeps going with your laptop shut; an always-on server keeps hand-offs, schedules and notifications going too. It's free and MIT licensed; boat.dev bills for VM time and model providers bill for tokens.

## One-liner

A fleet of coding agents, each in its own boat. (Joseph's line from the old site and README. The boat is literal: each VM runs on boat.dev.)

## Audience

- Developers who use coding agents daily and have started running more than one at a time.
- They live on a Mac, use GitHub and the gh CLI, and are used to paying for agents and compute.
- The job: start a task, switch away, come back to a result or a pull request that's ready to merge.

## What pushes them, what pulls them

- Push: agents on one laptop fight over ports, dev servers and the working copy; the laptop has to stay open; keeping branches and setup straight per agent is chores.
- Pull: one VM per thread; projects that start a thread in seconds with the repo set up; the PR on the thread with Fix, Resolve, Address review and Hold to merge; hand-offs between agents; the same threads on the iPhone.
- Anxiety, answered on the page: cost (boat.dev's paid plan, tokens as usual, idle VMs stop), setup (build from source today, the README covers it), the iPhone app (coming, not on the App Store or TestFlight yet).

## Proof we can use

The app itself: the Mac stills and the iPhone screenshots (the 90-second video came down on 2026-11-01; a new one comes later). Real commands (`flotilla setup --tailscale-serve`, `flotilla service install`, the build steps). Real numbers from the product: idle stop after 5, 10 or 30 minutes; three agents; MIT. No customers, testimonials, stars or benchmarks: don't invent any.

## Words

- Use: thread, VM, agent, project, branch, pull request, hand off, stop, wake, server, pair, hold to merge.
- Lead with isolated VMs, never branches or worktrees.
- Never: template (an implementation detail), seamless, powerful, effortless, supercharge, unlock, workflow, AI-powered, emoji, exclamation marks.
- Examples stay generic: no real machine names, accounts, tailnets, codes or tokens.
- Monospace only for real commands and data.

## Voice

Plain, short and concrete, in Joseph's register: say what happens, in the order it happens. One punchy line per page at most. The thanks to Theo and T3 Code stays in Joseph's own words, first person.

## Primary action

Get it on GitHub (build from source). When there's a new video, watching it is the second.

## Images on the site

Every product picture is a real capture with a Day and a Night version; the page swaps them with `<picture>` and `prefers-color-scheme`, so it works without JavaScript. Nothing is generated. Provenance is kept here rather than in sidecar files, which Cloudflare Pages would serve.

All of them come from copies of the video's scratch server database (cut 3), run offline: no VMs, no network. The copies stop every VM but two, put back the playground project, reopen pull request #19 as it stood before it merged (open, 2/2 checks), and drop the earlier review threads. The scripts are in `/tmp/flotilla-www2/round2/` (`prep.py`, `prep-c3.sh`, `mac-shots.sh`, `phone-shots.sh`).

| Files (`-day`, `-night`) | From |
| - | - |
| `www/img/mac-threads-*.webp` | The Mac app (`--features snapshot`, `FLOTILLA_OFFLINE=1 FLOTILLA_DEV=1`) on New thread, hovering the Slugify thread with the snapshot harness's `hover:` step (no pointer drawn), cropped to x 0, y 78, 432×358. The Palette thread is left out of this copy so no row sits under the card's edge. `cwebp -q 84` |
| `www/img/mac-merge-*.webp` | The same, with "Add retry onRetry callback" open, 1180×780. The page lays a working Hold to merge over its PR bar, in that theme's colours. `cwebp -q 82` |
| `www/img/mac-handoff-*.webp` | The same, with "Slugify function with reviews" open at its end: the reviewer cards, "2 agents answered" and who caught what. `cwebp -q 82` |
| `www/img/phone-threads-*.webp` | The iPhone app in my own iPhone 17 simulator (iOS 27.0, light and dark), paired with `flotilla serve --offline --url http://127.0.0.1:7797` (the app's transport security takes `127.0.0.1`, not `box.localhost`) on a copy of `/tmp/flotilla-ux3/dbs/handoff.db`: its timestamps moved so the newest is minutes old, the two older Slugify threads archived, Docs #18 unread and stopped, Palette stopped, Slugify and #19 up with time left, #19 open, approved and mergeable, the owner's GitHub login as `acme`, and `dev-db.py`'s scrub and leak check over it. `FlotillaUITests/test2Threads`. Status bar set to 9:41. Retaken on 2026-11-02 for the current UI (the Alpha tag, the Settings gear, search and the usage bar). `cwebp -q 78 -resize 603 1311` |
| `www/img/phone-handoff-*.webp` | `test7Look` on "Slugify function with reviews": its top, with both reviewer cards |
| `www/img/phone-merge-*.webp` | `VideoUITests/testV5HoldToMerge` on #19, a host screenshot (`simctl io screenshot` in a loop) part way through the hold |
| `www/og.png` | HTML drawn to match the hero (the three berths on Night plum), captured at 1200×630 with headless Chrome. Its origin is also in the PNG's text chunk. |

None shows a real machine name, account, tailnet, pairing code, token or pointer.
