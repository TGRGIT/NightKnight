import BackgroundTasks
import SwiftUI
import UIKit
import UserNotifications
import WidgetKit

@main
struct NightKnightApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @Environment(\.scenePhase) private var scenePhase

    init() {
        #if DEBUG
        // Screenshot/preview mode: seed display preferences before any view loads.
        Demo.applyToSettings()
        #endif
        // Start WatchConnectivity so we can push config to the Apple Watch.
        PhoneSyncManager.shared.start()
    }

    var body: some Scene {
        WindowGroup {
            RootTabView()
                .preferredColorScheme(.dark)
                .task {
                    // Make sure a background refresh is always queued, and — when Health
                    // is a data source — wake on new Health glucose to refresh promptly.
                    BackgroundRefresh.schedule()
                    if Settings.shared.readFromHealthKit {
                        HealthKitManager.shared.startBackgroundDelivery {
                            Task { await BackgroundRefresh.refreshNow() }
                        }
                    }
                }
            // Notification permission is requested only when the user enables alarms
            // (in Settings) — alarms are off by default, so we don't prompt on launch.
        }
        // The system runs this when it grants a background slot for our app-refresh task.
        .backgroundTask(.appRefresh(BackgroundRefresh.taskId)) {
            await BackgroundRefresh.run()
        }
        // Re-arm the next background refresh every time we leave the foreground, and
        // reload the widget on foreground so reopening the app recovers a widget that was
        // stuck on a stale/blank entry from a throttled background refresh.
        .onChange(of: scenePhase) { _, phase in
            if phase == .background { BackgroundRefresh.schedule() }
            else if phase == .active { WidgetCenter.shared.reloadAllTimelines() }
        }
    }
}

/// Minimal app delegate whose sole job is to set the notification-center delegate, so
/// glucose alarms are shown while the app is in the foreground. Without a delegate that
/// returns presentation options, iOS silently drops any notification posted while the app
/// is frontmost — which is exactly when the dashboard's poll loop evaluates alarms, so
/// out-of-range alerts never appeared while the user was looking at the app.
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    /// Present alarms (banner + sound + Notification Center entry) even in the foreground.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound, .list]
    }
}

/// Background data refresh for a follower app. The system grants app-refresh slots
/// roughly every 15–30 minutes (it learns your usage); each slot pulls the latest
/// reading, mirrors it to Health, evaluates alarms, and — crucially — reloads the
/// widget timelines, since a widget can't poll on its own. We always reschedule so the
/// chain never breaks. For minute-fresh updates a server-side **silent push** (APNs) is
/// the next step — the `aps-environment` entitlement and `remote-notification` background
/// mode are already in place; it needs the Worker to send a background push when a new
/// reading lands. Full implementation guide: `docs/SILENT-PUSH.md`.
enum BackgroundRefresh {
    /// Must match `BGTaskSchedulerPermittedIdentifiers` in Info.plist.
    static let taskId = "be.cooney.nightknight.refresh"

    /// Queue the next app-refresh (~15 min out; the OS decides the real time).
    static func schedule() {
        let request = BGAppRefreshTaskRequest(identifier: taskId)
        request.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)
        try? BGTaskScheduler.shared.submit(request)
    }

    /// The background-task entry point: reschedule first (so a failure can't break the
    /// chain), then do the refresh.
    @MainActor
    static func run() async {
        schedule()
        await refreshNow()
    }

    /// Pull the latest reading, mirror to Health, evaluate alarms, and reload widgets.
    /// Shared by the app-refresh task and the HealthKit background-delivery wake-up.
    /// Kept short to stay within the background time budget.
    @MainActor
    static func refreshNow() async {
        let settings = Settings.shared
        guard settings.isConfigured else { return }
        let client = APIClient(settings: settings)
        if let current = try? await client.current() {
            AlarmManager.shared.evaluate(current, settings: settings)
        }
        if settings.writeToHealthKit, let readings = try? await client.entries(hours: 6) {
            await HealthKitManager.shared.write(readings)
        }
        WidgetCenter.shared.reloadAllTimelines()
    }
}

/// The three top-level sections, mirroring the web app's tabs.
struct RootTabView: View {
    @State private var selection: Int = {
        #if DEBUG
        return Demo.initialTab
        #else
        return 0
        #endif
    }()

    var body: some View {
        TabView(selection: $selection) {
            DashboardView()
                .tabItem { Label("Dashboard", systemImage: "waveform.path.ecg") }
                .tag(0)
            AnalysisView()
                .tabItem { Label("Analysis", systemImage: "chart.bar.xaxis") }
                .tag(1)
            SettingsView()
                .tabItem { Label("Settings", systemImage: "gearshape") }
                .tag(2)
        }
        .tint(Color.nkAccent)
        #if DEBUG
        // Preview recording: walk Dashboard → Analysis → Settings → Dashboard.
        .task {
            guard Demo.autoplay else { return }
            let plan: [(Double, Int)] = [(10, 1), (9, 2), (6, 0)]
            for (dwell, tab) in plan {
                try? await Task.sleep(for: .seconds(dwell))
                withAnimation { selection = tab }
            }
        }
        #endif
    }
}
