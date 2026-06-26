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

    /// Ask for notification permission and report the resulting status, so the caller can
    /// surface a warning if the user declined (otherwise a denied permission is invisible —
    /// the toggle looks on but nothing ever delivers).
    @discardableResult
    func requestAuth() async -> UNAuthorizationStatus {
        let center = UNUserNotificationCenter.current()
        _ = try? await center.requestAuthorization(options: [.alert, .sound, .badge])
        return await center.notificationSettings().authorizationStatus
    }

    /// Current notification authorization status (for the Settings "notifications are off"
    /// warning when alarms are enabled but iOS won't deliver them).
    func authorizationStatus() async -> UNAuthorizationStatus {
        await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
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
        content.sound = .default
        // Time-sensitive breaks through Focus / Do Not Disturb (backed by the
        // com.apple.developer.usernotifications.time-sensitive entitlement). A louder
        // `.critical` level would need Apple's Critical Alerts entitlement.
        content.interruptionLevel = .timeSensitive
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }
}
