import SwiftUI

/// Connection, units, Apple Health, and alarm configuration. Alarms are fully
/// disableable via the master toggle.
struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    private let settings = Settings.shared
    @State private var baseURL = Settings.shared.baseURL
    @State private var token = Settings.shared.deviceToken
    @State private var cfId = Settings.shared.cfAccessClientId
    @State private var cfSecret = Settings.shared.cfAccessClientSecret
    @State private var connStatus: String?
    @State private var connOK = false
    @State private var isTesting = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Connection") {
                    TextField("Server URL", text: $baseURL).textInputAutocapitalization(.never).autocorrectionDisabled()
                    SecureField("Device token (api-secret)", text: $token)
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
                    SecureField("CF-Access-Client-Secret", text: $cfSecret)
                }
                Section("Display") {
                    Picker("Unit", selection: Binding(
                        get: { settings.preferredUnit },
                        set: { settings.preferredUnit = $0 }
                    )) {
                        ForEach(GlucoseUnit.allCases, id: \.self) { Text($0.label).tag($0) }
                    }
                }
                Section("Apple Health") {
                    Toggle("Read from Health", isOn: Binding(get: { settings.readFromHealthKit }, set: { settings.readFromHealthKit = $0 }))
                    Toggle("Write to Health", isOn: Binding(get: { settings.writeToHealthKit }, set: { settings.writeToHealthKit = $0 }))
                    Button("Authorize Apple Health") { Task { _ = await HealthKitManager.shared.requestAuth() } }
                }
                Section(header: Text("Alarms"),
                        footer: Text("On-device alarms for out-of-range and rapid drops. Simply on or off — there's no snooze. Nothing fires when disabled.")) {
                    Toggle("Enable alarms", isOn: Binding(get: { settings.alarmsEnabled }, set: { on in
                        settings.alarmsEnabled = on
                        if on { Task { await AlarmManager.shared.requestAuth() } }
                    }))
                    if settings.alarmsEnabled {
                        Stepper("Low: \(Int(settings.lowThresholdMgdl)) mg/dL",
                                value: Binding(get: { settings.lowThresholdMgdl }, set: { settings.lowThresholdMgdl = $0 }),
                                in: 50...110, step: 5)
                        Stepper("High: \(Int(settings.highThresholdMgdl)) mg/dL",
                                value: Binding(get: { settings.highThresholdMgdl }, set: { settings.highThresholdMgdl = $0 }),
                                in: 140...300, step: 5)
                        Toggle("Alert on rapid drop", isOn: Binding(get: { settings.fastDropAlarm }, set: { settings.fastDropAlarm = $0 }))
                    }
                }
            }
            .navigationTitle("Settings")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { save(); dismiss() }
                }
            }
        }
    }

    /// Save the typed values, then ping the API so the user knows it's reachable.
    private func testConnection() {
        save()
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

    private func save() {
        settings.baseURL = baseURL.trimmingCharacters(in: .whitespaces)
        settings.deviceToken = token.trimmingCharacters(in: .whitespaces)
        settings.cfAccessClientId = cfId.trimmingCharacters(in: .whitespaces)
        settings.cfAccessClientSecret = cfSecret.trimmingCharacters(in: .whitespaces)
        // Mirror the new config to the Apple Watch.
        PhoneSyncManager.shared.pushConfig()
    }
}
