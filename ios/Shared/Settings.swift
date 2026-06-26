import Foundation
import Observation

/// User settings, shared between the app, the widget and the watch via an App Group.
/// Everything — including the read-scoped credentials the widget/watch need to make
/// authenticated, Cloudflare-Access-passing API calls — lives in the shared App Group
/// `UserDefaults`. (The credentials used to live in the per-process Keychain, which the
/// widget's separate process couldn't read, so the widget could never fetch data.)
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

    // Credentials. Stored in the shared App Group (NOT the Keychain) so the widget and
    // watch — separate processes — can read them. These are a read-scoped follower token
    // and a Cloudflare Access service token, kept in the app's sandboxed shared container.
    var deviceToken: String { didSet { defaults.set(deviceToken, forKey: "deviceToken") } }
    var cfAccessClientId: String { didSet { defaults.set(cfAccessClientId, forKey: "cfId") } }
    var cfAccessClientSecret: String { didSet { defaults.set(cfAccessClientSecret, forKey: "cfSecret") } }

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
        deviceToken = Settings.sharedSecret(defaults, "deviceToken")
        cfAccessClientId = Settings.sharedSecret(defaults, "cfId")
        cfAccessClientSecret = Settings.sharedSecret(defaults, "cfSecret")

        #if DEBUG
        // Test hook: let a simulator launch inject a server + token (SIMCTL_CHILD_*).
        // Persist to the App Group too, so the widget's separate process picks them up.
        let env = ProcessInfo.processInfo.environment
        if let url = env["NK_BASE_URL"], !url.isEmpty { baseURL = url; defaults.set(url, forKey: "baseURL") }
        if let tok = env["NK_TOKEN"], !tok.isEmpty { deviceToken = tok; defaults.set(tok, forKey: "deviceToken") }
        #endif
    }

    /// Read a credential from the shared App Group, migrating it out of the old per-process
    /// Keychain location (which the widget/watch couldn't reach) on first launch so existing
    /// installs don't have to re-enter it.
    private static func sharedSecret(_ defaults: UserDefaults, _ key: String) -> String {
        if let v = defaults.string(forKey: key), !v.isEmpty { return v }
        let legacy = Keychain.get(key)
        if !legacy.isEmpty { defaults.set(legacy, forKey: key) }
        return legacy
    }

    var isConfigured: Bool { !baseURL.isEmpty && !deviceToken.isEmpty }
}
