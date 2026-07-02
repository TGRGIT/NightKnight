import SwiftUI
import UIKit
import UniformTypeIdentifiers
import UserNotifications
import WidgetKit

/// Data source, credentials, units, target range, Apple Health, and alarm
/// configuration. Display/health/alarm preferences persist as you change them.
///
/// Exactly ONE data source is active at a time, and you cannot pick a different one
/// here — you must Disconnect first (which returns to the first-run chooser). This
/// keeps the "single active source" rule airtight: there is no in-place switch that
/// could blend two accounts' cached readings. The active source's credential fields
/// stay editable so you can rotate a password/token/secret without disconnecting.
struct SettingsView: View {
    private let settings = Settings.shared
    @State private var staged = SourceSetup.Staged(from: .shared)
    @State private var connStatus: String?
    @State private var connOK = false
    @State private var isTesting = false
    @State private var isSaving = false
    @State private var showDisconnectConfirm = false
    /// The source the user tapped while another is active — drives the "disconnect
    /// first" modal (nil = not shown).
    @State private var blockedTarget: DataSource?
    @State private var showImporter = false
    @State private var importStatus: String?
    /// Notification permission, so we can warn when alarms are on but iOS won't deliver.
    @State private var notifStatus: UNAuthorizationStatus = .notDetermined

    /// SettingsView only renders inside the tabs, which the root shows only once a
    /// source is chosen — so this is always non-nil in practice.
    private var activeSource: DataSource { settings.dataSource ?? .nightknight }
    private var unit: GlucoseUnit { settings.preferredUnit }

    var body: some View {
        NavigationStack {
            Form {
                sourceSection
                credentialsSection
                actionsSection
                if activeSource.usesLocalAnalytics { importSection }
                displaySection
                healthSection
                alarmsSection
            }
            .navigationTitle("Settings")
            .task { notifStatus = await AlarmManager.shared.authorizationStatus() }
            .sheet(item: infoBinding) { SourceInfoSheet(info: $0) }
            .alert("Disconnect to switch source",
                   isPresented: Binding(get: { blockedTarget != nil },
                                        set: { if !$0 { blockedTarget = nil } })) {
                Button("Disconnect \(activeSource.label)", role: .destructive) {
                    blockedTarget = nil
                    showDisconnectConfirm = true
                }
                Button("Cancel", role: .cancel) { blockedTarget = nil }
            } message: {
                Text("NightKnight keeps one data source active at a time so cached readings from different accounts can never mix. Disconnect \(activeSource.label) first — that clears its data — then pick \(blockedTarget?.label ?? "another source").")
            }
            .confirmationDialog("Disconnect from \(activeSource.label)?",
                                isPresented: $showDisconnectConfirm, titleVisibility: .visible) {
                Button("Disconnect", role: .destructive) { disconnect() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Removes its stored credentials and locally cached readings from this iPhone, its widgets, and your Apple Watch, and returns to the data-source chooser.")
            }
            .fileImporter(isPresented: $showImporter,
                          allowedContentTypes: [.commaSeparatedText, .plainText]) { result in
                if case .success(let url) = result { Task { await importCSV(url) } }
            }
        }
    }

    // Sheet for the "?" pros/cons; separate from `blockedTarget` so a card's info can be
    // read without triggering the switch-blocked alert.
    @State private var infoSource: DataSource?
    private var infoBinding: Binding<DataSourceInfo?> {
        Binding(get: { infoSource.map(DataSourceInfo.info(for:)) },
                set: { infoSource = $0?.source })
    }

    // MARK: - Sections

    private var sourceSection: some View {
        Section(header: Text("Data source"), footer: Text("\(activeSource.label) is active. To use a different source, disconnect first.")) {
            ForEach(DataSourceInfo.all) { info in
                HStack {
                    Button {
                        // Selecting a different source is blocked while one is active.
                        if info.source != activeSource { blockedTarget = info.source }
                    } label: {
                        HStack {
                            Text(info.title)
                                .foregroundStyle(info.source == activeSource ? .primary : .secondary)
                            Spacer()
                            if info.source == activeSource {
                                Image(systemName: "checkmark").foregroundStyle(Color.nkAccent)
                            } else {
                                Image(systemName: "lock").font(.footnote).foregroundStyle(.tertiary)
                            }
                        }
                    }
                    Button {
                        infoSource = info.source
                    } label: {
                        Image(systemName: "questionmark.circle").foregroundStyle(Color.nkAccent)
                    }
                    .buttonStyle(.borderless)
                }
            }
        }
    }

    private var credentialsSection: some View {
        Section("\(activeSource.label) credentials") {
            SourceCredentialFields(source: activeSource, staged: $staged)
        }
    }

    private var actionsSection: some View {
        Section {
            Button(isTesting ? "Testing…" : "Test connection") { testConnection() }
                .disabled(isTesting || !staged.isComplete(for: activeSource))
            Button(isSaving ? "Saving…" : "Save changes") { saveChanges() }
                .disabled(isSaving || !staged.isComplete(for: activeSource))
            if let connStatus {
                Text(connStatus).font(.caption)
                    .foregroundStyle(connOK ? Color.green : Color.nkAccent)
            }
            Button("Disconnect", role: .destructive) { showDisconnectConfirm = true }
        }
    }

    private var importSection: some View {
        Section(header: Text("History"),
                footer: Text("Import a Dexcom Clarity or LibreView CSV export to backfill up to \(SourceSetup.renderedHistoryDays) days at once — the most history NightKnight shows. The format is detected automatically.")) {
            Button("Import history CSV…") { showImporter = true }
            if let importStatus {
                Text(importStatus).font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var displaySection: some View {
        Section(header: Text("Display & targets"),
                footer: Text("Your target range shades the in-range band on the glucose chart and drives the on-device alarms. Analytics time-in-range always uses the consensus 70–180 mg/dL range.")) {
            Picker("Unit", selection: Binding(get: { settings.preferredUnit }, set: { settings.preferredUnit = $0 })) {
                ForEach(GlucoseUnit.allCases, id: \.self) { Text($0.label).tag($0) }
            }
            Stepper("Target low: \(targetText(settings.lowThresholdMgdl))",
                    value: Binding(get: { settings.lowThresholdMgdl }, set: { settings.lowThresholdMgdl = $0 }),
                    in: 50...110, step: 5)
            Stepper("Target high: \(targetText(settings.highThresholdMgdl))",
                    value: Binding(get: { settings.highThresholdMgdl }, set: { settings.highThresholdMgdl = $0 }),
                    in: 140...300, step: 5)
        }
    }

    private var healthSection: some View {
        Section("Apple Health") {
            Toggle("Read from Health", isOn: Binding(get: { settings.readFromHealthKit }, set: { settings.readFromHealthKit = $0 }))
            Toggle("Write to Health", isOn: Binding(get: { settings.writeToHealthKit }, set: { settings.writeToHealthKit = $0 }))
            Button("Authorize Apple Health") { Task { _ = await HealthKitManager.shared.requestAuth() } }
        }
    }

    private var alarmsSection: some View {
        Section(header: Text("Alarms"), footer: Text(alarmsFooter)) {
            Toggle("Enable alarms", isOn: Binding(get: { settings.alarmsEnabled }, set: { on in
                settings.alarmsEnabled = on
                if on { Task { notifStatus = await AlarmManager.shared.requestAuth() } }
            }))
            if settings.alarmsEnabled {
                Toggle("Alert on rapid drop", isOn: Binding(get: { settings.fastDropAlarm }, set: { settings.fastDropAlarm = $0 }))
                if notifStatus == .denied {
                    Label("Notifications are turned off for NightKnight in iOS Settings, so alarms can't alert you.",
                          systemImage: "exclamationmark.triangle.fill")
                        .font(.footnote).foregroundStyle(.orange)
                    Button("Open iOS Settings") {
                        if let url = URL(string: UIApplication.openSettingsURLString) {
                            UIApplication.shared.open(url)
                        }
                    }
                }
            }
        }
    }

    /// Alarm copy is deliberately cautious and mechanism-free — consistent with the
    /// first-launch safety notice. Alarms are on-device and most reliable in the
    /// foreground; background delivery is limited by iOS regardless of data source.
    private var alarmsFooter: String {
        "On-device alarms for out-of-range readings and rapid drops, using your target range above. They're most reliable while NightKnight is open — in the background iOS limits how often the app can check, so alerts may be delayed or missed. There's no snooze; nothing fires when alarms are off."
    }

    // MARK: - Actions

    /// Test the STAGED credentials — persists nothing, so a typo can't clobber a
    /// working configuration.
    private func testConnection() {
        isTesting = true
        connStatus = nil
        Task {
            let result = await SourceSetup.test(activeSource, staged: staged)
            connOK = result.ok
            connStatus = result.message
            isTesting = false
        }
    }

    /// Save credential edits to the ACTIVE source. Changing the account identity
    /// (server URL, username, email, region — anything that changes the source key)
    /// is a source switch, which is only allowed via Disconnect; block it with the
    /// same modal as tapping another source. A same-key edit (password / token /
    /// secret rotation) commits directly with no wipe.
    private func saveChanges() {
        let newKey = staged.sourceKey(for: activeSource)
        guard newKey == settings.sourceKey else {
            blockedTarget = activeSource
            return
        }
        isSaving = true
        connStatus = nil
        Task {
            await SourceSetup.activate(activeSource, staged: staged, settings: settings, wipe: false)
            connOK = true
            connStatus = "Saved ✓"
            if settings.isConfigured && !settings.usesLocalAnalytics {
                UIApplication.shared.registerForRemoteNotifications()
            }
            isSaving = false
        }
    }

    /// Import a Clarity/LibreView CSV into the active source's local store — instant
    /// backfill, trimmed to the rendered window.
    private func importCSV(_ url: URL) async {
        importStatus = "Importing…"
        do {
            let parsed = try SourceSetup.parseHistoryCSV(url)
            guard !parsed.rows.isEmpty else {
                importStatus = "No readings in the last \(SourceSetup.renderedHistoryDays) days were found in that file."
                return
            }
            try await LocalStore.shared.upsertRows(parsed.rows, sourceKey: settings.sourceKey)
            try await LocalStore.shared.prune(olderThanDays: SourceSetup.renderedHistoryDays,
                                              sourceKey: settings.sourceKey)
            await AnalyticsMemo.shared.clear()
            importStatus = "Imported \(parsed.rows.count) readings (\(parsed.source) export)."
            WidgetCenter.shared.reloadAllTimelines()
        } catch {
            let message = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
            importStatus = "Import failed: \(message)"
        }
    }

    /// Remove the active source's credentials everywhere, wipe its cached readings, and
    /// return to the first-run chooser (dataSource → nil).
    private func disconnect() {
        // Unregister push first, from an immutable snapshot of the *current* credentials,
        // so the clear below can't pull them out from under the in-flight request.
        let apns = settings.apnsToken
        if !apns.isEmpty && !settings.usesLocalAnalytics {
            let client = APIClient(settings: Settings.current())
            Task { try? await client.unregisterPush(token: apns) }
        }
        settings.clearCredentials()
        settings.dataSource = nil
        staged = SourceSetup.Staged(from: settings)
        connStatus = nil; connOK = false; importStatus = nil
        // Full sign-out: the local store must go back to genuinely ownerless, not just
        // "empty" — otherwise a later `WelcomeView` onboarding into a DIFFERENT source
        // inherits this account's owner stamp and every subsequent write throws
        // sourceMismatch forever (WelcomeView assumes a fresh, unclaimed store).
        Task {
            try? await LocalStore.shared.clear()
            await AnalyticsMemo.shared.clear()
        }
        PhoneSyncManager.shared.pushConfig()
        WidgetCenter.shared.reloadAllTimelines()
    }

    /// The target value rendered in the user's display unit (e.g. "70 mg/dL" / "3.9 mmol/L").
    private func targetText(_ mgdl: Double) -> String {
        "\(GlucoseValue(mgdl: mgdl).display(in: unit)) \(unit.label)"
    }
}
