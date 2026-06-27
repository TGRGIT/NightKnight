//! NightKnight container server.
//!
//! An `axum` HTTP server that wraps the shared [`nightknight_api::ApiService`] over
//! the SQLite/Postgres store and serves the web SPA. This is the deployment target
//! for self-hosting **outside** Cloudflare (Docker + Postgres). The Cloudflare Worker
//! runtime is a separate crate; both share all the logic underneath.
//!
//! ## Identity behind the gate
//!
//! The container expects to run behind a reverse proxy / identity provider (Pocket
//! ID via oauth2-proxy, APISIX, etc.) that authenticates the user and forwards their
//! identity in a header. `NK_AUTH_MODE` selects how the human identity is resolved:
//!
//! * `trust-header` (default) — read the email from `NK_AUTH_HEADER`
//!   (default `x-auth-request-email`, what oauth2-proxy sets). **Only safe if the
//!   proxy strips this header from inbound client requests.** For defence in depth,
//!   set `NK_PROXY_SHARED_SECRET` (and have the proxy send it in `NK_PROXY_SECRET_HEADER`,
//!   default `x-internal-auth`): identity headers are then trusted only when that
//!   secret matches, so a forged `x-auth-request-email` alone cannot impersonate a
//!   user. Without it the server still runs but logs a warning at startup.
//! * `dev` — a fixed identity from `NK_DEV_USER` (local development only).
//! * `none` — no human identity; only device-token (`api-secret`/Bearer) auth works.
//!
//! Device-token auth always works regardless of mode (handled inside `ApiService`).

mod connector;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::http::{HeaderMap, Method as HttpMethod, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{any, get};
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

use nightknight_api::{
    ApiRequest, ApiResponse, ApiService, ApnsConfig, EdgeIdentity, Headers, Method, PrincipalKind,
};
use nightknight_storage::Storage;
use nightknight_store_sql::SqlStore;

/// How human identity is resolved from the request (device tokens are separate).
#[derive(Clone)]
enum AuthMode {
    TrustHeader {
        header: String,
        groups_header: String,
        /// Optional shared secret the upstream proxy must present (header name,
        /// expected value). When set, identity headers are trusted ONLY if this
        /// matches — so a forged `x-auth-request-email` on its own cannot impersonate
        /// a user even if the proxy fails to strip inbound auth headers.
        proxy_secret: Option<(String, String)>,
    },
    Dev { subject: String, groups: Vec<String> },
    None,
}

/// Constant-time string comparison for the proxy shared secret (avoids leaking how
/// many leading bytes matched). The length is allowed to leak.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Split a groups string (comma- or whitespace-separated) into trimmed entries.
fn split_groups(s: &str) -> Vec<String> {
    s.split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Clone)]
struct AppState {
    service: Arc<ApiService<SqlStore>>,
    auth: AuthMode,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let database_url = env_or("NK_DATABASE_URL", "sqlite://nightknight.db?mode=rwc");
    let bind = env_or("NK_BIND", "0.0.0.0:8787");
    let web_dir = env_or("NK_WEB_DIR", "web/dist");

    // Group requirement, enforced in-app (in addition to any upstream gate). When
    // set, human principals must carry this group; service/device tokens are exempt.
    let required_group = std::env::var("NK_REQUIRED_GROUP").ok().filter(|s| !s.is_empty());
    // In dev mode the demo user gets the required group by default so local use works;
    // override with NK_DEV_GROUPS to exercise the deny path.
    let dev_groups = match std::env::var("NK_DEV_GROUPS") {
        Ok(s) if !s.is_empty() => split_groups(&s),
        _ => required_group.clone().into_iter().collect(),
    };
    let auth = match env_or("NK_AUTH_MODE", "trust-header").as_str() {
        "dev" => AuthMode::Dev {
            subject: env_or("NK_DEV_USER", "dev@localhost"),
            groups: dev_groups,
        },
        "none" => AuthMode::None,
        _ => {
            // Optional defence: require the proxy to present a shared secret before we
            // trust any identity header. Without it we still work (back-compat), but a
            // misconfigured proxy that forwards client-supplied auth headers would be
            // spoofable — so warn loudly.
            let proxy_secret = std::env::var("NK_PROXY_SHARED_SECRET")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|secret| (env_or("NK_PROXY_SECRET_HEADER", "x-internal-auth"), secret));
            if proxy_secret.is_none() {
                tracing::warn!(
                    "NK_AUTH_MODE=trust-header without NK_PROXY_SHARED_SECRET: identity \
                     headers are trusted unconditionally. Ensure the upstream proxy strips \
                     inbound auth headers from client requests, or set NK_PROXY_SHARED_SECRET \
                     (and have the proxy send it) so a forged header alone cannot impersonate."
                );
            }
            AuthMode::TrustHeader {
                header: env_or("NK_AUTH_HEADER", "x-auth-request-email"),
                groups_header: env_or("NK_AUTH_GROUPS_HEADER", "x-auth-request-groups"),
                proxy_secret,
            }
        }
    };

    tracing::info!(%database_url, %bind, %web_dir, ?required_group, "starting NightKnight server");

    let store = SqlStore::connect(&database_url)
        .await
        .expect("connect to database");
    store.migrate().await.expect("apply database schema");
    // Connector encryption key (enables per-user CGM connector credentials + sync).
    let connector_key = std::env::var("NK_CONNECTOR_KEY")
        .ok()
        .and_then(|v| nightknight_crypto::parse_key(&v).ok());
    // One-time legacy-subject migration (re-key bare-email users → namespaced). Enable
    // for the migration window only.
    let migrate_legacy = std::env::var("NK_MIGRATE_LEGACY_SUBJECTS")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    // Silent-push (APNs) provider config. The container reads the same values as the
    // Worker; with all three secrets present it sends a silent push when a connector sync
    // brings in fresh readings. The container's reqwest transport speaks HTTP/2, so unlike
    // `wrangler dev` it can also drive APNs locally for end-to-end testing (see
    // docs/SILENT-PUSH.md). Push is disabled (but tokens still register) when unset.
    let apns = ApnsConfig::from_parts(
        std::env::var("APNS_KEY_P8").ok(),
        std::env::var("APNS_KEY_ID").ok(),
        std::env::var("APNS_TEAM_ID").ok(),
        std::env::var("APNS_BUNDLE_ID").ok(),
        std::env::var("APNS_DEFAULT_ENV").ok(),
    );
    if apns.is_some() {
        tracing::info!("APNs silent push enabled");
    }
    let service = Arc::new(
        ApiService::new(store)
            .require_group(required_group)
            .with_connector_key(connector_key)
            .with_apns(apns)
            .migrate_legacy_subjects(migrate_legacy),
    );

    // Start the optional CGM cloud connector (Dexcom Share / LibreLinkUp) poller.
    connector::spawn(service.clone());

    let state = AppState { service, auth };

    // SPA: serve static files, falling back to index.html for client-side routes.
    let index = format!("{web_dir}/index.html");
    let spa = ServeDir::new(&web_dir).not_found_service(ServeFile::new(index));

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/{*rest}", any(api_handler))
        .fallback_service(spa)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .expect("bind listener");
    tracing::info!("listening on http://{bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

/// Translate an axum request into an [`ApiRequest`], dispatch it, and translate the
/// [`ApiResponse`] back.
async fn api_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    method: HttpMethod,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let req = build_request(&method, &uri, &headers, body.to_vec());
    let edge = resolve_edge(&state.auth, &headers);
    let resp = state.service.handle(req, now_ms(), edge).await;
    to_axum_response(resp)
}

fn build_request(method: &HttpMethod, uri: &Uri, headers: &HeaderMap, body: Vec<u8>) -> ApiRequest {
    let query = uri
        .query()
        .map(|q| {
            q.split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let header_pairs = headers.iter().filter_map(|(k, v)| {
        v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string()))
    });
    ApiRequest {
        method: Method::parse(method.as_str()),
        path: uri.path().to_string(),
        query,
        headers: Headers::from_pairs(header_pairs),
        body,
    }
}

/// Resolve the human identity from the configured auth mode. Returns `None` when no
/// human identity is present (device-token-only requests).
fn resolve_edge(auth: &AuthMode, headers: &HeaderMap) -> Option<EdgeIdentity> {
    match auth {
        AuthMode::TrustHeader { header, groups_header, proxy_secret } => {
            // If a shared secret is configured, the proxy must present it; otherwise we
            // do not trust the identity headers at all (fail closed → device-token-only).
            if let Some((secret_header, expected)) = proxy_secret {
                let presented = headers.get(secret_header.as_str()).and_then(|v| v.to_str().ok());
                if !presented.map(|p| ct_eq(p, expected)).unwrap_or(false) {
                    return None;
                }
            }
            headers
            .get(header.as_str())
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|email| EdgeIdentity {
                // Normalize the tenancy key (the proxy has no immutable `sub` to offer,
                // so email is the key here): trim + lowercase so casing can't fork a
                // user into two tenants. The runtime namespaces it as `human:…`.
                subject: email.to_ascii_lowercase(),
                kind: PrincipalKind::Human,
                display_name: headers
                    .get("x-auth-request-user")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .or_else(|| Some(email.to_string())),
                email: Some(email.to_string()),
                groups: headers
                    .get(groups_header.as_str())
                    .and_then(|v| v.to_str().ok())
                    .map(split_groups)
                    .unwrap_or_default(),
            })
        }
        AuthMode::Dev { subject, groups } => Some(EdgeIdentity {
            subject: subject.clone(),
            kind: PrincipalKind::Human,
            display_name: None,
            email: Some(subject.clone()),
            groups: groups.clone(),
        }),
        AuthMode::None => None,
    }
}

fn to_axum_response(resp: ApiResponse) -> Response {
    let mut builder = Response::builder().status(StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));
    for (k, v) in resp.headers {
        builder = builder.header(k, v);
    }
    builder
        .body(axum::body::Body::from(resp.body))
        .unwrap_or_else(|_| Response::new(axum::body::Body::empty()))
}

/// Wait for Ctrl-C / SIGTERM for graceful shutdown.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}
