import Foundation
import Observation

/// User settings, shared between the app, widget, and watch via an App Group. ALL values —
/// including the credentials (device token + CF Access id/secret) — live in the App Group
/// `UserDefaults`, because that's the only store the extensions read reliably on-device
/// (keychain access groups need provisioning the free/misconfigured signing path lacks). The
/// `Keychain` type is now only a one-time migration reader (see `LegacyCredentialMigration`).
@Observable
final class Settings {
    static let appGroup = "group.be.cooney.nightknight"
    static let shared = Settings()

    private let defaults = UserDefaults(suiteName: Settings.appGroup) ?? .standard

    // True only while `reloadFromStore()` is copying persisted values *in*, so the `didSet`
    // blocks don't write them straight back out. Without this, reading an absent key (which
    // yields "") would persist an explicit "" — and that empty value defeats the launch
    // migration's "never held" gate, silently destroying an upgrader's Keychain credentials.
    @ObservationIgnored private var isReloading = false
    private func persist(_ value: Any, _ key: String) {
        guard !isReloading else { return }
        defaults.set(value, forKey: key)
    }

    /// Base URL of your NightKnight deployment, e.g. `https://nightknight.cooney.be`.
    var baseURL: String { didSet { persist(baseURL, "baseURL") } }
    var preferredUnit: GlucoseUnit { didSet { persist(preferredUnit.rawValue, "unit") } }
    /// Remembered trailing-summary period, in days (1/7/14/30/90).
    var trailingDays: Int { didSet { persist(trailingDays, "trailingDays") } }

    // Alarms (all disableable — `alarmsEnabled` is the master switch).
    var alarmsEnabled: Bool { didSet { persist(alarmsEnabled, "alarmsEnabled") } }
    var lowThresholdMgdl: Double { didSet { persist(lowThresholdMgdl, "low") } }
    var highThresholdMgdl: Double { didSet { persist(highThresholdMgdl, "high") } }
    var fastDropAlarm: Bool { didSet { persist(fastDropAlarm, "fastDrop") } }

    var writeToHealthKit: Bool { didSet { persist(writeToHealthKit, "hkWrite") } }
    var readFromHealthKit: Bool { didSet { persist(readFromHealthKit, "hkRead") } }

    // Credentials. Stored in the shared App Group (NOT the per-target Keychain) so the
    // widget + watch complication can read them — keychain access groups don't reliably
    // share on-device without provisioning, which left the widget unauthenticated ("--").
    var deviceToken: String { didSet { persist(deviceToken, "deviceToken") } }
    var cfAccessClientId: String { didSet { persist(cfAccessClientId, "cfId") } }
    var cfAccessClientSecret: String { didSet { persist(cfAccessClientSecret, "cfSecret") } }

    /// This device's last-seen APNs token (hex). Kept so "Disconnect" can unregister it from
    /// the server while the connection is still authenticated. Not a secret; not a credential.
    var apnsToken: String { didSet { persist(apnsToken, "apnsToken") } }

    private init() {
        // Seed with neutral defaults so every stored property is initialised, then read the
        // real persisted values in one place (`reloadFromStore`; the widget/watch build a fresh
        // instance via `current()`, which runs the same path).
        baseURL = ""; preferredUnit = .mgdl; trailingDays = 7
        alarmsEnabled = false; lowThresholdMgdl = 70; highThresholdMgdl = 180; fastDropAlarm = true
        writeToHealthKit = false; readFromHealthKit = false
        deviceToken = ""; cfAccessClientId = ""; cfAccessClientSecret = ""; apnsToken = ""
        reloadFromStore()

        #if DEBUG
        // Test hook: let a simulator launch inject a server + token (SIMCTL_CHILD_*).
        let env = ProcessInfo.processInfo.environment
        if let url = env["NK_BASE_URL"], !url.isEmpty { baseURL = url }
        if let tok = env["NK_TOKEN"], !tok.isEmpty { deviceToken = tok }
        #endif
    }

    var isConfigured: Bool { !baseURL.isEmpty && !deviceToken.isEmpty }

    /// A fresh instance reflecting the values currently persisted in the App Group.
    ///
    /// The widget and watch complication run in extension processes that iOS reuses across
    /// timeline reloads, so the `Settings.shared` singleton can hold values captured at an
    /// earlier reload — including a token the user has since edited or *cleared* in the app.
    /// They build one of these per fetch instead, so they always authenticate with the current
    /// credentials (and stop fetching once the token is removed) WITHOUT mutating the shared
    /// singleton from a background thread (which would race a concurrent timeline build).
    static func current() -> Settings { Settings() }

    /// Re-read every value from the shared App Group store into this instance. Used by `init`
    /// and by the app after the one-time `LegacyCredentialMigration`.
    ///
    /// This is a pure read: `isReloading` suppresses the `didSet` write-backs, so reading an
    /// absent key (→ "") never persists an explicit "". That matters because the migration
    /// treats a *present* empty value as "the user cleared it"; materialising "" here would
    /// make it skip importing — and then purge — an upgrader's real Keychain credentials.
    ///
    /// Credentials are read from the App Group *only* — never the Keychain. An empty value is
    /// authoritative ("the user cleared it"); the deliberate absence of a Keychain fall-back is
    /// what stops a deleted credential from being resurrected on the next launch.
    func reloadFromStore() {
        isReloading = true
        defer { isReloading = false }
        baseURL = defaults.string(forKey: "baseURL") ?? "https://nightknight.cooney.be"
        preferredUnit = GlucoseUnit(rawValue: defaults.string(forKey: "unit") ?? "") ?? .mgdl
        trailingDays = defaults.object(forKey: "trailingDays") as? Int ?? 7
        alarmsEnabled = defaults.object(forKey: "alarmsEnabled") as? Bool ?? false
        lowThresholdMgdl = defaults.object(forKey: "low") as? Double ?? 70
        highThresholdMgdl = defaults.object(forKey: "high") as? Double ?? 180
        fastDropAlarm = defaults.object(forKey: "fastDrop") as? Bool ?? true
        writeToHealthKit = defaults.object(forKey: "hkWrite") as? Bool ?? false
        readFromHealthKit = defaults.object(forKey: "hkRead") as? Bool ?? false
        deviceToken = defaults.string(forKey: "deviceToken") ?? ""
        cfAccessClientId = defaults.string(forKey: "cfId") ?? ""
        cfAccessClientSecret = defaults.string(forKey: "cfSecret") ?? ""
        apnsToken = defaults.string(forKey: "apnsToken") ?? ""
    }

    /// Remove the stored credentials ("disconnect" / sign out): empty them in the App Group
    /// so the widget and watch see an unconfigured app and fall back to "--", purge any legacy
    /// Keychain copies so they can't be re-imported, and drop the cached reading so no stale
    /// glucose lingers on a widget after the account is removed.
    func clearCredentials() {
        deviceToken = ""
        cfAccessClientId = ""
        cfAccessClientSecret = ""
        Keychain.delete(LegacyCredentialMigration.keys)
        ReadingCache.clear()
    }
}

/// One-time move of credentials an older build stored in the app's Keychain into the shared
/// App Group (the channel the widget/watch can actually read), then purge the Keychain copies.
///
/// Run **app-only**, at launch, before anything reads `Settings`. It is what lets the app keep
/// working for upgraders while making deletion stick: a credential the user has already cleared
/// (so the App Group holds an explicit empty string) is left untouched and is *not* re-imported
/// from the Keychain. The done-flag and the Keychain purge together guarantee it runs once.
enum LegacyCredentialMigration {
    static let doneKey = "creds.migratedFromKeychain.v1"
    static let keys = ["deviceToken", "cfId", "cfSecret"]

    /// Production entry point: migrate from the real Keychain into the shared App Group.
    static func run() {
        let defaults = UserDefaults(suiteName: Settings.appGroup) ?? .standard
        migrate(into: defaults, legacyGet: Keychain.get, purgeLegacy: { Keychain.delete(keys) })
    }

    /// Testable core. `legacyGet` reads a value an older build may have stored; `purgeLegacy`
    /// removes those legacy copies. Only migrates a key the App Group has *never* held — an
    /// existing value (even an empty one the user cleared) is authoritative and left as-is.
    static func migrate(into defaults: UserDefaults,
                        legacyGet: (String) -> String,
                        purgeLegacy: () -> Void) {
        guard !defaults.bool(forKey: doneKey) else { return }
        for key in keys where defaults.object(forKey: key) == nil {
            let legacy = legacyGet(key)
            if !legacy.isEmpty { defaults.set(legacy, forKey: key) }
        }
        purgeLegacy()
        defaults.set(true, forKey: doneKey)
    }
}
