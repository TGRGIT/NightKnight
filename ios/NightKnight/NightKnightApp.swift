import SwiftUI
import WidgetKit

@main
struct NightKnightApp: App {
    @Environment(\.scenePhase) private var scenePhase

    init() {
        // Start WatchConnectivity so we can push config to the Apple Watch.
        PhoneSyncManager.shared.start()
    }
    var body: some Scene {
        WindowGroup {
            DashboardView()
                .preferredColorScheme(.dark)
                // Reopening the app refreshes the widget: it re-runs its timeline (fetching
                // fresh, or falling back to the cache the app just wrote), so the widget
                // recovers instead of staying stuck on a stale/blank entry from a throttled
                // background refresh.
                .onChange(of: scenePhase) { _, phase in
                    if phase == .active { WidgetCenter.shared.reloadAllTimelines() }
                }
            // Notification permission is requested only when the user enables alarms
            // (in Settings) — alarms are off by default, so we don't prompt on launch.
        }
    }
}
