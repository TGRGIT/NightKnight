//! API contract tests — the behaviour every NightKnight client relies on, exercised
//! end-to-end through [`ApiService::handle`] against an in-memory SQLite store.
//!
//! These are written to be read by a human reviewer: each test states the real-world
//! scenario it protects (an uploader posting a reading, a follower app reading data,
//! a security guarantee) and asserts the externally-visible behaviour.

use nightknight_api::{ApiRequest, ApiResponse, ApiService, EdgeIdentity, Headers, Method, PrincipalKind};
use nightknight_storage::{Collection, Storage, StoredDoc, User};
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
        email: Some(email.to_string()),
        groups: Vec::new(),
    }
}

/// A human with explicit group memberships.
fn human_in(email: &str, groups: &[&str]) -> EdgeIdentity {
    EdgeIdentity {
        subject: email.to_string(),
        kind: PrincipalKind::Human,
        display_name: None,
        email: Some(email.to_string()),
        groups: groups.iter().map(|s| s.to_string()).collect(),
    }
}

/// A machine principal (Cloudflare Access service token).
fn service_token(name: &str) -> EdgeIdentity {
    EdgeIdentity {
        subject: name.to_string(),
        kind: PrincipalKind::Service,
        display_name: None,
        email: None,
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

/// First response header matching `name` (case-insensitive), if present.
fn header<'a>(resp: &'a ApiResponse, name: &str) -> Option<&'a str> {
    resp.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
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

/// SCENARIO: the dashboard's "current" reading prefers the **sensor's own** trend
/// arrow when the latest entry carries one, and labels it in plain language. Here two
/// flat readings would compute to "Flat", but the newest carries the sensor's "DoubleUp"
/// — which must win — and the expanded analytics payload exposes the full metric set.
#[tokio::test]
async fn current_prefers_first_party_trend_and_analytics_is_complete() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    let older = json!({ "type": "sgv", "date": NOW - 300_000, "sgv": 100, "device": "t" });
    let newest =
        json!({ "type": "sgv", "date": NOW - 60_000, "sgv": 100, "direction": "DoubleUp", "device": "t" });
    svc.handle(request("POST", "/api/v1/entries", &[], json!([older, newest])), NOW, edge.clone())
        .await;

    let cur = body_json(&svc.handle(request("GET", "/api/v4/current", &[], Value::Null), NOW, edge.clone()).await);
    assert_eq!(cur["current"]["direction"], "DoubleUp", "first-party sensor trend wins");
    assert_eq!(cur["current"]["trendLabel"], "Rising rapidly");

    let an = body_json(
        &svc.handle(request("GET", "/api/v4/analytics?hours=24", &[], Value::Null), NOW, edge.clone()).await,
    );
    assert_eq!(an["n"], 2);
    assert!(an["sdMgdl"].is_number(), "SD surfaced");
    assert!(an["uGmiPercent"].is_number(), "uGMI (the preferred A1c estimate) surfaced");
    assert!(an["coverage"]["percentActive"].is_number(), "data sufficiency surfaced");
    assert!(an["gri"]["value"].is_number() && an["gri"]["zone"].is_string(), "GRI surfaced");
    assert!(an["variability"]["jIndex"].is_number(), "advanced variability surfaced");
    assert!(an["episodes"]["low"]["count"].is_number(), "episodes surfaced");
    assert_eq!(an["patterns"].as_array().unwrap().len(), 4, "four time-of-day periods");

    let agp = body_json(&svc.handle(request("GET", "/api/v4/agp?days=14", &[], Value::Null), NOW, edge).await);
    assert_eq!(agp["bins"].as_array().unwrap().len(), 96, "AGP has 96 fifteen-minute bins");
}

/// SCENARIO: the Data view asks "which days actually have readings, and what do they
/// look like?". `/days` lists every local day with data and its reading count (the
/// importer-verification spine), and decorates recent days with a per-day glucose
/// summary led by uGMI. The headline window stats summarise the loaded window.
#[tokio::test]
async fn days_view_lists_coverage_and_recent_stats() {
    const DAY_MS: i64 = 86_400_000;
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    // Five readings across three local (UTC) days: 2 today, 2 yesterday, 1 two days ago.
    let entries = json!([
        { "type": "sgv", "date": NOW - 60_000, "sgv": 120, "device": "t" },
        { "type": "sgv", "date": NOW - 120_000, "sgv": 140, "device": "t" },
        { "type": "sgv", "date": NOW - DAY_MS - 60_000, "sgv": 90, "device": "t" },
        { "type": "sgv", "date": NOW - DAY_MS - 120_000, "sgv": 200, "device": "t" },
        { "type": "sgv", "date": NOW - 2 * DAY_MS - 60_000, "sgv": 110, "device": "t" },
    ]);
    svc.handle(request("POST", "/api/v1/entries", &[], entries), NOW, edge.clone()).await;

    let resp = svc.handle(request("GET", "/api/v4/days?tzOffset=0", &[], Value::Null), NOW, edge).await;
    assert_eq!(resp.status, 200);
    let b = body_json(&resp);
    assert_eq!(b["totalDays"], 3, "three distinct days have data");
    assert_eq!(b["totalReadings"], 5, "five readings across the whole history");
    let days = b["days"].as_array().unwrap();
    assert_eq!(days.len(), 3);
    assert_eq!(days[0]["n"], 2, "newest day first, with its reading count");
    assert!(days[0]["date"].as_str().unwrap().starts_with("2023-11"), "ISO day label");
    // Recent days carry a per-day glucose summary led by uGMI.
    assert!(days[0]["uGmiPercent"].is_number(), "recent day has uGMI");
    assert!(days[0]["meanMgdl"].is_number());
    assert!(days[0]["timeInRange"]["inRangePct"].is_number());
    // Headline window stats are present and uGMI-led.
    assert_eq!(b["windowStats"]["n"], 5);
    assert!(b["windowStats"]["uGmiPercent"].is_number());
}

/// REGRESSION (issue #17 follow-up, PR #28): per-day coverage must be judged against
/// **each day's own** sensor cadence, not one global rate. A genuinely complete day on a
/// slower sensor (5-min, n≈288) imported alongside a faster recent era (1-min, ≈1440/day)
/// used to render at ~20% coverage (288/1440) — lightest band, dragging down the average —
/// even though the day was fully covered. It must now expect ~288 and read ~100%.
#[tokio::test]
async fn days_view_coverage_is_per_day_not_a_global_cadence() {
    const DAY_MS: i64 = 86_400_000;
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));

    // An older full day on a 5-minute sensor: 288 readings across one UTC day.
    let old_base = 19_670 * DAY_MS;
    let mut entries: Vec<Value> = (0..288)
        .map(|i| json!({ "type": "sgv", "date": old_base + i * 5 * 60_000, "sgv": 120, "device": "dex" }))
        .collect();
    // A recent day on a 1-minute sensor, dense enough that the *global* inferred cadence is
    // 1-min (≈1440/day) — the value the old code wrongly applied to every day.
    let new_base = 19_675 * DAY_MS;
    entries.extend((0..400).map(|i| {
        json!({ "type": "sgv", "date": new_base + i * 60_000, "sgv": 120, "device": "libre" })
    }));
    svc.handle(request("POST", "/api/v1/entries", &[], json!(entries)), NOW, edge.clone()).await;

    let b = body_json(
        &svc.handle(request("GET", "/api/v4/days?tzOffset=0", &[], Value::Null), NOW, edge).await,
    );
    // The global cadence is the fast (recent) 1-minute rate — the trap the old code fell in.
    assert_eq!(b["cadenceMs"], 60_000, "global cadence inferred as the recent 1-min era");
    assert_eq!(b["expectedPerDay"], 1440, "global expectation is the fast 1-min rate");

    let days = b["days"].as_array().unwrap();
    let old_day = days.iter().find(|d| d["n"] == 288).expect("the 5-min day is listed");
    let new_day = days.iter().find(|d| d["n"] == 400).expect("the 1-min day is listed");

    // The old day carries its OWN expectation (~288), so it reads as fully covered — not
    // the ~20% it showed when divided by the global 1440.
    assert_eq!(old_day["expectedPerDay"], 288, "5-min day expects ~288, not the global 1440");
    let old_cov = old_day["n"].as_f64().unwrap() / old_day["expectedPerDay"].as_f64().unwrap();
    assert!(old_cov >= 0.95, "a complete 5-min day reads ~100% covered, got {old_cov}");

    // The recent day keeps the 1-min expectation (a partial day reads as partial coverage).
    assert_eq!(new_day["expectedPerDay"], 1440, "1-min day keeps its own fast expectation");
}

/// REGRESSION (validation finding #3): "% time active" must use the device's actual
/// cadence, not a hardcoded 5 minutes. A flawless 14-day FreeStyle Libre (15-min historic
/// cadence) used to read ~33% active and "limited data"; it must now read ~100% / sufficient.
#[tokio::test]
async fn analytics_coverage_uses_inferred_cadence_not_a_fixed_5min() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    const Q: i64 = 15 * 60_000; // 15-minute cadence
    let mut entries = Vec::new();
    let mut tms = NOW - 14 * 24 * 3_600_000;
    while tms <= NOW {
        entries.push(json!({ "type": "sgv", "date": tms, "sgv": 120, "device": "libre" }));
        tms += Q;
    }
    svc.handle(request("POST", "/api/v1/entries", &[], json!(entries)), NOW, edge.clone()).await;
    let a = body_json(
        &svc.handle(request("GET", "/api/v4/analytics?hours=336", &[], Value::Null), NOW, edge).await,
    );
    assert_eq!(a["cadenceMs"], 15 * 60_000, "cadence inferred as 15 min");
    let active = a["coverage"]["percentActive"].as_f64().unwrap();
    assert!(active > 90.0, "perfect 15-min data should read ~100% active, got {active}");
    assert_eq!(a["coverage"]["sufficient"], true, "14 days of full 15-min data is sufficient");
}

/// REGRESSION (validation finding #4): episode detection must work at a sparse cadence.
/// A real 3-hour low sampled hourly used to be invisible (every 1-h gap exceeded the fixed
/// 30-min break); with a cadence-derived break it is detected.
#[tokio::test]
async fn analytics_detects_episodes_at_sparse_hourly_cadence() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    let h = 3_600_000i64;
    let base = NOW - 6 * h;
    let vals = [120, 120, 60, 60, 60, 120, 120]; // a 3-hour low in the middle
    let entries: Vec<Value> = vals
        .iter()
        .enumerate()
        .map(|(i, &v)| json!({ "type": "sgv", "date": base + i as i64 * h, "sgv": v, "device": "t" }))
        .collect();
    svc.handle(request("POST", "/api/v1/entries", &[], json!(entries)), NOW, edge.clone()).await;
    let a = body_json(
        &svc.handle(request("GET", "/api/v4/analytics?hours=24", &[], Value::Null), NOW, edge).await,
    );
    assert_eq!(a["cadenceMs"], h, "cadence inferred as hourly");
    assert!(
        a["episodes"]["low"]["count"].as_i64().unwrap() >= 1,
        "a 3-hour low at hourly cadence must be detected, got {}",
        a["episodes"]["low"]["count"]
    );
}

/// REGRESSION (validation finding #2): the headline mean / A1c estimate is time-weighted,
/// so a dense burst (duplicate cluster / backfill overlap) can't drag it. 60 minutes of
/// 1-min readings at 100 plus a 20-second burst of 200: the count mean is ~177, but the
/// reported (time-weighted) mean stays near 100.
#[tokio::test]
async fn analytics_headline_mean_is_time_weighted() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    let mut entries: Vec<Value> = (0..60)
        .map(|m| json!({ "type": "sgv", "date": NOW - 60 * 60_000 + m * 60_000, "sgv": 100, "device": "t" }))
        .collect();
    let burst = NOW - 30 * 60_000;
    for s in 0..200i64 {
        entries.push(json!({ "type": "sgv", "date": burst + s * 100, "sgv": 200, "device": "burst" }));
    }
    svc.handle(request("POST", "/api/v1/entries", &[], json!(entries)), NOW, edge.clone()).await;
    let a = body_json(
        &svc.handle(request("GET", "/api/v4/analytics?hours=2", &[], Value::Null), NOW, edge).await,
    );
    let mean = a["meanMgdl"].as_f64().unwrap();
    assert!(mean < 120.0, "time-weighted headline mean must resist the burst, got {mean}");
    // uGMI follows the corrected mean (not the count-inflated one).
    assert!(a["uGmiPercent"].as_f64().unwrap() < 6.5, "uGMI tracks the time-weighted mean");
}

/// A window with no readings must not fabricate a "perfect" score — every metric,
/// including the Glycemia Risk Index (where 0 = best), reports null rather than a
/// best-possible value.
#[tokio::test]
async fn empty_window_analytics_are_null_not_fabricated() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    let an = body_json(
        &svc.handle(request("GET", "/api/v4/analytics?hours=24", &[], Value::Null), NOW, edge).await,
    );
    assert_eq!(an["n"], 0);
    assert!(an["meanMgdl"].is_null(), "mean is null on empty");
    assert!(an["gri"]["value"].is_null(), "GRI must be null, not a fabricated 0 / zone A");
    assert!(an["gri"]["zone"].is_null());
}

/// SCENARIO: a user exports their readings as CSV to share with a clinician or open in a
/// spreadsheet. The download is a labelled CSV (metadata preamble + header + one row per
/// reading, oldest first, in both units) with an attachment filename stamped with the
/// range, scoped to the caller's own readings.
#[tokio::test]
async fn export_csv_downloads_labelled_raw_readings() {
    const DAY_MS: i64 = 86_400_000;
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    let entries = json!([
        { "type": "sgv", "date": NOW - 60_000, "sgv": 180, "device": "t" },
        { "type": "sgv", "date": NOW - DAY_MS, "sgv": 90, "device": "t" },
    ]);
    svc.handle(request("POST", "/api/v1/entries", &[], entries), NOW, edge.clone()).await;

    let resp = svc
        .handle(request("GET", "/api/v4/export?format=csv&tzOffset=0", &[], Value::Null), NOW, edge)
        .await;
    assert_eq!(resp.status, 200);
    let ctype = header(&resp, "content-type").unwrap();
    assert!(ctype.starts_with("text/csv"), "served as CSV, got {ctype}");
    let disp = header(&resp, "content-disposition").unwrap();
    assert!(disp.contains("attachment; filename=\"nightknight-readings-"), "download prompt: {disp}");
    assert!(disp.ends_with(".csv\""));

    let csv = String::from_utf8(resp.body.clone()).unwrap();
    assert!(csv.contains("# NightKnight glucose export"), "self-describing preamble");
    assert!(csv.contains("# generated:") && csv.contains("# range:"), "labelled with time + range");
    assert!(csv.contains("timestamp,epoch_ms,mg_dL,mmol_L"), "column header");
    // Oldest first: the 90 mg/dL (5.0 mmol/L) row precedes the 180 (10.0) row.
    let data: Vec<&str> = csv.lines().filter(|l| !l.starts_with('#') && l.contains(",")).collect();
    let ninety = data.iter().position(|l| l.contains(",90,")).unwrap();
    let one_eighty = data.iter().position(|l| l.contains(",180,")).unwrap();
    assert!(ninety < one_eighty, "readings are oldest-first");
}

/// SCENARIO: a user exports the computed metric set as JSON. The download bundles the full
/// `/analytics` payload plus the AGP bands, labelled with the range + generation timestamp,
/// and is byte-identical in shape to the live Statistical-Analysis view.
#[tokio::test]
async fn export_json_bundles_the_full_metric_set() {
    const DAY_MS: i64 = 86_400_000;
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    // Two weeks of hourly readings so the metric set is fully populated.
    let mut arr = Vec::new();
    for i in 0..(14 * 24) {
        arr.push(json!({ "type": "sgv", "date": NOW - i * 3_600_000, "sgv": 120 + (i % 40), "device": "t" }));
    }
    svc.handle(request("POST", "/api/v1/entries", &[], Value::Array(arr)), NOW, edge.clone()).await;

    let start = NOW - 14 * DAY_MS;
    let path = format!("/api/v4/export?format=json&start={start}&end={NOW}&tzOffset=0");
    let resp = svc.handle(request("GET", &path, &[], Value::Null), NOW, edge).await;
    assert_eq!(resp.status, 200);
    assert!(header(&resp, "content-type").unwrap().starts_with("application/json"));
    let disp = header(&resp, "content-disposition").unwrap();
    assert!(disp.contains("nightknight-metrics-") && disp.ends_with(".json\""), "download prompt: {disp}");

    let v = body_json(&resp);
    assert_eq!(v["report"], "NightKnight glucose metrics export");
    assert_eq!(v["notMedicalDevice"], true);
    assert_eq!(v["range"]["startMs"], start);
    assert_eq!(v["range"]["endMs"], NOW);
    assert!(v["generated"]["iso"].is_string(), "labelled with a generation timestamp");
    // The full metric set is present under `analytics`, plus AGP bands.
    assert!(v["analytics"]["gri"]["value"].is_number(), "GRI");
    assert!(v["analytics"]["timeInRange"]["inRangePct"].is_number(), "TIR");
    assert!(v["analytics"]["variability"]["mage"].is_number() || v["analytics"]["variability"]["mage"].is_null());
    assert!(v["analytics"]["episodes"]["low"]["count"].is_number(), "episode roll-ups");
    assert_eq!(v["agp"]["bins"].as_array().unwrap().len(), 96, "AGP percentile bands");
}

/// The export is scoped to the caller: Bob's export can never contain Alice's readings,
/// and an unknown `format` is a clean 400 rather than a silent default.
#[tokio::test]
async fn export_is_user_scoped_and_validates_format() {
    let svc = service().await;
    svc.handle(request("POST", "/api/v1/entries", &[], json!([sgv(111)])), NOW, Some(human("alice@cooney.be")))
        .await;

    // Bob has no data; his CSV export carries only the preamble/header, none of Alice's rows.
    let bob = svc
        .handle(request("GET", "/api/v4/export?format=csv", &[], Value::Null), NOW, Some(human("bob@cooney.be")))
        .await;
    assert_eq!(bob.status, 200);
    let csv = String::from_utf8(bob.body.clone()).unwrap();
    assert!(csv.contains("# readings: 0"), "bob sees none of alice's readings");
    assert!(!csv.contains(",111,"));

    let bad = svc
        .handle(request("GET", "/api/v4/export?format=xml", &[], Value::Null), NOW, Some(human("alice@cooney.be")))
        .await;
    assert_eq!(bad.status, 400, "an unknown format is rejected, not silently defaulted");
}

/// REGRESSION: the export path used to issue a single unbounded `search_documents` for
/// the whole window, which 503'd the Cloudflare Worker at ~30k rows. It now walks the
/// window in bounded batches and returns every reading up to `MAX_EXPORT_POINTS`. A
/// 12 000-row seed (well above `EXPORT_BATCH_SIZE = 5_000` and above the old cap of
/// 20 000 that would have been the natural mistake) exercises the multi-batch path and
/// proves all 12 000 rows land in the CSV, ordered oldest-first, with no `truncated`.
#[tokio::test]
async fn export_walks_large_windows_in_batches_without_truncating() {
    const DAY_MS: i64 = 86_400_000;
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    // 12 000 sgv readings at 5-min cadence, ending shortly before `NOW`. Post in
    // 500-row chunks so the JSON body itself stays manageable.
    let cadence_ms = 5 * 60_000i64;
    let mut posted = 0;
    while posted < 12_000 {
        let mut chunk = Vec::with_capacity(500);
        for i in 0..500 {
            let idx = posted + i;
            chunk.push(json!({
                "type": "sgv",
                "date": NOW - 60_000 - idx * cadence_ms,
                "sgv": 100 + (idx % 40),
                "device": "batchtest",
            }));
        }
        svc.handle(request("POST", "/api/v1/entries", &[], Value::Array(chunk)), NOW, edge.clone()).await;
        posted += 500;
    }
    // Ask for the whole window (default 14-day export clamps to 90d, well over the
    // ~42-day span of the seed).
    let start = NOW - 90 * DAY_MS;
    let path = format!("/api/v4/export?format=csv&start={start}&end={NOW}&tzOffset=0");
    let resp = svc.handle(request("GET", &path, &[], Value::Null), NOW, edge).await;
    assert_eq!(resp.status, 200);
    let csv = String::from_utf8(resp.body.clone()).unwrap();
    assert!(csv.contains("# readings: 12000"), "all 12k rows survive the cursor walk");
    assert!(!csv.contains("# truncated"), "under the 100k cap, no truncated marker");
    // Data rows carry both units; count non-comment lines minus the header.
    let data_rows: Vec<&str> = csv.lines().filter(|l| !l.starts_with('#') && l.contains(",100,")).collect();
    assert!(data_rows.len() > 100, "many 100 mg/dL rows present (sanity)");
}

/// The JSON/report export AGGREGATES server-side: it downsamples dense data to at most one
/// reading per adaptive time-bucket, so a window of any density stays bounded (never 503s)
/// while still covering the WHOLE span. Here 2 days of 15-second-cadence data (~11 500
/// readings, 4/min) must collapse to ~1/min (~2 900) with the range still reading 2 days —
/// proving the report covers every day without loading every reading. `?samples=1` returns
/// the compact series the printable report draws its daily-profile thumbnails from.
#[tokio::test]
async fn export_json_downsamples_dense_data_but_covers_full_window() {
    const DAY_MS: i64 = 86_400_000;
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    // 2 days of 15-second-cadence sgv (4 readings/minute) = 11 520 readings. Post in chunks.
    let cadence_ms = 15_000i64;
    let total = 2 * 24 * 60 * 4; // 11 520
    let mut posted = 0;
    while posted < total {
        let mut chunk = Vec::with_capacity(480);
        for i in 0..480 {
            let idx = posted + i;
            chunk.push(json!({
                "type": "sgv",
                "date": NOW - 60_000 - (idx as i64) * cadence_ms,
                "sgv": 100 + (idx % 40),
                "device": "dense",
            }));
        }
        svc.handle(request("POST", "/api/v1/entries", &[], Value::Array(chunk)), NOW, edge.clone()).await;
        posted += 480;
    }

    let start = NOW - 2 * DAY_MS;
    let path = format!("/api/v4/export?format=json&start={start}&end={NOW}&tzOffset=0&samples=1");
    let resp = svc.handle(request("GET", &path, &[], Value::Null), NOW, edge).await;
    assert_eq!(resp.status, 200);
    let v = body_json(&resp);

    // Not truncated — the aggregated path always covers the full window.
    assert_eq!(v["truncated"], false);
    assert_eq!(v["range"]["days"], 2, "the report still spans the full 2-day window");
    assert_eq!(v["sampleBucketMs"], 60_000, "a 2-day window downsamples at 1-minute resolution");

    // The dense 4/min data collapsed to ~1/min: far below the raw 11 520, but ~2 days' worth.
    let n = v["analytics"]["n"].as_i64().unwrap();
    assert!(n < 5_000, "dense data is downsampled, got n={n} (raw was 11520)");
    assert!(n > 2_000, "but the full 2-day span is still represented, got n={n}");

    // The samples series is present, matches n, and is oldest-first `[ms, mgdl]` pairs.
    let samples = v["samples"].as_array().expect("samples array present");
    assert_eq!(samples.len() as i64, n, "one sample per downsampled reading");
    let first = samples.first().unwrap().as_array().unwrap();
    let last = samples.last().unwrap().as_array().unwrap();
    assert!(first[0].as_i64().unwrap() < last[0].as_i64().unwrap(), "samples are oldest-first");
    assert!(first[1].is_number() && last[1].is_number(), "each sample is [epoch_ms, mg_dL]");

    // The metric set is still fully populated over the downsampled series.
    assert!(v["analytics"]["gri"]["value"].is_number(), "GRI computed");
    assert!(v["analytics"]["timeInRange"]["inRangePct"].is_number(), "TIR computed");
    assert_eq!(v["agp"]["bins"].as_array().unwrap().len(), 96, "AGP bands present");

    // Without ?samples the download stays lean (no samples array).
    let lean = svc
        .handle(request("GET", &format!("/api/v4/export?format=json&start={start}&end={NOW}"), &[], Value::Null), NOW, Some(human("alice@cooney.be")))
        .await;
    assert!(body_json(&lean).get("samples").is_none(), "the plain metrics download omits samples");
}

/// The aggregated JSON fetch is PAGINATED (`EXPORT_BATCH_SIZE` per storage query, like the
/// CSV path) so no single subrequest materialises a large result — and it de-dupes to one
/// reading per bucket. This seeds > `EXPORT_BATCH_SIZE` distinct 1-minute buckets so the
/// downsample fetch must span multiple batches, and adds a duplicate-timestamp reading to
/// prove the per-bucket de-dup: the sample count equals the number of distinct minutes, not
/// the raw row count.
#[tokio::test]
async fn export_json_downsample_paginates_and_dedupes_buckets() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    // 7 000 readings at 1-minute cadence = 7 000 distinct minute-buckets (> EXPORT_BATCH_SIZE
    // of 5 000, so the downsample fetch pages at least twice). Window is ~5 days.
    let minutes = 7_000i64;
    let mut posted = 0;
    while posted < minutes {
        let mut chunk = Vec::with_capacity(500);
        for i in 0..500 {
            let idx = posted + i;
            chunk.push(json!({
                "type": "sgv", "date": NOW - 60_000 - idx * 60_000,
                "sgv": 100 + (idx % 50), "device": "d1",
            }));
        }
        svc.handle(request("POST", "/api/v1/entries", &[], Value::Array(chunk)), NOW, edge.clone()).await;
        posted += 500;
    }
    // A second device sampling the SAME instant as an existing reading (distinct identifier,
    // duplicate mills) — the per-bucket de-dup must keep the bucket at one reading.
    let dup_ms = NOW - 60_000 - 100 * 60_000;
    svc.handle(
        request("POST", "/api/v1/entries", &[], json!([{ "type": "sgv", "date": dup_ms, "sgv": 200, "device": "d2" }])),
        NOW, edge.clone(),
    ).await;

    let start = NOW - 6 * 86_400_000;
    let path = format!("/api/v4/export?format=json&start={start}&end={NOW}&tzOffset=0&samples=1");
    let v = body_json(&svc.handle(request("GET", &path, &[], Value::Null), NOW, edge).await);

    // Exactly one sample per distinct minute-bucket: 7 000, NOT 7 001 (the duplicate collapsed).
    let n = v["analytics"]["n"].as_i64().unwrap();
    assert_eq!(n, 7_000, "one reading per minute-bucket across batches, duplicate timestamp de-duped");
    assert_eq!(v["samples"].as_array().unwrap().len() as i64, n, "samples match the bucket count");
    assert_eq!(v["sampleBucketMs"], 60_000, "a ~5-day window stays at 1-minute buckets");
    assert_eq!(v["truncated"], false);
}

/// A stale latest reading gets no trend arrow — a half-hour-old "DoubleUp" would
/// mislead, so `current` reports NONE even though the entry stored an arrow.
#[tokio::test]
async fn current_suppresses_trend_for_a_stale_reading() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    let stale =
        json!({ "type": "sgv", "date": NOW - 30 * 60_000, "sgv": 100, "direction": "DoubleUp", "device": "t" });
    svc.handle(request("POST", "/api/v1/entries", &[], json!([stale])), NOW, edge.clone()).await;
    let cur = body_json(&svc.handle(request("GET", "/api/v4/current", &[], Value::Null), NOW, edge).await);
    assert_eq!(cur["current"]["direction"], "NONE", "a stale reading shows no arrow");
    assert_eq!(cur["current"]["trendLabel"], "", "no-trend states render empty");
}

/// SCENARIO: a user uploads a LibreView CSV export. The readings are parsed, ingested
/// into their own account, and queryable; a re-upload of the same export deduplicates
/// rather than doubling the points.
#[tokio::test]
async fn libreview_csv_import_round_trips_and_dedups() {
    let svc = service().await;
    let edge = Some(human("alice@cooney.be"));
    let csv = "Device,Serial Number,Device Timestamp,Record Type,Historic Glucose mg/dL\n\
               FreeStyle LibreLink,s1,11-14-2023 21:00,0,120\n\
               FreeStyle LibreLink,s1,11-14-2023 21:15,0,118\n";
    let import = |body: &str| ApiRequest {
        method: Method::Post,
        path: "/api/v4/import/libreview".to_string(),
        query: vec![("tzOffset".to_string(), "0".to_string())],
        headers: Headers::from_pairs(Vec::<(String, String)>::new()),
        body: body.as_bytes().to_vec(),
    };

    let resp = svc.handle(import(csv), NOW, edge.clone()).await;
    assert_eq!(resp.status, 200);
    let b = body_json(&resp);
    assert_eq!(b["unit"], "mg/dl");
    assert_eq!(b["imported"], 2, "two readings imported");

    // They read back through the normal entries API.
    let list = svc.handle(request("GET", "/api/v1/entries", &[], Value::Null), NOW, edge.clone()).await;
    assert_eq!(body_json(&list).as_array().unwrap().len(), 2);

    // Re-uploading the same export deduplicates — no new points.
    let resp2 = svc.handle(import(csv), NOW, edge.clone()).await;
    let b2 = body_json(&resp2);
    assert_eq!(b2["imported"], 0, "nothing new on re-import");
    assert_eq!(b2["duplicates"], 2, "both deduped");
    let list2 = svc.handle(request("GET", "/api/v1/entries", &[], Value::Null), NOW, edge).await;
    assert_eq!(body_json(&list2).as_array().unwrap().len(), 2, "still only two points");
}

/// A read-only follower token cannot import data (needs entries:create).
#[tokio::test]
async fn libreview_import_requires_create_scope() {
    let svc = service().await;
    let edge = human("alice@cooney.be");
    let mk = svc
        .handle(request("POST", "/api/v4/tokens", &[], json!({ "scopes": ["api:entries:read"] })), NOW, Some(edge))
        .await;
    let raw = body_json(&mk)["token"].as_str().unwrap().to_string();
    let req = ApiRequest {
        method: Method::Post,
        path: "/api/v4/import/libreview".to_string(),
        query: vec![],
        headers: Headers::from_pairs(vec![("api-secret".to_string(), raw)]),
        body: b"Device,Device Timestamp,Record Type,Historic Glucose mg/dL\nx,11-14-2023 21:00,0,120".to_vec(),
    };
    let resp = svc.handle(req, NOW, None).await;
    assert_eq!(resp.status, 403, "read-only token cannot import");
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

/// SECURITY (IDOR): knowing another user's document identifier must grant no access to
/// it. Every store query is scoped to the caller's own `user_id` and the primary key
/// is `(user_id, identifier)`, so the same id in another user's namespace is a wholly
/// separate row. Bob can neither read, delete, nor overwrite Alice's document by its
/// exact id, and the same isolation holds for device tokens.
#[tokio::test]
async fn one_user_cannot_reach_anothers_data_by_id() {
    let svc = service().await;
    let alice = || Some(human("alice@cooney.be"));
    let bob = || Some(human("bob@cooney.be"));

    // Alice creates a document; we learn its exact identifier.
    let created = svc
        .handle(request("POST", "/api/v3/entries", &[], sgv(111)), NOW, alice())
        .await;
    assert_eq!(created.status, 201);
    let alice_id = body_json(&created)["result"]["identifier"].as_str().unwrap().to_string();
    let path = format!("/api/v3/entries/{alice_id}");

    // Bob cannot READ Alice's document by its id …
    let read = svc.handle(request("GET", &path, &[], Value::Null), NOW, bob()).await;
    assert_eq!(read.status, 404, "Bob must not read Alice's document by id");

    // … nor DELETE it …
    let del = svc.handle(request("DELETE", &path, &[], Value::Null), NOW, bob()).await;
    assert_eq!(del.status, 404, "Bob must not delete Alice's document by id");

    // … and a PUT to that id writes into BOB's own namespace, never Alice's row.
    let put = svc
        .handle(
            request("PUT", &path, &[], json!({ "type": "sgv", "date": NOW - 60_000, "sgv": 999, "device": "bob" })),
            NOW,
            bob(),
        )
        .await;
    assert_eq!(put.status, 200);

    // Alice's document is untouched (still 111); Bob sees only his own value (999).
    let alice_view = svc.handle(request("GET", &path, &[], Value::Null), NOW, alice()).await;
    assert_eq!(body_json(&alice_view)["result"]["sgv"], 111, "Alice's data is unchanged");
    let bob_view = svc.handle(request("GET", &path, &[], Value::Null), NOW, bob()).await;
    assert_eq!(body_json(&bob_view)["result"]["sgv"], 999, "Bob has his own separate row");

    // Device tokens are isolated the same way: Bob cannot revoke Alice's token.
    let mk = svc
        .handle(request("POST", "/api/v4/tokens", &[], json!({ "name": "alice-phone" })), NOW, alice())
        .await;
    let alice_token_id = body_json(&mk)["id"].as_str().unwrap().to_string();
    let bob_revoke = svc
        .handle(request("DELETE", &format!("/api/v4/tokens/{alice_token_id}"), &[], Value::Null), NOW, bob())
        .await;
    assert_eq!(bob_revoke.status, 404, "Bob must not revoke Alice's token");
    let alice_tokens = svc.handle(request("GET", "/api/v4/tokens", &[], Value::Null), NOW, alice()).await;
    assert_eq!(
        body_json(&alice_tokens)["tokens"].as_array().map(|a| a.len()),
        Some(1),
        "Alice's token survived Bob's revoke attempt"
    );
}

/// SECURITY (namespace confusion): a service token whose `common_name` equals a human's
/// email must NOT resolve to that human. Humans (`human:`) and machines (`service:`) are
/// namespaced apart, so the service principal lands in its own, separate account and
/// sees none of the human's data — even though service tokens bypass the group gate.
#[tokio::test]
async fn service_token_cannot_impersonate_human_by_subject() {
    let svc = service_requiring_group("night_knight_users").await;
    // A real human (in the required group) stores a reading.
    svc.handle(
        request("POST", "/api/v1/entries", &[], sgv(111)),
        NOW,
        Some(human_in("alice@cooney.be", &["night_knight_users"])),
    )
    .await;
    // Attacker presents a service token named exactly like Alice's email.
    let attacker = svc
        .handle(request("GET", "/api/v1/entries", &[], Value::Null), NOW, Some(service_token("alice@cooney.be")))
        .await;
    assert_eq!(attacker.status, 200, "service token authenticates into its OWN namespace");
    assert_eq!(
        body_json(&attacker).as_array().unwrap().len(),
        0,
        "but it sees none of Alice's data"
    );
}

/// A service token (machine principal) is least-privilege: it may read and upload CGM
/// data, but cannot administer device tokens / connectors or change account settings.
#[tokio::test]
async fn service_token_is_least_privilege() {
    let svc = service().await;
    let st = || Some(service_token("uploader.svc"));
    // Allowed: upload an entry (has `api:entries:create`).
    let up = svc.handle(request("POST", "/api/v1/entries", &[], sgv(120)), NOW, st()).await;
    assert_eq!(up.status, 200, "service token may upload CGM data");
    // Denied: minting device tokens (needs tokens:admin) and changing settings.
    let mint = svc.handle(request("POST", "/api/v4/tokens", &[], json!({ "name": "x" })), NOW, st()).await;
    assert_eq!(mint.status, 403, "service token cannot mint device tokens");
    let me = svc.handle(request("PUT", "/api/v4/me", &[], json!({ "preferredUnit": "mmol/l" })), NOW, st()).await;
    assert_eq!(me.status, 403, "service token cannot change settings");
}

/// MIGRATION: a pre-namespacing user (keyed by the bare email, with data) is adopted on
/// next login — the row is re-keyed IN PLACE to the namespaced subject, preserving its
/// `id`, so none of their existing readings are orphaned.
#[tokio::test]
async fn legacy_email_keyed_user_is_adopted_on_login() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    // Seed the PRE-migration state: a user keyed by the bare email, plus one reading.
    let legacy = User {
        id: "u-legacy".into(),
        subject: "alice@cooney.be".into(),
        display_name: None,
        is_admin: false,
        preferred_unit: "mg/dl".into(),
        created_at: NOW,
    };
    store.upsert_user(&legacy).await.unwrap();
    store
        .upsert_document(
            Collection::Entries,
            StoredDoc {
                identifier: "doc1".into(),
                user_id: "u-legacy".into(),
                mills: NOW - 60_000,
                doc_type: Some("sgv".into()),
                srv_created: NOW,
                srv_modified: NOW,
                is_valid: true,
                is_read_only: false,
                subject: Some("alice@cooney.be".into()),
                doc: sgv(111),
            },
        )
        .await
        .unwrap();

    let svc = ApiService::new(store).migrate_legacy_subjects(true);

    // Alice logs in. Her namespaced subject ("human:alice@cooney.be") has no row yet, so
    // the legacy email row is adopted — and she sees her existing reading.
    let r = svc.handle(request("GET", "/api/v1/entries", &[], Value::Null), NOW, Some(human("alice@cooney.be"))).await;
    assert_eq!(r.status, 200);
    assert_eq!(body_json(&r).as_array().unwrap().len(), 1, "existing reading is not orphaned");
    assert_eq!(body_json(&r)[0]["sgv"], 111);

    // The row was re-keyed in place: legacy key gone, same id under the namespaced key.
    assert!(
        svc.storage().get_user_by_subject("alice@cooney.be").await.unwrap().is_none(),
        "legacy bare-email key no longer resolves"
    );
    let migrated = svc.storage().get_user_by_subject("human:alice@cooney.be").await.unwrap().unwrap();
    assert_eq!(migrated.id, "u-legacy", "same user id preserved across the migration");

    // Migration is idempotent: a second login is a plain lookup, still the same user.
    let again = svc.handle(request("GET", "/api/v1/entries", &[], Value::Null), NOW, Some(human("alice@cooney.be"))).await;
    assert_eq!(body_json(&again).as_array().unwrap().len(), 1);
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

/// A mock Nightscout instance that answers the entries read with one canned reading.
struct MockNightscout {
    reading_ms: i64,
}

#[async_trait::async_trait]
impl nightknight_connectors::HttpClient for MockNightscout {
    async fn send(
        &self,
        req: nightknight_connectors::HttpReq,
    ) -> Result<nightknight_connectors::HttpResp, nightknight_connectors::ConnectorError> {
        let body: Vec<u8> = if req.url.contains("/api/v1/entries/sgv.json") {
            format!(
                r#"[{{"_id":"abc","type":"sgv","date":{},"sgv":132,"direction":"Flat","device":"ns-test"}}]"#,
                self.reading_ms
            )
            .into_bytes()
        } else {
            b"[]".to_vec()
        };
        Ok(nightknight_connectors::HttpResp { status: 200, body })
    }
}

/// SCENARIO: a user mirrors another Nightscout instance. The URL+secret are stored
/// encrypted (and the URL is normalised), an internal URL is refused (SSRF guard), the
/// scheduler pulls and ingests the readings, and a re-sync deduplicates.
#[tokio::test]
async fn nightscout_connector_imports_and_dedups() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store).with_connector_key(Some([42u8; 32]));
    let edge = Some(human("member@cooney.be"));

    // Add the Nightscout source via the UI endpoint (full endpoint URL is normalised).
    let put = svc
        .handle(
            request(
                "PUT",
                "/api/v4/connectors/nightscout",
                &[],
                json!({ "url": "https://ns.example.com/api/v1/entries/sgv?count=100", "secret": "s3cr3t" }),
            ),
            NOW,
            edge.clone(),
        )
        .await;
    assert_eq!(put.status, 200);
    // The secret is never echoed back.
    assert!(!String::from_utf8(put.body.clone()).unwrap().contains("s3cr3t"));

    // SSRF: an internal / non-https URL is refused before anything is stored.
    let bad = svc
        .handle(
            request("PUT", "/api/v4/connectors/nightscout", &[], json!({ "url": "http://127.0.0.1:1337", "secret": "x" })),
            NOW,
            edge.clone(),
        )
        .await;
    assert_eq!(bad.status, 400, "internal / non-https url refused");

    // The scheduler pulls from the mock and ingests the reading.
    let reading_ms = NOW - 120_000;
    let n = svc.sync_connectors(&MockNightscout { reading_ms }, 60, NOW).await.unwrap();
    assert_eq!(n, 1, "one reading ingested");

    // A second sync re-fetches the same reading; storage dedups, so there is still
    // exactly one point for the user.
    let _ = svc.sync_connectors(&MockNightscout { reading_ms }, 60, NOW).await.unwrap();
    let entries = svc.handle(request("GET", "/api/v1/entries", &[], Value::Null), NOW, edge).await;
    let arr = body_json(&entries);
    assert_eq!(arr.as_array().unwrap().len(), 1, "dedup: one point after re-sync");
    assert_eq!(arr[0]["sgv"], 132);
    assert_eq!(arr[0]["device"], "ns-test");
}

/// Serves `total` readings (1 min apart, newest at `base_ms`), honouring `count` and the
/// percent-encoded `find[date][$lt]` cursor — so the cursored backfill can be walked.
struct MockNsHistory {
    total: i64,
    base_ms: i64,
}
fn url_int(url: &str, key: &str) -> Option<i64> {
    url.split(&['?', '&'][..])
        .find_map(|kv| kv.strip_prefix(key))
        .and_then(|v| v.parse().ok())
}
#[async_trait::async_trait]
impl nightknight_connectors::HttpClient for MockNsHistory {
    async fn send(
        &self,
        req: nightknight_connectors::HttpReq,
    ) -> Result<nightknight_connectors::HttpResp, nightknight_connectors::ConnectorError> {
        let count = url_int(&req.url, "count=").unwrap_or(10);
        // The cursor arrives percent-encoded as `find%5Bdate%5D%5B%24lt%5D=<ms>`.
        let before = url_int(&req.url, "find%5Bdate%5D%5B%24lt%5D=").unwrap_or(i64::MAX);
        let mut dates = Vec::new();
        let mut i = 0i64;
        while i < self.total && (dates.len() as i64) < count {
            let d = self.base_ms - i * 60_000; // newest-first
            if d < before {
                dates.push(d);
            }
            i += 1;
        }
        let items: Vec<String> = dates
            .iter()
            .map(|d| format!(r#"{{"type":"sgv","date":{d},"sgv":120,"device":"ns"}}"#))
            .collect();
        let body = format!("[{}]", items.join(",")).into_bytes();
        Ok(nightknight_connectors::HttpResp { status: 200, body })
    }
}

/// The cursored backfill walks a source's WHOLE history backward across cron ticks: each
/// tick pulls one bounded page (older than the persisted cursor) until the source is
/// exhausted, importing everything without any single pull exceeding the page size.
#[tokio::test]
async fn nightscout_backfill_walks_full_history() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store).with_connector_key(Some([7u8; 32]));
    let edge = Some(human("histuser@cooney.be"));
    svc.handle(
        request("PUT", "/api/v4/connectors/nightscout", &[],
            json!({ "url": "https://ns.example.com", "secret": "s" })),
        NOW, edge.clone(),
    ).await;

    // 2500 readings — more than one 2000-page, so it takes two backfill ticks.
    let src = MockNsHistory { total: 2500, base_ms: NOW - 60_000 };

    // Tick 1: recent window + the newest 2000-reading page.
    let n1 = svc.sync_connectors(&src, 60, NOW).await.unwrap();
    assert_eq!(n1, 2000, "first tick imports the newest full page");
    // Tick 2: the remaining 500 older readings; the short page marks the backfill done.
    let n2 = svc.sync_connectors(&src, 60, NOW).await.unwrap();
    assert_eq!(n2, 500, "second tick imports the rest");
    // Tick 3: backfill is complete, so only the (already-present) recent window is pulled.
    let n3 = svc.sync_connectors(&src, 60, NOW).await.unwrap();
    assert_eq!(n3, 0, "nothing new once the whole history is in");

    // All 2500 distinct readings are stored.
    let entries = svc
        .handle(request("GET", "/api/v1/entries?count=5000", &[], Value::Null), NOW, edge)
        .await;
    assert_eq!(body_json(&entries).as_array().unwrap().len(), 2500, "whole history imported");
}

/// REGRESSION (issue #17): a v1 upload batch must be resilient — one invalid reading in
/// the middle must not abort the import and discard every good reading after it. A backfill
/// from xDrip+/Loop/Trio can carry the odd dirty row; fail-fast here silently truncated the
/// whole upload at the first bad record.
#[tokio::test]
async fn v1_post_batch_skips_bad_rows_keeps_the_rest() {
    let svc = service().await;
    let edge = Some(human("uploader@cooney.be"));
    // Good, BAD (implausible glucose), good — the bad one sits between two valid readings.
    let batch = json!([
        { "type": "sgv", "date": NOW - 60_000, "sgv": 120, "device": "x" },
        { "type": "sgv", "date": NOW - 120_000, "sgv": 9999, "device": "x" },
        { "type": "sgv", "date": NOW - 180_000, "sgv": 110, "device": "x" },
    ]);
    let resp = svc.handle(request("POST", "/api/v1/entries", &[], batch), NOW, edge.clone()).await;
    assert_eq!(resp.status, 200, "the batch is accepted even with one bad row");
    assert_eq!(
        body_json(&resp).as_array().unwrap().len(),
        2,
        "both good readings are stored and returned; only the bad one is dropped"
    );

    // Both good readings are queryable; the implausible one never landed.
    let got = svc
        .handle(request("GET", "/api/v1/entries?count=10", &[], Value::Null), NOW, edge)
        .await;
    let arr = body_json(&got);
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 2, "exactly the two valid readings persisted");
    assert!(arr.iter().all(|e| e["sgv"].as_i64() != Some(9999)), "the bad reading was skipped");
}

/// A single bad document (not a batch) still returns its error, so a client posting one
/// malformed reading gets honest feedback rather than a silent 200.
#[tokio::test]
async fn v1_post_single_bad_doc_still_errors() {
    let svc = service().await;
    let edge = Some(human("uploader2@cooney.be"));
    let resp = svc
        .handle(
            request("POST", "/api/v1/entries", &[],
                json!({ "type": "sgv", "date": NOW - 60_000, "sgv": 9999, "device": "x" })),
            NOW, edge,
        )
        .await;
    assert_eq!(resp.status, 400, "a lone invalid reading is rejected, not silently accepted");
}

/// Like [`MockNsHistory`] but salts every page with `sgv: 0` error-code rows (which the
/// parser drops). So a FULL raw page of `count` records yields fewer than `count` usable
/// samples — exactly the shape that used to make the backfill mistake a filtered page for
/// the end of history and abandon all older readings.
struct MockNsDirty {
    total: i64,
    base_ms: i64,
}
#[async_trait::async_trait]
impl nightknight_connectors::HttpClient for MockNsDirty {
    async fn send(
        &self,
        req: nightknight_connectors::HttpReq,
    ) -> Result<nightknight_connectors::HttpResp, nightknight_connectors::ConnectorError> {
        let count = url_int(&req.url, "count=").unwrap_or(10);
        let before = url_int(&req.url, "find%5Bdate%5D%5B%24lt%5D=").unwrap_or(i64::MAX);
        // Serve up to `count` RAW records older than the cursor, newest-first. Every 100th
        // record is an sgv=0 error code (kept in the raw array, dropped on parse) — so a
        // full page still carries `count` raw records but fewer usable samples.
        let mut items = Vec::new();
        let mut i = 0i64;
        while i < self.total && (items.len() as i64) < count {
            let d = self.base_ms - i * 60_000;
            if d < before {
                let sgv = if i % 100 == 0 { 0 } else { 120 };
                items.push(format!(r#"{{"type":"sgv","date":{d},"sgv":{sgv},"device":"ns"}}"#));
            }
            i += 1;
        }
        let body = format!("[{}]", items.join(",")).into_bytes();
        Ok(nightknight_connectors::HttpResp { status: 200, body })
    }
}

/// REGRESSION (issue #17): the backfill must terminate on the RAW page size, not the
/// post-filter sample count. A single dropped row (an `sgv ≤ 0` error code) inside an
/// otherwise-full page used to look like end-of-history — the walk set `bfDone` after the
/// first page and silently abandoned every older reading (the user's "I had more data in
/// March" report). With the fix, the walk continues until the server returns a short raw
/// page, so the whole valid history imports despite the dropped rows.
#[tokio::test]
async fn nightscout_backfill_survives_filtered_full_pages() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store).with_connector_key(Some([7u8; 32]));
    let edge = Some(human("dirtyuser@cooney.be"));
    svc.handle(
        request("PUT", "/api/v4/connectors/nightscout", &[],
            json!({ "url": "https://ns.example.com", "secret": "s" })),
        NOW, edge.clone(),
    ).await;

    // 2500 records spanning >1 page; every 100th is an error code → 25 dropped, 2475 valid.
    let src = MockNsDirty { total: 2500, base_ms: NOW - 60_000 };

    // Walk the backfill to completion. Each tick advances one page; the first page is a
    // FULL raw page that filtered down to fewer samples — the old code stopped right here.
    let mut total = 0usize;
    for _ in 0..5 {
        total += svc.sync_connectors(&src, 60, NOW).await.unwrap();
    }
    assert_eq!(total, 2475, "every valid reading imports despite the dropped error-code rows");

    // The oldest valid record (index 2499) is present — proof the walk reached the start
    // of history rather than stopping at the first filtered page.
    let oldest_ms = (NOW - 60_000) - 2499 * 60_000;
    let entries = svc
        .handle(request("GET", "/api/v1/entries?count=5000", &[], Value::Null), NOW, edge)
        .await;
    let arr = body_json(&entries);
    let arr = arr.as_array().unwrap();
    assert_eq!(arr.len(), 2475, "the full valid history is stored");
    assert!(
        arr.iter().any(|e| e["date"].as_i64() == Some(oldest_ms)),
        "the oldest reading made it in — the backfill did not stop at the first filtered page"
    );
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

/// A throwaway P-256 PKCS#8 key for exercising the APNs send path. It authenticates
/// nothing real (no Apple-registered Key/Team ID), but it lets `provider_token` produce a
/// genuine signed JWT so the silent-push request is built exactly as in production.
const TEST_APNS_P8: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg+UX1vSS9hu87cb+j\n\
8IYJh/1gPjxMFJ++fcBqWz3VPeyhRANCAAQmIzIwzHseC+ITSgkQp2hZohMI9Jr3\n\
nohMe+5Ung2D+0iRphHJkTEAN8j5Tr6H/MBVZRlUTEYkn+wYRxPPW3kR\n\
-----END PRIVATE KEY-----\n";

fn test_apns() -> nightknight_api::ApnsConfig {
    nightknight_api::ApnsConfig {
        key_p8: TEST_APNS_P8.to_string(),
        key_id: "ABC1234DEF".to_string(),
        team_id: "XYZ9876WUV".to_string(),
        bundle_id: "be.cooney.nightknight.NightKnight".to_string(),
        default_env: nightknight_api::ApnsEnv::Sandbox,
    }
}

/// 64-hex-char values shaped like APNs device tokens (distinct per device/user).
fn fake_apns_token() -> String {
    "ab".repeat(32)
}
fn apns_token_a() -> String {
    "a1".repeat(32)
}
fn apns_token_b() -> String {
    "b2".repeat(32)
}

/// SCENARIO: the iOS follower registers its APNs device token so the server can wake it.
/// The app authenticates with a **read-only** device token (it only follows), so the
/// registration endpoint must accept `entries:read` — and the write must be isolated to
/// the caller, idempotent across the app's relaunch re-POSTs, and reject junk tokens.
#[tokio::test]
async fn push_register_is_follower_scoped_idempotent_and_isolated() {
    let svc = service().await;

    // Alice mints a read-only follower token (the iOS app's credential) and uses it.
    let mk = svc
        .handle(
            request("POST", "/api/v4/tokens", &[], json!({ "scopes": ["api:entries:read"] })),
            NOW,
            Some(human("alice@cooney.be")),
        )
        .await;
    let raw = body_json(&mk)["token"].as_str().unwrap().to_string();
    let auth = format!("Bearer {raw}");
    let token = fake_apns_token();

    // Register via the read-only token (no edge identity) — entries:read is enough.
    let reg = svc
        .handle(
            request("POST", "/api/v4/push/register", &[("authorization", &auth)],
                json!({ "token": token, "environment": "sandbox" })),
            NOW,
            None,
        )
        .await;
    assert_eq!(reg.status, 200, "a read-only follower token may register for push");
    assert_eq!(body_json(&reg)["ok"], true);

    // Re-registering the same token (every app launch) stays idempotent — one row.
    let again = svc
        .handle(
            request("POST", "/api/v4/push/register", &[("authorization", &auth)],
                json!({ "token": token, "environment": "production" })),
            NOW,
            None,
        )
        .await;
    assert_eq!(again.status, 200);
    let stored = svc.storage().list_push_tokens(
        &svc.storage().get_user_by_subject("human:alice@cooney.be").await.unwrap().unwrap().id,
    ).await.unwrap();
    assert_eq!(stored.len(), 1, "re-register updates, never duplicates");
    assert_eq!(stored[0].environment, "production", "environment update applied");

    // A malformed (non-hex) token is rejected before it can reach APNs.
    let bad = svc
        .handle(
            request("POST", "/api/v4/push/register", &[("authorization", &auth)],
                json!({ "token": "not-a-hex-token!!", "environment": "sandbox" })),
            NOW,
            None,
        )
        .await;
    assert_eq!(bad.status, 400, "a non-hex token is refused");

    // Unregister removes it; a second unregister is a 404 (nothing to remove).
    let del = svc
        .handle(
            request("DELETE", "/api/v4/push/register", &[("authorization", &auth)],
                json!({ "token": token })),
            NOW,
            None,
        )
        .await;
    assert_eq!(del.status, 204);
    let del2 = svc
        .handle(
            request("DELETE", "/api/v4/push/register", &[("authorization", &auth)],
                json!({ "token": token })),
            NOW,
            None,
        )
        .await;
    assert_eq!(del2.status, 404, "unregistering an absent token is a no-op 404");
}

/// SECURITY: an unauthenticated request can neither register nor read push tokens.
#[tokio::test]
async fn push_register_requires_authentication() {
    let svc = service().await;
    let resp = svc
        .handle(
            request("POST", "/api/v4/push/register", &[],
                json!({ "token": fake_apns_token(), "environment": "sandbox" })),
            NOW,
            None,
        )
        .await;
    assert_eq!(resp.status, 401, "no credential, no registration");
}

/// A mock that plays a Dexcom cloud AND a Nightscout source (so a sync can ingest a
/// reading for either connector type) AND APNs (so we can observe the silent push). It
/// records every APNs request for assertions. `dexcom_reading_ms` / `nightscout_reading_ms`
/// let two different users (one per connector) be given independently fresh-or-stale data
/// in a single `sync_connectors` run.
struct MockCloudAndApns {
    dexcom_reading_ms: Option<i64>,
    nightscout_reading_ms: Option<i64>,
    apns_status: u16,
    apns_calls: std::sync::Mutex<Vec<MockApnsCall>>,
}

#[derive(Clone)]
struct MockApnsCall {
    url: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl MockCloudAndApns {
    /// A Dexcom-only mock (the common single-user case).
    fn new(reading_ms: i64, apns_status: u16) -> Self {
        MockCloudAndApns {
            dexcom_reading_ms: Some(reading_ms),
            nightscout_reading_ms: None,
            apns_status,
            apns_calls: std::sync::Mutex::new(Vec::new()),
        }
    }
    /// Also serve a Nightscout source a reading at `ms` (for a second, different user).
    fn with_nightscout(mut self, ms: i64) -> Self {
        self.nightscout_reading_ms = Some(ms);
        self
    }
    fn apns_calls(&self) -> Vec<MockApnsCall> {
        self.apns_calls.lock().unwrap().clone()
    }
    /// The device token each recorded push was addressed to (from the per-device URL).
    fn pushed_tokens(&self) -> Vec<String> {
        self.apns_calls()
            .iter()
            .map(|c| c.url.rsplit('/').next().unwrap_or_default().to_string())
            .collect()
    }
}

#[async_trait::async_trait]
impl nightknight_connectors::HttpClient for MockCloudAndApns {
    async fn send(
        &self,
        req: nightknight_connectors::HttpReq,
    ) -> Result<nightknight_connectors::HttpResp, nightknight_connectors::ConnectorError> {
        if req.url.contains("push.apple.com") {
            self.apns_calls.lock().unwrap().push(MockApnsCall {
                url: req.url.clone(),
                headers: req.headers.clone(),
                body: req.body.as_ref().map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default(),
            });
            return Ok(nightknight_connectors::HttpResp { status: self.apns_status, body: Vec::new() });
        }
        // Nightscout entries endpoint (recent pull + cursored backfill both hit it).
        if req.url.contains("/entries") {
            let body = match self.nightscout_reading_ms {
                Some(ms) => format!(
                    r#"[{{"type":"sgv","date":{ms},"sgv":120,"device":"ns"}}]"#
                ).into_bytes(),
                None => b"[]".to_vec(),
            };
            return Ok(nightknight_connectors::HttpResp { status: 200, body });
        }
        let body: Vec<u8> = if req.url.contains("AuthenticatePublisherAccount") {
            br#""account-id-123""#.to_vec()
        } else if req.url.contains("LoginPublisherAccountById") {
            br#""session-id-456""#.to_vec()
        } else if req.url.contains("ReadPublisherLatestGlucoseValues") {
            match self.dexcom_reading_ms {
                Some(ms) => format!(r#"[{{"WT":"Date({ms})","Value":132,"Trend":"Flat"}}]"#).into_bytes(),
                None => b"[]".to_vec(),
            }
        } else {
            b"[]".to_vec()
        };
        Ok(nightknight_connectors::HttpResp { status: 200, body })
    }
}

/// Register a specific APNs token for a user (the iOS app's POST).
async fn register_push_token(svc: &ApiService<SqlStore>, email: &str, token: &str) {
    register_push_token_env(svc, email, token, "sandbox").await;
}

async fn register_push_token_env(svc: &ApiService<SqlStore>, email: &str, token: &str, env: &str) {
    svc.handle(
        request("POST", "/api/v4/push/register", &[],
            json!({ "token": token, "environment": env })),
        NOW, Some(human(email)),
    ).await;
}

/// Give a user a Dexcom connector.
async fn add_dexcom(svc: &ApiService<SqlStore>, email: &str) {
    svc.handle(
        request("PUT", "/api/v4/connectors/dexcom", &[],
            json!({ "username": "u@x", "password": "s3cret", "region": "us" })),
        NOW, Some(human(email)),
    ).await;
}

/// Give a user a Nightscout source connector.
async fn add_nightscout(svc: &ApiService<SqlStore>, email: &str) {
    svc.handle(
        request("PUT", "/api/v4/connectors/nightscout", &[],
            json!({ "url": "https://ns.example.com", "secret": "s" })),
        NOW, Some(human(email)),
    ).await;
}

async fn user_id(svc: &ApiService<SqlStore>, email: &str) -> String {
    svc.storage().get_user_by_subject(&format!("human:{email}")).await.unwrap().unwrap().id
}

/// Add a Dexcom connector + register a push token for one user, returning that user's id.
async fn setup_user_with_connector_and_push(svc: &ApiService<SqlStore>, email: &str) -> String {
    add_dexcom(svc, email).await;
    register_push_token(svc, email, &fake_apns_token()).await;
    user_id(svc, email).await
}

/// SCENARIO (the heart of issue #13): a connector sync ingests a **fresh** reading, and
/// the server sends a correctly-formed silent push to the user's registered device. This
/// is what makes the phone wake and refresh without being foregrounded.
#[tokio::test]
async fn fresh_connector_reading_sends_a_well_formed_silent_push() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store)
        .with_connector_key(Some([42u8; 32]))
        .with_apns(Some(test_apns()));
    setup_user_with_connector_and_push(&svc, "member@cooney.be").await;

    // A reading from one minute ago → fresh → must push.
    let mock = MockCloudAndApns::new(NOW - 60_000, 200);
    let n = svc.sync_connectors(&mock, 60, NOW).await.unwrap();
    assert_eq!(n, 1, "one reading ingested");

    let calls = mock.apns_calls();
    assert_eq!(calls.len(), 1, "exactly one silent push for the one device");
    let call = &calls[0];
    assert!(call.url.starts_with("https://api.sandbox.push.apple.com/3/device/"),
            "sandbox token → sandbox host, per-device path: {}", call.url);
    assert!(call.url.ends_with(&fake_apns_token()));
    let header = |k: &str| call.headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.as_str());
    assert_eq!(header("apns-push-type"), Some("background"), "silent push type");
    assert_eq!(header("apns-priority"), Some("5"), "background priority");
    assert_eq!(header("apns-topic"), Some("be.cooney.nightknight.NightKnight"));
    assert!(header("authorization").unwrap().starts_with("bearer "), "bearer provider JWT");
    let body: Value = serde_json::from_str(&call.body).unwrap();
    assert_eq!(body["aps"]["content-available"], 1, "silent payload, no alert");
}

/// A one-time history backfill (or any back-dated import) must NOT wake the phone — a
/// silent push means "fresh data now", and a stale reading fails the freshness window.
#[tokio::test]
async fn stale_reading_does_not_push() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store)
        .with_connector_key(Some([42u8; 32]))
        .with_apns(Some(test_apns()));
    setup_user_with_connector_and_push(&svc, "member@cooney.be").await;

    // A reading from 30 minutes ago → outside the 15-minute freshness window → no push.
    let mock = MockCloudAndApns::new(NOW - 30 * 60_000, 200);
    let n = svc.sync_connectors(&mock, 60, NOW).await.unwrap();
    assert_eq!(n, 1, "the (old) reading is still ingested");
    assert!(mock.apns_calls().is_empty(), "a stale reading must not trigger a wake-up");
}

/// The freshness gate is the load-bearing backfill-safety line (`now_ms - t <= 15 min`).
/// This pins BOTH the 15-minute constant and the inclusive `<=` comparator at the exact
/// boundary — a unit slip (s vs ms) or a flipped operator would still pass the coarse
/// 1-min/30-min tests above, but not this one. (A fresh user is needed each time, so use a
/// separate in-memory store per probe.)
#[tokio::test]
async fn freshness_gate_pushes_at_the_window_edge_not_past_it() {
    async fn pushes_at(age_ms: i64) -> bool {
        let store = SqlStore::connect("sqlite::memory:").await.unwrap();
        store.migrate().await.unwrap();
        let svc = ApiService::new(store)
            .with_connector_key(Some([42u8; 32]))
            .with_apns(Some(test_apns()));
        setup_user_with_connector_and_push(&svc, "edge@cooney.be").await;
        let mock = MockCloudAndApns::new(NOW - age_ms, 200);
        svc.sync_connectors(&mock, 60, NOW).await.unwrap();
        !mock.apns_calls().is_empty()
    }
    assert!(pushes_at(15 * 60_000).await, "exactly at the 15-min edge → push (inclusive)");
    assert!(!pushes_at(15 * 60_000 + 1).await, "one ms past the edge → no push");
}

/// A `production`-environment token must be sent to the PRODUCTION APNs host, end-to-end
/// through the real send loop (not just `device_url` in isolation) — proving the loop reads
/// each token's own `environment`, so a TestFlight/App Store device isn't sent to sandbox
/// (which would be a hard `BadDeviceToken`).
#[tokio::test]
async fn production_token_is_sent_to_the_production_host() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store)
        .with_connector_key(Some([42u8; 32]))
        .with_apns(Some(test_apns()));
    add_dexcom(&svc, "member@cooney.be").await;
    register_push_token_env(&svc, "member@cooney.be", &fake_apns_token(), "production").await;

    let mock = MockCloudAndApns::new(NOW - 60_000, 200);
    svc.sync_connectors(&mock, 60, NOW).await.unwrap();
    let calls = mock.apns_calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].url.starts_with("https://api.push.apple.com/3/device/"),
            "production token → production host, not sandbox: {}", calls[0].url);
}

/// When APNs reports `410 Unregistered`, the dead token is pruned so we stop sending into
/// the void — APNs is the source of truth for token validity.
#[tokio::test]
async fn unregistered_device_token_is_pruned_on_410() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store)
        .with_connector_key(Some([42u8; 32]))
        .with_apns(Some(test_apns()));
    let user_id = setup_user_with_connector_and_push(&svc, "member@cooney.be").await;
    assert_eq!(svc.storage().list_push_tokens(&user_id).await.unwrap().len(), 1);

    // APNs says the device is gone → the push is attempted, then the token is removed.
    let mock = MockCloudAndApns::new(NOW - 60_000, 410);
    svc.sync_connectors(&mock, 60, NOW).await.unwrap();
    assert_eq!(mock.apns_calls().len(), 1, "the push was attempted");
    assert!(svc.storage().list_push_tokens(&user_id).await.unwrap().is_empty(),
            "a 410 Unregistered prunes the dead token");
}

/// SECURITY (the spec's central claim): in one sync run, a push goes ONLY to the device of
/// the user who got fresh data — never to another user's device. Here user A (Dexcom) gets
/// a fresh reading and user B (Nightscout) gets a stale one; exactly one push must fire, to
/// A's token, proving both the per-user freshness gate (`to_notify`) and that the send loop
/// reads each user's *own* tokens (`list_push_tokens(user_id)`), never broadcasting.
#[tokio::test]
async fn push_is_isolated_to_the_user_with_fresh_data() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store)
        .with_connector_key(Some([42u8; 32]))
        .with_apns(Some(test_apns()));

    // User A: Dexcom + token A. User B: Nightscout + token B (a different device).
    add_dexcom(&svc, "alice@cooney.be").await;
    register_push_token(&svc, "alice@cooney.be", &apns_token_a()).await;
    add_nightscout(&svc, "bob@cooney.be").await;
    register_push_token(&svc, "bob@cooney.be", &apns_token_b()).await;

    // A's reading is fresh (1 min); B's is stale (30 min) → only A should be woken.
    let mock = MockCloudAndApns::new(NOW - 60_000, 200).with_nightscout(NOW - 30 * 60_000);
    svc.sync_connectors(&mock, 60, NOW).await.unwrap();

    let pushed = mock.pushed_tokens();
    assert_eq!(pushed, vec![apns_token_a()],
               "exactly one push, to the fresh user's device — never the other user's token");
}

/// A user with several devices is woken on all of them: `push_new_readings` fans out to
/// every token `list_push_tokens` returns. Two registered devices → two pushes, one each.
#[tokio::test]
async fn push_fans_out_to_all_of_a_users_devices() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store)
        .with_connector_key(Some([42u8; 32]))
        .with_apns(Some(test_apns()));
    add_dexcom(&svc, "member@cooney.be").await;
    register_push_token(&svc, "member@cooney.be", &apns_token_a()).await;
    register_push_token(&svc, "member@cooney.be", &apns_token_b()).await;

    let mock = MockCloudAndApns::new(NOW - 60_000, 200);
    svc.sync_connectors(&mock, 60, NOW).await.unwrap();

    let mut pushed = mock.pushed_tokens();
    pushed.sort();
    assert_eq!(pushed, vec![apns_token_a(), apns_token_b()],
               "both of the user's devices are woken, exactly once each");
}

/// A user with TWO connectors that both bring in fresh data this tick is woken just ONCE,
/// not once per connector — `sync_connectors` coalesces per user via the `to_notify` set.
#[tokio::test]
async fn multiple_fresh_connectors_coalesce_to_one_push_per_user() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store)
        .with_connector_key(Some([42u8; 32]))
        .with_apns(Some(test_apns()));
    // One user, one device, but BOTH a Dexcom and a Nightscout connector, each fresh.
    add_dexcom(&svc, "member@cooney.be").await;
    add_nightscout(&svc, "member@cooney.be").await;
    register_push_token(&svc, "member@cooney.be", &fake_apns_token()).await;

    let mock = MockCloudAndApns::new(NOW - 60_000, 200).with_nightscout(NOW - 90_000);
    svc.sync_connectors(&mock, 60, NOW).await.unwrap();
    assert_eq!(mock.apns_calls().len(), 1,
               "two fresh connectors for one user coalesce into a single wake-up");
}

/// The per-minute cron re-fetches the same recent reading every tick. A reading that's
/// already stored creates nothing (dedup), so `newest_created_ms` is None and NO second
/// push fires — the phone is woken once per genuinely-new reading, not every minute.
#[tokio::test]
async fn resyncing_the_same_reading_does_not_push_again() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store)
        .with_connector_key(Some([42u8; 32]))
        .with_apns(Some(test_apns()));
    setup_user_with_connector_and_push(&svc, "member@cooney.be").await;

    // First tick: the reading is new → one push.
    let mock = MockCloudAndApns::new(NOW - 60_000, 200);
    svc.sync_connectors(&mock, 60, NOW).await.unwrap();
    assert_eq!(mock.apns_calls().len(), 1, "first sight of the reading wakes the phone");

    // Second tick: the same reading is re-fetched → deduped, nothing created → no push.
    svc.sync_connectors(&mock, 60, NOW).await.unwrap();
    assert_eq!(mock.apns_calls().len(), 1, "a re-seen reading must not wake the phone again");
}

/// With no APNs configured, registration still records tokens (so they're ready when a
/// key is added) but a sync sends nothing — push is simply disabled, not broken.
#[tokio::test]
async fn no_apns_config_means_no_push_but_registration_still_works() {
    let store = SqlStore::connect("sqlite::memory:").await.unwrap();
    store.migrate().await.unwrap();
    let svc = ApiService::new(store).with_connector_key(Some([42u8; 32])); // no .with_apns
    let user_id = setup_user_with_connector_and_push(&svc, "member@cooney.be").await;
    assert_eq!(svc.storage().list_push_tokens(&user_id).await.unwrap().len(), 1,
               "token is stored even before a key is configured");
    let mock = MockCloudAndApns::new(NOW - 60_000, 200);
    svc.sync_connectors(&mock, 60, NOW).await.unwrap();
    assert!(mock.apns_calls().is_empty(), "no APNs key → no push attempted");
}

