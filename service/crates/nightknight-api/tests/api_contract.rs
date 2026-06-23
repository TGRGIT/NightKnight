//! API contract tests — the behaviour every NightKnight client relies on, exercised
//! end-to-end through [`ApiService::handle`] against an in-memory SQLite store.
//!
//! These are written to be read by a human reviewer: each test states the real-world
//! scenario it protects (an uploader posting a reading, a follower app reading data,
//! a security guarantee) and asserts the externally-visible behaviour.

use nightknight_api::{ApiRequest, ApiResponse, ApiService, EdgeIdentity, Headers, Method, PrincipalKind};
use nightknight_storage::Storage;
use nightknight_store_sql::SqlStore;
use serde_json::{json, Value};

/// A fixed "now" so timestamps are deterministic (2023-11-14T22:13:20Z).
const NOW: i64 = 1_700_000_000_000;

async fn service() -> ApiService<SqlStore> {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    ApiService::new(store)
}

/// A human logged in through Cloudflare Access / OIDC (no groups).
fn human(email: &str) -> EdgeIdentity {
    EdgeIdentity {
        subject: email.to_string(),
        kind: PrincipalKind::Human,
        display_name: None,
        groups: Vec::new(),
    }
}

/// A human with explicit group memberships.
fn human_in(email: &str, groups: &[&str]) -> EdgeIdentity {
    EdgeIdentity {
        subject: email.to_string(),
        kind: PrincipalKind::Human,
        display_name: None,
        groups: groups.iter().map(|s| s.to_string()).collect(),
    }
}

/// A machine principal (Cloudflare Access service token).
fn service_token(name: &str) -> EdgeIdentity {
    EdgeIdentity {
        subject: name.to_string(),
        kind: PrincipalKind::Service,
        display_name: None,
        groups: Vec::new(),
    }
}

/// A service that requires human principals to be in `group`.
async fn service_requiring_group(group: &str) -> ApiService<SqlStore> {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    ApiService::new(store).require_group(Some(group.to_string()))
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

fn request(method: &str, full_path: &str, headers: &[(&str, &str)], body: Value) -> ApiRequest {
    let (path, query) = match full_path.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (full_path.to_string(), vec![]),
    };
    let body = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&body).unwrap()
    };
    ApiRequest {
        method: Method::parse(method),
        path,
        query,
        headers: Headers::from_pairs(headers.iter().map(|(k, v)| (k.to_string(), v.to_string()))),
        body,
    }
}

fn body_json(resp: &ApiResponse) -> Value {
    serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
}

/// An SGV entry body timestamped shortly before `NOW`.
fn sgv(mgdl: i64) -> Value {
    json!({ "type": "sgv", "date": NOW - 60_000, "sgv": mgdl, "direction": "Flat", "device": "test" })
}

/// SCENARIO: an uploader POSTs a glucose reading via the legacy v1 API and a follower
/// app reads it back. This is the bread-and-butter Nightscout interaction.
#[tokio::test]
async fn nightscout_v1_round_trip() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));

    let post = svc
        .handle(
            request("POST", "/api/v1/entries", &[], json!([sgv(120)])),
            NOW,
            edge.clone(),
        )
        .await;
    assert_eq!(post.status, 200, "upload accepted");

    let get = svc
        .handle(request("GET", "/api/v1/entries.json", &[], Value::Null), NOW, edge)
        .await;
    assert_eq!(get.status, 200);
    let arr = body_json(&get);
    assert_eq!(arr.as_array().unwrap().len(), 1);
    assert_eq!(arr[0]["sgv"], 120);
    assert!(arr[0]["_id"].is_string(), "legacy clients expect an _id");
}

/// SCENARIO: a device token authenticates in every form a real client might use —
/// modern raw token (Bearer or api-secret) and a legacy SHA-1-hashed api-secret
/// (what xDrip+ sends). All three must reach the same user's data.
#[tokio::test]
async fn device_token_authenticates_in_all_forms() {
    let svc = service().await;
    let edge = human("alice@cooney.be");

    // Alice mints an uploader token with create+read scope.
    let mk = svc
        .handle(
            request(
                "POST",
                "/api/v4/tokens",
                &[],
                json!({ "name": "phone", "scopes": ["api:entries:create", "api:entries:read"] }),
            ),
            NOW,
            Some(edge),
        )
        .await;
    assert_eq!(mk.status, 201);
    let raw = body_json(&mk)["token"].as_str().unwrap().to_string();

    // Upload using the raw token in the api-secret header (no edge identity).
    let up = svc
        .handle(
            request("POST", "/api/v1/entries", &[("api-secret", &raw)], json!([sgv(100)])),
            NOW,
            None,
        )
        .await;
    assert_eq!(up.status, 200, "raw token via api-secret uploads");

    // Read back using the raw token as a Bearer credential.
    let bearer = format!("Bearer {raw}");
    let rd = svc
        .handle(
            request("GET", "/api/v1/entries", &[("authorization", &bearer)], Value::Null),
            NOW,
            None,
        )
        .await;
    assert_eq!(rd.status, 200);
    assert_eq!(body_json(&rd).as_array().unwrap().len(), 1);

    // Legacy xDrip+ form: SHA-1 hex of the raw token in api-secret.
    let sha1 = sha1_hex(&raw);
    let legacy = svc
        .handle(
            request("GET", "/api/v1/entries", &[("api-secret", &sha1)], Value::Null),
            NOW,
            None,
        )
        .await;
    assert_eq!(legacy.status, 200, "legacy SHA-1 api-secret authenticates");
    assert_eq!(body_json(&legacy).as_array().unwrap().len(), 1);
}

/// SECURITY: a credential supplied in the URL query string must NEVER authenticate.
/// This is the deliberate hardening over legacy Nightscout's `?token=` / `?secret=`.
#[tokio::test]
async fn credentials_are_never_accepted_in_the_query_string() {
    let svc = service().await;
    let edge = human("alice@cooney.be");
    let mk = svc
        .handle(
            request("POST", "/api/v4/tokens", &[], json!({ "scopes": ["api:entries:read"] })),
            NOW,
            Some(edge),
        )
        .await;
    let raw = body_json(&mk)["token"].as_str().unwrap().to_string();

    // Same token, but in the query string and with no headers → must be rejected.
    let path = format!("/api/v1/entries?token={raw}&secret={raw}");
    let resp = svc.handle(request("GET", &path, &[], Value::Null), NOW, None).await;
    assert_eq!(resp.status, 401, "query-string credentials are ignored");
}

/// SCENARIO: a read-only follower token must not be able to write. Least-privilege
/// scoping protects a caregiver's token from being abused to alter data.
#[tokio::test]
async fn read_only_token_cannot_write() {
    let svc = service().await;
    let edge = human("alice@cooney.be");
    let mk = svc
        .handle(
            request("POST", "/api/v4/tokens", &[], json!({ "scopes": ["api:entries:read"] })),
            NOW,
            Some(edge),
        )
        .await;
    let raw = body_json(&mk)["token"].as_str().unwrap().to_string();

    let resp = svc
        .handle(
            request("POST", "/api/v1/entries", &[("api-secret", &raw)], json!([sgv(100)])),
            NOW,
            None,
        )
        .await;
    assert_eq!(resp.status, 403, "create denied without create scope");
}

/// SCENARIO: re-posting the same reading (an uploader retry) deduplicates instead of
/// duplicating, and v3 wraps results in `{ status, result }`. `version` needs no auth.
#[tokio::test]
async fn v3_envelope_dedup_and_unauthenticated_version() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));

    let first = svc
        .handle(request("POST", "/api/v3/entries", &[], sgv(140)), NOW, edge.clone())
        .await;
    assert_eq!(first.status, 201, "first create");
    let second = svc
        .handle(request("POST", "/api/v3/entries", &[], sgv(140)), NOW, edge.clone())
        .await;
    assert_eq!(second.status, 200, "identical re-post deduplicates to an update");

    let list = svc
        .handle(request("GET", "/api/v3/entries", &[], Value::Null), NOW, edge)
        .await;
    let body = body_json(&list);
    assert_eq!(body["status"], 200, "v3 envelope present");
    assert_eq!(body["result"].as_array().unwrap().len(), 1, "no duplicate");

    // version requires no authentication at all.
    let ver = svc.handle(request("GET", "/api/v3/version", &[], Value::Null), NOW, None).await;
    assert_eq!(ver.status, 200);
    assert!(body_json(&ver)["result"]["version"].is_string());
}

/// SCENARIO: mg/dL and mmol/L readings in one stream are analysed together. A reading
/// entered in mmol/L must contribute to Time-in-Range exactly like its mg/dL twin.
#[tokio::test]
async fn mixed_units_current_and_analytics() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));

    // One mg/dL reading (in range) and one mmol/L reading (~in range), different times.
    let mgdl = json!({ "type": "sgv", "date": NOW - 600_000, "sgv": 120, "device": "t" });
    let mmol = json!({ "type": "sgv", "date": NOW - 60_000, "sgv": 6.0, "units": "mmol", "device": "t" });
    svc.handle(request("POST", "/api/v1/entries", &[], json!([mgdl, mmol])), NOW, edge.clone())
        .await;

    // `current` returns the latest (the mmol one), reported in both units.
    let cur = svc.handle(request("GET", "/api/v4/current", &[], Value::Null), NOW, edge.clone()).await;
    let c = body_json(&cur);
    assert_eq!(c["current"]["mmol"], 6.0, "mmol value preserved");
    assert_eq!(c["current"]["mgdl"], 108, "mmol shown in mg/dL too (6.0 × 18.0156 ≈ 108)");

    // Analytics see both readings; both are in range → 100% TIR.
    let an = svc.handle(request("GET", "/api/v4/analytics?hours=24", &[], Value::Null), NOW, edge).await;
    let a = body_json(&an);
    assert_eq!(a["n"], 2);
    assert_eq!(a["timeInRange"]["inRangePct"], 100.0);
}

/// SECURITY: two users behind the gate cannot see each other's data.
#[tokio::test]
async fn users_are_isolated_end_to_end() {
    let svc = service().await;
    svc.handle(request("POST", "/api/v1/entries", &[], json!([sgv(111)])), NOW, Some(human("alice@cooney.be")))
        .await;
    svc.handle(request("POST", "/api/v1/entries", &[], json!([sgv(222)])), NOW, Some(human("bob@cooney.be")))
        .await;

    let bob = svc
        .handle(request("GET", "/api/v1/entries", &[], Value::Null), NOW, Some(human("bob@cooney.be")))
        .await;
    let arr = body_json(&bob);
    assert_eq!(arr.as_array().unwrap().len(), 1, "bob sees only his own reading");
    assert_eq!(arr[0]["sgv"], 222);
}

/// SECURITY (defence in depth): when a required group is configured, the app itself
/// refuses a human who lacks it — not relying on the Cloudflare Access edge alone.
/// A member is admitted; a machine service token is exempt (the "API key" path).
#[tokio::test]
async fn app_enforces_group_membership_for_humans() {
    let svc = service_requiring_group("night_knight_users").await;

    // Human NOT in the group → 403, even though the edge "let them in".
    let denied = svc
        .handle(request("GET", "/api/v4/me", &[], Value::Null), NOW, Some(human_in("outsider@cooney.be", &[])))
        .await;
    assert_eq!(denied.status, 403, "human outside the group is refused by the app");

    // Human IN the group → allowed.
    let ok = svc
        .handle(
            request("GET", "/api/v4/me", &[], Value::Null),
            NOW,
            Some(human_in("member@cooney.be", &["other_group", "night_knight_users"])),
        )
        .await;
    assert_eq!(ok.status, 200, "group member is admitted");

    // A machine service token is the "service account + API key" path → exempt.
    let machine = svc
        .handle(request("GET", "/api/v4/status", &[], Value::Null), NOW, Some(service_token("uploader.svc")))
        .await;
    assert_eq!(machine.status, 200, "service token bypasses the group requirement");
}

/// A device token (API key) authenticates regardless of the group requirement — it is
/// minted by a member, then used machine-to-machine with no human identity attached.
#[tokio::test]
async fn device_token_bypasses_group_requirement() {
    let svc = service_requiring_group("night_knight_users").await;

    // A member mints an uploader token.
    let mk = svc
        .handle(
            request("POST", "/api/v4/tokens", &[], json!({ "scopes": ["api:entries:read"] })),
            NOW,
            Some(human_in("member@cooney.be", &["night_knight_users"])),
        )
        .await;
    assert_eq!(mk.status, 201);
    let raw = body_json(&mk)["token"].as_str().unwrap().to_string();

    // The token works with no edge identity at all — the group check never applies.
    let rd = svc
        .handle(request("GET", "/api/v1/entries", &[("api-secret", &raw)], Value::Null), NOW, None)
        .await;
    assert_eq!(rd.status, 200, "API-key path is not group-gated");
}

/// A mock CGM cloud that answers the Dexcom Share flow with one canned reading —
/// lets us exercise the whole per-user connector loop without a network.
struct MockDexcom {
    reading_ms: i64,
}

#[async_trait::async_trait]
impl nightknight_connectors::HttpClient for MockDexcom {
    async fn send(
        &self,
        req: nightknight_connectors::HttpReq,
    ) -> Result<nightknight_connectors::HttpResp, nightknight_connectors::ConnectorError> {
        let body: Vec<u8> = if req.url.contains("AuthenticatePublisherAccount") {
            br#""account-id-123""#.to_vec()
        } else if req.url.contains("LoginPublisherAccountById") {
            br#""session-id-456""#.to_vec()
        } else if req.url.contains("ReadPublisherLatestGlucoseValues") {
            format!(r#"[{{"WT":"Date({})","Value":132,"Trend":"Flat"}}]"#, self.reading_ms).into_bytes()
        } else {
            b"[]".to_vec()
        };
        Ok(nightknight_connectors::HttpResp { status: 200, body })
    }
}

/// SCENARIO: a user adds Dexcom credentials in the UI; the scheduler then syncs them.
/// Proves the whole loop — credentials are encrypted at rest (never echoed), the
/// secret-free list is returned, and a sync ingests the vendor's reading.
#[tokio::test]
async fn connector_credentials_encrypt_and_sync() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store).with_connector_key(Some([42u8; 32]));
    let edge = Some(human("member@cooney.be"));

    // Add Dexcom credentials via the UI endpoint.
    let put = svc
        .handle(
            request(
                "PUT",
                "/api/v4/connectors/dexcom",
                &[],
                json!({ "username": "u@x", "password": "s3cret", "region": "ous" }),
            ),
            NOW,
            edge.clone(),
        )
        .await;
    assert_eq!(put.status, 200);
    let view = body_json(&put);
    assert_eq!(view["provider"], "dexcom");
    assert_eq!(view["region"], "ous");
    // The secret must never be returned to the client.
    let raw = String::from_utf8(put.body.clone()).unwrap();
    assert!(!raw.contains("s3cret"), "password must not be echoed");

    // The listing is also secret-free.
    let list = svc
        .handle(request("GET", "/api/v4/connectors", &[], Value::Null), NOW, edge.clone())
        .await;
    assert!(!String::from_utf8(list.body.clone()).unwrap().contains("s3cret"));

    // Run a sync against the mock cloud → the reading is ingested for this user.
    let reading_ms = NOW - 120_000;
    let ingested = svc
        .sync_connectors(&MockDexcom { reading_ms }, 60, NOW)
        .await
        .unwrap();
    assert_eq!(ingested, 1, "one reading ingested");

    // The user can now read it back.
    let entries = svc
        .handle(request("GET", "/api/v1/entries", &[], Value::Null), NOW, edge)
        .await;
    let arr = body_json(&entries);
    assert_eq!(arr.as_array().unwrap().len(), 1);
    assert_eq!(arr[0]["sgv"], 132);
    assert_eq!(arr[0]["device"], "dexcom-share");
}

/// Local SHA-1 hex helper (mirrors what a legacy uploader computes over the secret).
fn sha1_hex(s: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut out = String::new();
    for b in Sha1::digest(s.as_bytes()) {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
