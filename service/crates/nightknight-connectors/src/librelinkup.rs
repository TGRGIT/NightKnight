//! LibreLinkUp connector.
//!
//! Implements the LibreLinkUp cloud flow (the same one the LibreLinkUp mobile app and
//! community bridges use): authenticate, list connections (the followed patient), and
//! read the latest measurement + recent graph. The protocol shaping is pure and
//! tested; network calls run through the injected [`HttpClient`].
//!
//! Note: these are **unofficial** endpoints. The `product`/`version` headers and the
//! hashed `account-id` header reflect the currently-known requirements and may need
//! updating if the vendor changes them.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use nightknight_core::{timeutil, Direction};

use crate::{to_hex, CgmSample, Connector, ConnectorError, Http, HttpReq};

pub const DEFAULT_BASE: &str = "https://api.libreview.io";
const PRODUCT: &str = "llu.android";
// LibreView gates the data endpoints (`/llu/connections`, `/graph`) on a minimum
// client version: an older value logs in fine but then gets `403 {"status":920,
// "data":{"minimumVersion":"4.16.0"}}`. Keep this at/above that floor.
const VERSION: &str = "4.16.0";

/// Regional API base after a login redirect (e.g. region `"eu"` → `api-eu.libreview.io`).
pub fn regional_base(region: &str) -> String {
    format!("https://api-{}.libreview.io", region.trim().to_ascii_lowercase())
}

/// LibreView region codes are short alphanumeric tokens (e.g. `"eu"`, `"us"`, `"de"`).
/// Validate before interpolating into the API host so a malformed/hostile value (a
/// `/`, `@`, `.` …) can't repoint the request elsewhere via [`regional_base`].
pub fn is_valid_region(region: &str) -> bool {
    let r = region.trim();
    !r.is_empty() && r.len() <= 16 && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// SHA-256 hex of the user id — the value LibreLinkUp expects in the `account-id`
/// header on authenticated requests.
pub fn account_id_hash(user_id: &str) -> String {
    to_hex(&Sha256::digest(user_id.as_bytes()))
}

/// Standard headers; pass the bearer token and account-id hash once authenticated.
pub fn headers(token: Option<&str>, account_id: Option<&str>) -> Vec<(String, String)> {
    let mut h = vec![
        ("product".into(), PRODUCT.into()),
        ("version".into(), VERSION.into()),
        ("accept".into(), "application/json".into()),
        ("cache-control".into(), "no-cache".into()),
        // The LibreLinkUp Android app talks via okhttp. We MUST send this explicitly:
        // on Cloudflare Workers an unset User-Agent gets a default (e.g.
        // "Cloudflare-Workers") that LibreView's edge (Akamai) blocks with a bare 403
        // before any credential check. A browser-style UA is also blocked; okhttp passes.
        ("User-Agent".into(), "okhttp/4.9.3".into()),
    ];
    if let Some(t) = token {
        h.push(("authorization".into(), format!("Bearer {t}")));
    }
    if let Some(a) = account_id {
        h.push(("account-id".into(), a.into()));
    }
    h
}

pub fn login_body(email: &str, password: &str) -> Value {
    json!({ "email": email, "password": password })
}

pub fn connections_url(base: &str) -> String {
    format!("{base}/llu/connections")
}

pub fn graph_url(base: &str, patient_id: &str) -> String {
    format!("{base}/llu/connections/{patient_id}/graph")
}

/// Outcome of a login attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum LoginResult {
    /// Authenticated: bearer token + the account's user id.
    Authenticated { token: String, user_id: String },
    /// The account lives in another region; re-login against it.
    Redirect { region: String },
}

pub fn parse_login(body: &[u8]) -> Result<LoginResult, ConnectorError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| ConnectorError::Parse(e.to_string()))?;
    // LibreLinkUp signals failure with a non-zero `status` (2 = bad credentials,
    // 4 = action required / terms not accepted, 429 = locked) and NO `data` object —
    // the reason lives in `error.message` (or `data.message` for lockouts). Surface it
    // so the connector's status tells the user exactly what's wrong.
    if let Some(code) = v.get("status").and_then(|s| s.as_i64()) {
        if code != 0 {
            let msg = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .or_else(|| v.get("data").and_then(|d| d.get("message")).and_then(|m| m.as_str()))
                .unwrap_or("login rejected");
            return Err(ConnectorError::Auth(format!(
                "LibreLinkUp login status {code}: {msg}"
            )));
        }
    }
    let data = v.get("data").ok_or_else(|| ConnectorError::Auth("login: no data".into()))?;
    if data.get("redirect").and_then(|r| r.as_bool()).unwrap_or(false) {
        let region = data
            .get("region")
            .and_then(|r| r.as_str())
            .ok_or_else(|| ConnectorError::Auth("login redirect without region".into()))?;
        if !is_valid_region(region) {
            return Err(ConnectorError::Auth(format!(
                "login redirect with invalid region {region:?}"
            )));
        }
        return Ok(LoginResult::Redirect { region: region.to_string() });
    }
    let token = data
        .get("authTicket")
        .and_then(|t| t.get("token"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| ConnectorError::Auth("login: no token (bad credentials?)".into()))?;
    let user_id = data
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(|u| u.as_str())
        .ok_or_else(|| ConnectorError::Auth("login: no user id".into()))?;
    Ok(LoginResult::Authenticated {
        token: token.to_string(),
        user_id: user_id.to_string(),
    })
}

/// Patient ids of the accounts this user follows.
pub fn parse_connections(body: &[u8]) -> Result<Vec<String>, ConnectorError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| ConnectorError::Parse(e.to_string()))?;
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| ConnectorError::Parse("connections: no data array".into()))?;
    Ok(arr
        .iter()
        .filter_map(|c| c.get("patientId").and_then(|p| p.as_str()).map(str::to_string))
        .collect())
}

/// LibreLinkUp `TrendArrow` integer → our [`Direction`].
pub fn trend_from_arrow(n: i64) -> Option<Direction> {
    match n {
        1 => Some(Direction::SingleDown),
        2 => Some(Direction::FortyFiveDown),
        3 => Some(Direction::Flat),
        4 => Some(Direction::FortyFiveUp),
        5 => Some(Direction::SingleUp),
        _ => None,
    }
}

/// Parse LibreLinkUp's `FactoryTimestamp` (UTC, `"M/D/YYYY h:mm:ss AM/PM"`) to epoch ms.
pub fn parse_factory_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(' ')?;
    let (time, ampm) = rest.rsplit_once(' ')?;
    let mut d = date.split('/');
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let year: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let mut hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    match ampm.to_ascii_uppercase().as_str() {
        "AM" => {
            if hour == 12 {
                hour = 0;
            }
        }
        "PM" => {
            if hour != 12 {
                hour += 12;
            }
        }
        _ => return None,
    }
    Some(timeutil::ymd_hms_milli_to_ms(year, month, day, hour, min, sec, 0))
}

fn sample_from_measurement(m: &Value, with_trend: bool) -> Option<CgmSample> {
    let mgdl = m.get("ValueInMgPerDl").and_then(|v| v.as_i64())?;
    let ts = m
        .get("FactoryTimestamp")
        .or_else(|| m.get("Timestamp"))
        .and_then(|v| v.as_str())?;
    let date_ms = parse_factory_timestamp(ts)?;
    let direction = if with_trend {
        m.get("TrendArrow").and_then(|v| v.as_i64()).and_then(trend_from_arrow)
    } else {
        None
    };
    Some(CgmSample { date_ms, mgdl, direction, device: "librelinkup".to_string() })
}

/// Parse the graph response: the latest measurement (with trend) + the recent points.
pub fn parse_graph(body: &[u8]) -> Result<Vec<CgmSample>, ConnectorError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| ConnectorError::Parse(e.to_string()))?;
    let data = v.get("data").ok_or_else(|| ConnectorError::Parse("graph: no data".into()))?;
    let mut out = Vec::new();
    if let Some(latest) = data.get("connection").and_then(|c| c.get("glucoseMeasurement")) {
        if let Some(s) = sample_from_measurement(latest, true) {
            out.push(s);
        }
    }
    if let Some(points) = data.get("graphData").and_then(|g| g.as_array()) {
        for p in points {
            if let Some(s) = sample_from_measurement(p, false) {
                out.push(s);
            }
        }
    }
    Ok(out)
}

/// A configured LibreLinkUp connector.
pub struct LibreLinkUpConnector {
    pub email: String,
    pub password: String,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Connector for LibreLinkUpConnector {
    fn name(&self) -> &'static str {
        "librelinkup"
    }

    async fn fetch_recent(
        &self,
        http: Http<'_>,
        _minutes: i64,
    ) -> Result<Vec<CgmSample>, ConnectorError> {
        // Log in, following one region redirect if the account lives elsewhere.
        let mut base = DEFAULT_BASE.to_string();
        let login = self.do_login(http, &base).await?;
        let (token, user_id) = match login {
            LoginResult::Authenticated { token, user_id } => (token, user_id),
            LoginResult::Redirect { region } => {
                base = regional_base(&region);
                match self.do_login(http, &base).await? {
                    LoginResult::Authenticated { token, user_id } => (token, user_id),
                    LoginResult::Redirect { .. } => {
                        return Err(ConnectorError::Auth("login redirect loop".into()))
                    }
                }
            }
        };
        let acct = account_id_hash(&user_id);

        // Find the followed patient.
        let conns = http
            .send(HttpReq::get(connections_url(&base), headers(Some(&token), Some(&acct))))
            .await?;
        if !conns.is_success() {
            return Err(ConnectorError::Protocol(format!(
                "connections failed ({}) {}",
                conns.status,
                snippet(&conns.body)
            )));
        }
        let patient = parse_connections(&conns.body)?
            .into_iter()
            .next()
            .ok_or_else(|| ConnectorError::Protocol("no LibreLinkUp connections".into()))?;

        // Read the graph.
        let graph = http
            .send(HttpReq::get(graph_url(&base, &patient), headers(Some(&token), Some(&acct))))
            .await?;
        if !graph.is_success() {
            return Err(ConnectorError::Protocol(format!(
                "graph failed ({}) {}",
                graph.status,
                snippet(&graph.body)
            )));
        }
        parse_graph(&graph.body)
    }
}

impl LibreLinkUpConnector {
    async fn do_login(&self, http: Http<'_>, base: &str) -> Result<LoginResult, ConnectorError> {
        let resp = http
            .send(HttpReq::post_json(
                format!("{base}/llu/auth/login"),
                headers(None, None),
                &login_body(&self.email, &self.password),
            ))
            .await?;
        if !resp.is_success() {
            return Err(ConnectorError::Auth(format!("login failed ({})", resp.status)));
        }
        parse_login(&resp.body)
    }
}

/// A compact, single-line preview of a response body for diagnostics — lets a 403
/// reveal *who* refused: an Akamai/edge bot-block (HTML "Access Denied" + reference)
/// vs a LibreView app-layer JSON message. Truncated; control chars collapsed.
fn snippet(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s: String = trimmed.chars().take(180).collect();
    if trimmed.chars().count() > 180 {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_is_one_line_and_truncated() {
        assert_eq!(snippet(b"<html>\n  Access\tDenied  </html>"), "<html> Access Denied </html>");
        assert!(snippet(&[b'x'; 500]).chars().count() <= 181);
    }

    #[test]
    fn account_id_is_sha256_hex() {
        // Deterministic, 64 hex chars.
        let h = account_id_hash("user-123");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn headers_include_auth_and_account_id() {
        let h = headers(Some("tok"), Some("acct"));
        assert!(h.iter().any(|(k, v)| k == "authorization" && v == "Bearer tok"));
        assert!(h.iter().any(|(k, v)| k == "account-id" && v == "acct"));
        assert!(h.iter().any(|(k, v)| k == "product" && v == PRODUCT));
    }

    /// A successful login yields token + user id; a regional redirect is surfaced.
    #[test]
    fn parses_login_success_and_redirect() {
        let ok = br#"{"status":0,"data":{"authTicket":{"token":"jwt-abc"},"user":{"id":"u-1"}}}"#;
        assert_eq!(
            parse_login(ok).unwrap(),
            LoginResult::Authenticated { token: "jwt-abc".into(), user_id: "u-1".into() }
        );
        let redir = br#"{"status":0,"data":{"redirect":true,"region":"eu"}}"#;
        assert_eq!(parse_login(redir).unwrap(), LoginResult::Redirect { region: "eu".into() });
        assert_eq!(regional_base("EU"), "https://api-eu.libreview.io");
    }

    /// A redirect carrying a hostile "region" (one that would reshape the API host)
    /// is rejected rather than interpolated into the URL.
    #[test]
    fn rejects_redirect_with_invalid_region() {
        assert!(is_valid_region("eu") && is_valid_region("ap-west"));
        assert!(!is_valid_region("x/@evil.com") && !is_valid_region("a.b") && !is_valid_region(""));
        let redir = br#"{"status":0,"data":{"redirect":true,"region":"x/@evil.com"}}"#;
        assert!(matches!(parse_login(redir), Err(ConnectorError::Auth(_))));
    }

    /// A non-zero `status` (no `data` object) must surface the real reason — so the
    /// connector's status in the UI says *why*, not a generic "no data".
    #[test]
    fn surfaces_login_failures() {
        let bad = br#"{"status":2,"error":{"message":"incorrect username/password"}}"#;
        let err = parse_login(bad).unwrap_err();
        assert!(
            matches!(&err, ConnectorError::Auth(m) if m.contains("status 2") && m.contains("incorrect username/password")),
            "got {err:?}"
        );

        let locked = br#"{"status":429,"data":{"message":"locked"}}"#;
        let err = parse_login(locked).unwrap_err();
        assert!(
            matches!(&err, ConnectorError::Auth(m) if m.contains("status 429") && m.contains("locked")),
            "got {err:?}"
        );
    }

    #[test]
    fn parses_connections_patient_ids() {
        let body = br#"{"status":0,"data":[{"patientId":"p-1"},{"patientId":"p-2"}]}"#;
        assert_eq!(parse_connections(body).unwrap(), vec!["p-1", "p-2"]);
    }

    /// LibreLinkUp trend integers map to the right arrows (3 = Flat).
    #[test]
    fn maps_trend_arrows() {
        assert_eq!(trend_from_arrow(3), Some(Direction::Flat));
        assert_eq!(trend_from_arrow(5), Some(Direction::SingleUp));
        assert_eq!(trend_from_arrow(1), Some(Direction::SingleDown));
        assert_eq!(trend_from_arrow(9), None);
    }

    /// The LibreLinkUp `M/D/YYYY h:mm:ss AM/PM` timestamp parses to UTC epoch ms,
    /// with correct 12-hour handling at noon and midnight.
    #[test]
    fn parses_factory_timestamp() {
        // 2023-11-14 22:13:19 UTC == 1_699_999_999_000 ms.
        assert_eq!(parse_factory_timestamp("11/14/2023 10:13:19 PM"), Some(1_699_999_999_000));
        // Midnight and noon edges.
        let midnight = parse_factory_timestamp("1/1/2024 12:00:00 AM").unwrap();
        assert_eq!(midnight, timeutil::parse_iso8601_ms("2024-01-01T00:00:00Z").unwrap());
        let noon = parse_factory_timestamp("1/1/2024 12:00:00 PM").unwrap();
        assert_eq!(noon, timeutil::parse_iso8601_ms("2024-01-01T12:00:00Z").unwrap());
    }

    /// The graph response yields the latest measurement (with trend) plus history.
    #[test]
    fn parses_graph_latest_and_history() {
        let body = br#"{
            "status":0,
            "data":{
                "connection":{ "glucoseMeasurement":{ "ValueInMgPerDl":120, "FactoryTimestamp":"11/14/2023 10:13:19 PM", "TrendArrow":3 } },
                "graphData":[
                    { "ValueInMgPerDl":118, "FactoryTimestamp":"11/14/2023 10:08:19 PM" },
                    { "ValueInMgPerDl":115, "FactoryTimestamp":"11/14/2023 10:03:19 PM" }
                ]
            }
        }"#;
        let samples = parse_graph(body).unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].mgdl, 120);
        assert_eq!(samples[0].direction, Some(Direction::Flat)); // latest has a trend
        assert_eq!(samples[1].mgdl, 118);
        assert_eq!(samples[1].direction, None); // history points carry no trend
        assert_eq!(samples[0].device, "librelinkup");
    }
}
