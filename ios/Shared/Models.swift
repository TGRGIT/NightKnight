import Foundation

/// A timestamped glucose reading.
struct GlucoseReading: Identifiable, Hashable, Sendable {
    let date: Date
    let value: GlucoseValue
    var id: Date { date }
    var mgdl: Double { value.mgdl }
}

/// Trend arrow (Nightscout names), with a display glyph and a plain-language label
/// that mirrors the server's `trend` module (steady / 45° drift / direct / fast).
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

    var label: String {
        switch self {
        case .doubleUp: return "Rising fast"
        case .singleUp: return "Rising"
        case .fortyFiveUp: return "Drifting up"
        case .flat: return "Steady"
        case .fortyFiveDown: return "Drifting down"
        case .singleDown: return "Falling"
        case .doubleDown: return "Falling fast"
        case .none: return "No trend"
        }
    }
}

/// The latest reading + trend (decoded from `/api/v4/current`).
struct CurrentReading: Sendable {
    let date: Date
    let value: GlucoseValue
    let trend: TrendDirection
    /// The server's plain-language trend label (falls back to the local one).
    let trendLabel: String
}

/// Trailing analytics (decoded from `/api/v4/analytics`). Keeps the original flat TIR
/// fields for the dashboard, and adds the full Statistical-Analysis set for the
/// Analysis view.
struct GlucoseAnalytics: Sendable {
    let n: Int
    let meanMgdl: Double?
    let sdMgdl: Double?
    let gmiPercent: Double?
    let estimatedA1cPercent: Double?
    let cvPercent: Double?
    let veryLowPct, lowPct, inRangePct, highPct, veryHighPct: Double
    let coverage: CoverageInfo
    let gri: GriInfo
    let variability: VariabilityInfo
    let patterns: [PeriodInfo]
    let episodes: EpisodesInfo
}

/// How much data the metrics are based on.
struct CoverageInfo: Sendable {
    let percentActive: Double?
    let daysCovered: Double?
    let distinctDays: Int?
    let sufficient: Bool
}

/// Glycemia Risk Index summary.
struct GriInfo: Sendable {
    let value: Double?
    let zone: String?
    let hypoComponent: Double?
    let hyperComponent: Double?
}

/// Advanced variability indices (mg/dL where applicable).
struct VariabilityInfo: Sendable {
    let jIndex: Double?
    let mage: Double?
    let conga: Double?
    let modd: Double?
    let congaHours: Double?
}

/// One time-of-day period's summary.
struct PeriodInfo: Sendable, Identifiable {
    let startHour: Int
    let endHour: Int
    let n: Int
    let meanMgdl: Double?
    let inRangePct: Double?
    var id: Int { startHour }
}

/// A roll-up of one threshold's episodes.
struct EpisodeStat: Sendable {
    let count: Int
    let nocturnal: Int
    let perDay: Double
    let longestMin: Double
    let totalMin: Double
}

/// One recent episode for the feed.
struct RecentEpisode: Sendable, Identifiable {
    let id = UUID()
    let kind: String           // "low" | "high"
    let start: Date
    let durationMin: Double
    let extremeMgdl: Double
    let nocturnal: Bool
}

/// Episodes block: per-threshold roll-ups plus the recent feed.
struct EpisodesInfo: Sendable {
    let low, veryLow, high, veryHigh: EpisodeStat
    let recent: [RecentEpisode]
}

/// One AGP time-of-day bin (percentiles in mg/dL).
struct AgpBin: Sendable, Identifiable {
    let minuteOfDay: Int
    let n: Int
    let p05, p25, p50, p75, p95: Double?
    var id: Int { minuteOfDay }
}

/// Trailing period options for the summary selector (1–90 days).
enum TrailingPeriod: Int, CaseIterable, Identifiable {
    case day = 1, week = 7, twoWeeks = 14, month = 30, quarter = 90
    var id: Int { rawValue }
    var label: String { self == .day ? "24h" : "\(rawValue)d" }
    var hours: Int { rawValue * 24 }
}
