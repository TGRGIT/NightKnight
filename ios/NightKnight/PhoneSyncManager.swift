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
        try? WCSession.default.updateApplicationContext(context(reading: ReadingCache.load()))
    }

    /// Push a fresh reading (piggybacked on the config context). In a local-analytics
    /// source the watch never talks to the vendor itself — this is its ONLY feed: the
    /// watch stores the reading in its own ReadingCache for the dashboard + complication.
    func pushReading(_ reading: CurrentReading) {
        guard WCSession.default.activationState == .activated else { return }
        try? WCSession.default.updateApplicationContext(context(reading: reading))
    }

    private func context(reading: CurrentReading?) -> [String: Any] {
        let s = Settings.shared
        var ctx: [String: Any] = [
            "baseURL": s.baseURL,
            "token": s.deviceToken,
            "unit": s.preferredUnit.rawValue,
            // The watch needs the source *kind* to know it must stay cache-only; the
            // vendor credentials themselves are deliberately never synced.
            "dataSource": s.dataSource?.rawValue ?? "",
        ]
        if let r = reading {
            ctx["reading.mgdl"] = r.value.mgdl
            ctx["reading.trend"] = r.trend.rawValue
            ctx["reading.date"] = r.date.timeIntervalSince1970
        }
        return ctx
    }

    func session(_ session: WCSession, activationDidCompleteWith state: WCSessionActivationState, error: Error?) {
        if state == .activated { pushConfig() }
    }
    // iOS-only delegate methods (required to conform on iOS).
    func sessionDidBecomeInactive(_ session: WCSession) {}
    func sessionDidDeactivate(_ session: WCSession) { session.activate() }
}
