import SwiftUI

@main
struct NightKnightWatchApp: App {
    init() {
        // Receive server URL + token from the paired iPhone over WatchConnectivity.
        WatchSyncManager.shared.start()
    }
    var body: some Scene {
        WindowGroup {
            WatchDashboardView()
        }
    }
}
