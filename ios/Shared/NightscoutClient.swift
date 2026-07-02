import Foundation

// Nightscout source connector — a 1:1 port of the Rust reference
// (`service/crates/nightknight-connectors/src/nightscout.rs`): pull entries from
// another Nightscout (or NightKnight) instance's `v1` API.
//
// Unlike the Dexcom/LibreLinkUp connectors, which talk to a *hard-coded* vendor host,
// this fetches a **user-supplied URL** — so it carries an SSRF guard (`isSafeBase`)
// that only allows `https` origins to public hosts, and it refuses redirects. The
// source `_id` is dropped from each reading so NightKnight's own content dedup
// (`date|type|device`) governs re-imports, exactly like a re-fetched live connector
// overlap.
//
// All the request/response shaping is pure static functions (unit-tested against the
// same fixture bytes as the Rust tests); only `fetchRecent`/`backfill` touch
// URLSession. No session cache and no `SourceBackoff` here: this is the user's own
// instance with a static api-secret — there is no login to storm and no account to
// lock, so every call is a single plain GET.

/// A configured Nightscout source client (origin URL + api-secret, supplied by
/// `StandaloneSources.make`). Mirrors Rust `NightscoutConnector { base_url, secret }`.
struct NightscoutClient: StandaloneSource {
    let baseURL: String
    let secret: String

    // MARK: - Pure request/response shaping (mirrors the Rust free functions)

    /// Trim a trailing slash and any `/api/...` suffix so a pasted *full endpoint* URL
    /// still resolves to the instance origin (`https://host[:port]`).
    /// Rust `normalize_base`.
    static func normalizeBase(_ url: String) -> String {
        var u = url.trimmingCharacters(in: .whitespacesAndNewlines)
        while u.hasSuffix("/") { u.removeLast() }
        if let i = u.range(of: "/api/") {
            return String(u[..<i.lowerBound])
        }
        return u
    }

    /// Build the read URL: `{base}/api/v1/entries/sgv.json?count=N` (newest-first).
    /// Rust `read_url`.
    static func readURL(base: String, count: Int64) -> String {
        "\(normalizeBase(base))/api/v1/entries/sgv.json?count=\(min(max(count, 1), 131_072))"
    }

    /// Build a paginated read URL for the history backfill: the newest `count` readings
    /// with `date < beforeMs`, so successive calls (each advancing `beforeMs` to the
    /// oldest seen) walk the full history backward one bounded page at a time. The
    /// Nightscout `find` filter is percent-encoded (`find[date][$lt]`).
    /// `beforeMs = Int64.max` ⇒ "from the most recent". Rust `read_url_before`.
    static func readURLBefore(base: String, count: Int64, beforeMs: Int64) -> String {
        "\(readURL(base: base, count: count))&find%5Bdate%5D%5B%24lt%5D=\(max(beforeMs, 0))"
    }

    /// SSRF guard: only `https` origins to non-internal hosts may be fetched. The URL
    /// is user-supplied, so we refuse loopback / link-local / RFC-1918 private
    /// addresses and any non-https scheme. (Not a substitute for network egress
    /// controls, but it blocks the obvious metadata-endpoint / internal-service
    /// targets.) Rust `is_safe_base`; the allow/deny spec is the shared fixture
    /// `ssrf-table.json`, asserted from both languages.
    static func isSafeBase(_ url: String) -> Bool {
        let trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("https://") else { return false }
        let rest = trimmed.dropFirst("https://".count)
        // The authority is everything before the path/query/fragment.
        let authority = rest.prefix { $0 != "/" && $0 != "?" && $0 != "#" }
        // Strip any `user:pass@` userinfo — the real host is after the LAST '@'.
        // Without this `https://x@169.254.169.254/` would parse the host as
        // `x@169.254.169.254` and slip past the prefix blocks while the HTTP client
        // still connects to the metadata IP.
        let hostport = authority.split(separator: "@", omittingEmptySubsequences: false).last ?? ""
        // Strip the port (IPv6 literals are bracketed and rejected wholesale below).
        let host = String(hostport.split(separator: ":", omittingEmptySubsequences: false).first ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
        if host.isEmpty || host.contains("@") || host.contains("[") || host.contains("]") {
            return false
        }
        let blockedPrefixes = ["localhost", "127.", "10.", "192.168.", "169.254.", "0.", "::1"]
        if blockedPrefixes.contains(where: host.hasPrefix) {
            return false
        }
        // Known cloud metadata / internal service hostnames.
        let blockedHosts = ["metadata.google.internal", "metadata", "instance-data"]
        if blockedHosts.contains(host) {
            return false
        }
        // RFC-1918 172.16/12 and CGNAT 100.64/10 (parse the second octet).
        for (prefix, lo, hi) in [("172.", UInt8(16), UInt8(31)), ("100.", UInt8(64), UInt8(127))] {
            if host.hasPrefix(prefix),
               let octet = host.dropFirst(prefix.count)
                   .split(separator: ".", omittingEmptySubsequences: false)
                   .first.flatMap({ UInt8($0) }),
               (lo...hi).contains(octet) {
                return false
            }
        }
        // Reject non-dotted-decimal IP encodings that smuggle a blocked address past
        // the prefix checks: a bare decimal integer (2130706433 = 127.0.0.1), a hex
        // literal (0x7f000001), or octal octets with a leading zero (0177.0.0.1). A
        // legitimate hostname is never an all-digit string or a 0x literal.
        let labels = host.split(separator: ".", omittingEmptySubsequences: false)
        let numericSmuggle = host.hasPrefix("0x")
            || host.allSatisfy { ("0"..."9").contains($0) }
            || (labels.count == 4
                && labels.allSatisfy { UInt32($0) != nil }
                && labels.contains { $0.count > 1 && $0.hasPrefix("0") })
        if numericSmuggle {
            return false
        }
        return true
    }

    /// Every Nightscout GET carries exactly these three headers. Rust `headers`.
    static func headers(secret: String) -> [(String, String)] {
        [("api-secret", secret),
         ("accept", "application/json"),
         ("user-agent", "NightKnight-import/1.0")]
    }

    /// A parsed history page plus the raw bookkeeping the backward backfill needs to
    /// paginate correctly: how many records the server actually returned **before**
    /// the `sgv`/`date` filtering, and the oldest raw `date` seen.
    ///
    /// The distinction matters: the backfill walks history one fixed-size page at a
    /// time and must decide "have I reached the start of history?". That answer is
    /// "the server returned fewer than a full page", which is a property of the
    /// **raw** array — *not* of the filtered sample count. A single dropped row (an
    /// `sgv ≤ 0` error code, a record missing `date`) inside an otherwise-full page
    /// would shrink `samples` below the requested count and, if used as the stop
    /// signal, look like end-of-history — silently abandoning every older reading.
    /// `rawMinDate` lets the cursor advance past a page even when it filtered down to
    /// nothing, so an all-error-code page can't stall the walk either.
    /// Rust `HistoryPage`.
    struct HistoryPage: Sendable {
        /// The usable readings parsed from the page.
        let samples: [CgmSample]
        /// Number of records in the server's JSON array, before any filtering.
        let rawLen: Int
        /// Oldest `date` (epoch ms) across **all** raw records, including ones that
        /// didn't yield a usable sample. `nil` if no record carried a numeric `date`.
        let rawMinDate: Int64?
    }

    /// Parse a Nightscout `/entries` JSON array into a `HistoryPage` — the usable
    /// `CgmSample`s plus the raw page bookkeeping (see `HistoryPage`). Non-`sgv`
    /// records and any reading without a plausible numeric `sgv` + `date` are skipped.
    /// The source `_id` is intentionally dropped so our `date|type|device` content
    /// dedup owns re-imports. Rust `parse_history_page` (JSONSerialization, because
    /// this is heterogeneous vendor JSON).
    static func parseHistoryPage(_ body: Data) throws -> HistoryPage {
        let parsed: Any
        do {
            parsed = try JSONSerialization.jsonObject(with: body)
        } catch {
            throw StandaloneError.parse(error.localizedDescription)
        }
        guard let arr = parsed as? [Any] else {
            throw StandaloneError.parse("expected a JSON array of entries")
        }
        var rawMinDate: Int64? = nil
        var out: [CgmSample] = []
        out.reserveCapacity(arr.count)
        for item in arr {
            let it = item as? [String: Any] ?? [:]
            let rawDate = jsonInt64(it["date"])
            // Track the oldest raw timestamp regardless of whether the record yields
            // a sample, so the cursor can always advance past this page (even an
            // all-filtered one).
            if let d = rawDate {
                rawMinDate = rawMinDate.map { min($0, d) } ?? d
            }
            if let t = it["type"] as? String, t != "sgv" {
                continue // skip cal/mbg/etc. (a record with NO type falls through)
            }
            // `sgv` may arrive as an integer or a float; ≤ 0 is a sensor error code,
            // not a reading (like Rust's `as_i64().or_else(as_f64().round())`).
            let sgv = jsonInt(it["sgv"]) ?? jsonDouble(it["sgv"]).map { Int($0.rounded()) }
            guard let mgdl = sgv, mgdl > 0 else {
                continue
            }
            guard let dateMs = rawDate else {
                continue
            }
            // Only the seven movement arrows survive (Rust filters `is_arrow()`);
            // Swift's `TrendDirection(rawValue: "NONE")` yields `.none`, which must
            // also drop to nil — no arrow.
            let direction = (it["direction"] as? String)
                .flatMap(TrendDirection.init(rawValue:))
                .flatMap { $0 == TrendDirection.none ? nil : $0 }
            let device = (it["device"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "nightscout"
            out.append(CgmSample(dateMs: dateMs, mgdl: mgdl, direction: direction, device: device))
        }
        return HistoryPage(samples: out, rawLen: arr.count, rawMinDate: rawMinDate)
    }

    /// Parse a Nightscout `/entries` JSON array into `CgmSample`s, discarding the raw
    /// page bookkeeping. Used by the recent-window pull, where the caller doesn't
    /// paginate. Rust `parse_entries`.
    static func parseEntries(_ body: Data) throws -> [CgmSample] {
        try parseHistoryPage(body).samples
    }

    // MARK: - The IO edge

    /// Refuses 3xx redirects (Rust `HttpReq::no_redirects`): the SSRF guard only
    /// vetted the *original* host, so a malicious source must not be able to `302`
    /// the request — carrying its api-secret — to a loopback / link-local / metadata
    /// target. With redirects refused, any 3xx comes back as-is and surfaces as a
    /// non-2xx error in the caller instead of being followed.
    private final class RedirectRefuser: NSObject, URLSessionTaskDelegate {
        func urlSession(_ session: URLSession, task: URLSessionTask,
                        willPerformHTTPRedirection response: HTTPURLResponse,
                        newRequest request: URLRequest,
                        completionHandler: @escaping (URLRequest?) -> Void) {
            completionHandler(nil)
        }
    }

    private static let session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 20
        return URLSession(configuration: config, delegate: RedirectRefuser(), delegateQueue: nil)
    }()

    private static func get(_ urlString: String, secret: String) async throws -> (status: Int, body: Data) {
        guard let url = URL(string: urlString) else {
            throw StandaloneError.proto("bad request URL")
        }
        var req = URLRequest(url: url, timeoutInterval: 20)
        req.httpMethod = "GET"
        for (name, value) in headers(secret: secret) {
            req.setValue(value, forHTTPHeaderField: name)
        }
        let (data, resp) = try await session.data(for: req)
        return ((resp as? HTTPURLResponse)?.statusCode ?? 0, data)
    }

    func fetchRecent() async throws -> [CgmSample] {
        guard Self.isSafeBase(baseURL) else {
            throw StandaloneError.proto("nightscout url must be https to a public host")
        }
        // Rust's `fetch_recent` takes `minutes` from the runtime; the app always
        // wants the vendor's full rolling day (1440). Nightscout pages by count
        // (newest-first), not time, so map the lookback window to a count
        // (~1 reading / 5 min) with a sensible floor; dedup makes any overlap
        // harmless, and `backfill()` owns full history.
        let minutes: Int64 = 1440
        let count = min(max(minutes / 5, 12), 131_072)
        let resp = try await Self.get(Self.readURL(base: baseURL, count: count), secret: secret)
        guard (200..<300).contains(resp.status) else {
            throw StandaloneError.proto("nightscout read failed (\(resp.status))")
        }
        // Nightscout returns newest-first; the protocol contract is newest-last.
        return try Self.parseEntries(resp.body).sorted { $0.dateMs < $1.dateMs }
    }

    func backfill() async throws -> [CgmSample] {
        // The full-history walk (Rust `fetch_before`, driven the same way the server
        // runtime drives it): newest `pageSize` readings with `date < cursor`, then
        // advance the cursor to the oldest RAW date seen. See `HistoryPage` for why
        // the stop signal is the RAW page size, not the filtered sample count.
        var cursor = Int64.max
        let pageSize: Int64 = 4000
        var all: [CgmSample] = []
        // 500 pages × 4000 entries ≈ 2M readings (~19 years at 5 min) — a
        // runaway-server stop, not a practical limit.
        for _ in 0..<500 {
            guard Self.isSafeBase(baseURL) else {
                throw StandaloneError.proto("nightscout url must be https to a public host")
            }
            let resp = try await Self.get(
                Self.readURLBefore(base: baseURL, count: pageSize, beforeMs: cursor),
                secret: secret)
            guard (200..<300).contains(resp.status) else {
                throw StandaloneError.proto("nightscout history read failed (\(resp.status))")
            }
            let page = try Self.parseHistoryPage(resp.body)
            all.append(contentsOf: page.samples)
            if page.rawLen < Int(pageSize) {
                break // a short page = the start of history
            }
            guard let oldest = page.rawMinDate else {
                break // no raw date anywhere on the page — the cursor cannot advance
            }
            cursor = oldest
        }
        return all.sorted { $0.dateMs < $1.dateMs }
    }
}
