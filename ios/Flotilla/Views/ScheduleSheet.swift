import FlotillaKit
import SwiftUI

/// What Send later starts with: from a thread's composer, New thread, or Scheduled's Edit.
struct ScheduleDraft: Identifiable {
    let id = UUID()
    /// Editing this schedule; nil: new ones.
    var editing: String?
    var prompt: String
    /// One per target (New thread's models each get one).
    var targets: [ScheduleTarget]
    /// Where it goes, in words.
    var to: String
    var when: ScheduleTiming?
    /// Its times' zone: this phone's for a new one.
    var zone: String = TimeZone.current.identifier
}

/// Send later: the prompt, where it goes, once at a time or again and again (at most daily),
/// and when it runs next. The server checks it again, and says why if it won't keep it.
struct ScheduleSheet: View {
    enum Kind: String, CaseIterable, Identifiable {
        case daily = "Daily", weekdays = "Weekdays", weekly = "Weekly", cron = "Cron"
        var id: String { rawValue }
    }

    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let draft: ScheduleDraft
    /// Kept, with the prompt as saved (trimmed): only while this sheet is still up.
    var onSaved: (String) -> Void = { _ in }

    @State private var prompt = ""
    @State private var repeats = false
    @State private var kind: Kind = .daily
    @State private var once = Date()
    @State private var time = Date()
    @State private var days: Set<Int> = [1]
    @State private var cron = ""
    @State private var saving = false
    @State private var refused: String?
    @State private var ready = false
    /// Dismissed: a save still under way then says nothing to what's beneath.
    @State private var gone = false
    /// One id per target, made once: saved again after a refusal, one already kept is the
    /// same schedule, not another.
    @State private var ids: [String] = []
    @FocusState private var typing: Bool
    @FocusState private var typingCron: Bool

    private var calendar: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: draft.zone) ?? .current
        return c
    }

    private static func ms(_ d: Date) -> Int64 { Int64((d.timeIntervalSince1970 * 1000).rounded()) }

    /// A time of day (minutes after midnight) as the time picker holds it: on 15 January 2001,
    /// a day no zone's clocks changed, so every time is there. (On today's, a time the clocks
    /// skip would come back an hour on, and be saved so.)
    static func clockDate(_ at: Int, calendar: Calendar) -> Date {
        calendar.date(from: DateComponents(year: 2001, month: 1, day: 15, hour: at / 60, minute: at % 60)) ?? Date()
    }

    /// The time of day the picker holds, in minutes after midnight.
    static func minutes(_ d: Date, calendar: Calendar) -> Int {
        let parts = calendar.dateComponents([.hour, .minute], from: d)
        return (parts.hour ?? 9) * 60 + (parts.minute ?? 0)
    }
    private static func date(_ ms: Int64) -> Date { Date(timeIntervalSince1970: Double(ms) / 1000) }

    /// The timing chosen, as the server takes it.
    private var when: ScheduleTiming {
        // A one-off's time as set (to the ms), until the picker changes it.
        if !repeats, case .once(let at)? = draft.when, calendar.isDate(once, equalTo: Self.date(at), toGranularity: .minute) { return .once(atMs: at) }
        guard repeats else { return .once(atMs: Self.ms(calendar.date(bySetting: .second, value: 0, of: once) ?? once)) }
        let at = Self.minutes(time, calendar: calendar)
        switch kind {
        case .daily: return .daily(at: at)
        case .weekdays: return .weekdays(at: at)
        case .weekly: return .weekly(days: days.sorted(), at: at)
        case .cron: return .cron(cron.trimmingCharacters(in: .whitespaces))
        }
    }

    /// When it runs next, in words, or why it can't (the server checks a cron).
    private var nextLine: (String, Bool)? {
        let now = Self.ms(Date())
        let w = when
        if case .weekly(let d, _) = w, d.isEmpty { return ("Pick a day.", true) }
        if case .cron(let e) = w { return e.isEmpty ? nil : (Schedules.describe(w, locale: .current), false) }
        // An edit that keeps its one-off's time (waiting on its thread past it) isn't refused.
        let kept = draft.editing != nil && draft.when == w
        if case .once(let at) = w, at <= now - 60_000, !kept { return ("That time has passed.", true) }
        let first: Int64? = switch w {
        case .once(let at): at
        default: Schedules.nextAfter(w, zone: draft.zone, after: now)
        }
        guard let first else { return nil }
        let words = Schedules.whenWords(first, now: now, zone: TimeZone.current.identifier, locale: .current)
        return (w.isOnce ? "Sends \(words)" : "Next \(words)", false)
    }

    /// In an hour, tomorrow at 09:00, next Monday at 09:00 (when tomorrow isn't Monday).
    private var quick: [(String, Date)] {
        let now = Date()
        var out = [("In 1 hour", calendar.date(bySetting: .second, value: 0, of: now.addingTimeInterval(3600)) ?? now)]
        let today = calendar.startOfDay(for: now)
        let nine = { (d: Date) in self.calendar.date(bySettingHour: 9, minute: 0, second: 0, of: d) }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: today), let t = nine(tomorrow) {
            out.append(("Tomorrow \(Schedules.clock(540, locale: .current))", t))
            if calendar.component(.weekday, from: tomorrow) != 2,
               let monday = (2...7).compactMap({ calendar.date(byAdding: .day, value: $0, to: today) }).first(where: { calendar.component(.weekday, from: $0) == 2 }),
               let m = nine(monday) {
                out.append(("Monday \(Schedules.clock(540, locale: .current))", m))
            }
        }
        return out
    }

    var body: some View {
        let empty = prompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let next = nextLine
        NavigationStack {
            Form {
                Section {
                    TextField("What should the agent do?", text: $prompt, axis: .vertical)
                        .lineLimit(3...8)
                        .focused($typing)
                        .accessibilityIdentifier("laterPrompt")
                    LabeledContent("To") { Text(draft.to).lineLimit(1) }
                }
                Section {
                    Picker("When", selection: $repeats) {
                        Text("Once").tag(false)
                        Text("Repeat").tag(true)
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("laterMode")
                }
                Section {
                    if repeats {
                        Picker("Repeat", selection: $kind) {
                            ForEach(Kind.allCases) { Text($0.rawValue).tag($0) }
                        }
                        .accessibilityIdentifier("laterKind")
                        switch kind {
                        case .daily, .weekdays:
                            DatePicker("At", selection: $time, displayedComponents: .hourAndMinute)
                        case .weekly:
                            HStack(spacing: 6) {
                                ForEach(1...7, id: \.self) { d in
                                    let on = days.contains(d)
                                    Button {
                                        if on { days.remove(d) } else { days.insert(d) }
                                    } label: {
                                        Text(["M", "T", "W", "T", "F", "S", "S"][d - 1])
                                            .font(.footnote.weight(.semibold))
                                            .frame(maxWidth: .infinity, minHeight: 32)
                                            .foregroundStyle(on ? Palette.onPrimary : Palette.text)
                                            .background(Circle().fill(on ? Palette.primary : Palette.muted))
                                    }
                                    .buttonStyle(.plain)
                                    .accessibilityLabel(Calendar.current.weekdaySymbols[d % 7])
                                    .accessibilityAddTraits(on ? .isSelected : [])
                                }
                            }
                            DatePicker("At", selection: $time, displayedComponents: .hourAndMinute)
                        case .cron:
                            TextField("0 9 * * 1-5", text: $cron)
                                .font(.body.monospaced())
                                .autocorrectionDisabled()
                                .textInputAutocapitalization(.never)
                                .focused($typingCron)
                                .accessibilityIdentifier("laterCron")
                        }
                    } else {
                        DatePicker("At", selection: $once, in: (draft.editing == nil ? Date() : .distantPast)..., displayedComponents: [.date, .hourAndMinute])
                            .accessibilityIdentifier("laterAt")
                        HStack(spacing: 8) {
                            ForEach(quick, id: \.0) { label, date in
                                Button(label) { once = date }
                                    .buttonStyle(.bordered)
                                    .font(.footnote)
                            }
                        }
                    }
                } footer: {
                    if let refused {
                        Text(refused).foregroundStyle(Palette.bad).accessibilityIdentifier("laterProblem")
                    } else if let (words, bad) = next {
                        Text(draft.zone == TimeZone.current.identifier || !repeats ? words : "\(words) · times in \(draft.zone)")
                            .foregroundStyle(bad ? Palette.bad : Palette.dim)
                            .accessibilityIdentifier("laterNext")
                    }
                }
            }
            .environment(\.timeZone, TimeZone(identifier: draft.zone) ?? .current)
            .scrollDismissesKeyboard(.immediately)
            .scrollContentBackground(.hidden)
            .background(Palette.background)
            .navigationTitle(draft.editing == nil ? "Send later" : "Edit schedule")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    if saving {
                        ProgressView()
                    } else {
                        Button(draft.editing == nil ? "Schedule" : "Save") { save() }
                            .disabled(empty || next?.1 == true)
                            .accessibilityIdentifier("laterSave")
                    }
                }
            }
        }
        .onAppear(perform: setUp)
        .onDisappear { gone = true }
        .onChange(of: repeats) { _, _ in
            refused = nil
            typing = false
        }
        .onChange(of: kind) { _, _ in refused = nil }
        .onChange(of: cron) { _, _ in refused = nil }
    }

    private func setUp() {
        guard !ready else { return }
        ready = true
        ids = draft.targets.indices.map { $0 == 0 ? (draft.editing ?? UUID().uuidString.lowercased()) : UUID().uuidString.lowercased() }
        prompt = draft.prompt
        once = quick.first?.1 ?? Date()
        time = Self.clockDate(9 * 60, calendar: calendar)
        let atTime = { (at: Int) in Self.clockDate(at, calendar: self.calendar) }
        switch draft.when {
        case nil: break
        case .once(let at)?: once = Self.date(at)
        // Every N hours, set before runs had to be a day apart: daily at its first run's time,
        // for an edit to make it so.
        case .hours(_, let f)?:
            (repeats, kind, time) = (true, .daily, atTime(Self.minutes(Self.date(f), calendar: calendar)))
        case .daily(let at)?: (repeats, kind, time) = (true, .daily, atTime(at))
        case .weekdays(let at)?: (repeats, kind, time) = (true, .weekdays, atTime(at))
        case .weekly(let d, let at)?: (repeats, kind, days, time) = (true, .weekly, Set(d), atTime(at))
        case .cron(let e)?: (repeats, kind, cron) = (true, .cron, e)
        }
    }

    private func save() {
        let text = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        let w = when
        let specs = zip(ids, draft.targets).map { id, target in ScheduleSpec(id: id, prompt: text, target: target, when: w, zone: draft.zone) }
        // The keyboard down, so a refusal under the form shows.
        typing = false
        typingCron = false
        saving = true
        refused = nil
        Task {
            let why = await model.putSchedules(specs)
            saving = false
            if let why {
                refused = why
            } else if !gone {
                onSaved(text)
                dismiss()
            }
        }
    }
}

/// Every scheduled prompt: when it runs next, where to, how its last run went. Swipe to
/// delete or pause; tap to edit; hold for Run now.
struct SchedulesScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var editing: ScheduleDraft?

    var body: some View {
        let now = Format.nowMs()
        let here = TimeZone.current.identifier
        NavigationStack {
            List {
                ForEach(model.schedules) { s in
                    Button {
                        editing = ScheduleDraft(editing: s.id, prompt: s.prompt, targets: [s.target], to: model.scheduleTo(s.target), when: s.when, zone: s.zone)
                    } label: {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(s.prompt.split(whereSeparator: \.isNewline).first.map(String.init) ?? s.prompt)
                                .font(Typeface.body)
                                .foregroundStyle(Palette.text)
                                .lineLimit(1)
                            Text(Schedules.summary(s, now: now, here: here, locale: .current)).font(Typeface.meta).foregroundStyle(Palette.dim)
                            Text("To \(model.scheduleTo(s.target))").font(Typeface.meta).foregroundStyle(Palette.dim).lineLimit(1)
                            if let last = Schedules.lastWords(s, now: now, here: here, locale: .current) {
                                Text(last).font(Typeface.meta).foregroundStyle(s.last?.outcome == .failed ? Palette.bad : Palette.dim)
                            }
                        }
                        .opacity(s.paused || s.done ? 0.7 : 1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .listRowBackground(Palette.card)
                    .accessibilityIdentifier("schedule-\(s.id)")
                    .swipeActions(edge: .trailing) {
                        Button("Delete", role: .destructive) { model.deleteSchedule(s.id) }
                        if !s.done {
                            Button(s.paused ? "Resume" : "Pause") { model.pauseSchedule(s.id, paused: !s.paused) }.tint(Palette.dim)
                        }
                    }
                    .contextMenu {
                        Button("Run now", systemImage: "play") { model.runSchedule(s.id) }
                        if !s.done {
                            Button(s.paused ? "Resume" : "Pause", systemImage: s.paused ? "playpause" : "pause") { model.pauseSchedule(s.id, paused: !s.paused) }
                        }
                        Button("Delete", systemImage: "trash", role: .destructive) { model.deleteSchedule(s.id) }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(Palette.background)
            .navigationTitle("Scheduled")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .sheet(item: $editing) { ScheduleSheet(draft: $0) }
        }
    }
}

/// Over a thread's composer: what's scheduled to send there next. Opens Scheduled.
struct ScheduledLine: View {
    @Environment(AppModel.self) private var model
    let thread: String

    var body: some View {
        let live = model.schedules.filter { $0.target.thread == thread && !$0.done }
        if let first = live.first {
            Button {
                model.showSchedules = true
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "clock")
                    Text(Schedules.summary(first, now: Format.nowMs(), here: TimeZone.current.identifier, locale: .current) + (live.count > 1 ? " · \(live.count - 1) more" : ""))
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .font(Typeface.meta)
                .foregroundStyle(Palette.dim)
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("scheduledLine")
            Rectangle().fill(Palette.border).frame(height: 1)
        }
    }
}
