import XCTest

// Shared (ReadingCache, models) + NightKnightWidget.swift (Provider) are compiled into
// this target, so their types are visible without an import.

/// Guards the fix for "the widget loses data after a while": a failed refresh must fall
/// back to the last cached reading instead of blanking to "--".
final class ReadingCacheTests: XCTestCase {

    func testRoundTripThroughAppGroup() {
        let reading = CurrentReading(date: Date(timeIntervalSince1970: 1_700_000_000),
                                     value: GlucoseValue(mgdl: 137), trend: .singleUp, trendLabel: "")
        ReadingCache.save(reading)

        let loaded = ReadingCache.load()
        XCTAssertEqual(loaded?.value.mgdl, 137)
        XCTAssertEqual(loaded?.trend, .singleUp)
        XCTAssertEqual(loaded?.date.timeIntervalSince1970, 1_700_000_000)
    }

    /// When the fetch fails (nil) but a reading is cached, the widget shows the cached
    /// value — the core of the "don't blank on a transient failure" fix.
    func testEntryFallsBackToCachedReading() {
        let cached = CurrentReading(date: .now, value: GlucoseValue(mgdl: 95), trend: .flat, trendLabel: "")
        let entry = Provider.entry(for: cached, unit: .mgdl)
        XCTAssertEqual(entry.value?.mgdl, 95, "should show the cached value, not --")
        XCTAssertEqual(entry.trend, .flat)
    }

    /// Only when there is neither a fetch nor a cache should the widget show "--".
    func testEntryShowsNoValueWhenNothingAvailable() {
        let entry = Provider.entry(for: nil, unit: .mgdl)
        XCTAssertNil(entry.value)
    }

    // MARK: - reading() resolution (the "stale after delete" fix)

    private var fresh: CurrentReading {
        CurrentReading(date: .now, value: GlucoseValue(mgdl: 140), trend: .singleUp, trendLabel: "")
    }
    private var cached: CurrentReading {
        CurrentReading(date: .now, value: GlucoseValue(mgdl: 95), trend: .flat, trendLabel: "")
    }

    func testReadingPrefersFreshFetch() {
        XCTAssertEqual(Provider.reading(fetched: fresh, cached: cached, isConfigured: true)?.value.mgdl, 140)
    }

    func testReadingFallsBackToCacheOnTransientFailure() {
        XCTAssertEqual(Provider.reading(fetched: nil, cached: cached, isConfigured: true)?.value.mgdl, 95,
                       "a configured widget keeps the last reading through a transient failure")
    }

    /// The core of the "credentials cached even when deleted" UX fix: once the account is
    /// removed (not configured), the widget must NOT keep showing the cached glucose.
    func testReadingShowsNothingWhenNotConfigured() {
        XCTAssertNil(Provider.reading(fetched: nil, cached: cached, isConfigured: false),
                     "a disconnected widget must drop to -- rather than show stale glucose")
    }

    // MARK: - ReadingCache.clear

    func testClearRemovesCachedReading() {
        ReadingCache.save(CurrentReading(date: .now, value: GlucoseValue(mgdl: 88), trend: .flat, trendLabel: ""))
        XCTAssertNotNil(ReadingCache.load(), "precondition: something is cached")
        ReadingCache.clear()
        XCTAssertNil(ReadingCache.load())
    }
}
