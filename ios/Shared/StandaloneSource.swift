import CoreFoundation
import Foundation

/// `JSONSerialization` backs a JSON boolean with an `NSNumber` (`__NSCFBoolean`), and
/// Foundation's `NSNumber as? Int`/`as? Int64` bridging silently coerces it to `1`/`0`
/// — unlike Rust's `serde_json::Value::as_i64()`, which returns `None` for
/// `Value::Bool`. Every connector parses vendor-controlled JSON (Nightscout is a
/// user-supplied, possibly hostile origin), so a malformed `{"sgv": true}` must be
/// discarded like the Rust reference discards it, not silently read as `mgdl: 1`.
/// Use these in place of a bare `as? Int`/`as? Int64` on any vendor numeric field.
///
/// These also reject a JSON *float* token (`90.6`, `90.0`), matching `as_i64()`, which
/// returns `None` for `Value::Number` that isn't integral. Without this guard,
/// `NSNumber.intValue`/`.int64Value` would silently truncate `90.6 → 90`, diverging
/// from the Rust reference at every `as_i64()` call site (a fractional `sgv` the server
/// would round to `91`, or a float `date` the server would skip entirely). A field the
/// reference *does* round (`as_i64().or_else(as_f64().round())`) must layer that on top
/// via `jsonDouble` explicitly, so the round is expressed at the call site, not hidden
/// in a truncation here.
func jsonInt(_ v: Any?) -> Int? {
    guard let n = v as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID(),
          !CFNumberIsFloatType(n as CFNumber) else { return nil }
    return n.intValue
}

func jsonInt64(_ v: Any?) -> Int64? {
    guard let n = v as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID(),
          !CFNumberIsFloatType(n as CFNumber) else { return nil }
    return n.int64Value
}

func jsonDouble(_ v: Any?) -> Double? {
    guard let n = v as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
    return n.doubleValue
}

// The data-source abstraction for the serverless modes: `APIClient` depends on the
// `StandaloneSource` protocol, never on a concrete provider; only the
// `StandaloneSources.make` factory names the concrete set (Dexcom Share /
// LibreLinkUp / Nightscout), so adding a fifth source touches one function.
//
// The value types here mirror the Rust `nightknight-connectors` crate 1:1 (same
// names, same fields, same error taxonomy) so a reviewer can diff the Swift port
// against the Rust reference side-by-side.

/// One normalised glucose reading from a vendor cloud. Mirrors Rust
/// `nightknight_connectors::CgmSample { date_ms, mgdl, direction, device }`.
struct CgmSample: Sendable, Equatable {
    /// Reading time, epoch milliseconds (UTC).
    let dateMs: Int64
    /// Glucose in mg/dL (vendor clouds report mg/dL natively).
    let mgdl: Int
    /// Trend arrow, if the vendor provided one.
    let direction: TrendDirection?
    /// Source device label, e.g. `"dexcom-share"`.
    let device: String
}

extension CgmSample {
    // The single conversion point from vendor samples to app models — everything
    // downstream of here is provider-agnostic.
    var asReading: GlucoseReading {
        GlucoseReading(date: Date(timeIntervalSince1970: Double(dateMs) / 1000),
                       value: GlucoseValue(mgdl: Double(mgdl)))
    }

    var asCurrent: CurrentReading {
        let trend = direction ?? .none
        return CurrentReading(date: Date(timeIntervalSince1970: Double(dateMs) / 1000),
                              value: GlucoseValue(mgdl: Double(mgdl)),
                              trend: trend,
                              trendLabel: trend.label)
    }
}

/// Mirrors Rust `ConnectorError::{Auth, Protocol, Parse}` — the vendor's real message
/// rides along so a version-floor rejection or account lockout is diagnosable in the
/// UI, not a generic failure. (Transport errors surface as thrown `URLError`s.)
enum StandaloneError: Error, LocalizedError {
    case auth(String)
    case proto(String)
    case parse(String)

    var errorDescription: String? {
        switch self {
        case .auth(let m): return "Authentication failed: \(m)"
        case .proto(let m): return "Vendor protocol error: \(m)"
        case .parse(let m): return "Could not parse vendor response: \(m)"
        }
    }
}

/// A direct (serverless) glucose source. Implementations are small value types
/// holding only the account credentials; session state is cached in the App Group so
/// a fresh struct per poll doesn't re-authenticate.
protocol StandaloneSource: Sendable {
    /// The vendor's recent window of raw readings, newest-last (chronological).
    func fetchRecent() async throws -> [CgmSample]
    /// The newest sample as a `CurrentReading`, or nil when the vendor has none.
    func current() async throws -> CurrentReading?
    /// Full history where the vendor supports it (Nightscout's paginated walk);
    /// sources with only a rolling window return `[]` and accumulate over time.
    func backfill() async throws -> [CgmSample]
}

extension StandaloneSource {
    func current() async throws -> CurrentReading? {
        try await fetchRecent().max(by: { $0.dateMs < $1.dateMs })?.asCurrent
    }

    func backfill() async throws -> [CgmSample] { [] }
}

/// The factory — the ONLY switch over the concrete source set.
enum StandaloneSources {
    /// Returns nil for `.nightknight` (and for "not yet chosen"): server mode has no
    /// standalone source; `APIClient` keeps its classic fetch path.
    static func make(_ settings: Settings) -> (any StandaloneSource)? {
        switch settings.dataSource ?? .nightknight {
        case .nightknight:
            return nil
        case .dexcom:
            return DexcomShareClient(region: DexcomShareClient.Region.parse(settings.dexcomRegion),
                                     username: settings.dexcomUsername,
                                     password: settings.dexcomPassword)
        case .libre:
            return LibreLinkUpClient(email: settings.libreEmail,
                                     password: settings.librePassword)
        case .nightscout:
            return NightscoutClient(baseURL: settings.nightscoutURL,
                                    secret: settings.nightscoutSecret)
        }
    }
}

/// Exponential backoff on repeated authentication failures, shared by every source:
/// with a 60 s poll, a changed password would otherwise become a re-login storm —
/// the exact pattern that trips LibreLinkUp's account lockout (`status 429`) and
/// flags Dexcom accounts. Keyed by the account (`sourceKey`); in-memory only, which
/// is enough because the app process is the sole fetcher.
actor SourceBackoff {
    static let shared = SourceBackoff()

    private var failures: [String: (count: Int, until: Date)] = [:]

    /// Throws while a previous auth failure's backoff window is still open — callers
    /// check this BEFORE attempting a vendor login.
    func checkPermission(_ key: String) throws {
        if let f = failures[key], Date() < f.until {
            let seconds = max(1, Int(f.until.timeIntervalSinceNow.rounded()))
            throw StandaloneError.auth(
                "backing off after \(f.count) failed sign-in\(f.count == 1 ? "" : "s") — retrying in \(seconds)s")
        }
    }

    /// Doubles the wait per consecutive failure: 30 s, 1 m, 2 m, … capped at 30 m.
    func recordAuthFailure(_ key: String) {
        let count = (failures[key]?.count ?? 0) + 1
        let delay = min(30 * pow(2, Double(count - 1)), 1800)
        failures[key] = (count, Date().addingTimeInterval(delay))
    }

    func recordSuccess(_ key: String) {
        failures[key] = nil
    }

    #if DEBUG
    /// Test hook: clear all state between test cases.
    func reset() { failures.removeAll() }
    #endif
}

/// The on-device analytics engine — the seam that keeps the Rust xcframework out of
/// the extensions. `Shared` code (APIClient) calls through this protocol; only the
/// APP target links the FFI and registers its `RustAnalytics` at launch. Extension
/// processes leave it nil and never compute analytics (they are cache-only in a
/// local-analytics source).
protocol AnalyticsEngine: Sendable {
    /// The `/api/v4/analytics`-shaped JSON for `[{date,mgdl}]` readings JSON.
    func analyticsJSON(readingsJSON: String, hours: Int, tzOffsetMin: Int) throws -> Data
    /// The `/api/v4/agp`-shaped JSON for `[{date,mgdl}]` readings JSON.
    func agpJSON(readingsJSON: String, days: Int, binMinutes: Int, tzOffsetMin: Int) throws -> Data
    /// Parse a Clarity/LibreView CSV export → `{entries:[{date,mgdl}],…}` JSON.
    func importGlucoseCSV(text: String, tzOffsetMin: Int) throws -> Data
}

enum LocalAnalytics {
    /// Registered once at app launch (before any fetch); nil in extensions.
    static var engine: (any AnalyticsEngine)?
}

/// The single `URLSession` round-trip every standalone connector's IO edge shares.
///
/// Dexcom, LibreLinkUp, and Nightscout each used to rebuild the identical boilerplate —
/// `URL(string:)` guard → `URLRequest(timeoutInterval: 20)` → set header pairs →
/// `session.data(for:)` → `(resp as? HTTPURLResponse)?.statusCode ?? 0`. That lived in
/// three files, so a change to the shared convention (timeout, the status-0 fallback, a
/// default header) meant editing three copies and every new source re-derived it. This is
/// the one place that convention now lives.
///
/// The `session` is a *parameter*, not a constant, because it's the one axis the connectors
/// genuinely differ on: Nightscout hits a **user-supplied** host and must use its
/// redirect-refusing session (a `302` carrying the api-secret to a loopback / metadata host
/// would defeat the SSRF guard), while the hardcoded-host vendor clients use `.shared` and
/// follow redirects — matching the Rust reference's `follow_redirects` default for those.
enum HTTPEdge {
    /// Run one request and normalise the response to `(status, body)`. A non-HTTP response
    /// collapses to status `0`, exactly as each connector's prior `?? 0` did (callers treat
    /// any non-2xx, including 0, as a failure).
    static func send(_ urlString: String,
                     method: String = "GET",
                     headers: [(String, String)] = [],
                     body: Data? = nil,
                     session: URLSession = .shared,
                     timeout: TimeInterval = 20) async throws -> (status: Int, body: Data) {
        guard let url = URL(string: urlString) else {
            throw StandaloneError.proto("bad request URL")
        }
        var req = URLRequest(url: url, timeoutInterval: timeout)
        req.httpMethod = method
        for (name, value) in headers { req.setValue(value, forHTTPHeaderField: name) }
        req.httpBody = body
        let (data, resp) = try await session.data(for: req)
        return ((resp as? HTTPURLResponse)?.statusCode ?? 0, data)
    }
}
