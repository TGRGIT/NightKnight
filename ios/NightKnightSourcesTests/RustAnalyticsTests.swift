import XCTest
import NightKnightFFI

/// The Swift half of the cross-language golden contract: feed the SAME committed
/// 14-day reading fixture through the real linked xcframework and assert the output
/// equals the SAME golden bytes the Rust test (`nightknight-ffi/tests/golden.rs`)
/// asserts. If the report composition, float formatting, or FFI plumbing drifts on
/// either side, one of the two suites fails.
final class RustAnalyticsTests: XCTestCase {
    // Must mirror nightknight-ffi/tests/golden.rs.
    private static let goldenHours = 336
    private static let goldenTz = 60
    private static let goldenDays = 14
    private static let goldenBin = 15

    private func fixtureString(_ name: String) throws -> String {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: name, withExtension: "json"))
        return try String(contentsOf: url, encoding: .utf8)
    }

    /// A stale checked-in xcframework must fail here (and at app launch), not as
    /// silent DTO-decode blanks.
    func testABIVersionMatchesTheSwiftExpectation() {
        XCTAssertEqual(nk_abi_version(), RustAnalytics.expectedABIVersion)
    }

    func testAnalyticsOutputIsByteIdenticalToTheGolden() throws {
        let readings = try fixtureString("readings-14d")
        let data = try RustAnalytics().analyticsJSON(readingsJSON: readings,
                                                     hours: Self.goldenHours,
                                                     tzOffsetMin: Self.goldenTz)
        XCTAssertEqual(String(data: data, encoding: .utf8), try fixtureString("analytics-golden"))
    }

    func testAgpOutputIsByteIdenticalToTheGolden() throws {
        let readings = try fixtureString("readings-14d")
        let data = try RustAnalytics().agpJSON(readingsJSON: readings,
                                               days: Self.goldenDays,
                                               binMinutes: Self.goldenBin,
                                               tzOffsetMin: Self.goldenTz)
        XCTAssertEqual(String(data: data, encoding: .utf8), try fixtureString("agp-golden"))
    }

    /// The in-band `{"error":…}` convention becomes a thrown Swift error — never a
    /// crash, never silently-empty analytics.
    func testMalformedInputSurfacesTheFFIError() {
        XCTAssertThrowsError(try RustAnalytics().analyticsJSON(readingsJSON: "not json",
                                                               hours: 24, tzOffsetMin: 0)) { error in
            let message = (error as? LocalizedError)?.errorDescription ?? ""
            XCTAssertTrue(message.contains("bad readings JSON"), "got: \(message)")
        }
    }

    func testCSVImportRoundTrip() throws {
        let csv = """
        Index,Timestamp (YYYY-MM-DDThh:mm:ss),Event Type,Event Subtype,Patient Info,Device Info,Source Device ID,Glucose Value (mg/dL),Insulin Value (u),Carb Value (grams),Duration (hh:mm:ss),Glucose Rate of Change (mg/dL/min),Transmitter Time (Long Integer)
        1,2024-01-01T00:00:00,EGV,,,,G6,120,,,,,
        2,2024-01-01T00:05:00,EGV,,,,G6,125,,,,,
        """
        let data = try RustAnalytics().importGlucoseCSV(text: csv, tzOffsetMin: 0)
        let obj = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(obj["source"] as? String, "dexcom")
        XCTAssertEqual(obj["imported"] as? Int, 2)
        let entries = try XCTUnwrap(obj["entries"] as? [[String: Any]])
        XCTAssertEqual(entries.count, 2)
        XCTAssertEqual(entries[0]["mgdl"] as? Double, 120.0)
    }
}
