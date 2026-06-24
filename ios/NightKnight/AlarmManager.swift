import Foundation
import UserNotifications

/// On-device glucose alarms — out-of-range and rapid-drop — delivered as local
/// notifications. Fully disableable: nothing fires unless `settings.alarmsEnabled`.
/// Throttled so a sustained low/high doesn't spam.
@MainActor
final class AlarmManager {
    static let shared = AlarmManager()
    private var lastFired: Date?
    private let throttle: TimeInterval = 15 * 60

    func requestAuth() async {
        _ = try? await UNUserNotificationCenter.current()
            .requestAuthorization(options: [.alert, .sound, .badge])
    }

    func evaluate(_ current: CurrentReading, settings: Settings) {
        guard settings.alarmsEnabled else { return }
        let mgdl = current.value.mgdl
        let shown = current.value.display(in: settings.preferredUnit)
        let unit = settings.preferredUnit.label

        var message: String?
        if mgdl < settings.lowThresholdMgdl {
            message = "Low glucose: \(shown) \(unit)"
        } else if mgdl > settings.highThresholdMgdl {
            message = "High glucose: \(shown) \(unit)"
        } else if settings.fastDropAlarm,
                  current.trend == .singleDown || current.trend == .doubleDown,
                  mgdl < settings.lowThresholdMgdl + 30 {
            message = "Dropping fast: \(shown) \(unit) \(current.trend.glyph)"
        }
        guard let message else { return }

        if let last = lastFired, Date().timeIntervalSince(last) < throttle { return }
        lastFired = Date()

        let content = UNMutableNotificationContent()
        content.title = "NightKnight"
        content.body = message
        content.sound = .defaultCritical
        content.interruptionLevel = .timeSensitive
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}
