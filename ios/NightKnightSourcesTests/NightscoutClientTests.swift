import XCTest

// Shared (NightscoutClient, StandaloneSource, Models) is compiled directly into this
// target, so its types are visible without an import. These tests mirror the Rust
// reference tests in `service/crates/nightknight-connectors/src/nightscout.rs` and
// assert against the SAME fixture bytes under `ios/Tests/Fixtures` (bundle resources),
// so the two parsers cannot drift silently.

final class NightscoutClientTests: XCTestCase {

    private func fixture(_ name: String) throws -> Data {
        try Data(contentsOf: XCTUnwrap(
            Bundle(for: Self.self).url(forResource: name, withExtension: "json"),
            "missing fixture \(name).json"))
    }

    /// A pasted URL is reduced to the instance origin, and the read URL clamps its
    /// count to a sane ceiling. Rust `normalizes_base_and_builds_url`.
    func testNormalizesBaseAndBuildsURL() {
        XCTAssertEqual(NightscoutClient.normalizeBase("https://x.cooney.be/"),
                       "https://x.cooney.be")
        // A pasted full endpoint URL is reduced to the origin.
        XCTAssertEqual(
            NightscoutClient.normalizeBase("https://x.cooney.be/api/v1/entries/sgv?count=100"),
            "https://x.cooney.be")
        XCTAssertEqual(NightscoutClient.readURL(base: "https://x.cooney.be", count: 50),
                       "https://x.cooney.be/api/v1/entries/sgv.json?count=50")
        // Count is clamped to a sane ceiling.
        XCTAssertTrue(NightscoutClient.readURL(base: "https://x.cooney.be", count: 9_999_999)
            .hasSuffix("count=131072"))
    }

    /// The `find[date][$lt]=ms` filter is percent-encoded so successive pages can
    /// walk the history backward. Rust `builds_paginated_history_url`.
    func testBuildsPaginatedHistoryURL() {
        XCTAssertEqual(
            NightscoutClient.readURLBefore(base: "https://x.cooney.be", count: 2000,
                                           beforeMs: 1_700_000_000_000),
            "https://x.cooney.be/api/v1/entries/sgv.json?count=2000&find%5Bdate%5D%5B%24lt%5D=1700000000000")
        // The first page (cursor = Int64.max) is "from the most recent", and a
        // negative cursor is floored to 0 (no negative in the URL).
        XCTAssertTrue(
            NightscoutClient.readURLBefore(base: "https://x.cooney.be", count: 2000,
                                           beforeMs: Int64.max)
                .contains("%24lt%5D=9223372036854775807"))
        XCTAssertTrue(
            NightscoutClient.readURLBefore(base: "https://x.cooney.be", count: 2000,
                                           beforeMs: -5)
                .hasSuffix("%24lt%5D=0"))
    }

    /// The full allow/deny table is the shared fixture `ssrf-table.json` — it is the
    /// spec for `isSafeBase`, asserted from both languages.
    /// Rust `ssrf_guard_matches_the_shared_table`.
    func testSSRFGuardMatchesTheSharedTable() throws {
        let table = try XCTUnwrap(
            JSONSerialization.jsonObject(with: fixture("ssrf-table")) as? [[String: Any]])
        XCTAssertGreaterThanOrEqual(table.count, 24, "SSRF table lost rows")
        for row in table {
            let url = try XCTUnwrap(row["url"] as? String)
            let safe = try XCTUnwrap(row["safe"] as? Bool)
            XCTAssertEqual(NightscoutClient.isSafeBase(url), safe,
                           "isSafeBase(\(url)) should be \(safe)")
        }
    }

    /// The exact shape returned by the live endpoint (shared fixture) parses into
    /// samples with mg/dL, time, trend, device. Rust `parses_a_real_entries_payload`
    /// (minus `to_entry_json`, which has no Swift counterpart on CgmSample — `_id` is
    /// dropped at parse time here, so nothing to re-assert).
    func testParsesARealEntriesPayload() throws {
        let samples = try NightscoutClient.parseEntries(fixture("nightscout-entries"))
        XCTAssertEqual(samples.count, 2, "the cal record and the 0-sgv reading are skipped")
        XCTAssertEqual(samples[0].mgdl, 91)
        XCTAssertEqual(samples[0].dateMs, 1_782_404_097_000)
        XCTAssertEqual(samples[0].direction, .flat)
        XCTAssertEqual(samples[0].device, "nightscout-librelink-up")
        XCTAssertEqual(samples[1].direction, .fortyFiveUp)
    }

    /// Rust `empty_or_garbage_body_is_handled`.
    func testEmptyOrGarbageBodyIsHandled() throws {
        XCTAssertEqual(try NightscoutClient.parseEntries(Data("[]".utf8)).count, 0)
        XCTAssertThrowsError(try NightscoutClient.parseEntries(Data("not json".utf8)))
        XCTAssertThrowsError(try NightscoutClient.parseEntries(Data("{\"not\":\"an array\"}".utf8)))
    }

    /// `JSONSerialization` backs a JSON boolean with an `NSNumber`, and a bare `as?
    /// Int`/`as? Int64` silently bridges `true`/`false` to `1`/`0` — unlike Rust's
    /// `serde_json::Value::as_i64()`, which returns `None` for a bool. A malformed or
    /// hostile record (the Nightscout origin is user-supplied) must be discarded, not
    /// read as a phantom `mgdl: 1` reading at epoch 1ms.
    func testBooleanSgvAndDateAreRejectedNotCoercedToOne() throws {
        let body = Data(#"[{"type":"sgv","sgv":true,"date":true}]"#.utf8)
        XCTAssertEqual(try NightscoutClient.parseEntries(body).count, 0,
                       "a boolean sgv/date must be discarded, not coerced to 1")
    }

    /// A JSON *float* must be treated exactly as Rust `serde_json`'s `as_i64()` treats it:
    /// `sgv` is rounded (Rust `as_i64().or_else(as_f64().round())`), while `date` is
    /// integer-only (`as_i64()` → `None` for a float) so the record is skipped. Foundation's
    /// `NSNumber.intValue`/`.int64Value` would instead *truncate* (`90.6 → 90`, a float
    /// `date` kept), silently diverging from the server's "byte-identical" output — the
    /// `jsonInt`/`jsonInt64` float guard is what prevents that.
    func testFractionalSgvRoundsAndFloatDateIsRejected() throws {
        // sgv 90.6 rounds to 91 (server would store 91, not a truncated 90).
        let rounded = try NightscoutClient.parseEntries(
            Data(#"[{"type":"sgv","sgv":90.6,"date":1782404097000}]"#.utf8))
        XCTAssertEqual(rounded.count, 1)
        XCTAssertEqual(rounded[0].mgdl, 91, "a fractional sgv rounds, matching the Rust reference")
        // A float `date` is not an integer → the record is skipped, like Rust `as_i64()`.
        XCTAssertEqual(
            try NightscoutClient.parseEntries(Data(#"[{"type":"sgv","sgv":90,"date":1.782404097e12}]"#.utf8)).count,
            0, "a float date must be skipped, not truncated to an integer")
    }

    /// The history page reports the RAW record count (before filtering) and the
    /// oldest raw `date`, so the backfill can tell "a full page that filtered down"
    /// from "a short page (end of history)". The payload has 4 raw records but only
    /// 2 usable sgv samples; the page must report `rawLen == 4`, not 2 — otherwise
    /// one error-coded row anywhere in a full backfill page would look like
    /// end-of-history and abandon all older readings.
    /// Rust `history_page_reports_raw_count_and_oldest_date`.
    func testHistoryPageReportsRawCountAndOldestDate() throws {
        let page = try NightscoutClient.parseHistoryPage(fixture("nightscout-history-page"))
        XCTAssertEqual(page.rawLen, 4, "raw count counts every record, before filtering")
        XCTAssertEqual(page.samples.count, 2, "only the two real sgv readings are usable")
        XCTAssertEqual(page.rawMinDate, 1_782_403_800_000,
                       "oldest raw date includes the filtered-out rows, so the cursor can still advance")
    }

    /// A page whose rows are ALL error codes / non-sgv still surfaces a non-empty raw
    /// count and an oldest date, so the backfill advances past it instead of stalling
    /// forever. Rust `all_filtered_page_still_reports_raw_progress`.
    func testAllFilteredPageStillReportsRawProgress() throws {
        let page = try NightscoutClient.parseHistoryPage(fixture("nightscout-history-page-filtered"))
        XCTAssertEqual(page.rawLen, 2)
        XCTAssertTrue(page.samples.isEmpty)
        XCTAssertEqual(page.rawMinDate, 1_782_403_800_000)
    }
}
