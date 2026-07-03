import Foundation
import Observation
import CryptoKit

/// Where the app's glucose data comes from — one flat choice, mapping 1:1 to the four
/// onboarding cards. `.nightknight` is the classic server mode (the server computes the
/// analytics); the other three fetch raw readings directly from the vendor in Swift and
/// compute the full statistics on-device via the Rust FFI.
enum DataSource: String, CaseIterable, Sendable {
    case nightknight, dexcom, libre, nightscout

    /// The one real behavioural split: everything except the NightKnight server
    /// accumulates raw readings locally and computes analytics on-device.
    var usesLocalAnalytics: Bool { self != .nightknight }

    var label: String {
        switch self {
        case .nightknight: return "NightKnight"
        case .dexcom: return "Dexcom Share"
        case .libre: return "Libreview"
        case .nightscout: return "Nightscout"
        }
    }
}

/// User settings, shared between the app, widget, and watch via an App Group. ALL values —
/// including the credentials (device token + CF Access id/secret) — live in the App Group
/// `UserDefaults`, because that's the only store the extensions read reliably on-device
/// (keychain access groups need provisioning the free/misconfigured signing path lacks). The
/// `Keychain` type is now only a one-time migration reader (see `LegacyCredentialMigration`).
/// The per-source vendor credentials below share that channel deliberately — the same
/// trade-off as the existing device token: sandboxed but plaintext-at-rest and included in
/// device backups.
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

    /// Whether the user has accepted the first-launch safety notice (not a medical
    /// device; alarms are reliable only while the app is open). Gates the data-source
    /// chooser — a returning user who has already accepted skips straight to it.
    var hasAcceptedDisclaimer: Bool { didSet { persist(hasAcceptedDisclaimer, "disclaimerAccepted") } }

    // Credentials. Stored in the shared App Group (NOT the per-target Keychain) so the
    // widget + watch complication can read them — keychain access groups don't reliably
    // share on-device without provisioning, which left the widget unauthenticated ("--").
    var deviceToken: String { didSet { persist(deviceToken, "deviceToken") } }
    var cfAccessClientId: String { didSet { persist(cfAccessClientId, "cfId") } }
    var cfAccessClientSecret: String { didSet { persist(cfAccessClientSecret, "cfSecret") } }

    /// This device's last-seen APNs token (hex). Kept so "Disconnect" can unregister it from
    /// the server while the connection is still authenticated. Not a secret; not a credential.
    var apnsToken: String { didSet { persist(apnsToken, "apnsToken") } }

    /// The selected data source. `nil` = never chosen → the app shows the first-run
    /// chooser (`WelcomeView`). Persisted as the raw value; absence of the key is the
    /// "not yet chosen" state, so clearing must remove the key rather than write "".
    var dataSource: DataSource? {
        didSet {
            guard !isReloading else { return }
            if let raw = dataSource?.rawValue {
                defaults.set(raw, forKey: "dataSource")
            } else {
                defaults.removeObject(forKey: "dataSource")
            }
        }
    }

    // Dexcom Share (unofficial follower API): account credentials + region (us/ous/jp).
    var dexcomRegion: String { didSet { persist(dexcomRegion, "dexcomRegion") } }
    var dexcomUsername: String { didSet { persist(dexcomUsername, "dexcomUsername") } }
    var dexcomPassword: String { didSet { persist(dexcomPassword, "dexcomPassword") } }

    // LibreLinkUp follower credentials. `libreRegion` is auto-discovered from the login
    // redirect and cached so subsequent logins go straight to the right regional host.
    var libreEmail: String { didSet { persist(libreEmail, "libreEmail") } }
    var librePassword: String { didSet { persist(librePassword, "librePassword") } }
    var libreRegion: String { didSet { persist(libreRegion, "libreRegion") } }

    // Nightscout: the user's own instance origin + api-secret (SHA-1 hash or access
    // token, sent as-is). The URL must pass the SSRF guard before any request carries
    // the secret.
    var nightscoutURL: String { didSet { persist(nightscoutURL, "nightscoutURL") } }
    var nightscoutSecret: String { didSet { persist(nightscoutSecret, "nightscoutSecret") } }

    private init() {
        // Seed with neutral defaults so every stored property is initialised, then read the
        // real persisted values in one place (`reloadFromStore`; the widget/watch build a fresh
        // instance via `current()`, which runs the same path).
        baseURL = ""; preferredUnit = .mgdl; trailingDays = 7
        alarmsEnabled = false; lowThresholdMgdl = 70; highThresholdMgdl = 180; fastDropAlarm = true
        writeToHealthKit = false; readFromHealthKit = false
        hasAcceptedDisclaimer = false
        deviceToken = ""; cfAccessClientId = ""; cfAccessClientSecret = ""; apnsToken = ""
        dataSource = nil
        dexcomRegion = "us"; dexcomUsername = ""; dexcomPassword = ""
        libreEmail = ""; librePassword = ""; libreRegion = ""
        nightscoutURL = ""; nightscoutSecret = ""
        reloadFromStore()

        // First-launch migration: an upgrader who already has a working server connection
        // (legacy baseURL + device token) skips the chooser — their existing NightKnight
        // config becomes the selected source. A fresh install has no token → stays nil.
        if dataSource == nil && !baseURL.isEmpty && !deviceToken.isEmpty {
            dataSource = .nightknight
        }

        #if DEBUG
        // Test hook: let a simulator launch inject a source + credentials (SIMCTL_CHILD_*).
        let env = ProcessInfo.processInfo.environment
        if let url = env["NK_BASE_URL"], !url.isEmpty { baseURL = url }
        if let tok = env["NK_TOKEN"], !tok.isEmpty { deviceToken = tok }
        if let src = env["NK_DATA_SOURCE"], let ds = DataSource(rawValue: src) { dataSource = ds }
        if let v = env["NK_DEXCOM_REGION"], !v.isEmpty { dexcomRegion = v }
        if let v = env["NK_DEXCOM_USER"], !v.isEmpty { dexcomUsername = v }
        if let v = env["NK_DEXCOM_PASS"], !v.isEmpty { dexcomPassword = v }
        if let v = env["NK_LIBRE_EMAIL"], !v.isEmpty { libreEmail = v }
        if let v = env["NK_LIBRE_PASS"], !v.isEmpty { librePassword = v }
        if let v = env["NK_NS_URL"], !v.isEmpty { nightscoutURL = v }
        if let v = env["NK_NS_SECRET"], !v.isEmpty { nightscoutSecret = v }
        #endif
    }

    /// Whether the ACTIVE source has everything it needs to fetch. `nil` (chooser not yet
    /// answered) falls back to the NightKnight-server check so widgets keep working across
    /// an upgrade installed before the app's first post-update launch runs the migration.
    var isConfigured: Bool {
        switch dataSource {
        case .none, .some(.nightknight):
            return !baseURL.isEmpty && !deviceToken.isEmpty
        case .some(.dexcom):
            return !dexcomUsername.isEmpty && !dexcomPassword.isEmpty
        case .some(.libre):
            return !libreEmail.isEmpty && !librePassword.isEmpty
        case .some(.nightscout):
            return !nightscoutSecret.isEmpty && NightscoutClient.isSafeBase(nightscoutURL)
        }
    }

    /// Convenience over the optional: an unchosen source behaves like server mode.
    var usesLocalAnalytics: Bool { (dataSource ?? .nightknight).usesLocalAnalytics }

    /// Non-reversible tag for a follower-account identifier (an email / username). The
    /// owner-guard key and the Libre session cache only need to compare accounts for
    /// equality, so tagging the raw PII keeps it out of the (plaintext-at-rest) App Group
    /// store and the local SQLite DB (CWE-312). Normalised first so the same account always
    /// tags equal, regardless of casing/whitespace.
    static func accountTag(_ raw: String) -> String {
        let norm = raw.trimmingCharacters(in: .whitespaces).lowercased()
        return SHA256.hash(data: Data(norm.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    /// Identifies the active source *by account identity* — the value the settings UI and
    /// the `LocalStore` owner-guard compare to decide "is this a switch that must wipe the
    /// local data?". Secrets are excluded on purpose: re-entering a password is not a
    /// switch; a different account or instance is. The follower-account identifier is
    /// tagged (`accountTag`) so this key — stamped into the on-device DB — never carries the
    /// raw email/username at rest.
    var sourceKey: String {
        switch dataSource ?? .nightknight {
        case .nightknight:
            return "nightknight:\(baseURL)"
        case .dexcom:
            return "dexcom:\(dexcomRegion.trimmingCharacters(in: .whitespaces).lowercased()):\(Self.accountTag(dexcomUsername))"
        case .libre:
            return "libre:\(Self.accountTag(libreEmail))"
        case .nightscout:
            return "nightscout:\(NightscoutClient.normalizeBase(nightscoutURL).lowercased())"
        }
    }

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
        hasAcceptedDisclaimer = defaults.object(forKey: "disclaimerAccepted") as? Bool ?? false
        deviceToken = defaults.string(forKey: "deviceToken") ?? ""
        cfAccessClientId = defaults.string(forKey: "cfId") ?? ""
        cfAccessClientSecret = defaults.string(forKey: "cfSecret") ?? ""
        apnsToken = defaults.string(forKey: "apnsToken") ?? ""
        dataSource = DataSource(rawValue: defaults.string(forKey: "dataSource") ?? "")
        dexcomRegion = defaults.string(forKey: "dexcomRegion") ?? "us"
        dexcomUsername = defaults.string(forKey: "dexcomUsername") ?? ""
        dexcomPassword = defaults.string(forKey: "dexcomPassword") ?? ""
        libreEmail = defaults.string(forKey: "libreEmail") ?? ""
        librePassword = defaults.string(forKey: "librePassword") ?? ""
        libreRegion = defaults.string(forKey: "libreRegion") ?? ""
        nightscoutURL = defaults.string(forKey: "nightscoutURL") ?? ""
        nightscoutSecret = defaults.string(forKey: "nightscoutSecret") ?? ""
    }

    /// Remove the stored credentials ("disconnect" / sign out) for EVERY source: empty them
    /// in the App Group so the widget and watch see an unconfigured app and fall back to
    /// "--", purge any legacy Keychain copies so they can't be re-imported, drop the cached
    /// vendor sessions, and drop the cached reading so no stale glucose lingers on a widget
    /// after the account is removed.
    func clearCredentials() {
        deviceToken = ""
        cfAccessClientId = ""
        cfAccessClientSecret = ""
        clearSourceCredentials(.dexcom)
        clearSourceCredentials(.libre)
        clearSourceCredentials(.nightscout)
        Keychain.delete(LegacyCredentialMigration.keys)
        ReadingCache.clear()
    }

    /// Remove one source's stored account fields and any cached session material (the
    /// Dexcom session id / Libre bearer token). Used when switching away from a source so
    /// nothing of the old account survives the wipe.
    func clearSourceCredentials(_ source: DataSource) {
        switch source {
        case .nightknight:
            deviceToken = ""
            cfAccessClientId = ""
            cfAccessClientSecret = ""
        case .dexcom:
            dexcomUsername = ""
            dexcomPassword = ""
            DexcomShareClient.clearCachedSession()
        case .libre:
            libreEmail = ""
            librePassword = ""
            libreRegion = ""
            LibreLinkUpClient.clearCachedSession()
        case .nightscout:
            nightscoutURL = ""
            nightscoutSecret = ""
        }
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
