//! NightKnight's modern **v4** API — the default for first-party clients (web SPA,
//! iOS app).
//!
//! * `GET  /api/v4/status`     — service + the caller's user/unit.
//! * `GET  /api/v4/current`    — latest reading with trend, in both units.
//! * `GET  /api/v4/entries`    — recent readings (`?hours=` / `?count=`).
//! * `GET  /api/v4/analytics`  — Time-in-Range, GMI, eA1c, CV over a window.
//! * `GET/PUT /api/v4/me`      — the caller's profile (preferred unit, name).
//! * `POST/GET /api/v4/tokens`, `DELETE /api/v4/tokens/{id}` — device tokens.

use serde_json::{json, Value};
use uuid::Uuid;

use nightknight_auth::{Action, Permission, Scope};
use nightknight_core::analytics::{GlucoseReading, GlucoseSummary, TirThresholds};
use nightknight_core::documents::Entry;
use nightknight_core::timeutil;
use nightknight_core::trend::Direction;
use nightknight_core::units::GlucoseUnit;
use nightknight_storage::{Collection, ConnectorCredential, DeviceToken, DocQuery, StoredDoc, Storage};

use super::{ApiError, ApiRequest, ApiResponse, ApiService, EdgeIdentity, Principal};
use crate::hashing::{legacy_hash, token_hash};
use crate::http::Method;
use crate::{SERVICE_NAME, SERVICE_VERSION};

const DEFAULT_HOURS: i64 = 24;
const MAX_ANALYTICS_POINTS: i64 = 20_000;

impl<S: Storage> ApiService<S> {
    pub(crate) async fn route_v4(
        &self,
        req: &ApiRequest,
        now_ms: i64,
        edge: Option<EdgeIdentity>,
        tail: &[&str],
    ) -> Result<ApiResponse, ApiError> {
        let principal = self.resolve_principal(req, edge, now_ms).await?;
        match (req.method, tail) {
            (Method::Get, ["status"]) => self.v4_status(&principal),
            (Method::Get, ["current"]) => self.v4_current(&principal).await,
            (Method::Get, ["entries"]) => self.v4_entries(req, &principal, now_ms).await,
            (Method::Get, ["analytics"]) => self.v4_analytics(req, &principal, now_ms).await,
            (Method::Get, ["me"]) => self.v4_me(&principal),
            (Method::Put, ["me"]) => self.v4_update_me(req, &principal).await,
            (Method::Get, ["tokens"]) => self.v4_list_tokens(&principal).await,
            (Method::Post, ["tokens"]) => self.v4_create_token(req, &principal, now_ms).await,
            (Method::Delete, ["tokens", id]) => self.v4_revoke_token(&principal, id).await,
            (Method::Get, ["connectors"]) => self.v4_list_connectors(&principal).await,
            (Method::Put, ["connectors", provider]) => {
                self.v4_put_connector(req, &principal, provider, now_ms).await
            }
            (Method::Delete, ["connectors", provider]) => {
                self.v4_delete_connector(&principal, provider).await
            }
            _ => Err(ApiError::NotFound),
        }
    }

    fn v4_status(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        Ok(ApiResponse::json(
            200,
            &json!({
                "name": SERVICE_NAME,
                "version": SERVICE_VERSION,
                "user": user_json(principal),
            }),
        ))
    }

    /// Latest reading + trend, expressed in both units so the client can show either.
    async fn v4_current(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new().doc_type("sgv").limit(2),
            )
            .await?;
        let readings: Vec<GlucoseReading> = docs.iter().filter_map(reading_from_doc).collect();
        let Some(latest) = readings.first() else {
            return Ok(ApiResponse::json(200, &json!({ "current": Value::Null })));
        };
        // Newest is index 0, previous is index 1 (search is newest-first).
        let direction = match readings.get(1) {
            Some(prev) => Direction::between(
                (prev.date_ms, prev.value),
                (latest.date_ms, latest.value),
            ),
            None => Direction::None,
        };
        let g = latest.value;
        // The glucose **level** band (Urgent low … Urgent high) is a separate dimension
        // from the **trend** arrow; clients show both. Computed here so web/iOS/watch
        // share one source of truth and one vocabulary.
        let band = TirThresholds::default().band(g.mgdl());
        Ok(ApiResponse::json(
            200,
            &json!({
                "current": {
                    "date": latest.date_ms,
                    "dateString": timeutil::to_iso8601_ms(latest.date_ms),
                    "mgdl": g.mgdl_rounded(),
                    "mmol": g.display(GlucoseUnit::Mmol),
                    "direction": direction.name(),
                    "trend": direction.arrow(),
                    "trendLabel": direction.label(),
                    "level": band.key(),
                    "levelLabel": band.label(),
                    "preferredUnit": principal.user.preferred_unit,
                }
            }),
        ))
    }

    async fn v4_entries(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let (since, limit) = window(req, now_ms);
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new().doc_type("sgv").date_gte(since).limit(limit),
            )
            .await?;
        let points: Vec<Value> = docs
            .iter()
            .filter_map(|d| {
                reading_from_doc(d).map(|r| {
                    json!({
                        "date": r.date_ms,
                        "mgdl": r.value.mgdl_rounded(),
                        "mmol": r.value.display(GlucoseUnit::Mmol),
                    })
                })
            })
            .collect();
        Ok(ApiResponse::json(200, &json!({ "entries": points })))
    }

    /// Time-in-Range / GMI / eA1c / CV over a window (default 24h).
    async fn v4_analytics(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let hours = req.query_int("hours").unwrap_or(DEFAULT_HOURS).clamp(1, 24 * 90);
        let since = now_ms - hours * 3_600_000;
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new()
                    .doc_type("sgv")
                    .date_gte(since)
                    .limit(MAX_ANALYTICS_POINTS),
            )
            .await?;
        let readings: Vec<GlucoseReading> = docs.iter().filter_map(reading_from_doc).collect();
        let summary = GlucoseSummary::compute(&readings, &TirThresholds::default());
        Ok(ApiResponse::json(
            200,
            &json!({
                "hours": hours,
                "n": summary.n,
                "meanMgdl": summary.mean_mgdl,
                "gmiPercent": summary.gmi_percent,
                "estimatedA1cPercent": summary.estimated_a1c_percent,
                "cvPercent": summary.cv_percent,
                "timeInRange": {
                    "veryLowPct": summary.tir.very_low_pct,
                    "lowPct": summary.tir.low_pct,
                    "inRangePct": summary.tir.in_range_pct,
                    "highPct": summary.tir.high_pct,
                    "veryHighPct": summary.tir.very_high_pct,
                }
            }),
        ))
    }

    fn v4_me(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        Ok(ApiResponse::json(200, &user_json(principal)))
    }

    async fn v4_update_me(
        &self,
        req: &ApiRequest,
        principal: &Principal,
    ) -> Result<ApiResponse, ApiError> {
        // Mutating the profile is an owner/admin action — a read-only follower token
        // must not be able to change the account's unit or display name.
        principal.require(Permission::api("settings", Action::Admin))?;
        let body = req.body_json()?;
        let mut user = principal.user.clone();
        if let Some(unit) = body.get("preferredUnit").and_then(|v| v.as_str()) {
            let parsed = GlucoseUnit::parse(unit)
                .ok_or_else(|| ApiError::BadRequest(format!("unknown unit: {unit}")))?;
            user.preferred_unit = parsed.as_str().to_string();
        }
        if let Some(name) = body.get("displayName").and_then(|v| v.as_str()) {
            user.display_name = Some(name.to_string());
        }
        self.storage.upsert_user(&user).await?;
        let updated = Principal {
            user,
            scopes: super::ScopeSet::all(),
            subject: principal.subject.clone(),
        };
        Ok(ApiResponse::json(200, &user_json(&updated)))
    }

    async fn v4_list_tokens(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("tokens", Action::Admin))?;
        let tokens = self.storage.list_device_tokens(&principal.user.id).await?;
        let list: Vec<Value> = tokens.iter().map(token_json).collect();
        Ok(ApiResponse::json(200, &json!({ "tokens": list })))
    }

    /// Mint a device token. The raw secret is returned **once** and never stored.
    async fn v4_create_token(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("tokens", Action::Admin))?;
        let body = req.body_json().unwrap_or_else(|_| json!({}));
        let name = body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("device")
            .to_string();
        let scopes: Vec<String> = body
            .get("scopes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            // Default: a read-only follower token. Callers grant create scopes explicitly.
            .unwrap_or_else(|| vec!["api:entries:read".into(), "api:treatments:read".into()]);

        // No privilege escalation: a token may only be issued scopes the caller itself
        // holds. The owner holds `*:*:*` so this is a no-op for them, but it stops a
        // token that merely has `api:tokens:admin` from minting a broader one.
        for s in &scopes {
            let scope = Scope::parse(s)
                .ok_or_else(|| ApiError::BadRequest(format!("malformed scope '{s}'")))?;
            if !principal.scopes.covers(&scope) {
                return Err(ApiError::Forbidden(format!(
                    "cannot grant scope '{s}' beyond your own access"
                )));
            }
        }

        let raw = format!("nk_{}", Uuid::new_v4().simple());
        let token = DeviceToken {
            id: Uuid::new_v4().to_string(),
            user_id: principal.user.id.clone(),
            name,
            token_hash: token_hash(&raw),
            scopes,
            created_at: now_ms,
            last_used_at: None,
            revoked: false,
            legacy_hash: Some(legacy_hash(&raw)),
        };
        self.storage.insert_device_token(&token).await?;
        let mut out = token_json(&token);
        if let Value::Object(map) = &mut out {
            // Shown exactly once — the client must store it now.
            map.insert("token".into(), json!(raw));
        }
        Ok(ApiResponse::json(201, &out))
    }

    async fn v4_revoke_token(
        &self,
        principal: &Principal,
        id: &str,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("tokens", Action::Admin))?;
        let revoked = self.storage.revoke_device_token(&principal.user.id, id).await?;
        if revoked {
            Ok(ApiResponse::empty(204))
        } else {
            Err(ApiError::NotFound)
        }
    }
}

impl<S: Storage> ApiService<S> {
    /// List the caller's connectors (no secrets).
    async fn v4_list_connectors(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("connectors", Action::Admin))?;
        let creds = self.storage().list_connector_credentials(&principal.user.id).await?;
        let list: Vec<Value> = creds.iter().map(connector_json).collect();
        Ok(ApiResponse::json(200, &json!({ "connectors": list })))
    }

    /// Create/update a connector credential. The secret is encrypted at rest and
    /// never returned.
    async fn v4_put_connector(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        provider: &str,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("connectors", Action::Admin))?;
        let Some(key) = self.connector_key() else {
            return Err(ApiError::Forbidden("connectors are not enabled on this server".into()));
        };
        let body = req.body_json()?;
        let enabled = body.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

        // Build the provider-specific secret blob, validating required fields.
        let (secret, region) = match provider {
            "dexcom" => {
                let region = body
                    .get("region")
                    .and_then(|v| v.as_str())
                    .unwrap_or("us")
                    .to_string();
                let secret = json!({
                    "username": req_field(&body, "username")?,
                    "password": req_field(&body, "password")?,
                    "region": region,
                });
                (secret, Some(region))
            }
            "librelinkup" => {
                let secret = json!({
                    "email": req_field(&body, "email")?,
                    "password": req_field(&body, "password")?,
                });
                (secret, None)
            }
            other => return Err(ApiError::BadRequest(format!("unknown provider '{other}'"))),
        };

        let secret_enc = nightknight_crypto::encrypt_str(&key, &secret.to_string())
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let cred = ConnectorCredential {
            user_id: principal.user.id.clone(),
            provider: provider.to_string(),
            enabled,
            secret_enc,
            region,
            created_at: now_ms,
            updated_at: now_ms,
            last_sync_at: None,
            last_status: None,
        };
        self.storage().upsert_connector_credential(&cred).await?;
        Ok(ApiResponse::json(200, &connector_json(&cred)))
    }

    async fn v4_delete_connector(
        &self,
        principal: &Principal,
        provider: &str,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("connectors", Action::Admin))?;
        if self.storage().delete_connector_credential(&principal.user.id, provider).await? {
            Ok(ApiResponse::empty(204))
        } else {
            Err(ApiError::NotFound)
        }
    }
}

/// Safe (secret-free) view of a connector credential.
fn connector_json(c: &ConnectorCredential) -> Value {
    json!({
        "provider": c.provider,
        "enabled": c.enabled,
        "region": c.region,
        "updatedAt": c.updated_at,
        "lastSyncAt": c.last_sync_at,
        "lastStatus": c.last_status,
    })
}

/// Extract a required string field from a request body.
fn req_field(v: &Value, key: &str) -> Result<String, ApiError> {
    v.get(key)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::BadRequest(format!("missing '{key}'")))
}

/// Build a [`GlucoseReading`] from a stored entry document, if it carries a value.
fn reading_from_doc(d: &StoredDoc) -> Option<GlucoseReading> {
    let entry: Entry = serde_json::from_value(d.doc.clone()).ok()?;
    let value = entry.glucose_value().ok().flatten()?;
    Some(GlucoseReading::new(d.mills, value))
}

/// Resolve a `?hours=`/`?count=` window into `(since_ms, limit)`.
fn window(req: &ApiRequest, now_ms: i64) -> (i64, i64) {
    let hours = req.query_int("hours").unwrap_or(DEFAULT_HOURS).clamp(1, 24 * 90);
    let since = now_ms - hours * 3_600_000;
    let limit = req.query_int("count").unwrap_or(MAX_ANALYTICS_POINTS).clamp(1, MAX_ANALYTICS_POINTS);
    (since, limit)
}

fn user_json(principal: &Principal) -> Value {
    json!({
        "id": principal.user.id,
        "subject": principal.user.subject,
        "displayName": principal.user.display_name,
        "preferredUnit": principal.user.preferred_unit,
        "isAdmin": principal.user.is_admin,
    })
}

fn token_json(t: &DeviceToken) -> Value {
    json!({
        "id": t.id,
        "name": t.name,
        "scopes": t.scopes,
        "createdAt": t.created_at,
        "lastUsedAt": t.last_used_at,
        "revoked": t.revoked,
    })
}
