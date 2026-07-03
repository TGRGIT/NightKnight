import Foundation

/// Errors surfaced by the API client.
enum APIError: Error, LocalizedError {
    case notConfigured
    case badURL
    case http(Int)
    case decode

    var errorDescription: String? {
        switch self {
        case .notConfigured: return "Set your server URL and device token in Settings."
        case .badURL: return "Invalid server URL."
        case .http(let code): return "Server returned \(code)."
        case .decode: return "Unexpected response from server."
        }
    }
}

/// The app's single data facade — every view fetches through here, which is why the
/// data source is swappable at exactly this layer (the same place DEBUG demo mode
/// already substitutes synthetic data).
///
/// * **NightKnight (server) mode** — talks to the NightKnight `/api/v4` API.
///   Authentication: `api-secret` (the device token created in the web UI) plus an
///   optional `CF-Access-Client-Id`/`-Secret` service token for the Cloudflare edge.
/// * **Local-analytics sources** (Dexcom Share / LibreLinkUp / Nightscout) — fetches
///   raw readings via the `StandaloneSource` protocol, accumulates them in
///   `LocalStore`, and computes the same wire payloads on-device through the Rust FFI
///   (`LocalAnalytics.engine`), decoded by the SAME DTOs below. Views never know the
///   difference.
struct APIClient {
    let settings: Settings

    private func makeRequest(path: String, query: [URLQueryItem] = []) throws -> URLRequest {
        guard settings.isConfigured else { throw APIError.notConfigured }
        guard var comps = URLComponents(string: settings.baseURL) else { throw APIError.badURL }
        comps.path = path
        if !query.isEmpty { comps.queryItems = query }
        guard let url = comps.url else { throw APIError.badURL }
        var req = URLRequest(url: url)
        req.setValue(settings.deviceToken, forHTTPHeaderField: "api-secret")
        if !settings.cfAccessClientId.isEmpty {
            req.setValue(settings.cfAccessClientId, forHTTPHeaderField: "CF-Access-Client-Id")
            req.setValue(settings.cfAccessClientSecret, forHTTPHeaderField: "CF-Access-Client-Secret")
        }
        req.timeoutInterval = 20
        return req
    }

    private func fetch<T: Decodable>(_ type: T.Type, path: String, query: [URLQueryItem] = []) async throws -> T {
        let req = try makeRequest(path: path, query: query)
        let (data, resp) = try await URLSession.shared.data(for: req)
        guard let http = resp as? HTTPURLResponse else { throw APIError.decode }
        guard (200..<300).contains(http.statusCode) else { throw APIError.http(http.statusCode) }
        guard let decoded = try? JSONDecoder().decode(T.self, from: data) else { throw APIError.decode }
        return decoded
    }

    /// Send a small JSON body and only check the status (no decoded result). Used by the
    /// push-registration endpoints, which return just `{ "ok": true }` / 204.
    private func sendJSON(method: String, path: String, body: [String: String]) async throws {
        var req = try makeRequest(path: path)
        req.httpMethod = method
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (_, resp) = try await URLSession.shared.data(for: req)
        guard let http = resp as? HTTPURLResponse else { throw APIError.decode }
        guard (200..<300).contains(http.statusCode) else { throw APIError.http(http.statusCode) }
    }

    // MARK: - Endpoints

    /// The device's UTC offset in minutes (east of UTC), for localising time-of-day
    /// analytics on the server (AGP, dawn patterns, nocturnal flags).
    static var tzOffsetMinutes: Int { TimeZone.current.secondsFromGMT() / 60 }

    func current() async throws -> CurrentReading? {
        #if DEBUG
        if Demo.isEnabled { return Demo.current() }
        #endif
        if settings.usesLocalAnalytics { return try await localCurrent() }
        let dto = try await fetch(CurrentEnvelope.self, path: "/api/v4/current")
        guard let c = dto.current else { return nil }
        let trend = TrendDirection(name: c.direction)
        return CurrentReading(
            date: Date(timeIntervalSince1970: Double(c.date) / 1000),
            value: GlucoseValue(mgdl: c.mgdl),
            trend: trend,
            trendLabel: c.trendLabel ?? trend.label
        )
    }

    func entries(hours: Int) async throws -> [GlucoseReading] {
        #if DEBUG
        if Demo.isEnabled { return Demo.readings(hours: hours) }
        #endif
        if settings.usesLocalAnalytics {
            return try await LocalStore.shared.entries(hours: hours, sourceKey: settings.sourceKey)
        }
        let dto = try await fetch(EntriesEnvelope.self, path: "/api/v4/entries",
                                  query: [.init(name: "hours", value: String(hours))])
        return dto.entries.map {
            GlucoseReading(date: Date(timeIntervalSince1970: Double($0.date) / 1000),
                           value: GlucoseValue(mgdl: $0.mgdl))
        }
    }

    func analytics(hours: Int) async throws -> GlucoseAnalytics {
        #if DEBUG
        if Demo.isEnabled { return Demo.analytics(hours: hours) }
        #endif
        if settings.usesLocalAnalytics { return try await localAnalytics(hours: hours) }
        let d = try await fetch(AnalyticsDTO.self, path: "/api/v4/analytics", query: [
            .init(name: "hours", value: String(hours)),
            .init(name: "tzOffset", value: String(Self.tzOffsetMinutes)),
        ])
        return Self.mapAnalytics(d)
    }

    /// DTO → app model, shared verbatim by the server fetch and the on-device FFI path
    /// (both decode the same wire JSON).
    private static func mapAnalytics(_ d: AnalyticsDTO) -> GlucoseAnalytics {
        let mapStat: (EpStatDTO) -> EpisodeStat = {
            EpisodeStat(count: $0.count, nocturnal: $0.nocturnal, perDay: $0.perDay, longestMin: $0.longestMin, totalMin: $0.totalMin)
        }
        // Every block below is OPTIONAL so the app degrades gracefully against a server
        // that pre-dates the Statistical-Analysis fields — the dashboard's core metrics
        // (mean/GMI/eA1c/CV/TIR) are always present; the deeper cards just show "--".
        let coverage = d.coverage.map {
            CoverageInfo(percentActive: $0.percentActive, daysCovered: $0.daysCovered, distinctDays: $0.distinctDays, sufficient: $0.sufficient)
        } ?? CoverageInfo(percentActive: nil, daysCovered: nil, distinctDays: nil, sufficient: false)
        let gri = d.gri.map {
            GriInfo(value: $0.value, zone: $0.zone, hypoComponent: $0.hypoComponent, hyperComponent: $0.hyperComponent)
        } ?? GriInfo(value: nil, zone: nil, hypoComponent: nil, hyperComponent: nil)
        let variability = d.variability.map {
            VariabilityInfo(jIndex: $0.jIndex, mage: $0.mage, conga: $0.conga, modd: $0.modd, congaHours: $0.congaHours)
        } ?? VariabilityInfo(jIndex: nil, mage: nil, conga: nil, modd: nil, congaHours: nil)
        let emptyStat = EpisodeStat(count: 0, nocturnal: 0, perDay: 0, longestMin: 0, totalMin: 0)
        let episodes = d.episodes.map { e in
            EpisodesInfo(
                low: mapStat(e.low), veryLow: mapStat(e.veryLow), high: mapStat(e.high), veryHigh: mapStat(e.veryHigh),
                recent: e.recent.map {
                    RecentEpisode(kind: $0.kind, start: Date(timeIntervalSince1970: Double($0.start) / 1000),
                                  durationMin: $0.durationMin, extremeMgdl: $0.extremeMgdl, nocturnal: $0.nocturnal)
                })
        } ?? EpisodesInfo(low: emptyStat, veryLow: emptyStat, high: emptyStat, veryHigh: emptyStat, recent: [])
        return GlucoseAnalytics(
            n: d.n, meanMgdl: d.meanMgdl, sdMgdl: d.sdMgdl, uGmiPercent: d.uGmiPercent,
            gmiPercent: d.gmiPercent,
            estimatedA1cPercent: d.estimatedA1cPercent, cvPercent: d.cvPercent,
            veryLowPct: d.timeInRange.veryLowPct, lowPct: d.timeInRange.lowPct,
            inRangePct: d.timeInRange.inRangePct, highPct: d.timeInRange.highPct,
            veryHighPct: d.timeInRange.veryHighPct,
            coverage: coverage, gri: gri, variability: variability,
            patterns: (d.patterns ?? []).map { PeriodInfo(startHour: $0.startHour, endHour: $0.endHour, n: $0.n, meanMgdl: $0.meanMgdl, inRangePct: $0.inRangePct) },
            episodes: episodes)
    }

    /// Register this device's APNs token so the server can send silent pushes that wake
    /// the app to refresh. Idempotent server-side, so it's safe to call on every launch
    /// (and whenever iOS rotates the token). `environment` is `"sandbox"` for a
    /// development build, `"production"` for TestFlight / App Store.
    /// No-op concept for local sources: there is no server to push, so this throws
    /// rather than sending the token (with an empty api-secret) to the default host.
    func registerPush(token: String, environment: String) async throws {
        guard !settings.usesLocalAnalytics else { throw APIError.notConfigured }
        try await sendJSON(method: "POST", path: "/api/v4/push/register",
                           body: ["token": token, "environment": environment])
    }

    /// Unregister this device's APNs token (sign-out / token change).
    func unregisterPush(token: String) async throws {
        guard !settings.usesLocalAnalytics else { throw APIError.notConfigured }
        try await sendJSON(method: "DELETE", path: "/api/v4/push/register",
                           body: ["token": token])
    }

    func agp(days: Int) async throws -> [AgpBin] {
        #if DEBUG
        if Demo.isEnabled { return Demo.agp(days: days) }
        #endif
        if settings.usesLocalAnalytics { return try await localAgp(days: days) }
        let d = try await fetch(AgpDTO.self, path: "/api/v4/agp", query: [
            .init(name: "days", value: String(days)),
            .init(name: "tzOffset", value: String(Self.tzOffsetMinutes)),
        ])
        return Self.mapAgp(d)
    }

    private static func mapAgp(_ d: AgpDTO) -> [AgpBin] {
        d.bins.map { AgpBin(minuteOfDay: $0.minuteOfDay, n: $0.n, p05: $0.p05, p25: $0.p25, p50: $0.p50, p75: $0.p75, p95: $0.p95) }
    }

    // MARK: - Local-analytics sources (Dexcom Share / LibreLinkUp / Nightscout)

    /// The server's default AGP bin width (minutes) — the app never overrides it.
    private static let agpBinMinutes = 15

    /// One vendor round-trip per poll: fetch the recent window, fold it into the local
    /// history, warm the extensions' `ReadingCache`, and return the newest sample. The
    /// app is the SOLE fetcher — widget/watch/complication only ever read the cache.
    private func localCurrent() async throws -> CurrentReading? {
        guard settings.isConfigured, let source = StandaloneSources.make(settings) else {
            throw APIError.notConfigured
        }
        let key = settings.sourceKey
        let samples = try await source.fetchRecent()
        try await LocalStore.shared.upsert(samples, sourceKey: key)
        // Trailing-90-day stats are the longest window; anything older is dead weight.
        try? await LocalStore.shared.prune(olderThanDays: 90, sourceKey: key)
        guard let newest = samples.max(by: { $0.dateMs < $1.dateMs }) else { return nil }
        let current = newest.asCurrent
        ReadingCache.save(current)
        return current
    }

    private func localAnalytics(hours: Int) async throws -> GlucoseAnalytics {
        let data = try await localReportJSON(kind: .analytics, window: hours)
        guard let d = try? JSONDecoder().decode(AnalyticsDTO.self, from: data) else {
            throw APIError.decode
        }
        return Self.mapAnalytics(d)
    }

    private func localAgp(days: Int) async throws -> [AgpBin] {
        let data = try await localReportJSON(kind: .agp, window: days)
        guard let d = try? JSONDecoder().decode(AgpDTO.self, from: data) else {
            throw APIError.decode
        }
        return Self.mapAgp(d)
    }

    /// Compute (or reuse) one FFI report. Memoised on `(kind, window, owner, maxDateMs,
    /// count, checksum, tz)`: the per-minute `current()` poll must not re-run a
    /// ~25k-reading analytics round-trip — the report recomputes only when new readings
    /// land, an existing reading's value is revised (the `checksum`; see `LocalStore.stats`),
    /// the period changes, or the store is reset.
    private func localReportJSON(kind: AnalyticsMemo.Kind, window: Int) async throws -> Data {
        guard let engine = LocalAnalytics.engine else {
            // Only the app registers an engine; extensions never compute analytics.
            throw APIError.notConfigured
        }
        let key = settings.sourceKey
        let tz = Self.tzOffsetMinutes
        let stats = try await LocalStore.shared.stats(sourceKey: key)
        let memoKey = AnalyticsMemo.Key(kind: kind, window: window, owner: key,
                                        maxDateMs: stats.maxDateMs ?? 0, count: stats.count,
                                        checksum: stats.checksum, tz: tz)
        if let cached = await AnalyticsMemo.shared.get(memoKey) { return cached }
        let hours = kind == .analytics ? window : window * 24
        let readingsJSON = try await LocalStore.shared.allReadingsJSON(hours: hours, sourceKey: key)
        let data: Data
        switch kind {
        case .analytics:
            data = try engine.analyticsJSON(readingsJSON: readingsJSON, hours: window, tzOffsetMin: tz)
        case .agp:
            data = try engine.agpJSON(readingsJSON: readingsJSON, days: window,
                                      binMinutes: Self.agpBinMinutes, tzOffsetMin: tz)
        }
        await AnalyticsMemo.shared.set(memoKey, data)
        return data
    }

    // MARK: - DTOs

    private struct CurrentEnvelope: Decodable { let current: CurrentDTO? }
    // Only `date` + `mgdl` are required; everything else is optional so server version
    // skew (or a server that omits the derived `mmol`) can't blank the live reading — the
    // display unit is computed client-side from mgdl anyway.
    private struct CurrentDTO: Decodable { let date: Int64; let mgdl: Double; let direction: String?; let trend: String?; let trendLabel: String? }
    private struct EntriesEnvelope: Decodable { let entries: [EntryDTO] }
    private struct EntryDTO: Decodable { let date: Int64; let mgdl: Double }
    private struct AnalyticsDTO: Decodable {
        let n: Int; let meanMgdl: Double?; let sdMgdl: Double?
        // Optional so an older server (pre-uGMI) still decodes — uGMI just shows "--".
        let uGmiPercent: Double?
        let gmiPercent: Double?
        let estimatedA1cPercent: Double?; let cvPercent: Double?; let timeInRange: TIRDTO
        // Optional so an older server (no Statistical-Analysis block) still decodes.
        let coverage: CoverageDTO?; let gri: GriDTO?; let variability: VarDTO?
        let patterns: [PeriodDTO]?; let episodes: EpisodesDTO?
    }
    private struct TIRDTO: Decodable { let veryLowPct, lowPct, inRangePct, highPct, veryHighPct: Double }
    private struct CoverageDTO: Decodable { let percentActive: Double?; let daysCovered: Double?; let distinctDays: Int?; let sufficient: Bool }
    private struct GriDTO: Decodable { let value: Double?; let zone: String?; let hypoComponent: Double?; let hyperComponent: Double? }
    private struct VarDTO: Decodable { let jIndex: Double?; let mage: Double?; let conga: Double?; let modd: Double?; let congaHours: Double? }
    private struct PeriodDTO: Decodable { let startHour: Int; let endHour: Int; let n: Int; let meanMgdl: Double?; let inRangePct: Double?; let cvPercent: Double? }
    private struct EpStatDTO: Decodable { let count: Int; let nocturnal: Int; let perDay: Double; let longestMin: Double; let totalMin: Double }
    private struct RecentEpDTO: Decodable { let kind: String; let start: Int64; let durationMin: Double; let extremeMgdl: Double; let nocturnal: Bool }
    private struct EpisodesDTO: Decodable { let low, veryLow, high, veryHigh: EpStatDTO; let recent: [RecentEpDTO] }
    private struct AgpDTO: Decodable { let bins: [AgpBinDTO] }
    private struct AgpBinDTO: Decodable { let minuteOfDay: Int; let n: Int; let p05: Double?; let p25: Double?; let p50: Double?; let p75: Double?; let p95: Double? }
}

/// Cache of the last FFI report per (kind, window, owner, data-version, tz). One small
/// entry per active view combination; wiped wholesale past a small cap rather than
/// tracking LRU — recomputing is cheap enough once a minute, just not once a second.
actor AnalyticsMemo {
    static let shared = AnalyticsMemo()

    enum Kind: Hashable { case analytics, agp }
    struct Key: Hashable {
        let kind: Kind
        let window: Int
        let owner: String
        let maxDateMs: Int64
        let count: Int
        /// Scaled sum of every stored `mgdl` — moves when a value is revised in place at
        /// an existing timestamp (which leaves `count`/`maxDateMs` unchanged).
        let checksum: Int64
        let tz: Int
    }

    private var store: [Key: Data] = [:]

    func get(_ key: Key) -> Data? { store[key] }

    func set(_ key: Key, _ data: Data) {
        if store.count >= 16 { store.removeAll() }
        store[key] = data
    }

    /// Drop everything — called when the local store is reset (source switch).
    func clear() { store.removeAll() }
}

extension Data {
    /// Lowercase hex encoding of these bytes — the wire format APNs and the server use for
    /// a device token (`didRegisterForRemoteNotificationsWithDeviceToken` hands over raw
    /// `Data`). Must be lowercase, two hex digits per byte, no separators.
    var apnsHexToken: String {
        map { String(format: "%02x", $0) }.joined()
    }
}
