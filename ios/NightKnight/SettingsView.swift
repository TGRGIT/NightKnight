import SwiftUI
import WidgetKit

/// Connection, units, target range, Apple Health, and alarm configuration. Presented as
/// a tab — edits persist as you make them. Alarms are fully disableable via the master
/// toggle.
struct SettingsView: View {
    private let settings = Settings.shared
    @State private var baseURL = Settings.shared.baseURL
    @State private var token = Settings.shared.deviceToken
    @State private var cfId = Settings.shared.cfAccessClientId
    @State private var cfSecret = Settings.shared.cfAccessClientSecret
    @State private var connStatus: String?
    @State private var connOK = false
    @State private var isTesting = false

    private var unit: GlucoseUnit { settings.preferredUnit }

    var body: some View {
        NavigationStack {
            Form {
                Section("Connection") {
                    TextField("Server URL", text: $baseURL)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                        .onChange(of: baseURL) { persist() }
                    SecureField("Device token (api-secret)", text: $token)
                        .onChange(of: token) { persist() }
                    Button(isTesting ? "Testing…" : "Test connection") { testConnection() }
                        .disabled(isTesting || baseURL.isEmpty || token.isEmpty)
                    if let connStatus {
                        Text(connStatus).font(.caption)
                            .foregroundStyle(connOK ? Color.green : Color.nkAccent)
                    }
                }
                Section(header: Text("Cloudflare Access (optional)"),
                        footer: Text("A service token to pass the Access gate when deployed behind Cloudflare Access.")) {
                    TextField("CF-Access-Client-Id", text: $cfId).autocorrectionDisabled()
                        .onChange(of: cfId) { persist() }
                    SecureField("CF-Access-Client-Secret", text: $cfSecret)
                        .onChange(of: cfSecret) { persist() }
                }
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
                Section("Apple Health") {
                    Toggle("Read from Health", isOn: Binding(get: { settings.readFromHealthKit }, set: { settings.readFromHealthKit = $0 }))
                    Toggle("Write to Health", isOn: Binding(get: { settings.writeToHealthKit }, set: { settings.writeToHealthKit = $0 }))
                    Button("Authorize Apple Health") { Task { _ = await HealthKitManager.shared.requestAuth() } }
                }
                Section(header: Text("Alarms"),
                        footer: Text("On-device alarms for out-of-range and rapid drops, using your target range above. Simply on or off — there's no snooze. Nothing fires when disabled.")) {
                    Toggle("Enable alarms", isOn: Binding(get: { settings.alarmsEnabled }, set: { on in
                        settings.alarmsEnabled = on
                        if on { Task { await AlarmManager.shared.requestAuth() } }
                    }))
                    if settings.alarmsEnabled {
                        Toggle("Alert on rapid drop", isOn: Binding(get: { settings.fastDropAlarm }, set: { settings.fastDropAlarm = $0 }))
                    }
                }
            }
            .navigationTitle("Settings")
        }
    }

    /// The target value rendered in the user's display unit (e.g. "70 mg/dL" / "3.9 mmol/L").
    private func targetText(_ mgdl: Double) -> String {
        "\(GlucoseValue(mgdl: mgdl).display(in: unit)) \(unit.label)"
    }

    /// Save the typed values, then ping the API so the user knows it's reachable.
    private func testConnection() {
        persist()
        isTesting = true
        connStatus = nil
        Task {
            do {
                let current = try await APIClient(settings: settings).current()
                connOK = true
                if let current {
                    let mins = Int(Date().timeIntervalSince(current.date) / 60)
                    connStatus = "Connected ✓ — last reading \(mins) min ago"
                } else {
                    connStatus = "Connected ✓ — no readings yet"
                }
            } catch {
                connOK = false
                connStatus = (error as? APIError)?.errorDescription ?? error.localizedDescription
            }
            isTesting = false
        }
    }

    /// Persist the connection credentials as they change, and mirror config to the Watch.
    private func persist() {
        settings.baseURL = baseURL.trimmingCharacters(in: .whitespaces)
        settings.deviceToken = token.trimmingCharacters(in: .whitespaces)
        settings.cfAccessClientId = cfId.trimmingCharacters(in: .whitespaces)
        settings.cfAccessClientSecret = cfSecret.trimmingCharacters(in: .whitespaces)
        PhoneSyncManager.shared.pushConfig()
        // The widget fetches independently; without an explicit reload it would keep
        // showing "--" until its next scheduled refresh (minutes away, and budget-
        // throttled). Reload now so it picks up the new URL/token immediately.
        WidgetCenter.shared.reloadAllTimelines()
    }
}
