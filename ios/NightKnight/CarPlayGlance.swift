import Foundation

/// Pure formatting for the CarPlay glance, kept free of CarPlay/UIKit so it can be
/// unit-tested hostlessly — the scene delegate maps these rows into `CPListItem`s and
/// resolves each row's `tint` to a concrete colour.
///
/// Driving Task apps must stay glanceable: a few short rows, no charts, no interaction
/// while moving. We surface exactly the four things a driver would otherwise pick up the
/// phone to read — the current value (+ unit), the level status, the trend, and how fresh
/// the reading is — value-forward, each row carrying a colour-coded leading icon so the
/// "is everything OK" signal lands at a glance (green / amber / red), not just in text.
enum CarPlayGlance {
    /// Semantic tint for a row's leading icon. Resolved to a concrete `UIColor` by the
    /// scene delegate, so this stays UIKit-free (and hostlessly testable).
    enum Tint: Equatable {
        /// Coloured by the current glucose band (the value and trend rows), so the whole
        /// glance reads green / amber / red with the level.
        case level(GlucoseBand)
        /// A quiet, secondary tint (freshness).
        case muted
        /// Attention (the no-data guidance row).
        case accent
    }

    /// One row of the glance: prominent `title`, secondary `detail`, and a colour-coded
    /// leading SF Symbol (`symbol` + `tint`).
    struct Item: Equatable {
        let title: String
        let detail: String
        /// SF Symbol system name for the row's leading icon.
        let symbol: String
        let tint: Tint
    }

    /// Build the glance rows for a reading, or short guidance when there's nothing to show
    /// (no account configured, or no cached reading yet). `now` is injectable so the
    /// "updated N min ago" line is deterministic in tests.
    static func items(for reading: CurrentReading?, unit: GlucoseUnit, now: Date = .now) -> [Item] {
        guard let r = reading else {
            return [Item(title: "No glucose data",
                         detail: "Open NightKnight on your phone",
                         symbol: "exclamationmark.triangle.fill",
                         tint: .accent)]
        }
        let band = GlucoseBand.of(mgdl: r.value.mgdl)
        let value = "\(r.value.display(in: unit)) \(unit.label)"
        // The leading arrow icon carries the direction, so the title is just the label
        // (no inline glyph). An unknown trend reads as a dash rather than a stray arrow.
        let trendTitle = r.trend == .none ? "—" : r.trendLabel
        return [
            Item(title: value, detail: band.label, symbol: "circle.fill", tint: .level(band)),
            Item(title: trendTitle, detail: "Trend", symbol: symbol(for: r.trend), tint: .level(band)),
            Item(title: age(of: r.date, now: now), detail: "Updated", symbol: "clock", tint: .muted),
        ]
    }

    /// SF Symbol arrow for a trend direction. Direction is the glance; magnitude (rapid vs
    /// slow) lives in the row's label, so single/double share an arrow.
    static func symbol(for trend: TrendDirection) -> String {
        switch trend {
        case .doubleUp, .singleUp: return "arrow.up"
        case .fortyFiveUp: return "arrow.up.right"
        case .flat: return "arrow.right"
        case .fortyFiveDown: return "arrow.down.right"
        case .singleDown, .doubleDown: return "arrow.down"
        case .none: return "minus"
        }
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
