//! # nightknight-worker
//!
//! The Cloudflare Worker entrypoint. For every request it:
//!
//! 1. serves the web SPA from Static Assets for non-`/api` paths;
//! 2. for `/api/*`, verifies the Cloudflare Access JWT (`Cf-Access-Jwt-Assertion`)
//!    against CF's JWKS + the application AUD, deriving the user; and
//! 3. dispatches to the shared [`ApiService`] over a D1-backed store.
//!
//! Verifying the Access JWT in the worker also closes the `*.pages.dev` / preview
//! bypass, since those hosts are not behind the Access application. wasm32 only.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;

use worker::wasm_bindgen::JsValue;
use worker::*;

use nightknight_api::{
    ApiRequest, ApiResponse, ApiService, EdgeIdentity, Headers as ApiHeaders, Method as ApiMethod,
    PrincipalKind,
};
use nightknight_auth::{Jwks, Verifier};
use nightknight_connectors::{ConnectorError, HttpClient, HttpReq, HttpResp};
use nightknight_store_d1::D1Store;

/// Read and parse the connector encryption key from the `CF_CONNECTOR_KEY` secret.
fn connector_key(env: &Env) -> Option<[u8; 32]> {
    env.var("CF_CONNECTOR_KEY")
        .ok()
        .and_then(|v| nightknight_crypto::parse_key(&v.to_string()).ok())
}

/// Build the API service from the environment (storage + group + connector key).
fn build_service(env: &Env) -> Result<ApiService<D1Store>> {
    let store = D1Store::new(env.d1("DB")?);
    let required_group = env.var("CF_REQUIRED_GROUP").ok().map(|v| v.to_string());
    let migrate_legacy = env
        .var("MIGRATE_LEGACY_SUBJECTS")
        .map(|v| v.to_string().eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    Ok(ApiService::new(store)
        .require_group(required_group)
        .with_connector_key(connector_key(env))
        .migrate_legacy_subjects(migrate_legacy))
}

thread_local! {
    /// Per-isolate cache of (JWKS JSON, fetched-at ms) and a one-time migration flag.
    static JWKS_CACHE: RefCell<Option<(String, f64)>> = const { RefCell::new(None) };
    static MIGRATED: RefCell<bool> = const { RefCell::new(false) };
}

const JWKS_TTL_MS: f64 = 3_600_000.0; // refresh the key set hourly

/// The daily job's lookback: larger than any vendor's history window, so each
/// connector returns the maximum it allows ("all available data"). Vendors clamp.
const ALL_MINUTES: i64 = 525_600; // 365 days

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_string();

    // The SPA static assets are served by the platform: wrangler is configured with
    // `run_worker_first = ["/api/*"]`, so this Worker is only invoked for the API.
    // Any non-API path reaching here is unexpected.
    if !path.starts_with("/api/") {
        return Response::error("not found", 404);
    }

    let service = build_service(&env)?;
    ensure_migrated(&service).await?;

    // Resolve the edge identity (Cloudflare Access). A present-but-invalid JWT is a
    // hard 401; absence means "no human identity" (device-token path).
    let edge = match resolve_edge(&req, &env).await {
        Ok(edge) => edge,
        Err(resp) => return Ok(resp),
    };

    let now_ms = Date::now().as_millis() as i64;
    // Log only method + path + status (never the query string or headers) — credentials
    // are header-only and never appear here. This surfaces every API call in the
    // Cloudflare observability logs without leaking secrets.
    let method = format!("{:?}", req.method());
    let api_req = build_api_request(&mut req, &url).await?;
    let resp = service.handle(api_req, now_ms, edge).await;
    console_log!("{method} {path} -> {}", resp.status);
    to_worker_response(resp)
}

async fn ensure_migrated(service: &ApiService<D1Store>) -> Result<()> {
    if MIGRATED.with(|m| *m.borrow()) {
        return Ok(());
    }
    use nightknight_storage::Storage;
    service
        .storage()
        .migrate()
        .await
        .map_err(|e| Error::RustError(e.to_string()))?;
    MIGRATED.with(|m| *m.borrow_mut() = true);
    Ok(())
}

/// Verify the Cloudflare Access JWT, returning the identity. On a present-but-invalid
/// token returns `Err(401 response)`; absent token / unconfigured → `Ok(None)`.
async fn resolve_edge(req: &Request, env: &Env) -> std::result::Result<Option<EdgeIdentity>, Response> {
    let jwt = req
        .headers()
        .get("Cf-Access-Jwt-Assertion")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty());
    let aud = env.var("CF_ACCESS_AUD").ok().map(|v| v.to_string());
    let team = env.var("CF_TEAM_DOMAIN").ok().map(|v| v.to_string());

    let (Some(jwt), Some(aud), Some(team)) = (jwt, aud, team) else {
        return Ok(None);
    };

    let jwks = get_jwks(&team)
        .await
        .map_err(|_| unauthorized("could not load Access keys"))?;
    let now_secs = (Date::now().as_millis() / 1000) as i64;
    match Verifier::cloudflare_access(aud).verify(&jwt, &jwks, now_secs) {
        Ok(id) => {
            // For a human, fetch their group memberships from the Access identity
            // endpoint (the JWT itself does not carry the `groups` claim). Service
            // tokens are the machine/API-key path and carry no groups.
            let groups = if matches!(id.kind, PrincipalKind::Human) {
                let cookie = req.headers().get("cookie").ok().flatten().unwrap_or_default();
                fetch_groups(&team, &cookie).await
            } else {
                Vec::new()
            };
            Ok(Some(EdgeIdentity {
                subject: id.subject,
                kind: id.kind,
                display_name: id.email.clone(),
                email: id.email,
                groups,
            }))
        }
        Err(_) => Err(unauthorized("invalid Cloudflare Access token")),
    }
}

/// Fetch the caller's group memberships from the Cloudflare Access identity endpoint,
/// using the request's `CF_Authorization` cookie. Returns an empty list on any error
/// (the caller treats "no groups" as "not in the required group" → denied).
async fn fetch_groups(team: &str, cookie: &str) -> Vec<String> {
    if cookie.is_empty() {
        return Vec::new();
    }
    let url = format!("https://{team}.cloudflareaccess.com/cdn-cgi/access/get-identity");
    let headers = Headers::new();
    if headers.set("cookie", cookie).is_err() {
        return Vec::new();
    }
    let mut init = RequestInit::new();
    init.with_headers(headers);
    let req = match Request::new_with_init(&url, &init) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    match Fetch::Request(req).send().await {
        Ok(mut resp) => match resp.json::<serde_json::Value>().await {
            Ok(v) => {
                let mut out = Vec::new();
                collect_groups(&v, &mut out);
                out
            }
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

/// Cron-triggered CGM sync. Three schedules (see wrangler.toml `[triggers]`):
/// `* * * * *` (every minute → latest readings), `0 * * * *` (hourly → up to a week
/// of trailing history), and `0 0 * * *` (daily → all available history). Each runs
/// all enabled per-user connectors and ingests the readings; the requested window is
/// always capped by what each vendor exposes (Dexcom ≤24h/288; LibreLinkUp graph).
#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let service = match build_service(&env) {
        Ok(s) => s,
        Err(e) => {
            console_log!("scheduled: build_service failed: {e}");
            return;
        }
    };
    if ensure_migrated(&service).await.is_err() {
        return;
    }
    // Map each cron to its lookback window. The daily job asks for "everything"
    // (vendors clamp far below this); hourly backfills a week; per-minute grabs latest.
    let minutes: i64 = match event.cron().as_str() {
        "0 0 * * *" => ALL_MINUTES,
        "0 * * * *" => 10_080,
        _ => 15,
    };
    let now_ms = Date::now().as_millis() as i64;
    match service.sync_connectors(&WorkerHttp, minutes, now_ms).await {
        Ok(n) => console_log!("connector sync ({minutes}m): {n} readings ingested"),
        Err(e) => console_log!("connector sync error: {e}"),
    }
}

/// A `worker::Fetch`-backed [`HttpClient`] for the connectors.
struct WorkerHttp;

#[async_trait::async_trait(?Send)]
impl HttpClient for WorkerHttp {
    async fn send(&self, req: HttpReq) -> std::result::Result<HttpResp, ConnectorError> {
        let method = match req.method {
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            _ => Method::Get,
        };
        let headers = Headers::new();
        for (k, v) in &req.headers {
            let _ = headers.set(k, v);
        }
        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
        if let Some(body) = &req.body {
            // Connector bodies are JSON (UTF-8) — send as a string.
            init.with_body(Some(JsValue::from_str(&String::from_utf8_lossy(body))));
        }
        let request = Request::new_with_init(&req.url, &init)
            .map_err(|e| ConnectorError::Http(e.to_string()))?;
        let mut resp = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| ConnectorError::Http(e.to_string()))?;
        let status = resp.status_code();
        // Read as text so the runtime decompresses (gzip/br) — `bytes()` can hand back
        // still-compressed data, which breaks JSON parsing. Vendor APIs return UTF-8.
        let text = resp.text().await.map_err(|e| ConnectorError::Http(e.to_string()))?;
        Ok(HttpResp { status, body: text.into_bytes() })
    }
}

/// Recursively collect group strings from a get-identity payload. Defensive across
/// shapes: any `"groups"` array of strings, or of objects with `name`/`id`/`email`.
fn collect_groups(v: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if k == "groups" {
                    if let Value::Array(items) = val {
                        for it in items {
                            match it {
                                Value::String(s) => out.push(s.clone()),
                                Value::Object(o) => {
                                    for f in ["name", "id", "email"] {
                                        if let Some(Value::String(s)) = o.get(f) {
                                            out.push(s.clone());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                } else {
                    collect_groups(val, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|it| collect_groups(it, out)),
        _ => {}
    }
}

fn unauthorized(msg: &str) -> Response {
    Response::error(msg, 401).unwrap_or_else(|_| Response::empty().unwrap())
}

/// Fetch (and cache per isolate) the Cloudflare Access JWKS for the team.
async fn get_jwks(team: &str) -> Result<Jwks> {
    let now = Date::now().as_millis() as f64;
    if let Some((json, ts)) = JWKS_CACHE.with(|c| c.borrow().clone()) {
        if now - ts < JWKS_TTL_MS {
            return Jwks::parse(&json).map_err(|e| Error::RustError(e.to_string()));
        }
    }
    let url = format!("https://{team}.cloudflareaccess.com/cdn-cgi/access/certs");
    let mut resp = Fetch::Url(url.parse().map_err(|_| Error::RustError("bad jwks url".into()))?)
        .send()
        .await?;
    let json = resp.text().await?;
    JWKS_CACHE.with(|c| *c.borrow_mut() = Some((json.clone(), now)));
    Jwks::parse(&json).map_err(|e| Error::RustError(e.to_string()))
}

async fn build_api_request(req: &mut Request, url: &Url) -> Result<ApiRequest> {
    let method = ApiMethod::parse(&method_str(req.method()));
    let path = url.path().to_string();
    let query = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut headers = ApiHeaders::new();
    for (k, v) in req.headers() {
        headers.insert(k, v);
    }
    let body = req.bytes().await.unwrap_or_default();
    Ok(ApiRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn method_str(m: Method) -> String {
    match m {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
        Method::Options => "OPTIONS",
        Method::Head => "HEAD",
        _ => "OTHER",
    }
    .to_string()
}

fn to_worker_response(r: ApiResponse) -> Result<Response> {
    let headers = Headers::new();
    for (k, v) in &r.headers {
        headers.set(k, v)?;
    }
    Ok(Response::from_bytes(r.body)?
        .with_status(r.status)
        .with_headers(headers))
}
