import SwiftUI
import UniformTypeIdentifiers

/// A step in the first-run flow after the safety notice.
private enum OnboardingRoute: Hashable {
    case credentials(DataSource)
    case importHistory(DataSource)
}

/// DEBUG-only deep-link into a specific onboarding screen for simulator screenshots
/// (a screen behind a live vendor connection, like the import step, is otherwise
/// unreachable without real credentials). Consistent with the existing `NK_*` sim
/// hooks. Never compiled into release.
private enum OnboardingDebug {
    #if DEBUG
    private static let route = ProcessInfo.processInfo.environment["NK_ONBOARD"]
    static var skipDisclaimer: Bool { route != nil }
    static var initialPath: [OnboardingRoute] {
        switch route {
        case "creds-nightknight": return [.credentials(.nightknight)]
        case "creds-dexcom": return [.credentials(.dexcom)]
        case "import-dexcom": return [.importHistory(.dexcom)]
        case "import-libre": return [.importHistory(.libre)]
        default: return []
        }
    }
    #else
    static var skipDisclaimer: Bool { false }
    static var initialPath: [OnboardingRoute] { [] }
    #endif
}

/// The first-run experience, shown while `Settings.dataSource == nil`:
/// 1. a safety notice the user must accept (not a medical device; alarms are only
///    reliable while the app is open),
/// 2. the data-source chooser (four cards, each with a "?" pros/cons sheet),
/// 3. that source's credential step with a "Connect" gate — only a SUCCESSFUL test
///    proceeds, so the app never lands on the dashboard with a source that doesn't work,
/// 4. for Dexcom / LibreLinkUp, an optional CSV history import (their follower feeds
///    only carry recent readings), then finish.
struct WelcomeView: View {
    private let settings = Settings.shared
    @State private var path: [OnboardingRoute] = OnboardingDebug.initialPath
    @State private var staged = SourceSetup.Staged(from: .shared)
    @State private var infoFor: DataSourceInfo?
    @State private var isTesting = false
    @State private var status: String?
    @State private var statusOK = false
    /// The in-flight "Connect" task. Plain `Task { }` (not `.task {}`) so it survives
    /// view updates within the credential step — but that also means it is NOT tied to
    /// the view's lifecycle, so it must be cancelled explicitly when the user navigates
    /// away; otherwise a test that succeeds after the user backed out would silently
    /// advance a source they abandoned.
    @State private var testTask: Task<Void, Never>?
    // Onboarding CSV import (Dexcom / Libre history step).
    @State private var showImporter = false
    @State private var importedRows: [(dateMs: Int64, mgdl: Double)] = []
    @State private var importStatus: String?
    @State private var importOK = false
    @State private var isFinishing = false

    var body: some View {
        NavigationStack(path: $path) {
            Group {
                if settings.hasAcceptedDisclaimer || OnboardingDebug.skipDisclaimer {
                    chooser
                } else {
                    DisclaimerStep { settings.hasAcceptedDisclaimer = true }
                }
            }
            .navigationDestination(for: OnboardingRoute.self) { route in
                switch route {
                case .credentials(let source): credentialStep(source)
                case .importHistory(let source): importStep(source)
                }
            }
            .sheet(item: $infoFor) { SourceInfoSheet(info: $0) }
        }
        .preferredColorScheme(.dark)
    }

    // MARK: - Chooser

    private var chooser: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("Where should your glucose data come from?")
                    .font(.title2.bold())
                    .padding(.top, 24)
                Text("Pick how NightKnight gets your readings. Tap the \(Image(systemName: "questionmark.circle")) on any option for the details, and you can change this later in Settings.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                ForEach(DataSourceInfo.all) { info in
                    card(info)
                }
            }
            .padding(.horizontal, 20)
        }
        .navigationTitle("Welcome")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func card(_ info: DataSourceInfo) -> some View {
        NavigationLink(value: OnboardingRoute.credentials(info.source)) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(info.title).font(.headline)
                    Text(info.tagline).font(.footnote).foregroundStyle(.secondary)
                        .multilineTextAlignment(.leading)
                }
                Spacer()
                // The "?" opens the pros/cons sheet WITHOUT selecting the card.
                Button {
                    infoFor = info
                } label: {
                    Image(systemName: "questionmark.circle")
                        .font(.title3)
                        .foregroundStyle(Color.nkAccent)
                }
                .buttonStyle(.plain)
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 14).fill(Color(.secondarySystemGroupedBackground)))
        }
        .buttonStyle(.plain)
    }

    // MARK: - Credentials

    private func credentialStep(_ source: DataSource) -> some View {
        let info = DataSourceInfo.info(for: source)
        return Form {
            Section(footer: Text(info.tagline)) {
                SourceCredentialFields(source: source, staged: $staged)
            }
            Section {
                Button(isTesting ? "Connecting…" : "Connect") {
                    testTask?.cancel()
                    testTask = Task { await testAndProceed(source) }
                }
                .disabled(isTesting || !staged.isComplete(for: source))
                if let status {
                    Text(status).font(.caption)
                        .foregroundStyle(statusOK ? Color.green : Color.nkAccent)
                }
            }
        }
        .navigationTitle(info.title)
        .navigationBarTitleDisplayMode(.inline)
        .onDisappear {
            status = nil
            // Cancel rather than let a delayed success advance a source the user just
            // navigated away from — this view no longer has anyone to show "Connected"
            // to, so there is no legitimate reason to keep it running.
            testTask?.cancel()
            testTask = nil
        }
    }

    /// Validate the staged credentials against the live source. Dexcom / Libre then
    /// go to the optional history-import step; NightKnight / Nightscout finish here.
    private func testAndProceed(_ source: DataSource) async {
        isTesting = true
        status = nil
        defer { isTesting = false }
        let result = await SourceSetup.test(source, staged: staged)
        // The user may have navigated away (and cancelled this task) while the vendor
        // round-trip was in flight — don't touch @State on a view no longer on screen.
        guard !Task.isCancelled else { return }
        statusOK = result.ok
        status = result.message
        guard result.ok, !Task.isCancelled else { return }
        if source == .dexcom || source == .libre {
            // Their follower feeds only carry recent readings — offer a CSV backfill
            // before finishing. Nothing is activated yet.
            path.append(.importHistory(source))
        } else {
            await finish(source)
        }
    }

    // MARK: - Import history (Dexcom / Libre)

    private func importStep(_ source: DataSource) -> some View {
        let vendor = source == .dexcom ? "Dexcom Clarity" : "LibreView"
        return Form {
            Section {
                Label("\(source.label) only shares recent readings, so your stats start out thin and fill in over time.",
                      systemImage: "clock.arrow.circlepath")
                    .font(.footnote).foregroundStyle(.secondary)
            }
            Section(header: Text("Import history (optional)"),
                    footer: Text("Export a CSV from \(vendor) and import it here to backfill up to \(SourceSetup.renderedHistoryDays) days at once — the most history NightKnight shows. You can also do this later in Settings.")) {
                Button(importedRows.isEmpty ? "Choose CSV file…" : "Choose a different file…") {
                    showImporter = true
                }
                if let importStatus {
                    Text(importStatus).font(.caption)
                        .foregroundStyle(importOK ? Color.green : Color.nkAccent)
                }
            }
            Section {
                Button(isFinishing ? "Finishing…" : (importedRows.isEmpty ? "Skip and finish" : "Finish setup")) {
                    Task { await finish(source, extraRows: importedRows) }
                }
                .disabled(isFinishing)
            }
        }
        .navigationTitle("Add history")
        .navigationBarTitleDisplayMode(.inline)
        .fileImporter(isPresented: $showImporter,
                      allowedContentTypes: [.commaSeparatedText, .plainText]) { result in
            if case .success(let url) = result { importCSV(url) }
        }
    }

    private func importCSV(_ url: URL) {
        importStatus = "Reading…"
        importOK = false
        do {
            let parsed = try SourceSetup.parseHistoryCSV(url)
            importedRows = parsed.rows
            importOK = !parsed.rows.isEmpty
            importStatus = parsed.rows.isEmpty
                ? "No readings in the last \(SourceSetup.renderedHistoryDays) days were found in that file."
                : "Ready to import \(parsed.rows.count) readings (\(parsed.source) export)."
        } catch {
            importedRows = []
            importStatus = "Import failed: \((error as? LocalizedError)?.errorDescription ?? error.localizedDescription)"
        }
    }

    // MARK: - Finish

    /// Activate the chosen source and land on the dashboard. `extraRows` (from the CSV
    /// step) are written AFTER activation stamps the local-store owner, then trimmed to
    /// the rendered window. Setting `dataSource` is what dismisses this whole view (the
    /// root gates on it); this Task is intentionally un-cancelled so the writes below
    /// still complete as the view tears down.
    private func finish(_ source: DataSource, extraRows: [(dateMs: Int64, mgdl: Double)] = []) async {
        isFinishing = true
        // Always wipe on activate: this screen only shows when `dataSource == nil`, but
        // that doesn't guarantee the local store is unclaimed (e.g. re-onboarding after
        // Disconnect), and there is nothing visible to lose on a first-run screen.
        await SourceSetup.activate(source, staged: staged, settings: settings, wipe: true)
        if !extraRows.isEmpty {
            try? await LocalStore.shared.upsertRows(extraRows, sourceKey: settings.sourceKey)
            try? await LocalStore.shared.prune(olderThanDays: SourceSetup.renderedHistoryDays,
                                               sourceKey: settings.sourceKey)
            await AnalyticsMemo.shared.clear()
        }
        if source == .nightscout {
            // Pull the instance's full history in the background; the dashboard is
            // usable immediately from the recent window.
            Task.detached { _ = await SourceSetup.initialBackfill(settings: .shared) }
        }
        if settings.isConfigured && !settings.usesLocalAnalytics {
            UIApplication.shared.registerForRemoteNotifications()
        }
    }
}

/// The mandatory first-launch safety notice. Plain language, one clear action.
private struct DisclaimerStep: View {
    let onAgree: () -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Image(systemName: "exclamationmark.shield")
                    .font(.system(size: 44))
                    .foregroundStyle(Color.nkAccent)
                    .padding(.top, 24)
                Text("Before you start")
                    .font(.title.bold())

                point("NightKnight is not a medical device.",
                      "Don't use it to make treatment or dosing decisions. Always confirm with your CGM app, a fingerstick meter, and your healthcare team.")
                point("Alarms are most reliable while the app is open.",
                      "Low and high alerts run on this device. When NightKnight is in the background, iOS limits how often it can check for new readings, so alarms may be delayed or may not fire at all.")
                point("Never rely on NightKnight alone.",
                      "Keep using your CGM's own alarms and alerts for anything urgent.")

                Spacer(minLength: 8)
            }
            .padding(.horizontal, 24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .safeAreaInset(edge: .bottom) {
            Button(action: onAgree) {
                Text("I Understand & Agree")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 14)
                    .background(Color.nkAccent, in: RoundedRectangle(cornerRadius: 14))
                    .foregroundStyle(.white)
            }
            .padding(.horizontal, 24)
            .padding(.bottom, 12)
            .background(.ultraThinMaterial)
        }
        .navigationTitle("Welcome")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func point(_ title: String, _ body: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.headline)
            Text(body).font(.subheadline).foregroundStyle(.secondary)
        }
    }
}
