import SwiftUI

@main
struct NightKnightApp: App {
    init() {
        // Start WatchConnectivity so we can push config to the Apple Watch.
        PhoneSyncManager.shared.start()
    }
    var body: some Scene {
        WindowGroup {
            DashboardView()
                .preferredColorScheme(.dark)
            // Notification permission is requested only when the user enables alarms
            // (in Settings) — alarms are off by default, so we don't prompt on launch.
        }
    }
}
