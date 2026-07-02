import SwiftUI
import WidgetKit

/// The staged-edit → test → activate/switch flow shared by the first-run chooser
/// (`WelcomeView`) and `SettingsView`. Fields are edited as a `Staged` copy — never
/// written to `Settings` keystroke-by-keystroke — so changing the source or the
/// account is an explicit commit that can be gated behind the destructive
/// "delete cached data & resync" confirmation.
@MainActor
enum SourceSetup {
    /// Editable copies of every per-source credential field, seeded from `Settings`.
    struct Staged {
        var baseURL: String
        var token: String
        var cfId: String
        var cfSecret: String
        var dexcomRegion: String
        var dexcomUsername: String
        var dexcomPassword: String
        var libreEmail: String
        var librePassword: String
        var nightscoutURL: String
        var nightscoutSecret: String

        init(from s: Settings) {
            baseURL = s.baseURL
            token = s.deviceToken
            cfId = s.cfAccessClientId
            cfSecret = s.cfAccessClientSecret
            dexcomRegion = s.dexcomRegion
            dexcomUsername = s.dexcomUsername
            dexcomPassword = s.dexcomPassword
            libreEmail = s.libreEmail
            librePassword = s.librePassword
            nightscoutURL = s.nightscoutURL
            nightscoutSecret = s.nightscoutSecret
        }

        /// Whether the chosen source has everything it needs (mirrors
        /// `Settings.isConfigured` for that source).
        func isComplete(for source: DataSource) -> Bool {
            switch source {
            case .nightknight:
                return !trimmed(baseURL).isEmpty && !trimmed(token).isEmpty
            case .dexcom:
                return !trimmed(dexcomUsername).isEmpty && !dexcomPassword.isEmpty
            case .libre:
                return !trimmed(libreEmail).isEmpty && !librePassword.isEmpty
            case .nightscout:
                return !trimmed(nightscoutSecret).isEmpty && NightscoutClient.isSafeBase(nightscoutURL)
            }
        }

        /// The PROSPECTIVE source-key these staged values would produce — must mirror
        /// `Settings.sourceKey` exactly, so "does this commit change the account?" is
        /// answered before anything persists.
        func sourceKey(for source: DataSource) -> String {
            switch source {
            case .nightknight:
                return "nightknight:\(trimmed(baseURL))"
            case .dexcom:
                return "dexcom:\(trimmed(dexcomRegion).lowercased()):\(trimmed(dexcomUsername).lowercased())"
            case .libre:
                return "libre:\(trimmed(libreEmail).lowercased())"
            case .nightscout:
                return "nightscout:\(NightscoutClient.normalizeBase(nightscoutURL).lowercased())"
            }
        }

        /// A throwaway client over the staged values for "Test connection" — nothing
        /// persisted, no global state touched. Nil for `.nightknight` (tested via a
        /// direct probe request instead).
        func makeStandalone(for source: DataSource) -> (any StandaloneSource)? {
            switch source {
            case .nightknight:
                return nil
            case .dexcom:
                return DexcomShareClient(region: DexcomShareClient.Region.parse(dexcomRegion),
                                         username: trimmed(dexcomUsername),
                                         password: dexcomPassword)
            case .libre:
                return LibreLinkUpClient(email: trimmed(libreEmail), password: librePassword)
            case .nightscout:
                return NightscoutClient(baseURL: trimmed(nightscoutURL),
                                        secret: trimmed(nightscoutSecret))
            }
        }

        /// Persist the staged fields (all of them — unedited ones are round-trips).
        func apply(to s: Settings) {
            s.baseURL = trimmed(baseURL)
            s.deviceToken = trimmed(token)
            s.cfAccessClientId = trimmed(cfId)
            s.cfAccessClientSecret = trimmed(cfSecret)
            s.dexcomRegion = trimmed(dexcomRegion)
            s.dexcomUsername = trimmed(dexcomUsername)
            s.dexcomPassword = dexcomPassword
            s.libreEmail = trimmed(libreEmail)
            s.librePassword = librePassword
            s.nightscoutURL = trimmed(nightscoutURL)
            s.nightscoutSecret = trimmed(nightscoutSecret)
        }

        private func trimmed(_ s: String) -> String {
            s.trimmingCharacters(in: .whitespaces)
        }
    }

    /// Commit the staged values as the active source. With `wipe`, the local data is
    /// reset to the new owner first (the confirmed switch path): cached readings from
    /// one source/account must never mix with another's.
    static func activate(_ source: DataSource, staged: Staged, settings: Settings, wipe: Bool) async {
        let oldSource = settings.dataSource
        // Unregister from the OLD NightKnight server BEFORE anything below can clear
        // the credentials that authenticate the unregister call — otherwise the
        // abandoned server never learns this device left and keeps sending silent
        // pushes to it indefinitely. Independent of `wipe`: this is server hygiene,
        // not local-data safety, so it must run on every switch away from
        // `.nightknight`, wipe or no wipe.
        if oldSource == .nightknight, oldSource != source, !settings.apnsToken.isEmpty {
            let oldClient = APIClient(settings: Settings.current())
            try? await oldClient.unregisterPush(token: settings.apnsToken)
        }
        staged.apply(to: settings)
        settings.dataSource = source
        if wipe {
            try? await LocalStore.shared.reset(to: settings.sourceKey)
            ReadingCache.clear()
            await AnalyticsMemo.shared.clear()
            // Nothing of the old account survives the switch: stored credentials and
            // any cached vendor session (Dexcom session id / Libre bearer token).
            if let old = oldSource, old != source {
                settings.clearSourceCredentials(old)
            }
        }
        PhoneSyncManager.shared.pushConfig()
        WidgetCenter.shared.reloadAllTimelines()
    }

    /// Nightscout first-connect: walk the instance's full history into the local
    /// store (the other sources only accumulate forward). Returns the reading count,
    /// or nil when the source doesn't backfill / the walk failed.
    static func initialBackfill(settings: Settings) async -> Int? {
        guard settings.dataSource == .nightscout,
              let source = StandaloneSources.make(settings) else { return nil }
        guard let samples = try? await source.backfill(), !samples.isEmpty else { return nil }
        try? await LocalStore.shared.upsert(samples, sourceKey: settings.sourceKey)
        return samples.count
    }

    /// The number of days of history the app renders — the cap the CSV importer
    /// targets, since anything older is never shown and just bloats the local store.
    static let renderedHistoryDays = 90

    /// Parse a Dexcom Clarity / LibreView CSV export into `(date_ms, mgdl)` rows via
    /// the Rust importer (format auto-detected), keeping only the last
    /// `renderedHistoryDays`. Reads and parses ONLY — the caller decides when to write
    /// (onboarding applies them after activating the source; Settings applies them to
    /// the already-active source). Returns nil when the on-device engine is
    /// unavailable (extensions never call this).
    static func parseHistoryCSV(_ url: URL, now: Date = Date()) throws
        -> (rows: [(dateMs: Int64, mgdl: Double)], source: String) {
        guard let engine = LocalAnalytics.engine else {
            throw ImportError.engineUnavailable
        }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let text = try String(contentsOf: url, encoding: .utf8)
        let data = try engine.importGlucoseCSV(text: text, tzOffsetMin: APIClient.tzOffsetMinutes)
        guard let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let entries = obj["entries"] as? [[String: Any]] else {
            throw ImportError.badOutput
        }
        let cutoff = Int64((now.timeIntervalSince1970 * 1000).rounded())
            - Int64(renderedHistoryDays) * 86_400_000
        let rows: [(dateMs: Int64, mgdl: Double)] = entries.compactMap {
            guard let date = $0["date"] as? Double, let mgdl = $0["mgdl"] as? Double else { return nil }
            let ms = Int64(date)
            guard ms >= cutoff else { return nil }
            return (ms, mgdl)
        }
        return (rows, (obj["source"] as? String) ?? "csv")
    }

    enum ImportError: Error, LocalizedError {
        case engineUnavailable
        case badOutput
        var errorDescription: String? {
            switch self {
            case .engineUnavailable: return "On-device import isn't available."
            case .badOutput: return "Couldn't read that file as a Dexcom or LibreView export."
            }
        }
    }

    /// Source-aware "Test connection" against the STAGED values — persists nothing.
    static func test(_ source: DataSource, staged: Staged) async -> (ok: Bool, message: String) {
        do {
            let current: CurrentReading?
            if let client = staged.makeStandalone(for: source) {
                current = try await client.current()
            } else {
                current = try await probeNightKnight(staged)
            }
            if let current {
                let mins = Int(Date().timeIntervalSince(current.date) / 60)
                return (true, "Connected ✓ — last reading \(mins) min ago")
            }
            return (true, "Connected ✓ — no readings yet")
        } catch {
            let message = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
            return (false, message)
        }
    }

    /// A direct `/api/v4/current` probe with the staged URL/token — bypasses
    /// `APIClient` so testing staged server credentials can't route through (or
    /// mutate) whatever source is currently active.
    private static func probeNightKnight(_ staged: Staged) async throws -> CurrentReading? {
        guard var comps = URLComponents(string: staged.baseURL.trimmingCharacters(in: .whitespaces)),
              !staged.baseURL.trimmingCharacters(in: .whitespaces).isEmpty else {
            throw APIError.badURL
        }
        comps.path = "/api/v4/current"
        guard let url = comps.url else { throw APIError.badURL }
        var req = URLRequest(url: url)
        req.setValue(staged.token.trimmingCharacters(in: .whitespaces), forHTTPHeaderField: "api-secret")
        if !staged.cfId.isEmpty {
            req.setValue(staged.cfId, forHTTPHeaderField: "CF-Access-Client-Id")
            req.setValue(staged.cfSecret, forHTTPHeaderField: "CF-Access-Client-Secret")
        }
        req.timeoutInterval = 20
        let (data, resp) = try await URLSession.shared.data(for: req)
        guard let http = resp as? HTTPURLResponse else { throw APIError.decode }
        guard (200..<300).contains(http.statusCode) else { throw APIError.http(http.statusCode) }
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw APIError.decode
        }
        guard let current = obj["current"] as? [String: Any],
              let ms = current["date"] as? Double,
              let mgdl = current["mgdl"] as? Double else { return nil }
        let trend = TrendDirection(name: current["direction"] as? String)
        return CurrentReading(date: Date(timeIntervalSince1970: ms / 1000),
                              value: GlucoseValue(mgdl: mgdl),
                              trend: trend,
                              trendLabel: (current["trendLabel"] as? String) ?? trend.label)
    }
}

/// The per-source credential fields, shared by the onboarding credential step and
/// SettingsView so the two never drift. Edits stay in `Staged` until committed.
struct SourceCredentialFields: View {
    let source: DataSource
    @Binding var staged: SourceSetup.Staged

    var body: some View {
        switch source {
        case .nightknight:
            TextField("Server URL", text: $staged.baseURL)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .keyboardType(.URL)
            SecureField("Device token (api-secret)", text: $staged.token)
            // Cloudflare Access is always offered for NightKnight (optional) — a
            // server deployed behind the Access gate needs these to connect, and
            // hiding them behind a toggle would strand those users. Leave blank if
            // the server isn't gated.
            TextField("CF-Access-Client-Id (optional)", text: $staged.cfId)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
            SecureField("CF-Access-Client-Secret (optional)", text: $staged.cfSecret)
        case .dexcom:
            Picker("Region", selection: $staged.dexcomRegion) {
                Text("United States").tag("us")
                Text("Outside US").tag("ous")
                Text("Japan").tag("jp")
            }
            TextField("Dexcom username", text: $staged.dexcomUsername)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
            SecureField("Dexcom password", text: $staged.dexcomPassword)
        case .libre:
            TextField("LibreLinkUp email", text: $staged.libreEmail)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .keyboardType(.emailAddress)
            SecureField("LibreLinkUp password", text: $staged.librePassword)
        case .nightscout:
            TextField("Instance URL", text: $staged.nightscoutURL)
                .textInputAutocapitalization(.never).autocorrectionDisabled()
                .keyboardType(.URL)
            SecureField("API secret (SHA-1 hash or access token)", text: $staged.nightscoutSecret)
            // Inline SSRF/https validation so a bad URL is flagged before any request
            // would carry the secret.
            if !staged.nightscoutURL.trimmingCharacters(in: .whitespaces).isEmpty,
               !NightscoutClient.isSafeBase(staged.nightscoutURL) {
                Label("URL must be https to a public host.", systemImage: "exclamationmark.triangle.fill")
                    .font(.footnote).foregroundStyle(.orange)
            }
        }
    }
}

/// The "?" pros/cons content for one source — shown as a sheet from the chooser
/// cards and the Settings source rows.
struct SourceInfoSheet: View {
    let info: DataSourceInfo

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Text(info.tagline)
                }
                Section("Pros") {
                    ForEach(info.pros, id: \.self) { pro in
                        Label(pro, systemImage: "checkmark.circle.fill")
                            .foregroundStyle(.primary)
                    }
                }
                Section("Cons") {
                    ForEach(info.cons, id: \.self) { con in
                        Label(con, systemImage: "minus.circle")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle(info.title)
            .navigationBarTitleDisplayMode(.inline)
        }
        .presentationDetents([.medium, .large])
    }
}
