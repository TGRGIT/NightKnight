import Foundation
import WatchConnectivity

/// Pushes the connection config (server URL, device token, unit) to the paired Apple
/// Watch over WatchConnectivity, so the watch app + complications can fetch data.
final class PhoneSyncManager: NSObject, WCSessionDelegate {
    static let shared = PhoneSyncManager()

    func start() {
        guard WCSession.isSupported() else { return }
        WCSession.default.delegate = self
        WCSession.default.activate()
    }

    /// Send the current settings to the watch (latest-value semantics).
    func pushConfig() {
        guard WCSession.default.activationState == .activated else { return }
        let s = Settings.shared
        let ctx: [String: Any] = [
            "baseURL": s.baseURL,
            "token": s.deviceToken,
            "unit": s.preferredUnit.rawValue,
        ]
        try? WCSession.default.updateApplicationContext(ctx)
    }

    func session(_ session: WCSession, activationDidCompleteWith state: WCSessionActivationState, error: Error?) {
        if state == .activated { pushConfig() }
    }
    // iOS-only delegate methods (required to conform on iOS).
    func sessionDidBecomeInactive(_ session: WCSession) {}
    func sessionDidDeactivate(_ session: WCSession) { session.activate() }
}
