//! Connector scheduler for the container runtime.
//!
//! A `reqwest`-backed [`HttpClient`] plus three tokio loops that drive
//! [`ApiService::sync_connectors`] over *all* users' stored, enabled credentials:
//! every minute (latest readings), hourly (≈1 week trailing backfill), and daily (all
//! available history). Every window is capped by what each vendor returns. Decryption,
//! polling, ingest and status are handled inside `sync_connectors`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use nightknight_api::ApiService;
use nightknight_connectors::{ConnectorError, HttpClient, HttpReq, HttpResp};
use nightknight_store_sql::SqlStore;

const LATEST_MINUTES: i64 = 15;
const TRAILING_MINUTES: i64 = 10_080; // 1 week (vendor-capped)
const ALL_MINUTES: i64 = 525_600; // 365 days: "all available" — vendors clamp far below
const DAY_SECS: u64 = 86_400;

/// A `reqwest`-backed HTTP transport for connectors.
pub struct ReqwestHttp {
    client: reqwest::Client,
    /// A second client that never follows redirects, for user-supplied-URL fetches
    /// (Nightscout) where following a 3xx would bypass the SSRF host check.
    no_redirect: reqwest::Client,
}

impl ReqwestHttp {
    pub fn new() -> Self {
        ReqwestHttp {
            client: reqwest::Client::builder().build().unwrap_or_default(),
            no_redirect: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl HttpClient for ReqwestHttp {
    async fn send(&self, req: HttpReq) -> Result<HttpResp, ConnectorError> {
        let client = if req.follow_redirects { &self.client } else { &self.no_redirect };
        let mut rb = match req.method {
            "POST" => client.post(&req.url),
            _ => client.get(&req.url),
        };
        for (k, v) in &req.headers {
            rb = rb.header(k, v);
        }
        if let Some(body) = req.body {
            rb = rb.body(body);
        }
        let resp = rb.send().await.map_err(|e| ConnectorError::Http(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| ConnectorError::Http(e.to_string()))?
            .to_vec();
        Ok(HttpResp { status, body })
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Start the sync loops if a connector key is configured (checked by the caller via
/// the service having a key). Spawns three tasks: per-minute "latest", hourly
/// "trailing", and daily "all".
pub fn spawn(service: Arc<ApiService<SqlStore>>) {
    if std::env::var("NK_CONNECTOR_KEY").ok().filter(|s| !s.is_empty()).is_none() {
        tracing::info!("no NK_CONNECTOR_KEY set — CGM connector sync disabled");
        return;
    }
    tracing::info!("starting CGM connector sync (per-minute latest + hourly trailing + daily all)");

    // Per-minute: latest readings.
    let svc_latest = service.clone();
    tokio::spawn(async move {
        let http = ReqwestHttp::new();
        loop {
            run_once(&svc_latest, &http, LATEST_MINUTES, "latest").await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    // Hourly: trailing backfill.
    let svc_trailing = service.clone();
    tokio::spawn(async move {
        let http = ReqwestHttp::new();
        loop {
            run_once(&svc_trailing, &http, TRAILING_MINUTES, "trailing").await;
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });

    // Daily: all available history.
    tokio::spawn(async move {
        let http = ReqwestHttp::new();
        loop {
            run_once(&service, &http, ALL_MINUTES, "all").await;
            tokio::time::sleep(Duration::from_secs(DAY_SECS)).await;
        }
    });
}

async fn run_once(service: &ApiService<SqlStore>, http: &ReqwestHttp, minutes: i64, label: &str) {
    match service.sync_connectors(http, minutes, now_ms()).await {
        Ok(n) => tracing::info!(label, minutes, ingested = n, "connector sync ok"),
        Err(e) => tracing::warn!(label, "connector sync failed: {e}"),
    }
}
