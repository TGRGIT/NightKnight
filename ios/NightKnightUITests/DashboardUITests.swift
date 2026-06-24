import XCTest

/// UI tests that drive the real app against the local dev server (run it first on
/// 127.0.0.1:8799 in dev auth mode with seeded data). The test mints its own device
/// token from the server, so it is self-contained.
final class DashboardUITests: XCTestCase {
    private let base = "http://localhost:8799"
    private var token = ""

    override func setUpWithError() throws {
        continueAfterFailure = false
        token = try mintToken()
    }

    /// POST /api/v4/tokens (dev-edge auth, no token needed) → a raw device token.
    private func mintToken() throws -> String {
        var req = URLRequest(url: URL(string: base + "/api/v4/tokens")!)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "content-type")
        req.httpBody = Data(#"{"name":"uitest","scopes":["api:entries:read","api:treatments:read"]}"#.utf8)
        let sem = DispatchSemaphore(value: 0)
        var token = ""
        var failure: Error?
        URLSession.shared.dataTask(with: req) { data, _, err in
            if let err { failure = err }
            else if let data,
                    let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                    let t = obj["token"] as? String { token = t }
            sem.signal()
        }.resume()
        _ = sem.wait(timeout: .now() + 15)
        if let failure { throw failure }
        XCTAssertFalse(token.isEmpty, "should mint a device token (is the dev server running on :8799?)")
        return token
    }

    private func launchApp() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchEnvironment["NK_BASE_URL"] = base
        app.launchEnvironment["NK_TOKEN"] = token
        app.launch()
        return app
    }

    private func periodButton(_ app: XCUIApplication, _ label: String) -> XCUIElement {
        let inSegmented = app.segmentedControls.buttons[label]
        return inSegmented.exists ? inSegmented : app.buttons[label]
    }

    /// The dashboard renders the trailing summary, and each period segment is
    /// selectable (selection moves to the tapped one).
    func testDashboardAndPeriodSelector() {
        let app = launchApp()
        XCTAssertTrue(app.staticTexts["TRAILING SUMMARY"].waitForExistence(timeout: 15),
                      "trailing summary header visible")
        XCTAssertTrue(app.staticTexts["EST. A1C"].exists, "A1c metric tile present")
        XCTAssertTrue(app.staticTexts["AVG"].exists, "average metric tile present")

        for label in ["90d", "24h", "30d", "7d"] {
            let seg = periodButton(app, label)
            XCTAssertTrue(seg.waitForExistence(timeout: 5), "period \(label) exists")
            seg.tap()
            XCTAssertTrue(seg.isSelected, "tapping \(label) selects it")
        }
        // Summary still present after switching periods.
        XCTAssertTrue(app.staticTexts["IN RANGE"].exists)
    }

    /// Settings opens, shows the connection fields, and the in-app "Test connection"
    /// reaches the server successfully.
    func testSettingsConnectionTest() {
        let app = launchApp()
        app.buttons["settingsButton"].tap()
        XCTAssertTrue(app.navigationBars["Settings"].waitForExistence(timeout: 5), "Settings opened")
        XCTAssertTrue(app.textFields["Server URL"].exists, "server URL field present")

        app.buttons["Test connection"].tap()
        let connected = app.staticTexts
            .containing(NSPredicate(format: "label CONTAINS 'Connected'")).firstMatch
        XCTAssertTrue(connected.waitForExistence(timeout: 15), "connection test reports success")
    }
}
