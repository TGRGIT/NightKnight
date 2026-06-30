import XCTest

// CarPlayGlance.swift and the Shared models are compiled directly into this hostless
// test target, so their types are visible without an import (mirrors NightKnightWidgetTests).

/// Guards the CarPlay "Driving Task" glance: the rows a driver sees must be the right,
/// glanceable summary of a reading — value + unit, level status, trend, and freshness —
/// and must degrade to clear guidance (not a blank or "--") when there's no data.
final class CarPlayGlanceTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    private func reading(_ mgdl: Double, _ trend: TrendDirection = .flat,
                         minutesAgo: Int = 0, label: String? = nil) -> CurrentReading {
        let t = trend
        return CurrentReading(date: now.addingTimeInterval(-Double(minutesAgo) * 60),
                              value: GlucoseValue(mgdl: mgdl), trend: t,
                              trendLabel: label ?? t.label)
    }

    // MARK: - Row content

    func testInRangeReadingRowsMgdl() {
        let items = CarPlayGlance.items(for: reading(112, .flat, minutesAgo: 3), unit: .mgdl, now: now)
        XCTAssertEqual(items.count, 3, "glance is three short rows — value, trend, freshness")
        XCTAssertEqual(items[0], .init(title: "112 mg/dL", detail: "In range"))
        XCTAssertEqual(items[1], .init(title: "Steady →", detail: "Trend"))
        XCTAssertEqual(items[2], .init(title: "3 min ago", detail: "Updated"))
    }

    func testValueIsShownInThePreferredUnit() {
        // 90 mg/dL ÷ 18.0156 ≈ 5.0 mmol/L (one decimal), the conventional mmol precision.
        let items = CarPlayGlance.items(for: reading(90, .fortyFiveUp), unit: .mmol, now: now)
        XCTAssertEqual(items[0].title, "5.0 mmol/L")
        XCTAssertEqual(items[1].title, "Rising slowly ↗")
    }

    func testLevelStatusReflectsBands() {
        XCTAssertEqual(CarPlayGlance.items(for: reading(48), unit: .mgdl, now: now)[0].detail, "Urgent low")
        XCTAssertEqual(CarPlayGlance.items(for: reading(65), unit: .mgdl, now: now)[0].detail, "Low")
        XCTAssertEqual(CarPlayGlance.items(for: reading(140), unit: .mgdl, now: now)[0].detail, "In range")
        XCTAssertEqual(CarPlayGlance.items(for: reading(220), unit: .mgdl, now: now)[0].detail, "High")
        XCTAssertEqual(CarPlayGlance.items(for: reading(300), unit: .mgdl, now: now)[0].detail, "Urgent high")
    }

    func testUnknownTrendShowsPlaceholderNotArrow() {
        let items = CarPlayGlance.items(for: reading(120, .none), unit: .mgdl, now: now)
        XCTAssertEqual(items[1], .init(title: "--", detail: "Trend"),
                       "a missing trend should not render a stray arrow glyph")
    }

    // MARK: - No data

    func testNoReadingShowsGuidanceRow() {
        let items = CarPlayGlance.items(for: nil, unit: .mgdl, now: now)
        XCTAssertEqual(items, [.init(title: "No glucose data", detail: "Open NightKnight on your phone")],
                       "an unconfigured / empty state must guide, never blank")
    }

    // MARK: - Freshness phrasing

    func testAgePhrasing() {
        XCTAssertEqual(CarPlayGlance.age(of: now, now: now), "just now")
        XCTAssertEqual(CarPlayGlance.age(of: now.addingTimeInterval(-30), now: now), "just now")
        XCTAssertEqual(CarPlayGlance.age(of: now.addingTimeInterval(-60), now: now), "1 min ago")
        XCTAssertEqual(CarPlayGlance.age(of: now.addingTimeInterval(-59 * 60), now: now), "59 min ago")
        XCTAssertEqual(CarPlayGlance.age(of: now.addingTimeInterval(-60 * 60), now: now), "1 hr ago")
        XCTAssertEqual(CarPlayGlance.age(of: now.addingTimeInterval(-65 * 60), now: now), "1 hr 5 min ago")
    }

    /// A reading dated slightly in the future (clock skew between phone and server) must
    /// not produce a negative or nonsense age.
    func testFutureReadingClampsToJustNow() {
        XCTAssertEqual(CarPlayGlance.age(of: now.addingTimeInterval(120), now: now), "just now")
    }
}
