import SwiftUI

/// Hub System palette + glucose band colours, matching the web dashboard.
extension Color {
    static let nkInk = Color(red: 0.043, green: 0.055, blue: 0.071)
    static let nkTile = Color(red: 0.071, green: 0.094, blue: 0.129)
    static let nkAccent = Color(red: 0.898, green: 0.282, blue: 0.302)
    static let nkInRange = Color(red: 0.212, green: 0.769, blue: 0.420)
    static let nkWarn = Color(red: 0.878, green: 0.635, blue: 0.235)
    static let nkDanger = Color(red: 0.898, green: 0.282, blue: 0.302)
    static let nkMuted = Color(red: 0.576, green: 0.627, blue: 0.678)
}

extension GlucoseBand {
    var color: Color {
        switch self {
        case .veryLow, .veryHigh: return .nkDanger
        case .low, .high: return .nkWarn
        case .inRange: return .nkInRange
        }
    }
}
