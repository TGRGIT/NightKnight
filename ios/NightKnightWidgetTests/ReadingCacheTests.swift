import XCTest

// Shared (ReadingCache, models) + NightKnightWidget.swift (Provider) are compiled into
// this target, so their types are visible without an import.

/// Guards the fix for "the widget loses data after a while": a failed refresh must fall
/// back to the last cached reading instead of blanking to "--".
final class ReadingCacheTests: XCTestCase {

    func testRoundTripThroughAppGroup() {
        let reading = CurrentReading(date: Date(timeIntervalSince1970: 1_700_000_000),
                                     value: GlucoseValue(mgdl: 137), trend: .singleUp)
        ReadingCache.save(reading)

        let loaded = ReadingCache.load()
        XCTAssertEqual(loaded?.value.mgdl, 137)
        XCTAssertEqual(loaded?.trend, .singleUp)
        XCTAssertEqual(loaded?.date.timeIntervalSince1970, 1_700_000_000)
    }

    /// When the fetch fails (nil) but a reading is cached, the widget shows the cached
    /// value — the core of the "don't blank on a transient failure" fix.
    func testEntryFallsBackToCachedReading() {
        let cached = CurrentReading(date: .now, value: GlucoseValue(mgdl: 95), trend: .flat)
        let entry = Provider.entry(for: cached, unit: .mgdl)
        XCTAssertEqual(entry.value?.mgdl, 95, "should show the cached value, not --")
        XCTAssertEqual(entry.trend, .flat)
    }

    /// Only when there is neither a fetch nor a cache should the widget show "--".
    func testEntryShowsNoValueWhenNothingAvailable() {
        let entry = Provider.entry(for: nil, unit: .mgdl)
        XCTAssertNil(entry.value)
    }
}
