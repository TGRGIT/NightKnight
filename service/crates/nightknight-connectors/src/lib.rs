//! # nightknight-connectors
//!
//! Pulls glucose from vendor clouds — **Dexcom Share** and **LibreLinkUp** — and
//! normalises it into [`CgmSample`]s ready to store as entries.
//!
//! The *protocol* (which URLs, which JSON, how to read the response) is pure,
//! synchronous, and unit-tested in [`dexcom`] and [`librelinkup`]. The *I/O* (the
//! actual HTTPS calls) is abstracted behind the [`HttpClient`] trait, so the same
//! connector runs on the Cloudflare Worker (via `worker::Fetch`) and the container
//! (via `reqwest`), and the tests need no network.
//!
//! These connectors talk to **unofficial** vendor endpoints; they are best-effort
//! and feature-flagged at the runtime. Credentials are supplied by the runtime
//! (decrypted from per-user storage) and never logged.

pub mod dexcom;
pub mod librelinkup;
pub mod nightscout;

use serde_json::{json, Value};

use nightknight_core::Direction;

/// One normalised glucose reading from a vendor cloud.
#[derive(Debug, Clone, PartialEq)]
pub struct CgmSample {
    /// Reading time, epoch milliseconds (UTC).
    pub date_ms: i64,
    /// Glucose in mg/dL (vendor clouds report mg/dL natively).
    pub mgdl: i64,
    /// Trend arrow, if the vendor provided one.
    pub direction: Option<Direction>,
    /// Source device label, e.g. `"dexcom-share"`.
    pub device: String,
}

impl CgmSample {
    /// Render as a Nightscout `sgv` entry body, ready for the storage/API layer to
    /// assign an identifier and persist (dedup handles re-fetched overlaps).
    pub fn to_entry_json(&self) -> Value {
        let mut o = json!({
            "type": "sgv",
            "date": self.date_ms,
            "sgv": self.mgdl,
            "device": self.device,
        });
        if let Some(d) = self.direction {
            o["direction"] = json!(d.name());
        }
        o
    }
}

/// Errors from a connector.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ConnectorError {
    #[error("http transport error: {0}")]
    Http(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("could not parse vendor response: {0}")]
    Parse(String),
    #[error("vendor protocol error: {0}")]
    Protocol(String),
}

/// A minimal HTTP request the runtime executes.
#[derive(Debug, Clone)]
pub struct HttpReq {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// Whether the transport may follow 3xx redirects. Connectors that hit hard-coded
    /// vendor hosts leave this `true`. The user-supplied-URL Nightscout fetch sets it
    /// `false`: the SSRF guard only validates the *original* host, so a malicious source
    /// could otherwise `302` the request (carrying its `api-secret`) to a loopback /
    /// link-local / metadata target. With redirects refused, any 3xx comes back as-is and
    /// the connector treats the non-2xx as an error instead of following it.
    pub follow_redirects: bool,
}

impl HttpReq {
    pub fn get(url: impl Into<String>, headers: Vec<(String, String)>) -> HttpReq {
        HttpReq { method: "GET", url: url.into(), headers, body: None, follow_redirects: true }
    }
    pub fn post_json(url: impl Into<String>, headers: Vec<(String, String)>, body: &Value) -> HttpReq {
        let mut headers = headers;
        headers.push(("content-type".into(), "application/json".into()));
        HttpReq {
            method: "POST",
            url: url.into(),
            headers,
            body: Some(serde_json::to_vec(body).unwrap_or_default()),
            follow_redirects: true,
        }
    }
    /// Refuse 3xx redirects — for user-supplied URLs where following one would bypass the
    /// SSRF host check.
    pub fn no_redirects(mut self) -> Self {
        self.follow_redirects = false;
        self
    }
}

/// A minimal HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResp {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResp {
    pub fn json(&self) -> Result<Value, ConnectorError> {
        serde_json::from_slice(&self.body).map_err(|e| ConnectorError::Parse(e.to_string()))
    }
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The runtime-provided HTTP transport. `Send` everywhere except on the
/// single-threaded Workers runtime.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait HttpClient {
    async fn send(&self, req: HttpReq) -> Result<HttpResp, ConnectorError>;
}

/// A borrowed HTTP transport. On native it must be `Sync` so connector futures stay
/// `Send` (axum/tokio); on the single-threaded Workers runtime that bound is dropped.
#[cfg(not(target_arch = "wasm32"))]
pub type Http<'a> = &'a (dyn HttpClient + Sync);
#[cfg(target_arch = "wasm32")]
pub type Http<'a> = &'a dyn HttpClient;

/// A vendor connector: given an HTTP transport, fetch recent readings.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Connector {
    /// Fetch readings from up to `minutes` ago.
    async fn fetch_recent(
        &self,
        http: Http<'_>,
        minutes: i64,
    ) -> Result<Vec<CgmSample>, ConnectorError>;

    /// A short identifier for logs/config.
    fn name(&self) -> &'static str;
}

/// Hex-encode bytes (small helper shared by the connectors).
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
