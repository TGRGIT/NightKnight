//! Storage contract tests — the executable specification of how *any* NightKnight
//! storage backend must behave. They run against an in-memory SQLite database (fast,
//! no external services). The Postgres path runs the identical SQL via the same
//! sqlx `Any` driver, and the Cloudflare D1 backend runs the same statement strings,
//! so satisfying this spec here is strong evidence all backends agree.
//!
//! Each test states, in plain language, the guarantee it protects and why that
//! guarantee matters when the data is someone's blood glucose.

use super::SqlStore;
use nightknight_storage::{
    Collection, ConnectorCredential, DeviceToken, DocQuery, PushToken, StoredDoc, Storage,
    WriteOutcome,
};
use serde_json::json;

/// A migrated, empty in-memory store — a clean slate per test.
async fn fresh_store() -> SqlStore {
    let store = SqlStore::connect("sqlite::memory:")
        .await
        .expect("connect to in-memory sqlite");
    store.migrate().await.expect("apply schema");
    store
}

/// Build an SGV document for `user` at `mills`, with the given server timestamp.
fn sgv_doc(user: &str, identifier: &str, mills: i64, sgv: i64, srv: i64) -> StoredDoc {
    StoredDoc {
        identifier: identifier.to_string(),
        user_id: user.to_string(),
        mills,
        doc_type: Some("sgv".to_string()),
        srv_created: srv,
        srv_modified: srv,
        is_valid: true,
        is_read_only: false,
        subject: Some(user.to_string()),
        doc: json!({ "type": "sgv", "date": mills, "sgv": sgv }),
    }
}

/// GUARANTEE: applying the schema twice is safe. The server runs migrations on every
/// boot, so a second run (after a restart) must not error or wipe data.
#[tokio::test]
async fn migrate_is_idempotent() {
    let store = fresh_store().await;
    store.migrate().await.expect("second migrate must succeed");
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "a", 1_000, 100, 10))
        .await
        .unwrap();
    // A third migrate after data exists must preserve it.
    store.migrate().await.unwrap();
    let got = store
        .get_document(Collection::Entries, "u1", "a")
        .await
        .unwrap();
    assert!(got.is_some(), "data survives re-migration");
}

/// GUARANTEE: a written document reads back exactly, including its JSON body and
/// metadata. A CGM reading must never be silently altered in storage.
#[tokio::test]
async fn create_then_read_round_trips() {
    let store = fresh_store().await;
    let doc = sgv_doc("u1", "entry-1", 1_700_000_000_000, 112, 5);
    let outcome = store
        .upsert_document(Collection::Entries, doc.clone())
        .await
        .unwrap();
    assert!(matches!(outcome, WriteOutcome::Created(_)), "first write is a create");

    let got = store
        .get_document(Collection::Entries, "u1", "entry-1")
        .await
        .unwrap()
        .expect("document exists");
    assert_eq!(got, doc);
    assert_eq!(got.doc["sgv"], 112);
}

/// GUARANTEE: re-sending a document with the same identifier UPDATES it rather than
/// creating a duplicate (Nightscout v3 dedup). Uploaders retry aggressively; without
/// this, a flaky connection would litter the chart with duplicate points.
#[tokio::test]
async fn duplicate_identifier_updates_not_duplicates() {
    let store = fresh_store().await;
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "dup", 1_000, 100, 10))
        .await
        .unwrap();

    let mut updated = sgv_doc("u1", "dup", 1_000, 105, 20);
    updated.doc = json!({ "type": "sgv", "date": 1_000, "sgv": 105, "corrected": true });
    let outcome = store
        .upsert_document(Collection::Entries, updated)
        .await
        .unwrap();
    assert!(matches!(outcome, WriteOutcome::Updated(_)), "second write is an update");

    // Exactly one row, carrying the updated body.
    let all = store
        .search_documents(Collection::Entries, "u1", &DocQuery::new())
        .await
        .unwrap();
    assert_eq!(all.len(), 1, "no duplicate row");
    assert_eq!(all[0].doc["sgv"], 105);
    assert_eq!(all[0].doc["corrected"], true);
    // srv_created is preserved from the original insert; srv_modified moves forward.
    assert_eq!(all[0].srv_created, 10);
    assert_eq!(all[0].srv_modified, 20);
}

/// GUARANTEE: search honours time-window, type, ordering and limit filters. The
/// dashboard asks for "the last N readings in this window" — these filters are how
/// it draws the right slice of the day.
#[tokio::test]
async fn search_filters_and_orders() {
    let store = fresh_store().await;
    for (i, mills) in [1_000i64, 2_000, 3_000, 4_000].iter().enumerate() {
        store
            .upsert_document(
                Collection::Entries,
                sgv_doc("u1", &format!("e{i}"), *mills, 100 + i as i64, 10 + i as i64),
            )
            .await
            .unwrap();
    }
    // A treatment in the same user's space must not appear in an entries search.
    let mut treat = sgv_doc("u1", "t1", 2_500, 0, 50);
    treat.doc_type = None;
    treat.doc = json!({ "eventType": "BG Check" });
    store
        .upsert_document(Collection::Treatments, treat)
        .await
        .unwrap();

    // Window [2000, 3000], newest first.
    let res = store
        .search_documents(
            Collection::Entries,
            "u1",
            &DocQuery::new().date_gte(2_000).date_lte(3_000),
        )
        .await
        .unwrap();
    let mills: Vec<i64> = res.iter().map(|d| d.mills).collect();
    assert_eq!(mills, vec![3_000, 2_000], "descending within the window");

    // Limit returns only the newest.
    let newest = store
        .search_documents(Collection::Entries, "u1", &DocQuery::new().limit(1))
        .await
        .unwrap();
    assert_eq!(newest.len(), 1);
    assert_eq!(newest[0].mills, 4_000);

    // Type filter (only sgv entries).
    let sgvs = store
        .search_documents(Collection::Entries, "u1", &DocQuery::new().doc_type("sgv"))
        .await
        .unwrap();
    assert_eq!(sgvs.len(), 4);
}

/// GUARANTEE: `daily_counts` buckets readings into local calendar days and returns one
/// row per day (newest first) with the correct count and first/last reading time, while
/// excluding other users, non-sgv types and soft-deleted rows. This cheap aggregation is
/// what the Data view trusts to prove "these days actually have data" across a whole
/// history without loading every reading.
#[tokio::test]
async fn daily_counts_buckets_by_local_day() {
    const DAY_MS: i64 = 86_400_000;
    let store = fresh_store().await;
    // Day 0: three readings.
    for (i, t) in [1_000i64, 2_000, 3_000].iter().enumerate() {
        store
            .upsert_document(Collection::Entries, sgv_doc("u1", &format!("a{i}"), *t, 100, 1))
            .await
            .unwrap();
    }
    // Day 5: two readings.
    let d5 = 5 * DAY_MS;
    for (i, off) in [600_000i64, 700_000].iter().enumerate() {
        store
            .upsert_document(Collection::Entries, sgv_doc("u1", &format!("b{i}"), d5 + off, 120, 1))
            .await
            .unwrap();
    }
    // Excluded: a non-sgv entry, another user's reading, and a soft-deleted reading.
    let mut mbg = sgv_doc("u1", "mbg0", d5 + 800_000, 99, 1);
    mbg.doc_type = Some("mbg".into());
    store.upsert_document(Collection::Entries, mbg).await.unwrap();
    store
        .upsert_document(Collection::Entries, sgv_doc("u2", "z0", 1_500, 100, 1))
        .await
        .unwrap();
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "del0", d5 + 900_000, 88, 1))
        .await
        .unwrap();
    store
        .soft_delete_document(Collection::Entries, "u1", "del0", 2)
        .await
        .unwrap();

    let days = store
        .daily_counts(Collection::Entries, "u1", "sgv", 0)
        .await
        .unwrap();
    assert_eq!(days.len(), 2, "two distinct days with valid sgv data");
    // Newest day first.
    assert_eq!(days[0].day_index, 5);
    assert_eq!(days[0].n, 2);
    assert_eq!(days[0].first_ms, d5 + 600_000);
    assert_eq!(days[0].last_ms, d5 + 700_000);
    assert_eq!(days[1].day_index, 0);
    assert_eq!(days[1].n, 3);
    assert_eq!(days[1].first_ms, 1_000);
    assert_eq!(days[1].last_ms, 3_000);
}

/// GUARANTEE: the UTC offset moves the local-day boundary, so a late-evening reading can
/// land on the next local day. A user east of UTC must see their own midnight, not UTC's
/// — otherwise the Data calendar would split or misdate their nights.
#[tokio::test]
async fn daily_counts_respects_utc_offset() {
    const DAY_MS: i64 = 86_400_000;
    let store = fresh_store().await;
    // 23:30 UTC on day 1.
    let t = 2 * DAY_MS - 30 * 60_000;
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "e", t, 100, 1))
        .await
        .unwrap();
    let utc = store
        .daily_counts(Collection::Entries, "u1", "sgv", 0)
        .await
        .unwrap();
    assert_eq!(utc[0].day_index, 1, "UTC bucket is day 1");
    // At +60 minutes it rolls into day 2 (00:30 local).
    let plus = store
        .daily_counts(Collection::Entries, "u1", "sgv", 60 * 60_000)
        .await
        .unwrap();
    assert_eq!(plus[0].day_index, 2, "+1h offset rolls the reading into day 2");
}

/// REGRESSION (D1 float-binding, found via a production HAR where `/api/v4/days` threw a
/// Worker exception): the day-bucket arithmetic must use an INTEGER offset, not a bound
/// param. D1 binds integer params as JS floats, which would make SQLite do REAL division
/// and explode `GROUP BY` to one group per reading (hundreds of thousands of rows → the
/// Worker OOMs). The shipped `daily_counts` inlines the offset as an integer literal; this
/// reproduces the float regime against SQLite to prove the difference and guard it.
#[tokio::test]
async fn daily_counts_resists_float_offset_binding_explosion() {
    const DAY_MS: i64 = 86_400_000;
    let store = fresh_store().await;
    // 3 readings on day 0, 2 on day 5 — distinct times within each day.
    for (i, t) in [1_000i64, 2_000, 3_000, 5 * DAY_MS + 10, 5 * DAY_MS + 20].iter().enumerate() {
        store
            .upsert_document(Collection::Entries, sgv_doc("u1", &format!("e{i}"), *t, 100, 1))
            .await
            .unwrap();
    }
    // The shipped query (offset inlined as an integer) buckets per day → 2 rows.
    let days = store.daily_counts(Collection::Entries, "u1", "sgv", 0).await.unwrap();
    assert_eq!(days.len(), 2, "inlined-offset day bucketing gives one row per day");
    // Simulate what D1 does: bind the offset as a float into the same expression. SQLite
    // then does REAL division and groups by fractional day → one group per reading.
    let exploded = sqlx::query(
        "SELECT (mills + ?) / 86400000 AS day FROM entries \
         WHERE user_id=? AND is_valid=1 AND doc_type='sgv' GROUP BY 1",
    )
    .bind(0.0_f64)
    .bind("u1")
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(exploded.len(), 5, "a float-bound offset explodes to one group per reading");
}

/// GUARANTEE: `downsampled_documents` returns exactly one representative reading per
/// time-bucket — the earliest in each bucket — so dense data collapses to a bounded
/// series while sparse data passes through untouched. This is what lets a long-window
/// aggregate report cover the whole range without loading every reading. Scoping (user,
/// type, validity, window) matches `search_documents`.
#[tokio::test]
async fn downsampled_documents_keeps_one_reading_per_bucket() {
    let store = fresh_store().await;
    // Bucket width 1000 ms. Bucket 0 [0,1000): three dense readings at 100/300/900 →
    // only the earliest (100) survives. Bucket 2 [2000,3000): one reading at 2500.
    // Bucket 5 [5000,6000): one reading at 5500. A 4th bucket reading is out of window.
    for (i, (t, sgv)) in [(100i64, 90), (300, 95), (900, 99), (2_500, 120), (5_500, 150), (9_500, 200)]
        .iter()
        .enumerate()
    {
        store
            .upsert_document(Collection::Entries, sgv_doc("u1", &format!("a{i}"), *t, *sgv, 1))
            .await
            .unwrap();
    }
    // Excluded: another user, a non-sgv type, and a soft-deleted reading in an occupied bucket.
    store
        .upsert_document(Collection::Entries, sgv_doc("u2", "z", 200, 100, 1))
        .await
        .unwrap();
    let mut mbg = sgv_doc("u1", "m", 400, 88, 1);
    mbg.doc_type = Some("mbg".into());
    store.upsert_document(Collection::Entries, mbg).await.unwrap();
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "del", 50, 77, 1))
        .await
        .unwrap();
    store.soft_delete_document(Collection::Entries, "u1", "del", 2).await.unwrap();

    // Window [0, 6000] with 1000 ms buckets → buckets 0, 2, 5 each contribute one row.
    let rows = store
        .downsampled_documents(Collection::Entries, "u1", "sgv", 0, 6_000, 1_000, None)
        .await
        .unwrap();
    let mills: Vec<i64> = rows.iter().map(|r| r.mills).collect();
    // Newest first, one representative (earliest) per occupied bucket; 9_500 is out of window.
    assert_eq!(mills, vec![5_500, 2_500, 100], "one earliest-per-bucket row, newest first, in window");
    // The 50 ms soft-deleted reading did NOT become bucket 0's representative.
    assert!(rows.iter().all(|r| r.mills != 50), "soft-deleted rows are excluded from bucketing");

    // `limit` caps the outer fetch (newest first) so a caller can paginate a long window
    // without materialising every bucket in one query.
    let capped = store
        .downsampled_documents(Collection::Entries, "u1", "sgv", 0, 6_000, 1_000, Some(2))
        .await
        .unwrap();
    assert_eq!(
        capped.iter().map(|r| r.mills).collect::<Vec<_>>(),
        vec![5_500, 2_500],
        "limit keeps the newest N bucket representatives"
    );
}

/// REGRESSION (mirrors `daily_counts_resists_float_offset_binding_explosion`): the
/// bucket divisor must be an INTEGER literal, not a bound param. If D1 bound `bucket_ms`
/// as a JS float, `mills / bucket_ms` would be REAL division and every reading would land
/// in its own fractional bucket — defeating the downsample and re-exposing the unbounded
/// row count that 503s the Worker. The shipped query inlines the divisor; this proves the
/// float regime would explode and that the shipped path does not.
#[tokio::test]
async fn downsampled_documents_resists_float_bucket_binding_explosion() {
    let store = fresh_store().await;
    // Five dense readings inside a single 1000 ms bucket.
    for (i, t) in [10i64, 100, 300, 600, 900].iter().enumerate() {
        store
            .upsert_document(Collection::Entries, sgv_doc("u1", &format!("e{i}"), *t, 100, 1))
            .await
            .unwrap();
    }
    // Shipped query (divisor inlined as integer): the whole bucket collapses to one row.
    let rows = store
        .downsampled_documents(Collection::Entries, "u1", "sgv", 0, 1_000, 1_000, None)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "inlined-integer bucketing keeps one row for the dense bucket");
    assert_eq!(rows[0].mills, 10, "and it is the earliest reading in the bucket");
    // Simulate D1's float bind into the same expression: REAL division → one group per reading.
    let exploded = sqlx::query(
        "SELECT MIN(mills) AS m FROM entries \
         WHERE user_id=? AND is_valid=1 AND doc_type='sgv' GROUP BY mills / ?",
    )
    .bind("u1")
    .bind(1000.0_f64)
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(exploded.len(), 5, "a float-bound divisor explodes to one group per reading");
}

/// DOCUMENTS a known edge of the `mills IN (SELECT MIN(mills) ...)` groupwise-min: when two
/// distinct documents share the SAME `mills` (allowed — the PK is `(user_id, identifier)`
/// and the `mills` index is non-unique, e.g. two devices sampling the same instant), and
/// that shared timestamp is a bucket minimum, the `IN` matches BOTH rows, so the bucket
/// emits two rows. The API layer that consumes this (`v4_export`) de-dupes to one reading
/// per bucket in Rust; this test pins the storage-level behaviour so that contract is explicit.
#[tokio::test]
async fn downsampled_documents_may_emit_duplicate_mills_within_a_bucket() {
    let store = fresh_store().await;
    // Two distinct docs at the same mills (100) in bucket 0, plus one later reading.
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "dexcom-100", 100, 90, 1))
        .await
        .unwrap();
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "libre-100", 100, 95, 1))
        .await
        .unwrap();
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "b", 2_500, 120, 1))
        .await
        .unwrap();
    let rows = store
        .downsampled_documents(Collection::Entries, "u1", "sgv", 0, 3_000, 1_000, None)
        .await
        .unwrap();
    let bucket0: Vec<i64> = rows.iter().filter(|r| r.mills == 100).map(|r| r.mills).collect();
    assert_eq!(bucket0.len(), 2, "tied-mills docs both match the bucket-min IN-set (deduped upstream)");
    // Deduping by bucket index (mills / 1000) collapses them to the intended one-per-bucket.
    let mut buckets: Vec<i64> = rows.iter().map(|r| r.mills / 1_000).collect();
    buckets.sort_unstable();
    buckets.dedup();
    assert_eq!(buckets, vec![0, 2], "two occupied buckets once de-duped");
}

/// GUARANTEE: a soft-deleted document disappears from normal search but remains
/// available through history (so a synced device learns it was removed), and the
/// collection's `last_modified` advances.
#[tokio::test]
async fn soft_delete_hides_but_keeps_history() {
    let store = fresh_store().await;
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "x", 1_000, 100, 10))
        .await
        .unwrap();

    let deleted = store
        .soft_delete_document(Collection::Entries, "u1", "x", 99)
        .await
        .unwrap();
    assert!(deleted, "an existing valid doc is flagged");

    // Gone from normal search …
    let visible = store
        .search_documents(Collection::Entries, "u1", &DocQuery::new())
        .await
        .unwrap();
    assert!(visible.is_empty(), "soft-deleted doc hidden from search");

    // … but present (and marked invalid) in history.
    let history = store
        .history_since(Collection::Entries, "u1", 0, 100)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert!(!history[0].is_valid, "history shows the deletion");

    assert_eq!(
        store.last_modified(Collection::Entries, "u1").await.unwrap(),
        Some(99),
        "deletion advanced last_modified"
    );
}

/// GUARANTEE: users are isolated. One person must NEVER see another person's glucose
/// data — the core privacy and safety property of a multi-user health service.
#[tokio::test]
async fn users_are_isolated() {
    let store = fresh_store().await;
    store
        .upsert_document(Collection::Entries, sgv_doc("alice", "a1", 1_000, 100, 10))
        .await
        .unwrap();
    store
        .upsert_document(Collection::Entries, sgv_doc("bob", "b1", 1_000, 200, 10))
        .await
        .unwrap();

    let alice = store
        .search_documents(Collection::Entries, "alice", &DocQuery::new())
        .await
        .unwrap();
    assert_eq!(alice.len(), 1);
    assert_eq!(alice[0].doc["sgv"], 100);

    // Alice cannot read Bob's document even by its identifier.
    assert!(
        store
            .get_document(Collection::Entries, "alice", "b1")
            .await
            .unwrap()
            .is_none(),
        "cross-user read returns nothing"
    );
}

/// GUARANTEE: `history_since` returns everything changed strictly after a watermark,
/// oldest-first — the basis of incremental sync for the iOS app and other clients.
#[tokio::test]
async fn history_since_is_incremental_and_ordered() {
    let store = fresh_store().await;
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "a", 1_000, 100, 10))
        .await
        .unwrap();
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "b", 2_000, 110, 20))
        .await
        .unwrap();
    store
        .upsert_document(Collection::Entries, sgv_doc("u1", "c", 3_000, 120, 30))
        .await
        .unwrap();

    // Everything changed after srv_modified=10 → b then c.
    let since = store
        .history_since(Collection::Entries, "u1", 10, 100)
        .await
        .unwrap();
    let idents: Vec<&str> = since.iter().map(|d| d.identifier.as_str()).collect();
    assert_eq!(idents, vec!["b", "c"], "strictly-after, oldest-first");
}

/// GUARANTEE: users upsert idempotently on `subject` and are retrievable by both
/// subject and id. The auth layer looks a user up by their verified subject on every
/// request; repeated logins must not create duplicate accounts.
#[tokio::test]
async fn user_upsert_and_lookup() {
    let store = fresh_store().await;
    let user = nightknight_storage::User {
        id: "user-uuid-1".into(),
        subject: "alice@example.com".into(),
        display_name: Some("Alice".into()),
        is_admin: false,
        preferred_unit: "mmol/l".into(),
        created_at: 1_700_000_000_000,
    };
    store.upsert_user(&user).await.unwrap();
    // Re-upsert (a second login) updates profile fields but keeps one row/id.
    let mut again = user.clone();
    again.id = "should-be-ignored".into();
    again.display_name = Some("Alice B.".into());
    store.upsert_user(&again).await.unwrap();

    let by_subject = store
        .get_user_by_subject("alice@example.com")
        .await
        .unwrap()
        .expect("found by subject");
    assert_eq!(by_subject.id, "user-uuid-1", "original id preserved");
    assert_eq!(by_subject.display_name.as_deref(), Some("Alice B."));
    assert_eq!(by_subject.preferred_unit, "mmol/l");

    let by_id = store.get_user_by_id("user-uuid-1").await.unwrap();
    assert_eq!(by_id, Some(by_subject));
}

/// GUARANTEE: device-token lifecycle works and only the hash is stored. Tokens
/// authenticate uploaders/clients; a stolen database must not yield usable tokens,
/// and a revoked token must stop working.
#[tokio::test]
async fn device_token_lifecycle() {
    let store = fresh_store().await;
    let token = DeviceToken {
        id: "tok-1".into(),
        user_id: "u1".into(),
        name: "Phone (xDrip+)".into(),
        token_hash: "sha256-of-raw".into(),
        scopes: vec!["api:entries:read".into(), "api:entries:create".into()],
        created_at: 1_000,
        last_used_at: None,
        revoked: false,
        legacy_hash: Some("sha256-of-sha1hex".into()),
    };
    store.insert_device_token(&token).await.unwrap();

    let fetched = store
        .get_device_token_by_hash("sha256-of-raw")
        .await
        .unwrap()
        .expect("token found by modern hash");
    assert_eq!(fetched.scopes.len(), 2);
    assert!(!fetched.revoked);

    // The legacy SHA-1 path resolves to the same token — this is what lets an
    // existing Nightscout uploader (xDrip+) authenticate unchanged.
    let by_legacy = store
        .get_device_token_by_hash("sha256-of-sha1hex")
        .await
        .unwrap()
        .expect("token found by legacy hash");
    assert_eq!(by_legacy.id, "tok-1");

    store.touch_device_token("sha256-of-raw", 2_000).await.unwrap();
    let touched = store
        .get_device_token_by_hash("sha256-of-raw")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(touched.last_used_at, Some(2_000));

    let list = store.list_device_tokens("u1").await.unwrap();
    assert_eq!(list.len(), 1);

    let revoked = store.revoke_device_token("u1", "tok-1").await.unwrap();
    assert!(revoked);
    let after = store
        .get_device_token_by_hash("sha256-of-raw")
        .await
        .unwrap()
        .unwrap();
    assert!(after.revoked, "token is now revoked");
}

/// GUARANTEE: connector credentials store/list/status/delete correctly, the
/// enabled-list (the scheduler's work list) reflects the flag, and `created_at`
/// survives updates. The sealed secret is stored verbatim (encryption happens above
/// this layer); storage just must not lose or corrupt it.
#[tokio::test]
async fn connector_credentials_crud() {
    let store = fresh_store().await;
    let cred = ConnectorCredential {
        user_id: "u1".into(),
        provider: "dexcom".into(),
        enabled: true,
        secret_enc: "sealed-blob".into(),
        region: Some("us".into()),
        created_at: 1_000,
        updated_at: 1_000,
        last_sync_at: None,
        last_status: None,
    };
    store.upsert_connector_credential(&cred).await.unwrap();

    let got = store.get_connector_credential("u1", "dexcom").await.unwrap().unwrap();
    assert_eq!(got.secret_enc, "sealed-blob");
    assert!(got.enabled);
    assert_eq!(store.list_connector_credentials("u1").await.unwrap().len(), 1);
    assert_eq!(store.list_enabled_connector_credentials().await.unwrap().len(), 1);

    store.update_connector_sync("u1", "dexcom", 2_000, "ok: 3 readings").await.unwrap();
    let synced = store.get_connector_credential("u1", "dexcom").await.unwrap().unwrap();
    assert_eq!(synced.last_sync_at, Some(2_000));
    assert_eq!(synced.last_status.as_deref(), Some("ok: 3 readings"));

    // Disabling drops it from the scheduler's work list, but keeps created_at.
    let mut disabled = cred.clone();
    disabled.enabled = false;
    disabled.updated_at = 3_000;
    store.upsert_connector_credential(&disabled).await.unwrap();
    assert!(store.list_enabled_connector_credentials().await.unwrap().is_empty());
    let after = store.get_connector_credential("u1", "dexcom").await.unwrap().unwrap();
    assert_eq!(after.created_at, 1_000, "created_at preserved across updates");

    assert!(store.delete_connector_credential("u1", "dexcom").await.unwrap());
    assert!(store.get_connector_credential("u1", "dexcom").await.unwrap().is_none());
}

/// GUARANTEE: APNs push tokens register idempotently, list per-user, and delete. The iOS
/// app re-POSTs its token on every launch (and when iOS rotates it), so a repeat
/// registration must UPDATE the one row — not pile up duplicates that each get a wasted
/// silent push — and changing environment (sandbox→production across a TestFlight build)
/// must take effect. Pruning is how a `410 Unregistered` from APNs is honoured.
#[tokio::test]
async fn push_tokens_register_idempotently_and_isolate_users() {
    let store = fresh_store().await;
    let tok = |user: &str, token: &str, env: &str, at: i64| PushToken {
        user_id: user.into(),
        token: token.into(),
        environment: env.into(),
        bundle_id: "be.cooney.nightknight.NightKnight".into(),
        updated_at: at,
    };

    // First registration, then a re-registration of the SAME token that flips the
    // environment and bumps updated_at — must collapse to one row carrying the new values.
    store.upsert_push_token(&tok("u1", "aaaa", "sandbox", 1_000)).await.unwrap();
    store.upsert_push_token(&tok("u1", "aaaa", "production", 2_000)).await.unwrap();
    let u1 = store.list_push_tokens("u1").await.unwrap();
    assert_eq!(u1.len(), 1, "re-registering a token updates, never duplicates");
    assert_eq!(u1[0].environment, "production", "environment update took effect");
    assert_eq!(u1[0].updated_at, 2_000);
    assert_eq!(u1[0].bundle_id, "be.cooney.nightknight.NightKnight",
               "bundle_id round-trips through storage (it becomes the APNs topic)");

    // A second distinct device for u1, and a token for a different user.
    store.upsert_push_token(&tok("u1", "bbbb", "sandbox", 3_000)).await.unwrap();
    store.upsert_push_token(&tok("u2", "cccc", "sandbox", 4_000)).await.unwrap();
    assert_eq!(store.list_push_tokens("u1").await.unwrap().len(), 2, "u1 has two devices");
    let u2 = store.list_push_tokens("u2").await.unwrap();
    assert_eq!(u2.len(), 1, "u2 sees only its own token — never u1's");
    assert_eq!(u2[0].token, "cccc");

    // Pruning (e.g. on 410 Unregistered) removes exactly that token, scoped to its owner.
    assert!(store.delete_push_token("u1", "aaaa").await.unwrap());
    assert!(!store.delete_push_token("u1", "aaaa").await.unwrap(), "second delete is a no-op");
    // u2 cannot delete u1's remaining token (cross-user delete is a no-op).
    assert!(!store.delete_push_token("u2", "bbbb").await.unwrap());
    assert_eq!(store.list_push_tokens("u1").await.unwrap().len(), 1, "only the pruned token went");
}
