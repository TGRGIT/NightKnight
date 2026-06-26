//! # nightknight-api
//!
//! The transport-agnostic HTTP API for NightKnight. It speaks three dialects over
//! one shared core:
//!
//! * **v1** — legacy Nightscout (`/api/v1/entries`, `/treatments`, `/status`, …) so
//!   existing uploaders (xDrip+, Loop, AndroidAPS, Trio) work unchanged.
//! * **v3** — Nightscout's generic CRUD (`/api/v3/{collection}` + history) with the
//!   `{ status, result }` envelope.
//! * **v4** — NightKnight's own modern API: current reading + trend, analytics
//!   (time-in-range / GMI), per-user settings, and device-token management.
//!
//! [`ApiService::handle`] is the single entry point. It is generic over [`Storage`]
//! and takes the current time plus an optional [`EdgeIdentity`] (the user the
//! runtime already verified at the Cloudflare Access / OIDC edge), so it stays pure
//! and testable. **Credentials are read from headers only — never the query string.**

mod error;
mod hashing;
mod http;
mod identifier;
mod v1;
mod v3;
mod v4;

pub use error::ApiError;
pub use http::{ApiRequest, ApiResponse, Headers, Method};
pub use nightknight_auth::PrincipalKind;

use serde_json::{json, Value};
use uuid::Uuid;

use nightknight_auth::{extract_bearer, Action, Permission, ScopeSet};
use nightknight_connectors::dexcom::{DexcomConnector, Region};
use nightknight_connectors::librelinkup::LibreLinkUpConnector;
use nightknight_connectors::nightscout::NightscoutConnector;
use nightknight_connectors::{Connector, Http};
use nightknight_core::documents::{Entry, Treatment};
use nightknight_crypto as crypto;
use nightknight_storage::{Collection, ConnectorCredential, StoredDoc, Storage, User, WriteOutcome};

use identifier::{derive_identifier, extract_doc_type, extract_mills};

/// Service name and version surfaced in `status` responses.
pub const SERVICE_NAME: &str = "nightknight";
pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Maximum accepted request-body size. Generous enough for large bulk uploads /
/// history backfills, but bounds the memory a single request can pin. Anything larger
/// is rejected with 413 before parsing.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Ceiling (minutes) on any single Nightscout pull, and the window the first sync uses to
/// bulk-import on the spot — ~7 days (≈2k readings at 5-min cadence). Far better than the
/// 12-reading drip, while staying within one Worker invocation's CPU/subrequest budget: it
/// matches the window the hourly cron already runs in production, so the unbatched per-row
/// ingest is known to fit. (Pulling 30–365 days here would risk a 10k–100k-row ingest that
/// can't complete in one invocation and then re-loops; deeper migration needs a future
/// chunked/cursored ingest — see docs.) Every Nightscout pull is clamped to this.
const NS_BACKFILL_MINUTES: i64 = 7 * 24 * 60;

/// The identity the runtime verified at the edge (Cloudflare Access JWT or OIDC),
/// before the request reached the API. `None` means no edge identity (e.g. a pure
/// device-token request, or an unauthenticated request that will be rejected).
#[derive(Clone, Debug)]
pub struct EdgeIdentity {
    pub subject: String,
    pub kind: PrincipalKind,
    pub display_name: Option<String>,
    /// The verified email, when the edge provided one. Display + (one-time) legacy
    /// subject migration only — it is NOT the tenancy key (see [`tenant_subject`]).
    pub email: Option<String>,
    /// Group memberships the runtime resolved for a human (from the Access
    /// `get-identity` endpoint, or a trusted proxy header). Empty for service tokens.
    pub groups: Vec<String>,
}

impl EdgeIdentity {
    /// The namespaced tenancy key used to key the app's user. Humans and machines live
    /// in **separate** namespaces (`human:` vs `service:`), so a service-token
    /// `common_name` (or an OIDC `sub`) can never collide with a human's email/sub and
    /// resolve to that human's account. The kind comes from the verified token, not
    /// from anything the client controls.
    fn tenant_subject(&self) -> String {
        let prefix = match self.kind {
            PrincipalKind::Human => "human",
            PrincipalKind::Service => "service",
        };
        format!("{prefix}:{}", self.subject.trim())
    }

    /// Pre-namespacing subjects a human row might have been stored under (the bare
    /// email, as the old code keyed it), for the one-time legacy migration. Empty for
    /// machines (service tokens were never email-keyed).
    fn legacy_subject_candidates(&self) -> Vec<String> {
        if !matches!(self.kind, PrincipalKind::Human) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for raw in [self.email.as_deref(), Some(self.subject.as_str())].into_iter().flatten() {
            let e = raw.trim();
            if e.is_empty() {
                continue;
            }
            for cand in [e.to_string(), e.to_ascii_lowercase()] {
                if !out.contains(&cand) {
                    out.push(cand);
                }
            }
        }
        out
    }
}

/// Default authority for a machine (service-token) principal: read everything and write
/// CGM data, but NOT manage device tokens / connectors or change settings — those are
/// owner-only. Deliberately narrower than a human owner's all-access (`*:*:*`).
fn service_scopes() -> ScopeSet {
    ScopeSet::parse_all([
        "api:*:read",
        "api:entries:create",
        "api:treatments:create",
        "api:devicestatus:create",
    ])
}

/// The authenticated principal for a request: which user, and what they may do.
pub struct Principal {
    pub user: User,
    pub scopes: ScopeSet,
    pub subject: String,
}

impl Principal {
    /// Authorize an operation, or fail with 403.
    fn require(&self, perm: Permission) -> Result<(), ApiError> {
        if self.scopes.grants(&perm) {
            Ok(())
        } else {
            Err(ApiError::Forbidden(format!(
                "missing scope {}:{}:{}",
                perm.api, perm.collection, perm.action
            )))
        }
    }
}

/// The HTTP API over a [`Storage`] backend.
pub struct ApiService<S: Storage> {
    storage: S,
    /// If set, a human (OIDC) principal must have this group in [`EdgeIdentity::groups`]
    /// or they are refused — the app enforces the group requirement itself, in
    /// addition to the Cloudflare Access edge policy (defence in depth). Service
    /// tokens and device tokens (the "API key" path) are not group-gated.
    required_group: Option<String>,
    /// AES-256 key sealing connector credentials at rest. `None` disables the
    /// connector endpoints (they return 503).
    connector_key: Option<[u8; 32]>,
    /// One-time migration: when on, a human whose namespaced subject is not yet stored
    /// is matched against the pre-namespacing (bare-email) key and that row is re-keyed
    /// in place. Default off; enable only for the migration window (and only where the
    /// edge's email is trustworthy — e.g. behind Cloudflare Access).
    migrate_legacy_subjects: bool,
}

impl<S: Storage> ApiService<S> {
    pub fn new(storage: S) -> Self {
        ApiService {
            storage,
            required_group: None,
            connector_key: None,
            migrate_legacy_subjects: false,
        }
    }

    /// Require human (OIDC) principals to belong to `group`. `None` disables the
    /// in-app group check (the edge gate still applies).
    pub fn require_group(mut self, group: Option<String>) -> Self {
        self.required_group = group.filter(|g| !g.is_empty());
        self
    }

    /// Enable the one-time legacy-subject migration (see [`migrate_legacy_subjects`]).
    pub fn migrate_legacy_subjects(mut self, on: bool) -> Self {
        self.migrate_legacy_subjects = on;
        self
    }

    /// Set the key used to encrypt connector credentials. Without it, connector
    /// endpoints and the sync scheduler are disabled.
    pub fn with_connector_key(mut self, key: Option<[u8; 32]>) -> Self {
        self.connector_key = key;
        self
    }

    pub(crate) fn connector_key(&self) -> Option<[u8; 32]> {
        self.connector_key
    }

    /// Borrow the storage (used by the runtime for migrations / health checks).
    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// Handle a request, always producing a response (errors become JSON bodies).
    pub async fn handle(
        &self,
        req: ApiRequest,
        now_ms: i64,
        edge: Option<EdgeIdentity>,
    ) -> ApiResponse {
        // Bound memory use: reject oversized bodies before any parsing/routing.
        if req.body.len() > MAX_BODY_BYTES {
            return ApiError::PayloadTooLarge.into_response();
        }
        match self.route(req, now_ms, edge).await {
            Ok(resp) => resp,
            Err(e) => e.into_response(),
        }
    }

    async fn route(
        &self,
        req: ApiRequest,
        now_ms: i64,
        edge: Option<EdgeIdentity>,
    ) -> Result<ApiResponse, ApiError> {
        let segs: Vec<String> = req
            .path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        let tail: Vec<&str> = segs.iter().skip(2).map(|s| s.as_str()).collect();

        match (segs.first().map(String::as_str), segs.get(1).map(String::as_str)) {
            (Some("api"), Some("v1")) => self.route_v1(&req, now_ms, edge, &tail).await,
            (Some("api"), Some("v3")) => self.route_v3(&req, now_ms, edge, &tail).await,
            (Some("api"), Some("v4")) => self.route_v4(&req, now_ms, edge, &tail).await,
            _ => Err(ApiError::NotFound),
        }
    }

    /// Resolve the principal for a request: a presented device token wins; otherwise
    /// the edge identity is mapped to (or creates) a user with full access to their
    /// own data. **Tokens are read from headers only.**
    async fn resolve_principal(
        &self,
        req: &ApiRequest,
        edge: Option<EdgeIdentity>,
        now_ms: i64,
    ) -> Result<Principal, ApiError> {
        // 1. A device token presented via Authorization: Bearer or the api-secret header.
        if let Some(secret) = token_secret_from_headers(req) {
            let lookup = hashing::lookup_hash(secret);
            if let Some(tok) = self.storage.get_device_token_by_hash(&lookup).await? {
                if tok.revoked {
                    return Err(ApiError::Unauthorized);
                }
                let user = self
                    .storage
                    .get_user_by_id(&tok.user_id)
                    .await?
                    .ok_or(ApiError::Unauthorized)?;
                // Best-effort last-used bookkeeping; ignore failures.
                let _ = self.storage.touch_device_token(&lookup, now_ms).await;
                let subject = user.subject.clone();
                return Ok(Principal {
                    user,
                    scopes: ScopeSet::parse_all(tok.scopes),
                    subject,
                });
            }
            // A credential was presented but is invalid — do not fall through.
            return Err(ApiError::Unauthorized);
        }

        // 2. Edge identity (human via passkey/OTP, or a bare service token).
        if let Some(edge) = edge {
            // Defence in depth: enforce the group requirement in the app itself, not
            // only at the Cloudflare Access edge. Humans must be in the required
            // group; service tokens are the machine/"API key" path and are exempt.
            if matches!(edge.kind, PrincipalKind::Human) {
                if let Some(required) = &self.required_group {
                    if !edge.groups.iter().any(|g| g == required) {
                        return Err(ApiError::Forbidden(format!(
                            "not a member of required group '{required}'"
                        )));
                    }
                }
            }
            // A human (passkey/OTP) owns their data → all-access. A bare service token
            // is the machine/API-key path → least-privilege (read + CGM-data writes),
            // never token/connector/settings administration.
            let scopes = match edge.kind {
                PrincipalKind::Human => ScopeSet::all(),
                PrincipalKind::Service => service_scopes(),
            };
            let user = self.get_or_create_user(&edge, now_ms).await?;
            return Ok(Principal {
                subject: user.subject.clone(),
                user,
                scopes,
            });
        }

        Err(ApiError::Unauthorized)
    }

    /// Find the user for an edge identity, creating one on first sight. New users are
    /// created non-admin; `is_admin` is reserved for future use and is never granted
    /// automatically (there is no privileged bootstrap user).
    async fn get_or_create_user(
        &self,
        edge: &EdgeIdentity,
        now_ms: i64,
    ) -> Result<User, ApiError> {
        // Reject an empty/blank identity outright — never key a user on "" (which would
        // otherwise become a shared bucket). Users are keyed by the namespaced subject.
        if edge.subject.trim().is_empty() {
            return Err(ApiError::Unauthorized);
        }
        let subject = edge.tenant_subject();
        if let Some(u) = self.storage.get_user_by_subject(&subject).await? {
            return Ok(u);
        }
        // One-time migration: adopt a pre-namespacing row (keyed by the bare email) by
        // re-keying it in place to the namespaced subject. No data moves — every row
        // references the user by its unchanged `id`. Bounded: only legacy (un-prefixed)
        // rows match, and once re-keyed the legacy key no longer exists.
        if self.migrate_legacy_subjects {
            for legacy in edge.legacy_subject_candidates() {
                if legacy == subject {
                    continue;
                }
                if self.storage.get_user_by_subject(&legacy).await?.is_some()
                    && self.storage.rekey_user_subject(&legacy, &subject).await?
                {
                    if let Some(u) = self.storage.get_user_by_subject(&subject).await? {
                        return Ok(u);
                    }
                }
            }
        }
        let user = User {
            id: Uuid::new_v4().to_string(),
            subject: subject.clone(),
            display_name: edge.display_name.clone(),
            is_admin: false,
            preferred_unit: "mg/dl".to_string(),
            created_at: now_ms,
        };
        self.storage.upsert_user(&user).await?;
        // Re-read to get the canonical row (handles a race where another request
        // created the same subject concurrently).
        self.storage
            .get_user_by_subject(&subject)
            .await?
            .ok_or_else(|| ApiError::Internal("user vanished after create".into()))
    }

    /// Ingest connector readings for a user (resolved/created by `subject`). Each
    /// entry is validated and stored with dedup, so re-fetched overlaps don't
    /// duplicate. Returns how many were stored. Used by the connector scheduler.
    pub async fn ingest_entries(
        &self,
        subject: &str,
        entries: Vec<Value>,
        now_ms: i64,
    ) -> Result<usize, ApiError> {
        let edge = EdgeIdentity {
            subject: subject.to_string(),
            kind: PrincipalKind::Service,
            display_name: None,
            email: None,
            groups: Vec::new(),
        };
        let user = self.get_or_create_user(&edge, now_ms).await?;
        let principal = Principal {
            subject: user.subject.clone(),
            user,
            scopes: ScopeSet::all(),
        };
        Ok(self.ingest_resilient(&principal, entries, now_ms).await)
    }

    /// Ingest entries for a known user id (the connector scheduler path). Unlike
    /// [`ingest_entries`](Self::ingest_entries), this never creates a user — the user
    /// must already exist (they entered the credentials).
    pub async fn ingest_for_user_id(
        &self,
        user_id: &str,
        entries: Vec<Value>,
        now_ms: i64,
    ) -> Result<usize, ApiError> {
        let user = self
            .storage
            .get_user_by_id(user_id)
            .await?
            .ok_or(ApiError::NotFound)?;
        let principal = Principal {
            subject: user.subject.clone(),
            user,
            scopes: ScopeSet::all(),
        };
        Ok(self.ingest_resilient(&principal, entries, now_ms).await)
    }

    /// Store a batch of connector readings, returning how many were written. A single
    /// implausible / future-dated / malformed reading is **skipped**, never aborting the
    /// whole batch — matching the resilient CSV-import path. This matters because a
    /// Nightscout source is user-controlled and can carry historically-dirty data (e.g. a
    /// stray out-of-range or future-dated reading), and `parse_entries`' `sgv > 0` filter
    /// is looser than `Entry::validate` (10–1000 mg/dL, timestamp ≤ now+24h); without this
    /// one bad row in a backfill would discard the entire pull and keep the sync erroring.
    async fn ingest_resilient(
        &self,
        principal: &Principal,
        entries: Vec<Value>,
        now_ms: i64,
    ) -> usize {
        let mut stored = 0usize;
        for entry in entries {
            if self
                .store_document(Collection::Entries, entry, principal, now_ms)
                .await
                .is_ok()
            {
                stored += 1;
            }
        }
        stored
    }

    /// Run every enabled connector once: fetch `minutes` of history, ingest it, and
    /// record per-credential sync status. `http` is the runtime's transport. A no-op
    /// (returns 0) if no connector key is configured.
    pub async fn sync_connectors(
        &self,
        http: Http<'_>,
        minutes: i64,
        now_ms: i64,
    ) -> Result<usize, ApiError> {
        let Some(key) = self.connector_key() else {
            return Ok(0);
        };
        let mut total = 0usize;
        for cred in self.storage.list_enabled_connector_credentials().await? {
            let status = match self.sync_one(&cred, &key, http, minutes, now_ms).await {
                Ok(n) => {
                    total += n;
                    format!("ok: {n} readings")
                }
                Err(e) => format!("error: {e}"),
            };
            let _ = self
                .storage
                .update_connector_sync(&cred.user_id, &cred.provider, now_ms, &status)
                .await;
        }
        Ok(total)
    }

    async fn sync_one(
        &self,
        cred: &ConnectorCredential,
        key: &[u8; 32],
        http: Http<'_>,
        minutes: i64,
        now_ms: i64,
    ) -> Result<usize, ApiError> {
        let json = crypto::decrypt_str(key, &cred.secret_enc)
            .map_err(|e| ApiError::Internal(format!("decrypt: {e}")))?;
        let creds: Value =
            serde_json::from_str(&json).map_err(|e| ApiError::Internal(e.to_string()))?;
        let samples = match cred.provider.as_str() {
            "dexcom" => {
                let c = DexcomConnector {
                    region: Region::parse(
                        creds.get("region").and_then(|v| v.as_str()).unwrap_or("us"),
                    ),
                    username: cred_field(&creds, "username")?,
                    password: cred_field(&creds, "password")?,
                };
                c.fetch_recent(http, minutes).await
            }
            "librelinkup" => {
                let c = LibreLinkUpConnector {
                    email: cred_field(&creds, "email")?,
                    password: cred_field(&creds, "password")?,
                };
                c.fetch_recent(http, minutes).await
            }
            "nightscout" => {
                let c = NightscoutConnector {
                    base_url: cred_field(&creds, "url")?,
                    secret: cred_field(&creds, "secret")?,
                };
                // A Nightscout source is migrated history, not a live vendor feed: until a
                // sync has SUCCEEDED, pull in BULK on the very first tick (within ~60s of
                // being added) instead of dripping the recent window 12 readings at a time.
                // "Not yet succeeded" = never synced OR the last attempt errored, so a
                // transient first-run failure retries (`last_sync_at` is stamped even on
                // error). EVERY Nightscout pull is capped to `NS_BACKFILL_MINUTES` — unlike
                // the vendor connectors (which hit hard-coded hosts that clamp recent
                // windows), Nightscout honours huge `count`s, so the daily "all" cron would
                // otherwise ask for ~365 days = 100k+ readings and try to ingest them in one
                // Worker invocation (blowing the CPU/subrequest budget; a kill mid-ingest
                // then never records success and re-loops). Capping to a window the hourly
                // cron already handles keeps every pull within one invocation. Ingest is
                // resilient (`ingest_resilient`), so a dirty historical row can't abort or
                // re-loop the sync. Deeper history needs a future chunked/cursored ingest.
                let needs_backfill = cred.last_sync_at.is_none()
                    || cred.last_status.as_deref().is_some_and(|s| s.starts_with("error"));
                let window = if needs_backfill { NS_BACKFILL_MINUTES } else { minutes };
                c.fetch_recent(http, window.min(NS_BACKFILL_MINUTES)).await
            }
            other => return Err(ApiError::BadRequest(format!("unknown provider {other}"))),
        }
        .map_err(|e| ApiError::Internal(e.to_string()))?;

        let entries: Vec<Value> = samples.iter().map(|s| s.to_entry_json()).collect();
        self.ingest_for_user_id(&cred.user_id, entries, now_ms).await
    }

    /// Validate, derive metadata for, and upsert one document. Returns the write
    /// outcome (created vs deduplicated-update).
    async fn store_document(
        &self,
        c: Collection,
        value: Value,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<WriteOutcome, ApiError> {
        validate_document(c, &value, now_ms)?;
        let identifier = derive_identifier(c, &value);
        let mills = extract_mills(&value, now_ms);
        let doc_type = extract_doc_type(c, &value);
        let doc = StoredDoc {
            identifier,
            user_id: principal.user.id.clone(),
            mills,
            doc_type,
            srv_created: now_ms,
            srv_modified: now_ms,
            is_valid: true,
            is_read_only: false,
            subject: Some(principal.subject.clone()),
            doc: value,
        };
        Ok(self.storage.upsert_document(c, doc).await?)
    }
}

/// Pull a credential from the headers ONLY. Order: `Authorization: Bearer`, then the
/// Nightscout `api-secret` header. The query string is never consulted.
fn token_secret_from_headers(req: &ApiRequest) -> Option<&str> {
    if let Some(bearer) = extract_bearer(req.headers.get("authorization")) {
        return Some(bearer);
    }
    req.headers
        .get("api-secret")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Validate a document body for a collection using the core clinical rules.
fn validate_document(c: Collection, value: &Value, now_ms: i64) -> Result<(), ApiError> {
    match c {
        Collection::Entries => {
            let entry: Entry = serde_json::from_value(value.clone())
                .map_err(|e| ApiError::BadRequest(format!("invalid entry: {e}")))?;
            entry.validate(now_ms)?;
        }
        Collection::Treatments => {
            let t: Treatment = serde_json::from_value(value.clone())
                .map_err(|e| ApiError::BadRequest(format!("invalid treatment: {e}")))?;
            t.validate()?;
        }
        // devicestatus/profile/food/settings are free-form; require an object.
        _ => {
            if !value.is_object() {
                return Err(ApiError::BadRequest("document must be a JSON object".into()));
            }
        }
    }
    Ok(())
}

/// Add the read-side metadata clients expect to a stored document body. `v3` adds the
/// full v3 common fields; otherwise just the Mongo-style `_id` (Nightscout v1).
fn enrich(doc: &StoredDoc, v3: bool) -> Value {
    let mut out = doc.doc.clone();
    if let Value::Object(map) = &mut out {
        map.insert("_id".into(), json!(doc.identifier));
        map.entry("mills").or_insert(json!(doc.mills));
        if v3 {
            map.insert("identifier".into(), json!(doc.identifier));
            map.insert("srvCreated".into(), json!(doc.srv_created));
            map.insert("srvModified".into(), json!(doc.srv_modified));
            map.insert("isValid".into(), json!(doc.is_valid));
            map.insert("subject".into(), json!(doc.subject));
        }
    }
    out
}

/// The permission required for a CRUD action on a collection.
fn perm(collection: Collection, action: Action) -> Permission {
    Permission::api(collection.table(), action)
}

/// Extract a required string field from a decrypted credential blob.
fn cred_field(v: &Value, key: &str) -> Result<String, ApiError> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .ok_or_else(|| ApiError::Internal(format!("credential missing '{key}'")))
}
