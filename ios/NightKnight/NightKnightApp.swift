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
            else if phase == .active {
                WidgetCenter.shared.reloadAllTimelines()
                // Re-register for silent push on foreground too, so a token that rotated
                // (or a connection configured) while we were backgrounded still reaches the
                // server. Cheap — iOS returns the cached token fast — and only POSTs once
                // we're configured (the callback guards on it).
                if Settings.shared.isConfigured {
                    UIApplication.shared.registerForRemoteNotifications()
                }
            }
        }
    }
}

/// The app delegate handles the two UIKit callbacks SwiftUI doesn't surface: foreground
/// notification presentation (so glucose alarms show while the app is frontmost — iOS
/// otherwise drops them, which is exactly when the dashboard's poll loop raises them) and
/// the remote-notification lifecycle for **silent push** background refresh (register the
/// APNs token, and handle the incoming background push by refreshing).
final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        // Silent pushes (content-available, no alert) need NO user-permission prompt — only
        // user-visible alerts do. So register for remote notifications unconditionally on
        // every launch (cheap; iOS returns the cached token fast and re-fires the callback
        // if it rotated), which keeps the app's "don't prompt on launch" rule intact while
        // letting the server wake us for new data.
        application.registerForRemoteNotifications()
        return true
    }

    /// iOS delivered (or rotated) our APNs device token → report it to the server, scoped
    /// to the authenticated user, so silent pushes can be addressed to this device.
    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { await PushRegistration.send(apnsToken: deviceToken.apnsHexToken) }
    }

    /// There is no APNs in the Simulator, and some dev setups lack the entitlement — log,
    /// never crash. (Background refresh still falls back to `BGAppRefreshTask` + Health.)
    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        NSLog("NightKnight: remote notification registration failed: \(error.localizedDescription)")
    }

    /// A silent push landed. We have ~30s of background runtime: pull the latest reading,
    /// mirror it to Health, evaluate alarms, and reload the widget timelines — then tell
    /// iOS we got new data (so it keeps granting us background wake-ups).
    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        Task { @MainActor in
            await BackgroundRefresh.refreshNow()
            completionHandler(.newData)
        }
    }

    /// Present alarms (banner + sound + Notification Center entry) even in the foreground.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound, .list]
    }
}

/// Reports this device's APNs token to the server so it can send silent pushes. The token
/// arrives from `didRegisterForRemoteNotificationsWithDeviceToken`; we only register when
/// the app is actually configured (server URL + device token present).
enum PushRegistration {
    static func send(apnsToken: String) async {
        let settings = Settings.shared
        guard settings.isConfigured else { return }
        // A development build (Xcode / direct install, `aps-environment=development`) mints
        // a *sandbox* APNs token; a TestFlight / App Store build mints a *production* one.
        // The server keys the APNs host off this, so the value must match the build.
        #if DEBUG
        let environment = "sandbox"
        #else
        let environment = "production"
        #endif
        try? await APIClient(settings: settings).registerPush(token: apnsToken, environment: environment)
    }
}

/// Background data refresh for a follower app, driven by three complementary triggers:
/// the **silent push** (APNs) the server sends when a new reading lands (the timely,
/// primary path — see `AppDelegate` and `docs/SILENT-PUSH.md`), the opportunistic
/// `BGAppRefreshTask` slot the system grants every 15–30 min, and a HealthKit
/// background-delivery wake-up when a vendor app writes glucose. Each runs `refreshNow`:
/// pull the latest reading, mirror it to Health, evaluate alarms, and — crucially —
/// reload the widget timelines, since a widget can't poll on its own. The app-refresh
/// task always reschedules so that chain never breaks.
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

    /// Owned here (not inside `DashboardView`) so the launch splash can observe when the
    /// first live reading lands and dismiss itself.
    @State private var model = DashboardModel()

    /// The branded launch splash covers the tabs until the live glucose stat is loaded.
    /// Skipped in demo/screenshot mode so App Store captures stay deterministic.
    @State private var showSplash: Bool = {
        #if DEBUG
        return !Demo.isEnabled
        #else
        return true
        #endif
    }()

    var body: some View {
        ZStack {
            TabView(selection: $selection) {
                DashboardView(model: model)
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

            if showSplash {
                SplashView()
                    .transition(.opacity)
                    .zIndex(1)
            }
        }
        // Dismiss once the live reading loads — or as soon as the dashboard surfaces an
        // error (e.g. not configured yet), so a new or offline user isn't trapped here.
        .onChange(of: model.current?.date) { dismissSplash() }
        .onChange(of: model.errorText) { dismissSplash() }
        // Safety net: never let the splash outlive a slow or stalled first fetch.
        .task {
            try? await Task.sleep(for: .seconds(10))
            hideSplash()
        }
    }

    private func dismissSplash() {
        guard showSplash, model.current != nil || model.errorText != nil else { return }
        hideSplash()
    }

    private func hideSplash() {
        guard showSplash else { return }
        withAnimation(.easeOut(duration: 0.45)) { showSplash = false }
    }
}

/// The branded launch screen: a large logo, a welcome line, and a "Loading data…"
/// indicator shown until the dashboard's first live reading arrives (see `RootTabView`).
struct SplashView: View {
    var body: some View {
        ZStack {
            Color.nkInk.ignoresSafeArea()
            VStack(spacing: 24) {
                Spacer()
                NightKnightLogo(height: 132)
                    .shadow(color: Color.nkAccent.opacity(0.25), radius: 24)
                VStack(spacing: 8) {
                    Text("NightKnight")
                        .font(.system(size: 36, weight: .bold, design: .rounded))
                    Text("Keeping watch over your glucose")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                Spacer()
                HStack(spacing: 10) {
                    ProgressView().tint(Color.nkAccent)
                    Text("Loading data…")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                .padding(.bottom, 56)
            }
            .padding(.horizontal, 32)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("NightKnight. Loading data.")
    }
}
