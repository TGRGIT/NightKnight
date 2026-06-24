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
