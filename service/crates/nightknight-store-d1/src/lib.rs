//! # nightknight-store-d1
//!
//! The [`Storage`] backend for Cloudflare D1 (SQLite running on the Workers edge).
//! It executes the exact statements from [`nightknight_storage::sql`] — the same
//! query strings the sqlx backend runs — so behaviour matches what the storage
//! contract tests verify. This crate only compiles for `wasm32`; on every other
//! target it is intentionally empty (the Workers runtime does not exist there).

#![cfg(target_arch = "wasm32")]

use async_trait::async_trait;
use serde_json::Value;
use worker::wasm_bindgen::JsValue;
use worker::D1Database;

use nightknight_storage::{
    connector_credential_from_cols, day_count_from_cols, device_token_from_cols, model::Param,
    push_token_from_cols, sql, stored_doc_from_cols, user_from_cols, Collection, ConnectorCredential,
    DayCount, DeviceToken, DocQuery, PushToken, Result, StoredDoc, Storage, StorageError, User,
    WriteOutcome,
};

/// A [`Storage`] backed by a bound D1 database.
pub struct D1Store {
    db: D1Database,
}

impl D1Store {
    pub fn new(db: D1Database) -> Self {
        D1Store { db }
    }

    async fn rows(&self, query: &str, params: Vec<Param>) -> Result<Vec<Value>> {
        let js = to_js(&params);
        let stmt = self.db.prepare(query).bind(&js).map_err(be)?;
        let result = stmt.all().await.map_err(be)?;
        result.results::<Value>().map_err(be)
    }

    async fn first(&self, query: &str, params: Vec<Param>) -> Result<Option<Value>> {
        Ok(self.rows(query, params).await?.into_iter().next())
    }

    async fn run(&self, query: &str, params: Vec<Param>) -> Result<()> {
        let js = to_js(&params);
        self.db.prepare(query).bind(&js).map_err(be)?.run().await.map_err(be)?;
        Ok(())
    }
}

fn be<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Backend(e.to_string())
}

fn to_js(params: &[Param]) -> Vec<JsValue> {
    params
        .iter()
        .map(|p| match p {
            Param::Text(s) => JsValue::from(s.as_str()),
            // D1 params are JS values, which are f64 — there is no integer JS-number type
            // it accepts (BigInt isn't a supported D1 bind type). This is invisible for
            // comparisons/LIMIT, but it makes SQLite do REAL arithmetic if a bound int is
            // used in an arithmetic expression. So shared SQL must NOT do arithmetic on a
            // bound int param — inline server-computed integer constants instead (see
            // `sql::daily_counts`).
            Param::Int(i) => JsValue::from_f64(*i as f64),
            Param::Null | Param::IntNull => JsValue::NULL,
        })
        .collect()
}

// --- column extraction from a D1 row (a JSON object keyed by column name) ---

fn col_str(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}
fn col_opt_str(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str().map(str::to_string))
}
fn col_i64(v: &Value, k: &str) -> i64 {
    v.get(k)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}
fn col_opt_i64(v: &Value, k: &str) -> Option<i64> {
    v.get(k)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
}

fn row_to_doc(v: &Value) -> Result<StoredDoc> {
    stored_doc_from_cols(
        col_str(v, "identifier"),
        col_str(v, "user_id"),
        col_i64(v, "mills"),
        col_opt_str(v, "doc_type"),
        col_i64(v, "srv_created"),
        col_i64(v, "srv_modified"),
        col_i64(v, "is_valid"),
        col_i64(v, "is_read_only"),
        col_opt_str(v, "subject"),
        col_str(v, "doc"),
    )
}

fn row_to_day_count(v: &Value) -> DayCount {
    day_count_from_cols(
        col_i64(v, "day"),
        col_i64(v, "n"),
        col_i64(v, "first_ms"),
        col_i64(v, "last_ms"),
    )
}

fn row_to_user(v: &Value) -> User {
    user_from_cols(
        col_str(v, "id"),
        col_str(v, "subject"),
        col_opt_str(v, "display_name"),
        col_i64(v, "is_admin"),
        col_str(v, "preferred_unit"),
        col_i64(v, "created_at"),
    )
}

fn row_to_cred(v: &Value) -> ConnectorCredential {
    connector_credential_from_cols(
        col_str(v, "user_id"),
        col_str(v, "provider"),
        col_i64(v, "enabled"),
        col_str(v, "secret_enc"),
        col_opt_str(v, "region"),
        col_i64(v, "created_at"),
        col_i64(v, "updated_at"),
        col_opt_i64(v, "last_sync_at"),
        col_opt_str(v, "last_status"),
    )
}

fn row_to_token(v: &Value) -> Result<DeviceToken> {
    device_token_from_cols(
        col_str(v, "id"),
        col_str(v, "user_id"),
        col_str(v, "name"),
        col_str(v, "token_hash"),
        col_str(v, "scopes"),
        col_i64(v, "created_at"),
        col_opt_i64(v, "last_used_at"),
        col_i64(v, "revoked"),
        col_opt_str(v, "legacy_hash"),
    )
}

fn row_to_push_token(v: &Value) -> PushToken {
    push_token_from_cols(
        col_str(v, "user_id"),
        col_str(v, "token"),
        col_str(v, "environment"),
        col_str(v, "bundle_id"),
        col_i64(v, "updated_at"),
    )
}

#[async_trait(?Send)]
impl Storage for D1Store {
    async fn migrate(&self) -> Result<()> {
        for stmt in sql::schema_statements() {
            self.run(&stmt, vec![]).await?;
        }
        Ok(())
    }

    async fn upsert_document(&self, c: Collection, doc: StoredDoc) -> Result<WriteOutcome> {
        let existed = self
            .get_document(c, &doc.user_id, &doc.identifier)
            .await?
            .is_some();
        let (q, params) = sql::upsert_document(c, &doc);
        self.run(&q, params).await?;
        Ok(if existed {
            WriteOutcome::Updated(doc)
        } else {
            WriteOutcome::Created(doc)
        })
    }

    async fn get_document(
        &self,
        c: Collection,
        user_id: &str,
        identifier: &str,
    ) -> Result<Option<StoredDoc>> {
        let (q, params) = sql::get_document(c, user_id, identifier);
        match self.first(&q, params).await? {
            Some(v) => Ok(Some(row_to_doc(&v)?)),
            None => Ok(None),
        }
    }

    async fn search_documents(
        &self,
        c: Collection,
        user_id: &str,
        query: &DocQuery,
    ) -> Result<Vec<StoredDoc>> {
        let (q, params) = sql::search_documents(c, user_id, query);
        self.rows(&q, params).await?.iter().map(row_to_doc).collect()
    }

    async fn soft_delete_document(
        &self,
        c: Collection,
        user_id: &str,
        identifier: &str,
        srv_modified: i64,
    ) -> Result<bool> {
        let was_valid = self
            .get_document(c, user_id, identifier)
            .await?
            .map(|d| d.is_valid)
            .unwrap_or(false);
        if was_valid {
            let (q, params) = sql::soft_delete_document(c, user_id, identifier, srv_modified);
            self.run(&q, params).await?;
        }
        Ok(was_valid)
    }

    async fn last_modified(&self, c: Collection, user_id: &str) -> Result<Option<i64>> {
        let (q, params) = sql::last_modified(c, user_id);
        Ok(self.first(&q, params).await?.and_then(|v| col_opt_i64(&v, "lm")))
    }

    async fn daily_counts(
        &self,
        c: Collection,
        user_id: &str,
        doc_type: &str,
        tz_offset_ms: i64,
    ) -> Result<Vec<DayCount>> {
        let (q, params) = sql::daily_counts(c, user_id, doc_type, tz_offset_ms);
        Ok(self.rows(&q, params).await?.iter().map(row_to_day_count).collect())
    }

    async fn downsampled_documents(
        &self,
        c: Collection,
        user_id: &str,
        doc_type: &str,
        start_ms: i64,
        end_ms: i64,
        bucket_ms: i64,
        limit: Option<i64>,
    ) -> Result<Vec<StoredDoc>> {
        let (q, params) =
            sql::downsampled_documents(c, user_id, doc_type, start_ms, end_ms, bucket_ms, limit);
        self.rows(&q, params).await?.iter().map(row_to_doc).collect()
    }

    async fn history_since(
        &self,
        c: Collection,
        user_id: &str,
        since_srv_modified: i64,
        limit: i64,
    ) -> Result<Vec<StoredDoc>> {
        let (q, params) = sql::history_since(c, user_id, since_srv_modified, limit);
        self.rows(&q, params).await?.iter().map(row_to_doc).collect()
    }

    async fn upsert_user(&self, user: &User) -> Result<()> {
        let (q, params) = sql::upsert_user(user);
        self.run(&q, params).await
    }

    async fn get_user_by_subject(&self, subject: &str) -> Result<Option<User>> {
        let (q, params) = sql::get_user_by_subject(subject);
        Ok(self.first(&q, params).await?.map(|v| row_to_user(&v)))
    }

    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> {
        let (q, params) = sql::get_user_by_id(id);
        Ok(self.first(&q, params).await?.map(|v| row_to_user(&v)))
    }

    async fn rekey_user_subject(&self, old_subject: &str, new_subject: &str) -> Result<bool> {
        // Existence check keeps the boolean honest without relying on driver metadata.
        let existed = self.get_user_by_subject(old_subject).await?.is_some();
        if existed {
            let (q, params) = sql::rekey_user_subject(old_subject, new_subject);
            self.run(&q, params).await?;
        }
        Ok(existed)
    }

    async fn insert_device_token(&self, token: &DeviceToken) -> Result<()> {
        let (q, params) = sql::insert_device_token(token);
        self.run(&q, params).await
    }

    async fn get_device_token_by_hash(&self, token_hash: &str) -> Result<Option<DeviceToken>> {
        let (q, params) = sql::get_device_token_by_hash(token_hash);
        match self.first(&q, params).await? {
            Some(v) => Ok(Some(row_to_token(&v)?)),
            None => Ok(None),
        }
    }

    async fn list_device_tokens(&self, user_id: &str) -> Result<Vec<DeviceToken>> {
        let (q, params) = sql::list_device_tokens(user_id);
        self.rows(&q, params).await?.iter().map(row_to_token).collect()
    }

    async fn revoke_device_token(&self, user_id: &str, id: &str) -> Result<bool> {
        // Existence check keeps the boolean honest without relying on driver metadata.
        let exists = self
            .list_device_tokens(user_id)
            .await?
            .into_iter()
            .any(|t| t.id == id && !t.revoked);
        if exists {
            let (q, params) = sql::revoke_device_token(user_id, id);
            self.run(&q, params).await?;
        }
        Ok(exists)
    }

    async fn touch_device_token(&self, token_hash: &str, when_ms: i64) -> Result<()> {
        let (q, params) = sql::touch_device_token(token_hash, when_ms);
        self.run(&q, params).await
    }

    async fn upsert_connector_credential(&self, cred: &ConnectorCredential) -> Result<()> {
        let (q, params) = sql::upsert_connector_credential(cred);
        self.run(&q, params).await
    }

    async fn get_connector_credential(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<Option<ConnectorCredential>> {
        let (q, params) = sql::get_connector_credential(user_id, provider);
        Ok(self.first(&q, params).await?.map(|v| row_to_cred(&v)))
    }

    async fn list_connector_credentials(&self, user_id: &str) -> Result<Vec<ConnectorCredential>> {
        let (q, params) = sql::list_connector_credentials(user_id);
        Ok(self.rows(&q, params).await?.iter().map(row_to_cred).collect())
    }

    async fn list_enabled_connector_credentials(&self) -> Result<Vec<ConnectorCredential>> {
        let (q, params) = sql::list_enabled_connector_credentials();
        Ok(self.rows(&q, params).await?.iter().map(row_to_cred).collect())
    }

    async fn delete_connector_credential(&self, user_id: &str, provider: &str) -> Result<bool> {
        let exists = self.get_connector_credential(user_id, provider).await?.is_some();
        if exists {
            let (q, params) = sql::delete_connector_credential(user_id, provider);
            self.run(&q, params).await?;
        }
        Ok(exists)
    }

    async fn update_connector_sync(
        &self,
        user_id: &str,
        provider: &str,
        last_sync_at: i64,
        last_status: &str,
    ) -> Result<()> {
        let (q, params) = sql::update_connector_sync(user_id, provider, last_sync_at, last_status);
        self.run(&q, params).await
    }

    async fn upsert_push_token(&self, token: &PushToken) -> Result<()> {
        let (q, params) = sql::upsert_push_token(token);
        self.run(&q, params).await
    }

    async fn list_push_tokens(&self, user_id: &str) -> Result<Vec<PushToken>> {
        let (q, params) = sql::list_push_tokens(user_id);
        Ok(self.rows(&q, params).await?.iter().map(row_to_push_token).collect())
    }

    async fn delete_push_token(&self, user_id: &str, token: &str) -> Result<bool> {
        let exists = self
            .list_push_tokens(user_id)
            .await?
            .into_iter()
            .any(|t| t.token == token);
        if exists {
            let (q, params) = sql::delete_push_token(user_id, token);
            self.run(&q, params).await?;
        }
        Ok(exists)
    }
}
