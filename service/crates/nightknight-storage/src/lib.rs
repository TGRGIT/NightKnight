//! # nightknight-storage
//!
//! The storage abstraction for NightKnight. It defines:
//!
//! * the [`Storage`] trait every backend implements,
//! * the document/user/token [`model`] types,
//! * the portable [`sql`] shared verbatim by all backends, and
//! * row-construction helpers so backends parse columns identically.
//!
//! Two backends implement [`Storage`]: `nightknight-store-sql` (sqlx → SQLite for
//! tests + Postgres for the self-hosted container) and `nightknight-store-d1`
//! (Cloudflare D1 in the worker). Because the SQL and parsing are shared, the two
//! behave identically — verified by the storage contract tests.
//!
//! ## Send-ness across runtimes
//!
//! The Cloudflare Worker runtime is single-threaded and its futures are `!Send`;
//! the native (axum/tokio) runtime needs `Send` futures. The trait therefore
//! requires `Send` everywhere *except* on `wasm32`, via `cfg_attr` on the
//! `async_trait` attribute. Each backend, compiled for only one target, picks up the
//! matching variant automatically.

pub mod model;
pub mod sql;

use serde_json::Value;

pub use model::{
    Collection, ConnectorCredential, DayCount, DeviceToken, DocQuery, Param, PushToken, StoredDoc,
    User, WriteOutcome,
};

/// A storage failure. Backends map their native errors into these variants so the
/// rest of the system never depends on a specific database library.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The underlying database returned an error.
    #[error("database error: {0}")]
    Backend(String),
    /// A stored value could not be (de)serialised.
    #[error("data error: {0}")]
    Data(String),
    /// The requested row does not exist.
    #[error("not found")]
    NotFound,
}

/// Result alias for storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

/// The persistence interface. All methods are scoped by `user_id` where applicable,
/// enforcing multi-user isolation at the lowest layer. Backends execute the shared
/// [`sql`] statements; the API layer composes these into REST semantics.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Storage {
    /// Apply the schema (idempotent — safe to run on every boot).
    async fn migrate(&self) -> Result<()>;

    // ----- documents -------------------------------------------------------

    /// Insert a document, or update it if one with the same `(user_id, identifier)`
    /// already exists. Reports whether a new document was [`WriteOutcome::Created`]
    /// or an existing one [`WriteOutcome::Updated`].
    async fn upsert_document(&self, c: Collection, doc: StoredDoc) -> Result<WriteOutcome>;

    /// Fetch one document by identifier for a user.
    async fn get_document(
        &self,
        c: Collection,
        user_id: &str,
        identifier: &str,
    ) -> Result<Option<StoredDoc>>;

    /// Search a user's documents with the given filter.
    async fn search_documents(
        &self,
        c: Collection,
        user_id: &str,
        q: &DocQuery,
    ) -> Result<Vec<StoredDoc>>;

    /// Soft-delete a document; returns `true` if a valid document was flagged.
    async fn soft_delete_document(
        &self,
        c: Collection,
        user_id: &str,
        identifier: &str,
        srv_modified: i64,
    ) -> Result<bool>;

    /// Latest server-modification time for a user's collection (`None` if empty).
    async fn last_modified(&self, c: Collection, user_id: &str) -> Result<Option<i64>>;

    /// Per-local-day reading counts (newest day first) for a collection and document
    /// type, bucketed in the given UTC offset (minutes east of UTC, as milliseconds).
    /// Aggregates on the indexed `mills` column only, so it scales to thousands of days
    /// without loading document bodies — the basis for the data-coverage view.
    async fn daily_counts(
        &self,
        c: Collection,
        user_id: &str,
        doc_type: &str,
        tz_offset_ms: i64,
    ) -> Result<Vec<DayCount>>;

    /// One representative document per fixed time-bucket across `[start_ms, end_ms]`,
    /// newest first — a **downsampled** view for aggregate reports (AGP, the metrics
    /// export) that must cover a long window without loading every reading.
    ///
    /// `bucket_ms` is the bucket width in milliseconds (e.g. 300_000 for 5-minute
    /// resolution). Within each bucket the earliest reading is kept. Like
    /// [`daily_counts`](Self::daily_counts) the bucketing is an **index-only `GROUP BY`
    /// on `mills`** (no JSON parsing, no document bodies scanned for the grouping), so a
    /// 90-day window collapses to a few tens of thousands of rows regardless of how dense
    /// the source data is — bounding the per-request work that was 503-ing the Worker on
    /// an unbounded fetch. Only the surviving representative rows have their bodies read.
    ///
    /// `limit` caps the returned rows (`None` = all buckets, newest first). A caller
    /// paginating a large window passes `Some(batch)` and re-requests with a shrunk
    /// `end_ms`, so no single call materialises more than `limit` document bodies.
    // The window/bucket/limit are distinct scalars; bundling them into a struct would
    // obscure more than it helps for a single call site.
    #[allow(clippy::too_many_arguments)]
    async fn downsampled_documents(
        &self,
        c: Collection,
        user_id: &str,
        doc_type: &str,
        start_ms: i64,
        end_ms: i64,
        bucket_ms: i64,
        limit: Option<i64>,
    ) -> Result<Vec<StoredDoc>>;

    /// Documents changed since `since_srv_modified` (oldest first, capped at `limit`),
    /// including soft-deleted ones so clients learn about deletions.
    async fn history_since(
        &self,
        c: Collection,
        user_id: &str,
        since_srv_modified: i64,
        limit: i64,
    ) -> Result<Vec<StoredDoc>>;

    // ----- users -----------------------------------------------------------

    async fn upsert_user(&self, user: &User) -> Result<()>;
    async fn get_user_by_subject(&self, subject: &str) -> Result<Option<User>>;
    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>>;
    /// Re-key a user from `old_subject` to `new_subject`, preserving the row's `id`
    /// (and therefore every row that references it by `user_id`). Returns whether a row
    /// was changed. Used by the one-time legacy-subject migration.
    async fn rekey_user_subject(&self, old_subject: &str, new_subject: &str) -> Result<bool>;

    // ----- device tokens ---------------------------------------------------

    async fn insert_device_token(&self, token: &DeviceToken) -> Result<()>;
    async fn get_device_token_by_hash(&self, token_hash: &str) -> Result<Option<DeviceToken>>;
    async fn list_device_tokens(&self, user_id: &str) -> Result<Vec<DeviceToken>>;
    async fn revoke_device_token(&self, user_id: &str, id: &str) -> Result<bool>;
    async fn touch_device_token(&self, token_hash: &str, when_ms: i64) -> Result<()>;

    // ----- connector credentials -------------------------------------------

    async fn upsert_connector_credential(&self, cred: &ConnectorCredential) -> Result<()>;
    async fn get_connector_credential(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<Option<ConnectorCredential>>;
    async fn list_connector_credentials(&self, user_id: &str) -> Result<Vec<ConnectorCredential>>;
    /// Every enabled credential across all users (for the sync scheduler).
    async fn list_enabled_connector_credentials(&self) -> Result<Vec<ConnectorCredential>>;
    async fn delete_connector_credential(&self, user_id: &str, provider: &str) -> Result<bool>;
    async fn update_connector_sync(
        &self,
        user_id: &str,
        provider: &str,
        last_sync_at: i64,
        last_status: &str,
    ) -> Result<()>;

    // ----- push tokens -----------------------------------------------------

    /// Register or refresh a user's APNs device token (idempotent on `(user_id, token)`).
    async fn upsert_push_token(&self, token: &PushToken) -> Result<()>;
    /// Every device token a user has registered (the push send list).
    async fn list_push_tokens(&self, user_id: &str) -> Result<Vec<PushToken>>;
    /// Remove one of a user's device tokens; returns whether a row was deleted.
    async fn delete_push_token(&self, user_id: &str, token: &str) -> Result<bool>;
}

/// Build a [`StoredDoc`] from already-extracted column values. Backends call this so
/// the (potentially error-prone) JSON/boolean parsing happens in exactly one place.
#[allow(clippy::too_many_arguments)]
pub fn stored_doc_from_cols(
    identifier: String,
    user_id: String,
    mills: i64,
    doc_type: Option<String>,
    srv_created: i64,
    srv_modified: i64,
    is_valid: i64,
    is_read_only: i64,
    subject: Option<String>,
    doc: String,
) -> Result<StoredDoc> {
    let doc: Value = serde_json::from_str(&doc).map_err(|e| StorageError::Data(e.to_string()))?;
    Ok(StoredDoc {
        identifier,
        user_id,
        mills,
        doc_type,
        srv_created,
        srv_modified,
        is_valid: is_valid != 0,
        is_read_only: is_read_only != 0,
        subject,
        doc,
    })
}

/// Build a [`User`] from extracted column values.
pub fn user_from_cols(
    id: String,
    subject: String,
    display_name: Option<String>,
    is_admin: i64,
    preferred_unit: String,
    created_at: i64,
) -> User {
    User {
        id,
        subject,
        display_name,
        is_admin: is_admin != 0,
        preferred_unit,
        created_at,
    }
}

/// Build a [`DeviceToken`] from extracted column values (parsing the JSON scopes).
#[allow(clippy::too_many_arguments)]
pub fn device_token_from_cols(
    id: String,
    user_id: String,
    name: String,
    token_hash: String,
    scopes: String,
    created_at: i64,
    last_used_at: Option<i64>,
    revoked: i64,
    legacy_hash: Option<String>,
) -> Result<DeviceToken> {
    let scopes: Vec<String> =
        serde_json::from_str(&scopes).map_err(|e| StorageError::Data(e.to_string()))?;
    Ok(DeviceToken {
        id,
        user_id,
        name,
        token_hash,
        scopes,
        created_at,
        last_used_at,
        revoked: revoked != 0,
        legacy_hash,
    })
}

/// Build a [`DayCount`] from extracted aggregate column values (`day, n, first_ms,
/// last_ms` order — matching [`sql::daily_counts`](crate::sql::daily_counts)).
pub fn day_count_from_cols(day_index: i64, n: i64, first_ms: i64, last_ms: i64) -> DayCount {
    DayCount {
        day_index,
        n,
        first_ms,
        last_ms,
    }
}

/// Build a [`PushToken`] from extracted column values (in `PUSH_COLS` order).
pub fn push_token_from_cols(
    user_id: String,
    token: String,
    environment: String,
    bundle_id: String,
    updated_at: i64,
) -> PushToken {
    PushToken {
        user_id,
        token,
        environment,
        bundle_id,
        updated_at,
    }
}

/// Build a [`ConnectorCredential`] from extracted column values (in `CRED_COLS` order).
#[allow(clippy::too_many_arguments)]
pub fn connector_credential_from_cols(
    user_id: String,
    provider: String,
    enabled: i64,
    secret_enc: String,
    region: Option<String>,
    created_at: i64,
    updated_at: i64,
    last_sync_at: Option<i64>,
    last_status: Option<String>,
) -> ConnectorCredential {
    ConnectorCredential {
        user_id,
        provider,
        enabled: enabled != 0,
        secret_enc,
        region,
        created_at,
        updated_at,
        last_sync_at,
        last_status,
    }
}
