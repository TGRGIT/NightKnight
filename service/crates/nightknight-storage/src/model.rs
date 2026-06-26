//! Storage-layer types shared by every backend.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The document collections NightKnight stores. Each maps to one SQL table with an
/// identical shape. The set mirrors Nightscout so the compat API can address them by
/// the same names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Collection {
    Entries,
    Treatments,
    DeviceStatus,
    Profile,
    Food,
    Settings,
}

impl Collection {
    /// The SQL table name. These are fixed identifiers (never user input), so it is
    /// safe to interpolate them into SQL.
    pub fn table(self) -> &'static str {
        match self {
            Collection::Entries => "entries",
            Collection::Treatments => "treatments",
            Collection::DeviceStatus => "devicestatus",
            Collection::Profile => "profile",
            Collection::Food => "food",
            Collection::Settings => "settings",
        }
    }

    /// Parse the collection name as it appears in an API path.
    pub fn from_path(s: &str) -> Option<Collection> {
        match s {
            "entries" => Some(Collection::Entries),
            "treatments" => Some(Collection::Treatments),
            "devicestatus" => Some(Collection::DeviceStatus),
            "profile" => Some(Collection::Profile),
            "food" => Some(Collection::Food),
            "settings" => Some(Collection::Settings),
            _ => None,
        }
    }

    /// Every collection — used by migrations and `lastModified` across collections.
    pub fn all() -> &'static [Collection] {
        &[
            Collection::Entries,
            Collection::Treatments,
            Collection::DeviceStatus,
            Collection::Profile,
            Collection::Food,
            Collection::Settings,
        ]
    }
}

/// A stored document: the full JSON body plus the indexed/metadata columns that make
/// querying, deduplication, history, and per-user isolation efficient. These columns
/// are the Nightscout v3 "common fields".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredDoc {
    /// Stable per-user identifier (the v3 `identifier`). Deduplication key.
    pub identifier: String,
    /// Owning user — every query is scoped by this for multi-user isolation.
    pub user_id: String,
    /// Primary time of the document in epoch ms (entry `date`, treatment time, …).
    pub mills: i64,
    /// Entry `type` (`sgv`/`mbg`/`cal`) where relevant; `None` for other collections.
    pub doc_type: Option<String>,
    /// Server insert time (epoch ms).
    pub srv_created: i64,
    /// Server last-modification time (epoch ms) — drives history/incremental sync.
    pub srv_modified: i64,
    /// `false` marks a soft-deleted document (still visible in history).
    pub is_valid: bool,
    /// `true` locks the document against further change.
    pub is_read_only: bool,
    /// The security subject that created/last-modified the document.
    pub subject: Option<String>,
    /// The full document body (all fields, including unknown ones).
    pub doc: Value,
}

/// Filter for [`crate::Storage::search_documents`]. Construct with [`DocQuery::new`]
/// and chain the setters; defaults to newest-first and hiding soft-deleted rows.
#[derive(Clone, Debug)]
pub struct DocQuery {
    pub date_gte: Option<i64>,
    pub date_lte: Option<i64>,
    pub doc_type: Option<String>,
    pub limit: Option<i64>,
    /// Include soft-deleted (`is_valid = false`) documents. Default `false`.
    pub include_invalid: bool,
    /// Sort by `mills` descending (newest first). Default `true`.
    pub sort_desc: bool,
}

impl Default for DocQuery {
    fn default() -> Self {
        DocQuery {
            date_gte: None,
            date_lte: None,
            doc_type: None,
            limit: None,
            include_invalid: false,
            sort_desc: true,
        }
    }
}

impl DocQuery {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn limit(mut self, n: i64) -> Self {
        self.limit = Some(n);
        self
    }
    pub fn date_gte(mut self, ms: i64) -> Self {
        self.date_gte = Some(ms);
        self
    }
    pub fn date_lte(mut self, ms: i64) -> Self {
        self.date_lte = Some(ms);
        self
    }
    pub fn doc_type(mut self, t: impl Into<String>) -> Self {
        self.doc_type = Some(t.into());
        self
    }
}

/// One local calendar day's worth of readings, as returned by the cheap
/// [`crate::Storage::daily_counts`] aggregation. Only the indexed `mills` column is
/// touched, so this scales to thousands of days without loading any document bodies —
/// it answers "which days have data, and how much" for the data-coverage view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayCount {
    /// Local day number — whole days since 1970-01-01 in the requested UTC offset
    /// (matches `nightknight_core::timeutil::day_number`).
    pub day_index: i64,
    /// Number of readings on that local day.
    pub n: i64,
    /// Epoch ms of the earliest reading that day.
    pub first_ms: i64,
    /// Epoch ms of the latest reading that day.
    pub last_ms: i64,
}

/// Whether an upsert created a new document or updated an existing one (the
/// Nightscout v3 "create-becomes-update on identifier match" semantics).
#[derive(Clone, Debug, PartialEq)]
pub enum WriteOutcome {
    Created(StoredDoc),
    Updated(StoredDoc),
}

impl WriteOutcome {
    pub fn doc(&self) -> &StoredDoc {
        match self {
            WriteOutcome::Created(d) | WriteOutcome::Updated(d) => d,
        }
    }
    pub fn created(&self) -> bool {
        matches!(self, WriteOutcome::Created(_))
    }
}

/// An application user. `subject` is the verified identity from the auth layer
/// (an email for a human, or a service-token common-name for a machine uploader).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub subject: String,
    pub display_name: Option<String>,
    pub is_admin: bool,
    /// Preferred display unit (`"mg/dl"` / `"mmol/l"`).
    pub preferred_unit: String,
    pub created_at: i64,
}

/// A per-device API token. The raw secret is never stored — only hashes, so a
/// database leak cannot be replayed against the API.
///
/// Two hashes are kept so both client styles work:
/// * `token_hash` = SHA-256 of the raw token — modern clients (our iOS app, v3/v4)
///   send the raw token via `Authorization: Bearer`.
/// * `legacy_hash` = SHA-256 of the *SHA-1 hex* of the raw token — legacy Nightscout
///   uploaders (xDrip+, …) SHA-1 the secret and send it in the `api-secret` header.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceToken {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub token_hash: String,
    /// Scope strings (`{api}:{collection}:{action}`) granted to this token.
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub revoked: bool,
    /// SHA-256 of the SHA-1 hex of the raw token, for legacy `api-secret` clients.
    pub legacy_hash: Option<String>,
}

/// A user's stored credentials for a CGM cloud connector (Dexcom Share /
/// LibreLinkUp). The secret (username/password/region) is held only as an encrypted
/// blob in `secret_enc`; the non-secret columns let the UI and scheduler list and
/// status connectors without decrypting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectorCredential {
    pub user_id: String,
    /// `"dexcom"` or `"librelinkup"`.
    pub provider: String,
    pub enabled: bool,
    /// AES-GCM-sealed JSON of the actual credentials. Never returned to clients.
    pub secret_enc: String,
    /// Non-secret hint (e.g. Dexcom region) for display.
    pub region: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_sync_at: Option<i64>,
    /// Short outcome of the most recent sync (`"ok: N readings"` / an error).
    pub last_status: Option<String>,
}

/// A bound query parameter. Backends bind these positionally against `?` placeholders.
/// Booleans are represented as `Int(0|1)`. NULLs are **typed** (`Null` = text, `IntNull`
/// = integer) because Postgres rejects a text-NULL bound to a bigint column (SQLite
/// does not — this distinction is what makes the two backends agree).
#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    Text(String),
    Int(i64),
    /// A NULL destined for a text column.
    Null,
    /// A NULL destined for an integer column.
    IntNull,
}

impl Param {
    pub fn text(s: impl Into<String>) -> Param {
        Param::Text(s.into())
    }
    pub fn bool(b: bool) -> Param {
        Param::Int(b as i64)
    }
    pub fn opt_text(s: Option<impl Into<String>>) -> Param {
        match s {
            Some(v) => Param::Text(v.into()),
            None => Param::Null,
        }
    }
    pub fn opt_int(n: Option<i64>) -> Param {
        match n {
            Some(v) => Param::Int(v),
            None => Param::IntNull,
        }
    }
}
