import Foundation

/// A timestamped glucose reading.
struct GlucoseReading: Identifiable, Hashable, Sendable {
    let date: Date
    let value: GlucoseValue
    var id: Date { date }
    var mgdl: Double { value.mgdl }
}

/// Trend arrow (Nightscout names), with a display glyph.
enum TrendDirection: String, Sendable {
    case doubleUp = "DoubleUp", singleUp = "SingleUp", fortyFiveUp = "FortyFiveUp"
    case flat = "Flat"
    case fortyFiveDown = "FortyFiveDown", singleDown = "SingleDown", doubleDown = "DoubleDown"
    case none = "NONE"

    init(name: String?) { self = TrendDirection(rawValue: name ?? "") ?? .none }

    var glyph: String {
        switch self {
        case .doubleUp: return "⇈"
        case .singleUp: return "↑"
        case .fortyFiveUp: return "↗"
        case .flat: return "→"
        case .fortyFiveDown: return "↘"
        case .singleDown: return "↓"
        case .doubleDown: return "⇊"
        case .none: return "–"
        }
    }

    /// Plain-language trend label matching the CGM ecosystem (Dexcom/Libre wording)
    /// and the server's `Direction::label`. The raw-value spellings (`DoubleUp`, …)
    /// stay on the wire; this is what the user sees.
    var label: String {
        switch self {
        case .doubleUp: return "Rising rapidly"
        case .singleUp: return "Rising"
        case .fortyFiveUp: return "Rising slowly"
        case .flat: return "Steady"
        case .fortyFiveDown: return "Falling slowly"
        case .singleDown: return "Falling"
        case .doubleDown: return "Falling rapidly"
        case .none: return ""
        }
    }
}

/// The latest reading + trend (decoded from `/api/v4/current`).
struct CurrentReading: Sendable {
    let date: Date
    let value: GlucoseValue
    let trend: TrendDirection
}

/// Trailing analytics (decoded from `/api/v4/analytics`).
struct GlucoseAnalytics: Sendable {
    let n: Int
    let meanMgdl: Double?
    let gmiPercent: Double?
    let estimatedA1cPercent: Double?
    let cvPercent: Double?
    let veryLowPct, lowPct, inRangePct, highPct, veryHighPct: Double
}

/// Trailing period options for the summary selector (1–90 days).
enum TrailingPeriod: Int, CaseIterable, Identifiable {
    case day = 1, week = 7, twoWeeks = 14, month = 30, quarter = 90
    var id: Int { rawValue }
    var label: String { self == .day ? "24h" : "\(rawValue)d" }
    var hours: Int { rawValue * 24 }
}
