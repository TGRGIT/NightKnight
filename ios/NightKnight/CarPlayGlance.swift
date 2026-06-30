import Foundation

/// Pure formatting for the CarPlay glance, kept free of CarPlay/UIKit so it can be
/// unit-tested hostlessly — the scene delegate maps these rows into `CPInformationItem`s.
///
/// Driving Task apps must stay glanceable: a few short rows, no charts, no interaction
/// while moving. We surface exactly the four things a driver would otherwise pick up the
/// phone to read — the current value (+ unit), the level status, the trend, and how fresh
/// the reading is — value-forward (the data is the row's prominent title).
enum CarPlayGlance {
    /// One title/detail row of the information template (title is the prominent text).
    struct Item: Equatable {
        let title: String
        let detail: String
    }

    /// Build the glance rows for a reading, or short guidance when there's nothing to show
    /// (no account configured, or no cached reading yet). `now` is injectable so the
    /// "updated N min ago" line is deterministic in tests.
    static func items(for reading: CurrentReading?, unit: GlucoseUnit, now: Date = .now) -> [Item] {
        guard let r = reading else {
            return [Item(title: "No glucose data", detail: "Open NightKnight on your phone")]
        }
        let band = GlucoseBand.of(mgdl: r.value.mgdl)
        let value = "\(r.value.display(in: unit)) \(unit.label)"
        let trend = r.trend == .none ? "--" : "\(r.trendLabel) \(r.trend.glyph)"
        return [
            Item(title: value, detail: band.label),
            Item(title: trend, detail: "Trend"),
            Item(title: age(of: r.date, now: now), detail: "Updated"),
        ]
    }

    /// The reading's freshness as a short phrase: "just now", "3 min ago", "1 hr 5 min ago".
    static func age(of date: Date, now: Date = .now) -> String {
        let minutes = max(0, Int(now.timeIntervalSince(date))) / 60
        if minutes <= 0 { return "just now" }
        if minutes < 60 { return "\(minutes) min ago" }
        let hours = minutes / 60
        let rem = minutes % 60
        return rem == 0 ? "\(hours) hr ago" : "\(hours) hr \(rem) min ago"
    }
}
