import XCTest

/// The analytics memo is what stops the per-minute current-reading poll from re-running
/// a ~25k-reading FFI analytics round-trip every cycle: `APIClient.localReportJSON`
/// caches on `(kind, window, owner, maxDateMs, count, checksum, tz)` and only recomputes
/// when one of those changes. These tests pin that key's cache-hit / cache-miss semantics
/// directly (Issue #41 Verification step 8), independent of the FFI.
final class AnalyticsMemoTests: XCTestCase {

    private func key(kind: AnalyticsMemo.Kind = .analytics,
                     window: Int = 24, owner: String = "dexcom:us:a",
                     maxDateMs: Int64 = 1_000, count: Int = 100,
                     checksum: Int64 = 5_000, tz: Int = 0) -> AnalyticsMemo.Key {
        AnalyticsMemo.Key(kind: kind, window: window, owner: owner,
                          maxDateMs: maxDateMs, count: count, checksum: checksum, tz: tz)
    }

    /// A current-reading-only poll (same period, owner, newest reading, count, tz) is a
    /// cache HIT — the FFI is not re-invoked.
    func testUnchangedKeyIsACacheHit() async {
        let memo = AnalyticsMemo()
        let payload = Data("{\"n\":100}".utf8)
        await memo.set(key(), payload)
        let hit = await memo.get(key())
        XCTAssertEqual(hit, payload)
    }

    /// New readings landing (maxDateMs and/or count advance) is a cache MISS, so the
    /// report recomputes.
    func testNewReadingsMissTheCache() async {
        let memo = AnalyticsMemo()
        await memo.set(key(maxDateMs: 1_000, count: 100), Data("old".utf8))
        let newerTimestamp = await memo.get(key(maxDateMs: 2_000, count: 100))
        XCTAssertNil(newerTimestamp, "a newer reading must not reuse the stale report")
        let moreReadings = await memo.get(key(maxDateMs: 1_000, count: 101))
        XCTAssertNil(moreReadings, "a changed reading count must not reuse the stale report")
    }

    /// An in-place value revision (a vendor re-smoothing the recent window, or a CSV
    /// re-import over the same timestamps) changes neither `count` nor `maxDateMs` — only
    /// the value `checksum`. That must still be a cache MISS, or the dashboard shows
    /// analytics computed from the pre-correction values.
    func testRevisedValueMissesTheCache() async {
        let memo = AnalyticsMemo()
        await memo.set(key(maxDateMs: 1_000, count: 100, checksum: 5_000), Data("old".utf8))
        let revised = await memo.get(key(maxDateMs: 1_000, count: 100, checksum: 5_123))
        XCTAssertNil(revised, "a revised reading value must not reuse the stale report")
    }

    /// Period change, source/owner change, AGP-vs-analytics, and tz change are all
    /// distinct cache entries — none aliases another.
    func testEveryKeyIngredientDiscriminates() async {
        let memo = AnalyticsMemo()
        await memo.set(key(), Data("base".utf8))
        let agp = await memo.get(key(kind: .agp))
        let window = await memo.get(key(window: 168))
        let owner = await memo.get(key(owner: "libre:b"))
        let tz = await memo.get(key(tz: 60))
        XCTAssertNil(agp)
        XCTAssertNil(window)
        XCTAssertNil(owner)
        XCTAssertNil(tz)
        // The original is still there — the misses above didn't disturb it.
        let base = await memo.get(key())
        XCTAssertEqual(base, Data("base".utf8))
    }

    /// A reset/disconnect clears the memo so a wiped store can't serve the old owner's
    /// analytics.
    func testClearEmptiesTheMemo() async {
        let memo = AnalyticsMemo()
        await memo.set(key(), Data("x".utf8))
        await memo.clear()
        let afterClear = await memo.get(key())
        XCTAssertNil(afterClear)
    }
}
