import Foundation
import WatchConnectivity

/// Receives the connection config (server URL, device token, unit) from the paired
/// iPhone via WatchConnectivity and stores it in the watch's `Settings`. watchOS and
/// iOS keychains/containers don't share, so the phone pushes the config here.
final class WatchSyncManager: NSObject, WCSessionDelegate {
    static let shared = WatchSyncManager()

    func start() {
        guard WCSession.isSupported() else { return }
        WCSession.default.delegate = self
        WCSession.default.activate()
    }

    func session(_ session: WCSession, activationDidCompleteWith state: WCSessionActivationState, error: Error?) {
        // Apply whatever the phone last pushed.
        apply(session.receivedApplicationContext)
    }

    func session(_ session: WCSession, didReceiveApplicationContext applicationContext: [String: Any]) {
        apply(applicationContext)
    }

    private func apply(_ ctx: [String: Any]) {
        guard !ctx.isEmpty else { return }
        DispatchQueue.main.async {
            let settings = Settings.shared
            if let url = ctx["baseURL"] as? String, !url.isEmpty { settings.baseURL = url }
            if let token = ctx["token"] as? String, !token.isEmpty { settings.deviceToken = token }
            if let unit = ctx["unit"] as? String, let parsed = GlucoseUnit(rawValue: unit) {
                settings.preferredUnit = parsed
            }
        }
    }
}
