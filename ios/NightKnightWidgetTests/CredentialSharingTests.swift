import XCTest

// Shared (Settings) is compiled into this target, so types are visible without import.

/// Guards the fix for "the widget renders only --": credentials the app writes must be
/// visible to the widget. The widget reads them from the shared App Group, so a value
/// set on `Settings` must round-trip through that App Group suite (the exact store the
/// widget process opens), NOT a per-process Keychain the widget can't read.
final class CredentialSharingTests: XCTestCase {

    /// What the widget process sees: a `UserDefaults` opened on the same App Group suite.
    private var appGroupAsWidgetSeesIt: UserDefaults {
        UserDefaults(suiteName: Settings.appGroup) ?? .standard
    }

    func testDeviceTokenReachesAppGroup() {
        let token = "tok-\(UUID().uuidString)"
        Settings.shared.deviceToken = token
        XCTAssertEqual(appGroupAsWidgetSeesIt.string(forKey: "deviceToken"), token,
                       "device token must be in the App Group so the widget can authenticate")
    }

    func testCloudflareAccessCredsReachAppGroup() {
        let id = "cf-id-\(UUID().uuidString)"
        let secret = "cf-secret-\(UUID().uuidString)"
        Settings.shared.cfAccessClientId = id
        Settings.shared.cfAccessClientSecret = secret
        XCTAssertEqual(appGroupAsWidgetSeesIt.string(forKey: "cfId"), id,
                       "CF Access client id must reach the widget to pass the Access gate")
        XCTAssertEqual(appGroupAsWidgetSeesIt.string(forKey: "cfSecret"), secret,
                       "CF Access client secret must reach the widget to pass the Access gate")
    }

    /// A configured Settings (URL + token) must report `isConfigured`, which is what
    /// gates the widget's fetch — if false, the widget never calls the API and shows "--".
    func testConfiguredWhenTokenPresent() {
        Settings.shared.baseURL = "https://notebook.cooney.be"
        Settings.shared.deviceToken = "tok-\(UUID().uuidString)"
        XCTAssertTrue(Settings.shared.isConfigured)
    }
}

/// Guards the credential *lifecycle* — the "credentials stay cached even after you delete
/// them" bug and its fix. Deletion must be authoritative: a cleared credential must not be
/// resurrected on the next launch, must leave the app unconfigured (so the widget shows
/// "--"), and must take any cached reading with it.
final class CredentialLifecycleTests: XCTestCase {

    private var appGroup: UserDefaults { UserDefaults(suiteName: Settings.appGroup) ?? .standard }

    /// A fresh, throwaway defaults suite so migration tests don't touch the real App Group.
    private func scratchDefaults() -> (UserDefaults, String) {
        let name = "test.creds.\(UUID().uuidString)"
        return (UserDefaults(suiteName: name) ?? .standard, name)
    }

    override func tearDown() {
        // Leave the shared singleton/App Group in a clean, unconfigured state for other tests.
        Settings.shared.clearCredentials()
        super.tearDown()
    }

    // MARK: - clearCredentials

    func testClearCredentialsEmptiesEverythingAndDropsConfigured() {
        let s = Settings.shared
        s.baseURL = "https://nk.cooney.be"
        s.deviceToken = "tok"; s.cfAccessClientId = "cf-id"; s.cfAccessClientSecret = "cf-secret"
        XCTAssertTrue(s.isConfigured)

        s.clearCredentials()

        XCTAssertEqual(s.deviceToken, "")
        XCTAssertEqual(s.cfAccessClientId, "")
        XCTAssertEqual(s.cfAccessClientSecret, "")
        XCTAssertFalse(s.isConfigured, "clearing the token must make the app unconfigured")
        // And, crucially, the cleared values must be what the widget reads from the App Group.
        XCTAssertEqual(appGroup.string(forKey: "deviceToken"), "")
        XCTAssertEqual(appGroup.string(forKey: "cfId"), "")
        XCTAssertEqual(appGroup.string(forKey: "cfSecret"), "")
    }

    func testClearCredentialsPurgesCachedReading() {
        ReadingCache.save(CurrentReading(date: .now, value: GlucoseValue(mgdl: 123), trend: .flat, trendLabel: ""))
        XCTAssertNotNil(ReadingCache.load(), "precondition: a reading is cached")

        Settings.shared.clearCredentials()

        XCTAssertNil(ReadingCache.load(), "disconnect must drop the cached reading so no stale glucose lingers")
    }

    // MARK: - reloadFromStore (widget/watch freshness)

    /// The widget runs in a reused extension process; it must see credentials the app wrote
    /// after the process started. `reloadFromStore` is what re-reads them.
    func testReloadFromStorePicksUpExternallyWrittenCredentials() {
        let token = "ext-\(UUID().uuidString)"
        appGroup.set("https://nk.cooney.be", forKey: "baseURL")
        appGroup.set(token, forKey: "deviceToken")

        Settings.shared.reloadFromStore()

        XCTAssertEqual(Settings.shared.deviceToken, token,
                       "the widget must see a token the app wrote after the extension process started")
        XCTAssertTrue(Settings.shared.isConfigured)
    }

    /// The flip side: once the app clears the token, a reload must reflect the deletion so the
    /// widget stops authenticating with it — not keep a value cached in the singleton.
    func testReloadFromStoreReflectsAClearedToken() {
        Settings.shared.deviceToken = "tok"
        appGroup.set("", forKey: "deviceToken")   // as if the app cleared it in another process

        Settings.shared.reloadFromStore()

        XCTAssertEqual(Settings.shared.deviceToken, "")
        XCTAssertFalse(Settings.shared.isConfigured)
    }

    /// Regression guard for the credential-loss blocker: a read (e.g. a widget refresh on a
    /// Keychain-only upgrade, before the app's migration) must NOT write empty credential keys
    /// back. If it did, the migration would see a present "" instead of `nil`, skip importing
    /// the Keychain credentials, and then purge them — silent, permanent loss.
    func testReloadFromStoreDoesNotMaterializeAbsentCredentialKeys() {
        for key in LegacyCredentialMigration.keys { appGroup.removeObject(forKey: key) }

        Settings.shared.reloadFromStore()

        for key in LegacyCredentialMigration.keys {
            XCTAssertNil(appGroup.object(forKey: key),
                         "reloadFromStore must not persist an empty \(key); that defeats the launch migration")
        }
    }

    /// The widget/watch build a fresh snapshot per fetch. It must reflect the current store and,
    /// like a read, must not write anything back (so an extension can never materialize empties).
    func testCurrentIsAFreshNonMutatingSnapshot() {
        for key in LegacyCredentialMigration.keys { appGroup.removeObject(forKey: key) }
        appGroup.set("https://nk.cooney.be", forKey: "baseURL")
        appGroup.set("snap-tok", forKey: "deviceToken")

        let snap = Settings.current()

        XCTAssertEqual(snap.deviceToken, "snap-tok")
        XCTAssertTrue(snap.isConfigured)
        // Building the snapshot must not have stamped empty values for the absent CF keys.
        XCTAssertNil(appGroup.object(forKey: "cfId"))
        XCTAssertNil(appGroup.object(forKey: "cfSecret"))
    }

    // MARK: - LegacyCredentialMigration (no resurrection)

    func testMissingCredentialIsImportedFromKeychainOnce() {
        let (d, name) = scratchDefaults()
        defer { d.removePersistentDomain(forName: name) }

        LegacyCredentialMigration.migrate(into: d,
                                          legacyGet: { $0 == "deviceToken" ? "OLD-TOKEN" : "" },
                                          purgeLegacy: {})

        XCTAssertEqual(d.string(forKey: "deviceToken"), "OLD-TOKEN",
                       "an un-cleared legacy credential should be preserved for upgraders")
    }

    func testClearedCredentialIsNeverResurrected() {
        let (d, name) = scratchDefaults()
        defer { d.removePersistentDomain(forName: name) }
        d.set("", forKey: "deviceToken")   // the user explicitly cleared it

        LegacyCredentialMigration.migrate(into: d,
                                          legacyGet: { _ in "OLD-TOKEN" },   // still in the Keychain
                                          purgeLegacy: {})

        XCTAssertEqual(d.string(forKey: "deviceToken"), "",
                       "a credential the user cleared must NOT be resurrected from the Keychain")
    }

    func testMigrationRunsExactlyOnceAndPurgesTheKeychain() {
        let (d, name) = scratchDefaults()
        defer { d.removePersistentDomain(forName: name) }
        var purges = 0

        LegacyCredentialMigration.migrate(into: d, legacyGet: { _ in "" }, purgeLegacy: { purges += 1 })
        // A second pass (e.g. next launch) must be a no-op — no re-import, no second purge.
        LegacyCredentialMigration.migrate(into: d, legacyGet: { _ in "NEW-TOKEN" }, purgeLegacy: { purges += 1 })

        XCTAssertEqual(purges, 1, "migration (and the Keychain purge) must happen exactly once")
        XCTAssertNil(d.string(forKey: "deviceToken"), "a later launch must not re-import from the Keychain")
    }
}

/// Guards the encoding of the APNs device token. `didRegisterForRemoteNotifications
/// WithDeviceToken` hands over raw `Data`; the server and APNs expect it as a lowercase,
/// zero-padded, separator-free hex string. A wrong encoding here is invisible until every
/// silent push fails with `BadDeviceToken`, so it's worth pinning down.
final class PushTokenEncodingTests: XCTestCase {
    func testHexEncodingIsLowercaseAndZeroPadded() {
        let data = Data([0x00, 0x0f, 0xab, 0xff, 0x10])
        XCTAssertEqual(data.apnsHexToken, "000fabff10",
                       "two lowercase hex digits per byte, including a leading zero")
    }

    func testEmptyTokenIsEmptyString() {
        XCTAssertEqual(Data().apnsHexToken, "")
    }

    func testRealisticTokenLengthRoundTrips() {
        // A 32-byte APNs token encodes to exactly 64 hex characters, all lowercase hex.
        let bytes = (0..<32).map { UInt8($0) }
        let hex = Data(bytes).apnsHexToken
        XCTAssertEqual(hex.count, 64)
        XCTAssertTrue(hex.allSatisfy { "0123456789abcdef".contains($0) })
        XCTAssertTrue(hex.hasPrefix("000102030405"), "byte order is preserved")
    }
}
