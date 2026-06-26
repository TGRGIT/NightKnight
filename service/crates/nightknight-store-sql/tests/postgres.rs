//! Postgres parity test.
//!
//! Proves the sqlx backend behaves identically on **Postgres** (the container
//! deployment target), not just SQLite. It runs the same key guarantees from the
//! contract suite against a real Postgres, scoped to a unique user id so the test is
//! isolated and idempotent on a shared database.
//!
//! It is skipped unless `NK_TEST_PG_URL` is set, e.g.:
//!
//! ```bash
//! docker run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=pw --name nk-pg postgres:16
//! NK_TEST_PG_URL='postgres://postgres:pw@localhost:5433/postgres' \
//!   cargo test -p nightknight-store-sql --test postgres -- --nocapture
//! ```

use nightknight_storage::{Collection, DeviceToken, DocQuery, StoredDoc, Storage, WriteOutcome};
use nightknight_store_sql::SqlStore;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

fn sgv_doc(user: &str, identifier: &str, mills: i64, sgv: i64, srv: i64) -> StoredDoc {
    StoredDoc {
        identifier: identifier.into(),
        user_id: user.into(),
        mills,
        doc_type: Some("sgv".into()),
        srv_created: srv,
        srv_modified: srv,
        is_valid: true,
        is_read_only: false,
        subject: Some(user.into()),
        doc: json!({ "type": "sgv", "date": mills, "sgv": sgv }),
    }
}

#[tokio::test]
async fn postgres_parity() {
    let Ok(url) = std::env::var("NK_TEST_PG_URL") else {
        eprintln!("SKIP postgres_parity: set NK_TEST_PG_URL to run against Postgres");
        return;
    };

    let store = SqlStore::connect(&url).await.expect("connect to Postgres");
    store.migrate().await.expect("migrate Postgres");

    // Unique owner per run so the test is isolated on a shared database.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
    let user = format!("pg-test-{now}");

    // Create + read.
    let outcome = store
        .upsert_document(Collection::Entries, sgv_doc(&user, "e1", now, 120, now))
        .await
        .unwrap();
    assert!(matches!(outcome, WriteOutcome::Created(_)), "first write creates");
    let got = store
        .get_document(Collection::Entries, &user, "e1")
        .await
        .unwrap()
        .expect("exists");
    assert_eq!(got.doc["sgv"], 120);

    // Dedup: same identifier updates, not duplicates.
    let outcome2 = store
        .upsert_document(Collection::Entries, sgv_doc(&user, "e1", now, 125, now + 1))
        .await
        .unwrap();
    assert!(matches!(outcome2, WriteOutcome::Updated(_)), "re-post updates");
    let all = store
        .search_documents(Collection::Entries, &user, &DocQuery::new())
        .await
        .unwrap();
    assert_eq!(all.len(), 1, "no duplicate on Postgres");
    assert_eq!(all[0].doc["sgv"], 125);
    assert_eq!(all[0].srv_created, now, "srv_created preserved on update");

    // Soft-delete + history + last_modified.
    assert!(store
        .soft_delete_document(Collection::Entries, &user, "e1", now + 2)
        .await
        .unwrap());
    assert!(store
        .search_documents(Collection::Entries, &user, &DocQuery::new())
        .await
        .unwrap()
        .is_empty(), "soft-deleted hidden");
    let history = store.history_since(Collection::Entries, &user, 0, 100).await.unwrap();
    assert_eq!(history.len(), 1);
    assert!(!history[0].is_valid, "deletion visible in history");
    assert_eq!(store.last_modified(Collection::Entries, &user).await.unwrap(), Some(now + 2));

    // Device token lifecycle incl. legacy-hash lookup.
    let token = DeviceToken {
        id: format!("tok-{now}"),
        user_id: user.clone(),
        name: "pg".into(),
        token_hash: format!("th-{now}"),
        scopes: vec!["api:entries:read".into()],
        created_at: now,
        last_used_at: None,
        revoked: false,
        legacy_hash: Some(format!("lh-{now}")),
    };
    store.insert_device_token(&token).await.unwrap();
    assert!(store.get_device_token_by_hash(&format!("th-{now}")).await.unwrap().is_some());
    assert!(store.get_device_token_by_hash(&format!("lh-{now}")).await.unwrap().is_some());
    assert!(store.revoke_device_token(&user, &format!("tok-{now}")).await.unwrap());

    // Daily-count aggregation must behave identically on Postgres — the `/days` view
    // relies on the integer day-bucket expression dividing the same way as SQLite/D1.
    const DAY_MS: i64 = 86_400_000;
    let dayuser = format!("pg-days-{now}");
    let base = 19_000 * DAY_MS; // a fixed, far-future-proof day so buckets are stable
    for (i, t) in [base + 1_000, base + 2_000, base + DAY_MS + 5_000].iter().enumerate() {
        store
            .upsert_document(Collection::Entries, sgv_doc(&dayuser, &format!("d{i}"), *t, 120, now))
            .await
            .unwrap();
    }
    let days = store
        .daily_counts(Collection::Entries, &dayuser, "sgv", 0)
        .await
        .unwrap();
    assert_eq!(days.len(), 2, "two distinct days on Postgres");
    assert_eq!(days[0].day_index, 19_001, "newest day first, correct bucket");
    assert_eq!(days[0].n, 1);
    assert_eq!(days[1].day_index, 19_000);
    assert_eq!(days[1].n, 2);
    assert_eq!(days[1].first_ms, base + 1_000);
    assert_eq!(days[1].last_ms, base + 2_000);

    eprintln!("postgres_parity passed against {url}");
}
