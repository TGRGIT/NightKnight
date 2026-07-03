import Foundation
import WatchConnectivity
import WidgetKit

/// Receives the connection config (server URL, device token, unit) from the paired
/// iPhone via WatchConnectivity and stores it in the watch's `Settings`. watchOS and
/// iOS keychains/containers don't share, so the phone pushes the config here.
final class WatchSyncManager: NSObject, WCSessionDelegate {
    static let shared = WatchSyncManager()

    /// App-Group key holding the fingerprint of the account the watch last synced, so a
    /// same-vendor account switch (which the `dataSource` kind can't distinguish) is detected.
    private static let lastSourceIDKey = "watch.lastSourceID"

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
            // Apply the token even when it's empty: the phone sends "" to sign the watch out
            // when the user disconnects, and dropping it here would strand a deleted token.
            if let token = ctx["token"] as? String { settings.deviceToken = token }
            if let unit = ctx["unit"] as? String, let parsed = GlucoseUnit(rawValue: unit) {
                settings.preferredUnit = parsed
            }
            // The source kind decides whether this watch fetches (.nightknight) or stays
            // cache-only (local sources — no vendor credentials ever reach the watch).
            // Detect a SWITCH before applying it: the "only move forward" guard below
            // compares raw timestamps, which is only meaningful within one account's
            // stream. Across a switch it must not run — otherwise a still-fresh-looking
            // cached reading from the abandoned account can outlive the switch and be
            // mistaken for live data from the new one (glucose from the wrong person).
            var sourceChanged = false
            if let source = ctx["dataSource"] as? String {
                let parsed = DataSource(rawValue: source)
                sourceChanged = parsed != settings.dataSource
                settings.dataSource = parsed
            }
            // A switch between two accounts of the SAME vendor (Dexcom A→B, a different
            // Nightscout instance) keeps `dataSource` identical, so the kind check above
            // can't see it. Compare the phone's opaque per-account fingerprint too. This
            // also survives `updateApplicationContext` coalescing: if the watch was
            // unreachable during the switch, the intermediate sign-out ("") context can be
            // dropped, but the final context's fingerprint still differs from the stored one,
            // so the abandoned account's cached reading is still cleared. Absent (older phone
            // build): fall back to the kind-only check above.
            if let sourceID = ctx["sourceID"] as? String {
                let store = UserDefaults(suiteName: Settings.appGroup)
                if store?.string(forKey: Self.lastSourceIDKey) != sourceID {
                    sourceChanged = true
                    store?.set(sourceID, forKey: Self.lastSourceIDKey)
                }
            }
            if sourceChanged { ReadingCache.clear() }
            // A pushed reading is the watch's data feed in a local-analytics source; store
            // it where the dashboard + complication already look.
            if let mgdl = ctx["reading.mgdl"] as? Double,
               let date = ctx["reading.date"] as? Double {
                let trend = TrendDirection(rawValue: ctx["reading.trend"] as? String ?? "") ?? .none
                let reading = CurrentReading(date: Date(timeIntervalSince1970: date),
                                             value: GlucoseValue(mgdl: mgdl),
                                             trend: trend,
                                             trendLabel: trend.label)
                // Only move forward — a stale context replay must not overwrite a newer
                // reading the watch already has. Skipped right after a source switch,
                // where "newer" from the old account means nothing.
                if !sourceChanged, let cached = ReadingCache.load(), cached.date >= reading.date {
                    // keep the newer cached reading
                } else {
                    ReadingCache.save(reading)
                }
            }
            // The phone can't reach the watch's complication; reload it here so a changed or
            // cleared token takes effect now, not on the complication's next ~5-min timeline.
            WidgetCenter.shared.reloadAllTimelines()
        }
    }
}
