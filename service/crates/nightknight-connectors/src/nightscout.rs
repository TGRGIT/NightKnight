//! Nightscout source connector — pull entries from another Nightscout (or NightKnight)
//! instance's `v1` API and ingest them, deduped, into the canonical store.
//!
//! Unlike the Dexcom/LibreLinkUp connectors, which talk to a *hard-coded* vendor host,
//! this fetches a **user-supplied URL** — so it carries an SSRF guard ([`is_safe_base`])
//! that only allows `https` origins to public hosts. The source `_id` is dropped from
//! each reading so NightKnight's own content dedup (`date|type|device`) governs
//! re-imports, exactly like a re-fetched live connector overlap.

use serde_json::Value;

use nightknight_core::Direction;

use crate::{CgmSample, Connector, ConnectorError, Http, HttpReq};

/// Trim a trailing slash and any `/api/...` suffix so a pasted *full endpoint* URL
/// still resolves to the instance origin (`https://host[:port]`).
pub fn normalize_base(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    match u.find("/api/") {
        Some(i) => u[..i].to_string(),
        None => u.to_string(),
    }
}

/// Build the read URL: `{base}/api/v1/entries/sgv.json?count=N` (newest-first).
pub fn read_url(base: &str, count: i64) -> String {
    format!(
        "{}/api/v1/entries/sgv.json?count={}",
        normalize_base(base),
        count.clamp(1, 131_072)
    )
}

/// Build a paginated read URL for the history backfill: the newest `count` readings with
/// `date < before_ms`, so successive calls (each advancing `before_ms` to the oldest seen)
/// walk the full history backward one bounded page at a time. The Nightscout `find` filter
/// is percent-encoded (`find[date][$lt]`). `before_ms = i64::MAX` ⇒ "from the most recent".
pub fn read_url_before(base: &str, count: i64, before_ms: i64) -> String {
    format!(
        "{}/api/v1/entries/sgv.json?count={}&find%5Bdate%5D%5B%24lt%5D={}",
        normalize_base(base),
        count.clamp(1, 131_072),
        before_ms.max(0)
    )
}

impl NightscoutConnector {
    /// Fetch one history page: up to `count` readings older than `before_ms` (newest-first).
    /// Returns a [`HistoryPage`] so the caller can paginate on the **raw** page size rather
    /// than the filtered sample count (see [`HistoryPage`]). SSRF-guarded and
    /// redirect-refusing exactly like [`fetch_recent`](NightscoutConnector::fetch_recent).
    pub async fn fetch_before(
        &self,
        http: Http<'_>,
        before_ms: i64,
        count: i64,
    ) -> Result<HistoryPage, ConnectorError> {
        if !is_safe_base(&self.base_url) {
            return Err(ConnectorError::Protocol(
                "nightscout url must be https to a public host".into(),
            ));
        }
        let url = read_url_before(&self.base_url, count, before_ms);
        let resp = http.send(HttpReq::get(url, headers(&self.secret)).no_redirects()).await?;
        if !resp.is_success() {
            return Err(ConnectorError::Protocol(format!(
                "nightscout history read failed ({})",
                resp.status
            )));
        }
        parse_history_page(&resp.body)
    }
}

/// SSRF guard: only `https` origins to non-internal hosts may be fetched. The URL is
/// user-supplied, so we refuse loopback / link-local / RFC-1918 private addresses and
/// any non-https scheme. (Not a substitute for network egress controls, but it blocks
/// the obvious metadata-endpoint / internal-service targets.)
pub fn is_safe_base(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix("https://") else {
        return false;
    };
    // The authority is everything before the path/query/fragment.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Strip any `user:pass@` userinfo — the real host is after the LAST '@'. Without this
    // `https://x@169.254.169.254/` would parse the host as `x@169.254.169.254` and slip
    // past the prefix blocks while the HTTP client still connects to the metadata IP.
    let hostport = authority.rsplit('@').next().unwrap_or("");
    // Strip the port (IPv6 literals are bracketed and rejected wholesale below).
    let host = hostport.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
    if host.is_empty() || host.contains('@') || host.contains('[') || host.contains(']') {
        return false;
    }
    const BLOCKED_PREFIX: &[&str] = &["localhost", "127.", "10.", "192.168.", "169.254.", "0.", "::1"];
    if BLOCKED_PREFIX.iter().any(|p| host.starts_with(p)) {
        return false;
    }
    // Known cloud metadata / internal service hostnames.
    const BLOCKED_HOST: &[&str] = &["metadata.google.internal", "metadata", "instance-data"];
    if BLOCKED_HOST.contains(&host.as_str()) {
        return false;
    }
    // RFC-1918 172.16/12 and CGNAT 100.64/10 (parse the second octet).
    for (prefix, lo, hi) in [("172.", 16u8, 31u8), ("100.", 64u8, 127u8)] {
        if let Some(rest) = host.strip_prefix(prefix) {
            if let Some(o) = rest.split('.').next().and_then(|s| s.parse::<u8>().ok()) {
                if (lo..=hi).contains(&o) {
                    return false;
                }
            }
        }
    }
    // Reject non-dotted-decimal IP encodings that smuggle a blocked address past the
    // prefix checks: a bare decimal integer (2130706433 = 127.0.0.1), a hex literal
    // (0x7f000001), or octal octets with a leading zero (0177.0.0.1). A legitimate
    // hostname is never an all-digit string or a 0x literal.
    let labels: Vec<&str> = host.split('.').collect();
    let numeric_smuggle = host.starts_with("0x")
        || host.chars().all(|c| c.is_ascii_digit())
        || (labels.len() == 4
            && labels.iter().all(|l| l.parse::<u32>().is_ok())
            && labels.iter().any(|l| l.len() > 1 && l.starts_with('0')));
    if numeric_smuggle {
        return false;
    }
    true
}

fn headers(secret: &str) -> Vec<(String, String)> {
    vec![
        ("api-secret".into(), secret.into()),
        ("accept".into(), "application/json".into()),
        ("user-agent".into(), "NightKnight-import/1.0".into()),
    ]
}

/// A parsed history page plus the raw bookkeeping the backward backfill needs to
/// paginate correctly: how many records the server actually returned **before** the
/// `sgv`/`date` filtering below, and the oldest raw `date` seen.
///
/// The distinction matters: the backfill walks history one fixed-size page at a time and
/// must decide "have I reached the start of history?". That answer is "the server returned
/// fewer than a full page", which is a property of the **raw** array — *not* of the
/// filtered sample count. A single dropped row (an `sgv ≤ 0` error code, a record missing
/// `date`) inside an otherwise-full page would shrink `samples` below the requested count
/// and, if used as the stop signal, look like end-of-history — silently abandoning every
/// older reading. `raw_min_date` lets the cursor advance past a page even when it filtered
/// down to nothing, so an all-error-code page can't stall the walk either.
pub struct HistoryPage {
    /// The usable readings parsed from the page.
    pub samples: Vec<CgmSample>,
    /// Number of records in the server's JSON array, before any filtering.
    pub raw_len: usize,
    /// Oldest `date` (epoch ms) across **all** raw records, including ones that didn't
    /// yield a usable sample. `None` if no record carried a numeric `date`.
    pub raw_min_date: Option<i64>,
}

/// Parse a Nightscout `/entries` JSON array into a [`HistoryPage`] — the usable
/// [`CgmSample`]s plus the raw page bookkeeping (see [`HistoryPage`]). Non-`sgv` records
/// and any reading without a plausible numeric `sgv` + `date` are skipped. The source
/// `_id` is intentionally dropped so our `date|type|device` dedup owns re-imports.
pub fn parse_history_page(body: &[u8]) -> Result<HistoryPage, ConnectorError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| ConnectorError::Parse(e.to_string()))?;
    let arr = v
        .as_array()
        .ok_or_else(|| ConnectorError::Parse("expected a JSON array of entries".into()))?;
    let raw_len = arr.len();
    let mut raw_min_date: Option<i64> = None;
    let mut out = Vec::with_capacity(arr.len());
    for it in arr {
        // Track the oldest raw timestamp regardless of whether the record yields a sample,
        // so the cursor can always advance past this page (even an all-filtered one).
        if let Some(d) = it.get("date").and_then(|v| v.as_i64()) {
            raw_min_date = Some(raw_min_date.map_or(d, |m| m.min(d)));
        }
        if let Some(t) = it.get("type").and_then(|t| t.as_str()) {
            if t != "sgv" {
                continue; // skip cal/mbg/etc.
            }
        }
        let Some(mgdl) = it
            .get("sgv")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64)))
            .filter(|&m| m > 0)
        else {
            continue;
        };
        let Some(date_ms) = it.get("date").and_then(|v| v.as_i64()) else {
            continue;
        };
        let direction = it
            .get("direction")
            .and_then(|d| d.as_str())
            .and_then(|s| serde_json::from_value::<Direction>(Value::String(s.to_string())).ok())
            .filter(|d| d.is_arrow());
        let device = it
            .get("device")
            .and_then(|d| d.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("nightscout")
            .to_string();
        out.push(CgmSample { date_ms, mgdl, direction, device });
    }
    Ok(HistoryPage { samples: out, raw_len, raw_min_date })
}

/// Parse a Nightscout `/entries` JSON array into [`CgmSample`]s, discarding the raw page
/// bookkeeping. Used by the recent-window pull, where the caller doesn't paginate.
pub fn parse_entries(body: &[u8]) -> Result<Vec<CgmSample>, ConnectorError> {
    Ok(parse_history_page(body)?.samples)
}

/// A configured Nightscout source connector (origin URL + api-secret).
pub struct NightscoutConnector {
    pub base_url: String,
    pub secret: String,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Connector for NightscoutConnector {
    fn name(&self) -> &'static str {
        "nightscout"
    }

    async fn fetch_recent(
        &self,
        http: Http<'_>,
        minutes: i64,
    ) -> Result<Vec<CgmSample>, ConnectorError> {
        if !is_safe_base(&self.base_url) {
            return Err(ConnectorError::Protocol(
                "nightscout url must be https to a public host".into(),
            ));
        }
        // Nightscout pages by count (newest-first), not time, so map the lookback
        // window to a count (~1 reading / 5 min) with a sensible floor; dedup makes any
        // overlap harmless, and the daily "all" sync (a huge window) backfills history.
        let count = (minutes / 5).clamp(12, 131_072);
        // Refuse redirects: the SSRF guard only vetted `base_url`, so a malicious source
        // must not be able to 302 us (with the api-secret) to an internal address.
        let resp = http
            .send(HttpReq::get(read_url(&self.base_url, count), headers(&self.secret)).no_redirects())
            .await?;
        if !resp.is_success() {
            return Err(ConnectorError::Protocol(format!(
                "nightscout read failed ({})",
                resp.status
            )));
        }
        parse_entries(&resp.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical payloads live under `ios/Tests/Fixtures/` and are shared byte-for-byte
    /// with the Swift port's tests (`NightKnightSourcesTests`) — the two parsers cannot
    /// drift silently.
    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../ios/Tests/Fixtures/",
                $name
            ))
        };
    }

    #[test]
    fn normalizes_base_and_builds_url() {
        assert_eq!(normalize_base("https://x.cooney.be/"), "https://x.cooney.be");
        // A pasted full endpoint URL is reduced to the origin.
        assert_eq!(
            normalize_base("https://x.cooney.be/api/v1/entries/sgv?count=100"),
            "https://x.cooney.be"
        );
        assert_eq!(
            read_url("https://x.cooney.be", 50),
            "https://x.cooney.be/api/v1/entries/sgv.json?count=50"
        );
        // Count is clamped to a sane ceiling.
        assert!(read_url("https://x.cooney.be", 9_999_999).ends_with("count=131072"));
    }

    #[test]
    fn builds_paginated_history_url() {
        // The `find[date][$lt]=ms` filter is percent-encoded so successive pages can walk
        // the history backward.
        assert_eq!(
            read_url_before("https://x.cooney.be", 2000, 1_700_000_000_000),
            "https://x.cooney.be/api/v1/entries/sgv.json?count=2000&find%5Bdate%5D%5B%24lt%5D=1700000000000"
        );
        // The first page (cursor = i64::MAX) is "from the most recent", and a negative
        // cursor is floored to 0 (no panic / no negative in the URL).
        assert!(read_url_before("https://x.cooney.be", 2000, i64::MAX).contains("%24lt%5D=9223372036854775807"));
        assert!(read_url_before("https://x.cooney.be", 2000, -5).ends_with("%24lt%5D=0"));
    }

    /// The full allow/deny table is the shared fixture `ssrf-table.json` — it is the
    /// spec for the Swift port's `isSafeBase`, asserted from both languages.
    #[test]
    fn ssrf_guard_matches_the_shared_table() {
        let table: Vec<serde_json::Value> =
            serde_json::from_slice(fixture!("ssrf-table.json")).unwrap();
        assert!(table.len() >= 24, "SSRF table lost rows");
        for row in &table {
            let url = row["url"].as_str().unwrap();
            let safe = row["safe"].as_bool().unwrap();
            assert_eq!(is_safe_base(url), safe, "is_safe_base({url:?}) should be {safe}");
        }
    }

    #[test]
    fn ssrf_guard_blocks_internal_and_non_https() {
        assert!(is_safe_base("https://nightscout.example.com"));
        assert!(is_safe_base("https://e77f02d3-4537-46df-b77e-1fcb254013fb.cooney.be"));
        assert!(!is_safe_base("http://nightscout.example.com")); // not https
        assert!(!is_safe_base("https://localhost:1337"));
        assert!(!is_safe_base("https://127.0.0.1"));
        assert!(!is_safe_base("https://10.0.0.5"));
        assert!(!is_safe_base("https://192.168.1.10"));
        assert!(!is_safe_base("https://169.254.169.254")); // cloud metadata
        assert!(!is_safe_base("https://172.16.0.1"));
        assert!(is_safe_base("https://172.15.0.1")); // just outside the private block
        assert!(!is_safe_base("https://[::1]"));
        assert!(!is_safe_base("ftp://x"));
        // Bypass attempts that must NOT slip past the guard:
        assert!(!is_safe_base("https://anything@169.254.169.254/")); // userinfo masks the host
        assert!(!is_safe_base("https://user:pass@127.0.0.1/")); // userinfo + loopback
        assert!(!is_safe_base("https://2130706433/")); // decimal-packed 127.0.0.1
        assert!(!is_safe_base("https://0x7f000001/")); // hex-packed 127.0.0.1
        assert!(!is_safe_base("https://0177.0.0.1/")); // octal-octet 127.0.0.1
        assert!(!is_safe_base("https://100.64.0.1")); // CGNAT
        assert!(!is_safe_base("https://100.127.255.255")); // CGNAT upper
        assert!(is_safe_base("https://100.63.0.1")); // just below CGNAT — allowed
        assert!(is_safe_base("https://100.128.0.1")); // just above CGNAT — allowed
        assert!(!is_safe_base("https://metadata.google.internal/")); // metadata hostname
        assert!(is_safe_base("https://8.8.8.8")); // ordinary public dotted IP — allowed
        assert!(is_safe_base("https://user@nightscout.example.com")); // userinfo + legit host is fine
    }

    #[test]
    fn parses_a_real_entries_payload() {
        // The exact shape returned by the live endpoint (shared fixture).
        let body = fixture!("nightscout-entries.json");
        let samples = parse_entries(body).unwrap();
        assert_eq!(samples.len(), 2, "the cal record and the 0-sgv reading are skipped");
        assert_eq!(samples[0].mgdl, 91);
        assert_eq!(samples[0].date_ms, 1782404097000);
        assert_eq!(samples[0].direction, Some(Direction::Flat));
        assert_eq!(samples[0].device, "nightscout-librelink-up");
        assert_eq!(samples[1].direction, Some(Direction::FortyFiveUp));
        // No `_id` rides along — content dedup (date|type|device) governs.
        let entry = samples[0].to_entry_json();
        assert!(entry.get("_id").is_none() && entry.get("identifier").is_none());
        assert_eq!(entry["type"], "sgv");
        assert_eq!(entry["sgv"], 91);
    }

    #[test]
    fn empty_or_garbage_body_is_handled() {
        assert_eq!(parse_entries(b"[]").unwrap().len(), 0);
        assert!(parse_entries(b"not json").is_err());
        assert!(parse_entries(b"{\"not\":\"an array\"}").is_err());
    }

    /// The history page reports the RAW record count (before filtering) and the oldest raw
    /// `date`, so the backfill can tell "a full page that filtered down" from "a short page
    /// (end of history)". The payload has 4 raw records but only 2 usable sgv samples; the
    /// page must report `raw_len == 4`, not 2 — otherwise one error-coded row anywhere in a
    /// full backfill page would look like end-of-history and abandon all older readings.
    #[test]
    fn history_page_reports_raw_count_and_oldest_date() {
        let body = fixture!("nightscout-history-page.json");
        let page = parse_history_page(body).unwrap();
        assert_eq!(page.raw_len, 4, "raw count counts every record, before filtering");
        assert_eq!(page.samples.len(), 2, "only the two real sgv readings are usable");
        assert_eq!(
            page.raw_min_date,
            Some(1782403800000),
            "oldest raw date includes the filtered-out rows, so the cursor can still advance"
        );
    }

    /// A page whose rows are ALL error codes / non-sgv still surfaces a non-empty raw count
    /// and an oldest date, so the backfill advances past it instead of stalling forever.
    #[test]
    fn all_filtered_page_still_reports_raw_progress() {
        let body = fixture!("nightscout-history-page-filtered.json");
        let page = parse_history_page(body).unwrap();
        assert_eq!(page.raw_len, 2);
        assert!(page.samples.is_empty());
        assert_eq!(page.raw_min_date, Some(1782403800000));
    }
}
