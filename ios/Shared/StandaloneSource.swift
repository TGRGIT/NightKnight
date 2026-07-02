import CoreFoundation
import Foundation

/// `JSONSerialization` backs a JSON boolean with an `NSNumber` (`__NSCFBoolean`), and
/// Foundation's `NSNumber as? Int`/`as? Int64` bridging silently coerces it to `1`/`0`
/// — unlike Rust's `serde_json::Value::as_i64()`, which returns `None` for
/// `Value::Bool`. Every connector parses vendor-controlled JSON (Nightscout is a
/// user-supplied, possibly hostile origin), so a malformed `{"sgv": true}` must be
/// discarded like the Rust reference discards it, not silently read as `mgdl: 1`.
/// Use these in place of a bare `as? Int`/`as? Int64` on any vendor numeric field.
func jsonInt(_ v: Any?) -> Int? {
    guard let n = v as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
    return n.intValue
}

func jsonInt64(_ v: Any?) -> Int64? {
    guard let n = v as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
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
