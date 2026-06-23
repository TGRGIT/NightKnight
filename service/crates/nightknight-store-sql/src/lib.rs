//! # nightknight-store-sql
//!
//! A [`Storage`] backend built on [`sqlx`]'s `Any` driver, so the *same* code talks
//! to **SQLite** (used for the fast in-memory contract tests and for a lightweight
//! single-file self-host) and **Postgres** (the container deployment). It executes
//! the portable statements from [`nightknight_storage::sql`] verbatim — the query
//! logic is shared with the Cloudflare D1 backend, so all backends behave alike.
//!
//! The contract-test suite at the bottom of this file is the executable spec for
//! "how storage must behave"; it runs against an in-memory SQLite database and
//! documents, in plain language, why each guarantee matters for a health app.

use async_trait::async_trait;
use sqlx::any::{AnyPoolOptions, AnyRow};
use sqlx::{AnyPool, Row};

use nightknight_storage::{
    connector_credential_from_cols, device_token_from_cols, model::Param, sql, stored_doc_from_cols,
    user_from_cols, Collection, ConnectorCredential, DeviceToken, DocQuery, Result, StoredDoc,
    Storage, StorageError, User, WriteOutcome,
};

/// A SQL-backed store over a sqlx `Any` pool (SQLite or Postgres).
pub struct SqlStore {
    pool: AnyPool,
    /// Postgres needs `$1`-style placeholders; SQLite (and D1) use `?`. The shared
    /// SQL is written with `?`, so we rewrite for Postgres at execution time.
    is_postgres: bool,
}

/// Rewrite positional `?` placeholders to Postgres `$1, $2, …`. Safe because none of
/// NightKnight's SQL uses `?` as an operator — every `?` is a bind placeholder.
fn to_pg_placeholders(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let mut n = 0u32;
    for c in sql.chars() {
        if c == '?' {
            n += 1;
            out.push('$');
            out.push_str(&n.to_string());
        } else {
            out.push(c);
        }
    }
    out
}

fn backend_err<E: std::fmt::Display>(e: E) -> StorageError {
    StorageError::Backend(e.to_string())
}

impl SqlStore {
    /// Connect using a URL (`sqlite::memory:`, `sqlite:///path/file.db`,
    /// `postgres://user:pass@host/db`). Uses a sensible default pool size.
    pub async fn connect(url: &str) -> Result<Self> {
        // SQLite in-memory databases are per-connection, so a multi-connection pool
        // would see different data on each query. Pin memory DBs to one connection.
        let max = if url.starts_with("sqlite::memory:") { 1 } else { 5 };
        Self::connect_with_pool_size(url, max).await
    }

    /// Connect with an explicit maximum pool size.
    pub async fn connect_with_pool_size(url: &str, max_connections: u32) -> Result<Self> {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .map_err(backend_err)?;
        let is_postgres = url.starts_with("postgres://") || url.starts_with("postgresql://");
        Ok(Self { pool, is_postgres })
    }

    /// Adapt the shared `?`-placeholder SQL to the active dialect.
    fn dialect_sql(&self, sql: &str) -> String {
        if self.is_postgres {
            to_pg_placeholders(sql)
        } else {
            sql.to_string()
        }
    }

    /// Expose the underlying pool (used by the server runtime for health checks).
    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    async fn fetch_all(&self, sql: &str, params: Vec<Param>) -> Result<Vec<AnyRow>> {
        let sql = self.dialect_sql(sql);
        let mut q = sqlx::query(&sql);
        for p in &params {
            q = match p {
                Param::Text(s) => q.bind(s.clone()),
                Param::Int(i) => q.bind(*i),
                Param::Null => q.bind(Option::<String>::None),
                Param::IntNull => q.bind(Option::<i64>::None),
            };
        }
        q.fetch_all(&self.pool).await.map_err(backend_err)
    }

    async fn fetch_optional(&self, sql: &str, params: Vec<Param>) -> Result<Option<AnyRow>> {
        let sql = self.dialect_sql(sql);
        let mut q = sqlx::query(&sql);
        for p in &params {
            q = match p {
                Param::Text(s) => q.bind(s.clone()),
                Param::Int(i) => q.bind(*i),
                Param::Null => q.bind(Option::<String>::None),
                Param::IntNull => q.bind(Option::<i64>::None),
            };
        }
        q.fetch_optional(&self.pool).await.map_err(backend_err)
    }

    async fn execute(&self, sql: &str, params: Vec<Param>) -> Result<u64> {
        let sql = self.dialect_sql(sql);
        let mut q = sqlx::query(&sql);
        for p in &params {
            q = match p {
                Param::Text(s) => q.bind(s.clone()),
                Param::Int(i) => q.bind(*i),
                Param::Null => q.bind(Option::<String>::None),
                Param::IntNull => q.bind(Option::<i64>::None),
            };
        }
        let r = q.execute(&self.pool).await.map_err(backend_err)?;
        Ok(r.rows_affected())
    }
}

/// Map a document row (columns in [`sql::DOC_COLS`] order) into a [`StoredDoc`].
fn row_to_doc(row: &AnyRow) -> Result<StoredDoc> {
    stored_doc_from_cols(
        row.try_get(0).map_err(backend_err)?,
        row.try_get(1).map_err(backend_err)?,
        row.try_get(2).map_err(backend_err)?,
        row.try_get(3).map_err(backend_err)?,
        row.try_get(4).map_err(backend_err)?,
        row.try_get(5).map_err(backend_err)?,
        row.try_get(6).map_err(backend_err)?,
        row.try_get(7).map_err(backend_err)?,
        row.try_get(8).map_err(backend_err)?,
        row.try_get(9).map_err(backend_err)?,
    )
}

fn row_to_user(row: &AnyRow) -> Result<User> {
    Ok(user_from_cols(
        row.try_get(0).map_err(backend_err)?,
        row.try_get(1).map_err(backend_err)?,
        row.try_get(2).map_err(backend_err)?,
        row.try_get(3).map_err(backend_err)?,
        row.try_get(4).map_err(backend_err)?,
        row.try_get(5).map_err(backend_err)?,
    ))
}

fn row_to_cred(row: &AnyRow) -> Result<ConnectorCredential> {
    Ok(connector_credential_from_cols(
        row.try_get(0).map_err(backend_err)?,
        row.try_get(1).map_err(backend_err)?,
        row.try_get(2).map_err(backend_err)?,
        row.try_get(3).map_err(backend_err)?,
        row.try_get(4).map_err(backend_err)?,
        row.try_get(5).map_err(backend_err)?,
        row.try_get(6).map_err(backend_err)?,
        row.try_get(7).map_err(backend_err)?,
        row.try_get(8).map_err(backend_err)?,
    ))
}

fn row_to_token(row: &AnyRow) -> Result<DeviceToken> {
    device_token_from_cols(
        row.try_get(0).map_err(backend_err)?,
        row.try_get(1).map_err(backend_err)?,
        row.try_get(2).map_err(backend_err)?,
        row.try_get(3).map_err(backend_err)?,
        row.try_get(4).map_err(backend_err)?,
        row.try_get(5).map_err(backend_err)?,
        row.try_get(6).map_err(backend_err)?,
        row.try_get(7).map_err(backend_err)?,
        row.try_get(8).map_err(backend_err)?,
    )
}

#[async_trait]
impl Storage for SqlStore {
    async fn migrate(&self) -> Result<()> {
        for stmt in sql::schema_statements() {
            self.execute(&stmt, vec![]).await?;
        }
        Ok(())
    }

    async fn upsert_document(&self, c: Collection, doc: StoredDoc) -> Result<WriteOutcome> {
        let existed = self
            .get_document(c, &doc.user_id, &doc.identifier)
            .await?
            .is_some();
        let (sql, params) = sql::upsert_document(c, &doc);
        self.execute(&sql, params).await?;
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
        let (sql, params) = sql::get_document(c, user_id, identifier);
        match self.fetch_optional(&sql, params).await? {
            Some(row) => Ok(Some(row_to_doc(&row)?)),
            None => Ok(None),
        }
    }

    async fn search_documents(
        &self,
        c: Collection,
        user_id: &str,
        q: &DocQuery,
    ) -> Result<Vec<StoredDoc>> {
        let (sql, params) = sql::search_documents(c, user_id, q);
        self.fetch_all(&sql, params)
            .await?
            .iter()
            .map(row_to_doc)
            .collect()
    }

    async fn soft_delete_document(
        &self,
        c: Collection,
        user_id: &str,
        identifier: &str,
        srv_modified: i64,
    ) -> Result<bool> {
        let (sql, params) = sql::soft_delete_document(c, user_id, identifier, srv_modified);
        Ok(self.execute(&sql, params).await? > 0)
    }

    async fn last_modified(&self, c: Collection, user_id: &str) -> Result<Option<i64>> {
        let (sql, params) = sql::last_modified(c, user_id);
        match self.fetch_optional(&sql, params).await? {
            Some(row) => Ok(row.try_get::<Option<i64>, _>(0).map_err(backend_err)?),
            None => Ok(None),
        }
    }

    async fn history_since(
        &self,
        c: Collection,
        user_id: &str,
        since_srv_modified: i64,
        limit: i64,
    ) -> Result<Vec<StoredDoc>> {
        let (sql, params) = sql::history_since(c, user_id, since_srv_modified, limit);
        self.fetch_all(&sql, params)
            .await?
            .iter()
            .map(row_to_doc)
            .collect()
    }

    async fn upsert_user(&self, user: &User) -> Result<()> {
        let (sql, params) = sql::upsert_user(user);
        self.execute(&sql, params).await?;
        Ok(())
    }

    async fn get_user_by_subject(&self, subject: &str) -> Result<Option<User>> {
        let (sql, params) = sql::get_user_by_subject(subject);
        match self.fetch_optional(&sql, params).await? {
            Some(row) => Ok(Some(row_to_user(&row)?)),
            None => Ok(None),
        }
    }

    async fn get_user_by_id(&self, id: &str) -> Result<Option<User>> {
        let (sql, params) = sql::get_user_by_id(id);
        match self.fetch_optional(&sql, params).await? {
            Some(row) => Ok(Some(row_to_user(&row)?)),
            None => Ok(None),
        }
    }

    async fn rekey_user_subject(&self, old_subject: &str, new_subject: &str) -> Result<bool> {
        let (sql, params) = sql::rekey_user_subject(old_subject, new_subject);
        Ok(self.execute(&sql, params).await? > 0)
    }

    async fn insert_device_token(&self, token: &DeviceToken) -> Result<()> {
        let (sql, params) = sql::insert_device_token(token);
        self.execute(&sql, params).await?;
        Ok(())
    }

    async fn get_device_token_by_hash(&self, token_hash: &str) -> Result<Option<DeviceToken>> {
        let (sql, params) = sql::get_device_token_by_hash(token_hash);
        match self.fetch_optional(&sql, params).await? {
            Some(row) => Ok(Some(row_to_token(&row)?)),
            None => Ok(None),
        }
    }

    async fn list_device_tokens(&self, user_id: &str) -> Result<Vec<DeviceToken>> {
        let (sql, params) = sql::list_device_tokens(user_id);
        self.fetch_all(&sql, params)
            .await?
            .iter()
            .map(row_to_token)
            .collect()
    }

    async fn revoke_device_token(&self, user_id: &str, id: &str) -> Result<bool> {
        let (sql, params) = sql::revoke_device_token(user_id, id);
        Ok(self.execute(&sql, params).await? > 0)
    }

    async fn touch_device_token(&self, token_hash: &str, when_ms: i64) -> Result<()> {
        let (sql, params) = sql::touch_device_token(token_hash, when_ms);
        self.execute(&sql, params).await?;
        Ok(())
    }

    async fn upsert_connector_credential(&self, cred: &ConnectorCredential) -> Result<()> {
        let (sql, params) = sql::upsert_connector_credential(cred);
        self.execute(&sql, params).await?;
        Ok(())
    }

    async fn get_connector_credential(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<Option<ConnectorCredential>> {
        let (sql, params) = sql::get_connector_credential(user_id, provider);
        match self.fetch_optional(&sql, params).await? {
            Some(row) => Ok(Some(row_to_cred(&row)?)),
            None => Ok(None),
        }
    }

    async fn list_connector_credentials(&self, user_id: &str) -> Result<Vec<ConnectorCredential>> {
        let (sql, params) = sql::list_connector_credentials(user_id);
        self.fetch_all(&sql, params).await?.iter().map(row_to_cred).collect()
    }

    async fn list_enabled_connector_credentials(&self) -> Result<Vec<ConnectorCredential>> {
        let (sql, params) = sql::list_enabled_connector_credentials();
        self.fetch_all(&sql, params).await?.iter().map(row_to_cred).collect()
    }

    async fn delete_connector_credential(&self, user_id: &str, provider: &str) -> Result<bool> {
        let (sql, params) = sql::delete_connector_credential(user_id, provider);
        Ok(self.execute(&sql, params).await? > 0)
    }

    async fn update_connector_sync(
        &self,
        user_id: &str,
        provider: &str,
        last_sync_at: i64,
        last_status: &str,
    ) -> Result<()> {
        let (sql, params) = sql::update_connector_sync(user_id, provider, last_sync_at, last_status);
        self.execute(&sql, params).await?;
        Ok(())
    }
}

#[cfg(test)]
mod contract_tests;

#[cfg(test)]
mod placeholder_tests {
    use super::to_pg_placeholders;

    /// `?` placeholders are rewritten to `$1, $2, …` in order, and text without
    /// placeholders is untouched. NB: this rewrite is only correct while none of the
    /// shared SQL uses Postgres `?`/`?|`/`?&` JSON operators — keep it that way.
    #[test]
    fn rewrites_question_marks_in_order() {
        assert_eq!(to_pg_placeholders("a=? AND b=?"), "a=$1 AND b=$2");
        assert_eq!(
            to_pg_placeholders("INSERT INTO t VALUES (?,?,?)"),
            "INSERT INTO t VALUES ($1,$2,$3)"
        );
        assert_eq!(to_pg_placeholders("SELECT 1"), "SELECT 1");
    }
}
