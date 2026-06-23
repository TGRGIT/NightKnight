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

    func current() async throws -> CurrentReading? {
        let dto = try await fetch(CurrentEnvelope.self, path: "/api/v4/current")
        guard let c = dto.current else { return nil }
        return CurrentReading(
            date: Date(timeIntervalSince1970: Double(c.date) / 1000),
            value: GlucoseValue(mgdl: c.mgdl),
            trend: TrendDirection(name: c.trend == nil ? c.direction : c.direction)
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
        let d = try await fetch(AnalyticsDTO.self, path: "/api/v4/analytics",
                                query: [.init(name: "hours", value: String(hours))])
        return GlucoseAnalytics(
            n: d.n, meanMgdl: d.meanMgdl, gmiPercent: d.gmiPercent,
            estimatedA1cPercent: d.estimatedA1cPercent, cvPercent: d.cvPercent,
            veryLowPct: d.timeInRange.veryLowPct, lowPct: d.timeInRange.lowPct,
            inRangePct: d.timeInRange.inRangePct, highPct: d.timeInRange.highPct,
            veryHighPct: d.timeInRange.veryHighPct)
    }

    // MARK: - DTOs

    private struct CurrentEnvelope: Decodable { let current: CurrentDTO? }
    private struct CurrentDTO: Decodable { let date: Int64; let mgdl: Double; let mmol: Double; let direction: String?; let trend: String? }
    private struct EntriesEnvelope: Decodable { let entries: [EntryDTO] }
    private struct EntryDTO: Decodable { let date: Int64; let mgdl: Double }
    private struct AnalyticsDTO: Decodable {
        let n: Int; let meanMgdl: Double?; let gmiPercent: Double?
        let estimatedA1cPercent: Double?; let cvPercent: Double?; let timeInRange: TIRDTO
    }
    private struct TIRDTO: Decodable { let veryLowPct, lowPct, inRangePct, highPct, veryHighPct: Double }
}
