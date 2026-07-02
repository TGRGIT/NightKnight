import Foundation

// Dexcom Share connector — a 1:1 port of the Rust reference
// (`service/crates/nightknight-connectors/src/dexcom.rs`), which reproduces the
// Dexcom Share API flow used by `pydexcom`:
// 1. `AuthenticatePublisherAccount` (accountName/password/applicationId) → account id
// 2. `LoginPublisherAccountById` (accountId/password/applicationId) → session id
// 3. `ReadPublisherLatestGlucoseValues?sessionId=…&minutes=…&maxCount=…` → readings
//
// All the request/response shaping is pure static functions (unit-tested against the
// same fixture bytes as the Rust tests); only `fetchRecent` touches URLSession.

/// A configured Dexcom Share client (credentials supplied by `StandaloneSources.make`).
struct DexcomShareClient: StandaloneSource {
    let region: Region
    let username: String
    let password: String

    /// Dexcom Share application id (US/OUS). Rust `APP_ID_US`.
    static let appIdUS = "d89443d2-327c-4a6f-89e5-496bbb0317db"
    /// Dexcom Share application id (Japan). Rust `APP_ID_JP`.
    static let appIdJP = "d8665ade-9673-4e27-9ff6-92db4ce13d13"

    /// Dexcom Share server region. Mirrors Rust `dexcom::Region`.
    enum Region: Sendable {
        case us, ous, jp

        static func parse(_ s: String) -> Region {
            switch s.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
            case "ous", "eu": return .ous
            case "jp": return .jp
            default: return .us
            }
        }

        var baseURL: String {
            switch self {
            case .us: return "https://share2.dexcom.com/ShareWebServices/Services"
            case .ous: return "https://shareous1.dexcom.com/ShareWebServices/Services"
            case .jp: return "https://share.dexcom.jp/ShareWebServices/Services"
            }
        }

        var applicationId: String {
            switch self {
            case .jp: return DexcomShareClient.appIdJP
            default: return DexcomShareClient.appIdUS
            }
        }
    }

    // MARK: - Pure request/response shaping (mirrors the Rust free functions)

    /// Body for `AuthenticatePublisherAccount`. Rust `authenticate_body`.
    static func authenticateBody(username: String, password: String,
                                 applicationId: String) -> [String: String] {
        ["accountName": username, "password": password, "applicationId": applicationId]
    }

    /// Body for `LoginPublisherAccountById`. Rust `login_body`.
    static func loginBody(accountId: String, password: String,
                          applicationId: String) -> [String: String] {
        ["accountId": accountId, "password": password, "applicationId": applicationId]
    }

    /// URL for `ReadPublisherLatestGlucoseValues`. Rust `read_url` (same query
    /// parameter order: sessionId, minutes, maxCount).
    static func readURL(base: String, sessionId: String, minutes: Int64,
                        maxCount: Int64) -> String {
        "\(base)/Publisher/ReadPublisherLatestGlucoseValues?sessionId=\(sessionId)&minutes=\(minutes)&maxCount=\(maxCount)"
    }

    /// The auth/login endpoints return a bare JSON string (a quoted UUID). Extract it.
    /// Rust `parse_quoted_id`.
    static func parseQuotedId(_ body: Data) throws -> String {
        guard let s = try? JSONSerialization.jsonObject(with: body, options: .fragmentsAllowed),
              let id = s as? String else {
            throw StandaloneError.auth("expected a quoted id string")
        }
        return id
    }

    /// Extract the epoch-ms out of a Dexcom `WT`/`ST` timestamp like
    /// `"Date(1699999999000-0500)"` — leading ASCII digits after `Date(`, timezone
    /// suffix ignored. Rust `parse_wt_ms`.
    static func parseWTMs(_ wt: String) -> Int64? {
        guard let marker = wt.range(of: "Date(") else { return nil }
        let digits = wt[marker.upperBound...].prefix(while: { ("0"..."9").contains($0) })
        return Int64(digits)
    }

    /// Map a Dexcom Share `Trend` field to a `TrendDirection`. Newer transmitters
    /// report a **string** (`"Flat"`, `"FortyFiveUp"`, …) that matches our Nightscout
    /// raw values 1:1; older ones report a legacy **integer** code (verified against
    /// `pydexcom`): `0=None, 1=DoubleUp, 2=SingleUp, 3=FortyFiveUp, 4=Flat,
    /// 5=FortyFiveDown, 6=SingleDown, 7=DoubleDown, 8=NotComputable, 9=RateOutOfRange`.
    /// Rust `trend_from_share`; Rust maps 8/9 to `NotComputable`/`RateOutOfRange` and
    /// the string sentinels via serde aliases — Swift's `TrendDirection` has no such
    /// cases, so those (and 0/unknown) become nil: no arrow.
    static func trendFromShare(_ v: Any) -> TrendDirection? {
        if let s = v as? String {
            return TrendDirection(rawValue: s)
        }
        guard let i = jsonInt(v) else { return nil }
        switch i {
        case 1: return .doubleUp
        case 2: return .singleUp
        case 3: return .fortyFiveUp
        case 4: return .flat
        case 5: return .fortyFiveDown
        case 6: return .singleDown
        case 7: return .doubleDown
        default: return nil // 0 = None (no arrow); 8/9 have no Swift case
        }
    }

    /// Parse the glucose-values array into `CgmSample`s. Rust `parse_glucose`: any
    /// single bad reading fails the whole parse (a truncated/garbled payload should
    /// surface, not silently thin out).
    static func parseGlucose(_ body: Data) throws -> [CgmSample] {
        let parsed: Any
        do {
            parsed = try JSONSerialization.jsonObject(with: body)
        } catch {
            throw StandaloneError.parse(error.localizedDescription)
        }
        guard let items = parsed as? [[String: Any]] else {
            throw StandaloneError.parse("expected a JSON array of readings")
        }
        var out: [CgmSample] = []
        out.reserveCapacity(items.count)
        for it in items {
            guard let mgdl = jsonInt(it["Value"]) else {
                throw StandaloneError.parse("reading missing Value")
            }
            // WT preferred; ST fallback only when WT is absent (like Rust's
            // `get("WT").or_else(|| get("ST"))`, a present-but-non-string WT errors).
            guard let wt = (it["WT"] ?? it["ST"]) as? String else {
                throw StandaloneError.parse("reading missing WT")
            }
            guard let dateMs = parseWTMs(wt) else {
                throw StandaloneError.parse("bad WT timestamp: \(wt)")
            }
            // Trend is a string on newer transmitters or a legacy integer code on
            // older ones — both map to our TrendDirection.
            let direction = it["Trend"].flatMap(trendFromShare)
            out.append(CgmSample(dateMs: dateMs, mgdl: mgdl,
                                 direction: direction, device: "dexcom-share"))
        }
        return out
    }

    // MARK: - Session cache (Swift addition over the Rust reference)

    // Rust's connector runs server-side where three round-trips per poll are cheap;
    // on-device, the app + widget + watch each poll on their own cadence, and
    // re-running the login pair every time is the pattern that gets Share accounts
    // flagged. Cache the session id in the App Group (shared with the extensions)
    // for a short TTL; a rejected read clears it and reruns the full flow once, so
    // an expired session costs one extra round-trip, not a failed poll.
    private static let sessionIdKey = "dexcom.session.id"
    private static let sessionAccountKey = "dexcom.session.account"
    private static let sessionExpiresKey = "dexcom.session.expires"
    private static let sessionTTL: TimeInterval = 20 * 60

    /// Which account the cached session belongs to — a region or username change
    /// must invalidate the cache. No password material goes into UserDefaults.
    private var accountFingerprint: String {
        "\(String(describing: region)):\(username.lowercased())"
    }

    private func cachedSessionId() -> String? {
        guard let d = UserDefaults(suiteName: Settings.appGroup),
              let id = d.string(forKey: Self.sessionIdKey),
              d.string(forKey: Self.sessionAccountKey) == accountFingerprint,
              Date().timeIntervalSince1970 < d.double(forKey: Self.sessionExpiresKey)
        else { return nil }
        return id
    }

    private func cacheSessionId(_ id: String) {
        guard let d = UserDefaults(suiteName: Settings.appGroup) else { return }
        d.set(id, forKey: Self.sessionIdKey)
        d.set(accountFingerprint, forKey: Self.sessionAccountKey)
        d.set(Date().timeIntervalSince1970 + Self.sessionTTL, forKey: Self.sessionExpiresKey)
    }

    static func clearCachedSession() {
        guard let d = UserDefaults(suiteName: Settings.appGroup) else { return }
        d.removeObject(forKey: sessionIdKey)
        d.removeObject(forKey: sessionAccountKey)
        d.removeObject(forKey: sessionExpiresKey)
    }

    // MARK: - The IO edge

    /// Every Dexcom POST carries exactly these three headers: Rust's `json_headers()`
    /// plus the `content-type` that `HttpReq::post_json` appends.
    private static func post(_ urlString: String, json: Any) async throws -> (status: Int, body: Data) {
        guard let url = URL(string: urlString) else {
            throw StandaloneError.proto("bad request URL")
        }
        var req = URLRequest(url: url, timeoutInterval: 20)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "accept")
        req.setValue("Dexcom Share/3.0.2.11 CFNetwork", forHTTPHeaderField: "user-agent")
        req.setValue("application/json", forHTTPHeaderField: "content-type")
        req.httpBody = try JSONSerialization.data(withJSONObject: json)
        let (data, resp) = try await URLSession.shared.data(for: req)
        return ((resp as? HTTPURLResponse)?.statusCode ?? 0, data)
    }

    func fetchRecent() async throws -> [CgmSample] {
        let base = region.baseURL
        // Rust's `fetch_recent` takes `minutes` from the runtime; the app always
        // wants the vendor's full rolling day, so Rust's clamps collapse to
        // constants: minutes.clamp(1, 1440) = 1440, (minutes / 5).clamp(1, 288) = 288.
        let minutes: Int64 = 1440
        let maxCount: Int64 = 288
        // 3. readings — POST with the literal empty JSON object body, params in the
        // query string only.
        let emptyBody: [String: String] = [:]

        // Cached session first: skip the two login round-trips. A non-2xx read means
        // the session expired (or the account changed server-side) — clear the cache
        // and run the full flow once, so this poll still returns fresh readings.
        if let session = cachedSessionId() {
            let read = try await Self.post(
                Self.readURL(base: base, sessionId: session, minutes: minutes, maxCount: maxCount),
                json: emptyBody)
            if (200..<300).contains(read.status) {
                return try Self.parseGlucose(read.body).sorted { $0.dateMs < $1.dateMs }
            }
            Self.clearCachedSession()
        }

        // A changed password must not become a re-login storm on a 60 s poll —
        // check the shared backoff before touching the vendor's login endpoints.
        let key = "dexcom:\(String(describing: region)):\(username.lowercased())"
        try await SourceBackoff.shared.checkPermission(key)

        let app = region.applicationId

        // 1. account id
        let acct = try await Self.post(
            "\(base)/General/AuthenticatePublisherAccount",
            json: Self.authenticateBody(username: username, password: password, applicationId: app))
        guard (200..<300).contains(acct.status) else {
            await SourceBackoff.shared.recordAuthFailure(key)
            throw StandaloneError.auth("authenticate failed (\(acct.status))")
        }
        let accountId: String
        do {
            accountId = try Self.parseQuotedId(acct.body)
        } catch {
            await SourceBackoff.shared.recordAuthFailure(key)
            throw error
        }

        // 2. session id
        let sess = try await Self.post(
            "\(base)/General/LoginPublisherAccountById",
            json: Self.loginBody(accountId: accountId, password: password, applicationId: app))
        guard (200..<300).contains(sess.status) else {
            await SourceBackoff.shared.recordAuthFailure(key)
            throw StandaloneError.auth("login failed (\(sess.status))")
        }
        let sessionId: String
        do {
            sessionId = try Self.parseQuotedId(sess.body)
        } catch {
            await SourceBackoff.shared.recordAuthFailure(key)
            throw error
        }
        await SourceBackoff.shared.recordSuccess(key)
        cacheSessionId(sessionId)

        let read = try await Self.post(
            Self.readURL(base: base, sessionId: sessionId, minutes: minutes, maxCount: maxCount),
            json: emptyBody)
        guard (200..<300).contains(read.status) else {
            throw StandaloneError.proto("read failed (\(read.status))")
        }
        // Dexcom returns newest-first; the protocol contract is newest-last.
        return try Self.parseGlucose(read.body).sorted { $0.dateMs < $1.dateMs }
    }
}
