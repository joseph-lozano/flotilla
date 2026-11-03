# Product

<!-- impeccable:product-schema 1 -->

> Written 2026-10-30 by the design-pass agent from the repo and Joseph's brief, without an interview: Joseph asked for the design calls to be made and corrected afterwards. Facts marked *(inferred)* are unconfirmed.

## Platform

macos — a native macOS desktop app in Rust on GPUI (gpui-kit 0.8.1). Not web; none of impeccable's platform values fit, and the detector doesn't apply.

## Users

One developer (Joseph, the author) running many coding agents at once. He starts a task, lets an agent work in its own cloud VM, switches to other threads, and comes back to read results, review pull requests and merge. *(inferred: other developers with a boat.dev account may use it later.)*

## Product Purpose

Flotilla runs coding-agent threads (Claude Code, Codex and others) in boat.dev VMs, one VM per thread, and keeps them in order: a sidebar of threads by state, a thread view that follows the agent live, and for GitHub projects a prepared VM per repo, a branch per thread, and the thread's pull request with its checks, fixes and merge. Success is many agents working in parallel without the user babysitting VMs, branches or setup.

## Positioning

A desktop control room for a fleet of agents on real cloud machines: each thread is a whole VM (idle ones stop on their own and wake on send), project threads start from a prepared copy of the repo in seconds, and the PR lives next to the conversation that made it.

## Operating Context

- Runs beside an editor and a browser on a Mac, for long sessions; glanced at between other work. Follows macOS light and dark.
- Threads are long-lived; the sidebar groups them (Unread, Idle, Working, Archived) and every row is scannable at a glance.
- Repos declare their own hooks: `.agents/setup`, `.agents/resume`, `.agents/archive`, and services in `.agents/services.yml` (Amp's format).
- GitHub through the gh CLI already signed in on the Mac; boat through an API key in the keychain.

## Capabilities and Constraints

- One VM per thread on boat.dev (paid plans only); idle VMs stop after 5/10/30 min; Keep running.
- Projects: prepared VM per repo, setup runs on every new commit to main, a branch per thread (named by the preview model), the PR bar with Fix / Resolve / Address review and Hold to merge.
- No-project threads: a blank VM.
- Terminology: "thread", "VM", "project", "setup", "branch", "pull request". The UI never says "template" (an implementation detail).
- Usage in full (the info button by the counts), but no prices, plans or upsell: boat.dev's billing page has those.

## Brand Commitments

- The app icon: three pixel sails (coral #F37173, amber #F6BC63, lavender #C2A7E2) with stepped edges on plum (#311E36). Joseph: use the plum colours and soft pixelation.
- Inspired by boat.dev (pixel sails on a duotone sea, small monospace captions for real commands), not a clone of it.
- Settled UI decisions (Joseph, 2026-10-29/30): sidebar groups and four-line rows with a hover card; no sidebar animation (a static green mark for working); the two-line thread header and the PR bar joined to the composer with Hold to merge; plain text for facts, real buttons for actions, big targets at the edges (Fitts's law).

## Evidence on Hand

The app itself (screens captured with the snapshot harness), the icon at design/icons/AppIcon.png, the demo videos under ~/Movies/Flotilla. No users, customers or metrics to cite.

## Product Principles

1. The agents do the work; the app keeps out of the way and makes state legible at a glance.
2. Real data reads as data: branches, SHAs, scripts and commands in monospace; prose for everything else.
3. Problems surface, successes stay quiet.
4. Every action is one obvious, generous target; destructive or irreversible ones ask for intent (hold to merge).
5. The brand lives in a few precise details (palette, the pixel sail), never in the way of the task.
