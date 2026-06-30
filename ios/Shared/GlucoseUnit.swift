import Foundation

/// Blood-glucose unit. Mirrors the server: both units are first-class, conversion
/// uses the single molar-mass constant 18.0156, all maths is on canonical mg/dL.
enum GlucoseUnit: String, CaseIterable, Codable, Sendable {
    case mgdl = "mg/dl"
    case mmol = "mmol/l"

    /// mg/dL in one mmol/L of glucose (molar mass 180.156 g/mol).
    static let mgdlPerMmol = 18.0156

    var label: String { self == .mgdl ? "mg/dL" : "mmol/L" }
}

/// A glucose value carrying canonical mg/dL, with display in either unit.
struct GlucoseValue: Hashable, Sendable {
    let mgdl: Double

    var mmol: Double { mgdl / GlucoseUnit.mgdlPerMmol }

    func value(in unit: GlucoseUnit) -> Double {
        unit == .mgdl ? mgdl : mmol
    }

    /// Display string with the conventional precision (integer mg/dL, 0.1 mmol/L).
    func display(in unit: GlucoseUnit) -> String {
        switch unit {
        case .mgdl: return String(Int(mgdl.rounded()))
        case .mmol: return String(format: "%.1f", mmol)
        }
    }
}

/// Clinical glucose band (ADA/ATTD consensus thresholds, mg/dL).
enum GlucoseBand: Sendable, Equatable {
    case veryLow, low, inRange, high, veryHigh

    static func of(mgdl: Double) -> GlucoseBand {
        if mgdl < 54 { return .veryLow }
        if mgdl < 70 { return .low }
        if mgdl <= 180 { return .inRange }
        if mgdl <= 250 { return .high }
        return .veryHigh
    }

    /// Plain-language level status, matching the CGM ecosystem ("Urgent low" at the
    /// level-2 threshold) and the server's `GlucoseBand::label`. This is the glucose
    /// **level** dimension — distinct from the **trend** (see `TrendDirection.label`).
    var label: String {
        switch self {
        case .veryLow: return "Urgent low"
        case .low: return "Low"
        case .inRange: return "In range"
        case .high: return "High"
        case .veryHigh: return "Urgent high"
        }
    }
}
