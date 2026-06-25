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

/// Parse a Nightscout `/entries` JSON array into [`CgmSample`]s. Non-`sgv` records and
/// any reading without a plausible numeric `sgv` + `date` are skipped. The source `_id`
/// is intentionally dropped so our `date|type|device` dedup owns re-imports.
pub fn parse_entries(body: &[u8]) -> Result<Vec<CgmSample>, ConnectorError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| ConnectorError::Parse(e.to_string()))?;
    let arr = v
        .as_array()
        .ok_or_else(|| ConnectorError::Parse("expected a JSON array of entries".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for it in arr {
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
    Ok(out)
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
        let resp = http
            .send(HttpReq::get(read_url(&self.base_url, count), headers(&self.secret)))
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
        // The exact shape returned by the live endpoint.
        let body = br#"[
            {"_id":"6a3d54045da0bda161923313","type":"sgv","date":1782404097000,"dateString":"2026-06-25T16:14:57.000Z","device":"nightscout-librelink-up","direction":"Flat","sgv":91,"utcOffset":0,"mills":1782404097000},
            {"_id":"x","type":"sgv","date":1782403977000,"sgv":90,"direction":"FortyFiveUp","device":"nightscout-librelink-up"},
            {"_id":"y","type":"cal","date":1782403900000,"sgv":0},
            {"_id":"z","type":"sgv","date":1782403800000,"sgv":0}
        ]"#;
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
}
