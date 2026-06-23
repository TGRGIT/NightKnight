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
    Collection, ConnectorCredential, DeviceToken, DocQuery, StoredDoc, Storage, WriteOutcome,
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
