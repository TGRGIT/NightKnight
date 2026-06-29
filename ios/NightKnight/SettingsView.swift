import SwiftUI
import UIKit
import UserNotifications
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
    @State private var showDisconnectConfirm = false
    /// Notification permission, so we can warn when alarms are on but iOS won't deliver.
    @State private var notifStatus: UNAuthorizationStatus = .notDetermined

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
                    // Only offered once there's a credential to remove.
                    if !token.isEmpty || !cfId.isEmpty || !cfSecret.isEmpty {
                        Button("Disconnect", role: .destructive) { showDisconnectConfirm = true }
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
            .navigationTitle("Settings")
            .task { notifStatus = await AlarmManager.shared.authorizationStatus() }
            .confirmationDialog("Disconnect from server?", isPresented: $showDisconnectConfirm, titleVisibility: .visible) {
                Button("Disconnect", role: .destructive) { disconnect() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Removes your device token and Cloudflare Access credentials from this iPhone, its widgets, and your Apple Watch.")
            }
        }
    }

    /// Remove the stored credentials everywhere: unregister this device's push token from the
    /// server (while we're still authenticated), clear the credentials and cached reading in the
    /// shared store, purge any legacy Keychain copies, mirror the cleared token to the Watch, and
    /// reload the widget/complication so they drop to "--" right away.
    private func disconnect() {
        // Unregister push first, from an immutable snapshot of the *current* credentials, so the
        // clear below can't pull them out from under the in-flight request.
        let apns = settings.apnsToken
        if !apns.isEmpty {
            let client = APIClient(settings: Settings.current())
            Task { try? await client.unregisterPush(token: apns) }
        }
        settings.clearCredentials()
        token = ""; cfId = ""; cfSecret = ""
        connStatus = nil; connOK = false
        PhoneSyncManager.shared.pushConfig()
        WidgetCenter.shared.reloadAllTimelines()
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
        // Register for silent push now that we may finally be configured. The token
        // callback at launch is a no-op until `isConfigured`, and the common path is to
        // configure the connection *after* first launch — so without this, the APNs token
        // wouldn't reach the server (and silent-push wake-ups wouldn't work) until the next
        // cold launch. Re-registering is cheap (iOS returns the cached token fast) and
        // re-fires `didRegisterForRemoteNotificationsWithDeviceToken`, which now POSTs it.
        if settings.isConfigured {
            UIApplication.shared.registerForRemoteNotifications()
        }
    }
}
