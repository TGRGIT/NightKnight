import XCTest

// Shared (DexcomShareClient, StandaloneSource, Models) is compiled directly into this
// target, so its types are visible without an import. These tests mirror the Rust
// reference tests in `service/crates/nightknight-connectors/src/dexcom.rs` and assert
// against the SAME fixture bytes under `ios/Tests/Fixtures` (bundle resources), so the
// two parsers cannot drift silently.

final class DexcomShareClientTests: XCTestCase {

    private func fixture(_ name: String) throws -> Data {
        try Data(contentsOf: XCTUnwrap(
            Bundle(for: Self.self).url(forResource: name, withExtension: "json"),
            "missing fixture \(name).json"))
    }

    /// Region selection picks the right base URL and application id.
    /// Rust `region_endpoints`.
    func testRegionEndpoints() {
        XCTAssertTrue(DexcomShareClient.Region.us.baseURL.contains("share2.dexcom.com"))
        XCTAssertTrue(DexcomShareClient.Region.ous.baseURL.contains("shareous1.dexcom.com"))
        XCTAssertEqual(DexcomShareClient.Region.us.applicationId, DexcomShareClient.appIdUS)
        XCTAssertEqual(DexcomShareClient.Region.jp.applicationId, DexcomShareClient.appIdJP)
        XCTAssertEqual(DexcomShareClient.Region.parse("EU"), .ous)
    }

    /// Request bodies carry exactly the fields the Dexcom Share API expects.
    /// Rust `request_bodies`.
    func testRequestBodies() {
        let a = DexcomShareClient.authenticateBody(username: "user", password: "pass",
                                                   applicationId: DexcomShareClient.appIdUS)
        XCTAssertEqual(a["accountName"], "user")
        XCTAssertEqual(a["applicationId"], DexcomShareClient.appIdUS)
        let l = DexcomShareClient.loginBody(accountId: "acct-id", password: "pass",
                                            applicationId: DexcomShareClient.appIdUS)
        XCTAssertEqual(l["accountId"], "acct-id")
        let url = DexcomShareClient.readURL(base: DexcomShareClient.Region.us.baseURL,
                                            sessionId: "sess", minutes: 1440, maxCount: 288)
        XCTAssertTrue(url.contains("sessionId=sess"))
        XCTAssertTrue(url.contains("minutes=1440"))
        XCTAssertTrue(url.contains("maxCount=288"))
    }

    /// The auth/login endpoints return a quoted UUID string; we unwrap it.
    /// Rust `parses_quoted_id`.
    func testParsesQuotedId() throws {
        XCTAssertEqual(try DexcomShareClient.parseQuotedId(Data("\"abc-123\"".utf8)), "abc-123")
        XCTAssertThrowsError(try DexcomShareClient.parseQuotedId(Data("{\"not\":\"a string\"}".utf8)))
    }

    /// The Dexcom `WT` timestamp yields epoch ms regardless of the timezone suffix.
    /// The case table is the shared fixture, asserted identically by the Rust tests.
    /// Rust `parses_wt_timestamp`.
    func testParsesWTTimestamp() throws {
        let table = try XCTUnwrap(
            JSONSerialization.jsonObject(with: fixture("dexcom-wt-timestamps")) as? [[String: Any]])
        XCTAssertFalse(table.isEmpty)
        for row in table {
            let wt = try XCTUnwrap(row["wt"] as? String)
            let expected = (row["ms"] as? NSNumber)?.int64Value // JSON null → nil
            XCTAssertEqual(DexcomShareClient.parseWTMs(wt), expected, "parseWTMs(\(wt))")
        }
    }

    /// The Dexcom Share `Trend` field maps to a TrendDirection whether it arrives as
    /// a string (newer transmitters) or a legacy integer code (older ones).
    /// Rust `maps_share_trend_string_and_integer`.
    func testMapsShareTrendStringAndInteger() {
        XCTAssertEqual(DexcomShareClient.trendFromShare("Flat"), .flat)
        XCTAssertEqual(DexcomShareClient.trendFromShare("FortyFiveDown"), .fortyFiveDown)
        // Legacy integer codes (pydexcom): 4 = Flat, 5 = FortyFiveDown, 1 = DoubleUp.
        XCTAssertEqual(DexcomShareClient.trendFromShare(4), .flat)
        XCTAssertEqual(DexcomShareClient.trendFromShare(5), .fortyFiveDown)
        XCTAssertEqual(DexcomShareClient.trendFromShare(1), .doubleUp)
        XCTAssertNil(DexcomShareClient.trendFromShare(0)) // 0 = None → no arrow
    }

    /// `JSONSerialization` backs a JSON boolean with an `NSNumber`, and a bare `as?
    /// Int` silently bridges `true`/`false` to `1`/`0` — unlike Rust's
    /// `serde_json::Value::as_i64()`, which returns `None` for a bool. A malformed
    /// `Trend: true` must not fabricate a `.doubleUp` arrow.
    func testBooleanTrendIsRejectedNotCoercedToDoubleUp() throws {
        let json = try JSONSerialization.jsonObject(with: Data(#"{"Trend": true}"#.utf8)) as! [String: Any]
        XCTAssertNil(DexcomShareClient.trendFromShare(json["Trend"] as Any))
    }

    /// Same coercion bug on the `Value` (mgdl) field — a boolean must fail the parse
    /// like Rust does, not silently succeed as `mgdl: 1`.
    func testBooleanValueIsRejectedNotCoercedToOne() throws {
        let body = Data(#"[{"WT":"Date(1699999999000)","Value":true,"Trend":"Flat"}]"#.utf8)
        XCTAssertThrowsError(try DexcomShareClient.parseGlucose(body))
    }

    /// A representative readings payload (shared fixture) parses into samples with
    /// mg/dL, time, trend. Rust `parses_glucose_payload` (minus `to_entry_json`,
    /// which has no Swift counterpart on CgmSample).
    func testParsesGlucosePayload() throws {
        let samples = try DexcomShareClient.parseGlucose(fixture("dexcom-glucose"))
        XCTAssertEqual(samples.count, 2)
        XCTAssertEqual(samples[0].mgdl, 120)
        XCTAssertEqual(samples[0].dateMs, 1_699_999_999_000)
        XCTAssertEqual(samples[0].direction, .flat)
        XCTAssertEqual(samples[1].direction, .fortyFiveUp)
        XCTAssertEqual(samples[0].device, "dexcom-share")
    }
}
