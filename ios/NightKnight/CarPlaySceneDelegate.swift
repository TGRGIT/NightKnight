import CarPlay
import UIKit
import os

/// CarPlay "Driving Task" scene: a single glanceable screen showing the latest glucose
/// value, level status, trend, and reading age — no charts, no scrolling, no interaction
/// needed while driving. It just displays and refreshes on the CGM cadence.
///
/// Declared in `Info.plist` under `UIApplicationSceneManifest` for the role
/// `CPTemplateApplicationSceneSessionRoleApplication`; the SwiftUI `WindowGroup` keeps
/// managing the phone window scene (we deliberately leave the window role unspecified).
/// Reuses the existing `APIClient` / `Settings` / `ReadingCache` data layer — the row
/// formatting lives in the pure, unit-tested `CarPlayGlance`.
///
/// The whole class is `@MainActor`: CarPlay calls the scene-delegate methods on the main
/// thread and every CarPlay template object must be created and mutated there, so isolating
/// the type keeps all of it on the main actor and lets the refresh `Task` inherit it.
@MainActor
final class CarPlaySceneDelegate: UIResponder, CPTemplateApplicationSceneDelegate {
    private static let log = Logger(subsystem: "be.cooney.nightknight", category: "carplay")

    private var interfaceController: CPInterfaceController?
    private var template: CPInformationTemplate?
    private var refreshTask: Task<Void, Never>?

    func templateApplicationScene(_ scene: CPTemplateApplicationScene,
                                  didConnect interfaceController: CPInterfaceController) {
        Self.log.notice("scene didConnect")
        self.interfaceController = interfaceController

        // Build the template and paint it from the warm cache *synchronously* before
        // returning, so the head unit shows content immediately and the connect handler
        // never waits on the network (which would trip CarPlay's watchdog).
        let template = CPInformationTemplate(title: "Glucose", layout: .leading, items: [], actions: [])
        self.template = template
        let settings = Settings.current()
        apply(reading: settings.isConfigured ? ReadingCache.load() : nil, unit: settings.preferredUnit)
        interfaceController.setRootTemplate(template, animated: false) { ok, error in
            Self.log.notice("setRootTemplate ok=\(ok, privacy: .public) error=\(String(describing: error), privacy: .public)")
        }
        startRefreshing()
    }

    func templateApplicationScene(_ scene: CPTemplateApplicationScene,
                                  didDisconnectInterfaceController interfaceController: CPInterfaceController) {
        Self.log.notice("scene didDisconnect")
        refreshTask?.cancel()
        refreshTask = nil
        self.interfaceController = nil
        self.template = nil
    }

    /// Poll for the latest reading and repaint the template on the CGM cadence, only while
    /// the CarPlay scene is connected.
    private func startRefreshing() {
        refreshTask?.cancel()
        refreshTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshOnce()
                // ~1 min: comfortably inside the ~5-min CGM cadence, cheap on the server,
                // and keeps the "updated N min ago" line honest.
                try? await Task.sleep(for: .seconds(60))
            }
        }
    }

    /// One fetch-and-repaint. A fresh fetch wins; a transient failure falls back to the
    /// cache the app and widget keep warm, so the screen never blanks mid-drive. A removed
    /// account (not configured) drops to the guidance row rather than stale glucose.
    private func refreshOnce() async {
        // A fresh settings snapshot each fetch (mirroring the widget) so a token the user
        // changed or cleared in the app is picked up.
        let settings = Settings.current()
        var reading: CurrentReading?
        if settings.isConfigured {
            let fetched = try? await APIClient(settings: settings).current()
            if let fetched { ReadingCache.save(fetched) }
            reading = fetched ?? ReadingCache.load()
        }
        apply(reading: reading, unit: settings.preferredUnit)
        // Per-minute, so `.debug` keeps it out of the default log but available when needed.
        Self.log.debug("refresh -> \(reading.map { "\(Int($0.value.mgdl.rounded())) mg/dL" } ?? "no data", privacy: .public)")
    }

    private func apply(reading: CurrentReading?, unit: GlucoseUnit) {
        template?.items = CarPlayGlance.items(for: reading, unit: unit)
            .map { CPInformationItem(title: $0.title, detail: $0.detail) }
    }
}
