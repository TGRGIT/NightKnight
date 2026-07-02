import CryptoKit
import Foundation

/// LibreLinkUp connector.
///
/// Implements the LibreLinkUp cloud flow (the same one the LibreLinkUp mobile app and
/// community bridges use): authenticate, list connections (the followed patient), and
/// read the latest measurement + recent graph. A 1:1 port of the Rust reference
/// (`nightknight-connectors/src/librelinkup.rs`) — same function names, constants and
/// edge-case handling — so the two implementations can be diffed side-by-side. The
/// protocol shaping (URLs, headers, body, parsing) is pure static functions; only the
/// thin send-loop at the bottom touches `URLSession`.
///
/// On top of the Rust flow this port caches the bearer session in the App Group (see
/// `fetchRecent`): the Rust service is one server-side poller, but on a phone the app,
/// widget and watch each poll — logging in per fetch is exactly the re-login storm
/// that trips LibreLinkUp's account lockout (`status 429`).
///
/// Note: these are **unofficial** endpoints. The `product`/`version` headers and the
/// hashed `account-id` header reflect the currently-known requirements and may need
/// updating if the vendor changes them.
struct LibreLinkUpClient: StandaloneSource {
    let email: String
    let password: String

    static let defaultBase = "https://api.libreview.io"
    static let product = "llu.android"
    // LibreView gates the data endpoints (`/llu/connections`, `/graph`) on a minimum
    // client version: an older value logs in fine but then gets `403 {"status":920,
    // "data":{"minimumVersion":"4.16.0"}}`. Keep this at/above that floor.
    static let version = "4.16.0"

    // MARK: - Pure protocol shaping (mirrors the Rust reference)

    /// Regional API base after a login redirect (e.g. region `"eu"` → `api-eu.libreview.io`).
    static func regionalBase(_ region: String) -> String {
        "https://api-\(region.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()).libreview.io"
    }

    /// LibreView region codes are short alphanumeric tokens (e.g. `"eu"`, `"us"`, `"de"`).
    /// Validate before interpolating into the API host so a malformed/hostile value (a
    /// `/`, `@`, `.` …) can't repoint the request elsewhere via `regionalBase`.
    static func isValidRegion(_ region: String) -> Bool {
        let r = region.trimmingCharacters(in: .whitespacesAndNewlines)
        return !r.isEmpty && r.utf8.count <= 16 && r.unicodeScalars.allSatisfy { c in
            ("a"..."z").contains(c) || ("A"..."Z").contains(c) || ("0"..."9").contains(c) || c == "-"
        }
    }

    /// SHA-256 hex (lowercase) of the **user id** from the login response — NOT the
    /// email — the value LibreLinkUp expects in the `account-id` header on
    /// authenticated requests.
    static func accountIdHash(_ userId: String) -> String {
        SHA256.hash(data: Data(userId.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    /// Standard headers; pass the bearer token and account-id hash once authenticated.
    static func headers(token: String?, accountId: String?) -> [(String, String)] {
        var h: [(String, String)] = [
            ("product", product),
            ("version", version),
            ("accept", "application/json"),
            ("cache-control", "no-cache"),
            // The LibreLinkUp Android app talks via okhttp. We MUST send this
            // explicitly: LibreView's edge (Akamai) blocks unrecognised User-Agents
            // with a bare 403 before any credential check. A browser-style UA is also
            // blocked; okhttp passes.
            ("User-Agent", "okhttp/4.9.3"),
        ]
        if let t = token { h.append(("authorization", "Bearer \(t)")) }
        if let a = accountId { h.append(("account-id", a)) }
        return h
    }

    static func loginBody(email: String, password: String) -> Data {
        // A two-string dictionary is always serialisable.
        (try? JSONSerialization.data(withJSONObject: ["email": email, "password": password])) ?? Data()
    }

    static func connectionsURL(base: String) -> String {
        "\(base)/llu/connections"
    }

    static func graphURL(base: String, patientId: String) -> String {
        "\(base)/llu/connections/\(patientId)/graph"
    }

    /// The connection logbook — ~2 weeks of alarm/scan **event** points (not a
    /// continuous trace). Reachable with the LibreLinkUp token; used as the backfill
    /// fallback when the dense history endpoint isn't available.
    static func logbookURL(base: String, patientId: String) -> String {
        "\(base)/llu/connections/\(patientId)/logbook"
    }

    /// The LibreView patient glucose-history endpoint (up to 90 days, dense). This is
    /// the *first-choice* backfill source, but empirically the LibreLinkUp-issued
    /// token is product-gated out of it (`400 ProductMismatch`) — it belongs to the
    /// LibreView **web** product — so `backfill()` treats it as best-effort and falls
    /// back to the logbook. `period` is the window in days (max 90 = the app's rendered
    /// window); `numPeriods=1` requests it as a single span.
    static func historyURL(base: String, days: Int = 90) -> String {
        "\(base)/glucoseHistory?numPeriods=1&period=\(max(1, min(days, 90)))"
    }

    /// Outcome of a login attempt.
    enum LoginResult: Equatable {
        /// Authenticated: bearer token + the account's user id.
        case authenticated(token: String, userId: String)
        /// The account lives in another region; re-login against it.
        case redirect(region: String)
    }

    static func parseLogin(_ body: Data) throws -> LoginResult {
        let v = try jsonObject(body)
        // LibreLinkUp signals failure with a non-zero `status` (2 = bad credentials,
        // 4 = action required / terms not accepted, 429 = locked) and NO `data` object —
        // the reason lives in `error.message` (or `data.message` for lockouts). Surface
        // it so the connector's status tells the user exactly what's wrong.
        if let code = (v["status"] as? NSNumber)?.int64Value, code != 0 {
            let msg = ((v["error"] as? [String: Any])?["message"] as? String)
                ?? ((v["data"] as? [String: Any])?["message"] as? String)
                ?? "login rejected"
            throw StandaloneError.auth("LibreLinkUp login status \(code): \(msg)")
        }
        guard let data = v["data"] else { throw StandaloneError.auth("login: no data") }
        let d = data as? [String: Any] ?? [:]
        if (d["redirect"] as? Bool) ?? false {
            guard let region = d["region"] as? String else {
                throw StandaloneError.auth("login redirect without region")
            }
            guard isValidRegion(region) else {
                throw StandaloneError.auth("login redirect with invalid region \"\(region)\"")
            }
            return .redirect(region: region)
        }
        guard let token = (d["authTicket"] as? [String: Any])?["token"] as? String else {
            throw StandaloneError.auth("login: no token (bad credentials?)")
        }
        guard let userId = (d["user"] as? [String: Any])?["id"] as? String else {
            throw StandaloneError.auth("login: no user id")
        }
        return .authenticated(token: token, userId: userId)
    }

    /// Patient ids of the accounts this user follows.
    static func parseConnections(_ body: Data) throws -> [String] {
        let v = try jsonObject(body)
        guard let arr = v["data"] as? [Any] else {
            throw StandaloneError.parse("connections: no data array")
        }
        return arr.compactMap { ($0 as? [String: Any])?["patientId"] as? String }
    }

    /// LibreLinkUp `TrendArrow` integer → our `TrendDirection`. Only 1–5 exist on the
    /// wire; anything else is nil (no arrow) — which also covers Rust `Direction`'s
    /// `None`/`NotComputable`/`RateOutOfRange` sentinels, none of which the Swift enum
    /// needs as arrow-bearing cases.
    static func trendFromArrow(_ n: Int) -> TrendDirection? {
        switch n {
        case 1: return .singleDown
        case 2: return .fortyFiveDown
        case 3: return .flat
        case 4: return .fortyFiveUp
        case 5: return .singleUp
        default: return nil
        }
    }

    /// Parse LibreLinkUp's `FactoryTimestamp` (UTC, `"M/D/YYYY h:mm:ss AM/PM"`) to
    /// epoch ms. Manual split/compose — no `DateFormatter` — so the result is
    /// deterministic against device locale/12-hour settings and byte-identical to the
    /// Rust parser: seconds optional (default 0), AM/PM case-insensitive, 12 AM → 0 h,
    /// 12 PM → 12 h.
    static func parseFactoryTimestamp(_ s: String) -> Int64? {
        let s = s.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let dateEnd = s.firstIndex(of: " ") else { return nil }
        let date = s[..<dateEnd]
        let rest = s[s.index(after: dateEnd)...]
        guard let timeEnd = rest.lastIndex(of: " ") else { return nil }
        let time = rest[..<timeEnd]
        let ampm = rest[rest.index(after: timeEnd)...]
        let d = date.split(separator: "/", omittingEmptySubsequences: false)
        guard d.count >= 3,
              let month = Int64(d[0]), let day = Int64(d[1]), let year = Int64(d[2]) else { return nil }
        let t = time.split(separator: ":", omittingEmptySubsequences: false)
        guard t.count >= 2, var hour = Int64(t[0]), let min = Int64(t[1]) else { return nil }
        let sec = t.count >= 3 ? (Int64(t[2]) ?? 0) : 0
        switch ampm.uppercased() {
        case "AM": if hour == 12 { hour = 0 }
        case "PM": if hour != 12 { hour += 12 }
        default: return nil
        }
        return ymdHmsMilliToMs(year, month, day, hour, min, sec, 0)
    }

    /// One measurement object → a sample. `withTrend` is true only for the latest
    /// reading (`connection.glucoseMeasurement`) — history points carry no arrow.
    static func sampleFromMeasurement(_ m: [String: Any], withTrend: Bool) -> CgmSample? {
        guard let mgdl = jsonInt(m["ValueInMgPerDl"]) else { return nil }
        guard let ts = (m["FactoryTimestamp"] ?? m["Timestamp"]) as? String else { return nil }
        guard let dateMs = parseFactoryTimestamp(ts) else { return nil }
        let direction = withTrend ? jsonInt(m["TrendArrow"]).flatMap(trendFromArrow) : nil
        return CgmSample(dateMs: dateMs, mgdl: mgdl, direction: direction, device: "librelinkup")
    }

    /// Parse the graph response: the latest measurement (with trend) + the recent
    /// points. Unparseable measurements are silently dropped, like the Rust reference.
    static func parseGraph(_ body: Data) throws -> [CgmSample] {
        let v = try jsonObject(body)
        guard let data = v["data"] else { throw StandaloneError.parse("graph: no data") }
        let d = data as? [String: Any] ?? [:]
        var out: [CgmSample] = []
        if let latest = (d["connection"] as? [String: Any])?["glucoseMeasurement"] as? [String: Any],
           let s = sampleFromMeasurement(latest, withTrend: true) {
            out.append(s)
        }
        if let points = d["graphData"] as? [Any] {
            for p in points {
                if let m = p as? [String: Any], let s = sampleFromMeasurement(m, withTrend: false) {
                    out.append(s)
                }
            }
        }
        return out
    }

    /// Parse the connection logbook: `data` is a flat array of measurement objects
    /// (`ValueInMgPerDl` + `FactoryTimestamp`), same shape as a graph point. These are
    /// alarm/scan events rather than a continuous trace, so they're sparse and skewed
    /// toward highs/lows — a thin backfill, not a substitute for a CSV import.
    static func parseLogbook(_ body: Data) throws -> [CgmSample] {
        let v = try jsonObject(body)
        guard let arr = v["data"] as? [Any] else {
            throw StandaloneError.parse("logbook: no data array")
        }
        return arr.compactMap {
            ($0 as? [String: Any]).flatMap { sampleFromMeasurement($0, withTrend: false) }
        }
    }

    /// Parse the glucose-history response. Its exact shape is **unverified** — the
    /// endpoint is product-gated for LibreLinkUp tokens, so we've never seen a success
    /// body — so this walks `data` and collects every measurement-shaped object
    /// (`ValueInMgPerDl` + a timestamp) at any nesting, which is robust to however the
    /// web product happens to wrap the readings.
    static func parseGlucoseHistory(_ body: Data) throws -> [CgmSample] {
        let v = try jsonObject(body)
        guard let data = v["data"] else { return [] }
        var out: [CgmSample] = []
        collectMeasurements(data, into: &out)
        return out
    }

    private static func collectMeasurements(_ any: Any, into out: inout [CgmSample]) {
        if let dict = any as? [String: Any] {
            // A measurement object is a leaf — parse it and don't recurse into it (so a
            // reading's own fields can't be double-counted).
            if let s = sampleFromMeasurement(dict, withTrend: false) { out.append(s) }
            else { for value in dict.values { collectMeasurements(value, into: &out) } }
        } else if let arr = any as? [Any] {
            for element in arr { collectMeasurements(element, into: &out) }
        }
    }

    /// A compact, single-line preview of a response body for diagnostics — lets a 403
    /// reveal *who* refused: an Akamai/edge bot-block (HTML "Access Denied" + reference)
    /// vs a LibreView app-layer JSON message. Truncated; control chars collapsed.
    static func snippet(_ body: Data) -> String {
        let text = String(decoding: body, as: UTF8.self)
        let cleaned = String(String.UnicodeScalarView(text.unicodeScalars.map {
            $0.properties.generalCategory == .control ? " " : $0
        }))
        let trimmed = cleaned.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        var s = String(String.UnicodeScalarView(trimmed.unicodeScalars.prefix(180)))
        if trimmed.unicodeScalars.count > 180 { s.append("…") }
        return s
    }

    /// The `exp` claim (epoch seconds) of a JWT's base64url payload segment.
    /// LibreLinkUp bearer tokens are JWTs; this is what lets the session cache reuse a
    /// token until just before the vendor would reject it. Nil for anything unreadable
    /// (treated as already expired).
    static func jwtExp(_ token: String) -> Int64? {
        let parts = token.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count >= 2 else { return nil }
        var b64 = parts[1].replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        while b64.count % 4 != 0 { b64.append("=") }
        guard let payload = Data(base64Encoded: b64),
              let obj = (try? JSONSerialization.jsonObject(with: payload)) as? [String: Any],
              let exp = obj["exp"] as? NSNumber else { return nil }
        return exp.int64Value
    }

    /// Top-level JSON object, or `[:]` for valid JSON of another shape — mirrors the
    /// Rust `Value` accessors, where `.get(...)` on a non-object simply yields nothing,
    /// so shape errors surface as the caller's specific message rather than a generic
    /// decode failure.
    private static func jsonObject(_ body: Data) throws -> [String: Any] {
        let v: Any
        do { v = try JSONSerialization.jsonObject(with: body, options: [.fragmentsAllowed]) }
        catch { throw StandaloneError.parse(error.localizedDescription) }
        return v as? [String: Any] ?? [:]
    }

    // MARK: - Civil time (verbatim port of nightknight-core/src/timeutil.rs)

    /// Compose a UTC instant (epoch ms) from civil date/time components. Exact for all
    /// proleptic-Gregorian dates.
    private static func ymdHmsMilliToMs(_ year: Int64, _ month: Int64, _ day: Int64,
                                        _ hour: Int64, _ min: Int64, _ sec: Int64,
                                        _ milli: Int64) -> Int64 {
        let days = daysFromCivil(year, month, day)
        return (((days * 24 + hour) * 60 + min) * 60 + sec) * 1000 + milli
    }

    /// Days from 1970-01-01 to the given proleptic-Gregorian date — Howard Hinnant's
    /// `days_from_civil`, exact for all dates (correct leap-year handling included).
    private static func daysFromCivil(_ year: Int64, _ month: Int64, _ day: Int64) -> Int64 {
        let y = month <= 2 ? year - 1 : year
        let era = (y >= 0 ? y : y - 399) / 400
        let yoe = y - era * 400 // [0, 399]
        let doy = (153 * (month > 2 ? month - 3 : month + 9) + 2) / 5 + day - 1 // [0, 365]
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy // [0, 146096]
        return era * 146097 + doe - 719468
    }

    // MARK: - Session cache (App Group)

    // Keys are `libre.`-prefixed so `clearCachedSession` can wipe the whole session
    // without touching the user's Settings entries. `libre.account` records which
    // email the session belongs to, so switching accounts invalidates it implicitly.
    private static let tokenKey = "libre.token"
    private static let accountIdKey = "libre.accountId"
    private static let regionKey = "libre.region"
    private static let tokenExpKey = "libre.tokenExp"
    private static let accountKey = "libre.account"

    private static var store: UserDefaults {
        UserDefaults(suiteName: Settings.appGroup) ?? .standard
    }

    /// Drop the cached bearer session (sign-out / account switch).
    static func clearCachedSession() {
        let d = store
        for key in [tokenKey, accountIdKey, regionKey, tokenExpKey, accountKey] {
            d.removeObject(forKey: key)
        }
    }

    /// The cached session, if it belongs to this account and its JWT is still valid —
    /// with a 60 s margin so a token can't expire mid-request.
    private static func cachedSession(_ d: UserDefaults, email: String) -> (token: String, accountId: String)? {
        guard d.string(forKey: accountKey) == email,
              let token = d.string(forKey: tokenKey), !token.isEmpty,
              let accountId = d.string(forKey: accountIdKey), !accountId.isEmpty else { return nil }
        let exp = (d.object(forKey: tokenExpKey) as? NSNumber)?.int64Value ?? 0
        guard Int64(Date().timeIntervalSince1970) < exp - 60 else { return nil }
        return (token, accountId)
    }

    // MARK: - Fetch flow (IO at the edges)

    /// Mirrors Rust `fetch_recent` — login (following one region redirect), list
    /// connections, read the first patient's graph — with the session cache in front:
    /// a still-valid cached token skips the login POST entirely, and a 401 on either
    /// authed GET clears the cache and falls through to exactly one fresh login.
    func fetchRecent() async throws -> [CgmSample] {
        let defaults = Self.store
        let backoffKey = "libre:\(email.lowercased())"
        let cachedRegion = defaults.string(forKey: Self.regionKey) ?? ""
        let region = cachedRegion.isEmpty ? (defaults.string(forKey: "libreRegion") ?? "") : cachedRegion

        if let session = Self.cachedSession(defaults, email: email) {
            let base = Self.isValidRegion(region) ? Self.regionalBase(region) : Self.defaultBase
            do {
                let samples = try await fetchData(base: base, token: session.token,
                                                  accountId: session.accountId, retryOn401: true)
                return samples.sorted { $0.dateMs < $1.dateMs }
            } catch is Unauthorized {
                // The vendor killed the token before its `exp` claim — drop the
                // session and re-authenticate once below.
                Self.clearCachedSession()
            }
        }

        let fresh = try await login(defaults, region: region, backoffKey: backoffKey)
        // `retryOn401: false`: a 401 straight after a successful login surfaces as the
        // plain Rust-shaped "… failed (401)" protocol error instead of a login loop.
        let samples = try await fetchData(base: fresh.base, token: fresh.token,
                                          accountId: fresh.accountId, retryOn401: false)
        return samples.sorted { $0.dateMs < $1.dateMs }
    }

    /// Best-effort history backfill (a Swift-side extension — the Rust reference has no
    /// LibreLinkUp backfill). First choice: the dense `/glucoseHistory` window (up to 90
    /// days). In practice the LibreLinkUp token is product-gated out of that endpoint
    /// (`400 ProductMismatch`), so on any failure this falls back to the `/logbook`
    /// (~2 weeks of alarm/scan events — sparse and skewed, hence the CSV import remains
    /// the way to get a true dense history). Returns whatever it could get, newest-last.
    func backfill() async throws -> [CgmSample] {
        let session = try await resolveSession()
        let authed = Self.headers(token: session.token, accountId: session.accountId)

        // First option: dense patient history, up to the rendered window.
        if let hist = try? await Self.get(Self.historyURL(base: session.base), headers: authed),
           (200..<300).contains(hist.status),
           let samples = try? Self.parseGlucoseHistory(hist.body), !samples.isEmpty {
            return samples.sorted { $0.dateMs < $1.dateMs }
        }

        // Fallback: the connection logbook (~2 weeks of events).
        let patient = try await firstPatient(session)
        let log = try await Self.get(Self.logbookURL(base: session.base, patientId: patient),
                                     headers: authed)
        guard (200..<300).contains(log.status) else {
            throw StandaloneError.proto("logbook failed (\(log.status)) \(Self.snippet(log.body))")
        }
        return try Self.parseLogbook(log.body).sorted { $0.dateMs < $1.dateMs }
    }

    /// A valid session for the authed backfill requests — the cached bearer if it's
    /// still good, otherwise a fresh login (which is backoff-gated and re-cached).
    private func resolveSession() async throws -> (base: String, token: String, accountId: String) {
        let defaults = Self.store
        let cachedRegion = defaults.string(forKey: Self.regionKey) ?? ""
        let region = cachedRegion.isEmpty ? (defaults.string(forKey: "libreRegion") ?? "") : cachedRegion
        if let s = Self.cachedSession(defaults, email: email) {
            let base = Self.isValidRegion(region) ? Self.regionalBase(region) : Self.defaultBase
            return (base, s.token, s.accountId)
        }
        return try await login(defaults, region: region, backoffKey: "libre:\(email.lowercased())")
    }

    /// The first followed patient's id (backfill needs it for the logbook URL).
    private func firstPatient(_ session: (base: String, token: String, accountId: String)) async throws -> String {
        let authed = Self.headers(token: session.token, accountId: session.accountId)
        let conns = try await Self.get(Self.connectionsURL(base: session.base), headers: authed)
        guard (200..<300).contains(conns.status) else {
            throw StandaloneError.proto("connections failed (\(conns.status)) \(Self.snippet(conns.body))")
        }
        guard let patient = try Self.parseConnections(conns.body).first else {
            throw StandaloneError.proto("no LibreLinkUp connections")
        }
        return patient
    }

    /// The authed data flow: connections → first patientId → graph. With
    /// `retryOn401: true` (the cached-token path) a 401 throws the private
    /// `Unauthorized` marker so the caller can refresh the login; otherwise every
    /// non-2xx surfaces with the Rust reference's exact message shape.
    private func fetchData(base: String, token: String, accountId: String,
                           retryOn401: Bool) async throws -> [CgmSample] {
        let authed = Self.headers(token: token, accountId: accountId)

        let conns = try await Self.get(Self.connectionsURL(base: base), headers: authed)
        if retryOn401 && conns.status == 401 { throw Unauthorized() }
        guard (200..<300).contains(conns.status) else {
            throw StandaloneError.proto("connections failed (\(conns.status)) \(Self.snippet(conns.body))")
        }
        guard let patient = try Self.parseConnections(conns.body).first else {
            throw StandaloneError.proto("no LibreLinkUp connections")
        }

        let graph = try await Self.get(Self.graphURL(base: base, patientId: patient), headers: authed)
        if retryOn401 && graph.status == 401 { throw Unauthorized() }
        guard (200..<300).contains(graph.status) else {
            throw StandaloneError.proto("graph failed (\(graph.status)) \(Self.snippet(graph.body))")
        }
        return try Self.parseGraph(graph.body)
    }

    /// Fresh login, following at most one region redirect (mirrors Rust
    /// `fetch_recent`'s login phase), then cache the session for subsequent polls.
    private func login(_ defaults: UserDefaults, region initialRegion: String,
                       backoffKey: String) async throws -> (base: String, token: String, accountId: String) {
        // Backoff gate BEFORE any login POST: with a 60 s poll a bad password would
        // otherwise become a re-login storm — the exact pattern that trips
        // LibreLinkUp's account lockout (status 429).
        try await SourceBackoff.shared.checkPermission(backoffKey)
        do {
            var base = Self.isValidRegion(initialRegion) ? Self.regionalBase(initialRegion) : Self.defaultBase
            var result = try await doLogin(base: base)
            if case .redirect(let region) = result {
                // The account lives in another region: remember it under the cache
                // prefix AND the Settings key (so the settings UI can show it), then
                // re-login against the regional host.
                base = Self.regionalBase(region)
                defaults.set(region, forKey: Self.regionKey)
                defaults.set(region, forKey: "libreRegion")
                result = try await doLogin(base: base)
            }
            guard case .authenticated(let token, let userId) = result else {
                // A second redirect — exactly one is followed, like the Rust reference.
                throw StandaloneError.auth("login redirect loop")
            }
            let accountId = Self.accountIdHash(userId)
            defaults.set(email, forKey: Self.accountKey)
            defaults.set(token, forKey: Self.tokenKey)
            defaults.set(accountId, forKey: Self.accountIdKey)
            defaults.set(Self.jwtExp(token) ?? 0, forKey: Self.tokenExpKey)
            await SourceBackoff.shared.recordSuccess(backoffKey)
            return (base, token, accountId)
        } catch {
            // Only a vendor sign-in rejection (`.auth`) charges the backoff — a
            // transport blip (thrown URLError) or a parse/protocol hiccup isn't a
            // failed credential attempt and shouldn't walk toward a synthetic lockout.
            if let e = error as? StandaloneError, case .auth = e {
                await SourceBackoff.shared.recordAuthFailure(backoffKey)
            }
            throw error
        }
    }

    /// Mirrors Rust `do_login`: POST the credentials, surface a non-2xx as the bare
    /// status (an auth endpoint's body is never echoed into errors), else parse.
    private func doLogin(base: String) async throws -> LoginResult {
        let resp = try await Self.postJSON("\(base)/llu/auth/login",
                                           headers: Self.headers(token: nil, accountId: nil),
                                           body: Self.loginBody(email: email, password: password))
        guard (200..<300).contains(resp.status) else {
            throw StandaloneError.auth("login failed (\(resp.status))")
        }
        return try Self.parseLogin(resp.body)
    }

    // MARK: - Thin URLSession edge

    /// A 401 on an authed GET with a cached token — the signal to re-login once.
    private struct Unauthorized: Error {}

    private static func request(_ url: String, headers: [(String, String)]) throws -> URLRequest {
        // The bases are constants + a validated region and the patient id comes from
        // the vendor's own JSON, but keep the guard so a malformed value fails loudly.
        guard let u = URL(string: url) else { throw StandaloneError.proto("bad URL") }
        var req = URLRequest(url: u)
        req.timeoutInterval = 20
        for (name, value) in headers { req.setValue(value, forHTTPHeaderField: name) }
        return req
    }

    private static func get(_ url: String, headers: [(String, String)]) async throws -> (body: Data, status: Int) {
        try await send(request(url, headers: headers))
    }

    private static func postJSON(_ url: String, headers: [(String, String)],
                                 body: Data) async throws -> (body: Data, status: Int) {
        var req = try request(url, headers: headers)
        req.httpMethod = "POST"
        // Mirrors Rust `HttpReq::post_json`, which appends the content type itself.
        req.setValue("application/json", forHTTPHeaderField: "content-type")
        req.httpBody = body
        return try await send(req)
    }

    private static func send(_ req: URLRequest) async throws -> (body: Data, status: Int) {
        let (data, resp) = try await URLSession.shared.data(for: req)
        return (data, (resp as? HTTPURLResponse)?.statusCode ?? 0)
    }
}
