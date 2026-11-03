# Scheduled prompts

Joseph, 2026-11-01: "cron messages, or later messages … have the server be able to schedule or cron prompts to send, either to new threads, or to the same long running thread."

## What others do

**T3 Code** (`/tmp/t3code-latest`):
- **Data and storage.** A `scheduled_tasks` table holds the title, prompt, enabled flag, schedule (JSON), project, an optional bound thread, model selection and workspace strategy, plus `nextRunAt`, `lastRunAt`, `lastRunStatus`, `lastRunError` and `runCount`.
- **Timing.** Three kinds: an interval (at least a minute, anchored to when the last run finished), a fixed time of day with optional weekdays, or a webhook. It has no cron and no one-off. Fixed times use the **server's** time zone, and the docs warn that it may differ from your phone's. DST keeps the wall-clock time, with Temporal's "compatible" handling of gaps and repeats.
- **Target.**
  - **A new thread** launches with the stored model, project and workspace.
  - **A bound thread** gets the prompt as a follow-up using the thread's own model.
    - Busy: the prompt is **queued** after the active turn.
    - Archived: the run fails with "Thread is archived", and the task stays scheduled, so it fails again every period.
    - Deleted: its tasks are deleted too.
    - Stopped: the prompt still starts.
- **Missed runs.**
  - A fixed time more than 10 minutes late is skipped to the next occurrence. This is only logged.
  - An overdue interval runs once.
  - A run cut off by a restart is marked failed and not rerun.
  - "Resume at reset" continuations are armed on the thread and stored. After a restart, the first sweep fires every overdue one, with no limit on how late.
- **UI and bookkeeping.**
  - Settings → Scheduled tasks lists each task with "Next run in 3h", a status badge and the error in red.
  - Each row has a switch to pause, plus Edit, Run now and Delete. A thread's details list the tasks bound to it.
  - A scheduled message is a folded "Automation" bubble that links to its task.
  - Failures send no notification, and there is no run history.

**Amp and Amp Orbs** (ampcode.com/docs/orbs/automations, /news/schedule):
- An orb is a remote agent in its own machine that sleeps when idle.
- Agents set schedules from plain language in the prompt ("every morning at 9", "in two days"). There's no cron and no picker. A schedule runs once or repeats.
- By default a run continues the same thread. A run can also start a new thread.
- A scheduled thread shows an automation indicator with the schedule, next run, prompt, a pause switch and delete.
- Archiving the thread pauses its schedule. Snooze hides a thread until the next run brings it back.
- Undocumented: time zones, missed runs, limits and run history.

**Others:**
- Claude Code's scheduled tasks have presets plus custom cron. Times are entered in local time. Nothing may run more often than hourly. A one-off shows "Completed" after it runs.
- Codex makes the thread choice explicit: a standalone task starts a new chat, and an in-chat task returns to that chat.
- Cursor runs late but never early.

## What Flotilla does

**The server owns schedules.** They live in its database (table `schedules`) and run in its engine (`ScheduleWorker`). The always-on server runs them around the clock. This Mac's own server runs them only while the app is open: the Scheduled list says so when it's on This Mac.

**A schedule** is a prompt, a target and a time.
- **Target:**
  - **A new thread each time,** started as New thread starts one: agent, model, effort, fast, project, branch, environment.
  - **An existing thread,** sent as a follow-up. If a turn is running, the run **waits** until the turn ends, then sends, as T3 queues. At most one run waits per schedule: a time that comes round while one waits is taken up by it.
- **Timing:**
  - **Once,** at a time ("Send later").
  - **Daily at**, **weekdays at**, and **weekly on** chosen days at a time.
  - **A cron expression**: 5 fields, with ranges, lists, steps and names. When both day fields are restricted, either may match, as in Vixie cron.
  - Every time is in the **zone of the device that set it** (IANA, stored with the schedule), not the server's. This fixes T3's flaw for a remote Linux server.
  - DST keeps the wall-clock time. A time the clocks skip runs as far after the jump as it was into the gap (02:30 runs at 03:30), as Temporal's "compatible" rule does and T3 does. A time repeated in a fall-back runs once, at its first occurrence.
- **At most once a day** (Joseph, 2026-11-01), into a thread or starting new threads alike: "Runs must be at least a day apart." A one-off is unaffected.
  - Send later on Mac, Linux and iPhone offers nothing more often than daily. A cron stays, but must have one time of day (one minute, one hour): on the clock its runs are then whole days apart. A cron with two times or more runs twice on a day it matches, and is refused with that message.
  - The day is on the clock, so a change of the clocks never refuses a daily time, though a daily 09:00 is 23 real hours from the one before when they go forward.
  - **Every N hours**, once offered (1 to 24, counted from the first run), is no longer offered anywhere. Schedules set that way still read: every 24 hours runs on; anything shorter, like a cron with two times of day set before the rule, is **paused** at the server's next look with that reason, said once, the way a schedule that can't run as it is pauses. It won't resume, or keep a new prompt, until it's retimed; both editors open it as daily at its first run's time.

**Missed runs (T3's rules):**
- A daily, weekly or cron time missed by more than 10 minutes, because the server was off, is skipped to the next time. Flotilla records it on the row ("Missed …"), where T3 only logs it.
- An overdue every-24-hours run (one from before the daily rule) runs once, then keeps its own rhythm.
- A one-off is sent late, like T3's overdue continuations after a restart.

**Cost and limits.** A run that would start a VM needs boat to have starts to spare: the idle archiver's reserve (`Limits::spares_starts`, at least 5 left this hour). That covers a new thread, and waking a stopped thread to send to it. A run that can't happen is recorded on the row and reported once through the alert rule (`Note::ScheduleFailed` → `AlertKind::ScheduleFailed`). It is never retried in a loop:
- a one-off stays listed with the reason, and Run now tries it again;
- a recurring schedule waits for its next time.

Repeated failures for the same reason alert only once until a run works again. That includes a scheduled thread's failed start, which skips the usual start-failed alert.

**Archived, expired and deleted threads.** Following the hand-off rule, a run never un-archives a thread or starts an expired one. The run is recorded ("The thread is archived") and the schedule **pauses**, as Amp's archive does, and alerts once. T3 instead fails every period forever. A stopped VM is woken, as a follow-up would wake it, which matches T3's stopped threads, if boat has starts to spare. Deleting or forgetting a thread deletes its schedules (T3).

**Idle archiving leaves a scheduled thread alone.** Settings → Archiving → "Archive threads idle for" never archives a thread that a schedule which isn't paused sends to (one with a run to come, or waiting for its turn). Between a weekly schedule's runs its thread looks idle, and archiving it would pause the schedule. Archiving it yourself works as before, and pauses the schedule at its next run. A merged pull request still archives its thread.

**Bookkeeping.**
- Each schedule keeps its next run, its last run (when, what happened, the thread, why not), a count, whether it's paused, and whether a run is waiting for a turn.
- A one-off that ran is listed as done for a week, then goes.
- **Edits:** changing the prompt keeps the next run. Changing the time, or resuming, counts from now (T3). Run now leaves the next run alone.
- A prompt a schedule sent is marked (`SentPrompt.schedule`), and both apps show "Scheduled" above it. In a thread a schedule made, that is its first prompt, where you'd look for why the thread exists.

**Protocol.**
- Requests:
  - `schedule.put` creates or edits, by a client-made id, so sending it again is safe;
  - `schedule.pause` pauses or resumes;
  - `schedule.delete` deletes;
  - `schedule.run` is Run now.
- One view, `schedules`, with every schedule. Clients subscribe at start and diff by id. Rust and Swift change together, and the conformance session includes it.

## Left out, and why

- **Runs more often than once a day** (T3's intervals of a minute or more, Claude Code's hourly). Each run is a turn, and a new thread's run a VM start (Joseph, 2026-11-01).
- **Natural-language timing** (Amp) and **agents making schedules** (T3's MCP `schedule_task`). The pickers cover the cases, and an agent scheduling itself spends VM starts unseen.
- **Webhooks** (T3, Amp): another product.
- **Files with a scheduled prompt.** Uploads are swept, so a file attached for next week could be gone by then.
- **Run history** beyond the last run. Each run's thread is its own record, and the row says what the last one did.
- **Snooze** (Amp): the sidebar's Unread already brings a finished run back.
