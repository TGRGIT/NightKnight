//! The portable SQL shared by every backend.
//!
//! These builders return a statement string with `?` placeholders plus the
//! positional [`Param`]s to bind. Both the Cloudflare D1 backend and the sqlx
//! (SQLite/Postgres) backend execute exactly these strings, so query logic lives in
//! one place and behaves identically across deployment targets. The SQL is written
//! in the portable subset both engines share: `?` placeholders, `BIGINT`/`TEXT`,
//! integer booleans, and `INSERT … ON CONFLICT … DO UPDATE`.

use serde_json::Value;

use crate::model::{
    Collection, ConnectorCredential, DeviceToken, DocQuery, Param, StoredDoc, User,
};

/// Column list for document rows, in a fixed order shared by reads and writes.
pub const DOC_COLS: &str =
    "identifier,user_id,mills,doc_type,srv_created,srv_modified,is_valid,is_read_only,subject,doc";

const USER_COLS: &str = "id,subject,display_name,is_admin,preferred_unit,created_at";
const CRED_COLS: &str =
    "user_id,provider,enabled,secret_enc,region,created_at,updated_at,last_sync_at,last_status";
const TOKEN_COLS: &str =
    "id,user_id,name,token_hash,scopes,created_at,last_used_at,revoked,legacy_hash";

/// All DDL statements that build the schema, in order. Every statement is
/// `IF NOT EXISTS`, so running them on each boot is a safe, idempotent migration.
pub fn schema_statements() -> Vec<String> {
    let mut s = vec![
        "CREATE TABLE IF NOT EXISTS users (\
            id TEXT PRIMARY KEY, \
            subject TEXT NOT NULL UNIQUE, \
            display_name TEXT, \
            is_admin BIGINT NOT NULL DEFAULT 0, \
            preferred_unit TEXT NOT NULL DEFAULT 'mg/dl', \
            created_at BIGINT NOT NULL)"
            .to_string(),
        "CREATE TABLE IF NOT EXISTS device_tokens (\
            id TEXT PRIMARY KEY, \
            user_id TEXT NOT NULL, \
            name TEXT NOT NULL, \
            token_hash TEXT NOT NULL UNIQUE, \
            scopes TEXT NOT NULL, \
            created_at BIGINT NOT NULL, \
            last_used_at BIGINT, \
            revoked BIGINT NOT NULL DEFAULT 0, \
            legacy_hash TEXT)"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS idx_device_tokens_user ON device_tokens(user_id)".to_string(),
        "CREATE INDEX IF NOT EXISTS idx_device_tokens_legacy ON device_tokens(legacy_hash)"
            .to_string(),
        "CREATE TABLE IF NOT EXISTS connector_credentials (\
            user_id TEXT NOT NULL, \
            provider TEXT NOT NULL, \
            enabled BIGINT NOT NULL DEFAULT 1, \
            secret_enc TEXT NOT NULL, \
            region TEXT, \
            created_at BIGINT NOT NULL, \
            updated_at BIGINT NOT NULL, \
            last_sync_at BIGINT, \
            last_status TEXT, \
            PRIMARY KEY (user_id, provider))"
            .to_string(),
        "CREATE INDEX IF NOT EXISTS idx_connector_enabled ON connector_credentials(enabled)"
            .to_string(),
    ];
    for c in Collection::all() {
        let t = c.table();
        s.push(format!(
            "CREATE TABLE IF NOT EXISTS {t} (\
                identifier TEXT NOT NULL, \
                user_id TEXT NOT NULL, \
                mills BIGINT NOT NULL, \
                doc_type TEXT, \
                srv_created BIGINT NOT NULL, \
                srv_modified BIGINT NOT NULL, \
                is_valid BIGINT NOT NULL DEFAULT 1, \
                is_read_only BIGINT NOT NULL DEFAULT 0, \
                subject TEXT, \
                doc TEXT NOT NULL, \
                PRIMARY KEY (user_id, identifier))"
        ));
        s.push(format!(
            "CREATE INDEX IF NOT EXISTS idx_{t}_user_mills ON {t}(user_id, mills)"
        ));
        s.push(format!(
            "CREATE INDEX IF NOT EXISTS idx_{t}_user_srvmod ON {t}(user_id, srv_modified)"
        ));
    }
    s
}

fn doc_to_text(doc: &Value) -> Param {
    Param::Text(doc.to_string())
}

/// INSERT a document, transforming into an UPDATE when `(user_id, identifier)`
/// already exists (Nightscout v3 deduplication). `srv_created` is preserved on
/// update; everything else is replaced.
pub fn upsert_document(c: Collection, d: &StoredDoc) -> (String, Vec<Param>) {
    let t = c.table();
    let sql = format!(
        "INSERT INTO {t} ({DOC_COLS}) VALUES (?,?,?,?,?,?,?,?,?,?) \
         ON CONFLICT(user_id, identifier) DO UPDATE SET \
            mills=excluded.mills, doc_type=excluded.doc_type, srv_modified=excluded.srv_modified, \
            is_valid=excluded.is_valid, is_read_only=excluded.is_read_only, \
            subject=excluded.subject, doc=excluded.doc"
    );
    let params = vec![
        Param::text(&d.identifier),
        Param::text(&d.user_id),
        Param::Int(d.mills),
        Param::opt_text(d.doc_type.clone()),
        Param::Int(d.srv_created),
        Param::Int(d.srv_modified),
        Param::bool(d.is_valid),
        Param::bool(d.is_read_only),
        Param::opt_text(d.subject.clone()),
        doc_to_text(&d.doc),
    ];
    (sql, params)
}

/// Read one document by identifier, scoped to its owner.
pub fn get_document(c: Collection, user_id: &str, identifier: &str) -> (String, Vec<Param>) {
    let t = c.table();
    let sql = format!("SELECT {DOC_COLS} FROM {t} WHERE user_id=? AND identifier=?");
    (sql, vec![Param::text(user_id), Param::text(identifier)])
}

/// Search documents for a user with optional time/type filters and ordering.
pub fn search_documents(c: Collection, user_id: &str, q: &DocQuery) -> (String, Vec<Param>) {
    let t = c.table();
    let mut sql = format!("SELECT {DOC_COLS} FROM {t} WHERE user_id=?");
    let mut params = vec![Param::text(user_id)];
    if !q.include_invalid {
        sql.push_str(" AND is_valid=1");
    }
    if let Some(gte) = q.date_gte {
        sql.push_str(" AND mills>=?");
        params.push(Param::Int(gte));
    }
    if let Some(lte) = q.date_lte {
        sql.push_str(" AND mills<=?");
        params.push(Param::Int(lte));
    }
    if let Some(ty) = &q.doc_type {
        sql.push_str(" AND doc_type=?");
        params.push(Param::text(ty));
    }
    sql.push_str(if q.sort_desc {
        " ORDER BY mills DESC"
    } else {
        " ORDER BY mills ASC"
    });
    if let Some(limit) = q.limit {
        sql.push_str(" LIMIT ?");
        params.push(Param::Int(limit));
    }
    (sql, params)
}

/// Per-local-day reading counts for a collection, newest day first. Buckets on the
/// indexed `mills` column alone — `(mills + offset) / 86_400_000` is the local
/// day-number (integer division, identical in SQLite/D1 and Postgres for the positive
/// epoch values we store) — so it never reads a document body and stays cheap across
/// thousands of days. Columns are aliased (`day`, `n`, `first_ms`, `last_ms`) so the
/// D1 backend, which reads by name, finds them; the sqlx backend reads by position.
///
/// `GROUP BY`/`ORDER BY` use the output **ordinal** (`1`), not a repeat of the bucket
/// expression: the `?`→`$n` rewrite would turn two textually-identical expressions into
/// `$1` and `$4`, which Postgres treats as *different* expressions and then rejects
/// ("mills must appear in the GROUP BY clause"). The ordinal sidesteps that.
///
/// The offset is **inlined as an integer literal**, NOT a bound parameter, on purpose:
/// the D1 backend binds every integer param as a JS float, so `(mills + ?)` would make
/// SQLite do REAL arithmetic and `/ 86400000` REAL division — the bucket becomes a
/// fractional day and `GROUP BY` explodes to one group per reading (hundreds of thousands
/// of rows → the Worker OOMs). Inlining keeps the arithmetic integer on every backend.
/// `tz_offset_ms` is a server-computed, clamped `i64` (never user SQL), so interpolating
/// the number is safe.
pub fn daily_counts(
    c: Collection,
    user_id: &str,
    doc_type: &str,
    tz_offset_ms: i64,
) -> (String, Vec<Param>) {
    let t = c.table();
    let sql = format!(
        "SELECT (mills + {tz_offset_ms}) / 86400000 AS day, COUNT(*) AS n, \
                MIN(mills) AS first_ms, MAX(mills) AS last_ms \
         FROM {t} WHERE user_id=? AND is_valid=1 AND doc_type=? \
         GROUP BY 1 ORDER BY 1 DESC"
    );
    (sql, vec![Param::text(user_id), Param::text(doc_type)])
}

/// Soft-delete: flag `is_valid=0` so the document drops out of normal results but
/// still surfaces in history. Only affects a currently-valid row.
pub fn soft_delete_document(
    c: Collection,
    user_id: &str,
    identifier: &str,
    srv_modified: i64,
) -> (String, Vec<Param>) {
    let t = c.table();
    let sql = format!(
        "UPDATE {t} SET is_valid=0, srv_modified=? WHERE user_id=? AND identifier=? AND is_valid=1"
    );
    (
        sql,
        vec![
            Param::Int(srv_modified),
            Param::text(user_id),
            Param::text(identifier),
        ],
    )
}

/// The latest `srv_modified` for a user's collection (`NULL`/`None` if empty).
pub fn last_modified(c: Collection, user_id: &str) -> (String, Vec<Param>) {
    let t = c.table();
    (
        // Aliased so backends that read results by column name (D1) can find it.
        format!("SELECT MAX(srv_modified) AS lm FROM {t} WHERE user_id=?"),
        vec![Param::text(user_id)],
    )
}

/// Documents changed since a server timestamp (incremental sync / history). Includes
/// soft-deleted documents so clients learn about deletions.
pub fn history_since(
    c: Collection,
    user_id: &str,
    since_srv_modified: i64,
    limit: i64,
) -> (String, Vec<Param>) {
    let t = c.table();
    let sql = format!(
        "SELECT {DOC_COLS} FROM {t} WHERE user_id=? AND srv_modified>? ORDER BY srv_modified ASC LIMIT ?"
    );
    (
        sql,
        vec![
            Param::text(user_id),
            Param::Int(since_srv_modified),
            Param::Int(limit),
        ],
    )
}

/// Create or update a user, keyed on the unique `subject`. The row's `id` and
/// `created_at` are preserved across updates.
pub fn upsert_user(u: &User) -> (String, Vec<Param>) {
    let sql = format!(
        "INSERT INTO users ({USER_COLS}) VALUES (?,?,?,?,?,?) \
         ON CONFLICT(subject) DO UPDATE SET \
            display_name=excluded.display_name, is_admin=excluded.is_admin, \
            preferred_unit=excluded.preferred_unit"
    );
    let params = vec![
        Param::text(&u.id),
        Param::text(&u.subject),
        Param::opt_text(u.display_name.clone()),
        Param::bool(u.is_admin),
        Param::text(&u.preferred_unit),
        Param::Int(u.created_at),
    ];
    (sql, params)
}

pub fn get_user_by_subject(subject: &str) -> (String, Vec<Param>) {
    (
        format!("SELECT {USER_COLS} FROM users WHERE subject=?"),
        vec![Param::text(subject)],
    )
}

pub fn get_user_by_id(id: &str) -> (String, Vec<Param>) {
    (
        format!("SELECT {USER_COLS} FROM users WHERE id=?"),
        vec![Param::text(id)],
    )
}

/// Re-key a user in place: change only `subject`, leaving `id` (and therefore every
/// `user_id` reference) untouched. Guarded by `WHERE subject=?` so it is a safe no-op
/// once a row has already been migrated.
pub fn rekey_user_subject(old_subject: &str, new_subject: &str) -> (String, Vec<Param>) {
    (
        "UPDATE users SET subject=? WHERE subject=?".to_string(),
        vec![Param::text(new_subject), Param::text(old_subject)],
    )
}

pub fn insert_device_token(tok: &DeviceToken) -> (String, Vec<Param>) {
    let sql = format!("INSERT INTO device_tokens ({TOKEN_COLS}) VALUES (?,?,?,?,?,?,?,?,?)");
    let scopes = serde_json::to_string(&tok.scopes).unwrap_or_else(|_| "[]".to_string());
    let params = vec![
        Param::text(&tok.id),
        Param::text(&tok.user_id),
        Param::text(&tok.name),
        Param::text(&tok.token_hash),
        Param::Text(scopes),
        Param::Int(tok.created_at),
        Param::opt_int(tok.last_used_at),
        Param::bool(tok.revoked),
        Param::opt_text(tok.legacy_hash.clone()),
    ];
    (sql, params)
}

/// Look up a token by a presented hash, matching either the modern `token_hash`
/// (raw token → SHA-256) or the `legacy_hash` (SHA-1-hex → SHA-256). The same value
/// is compared against both columns.
pub fn get_device_token_by_hash(presented_hash: &str) -> (String, Vec<Param>) {
    (
        format!("SELECT {TOKEN_COLS} FROM device_tokens WHERE token_hash=? OR legacy_hash=?"),
        vec![Param::text(presented_hash), Param::text(presented_hash)],
    )
}

pub fn list_device_tokens(user_id: &str) -> (String, Vec<Param>) {
    (
        format!("SELECT {TOKEN_COLS} FROM device_tokens WHERE user_id=? ORDER BY created_at DESC"),
        vec![Param::text(user_id)],
    )
}

pub fn revoke_device_token(user_id: &str, id: &str) -> (String, Vec<Param>) {
    (
        "UPDATE device_tokens SET revoked=1 WHERE user_id=? AND id=? AND revoked=0".to_string(),
        vec![Param::text(user_id), Param::text(id)],
    )
}

pub fn touch_device_token(token_hash: &str, when_ms: i64) -> (String, Vec<Param>) {
    (
        "UPDATE device_tokens SET last_used_at=? WHERE token_hash=?".to_string(),
        vec![Param::Int(when_ms), Param::text(token_hash)],
    )
}

// ----- connector credentials --------------------------------------------------

/// Create or update a user's connector credential. On update, the sealed secret,
/// region, enabled flag and `updated_at` change; `created_at` and the last-sync
/// status are preserved.
pub fn upsert_connector_credential(c: &ConnectorCredential) -> (String, Vec<Param>) {
    let sql = format!(
        "INSERT INTO connector_credentials ({CRED_COLS}) VALUES (?,?,?,?,?,?,?,?,?) \
         ON CONFLICT(user_id, provider) DO UPDATE SET \
            enabled=excluded.enabled, secret_enc=excluded.secret_enc, \
            region=excluded.region, updated_at=excluded.updated_at"
    );
    let params = vec![
        Param::text(&c.user_id),
        Param::text(&c.provider),
        Param::bool(c.enabled),
        Param::text(&c.secret_enc),
        Param::opt_text(c.region.clone()),
        Param::Int(c.created_at),
        Param::Int(c.updated_at),
        Param::opt_int(c.last_sync_at),
        Param::opt_text(c.last_status.clone()),
    ];
    (sql, params)
}

pub fn get_connector_credential(user_id: &str, provider: &str) -> (String, Vec<Param>) {
    (
        format!("SELECT {CRED_COLS} FROM connector_credentials WHERE user_id=? AND provider=?"),
        vec![Param::text(user_id), Param::text(provider)],
    )
}

pub fn list_connector_credentials(user_id: &str) -> (String, Vec<Param>) {
    (
        format!("SELECT {CRED_COLS} FROM connector_credentials WHERE user_id=? ORDER BY provider"),
        vec![Param::text(user_id)],
    )
}

/// Every enabled credential across all users — the scheduler's work list.
pub fn list_enabled_connector_credentials() -> (String, Vec<Param>) {
    (
        format!("SELECT {CRED_COLS} FROM connector_credentials WHERE enabled=1"),
        vec![],
    )
}

pub fn delete_connector_credential(user_id: &str, provider: &str) -> (String, Vec<Param>) {
    (
        "DELETE FROM connector_credentials WHERE user_id=? AND provider=?".to_string(),
        vec![Param::text(user_id), Param::text(provider)],
    )
}

pub fn update_connector_sync(
    user_id: &str,
    provider: &str,
    last_sync_at: i64,
    last_status: &str,
) -> (String, Vec<Param>) {
    (
        "UPDATE connector_credentials SET last_sync_at=?, last_status=? WHERE user_id=? AND provider=?"
            .to_string(),
        vec![
            Param::Int(last_sync_at),
            Param::text(last_status),
            Param::text(user_id),
            Param::text(provider),
        ],
    )
}
