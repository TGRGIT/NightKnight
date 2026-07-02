import SwiftUI

/// The first-run chooser, shown while `Settings.dataSource == nil`: four source cards
/// (each with a "?" pros/cons sheet), then that source's credential step with a
/// "Test & finish" gate. Only a SUCCESSFUL test persists the choice — so the app can
/// never land on the dashboard with a source that doesn't work.
struct WelcomeView: View {
    private let settings = Settings.shared
    @State private var staged = SourceSetup.Staged(from: .shared)
    @State private var infoFor: DataSourceInfo?
    @State private var isTesting = false
    @State private var status: String?
    @State private var statusOK = false
    /// The in-flight "Test & finish" task. Plain `Task { }` (not `.task {}`) so it
    /// survives view updates within the credential step — but that also means it is
    /// NOT tied to the view's lifecycle, so it must be cancelled explicitly when the
    /// user navigates away; otherwise a test that succeeds after the user backed out
    /// would silently activate (and wipe local storage for) a source they abandoned.
    @State private var testTask: Task<Void, Never>?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text("Where should your glucose data come from?")
                        .font(.title2.bold())
                        .padding(.top, 24)
                    Text("Pick a data source to get started. You can change it later in Settings.")
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
            .navigationDestination(for: DataSource.self) { source in
                credentialStep(source)
            }
            .sheet(item: $infoFor) { SourceInfoSheet(info: $0) }
        }
        .preferredColorScheme(.dark)
    }

    private func card(_ info: DataSourceInfo) -> some View {
        NavigationLink(value: info.source) {
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

    private func credentialStep(_ source: DataSource) -> some View {
        let info = DataSourceInfo.info(for: source)
        return Form {
            Section(footer: Text(info.tagline)) {
                SourceCredentialFields(source: source, staged: $staged)
            }
            if source.usesLocalAnalytics {
                Section {
                    Label("Without a server to push, background alarms are best-effort — iOS decides when the app may refresh. For reliable alarms use NightKnight (or a push-capable Nightscout setup).",
                          systemImage: "info.circle")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            Section {
                Button(isTesting ? "Testing…" : "Test & finish") {
                    testTask?.cancel()
                    testTask = Task { await testAndFinish(source) }
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
            // Cancel rather than let a delayed success silently activate a source the
            // user just navigated away from — this view no longer has anyone to show
            // "Connected ✓" to, so there is no legitimate reason to keep it running.
            testTask?.cancel()
            testTask = nil
        }
    }

    /// Validate the staged credentials against the live source, and only then commit
    /// the choice. Setting `dataSource` is what dismisses this whole view (the root
    /// gates on it), so the last step "just works".
    private func testAndFinish(_ source: DataSource) async {
        isTesting = true
        status = nil
        defer { isTesting = false }
        let result = await SourceSetup.test(source, staged: staged)
        // The user may have navigated away (and cancelled this task) while the vendor
        // round-trip was in flight — don't touch @State on a view no longer on screen,
        // and never activate a source the user has already abandoned.
        guard !Task.isCancelled else { return }
        statusOK = result.ok
        status = result.message
        guard result.ok else { return }
        guard !Task.isCancelled else { return }
        // Always wipe here, even on a genuine first run (where it's a cheap no-op):
        // this screen only shows when `dataSource == nil`, but that doesn't guarantee
        // the local store is unclaimed — e.g. `SettingsView.disconnect()` clears
        // credentials and routes back here, and (defensively) any other path that
        // reset dataSource without touching LocalStore. No confirmation dialog makes
        // sense on a first-run screen anyway — there's nothing visible to lose.
        await SourceSetup.activate(source, staged: staged, settings: settings, wipe: true)
        if source == .nightscout {
            // Pull the instance's full history in the background; the dashboard is
            // usable immediately from the recent window.
            Task.detached { _ = await SourceSetup.initialBackfill(settings: .shared) }
        }
        if settings.isConfigured && !settings.usesLocalAnalytics {
            // Server mode gets silent-push refresh; register now that we're configured.
            UIApplication.shared.registerForRemoteNotifications()
        }
    }
}
