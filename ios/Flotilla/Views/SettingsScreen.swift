import FlotillaKit
import SwiftUI

/// The server's Settings, as far as a phone goes: its setup checks (read-only, with the
/// commands that fix them, to run on the server), its devices, and how the app looks.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Phone.self) private var phone
    @Environment(\.dismiss) private var dismiss
    @AppStorage("appearance") private var appearance: Appearance = .phone
    @State private var code: PairingCode?
    @State private var making = false
    @State private var revoking: DeviceRow?
    @State private var renaming: DeviceRow?
    @State private var newName = ""
    @State private var forgetting: PairedServer?

    var body: some View {
        NavigationStack {
            List {
                server
                SetupSection()
                archiving
                devices
                Section {
                    Picker("Appearance", selection: $appearance) {
                        ForEach(Appearance.allCases) { Text($0.label).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .listRowBackground(Palette.card)
                    .accessibilityIdentifier("appearance")
                } header: {
                    title("Appearance")
                }
                notifications
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(Palette.background)
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .sheet(item: $code) { AddDevice(code: $0) }
            .confirmationDialog("Forget \(forgetting?.name ?? "this server")?", isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }), titleVisibility: .visible, presenting: forgetting) { _ in
                Button("Forget", role: .destructive) {
                    dismiss()
                    phone.forget()
                }
            } message: { _ in
                Text("The phone leaves the server and forgets its pairing. Pair again with a new code.")
            }
            .confirmationDialog("Revoke \(revoking?.name ?? "the device")?", isPresented: Binding(get: { revoking != nil }, set: { if !$0 { revoking = nil } }), titleVisibility: .visible, presenting: revoking) { d in
                Button("Revoke", role: .destructive) { model.revoke(device: d.id) }
            } message: { _ in
                Text("Its token stops working and it disconnects. It can pair again with a new code.")
            }
            .alert("Rename device", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } }), presenting: renaming) { d in
                TextField("Name", text: $newName)
                    .accessibilityIdentifier("deviceNameField")
                Button("Cancel", role: .cancel) {}
                Button("Rename") { model.rename(device: d.id, to: newName) }
                    .disabled(newName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("deviceNameSave")
            }
        }
        .onAppear { model.watchDevices(true) }
        .task { await model.notifications.refresh() }
        .onDisappear { model.watchDevices(false) }
    }

    private func title(_ s: String) -> some View {
        Text(s).font(.caption.weight(.semibold)).textCase(.uppercase).kerning(0.6).foregroundStyle(Palette.dim)
    }

    // MARK: Server

    /// The server this phone is paired with: pairing with a different one (in its place), and
    /// forgetting it (back to pairing).
    @ViewBuilder private var server: some View {
        if let s = model.server {
            Section {
                Text(s.name)
                    .foregroundStyle(Palette.text)
                    .listRowBackground(Palette.card)
                    .accessibilityIdentifier("server")
                Button("Pair with a different server", systemImage: "qrcode") {
                    model.pairAfterSettings = true
                    dismiss()
                }
                .listRowBackground(Palette.card)
                .accessibilityIdentifier("pairDifferent")
                Button("Forget \(s.name)", systemImage: "minus.circle", role: .destructive) { forgetting = s }
                    .listRowBackground(Palette.card)
                    .accessibilityIdentifier("forgetServer")
            } header: {
                title("Server")
            }
        }
    }

    // MARK: Notifications

    /// What iOS allows: on; off, with the way to Flotilla's page in iOS's Settings; or not
    /// asked yet.
    @ViewBuilder private var notifications: some View {
        if let access = model.notifications.access {
            Section {
                switch access {
                case .allowed:
                    Text("On").foregroundStyle(Palette.text)
                        .listRowBackground(Palette.card)
                        .accessibilityIdentifier("notificationsOn")
                case .denied:
                    HStack {
                        Text("Off").foregroundStyle(Palette.text)
                            .accessibilityIdentifier("notificationsOff")
                        Spacer()
                        Button("Open Settings") {
                            if let url = URL(string: UIApplication.openNotificationSettingsURLString) { UIApplication.shared.open(url) }
                        }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier("openNotificationSettings")
                    }
                    .listRowBackground(Palette.card)
                case .notAsked:
                    Button("Turn on", systemImage: "bell") { Task { await model.notifications.ask() } }
                        .listRowBackground(Palette.card)
                        .accessibilityIdentifier("askNotifications")
                }
            } header: {
                title("Notifications")
            }
        }
    }

    // MARK: Archiving

    /// The server archiving threads by itself: when their pull request merges, and after days
    /// idle.
    private var archiving: some View {
        let s = model.settings
        return Section {
            Toggle(isOn: Binding(get: { s?.archiveOnMerge ?? true }, set: { model.set(.archiveOnMerge($0)) })) {
                Text("Archive a thread when its pull request merges").font(Typeface.body).foregroundStyle(Palette.text)
            }
            .tint(Palette.primary)
            .listRowBackground(Palette.card)
            .accessibilityIdentifier("archiveOnMerge")
            Picker(selection: Binding(get: { s?.archiveIdleDays ?? 0 }, set: { model.set(.archiveIdleDays($0)) })) {
                ForEach(Setting.idleDayChoices, id: \.self) { d in
                    Text(d == 0 ? "Never" : "\(d) days").tag(d)
                }
            } label: {
                Text("Archive threads idle for").font(Typeface.body).foregroundStyle(Palette.text)
            }
            .pickerStyle(.menu)
            .listRowBackground(Palette.card)
            .accessibilityIdentifier("archiveIdleDays")
        } header: {
            title("Archiving")
        }
        .disabled(s == nil)
    }

    // MARK: Devices

    private var devices: some View {
        Section {
            if let list = model.devices {
                ForEach(list) { d in
                    let mine = d.id == model.server?.device
                    HStack(spacing: 12) {
                        Image(systemName: d.agent ? "terminal" : ["ios", "android"].contains(d.platform) ? "iphone" : "laptopcomputer").font(.body).foregroundStyle(Palette.dim).frame(width: 22)
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(d.name).font(Typeface.body.weight(.semibold)).foregroundStyle(Palette.text).lineLimit(1)
                                if mine { Text("This device").font(Typeface.meta).foregroundStyle(Palette.dim) }
                            }
                            HStack(spacing: 4) {
                                Text("\(d.agent ? "Agent" : platform(d.platform)) ·").foregroundStyle(Palette.dim)
                                if d.connected { Text("Connected").foregroundStyle(Palette.ok) } else { Text("Last seen \(Format.agoSpoken(d.lastSeenMs))").foregroundStyle(Palette.dim) }
                            }
                            .font(Typeface.meta)
                        }
                        Spacer(minLength: 4)
                        Menu {
                            deviceActions(d, mine: mine)
                        } label: {
                            Image(systemName: "ellipsis")
                                .font(.body.weight(.medium))
                                .foregroundStyle(Palette.dim)
                                .frame(width: 36, height: 36)
                                .contentShape(Rectangle())
                        }
                        .accessibilityLabel("\(d.name), more")
                        .accessibilityIdentifier("device-\(d.name)")
                    }
                    .padding(.vertical, 2)
                    .listRowBackground(Palette.card)
                    .contextMenu { deviceActions(d, mine: mine) }
                    .accessibilityElement(children: .contain)
                }
            } else {
                ProgressView().controlSize(.small).listRowBackground(Palette.card)
            }
        } header: {
            HStack {
                title("Devices")
                Spacer()
                Button {
                    making = true
                    Task {
                        switch await model.pairingCode() {
                        case .success(let c): code = c
                        case .failure(let e): model.problem = "Couldn't make a pairing code: \(e.message)"
                        }
                        making = false
                    }
                } label: {
                    if making { ProgressView().controlSize(.mini) } else { Label("Add device", systemImage: "plus").font(.footnote.weight(.semibold)).textCase(nil) }
                }
                .disabled(making)
                .accessibilityIdentifier("addDevice")
            }
        }
    }

    /// Rename any device (this phone too); revoke another.
    @ViewBuilder private func deviceActions(_ d: DeviceRow, mine: Bool) -> some View {
        Button("Rename…", systemImage: "pencil") {
            newName = d.name
            renaming = d
        }
        if !mine {
            Button("Revoke…", systemImage: "minus.circle", role: .destructive) { revoking = d }
        }
    }

    private func platform(_ p: String) -> String {
        switch p {
        case "macos": "Mac"
        case "ios": "iPhone"
        case "linux": "Linux"
        case "android": "Android"
        case "": "Device"
        default: p
        }
    }
}

/// Server setup: one line, how many of the checks pass ("6/7"), until it's opened; open by
/// itself when one fails or warns (it needs you). `fixed`: always open, where New thread would
/// be while threads can't run.
struct SetupSection: View {
    @Environment(AppModel.self) private var model
    var fixed = false
    @State private var checking = false
    /// The setup checks shown in full (nil: as their health says).
    @State private var setupOpen: Bool?

    var body: some View {
        let checks = model.settings?.checks
        let (ok, of) = model.settings?.tally ?? (0, 0)
        let needsYou = checks?.contains { $0.health == "fail" || $0.health == "warn" } ?? false
        let open = fixed || (setupOpen ?? needsYou)
        Section {
            if let checks {
                if open { ForEach(Array(checks.enumerated()), id: \.offset) { _, c in CheckRow(check: c) } }
            } else {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Checking…").font(Typeface.label).foregroundStyle(Palette.dim)
                }
                .listRowBackground(Palette.card)
            }
        } header: {
            HStack {
                let title = HStack(spacing: 6) {
                    if !fixed {
                        Image(systemName: open ? "chevron.down" : "chevron.right").font(.caption2.weight(.semibold)).foregroundStyle(Palette.dim)
                    }
                    Text("Server setup").font(.caption.weight(.semibold)).textCase(.uppercase).kerning(0.6).foregroundStyle(Palette.dim)
                    if let checks, !checks.isEmpty {
                        Text("\(ok)/\(of)").font(.caption.weight(.semibold)).foregroundStyle(ok == of ? Palette.dim : Palette.text)
                    }
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("Server setup, \(ok) of \(of) ready")
                if fixed {
                    title
                } else {
                    Button { setupOpen = !open } label: { title }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("setupToggle")
                }
                Spacer()
                Button {
                    checking = true
                    Task {
                        await model.checkAgain()
                        checking = false
                    }
                } label: {
                    if checking { ProgressView().controlSize(.mini) } else { Text("Check again").font(.footnote.weight(.semibold)).textCase(nil) }
                }
                .disabled(checking)
                .accessibilityIdentifier("checkAgain")
            }
        }
    }
}

/// The server's setup in New thread's place, while a check threads need fails: every check,
/// opened, with the commands that fix it.
struct SetupScreen: View {
    var body: some View {
        List { SetupSection(fixed: true) }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(Palette.background)
    }
}

/// One setup check: how it went, what was found, and each command that fixes it, with Copy.
private struct CheckRow: View {
    let check: Diagnosis

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: icon).font(.footnote.weight(.semibold)).foregroundStyle(color).frame(width: 16)
                VStack(alignment: .leading, spacing: 1) {
                    Text(check.label).font(Typeface.body.weight(.semibold)).foregroundStyle(Palette.text)
                    Text(check.says).font(Typeface.label).foregroundStyle(check.health == "fail" ? Palette.bad : Palette.dim).textSelection(.enabled)
                }
            }
            ForEach(check.fix, id: \.self) { fix in
                HStack(spacing: 6) {
                    Text(fix)
                        .font(Typeface.mono(.caption))
                        .foregroundStyle(Palette.text)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 6)
                        .background(RoundedRectangle(cornerRadius: 6, style: .continuous).fill(Palette.muted))
                    Button {
                        UIPasteboard.general.string = fix
                    } label: {
                        Image(systemName: "doc.on.doc").font(.footnote).foregroundStyle(Palette.dim).frame(width: 32, height: 32).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Copy \(fix)")
                }
                .padding(.leading, 24)
            }
        }
        .padding(.vertical, 3)
        .listRowBackground(Palette.card)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("check-\(check.check)")
    }

    private var icon: String {
        switch check.health {
        case "ok": "checkmark.circle.fill"
        case "warn": "exclamationmark.triangle.fill"
        case "fail": "xmark.circle.fill"
        default: "minus.circle"
        }
    }

    private var color: Color {
        switch check.health {
        case "ok": Palette.ok
        case "warn": Palette.warn
        case "fail": Palette.bad
        default: Palette.dim
        }
    }
}

/// Add device: the one-time code, its link, and the QR code another phone scans.
struct AddDevice: View {
    let code: PairingCode
    @Environment(\.dismiss) private var dismiss
    @State private var copied = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    if let qr = QR.image(for: code.link, scale: 10) {
                        Image(qr, scale: 1, label: Text("QR code for the pairing link"))
                            .interpolation(.none)
                            .resizable()
                            .scaledToFit()
                            .frame(width: 220, height: 220)
                            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                            .accessibilityIdentifier("pairQR")
                    }
                    Text(Pairing.display(code: code.code))
                        .font(.custom("Menlo", size: 24, relativeTo: .title2))
                        .kerning(2)
                        .foregroundStyle(Palette.text)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("pairCodeShown")
                    TimelineView(.periodic(from: .now, by: 1)) { _ in
                        let left = code.expiresMs - Format.nowMs()
                        Text(left > 0 ? Format.timeLeft(left) : "Expired: make another")
                            .font(Typeface.label)
                            .foregroundStyle(left > 0 ? Palette.dim : Palette.bad)
                    }
                    Text(code.link)
                        .font(Typeface.mono(.caption))
                        .foregroundStyle(Palette.dim)
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 24)
                    HStack(spacing: 10) {
                        Button {
                            UIPasteboard.general.string = code.link
                            copied = true
                        } label: {
                            Label(copied ? "Copied" : "Copy link", systemImage: copied ? "checkmark" : "doc.on.doc")
                        }
                        .buttonStyle(.bordered)
                        if let url = URL(string: code.link) {
                            ShareLink(item: url) { Label("Share", systemImage: "square.and.arrow.up") }.buttonStyle(.bordered)
                        }
                    }
                    .font(.footnote.weight(.semibold))
                }
                .padding(.top, 24)
                .frame(maxWidth: .infinity)
            }
            .background(Palette.background)
            .navigationTitle("Add device")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        .presentationDetents([.large])
    }
}

extension PairingCode: @retroactive Identifiable {
    public var id: String { link }
}
