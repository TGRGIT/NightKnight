import Foundation
import Observation

/// User settings, shared between the app, the widget and the watch. Non-secret values
/// live in the shared App Group `UserDefaults`; the credentials live in a **shared
/// Keychain access group** (`keychain-access-groups` entitlement on every target) so the
/// widget/watch — separate processes — can read them WITHOUT giving up the Keychain's
/// device-only, backup-excluded protection (`…ThisDeviceOnly`). A `kSecAttrAccessGroup`
/// isn't set in queries: with a single shared group in the entitlement it becomes the
/// default group, so the existing `Keychain` helper shares automatically.
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

    // Credentials. Stored in the SHARED Keychain access group — readable by the widget and
    // watch, but still device-only and excluded from backups (unlike App-Group UserDefaults).
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
        deviceToken = Settings.credential(defaults, "deviceToken")
        cfAccessClientId = Settings.credential(defaults, "cfId")
        cfAccessClientSecret = Settings.credential(defaults, "cfSecret")

        #if DEBUG
        // Test hook: let a simulator launch inject a server + token (SIMCTL_CHILD_*).
        // Persist the token to the shared Keychain so the widget's process picks it up.
        let env = ProcessInfo.processInfo.environment
        if let url = env["NK_BASE_URL"], !url.isEmpty { baseURL = url; defaults.set(url, forKey: "baseURL") }
        if let tok = env["NK_TOKEN"], !tok.isEmpty { deviceToken = tok; Keychain.set("deviceToken", tok) }
        #endif
    }

    /// Read a credential from the shared Keychain access group. If absent, migrate it out
    /// of the interim App-Group `UserDefaults` location (an earlier build stored creds
    /// there) into the Keychain and **clear the plaintext copy**, so the secret ends up in
    /// exactly one, properly-protected place.
    private static func credential(_ defaults: UserDefaults, _ key: String) -> String {
        let kc = Keychain.get(key)
        if !kc.isEmpty { return kc }
        if let stray = defaults.string(forKey: key), !stray.isEmpty {
            Keychain.set(key, stray)
            defaults.removeObject(forKey: key)
            return stray
        }
        return ""
    }

    var isConfigured: Bool { !baseURL.isEmpty && !deviceToken.isEmpty }
}
