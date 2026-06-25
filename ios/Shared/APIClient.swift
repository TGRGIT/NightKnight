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

/// Talks to the NightKnight `/api/v4` API. Authentication:
/// * `api-secret` — the device token created in the web UI (app-level auth).
/// * `CF-Access-Client-Id` / `CF-Access-Client-Secret` — a Cloudflare Access service
///   token to pass the edge gate (optional; only when deployed behind Access).
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

    // MARK: - Endpoints

    /// The device's UTC offset in minutes (east of UTC), for localising time-of-day
    /// analytics on the server (AGP, dawn patterns, nocturnal flags).
    static var tzOffsetMinutes: Int { TimeZone.current.secondsFromGMT() / 60 }

    func current() async throws -> CurrentReading? {
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
        let dto = try await fetch(EntriesEnvelope.self, path: "/api/v4/entries",
                                  query: [.init(name: "hours", value: String(hours))])
        return dto.entries.map {
            GlucoseReading(date: Date(timeIntervalSince1970: Double($0.date) / 1000),
                           value: GlucoseValue(mgdl: $0.mgdl))
        }
    }

    func analytics(hours: Int) async throws -> GlucoseAnalytics {
        let d = try await fetch(AnalyticsDTO.self, path: "/api/v4/analytics", query: [
            .init(name: "hours", value: String(hours)),
            .init(name: "tzOffset", value: String(Self.tzOffsetMinutes)),
        ])
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
            n: d.n, meanMgdl: d.meanMgdl, sdMgdl: d.sdMgdl, gmiPercent: d.gmiPercent,
            estimatedA1cPercent: d.estimatedA1cPercent, cvPercent: d.cvPercent,
            veryLowPct: d.timeInRange.veryLowPct, lowPct: d.timeInRange.lowPct,
            inRangePct: d.timeInRange.inRangePct, highPct: d.timeInRange.highPct,
            veryHighPct: d.timeInRange.veryHighPct,
            coverage: coverage, gri: gri, variability: variability,
            patterns: (d.patterns ?? []).map { PeriodInfo(startHour: $0.startHour, endHour: $0.endHour, n: $0.n, meanMgdl: $0.meanMgdl, inRangePct: $0.inRangePct) },
            episodes: episodes)
    }

    func agp(days: Int) async throws -> [AgpBin] {
        let d = try await fetch(AgpDTO.self, path: "/api/v4/agp", query: [
            .init(name: "days", value: String(days)),
            .init(name: "tzOffset", value: String(Self.tzOffsetMinutes)),
        ])
        return d.bins.map { AgpBin(minuteOfDay: $0.minuteOfDay, n: $0.n, p05: $0.p05, p25: $0.p25, p50: $0.p50, p75: $0.p75, p95: $0.p95) }
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
        let n: Int; let meanMgdl: Double?; let sdMgdl: Double?; let gmiPercent: Double?
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
