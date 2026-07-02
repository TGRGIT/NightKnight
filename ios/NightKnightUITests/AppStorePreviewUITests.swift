import XCTest

/// Drives the App Store *App Preview* walk: launches the app in demo/autoplay mode
/// (synthetic data, no network) and holds it frontmost while the built-in autoplay
/// plan tours Dashboard → Analysis → Settings → Dashboard (~30 s). The video itself
/// is captured OUTSIDE the test by `marketing/appstore/record.sh`, which runs this
/// test with `xcodebuild test -only-testing:` and records the simulator while it
/// walks — the assertions here just guarantee the tour actually happened, so a
/// broken walk fails the recording instead of producing a truncated/blank capture.
///
/// `record.sh` cannot know in advance how long app launch takes (it varies wildly
/// with host load — RustAnalytics.assertABI(), the legacy-credential migration, and
/// WatchConnectivity startup all run before the first frame) or exactly when the
/// tour finishes, so this test prints two plain-text markers to stdout at the two
/// moments record.sh actually cares about: `NKPREVIEW_READY` right when there is a
/// clean opening frame to start recording from, and `NKPREVIEW_DONE` right when the
/// closing frame is ready to stop on. record.sh tails its own log for these lines
/// instead of guessing with a fixed sleep, so the capture window always matches the
/// walk regardless of how slow or fast this particular run is.
final class AppStorePreviewUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    func testAppStorePreviewWalk() throws {
        let app = XCUIApplication()
        // Same demo profile record.sh used historically: mg/dL, 7-day period,
        // autoplay tab tour (RootTabView's DEBUG plan: 10 s → Analysis, 9 s →
        // Settings, 6 s → Dashboard).
        app.launchEnvironment["NK_DEMO"] = "1"
        app.launchEnvironment["NK_AUTOPLAY"] = "1"
        app.launchEnvironment["NK_UNIT"] = "mgdl"
        app.launchEnvironment["NK_PERIOD"] = "7"
        app.launchArguments += ["-NKDemo"]
        app.launch()

        let tabs = app.tabBars
        let dashboard = tabs.buttons["Dashboard"]
        let analysis = tabs.buttons["Analysis"]
        let settings = tabs.buttons["Settings"]
        // Generous: a loaded CI/dev machine can take well over 10 s to reach the
        // first frame (launch is not what this test times — the TOUR is).
        XCTAssertTrue(dashboard.waitForExistence(timeout: 40), "demo dashboard should render")
        print("NKPREVIEW_READY")

        // Follow the autoplay plan with generous timeouts; each wait also keeps the
        // app frontmost for the recording.
        XCTAssertTrue(waitUntilSelected(analysis, timeout: 16), "autoplay should reach Analysis")
        XCTAssertTrue(waitUntilSelected(settings, timeout: 14), "autoplay should reach Settings")
        XCTAssertTrue(waitUntilSelected(dashboard, timeout: 11), "autoplay should return to Dashboard")
        // Hold the closing dashboard so the recording has a clean tail to trim.
        Thread.sleep(forTimeInterval: 6)
        XCTAssertTrue(dashboard.isSelected)
        print("NKPREVIEW_DONE")
    }

    private func waitUntilSelected(_ element: XCUIElement, timeout: TimeInterval) -> Bool {
        let selected = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "isSelected == true"), object: element)
        return XCTWaiter().wait(for: [selected], timeout: timeout) == .completed
    }
}
