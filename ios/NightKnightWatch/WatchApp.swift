import SwiftUI
import WidgetKit

@main
struct NightKnightWatchApp: App {
    @Environment(\.scenePhase) private var scenePhase

    init() {
        #if DEBUG
        Demo.applyToSettings()
        #endif
        // Receive server URL + token from the paired iPhone over WatchConnectivity.
        WatchSyncManager.shared.start()
    }
    var body: some Scene {
        WindowGroup {
            WatchDashboardView()
                // Reopening the watch app refreshes the complication (same rationale as iOS).
                .onChange(of: scenePhase) { _, phase in
                    if phase == .active { WidgetCenter.shared.reloadAllTimelines() }
                }
        }
    }
}
