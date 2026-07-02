import SwiftUI
import UIKit
import UniformTypeIdentifiers
import UserNotifications
import WidgetKit

/// Data source, credentials, units, target range, Apple Health, and alarm
/// configuration. Display/health/alarm preferences persist as you change them; the
/// SOURCE and its credentials are staged and only commit through "Save & activate" —
/// switching source or account is gated behind a destructive "delete cached data &
/// resync" confirmation so readings from two accounts can never mix.
struct SettingsView: View {
    private let settings = Settings.shared
    @State private var selectedSource: DataSource = Settings.shared.dataSource ?? .nightknight
    @State private var staged = SourceSetup.Staged(from: .shared)
    @State private var infoFor: DataSourceInfo?
    @State private var connStatus: String?
    @State private var connOK = false
    @State private var isTesting = false
    @State private var isSaving = false
    @State private var showSwitchConfirm = false
    @State private var showDisconnectConfirm = false
    @State private var showImporter = false
    @State private var importStatus: String?
    /// Notification permission, so we can warn when alarms are on but iOS won't deliver.
    @State private var notifStatus: UNAuthorizationStatus = .notDetermined

    private var unit: GlucoseUnit { settings.preferredUnit }

    var body: some View {
        NavigationStack {
            Form {
                sourceSection
                credentialsSection
                if selectedSource == .nightknight { cfAccessSection }
                actionsSection
                if let active = settings.dataSource, active.usesLocalAnalytics, active == selectedSource {
                    importSection
                }
                displaySection
                healthSection
                alarmsSection
            }
            .navigationTitle("Settings")
            .task { notifStatus = await AlarmManager.shared.authorizationStatus() }
            .sheet(item: $infoFor) { SourceInfoSheet(info: $0) }
            .confirmationDialog("Switch to \(selectedSource.label)?",
                                isPresented: $showSwitchConfirm, titleVisibility: .visible) {
                Button("Delete & switch", role: .destructive) { Task { await performSwitch() } }
                Button("Cancel", role: .cancel) { revertStaged() }
            } message: {
                Text("This deletes all locally cached glucose data so NightKnight can resync cleanly from the new source.")
            }
            .confirmationDialog("Disconnect from your data source?",
                                isPresented: $showDisconnectConfirm, titleVisibility: .visible) {
                Button("Disconnect", role: .destructive) { disconnect() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Removes every stored credential from this iPhone, its widgets, and your Apple Watch, and returns to the data-source chooser.")
            }
            .fileImporter(isPresented: $showImporter,
                          allowedContentTypes: [.commaSeparatedText, .plainText]) { result in
                if case .success(let url) = result { Task { await importCSV(url) } }
            }
        }
    }

    // MARK: - Sections

    private var sourceSection: some View {
        Section(header: Text("Data source"),
                footer: Text(sourceFooter)) {
            ForEach(DataSourceInfo.all) { info in
                HStack {
                    Button {
                        selectedSource = info.source
                        connStatus = nil
                    } label: {
                        HStack {
                            Text(info.title).foregroundStyle(.primary)
                            Spacer()
                            if selectedSource == info.source {
                                Image(systemName: "checkmark").foregroundStyle(Color.nkAccent)
                            }
                        }
                    }
                    Button {
                        infoFor = info
                    } label: {
                        Image(systemName: "questionmark.circle").foregroundStyle(Color.nkAccent)
                    }
                    .buttonStyle(.borderless)
                }
            }
        }
    }

    private var sourceFooter: String {
        guard let active = settings.dataSource else {
            return "No source active yet."
        }
        if active == selectedSource {
            return "\(active.label) is active."
        }
        return "\(active.label) is active — enter credentials below and tap Save & activate to switch."
    }

    private var credentialsSection: some View {
        Section("\(selectedSource.label) credentials") {
            SourceCredentialFields(source: selectedSource, staged: $staged)
        }
    }

    private var cfAccessSection: some View {
        Section(header: Text("Cloudflare Access (optional)"),
                footer: Text("A service token to pass the Access gate when deployed behind Cloudflare Access.")) {
            TextField("CF-Access-Client-Id", text: $staged.cfId).autocorrectionDisabled()
            SecureField("CF-Access-Client-Secret", text: $staged.cfSecret)
        }
    }

    private var actionsSection: some View {
        Section {
            Button(isTesting ? "Testing…" : "Test connection") { testConnection() }
                .disabled(isTesting || !staged.isComplete(for: selectedSource))
            Button(isSaving ? "Saving…" : "Save & activate") { saveAndActivate() }
                .disabled(isSaving || !staged.isComplete(for: selectedSource))
            if let connStatus {
                Text(connStatus).font(.caption)
                    .foregroundStyle(connOK ? Color.green : Color.nkAccent)
            }
            // Only offered once something is stored to remove.
            if settings.dataSource != nil {
                Button("Disconnect", role: .destructive) { showDisconnectConfirm = true }
            }
        }
    }

    private var importSection: some View {
        Section(header: Text("History"),
                footer: Text("Direct sources start with only the vendor's recent window; stats build up as readings accumulate. Import a Dexcom Clarity or LibreView CSV export for instant history — the format is auto-detected.")) {
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

    private var alarmsFooter: String {
        var text = "On-device alarms for out-of-range and rapid drops, using your target range above. Simply on or off — there's no snooze. Nothing fires when disabled."
        if settings.usesLocalAnalytics {
            // Be explicit about the real limitation of going serverless — don't let a
            // local source silently imply server-mode alarm timeliness.
            text += " With a direct source there is no server push, so background alarms are best-effort: iOS decides when the app may refresh, and a backgrounded alarm can fire late. Reliable background alarms need NightKnight (or a push-capable Nightscout setup)."
        }
        return text
    }

    // MARK: - Actions

    /// Test the STAGED credentials — persists nothing, so a typo can't clobber a
    /// working configuration.
    private func testConnection() {
        isTesting = true
        connStatus = nil
        Task {
            let result = await SourceSetup.test(selectedSource, staged: staged)
            connOK = result.ok
            connStatus = result.message
            isTesting = false
        }
    }

    /// Commit the staged source + credentials. If the local store holds another
    /// source/account's readings this is a SWITCH and must be confirmed (wipe &
    /// resync); otherwise it commits directly.
    private func saveAndActivate() {
        isSaving = true
        let newKey = staged.sourceKey(for: selectedSource)
        Task {
            if await SourceSetup.needsWipe(newKey: newKey) {
                isSaving = false
                showSwitchConfirm = true
            } else {
                await SourceSetup.activate(selectedSource, staged: staged, settings: settings, wipe: false)
                await afterActivate(didWipe: false)
            }
        }
    }

    /// The confirmed destructive path: wipe the local data, activate the new source,
    /// and resync from scratch.
    private func performSwitch() async {
        isSaving = true
        await SourceSetup.activate(selectedSource, staged: staged, settings: settings, wipe: true)
        await afterActivate(didWipe: true)
    }

    /// Cancel path of the switch dialog: nothing persisted — put the controls back to
    /// the active configuration.
    private func revertStaged() {
        staged = SourceSetup.Staged(from: settings)
        selectedSource = settings.dataSource ?? .nightknight
        isSaving = false
    }

    private func afterActivate(didWipe: Bool) async {
        connOK = true
        connStatus = "Saved ✓ — \(selectedSource.label) is active"
        // `||` can't await its right operand, so resolve the store check up front.
        var firstConnect = didWipe
        if !firstConnect {
            firstConnect = (try? await LocalStore.shared.isEmpty()) ?? false
        }
        if selectedSource == .nightscout, firstConnect {
            // First connect to this instance: pull its full history in the background.
            connStatus = "Saved ✓ — backfilling history from Nightscout…"
            Task.detached {
                let n = await SourceSetup.initialBackfill(settings: .shared)
                await MainActor.run {
                    if let n { connStatus = "Saved ✓ — imported \(n) readings from Nightscout" }
                }
            }
        }
        if settings.isConfigured && !settings.usesLocalAnalytics {
            // Server mode: register for silent push now that we may finally be
            // configured (the launch callback was a no-op until then).
            UIApplication.shared.registerForRemoteNotifications()
        }
        isSaving = false
    }

    /// Import a Clarity/LibreView CSV through the Rust importer (auto-detects the
    /// format) straight into the local store — instant backfill for direct sources.
    private func importCSV(_ url: URL) async {
        guard let engine = LocalAnalytics.engine else { return }
        importStatus = "Importing…"
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        do {
            let text = try String(contentsOf: url, encoding: .utf8)
            let data = try engine.importGlucoseCSV(text: text, tzOffsetMin: APIClient.tzOffsetMinutes)
            guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let entries = obj["entries"] as? [[String: Any]] else {
                importStatus = "Import failed: unexpected importer output."
                return
            }
            let rows: [(dateMs: Int64, mgdl: Double)] = entries.compactMap {
                guard let date = $0["date"] as? Double, let mgdl = $0["mgdl"] as? Double else { return nil }
                return (Int64(date), mgdl)
            }
            try await LocalStore.shared.upsertRows(rows, sourceKey: settings.sourceKey)
            let source = (obj["source"] as? String) ?? "csv"
            importStatus = "Imported \(rows.count) readings (\(source) export)."
            WidgetCenter.shared.reloadAllTimelines()
        } catch {
            let message = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
            importStatus = "Import failed: \(message)"
        }
    }

    /// Remove the stored credentials everywhere: unregister this device's push token
    /// from the server (while we're still authenticated), clear every source's
    /// credentials and the cached reading, mirror the sign-out to the Watch, and
    /// return to the first-run chooser.
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
