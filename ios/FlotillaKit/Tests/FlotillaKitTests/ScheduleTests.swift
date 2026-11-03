import Foundation
import Testing
@testable import FlotillaKit

/// Scheduled prompts as `flotilla_proto::schedule` writes them, and the words for a row as
/// `flotilla_model::schedule` says them (the same cases as its tests).
@Suite struct Schedules_ {
    let ny = "America/New_York"

    func at(_ zone: String, _ y: Int, _ m: Int, _ d: Int, _ h: Int, _ min: Int) -> Int64 {
        Schedules.localMs(zone, year: y, month: m, day: d, minutes: h * 60 + min)!
    }

    @Test func `requests write what Rust reads`() {
        let spec = ScheduleSpec(id: "s1", prompt: "Check CI", target: .thread("t1"), when: .weekly(days: [1, 3], at: 540), zone: "Europe/London")
        #expect(Up.req(id: 3, .putSchedule(spec)).json.text == #"{"id":3,"m":"schedule.put","p":{"id":"s1","prompt":"Check CI","target":{"thread":"t1"},"when":{"weekly":{"at":540,"days":[1,3]}},"zone":"Europe/London"},"t":"req"}"#)
        let fresh = FreshThread(environment: "base", projectID: nil, provider: "claude-code", model: "claude-haiku-5-5", effort: "low", fast: false)
        let new = ScheduleSpec(id: "s2", prompt: "p", target: .new(fresh), when: .cron("0 9 * * 1-5"), zone: "UTC")
        #expect(Up.req(id: 4, .putSchedule(new)).json.text == #"{"id":4,"m":"schedule.put","p":{"id":"s2","prompt":"p","target":{"new":{"effort":"low","environment":"base","fast":false,"machine_type":null,"model":"claude-haiku-5-5","on_branch":null,"project_id":null,"provider":"claude-code"}},"when":{"cron":{"expr":"0 9 * * 1-5"}},"zone":"UTC"},"t":"req"}"#)
        #expect(Up.req(id: 5, .pauseSchedule("s1", paused: true)).json.text == #"{"id":5,"m":"schedule.pause","p":{"paused":true,"schedule":"s1"},"t":"req"}"#)
        #expect(Up.req(id: 6, .deleteSchedule("s1")).json.text == #"{"id":6,"m":"schedule.delete","p":"s1","t":"req"}"#)
        #expect(Up.req(id: 7, .runSchedule("s1")).json.text == #"{"id":7,"m":"schedule.run","p":"s1","t":"req"}"#)
        #expect(Request.putSchedule(spec).resendable && !Request.runSchedule("s1").resendable)
    }

    @Test func `the view reads each timing and target, and leaves out one it can't`() throws {
        let data = try JSON(parsing: #"""
        {"schedules":[
          {"id":"a","prompt":"p","target":{"thread":"t1"},"when":{"once":{"at_ms":5}},"zone":"UTC","paused":false,"next_ms":5,"waiting_ms":null,"last":null,"runs":0,"from":"Mac","created_ms":1,"updated_ms":1},
          {"id":"b","prompt":"p","target":{"new":{"environment":"base","project_id":"p1","provider":"codex","model":"gpt-6-luna","effort":null,"fast":true,"machine_type":"small","on_branch":{"name":"fix","base":"main"}}},"when":{"hours":{"every":3,"from_ms":9}},"zone":"UTC","paused":true,"next_ms":null,"waiting_ms":7,"last":{"at_ms":2,"outcome":"failed","thread":null,"why":"boat has no VM starts to spare this hour."},"runs":4,"from":"Phone","created_ms":1,"updated_ms":2},
          {"id":"c","prompt":"p","target":{"thread":"t1"},"when":{"lunar":{"phase":"full"}},"zone":"UTC","paused":false,"next_ms":null,"waiting_ms":null,"last":{"at_ms":3,"outcome":"exploded"},"runs":0,"from":"","created_ms":1,"updated_ms":1}
        ]}
        """#)
        guard case .schedules(let all) = try ServerView(key: .schedules, data: data) else { Issue.record("not schedules"); return }
        #expect(all.map(\.id) == ["a", "b"])
        #expect(all[0].when == .once(atMs: 5) && all[0].target == .thread("t1"))
        let b = all[1]
        #expect(b.when == .hours(every: 3, fromMs: 9) && b.paused && b.waitingMs == 7 && b.runs == 4)
        #expect(b.target == .new(FreshThread(environment: "base", projectID: "p1", provider: "codex", model: "gpt-6-luna", effort: nil, fast: true, machineType: "small", onBranch: OnBranch(name: "fix", base: "main"))))
        // Put back as it came (an edit of its prompt keeps its machine).
        if case .new(let f) = b.target { #expect(f.json["machine_type"]?.string == "small") }
        #expect(b.last == ScheduleRun(atMs: 2, outcome: .failed, why: "boat has no VM starts to spare this hour."))
        #expect(ViewKey(fields: ["view": "schedules"]) == .schedules)
        #expect(Delta.keyed(.schedules, "schedules") == ["id"])
        let note = try #require(Down(text: #"{"t":"note","note":{"kind":"schedule_failed","data":{"schedule":"s1","prompt":"Check CI","thread":null,"why":"The thread is archived."}}}"#))
        guard case .note(let n, _) = note, let f = n.scheduleFailed else { Issue.record("\(note)"); return }
        #expect(f.schedule == "s1" && f.thread == nil && f.why == "The thread is archived.")
    }

    @Test func `words for each timing`() {
        #expect(Schedules.describe(.once(atMs: 0)) == "Once")
        #expect(Schedules.describe(.hours(every: 1, fromMs: 0)) == "Every hour")
        #expect(Schedules.describe(.hours(every: 6, fromMs: 0)) == "Every 6 hours")
        #expect(Schedules.describe(.daily(at: 540)) == "Daily at 09:00")
        #expect(Schedules.describe(.weekdays(at: 510)) == "Weekdays at 08:30")
        #expect(Schedules.describe(.weekly(days: [1], at: 1080)) == "Mondays at 18:00")
        #expect(Schedules.describe(.weekly(days: [5, 1, 3], at: 1080)) == "Mon, Wed and Fri at 18:00")
        #expect(Schedules.describe(.weekly(days: [1, 2, 3, 4, 5], at: 0)) == "Weekdays at 00:00")
        #expect(Schedules.describe(.cron("0  9 * *   1-5")) == "0 9 * * 1-5 (cron)")
        // On the phone, times as its locale writes them.
        #expect(Schedules.describe(.daily(at: 1290), locale: Locale(identifier: "en_US")) == "Daily at 9:30\u{202F}PM")
        #expect(Schedules.describe(.daily(at: 1290), locale: Locale(identifier: "en_GB")) == "Daily at 21:30")
    }

    @Test func `rows say when and how it went`() {
        // Wednesday 4 November 2026, 12:00 in New York.
        let now = at(ny, 2026, 11, 4, 12, 0)
        #expect(Schedules.whenWords(at(ny, 2026, 11, 4, 18, 30), now: now, zone: ny) == "today 18:30")
        #expect(Schedules.whenWords(at(ny, 2026, 11, 5, 9, 0), now: now, zone: ny) == "tomorrow 09:00")
        #expect(Schedules.whenWords(at(ny, 2026, 11, 3, 9, 0), now: now, zone: ny) == "yesterday 09:00")
        #expect(Schedules.whenWords(at(ny, 2026, 11, 7, 9, 0), now: now, zone: ny) == "Sat 09:00")
        #expect(Schedules.whenWords(at(ny, 2026, 11, 16, 9, 0), now: now, zone: ny) == "Mon 16 Nov 09:00")
        #expect(Schedules.whenWords(at(ny, 2027, 1, 4, 9, 0), now: now, zone: ny) == "Mon 4 Jan 2027 09:00")
        let s = Schedule(id: "s", prompt: "p", target: .thread("t"), when: .daily(at: 540), zone: ny, nextMs: at(ny, 2026, 11, 5, 9, 0), last: ScheduleRun(atMs: at(ny, 2026, 11, 4, 9, 0), outcome: .sent), runs: 3)
        #expect(Schedules.summary(s, now: now, here: ny) == "Daily at 09:00 · next tomorrow 09:00")
        #expect(Schedules.lastWords(s, now: now, here: ny) == "Last sent today 09:00")
        #expect(Schedules.summary(s, now: now, here: "Europe/London") == "Daily at 09:00 (America/New_York) · next tomorrow 14:00")
        var paused = s
        paused.paused = true
        paused.nextMs = nil
        #expect(Schedules.summary(paused, now: now, here: ny) == "Daily at 09:00 · paused")
        var waiting = s
        waiting.waitingMs = now
        #expect(Schedules.summary(waiting, now: now, here: ny) == "Daily at 09:00 · waiting for the turn to end")
        var failed = s
        failed.last = ScheduleRun(atMs: at(ny, 2026, 11, 4, 9, 0), outcome: .failed, why: "The thread is archived.")
        #expect(Schedules.lastWords(failed, now: now, here: ny) == "Didn't run today 09:00: the thread is archived.")
        var once = s
        once.when = .once(atMs: at(ny, 2026, 11, 5, 9, 0))
        once.last = nil
        #expect(Schedules.summary(once, now: now, here: ny) == "Once · tomorrow 09:00")
        once.nextMs = nil
        once.last = ScheduleRun(atMs: at(ny, 2026, 11, 4, 9, 1), outcome: .sent)
        #expect(Schedules.summary(once, now: now, here: ny) == "Once · sent today 09:01" && Schedules.lastWords(once, now: now, here: ny) == nil)
        #expect(once.done)
    }

    @Test func `the form's next run keeps wall-clock times across the clocks changing`() {
        // Spring forward in New York, Sunday 8 March 2026: 09:00 stays 09:00; 02:30 runs at 03:30.
        let sat = at(ny, 2026, 3, 7, 12, 0)
        #expect(Schedules.nextAfter(.daily(at: 540), zone: ny, after: sat) == at(ny, 2026, 3, 8, 9, 0))
        #expect(Schedules.nextAfter(.daily(at: 150), zone: ny, after: sat) == at(ny, 2026, 3, 8, 3, 30))
        // Weekdays from Friday evening: Monday.
        #expect(Schedules.nextAfter(.weekdays(at: 570), zone: ny, after: at(ny, 2025, 11, 7, 18, 0)) == at(ny, 2025, 11, 10, 9, 30))
        #expect(Schedules.nextAfter(.weekly(days: [1, 3, 7], at: 1200), zone: ny, after: at(ny, 2025, 11, 3, 21, 0)) == at(ny, 2025, 11, 5, 20, 0))
        // Every 3 hours counts real hours from the first run.
        let from = at(ny, 2026, 3, 7, 22, 0)
        #expect(Schedules.nextAfter(.hours(every: 3, fromMs: from), zone: ny, after: from) == from + 3 * 3_600_000)
        #expect(Schedules.nextAfter(.once(atMs: 9), zone: ny, after: 9) == nil)
        #expect(Schedules.nextAfter(.cron("0 9 * * *"), zone: ny, after: 0) == nil)
    }
}
