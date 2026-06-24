import Foundation
import Observation

/// User settings, shared between the app and the widget via an App Group. Non-secret
/// values live in the shared `UserDefaults`; credentials live in the Keychain.
@Observable
final class Settings {
    static let appGroup = "group.be.cooney.nightknight"
    static let shared = Settings()

    private let defaults = UserDefaults(suiteName: Settings.appGroup) ?? .standard

    /// Base URL of your NightKnight deployment, e.g. `https://nightknight.cooney.be`.
    var baseURL: String { didSet { defaults.set(baseURL, forKey: "baseURL") } }
    var preferredUnit: GlucoseUnit { didSet { defaults.set(preferredUnit.rawValue, forKey: "unit") } }
    /// Remembered trailing-summary period, in days (1/7/14/30/90).
    var trailingDays: Int { didSet { defaults.set(trailingDays, forKey: "trailingDays") } }

    // Alarms (all disableable — `alarmsEnabled` is the master switch).
    var alarmsEnabled: Bool { didSet { defaults.set(alarmsEnabled, forKey: "alarmsEnabled") } }
    var lowThresholdMgdl: Double { didSet { defaults.set(lowThresholdMgdl, forKey: "low") } }
    var highThresholdMgdl: Double { didSet { defaults.set(highThresholdMgdl, forKey: "high") } }
    var fastDropAlarm: Bool { didSet { defaults.set(fastDropAlarm, forKey: "fastDrop") } }

    var writeToHealthKit: Bool { didSet { defaults.set(writeToHealthKit, forKey: "hkWrite") } }
    var readFromHealthKit: Bool { didSet { defaults.set(readFromHealthKit, forKey: "hkRead") } }

    // Credentials (Keychain).
    var deviceToken: String { didSet { Keychain.set("deviceToken", deviceToken) } }
    var cfAccessClientId: String { didSet { Keychain.set("cfId", cfAccessClientId) } }
    var cfAccessClientSecret: String { didSet { Keychain.set("cfSecret", cfAccessClientSecret) } }

    private init() {
        baseURL = defaults.string(forKey: "baseURL") ?? "https://nightknight.cooney.be"
        preferredUnit = GlucoseUnit(rawValue: defaults.string(forKey: "unit") ?? "") ?? .mgdl
        trailingDays = defaults.object(forKey: "trailingDays") as? Int ?? 7
        alarmsEnabled = defaults.object(forKey: "alarmsEnabled") as? Bool ?? false
        lowThresholdMgdl = defaults.object(forKey: "low") as? Double ?? 70
        highThresholdMgdl = defaults.object(forKey: "high") as? Double ?? 180
        fastDropAlarm = defaults.object(forKey: "fastDrop") as? Bool ?? true
        writeToHealthKit = defaults.object(forKey: "hkWrite") as? Bool ?? false
        readFromHealthKit = defaults.object(forKey: "hkRead") as? Bool ?? false
        deviceToken = Keychain.get("deviceToken")
        cfAccessClientId = Keychain.get("cfId")
        cfAccessClientSecret = Keychain.get("cfSecret")

        #if DEBUG
        // Test hook: let a simulator launch inject a server + token (SIMCTL_CHILD_*).
        let env = ProcessInfo.processInfo.environment
        if let url = env["NK_BASE_URL"], !url.isEmpty { baseURL = url }
        if let tok = env["NK_TOKEN"], !tok.isEmpty { deviceToken = tok }
        #endif
    }

    var isConfigured: Bool { !baseURL.isEmpty && !deviceToken.isEmpty }
}
