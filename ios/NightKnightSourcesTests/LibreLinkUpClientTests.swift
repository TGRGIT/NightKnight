import XCTest

// Shared (LibreLinkUpClient, StandaloneSource, Models) is compiled directly into this
// target, so its types are visible without an import. These tests mirror the Rust
// reference tests in `service/crates/nightknight-connectors/src/librelinkup.rs` and
// assert against the SAME fixture bytes under `ios/Tests/Fixtures` (bundle resources),
// so the two parsers cannot drift silently.

final class LibreLinkUpClientTests: XCTestCase {

    private func fixture(_ name: String) throws -> Data {
        try Data(contentsOf: XCTUnwrap(
            Bundle(for: Self.self).url(forResource: name, withExtension: "json"),
            "missing fixture \(name).json"))
    }

    /// Body previews collapse whitespace/control chars to one line and truncate, so a
    /// 403's diagnostic snippet can't flood the error message.
    /// Rust `snippet_is_one_line_and_truncated`.
    func testSnippetIsOneLineAndTruncated() {
        XCTAssertEqual(LibreLinkUpClient.snippet(Data("<html>\n  Access\tDenied  </html>".utf8)),
                       "<html> Access Denied </html>")
        XCTAssertLessThanOrEqual(
            LibreLinkUpClient.snippet(Data(repeating: UInt8(ascii: "x"), count: 500)).count, 181)
    }

    /// The `account-id` header value is the SHA-256 of the user id: deterministic,
    /// 64 lowercase hex chars. Rust `account_id_is_sha256_hex`.
    func testAccountIdIsSha256Hex() {
        let h = LibreLinkUpClient.accountIdHash("user-123")
        XCTAssertEqual(h.count, 64)
        XCTAssertTrue(h.allSatisfy { "0123456789abcdef".contains($0) }, h)
    }

    /// Rust `headers_include_auth_and_account_id`.
    func testHeadersIncludeAuthAndAccountId() {
        let h = LibreLinkUpClient.headers(token: "tok", accountId: "acct")
        XCTAssertTrue(h.contains { $0 == ("authorization", "Bearer tok") })
        XCTAssertTrue(h.contains { $0 == ("account-id", "acct") })
        XCTAssertTrue(h.contains { $0 == ("product", "llu.android") })
    }

    /// A successful login yields token + user id; a regional redirect is surfaced.
    /// Rust `parses_login_success_and_redirect`.
    func testParsesLoginSuccessAndRedirect() throws {
        XCTAssertEqual(try LibreLinkUpClient.parseLogin(fixture("libre-login-ok")),
                       .authenticated(token: "jwt-abc", userId: "u-1"))
        XCTAssertEqual(try LibreLinkUpClient.parseLogin(fixture("libre-login-redirect")),
                       .redirect(region: "eu"))
        XCTAssertEqual(LibreLinkUpClient.regionalBase("EU"), "https://api-eu.libreview.io")
    }

    /// A redirect carrying a hostile "region" (one that would reshape the API host)
    /// is rejected rather than interpolated into the URL.
    /// Rust `rejects_redirect_with_invalid_region`.
    func testRejectsRedirectWithInvalidRegion() throws {
        XCTAssertTrue(LibreLinkUpClient.isValidRegion("eu"))
        XCTAssertTrue(LibreLinkUpClient.isValidRegion("ap-west"))
        XCTAssertFalse(LibreLinkUpClient.isValidRegion("x/@evil.com"))
        XCTAssertFalse(LibreLinkUpClient.isValidRegion("a.b"))
        XCTAssertFalse(LibreLinkUpClient.isValidRegion(""))
        let redir = try fixture("libre-login-redirect-invalid")
        XCTAssertThrowsError(try LibreLinkUpClient.parseLogin(redir)) { error in
            guard case StandaloneError.auth = error else {
                return XCTFail("expected .auth, got \(error)")
            }
        }
    }

    /// A non-zero `status` (no `data` object) must surface the real reason — so the
    /// connector's status in the UI says *why*, not a generic "no data".
    /// Rust `surfaces_login_failures`.
    func testSurfacesLoginFailures() throws {
        let bad = try fixture("libre-login-bad")
        XCTAssertThrowsError(try LibreLinkUpClient.parseLogin(bad)) { error in
            guard case StandaloneError.auth(let m) = error else {
                return XCTFail("expected .auth, got \(error)")
            }
            XCTAssertTrue(m.contains("status 2"), m)
            XCTAssertTrue(m.contains("incorrect username/password"), m)
        }

        let locked = try fixture("libre-login-locked")
        XCTAssertThrowsError(try LibreLinkUpClient.parseLogin(locked)) { error in
            guard case StandaloneError.auth(let m) = error else {
                return XCTFail("expected .auth, got \(error)")
            }
            XCTAssertTrue(m.contains("status 429"), m)
            XCTAssertTrue(m.contains("locked"), m)
        }
    }

    /// Rust `parses_connections_patient_ids`.
    func testParsesConnectionsPatientIds() throws {
        XCTAssertEqual(try LibreLinkUpClient.parseConnections(fixture("libre-connections")),
                       ["p-1", "p-2"])
    }

    /// LibreLinkUp trend integers map to the right arrows (3 = Flat).
    /// Rust `maps_trend_arrows`.
    func testMapsTrendArrows() {
        XCTAssertEqual(LibreLinkUpClient.trendFromArrow(3), .flat)
        XCTAssertEqual(LibreLinkUpClient.trendFromArrow(5), .singleUp)
        XCTAssertEqual(LibreLinkUpClient.trendFromArrow(1), .singleDown)
        XCTAssertNil(LibreLinkUpClient.trendFromArrow(9))
    }

    /// The LibreLinkUp `M/D/YYYY h:mm:ss AM/PM` timestamp parses to UTC epoch ms,
    /// with correct 12-hour handling at noon and midnight. The case table is the
    /// shared fixture, asserted identically by the Rust tests.
    /// Rust `parses_factory_timestamp`.
    func testParsesFactoryTimestamp() throws {
        let table = try XCTUnwrap(
            JSONSerialization.jsonObject(with: fixture("libre-factory-timestamps")) as? [[String: Any]])
        XCTAssertFalse(table.isEmpty)
        for row in table {
            let s = try XCTUnwrap(row["s"] as? String)
            let expected = (row["ms"] as? NSNumber)?.int64Value // JSON null → nil
            XCTAssertEqual(LibreLinkUpClient.parseFactoryTimestamp(s), expected,
                           "parseFactoryTimestamp(\(s))")
        }
    }

    /// The graph response (shared fixture) yields the latest measurement (with trend)
    /// plus history. Rust `parses_graph_latest_and_history`.
    func testParsesGraphLatestAndHistory() throws {
        let samples = try LibreLinkUpClient.parseGraph(fixture("libre-graph"))
        XCTAssertEqual(samples.count, 3)
        XCTAssertEqual(samples[0].mgdl, 120)
        XCTAssertEqual(samples[0].direction, .flat) // latest has a trend
        XCTAssertEqual(samples[1].mgdl, 118)
        XCTAssertNil(samples[1].direction) // history points carry no trend
        XCTAssertEqual(samples[0].device, "librelinkup")
    }

    /// `JSONSerialization` backs a JSON boolean with an `NSNumber`, and a bare `as?
    /// Int` silently bridges `true`/`false` to `1`/`0` — unlike Rust's
    /// `serde_json::Value::as_i64()`, which returns `None` for a bool. A malformed
    /// `ValueInMgPerDl: true` / `TrendArrow: true` must be discarded, not read as a
    /// phantom mgdl-1 reading with a fabricated arrow.
    func testBooleanValueAndTrendArrowAreRejectedNotCoercedToOne() throws {
        let m: [String: Any] = [
            "ValueInMgPerDl": true,
            "FactoryTimestamp": "11/14/2023 10:13:19 PM",
            "TrendArrow": true,
        ]
        XCTAssertNil(LibreLinkUpClient.sampleFromMeasurement(m, withTrend: true))
    }

    /// The session cache trusts a bearer token exactly as long as its JWT `exp` claim;
    /// anything unreadable counts as already expired. (Swift-only: the Rust service
    /// logs in per poll and has no session cache.)
    func testJwtExp() throws {
        let payload = try JSONSerialization.data(withJSONObject: ["exp": 1_893_456_000])
        let b64url = payload.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
        XCTAssertEqual(LibreLinkUpClient.jwtExp("x.\(b64url).y"), 1_893_456_000)
        XCTAssertNil(LibreLinkUpClient.jwtExp("garbage"))
    }

    // MARK: - History backfill (Swift-side extension; verified against live-recorded shapes)

    func testBackfillURLs() {
        XCTAssertEqual(LibreLinkUpClient.historyURL(base: "https://api.libreview.io"),
                       "https://api.libreview.io/glucoseHistory?numPeriods=1&period=90")
        XCTAssertEqual(LibreLinkUpClient.historyURL(base: "https://api.libreview.io", days: 200),
                       "https://api.libreview.io/glucoseHistory?numPeriods=1&period=90", "days clamps to 90")
        XCTAssertEqual(LibreLinkUpClient.logbookURL(base: "https://api.libreview.io", patientId: "p-1"),
                       "https://api.libreview.io/llu/connections/p-1/logbook")
    }

    /// The logbook `data` is a flat array of measurement objects (the shape recorded
    /// from a live `/logbook` response); each parses into a `CgmSample`.
    func testParsesLogbook() throws {
        let samples = try LibreLinkUpClient.parseLogbook(fixture("libre-logbook"))
        XCTAssertEqual(samples.count, 3)
        XCTAssertEqual(samples.map(\.mgdl), [69, 67, 210])
        XCTAssertEqual(samples[0].device, "librelinkup")
        XCTAssertTrue(samples.allSatisfy { $0.dateMs > 0 })
        // Logbook points are events, not a trend series — no arrow attached.
        XCTAssertTrue(samples.allSatisfy { $0.direction == nil })
    }

    /// The glucose-history parser walks the (unverified) response shape and collects
    /// every measurement-shaped object, wherever it's nested.
    func testParsesGlucoseHistory() throws {
        let samples = try LibreLinkUpClient.parseGlucoseHistory(fixture("libre-glucosehistory"))
        XCTAssertEqual(samples.count, 2)
        XCTAssertEqual(samples.map(\.mgdl), [120, 125])
    }
}
