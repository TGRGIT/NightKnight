//! Dexcom Share connector.
//!
//! Reproduces the Dexcom Share API flow used by `pydexcom`:
//! 1. `AuthenticatePublisherAccount` (accountName/password/applicationId) → account id
//! 2. `LoginPublisherAccountById` (accountId/password/applicationId) → session id
//! 3. `ReadPublisherLatestGlucoseValues?sessionId=…&minutes=…&maxCount=…` → readings
//!
//! All the request/response shaping here is pure and tested; the network calls run
//! through the injected [`HttpClient`].

use serde_json::{json, Value};

use nightknight_core::Direction;

use crate::{CgmSample, Connector, ConnectorError, Http, HttpReq};

/// Dexcom Share application id (US/OUS).
pub const APP_ID_US: &str = "d89443d2-327c-4a6f-89e5-496bbb0317db";
/// Dexcom Share application id (Japan).
pub const APP_ID_JP: &str = "d8665ade-9673-4e27-9ff6-92db4ce13d13";

/// Dexcom Share server region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Us,
    Ous,
    Jp,
}

impl Region {
    pub fn parse(s: &str) -> Region {
        match s.trim().to_ascii_lowercase().as_str() {
            "ous" | "eu" => Region::Ous,
            "jp" => Region::Jp,
            _ => Region::Us,
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Region::Us => "https://share2.dexcom.com/ShareWebServices/Services",
            Region::Ous => "https://shareous1.dexcom.com/ShareWebServices/Services",
            Region::Jp => "https://share.dexcom.jp/ShareWebServices/Services",
        }
    }

    pub fn application_id(self) -> &'static str {
        match self {
            Region::Jp => APP_ID_JP,
            _ => APP_ID_US,
        }
    }
}

fn json_headers() -> Vec<(String, String)> {
    vec![
        ("accept".into(), "application/json".into()),
        ("user-agent".into(), "Dexcom Share/3.0.2.11 CFNetwork".into()),
    ]
}

/// Body for `AuthenticatePublisherAccount`.
pub fn authenticate_body(username: &str, password: &str, application_id: &str) -> Value {
    json!({ "accountName": username, "password": password, "applicationId": application_id })
}

/// Body for `LoginPublisherAccountById`.
pub fn login_body(account_id: &str, password: &str, application_id: &str) -> Value {
    json!({ "accountId": account_id, "password": password, "applicationId": application_id })
}

/// URL for `ReadPublisherLatestGlucoseValues`.
pub fn read_url(base: &str, session_id: &str, minutes: i64, max_count: i64) -> String {
    format!(
        "{base}/Publisher/ReadPublisherLatestGlucoseValues?sessionId={session_id}&minutes={minutes}&maxCount={max_count}"
    )
}

/// The auth/login endpoints return a bare JSON string (a quoted UUID). Extract it.
pub fn parse_quoted_id(body: &[u8]) -> Result<String, ConnectorError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::String(s)) => Ok(s),
        _ => Err(ConnectorError::Auth("expected a quoted id string".into())),
    }
}

/// Map a Dexcom Share `Trend` field to a [`Direction`]. Newer transmitters report a
/// **string** (`"Flat"`, `"FortyFiveUp"`, …) that matches our Nightscout names 1:1;
/// older ones report a legacy **integer** code (verified against `pydexcom`):
/// `0=None, 1=DoubleUp, 2=SingleUp, 3=FortyFiveUp, 4=Flat, 5=FortyFiveDown,
/// 6=SingleDown, 7=DoubleDown, 8=NotComputable, 9=RateOutOfRange`.
pub fn trend_from_share(v: &Value) -> Option<Direction> {
    if let Some(s) = v.as_str() {
        return serde_json::from_value::<Direction>(Value::String(s.to_string())).ok();
    }
    match v.as_i64()? {
        1 => Some(Direction::DoubleUp),
        2 => Some(Direction::SingleUp),
        3 => Some(Direction::FortyFiveUp),
        4 => Some(Direction::Flat),
        5 => Some(Direction::FortyFiveDown),
        6 => Some(Direction::SingleDown),
        7 => Some(Direction::DoubleDown),
        8 => Some(Direction::NotComputable),
        9 => Some(Direction::RateOutOfRange),
        _ => None, // 0 = None (no arrow)
    }
}

/// Extract the epoch-ms out of a Dexcom `WT`/`ST` timestamp like `"Date(1699999999000-0500)"`.
pub fn parse_wt_ms(wt: &str) -> Option<i64> {
    let start = wt.find("Date(")? + 5;
    let rest = &wt[start..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<i64>().ok()
}

/// Parse the glucose-values array into [`CgmSample`]s.
pub fn parse_glucose(body: &[u8]) -> Result<Vec<CgmSample>, ConnectorError> {
    let arr: Value = serde_json::from_slice(body).map_err(|e| ConnectorError::Parse(e.to_string()))?;
    let items = arr
        .as_array()
        .ok_or_else(|| ConnectorError::Parse("expected a JSON array of readings".into()))?;
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let mgdl = it
            .get("Value")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| ConnectorError::Parse("reading missing Value".into()))?;
        let wt = it
            .get("WT")
            .or_else(|| it.get("ST"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConnectorError::Parse("reading missing WT".into()))?;
        let date_ms = parse_wt_ms(wt)
            .ok_or_else(|| ConnectorError::Parse(format!("bad WT timestamp: {wt}")))?;
        // Dexcom trend is a string ("Flat", …) on newer transmitters or a legacy
        // integer code on older ones — both map to our Direction.
        let direction = it.get("Trend").and_then(trend_from_share);
        out.push(CgmSample {
            date_ms,
            mgdl,
            direction,
            device: "dexcom-share".to_string(),
        });
    }
    Ok(out)
}

/// A configured Dexcom Share connector (credentials supplied by the runtime).
pub struct DexcomConnector {
    pub region: Region,
    pub username: String,
    pub password: String,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Connector for DexcomConnector {
    fn name(&self) -> &'static str {
        "dexcom-share"
    }

    async fn fetch_recent(
        &self,
        http: Http<'_>,
        minutes: i64,
    ) -> Result<Vec<CgmSample>, ConnectorError> {
        let base = self.region.base_url();
        let app = self.region.application_id();

        // 1. account id
        let acct_resp = http
            .send(HttpReq::post_json(
                format!("{base}/General/AuthenticatePublisherAccount"),
                json_headers(),
                &authenticate_body(&self.username, &self.password, app),
            ))
            .await?;
        if !acct_resp.is_success() {
            return Err(ConnectorError::Auth(format!("authenticate failed ({})", acct_resp.status)));
        }
        let account_id = parse_quoted_id(&acct_resp.body)?;

        // 2. session id
        let sess_resp = http
            .send(HttpReq::post_json(
                format!("{base}/General/LoginPublisherAccountById"),
                json_headers(),
                &login_body(&account_id, &self.password, app),
            ))
            .await?;
        if !sess_resp.is_success() {
            return Err(ConnectorError::Auth(format!("login failed ({})", sess_resp.status)));
        }
        let session_id = parse_quoted_id(&sess_resp.body)?;

        // 3. readings (POST with empty body, params in the query string)
        let max_count = (minutes / 5).clamp(1, 288);
        let read_resp = http
            .send(HttpReq::post_json(
                read_url(base, &session_id, minutes.clamp(1, 1440), max_count),
                json_headers(),
                &json!({}),
            ))
            .await?;
        if !read_resp.is_success() {
            return Err(ConnectorError::Protocol(format!("read failed ({})", read_resp.status)));
        }
        parse_glucose(&read_resp.body)
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

    /// Region selection picks the right base URL and application id.
    #[test]
    fn region_endpoints() {
        assert!(Region::Us.base_url().contains("share2.dexcom.com"));
        assert!(Region::Ous.base_url().contains("shareous1.dexcom.com"));
        assert_eq!(Region::Us.application_id(), APP_ID_US);
        assert_eq!(Region::Jp.application_id(), APP_ID_JP);
        assert_eq!(Region::parse("EU"), Region::Ous);
    }

    /// Request bodies carry exactly the fields the Dexcom Share API expects.
    #[test]
    fn request_bodies() {
        let a = authenticate_body("user", "pass", APP_ID_US);
        assert_eq!(a["accountName"], "user");
        assert_eq!(a["applicationId"], APP_ID_US);
        let l = login_body("acct-id", "pass", APP_ID_US);
        assert_eq!(l["accountId"], "acct-id");
        let url = read_url(Region::Us.base_url(), "sess", 1440, 288);
        assert!(url.contains("sessionId=sess") && url.contains("minutes=1440") && url.contains("maxCount=288"));
    }

    /// The auth/login endpoints return a quoted UUID string; we unwrap it.
    #[test]
    fn parses_quoted_id() {
        assert_eq!(parse_quoted_id(b"\"abc-123\"").unwrap(), "abc-123");
        assert!(parse_quoted_id(b"{\"not\":\"a string\"}").is_err());
    }

    /// The Dexcom `WT` timestamp yields epoch ms regardless of the timezone suffix.
    /// The case table is the shared fixture, asserted identically by the Swift port.
    #[test]
    fn parses_wt_timestamp() {
        let table: Vec<Value> =
            serde_json::from_slice(fixture!("dexcom-wt-timestamps.json")).unwrap();
        assert!(!table.is_empty());
        for row in &table {
            let wt = row["wt"].as_str().unwrap();
            assert_eq!(parse_wt_ms(wt), row["ms"].as_i64(), "parse_wt_ms({wt:?})");
        }
    }

    /// The Dexcom Share `Trend` field maps to a Direction whether it arrives as a
    /// string (newer transmitters) or a legacy integer code (older ones).
    #[test]
    fn maps_share_trend_string_and_integer() {
        assert_eq!(trend_from_share(&json!("Flat")), Some(Direction::Flat));
        assert_eq!(trend_from_share(&json!("FortyFiveDown")), Some(Direction::FortyFiveDown));
        // Legacy integer codes (pydexcom): 4 = Flat, 5 = FortyFiveDown, 1 = DoubleUp.
        assert_eq!(trend_from_share(&json!(4)), Some(Direction::Flat));
        assert_eq!(trend_from_share(&json!(5)), Some(Direction::FortyFiveDown));
        assert_eq!(trend_from_share(&json!(1)), Some(Direction::DoubleUp));
        assert_eq!(trend_from_share(&json!(0)), None); // 0 = None → no arrow
    }

    /// A representative readings payload (shared fixture) parses into samples with
    /// mg/dL, time, trend.
    #[test]
    fn parses_glucose_payload() {
        let body = fixture!("dexcom-glucose.json");
        let samples = parse_glucose(body).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].mgdl, 120);
        assert_eq!(samples[0].date_ms, 1_699_999_999_000);
        assert_eq!(samples[0].direction, Some(Direction::Flat));
        assert_eq!(samples[1].direction, Some(Direction::FortyFiveUp));
        assert_eq!(samples[0].device, "dexcom-share");
        // And it converts cleanly to a Nightscout entry body.
        let entry = samples[0].to_entry_json();
        assert_eq!(entry["type"], "sgv");
        assert_eq!(entry["sgv"], 120);
        assert_eq!(entry["direction"], "Flat");
    }
}
