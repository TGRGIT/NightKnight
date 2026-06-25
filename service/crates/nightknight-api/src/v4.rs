//! NightKnight's modern **v4** API — the default for first-party clients (web SPA,
//! iOS app).
//!
//! * `GET  /api/v4/status`     — service + the caller's user/unit.
//! * `GET  /api/v4/current`    — latest reading with trend, in both units.
//! * `GET  /api/v4/entries`    — recent readings (`?hours=` / `?count=`).
//! * `GET  /api/v4/analytics`  — the full Statistical-Analysis set over a window:
//!   data sufficiency, Time-in-Range (count + time-weighted), GMI/eA1c, SD/CV, GRI,
//!   time-of-day patterns, episodes, and advanced variability.
//! * `GET  /api/v4/agp`        — Ambulatory Glucose Profile percentile bands.
//! * `GET/PUT /api/v4/me`      — the caller's profile (preferred unit, name).
//! * `POST/GET /api/v4/tokens`, `DELETE /api/v4/tokens/{id}` — device tokens.
//!
//! Window-based endpoints accept `?tzOffset=<minutes-east-of-UTC>` so time-of-day
//! analytics (AGP, dawn patterns, nocturnal flags) use the caller's local clock.

use serde_json::{json, Value};
use uuid::Uuid;

use nightknight_auth::{Action, Permission, Scope};
use nightknight_core::analytics::{
    self, Coverage, GlucoseEpisode, GlucoseReading, GlucoseSummary, GlycemiaRiskIndex, PeriodStats,
    TimeInRange, TirThresholds, DEFAULT_CADENCE_MS, DEFAULT_EPISODE_GAP_MS, DEFAULT_MAX_GAP_MS,
};
use nightknight_core::documents::Entry;
use nightknight_core::import::{parse_glucose_csv, DateOrder};
use nightknight_core::timeutil;
use nightknight_core::trend::{self, Direction};
use nightknight_core::units::GlucoseUnit;
use nightknight_storage::{Collection, ConnectorCredential, DeviceToken, DocQuery, Storage, StoredDoc};

use super::{ApiError, ApiRequest, ApiResponse, ApiService, EdgeIdentity, Principal};
use crate::hashing::{legacy_hash, token_hash};
use crate::http::Method;
use crate::{SERVICE_NAME, SERVICE_VERSION};

const DEFAULT_HOURS: i64 = 24;
const MAX_ANALYTICS_POINTS: i64 = 20_000;
/// How many recent readings `current` pulls to estimate a fallback trend — enough to
/// span the 15-minute regression window at 5-minute cadence with headroom.
const CURRENT_TREND_POINTS: i64 = 8;
/// If the latest reading is older than this, the trend is stale and we report no arrow
/// rather than a misleading one. The research staleness basis is ~2× the source's
/// cadence (≈10–11 min at the 5-minute Dexcom cadence); we allow a small grace for one
/// late sample. The reading *value* itself is always still returned. (Distinct from the
/// 15-minute regression window — this guards the latest sample's freshness, not span.)
const TREND_STALE_MS: i64 = 12 * 60_000;
/// Tolerance for matching a reading's lagged partner in CONGA / MODD — generous enough
/// to find a near-match across 5-min and 15-min cadences without matching a far-off time.
const LAG_TOLERANCE_MS: i64 = 10 * 60_000;
/// CONGA lag (hours) surfaced in the advanced-variability block.
const CONGA_HOURS: f64 = 2.0;
/// How many of the most recent episodes the analytics payload lists for the UI feed.
const RECENT_EPISODES: usize = 8;

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
            (Method::Get, ["current"]) => self.v4_current(&principal, now_ms).await,
            (Method::Get, ["entries"]) => self.v4_entries(req, &principal, now_ms).await,
            (Method::Get, ["analytics"]) => self.v4_analytics(req, &principal, now_ms).await,
            (Method::Get, ["agp"]) => self.v4_agp(req, &principal, now_ms).await,
            // `csv` auto-detects the exporter; `libreview` is kept as a back-compat alias.
            (Method::Post, ["import", "csv"] | ["import", "libreview"]) => {
                self.v4_import_csv(req, &principal, now_ms).await
            }
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
    ///
    /// Trend is taken from the **sensor's own** arrow when the latest entry carries one
    /// (Dexcom Share / LibreLinkUp report it, and the connectors persist it onto the
    /// entry's `direction`) — that figure comes from the transmitter's unfiltered,
    /// higher-cadence data and is authoritative. When absent (e.g. a manual entry or a
    /// source with no trend) we fall back to a least-squares estimate over the last
    /// 15 minutes, which is far steadier than a two-point delta.
    async fn v4_current(&self, principal: &Principal, now_ms: i64) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new().doc_type("sgv").limit(CURRENT_TREND_POINTS),
            )
            .await?;
        let readings: Vec<GlucoseReading> = docs.iter().filter_map(reading_from_doc).collect();
        let Some(latest) = readings.first().copied() else {
            return Ok(ApiResponse::json(200, &json!({ "current": Value::Null })));
        };
        // A stale latest reading gets no arrow — a two-hour-old "Steady" would mislead.
        let direction = if now_ms - latest.date_ms > TREND_STALE_MS {
            Direction::None
        } else {
            // Prefer the sensor's first-party arrow on the newest entry (docs are
            // newest-first); only fall back to our own estimate when it has none.
            match docs.first().and_then(direction_from_doc) {
                Some(d) if d.is_arrow() => d,
                _ => trend::classify_recent(&readings),
            }
        };
        let g = latest.value;
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

    /// The full Statistical-Analysis set over a window (default 24h). Backward
    /// compatible: every field the original payload carried is still present, with the
    /// deeper metrics added alongside. `?tzOffset=` (minutes east of UTC) localises the
    /// time-of-day analytics and the nocturnal-episode flag.
    async fn v4_analytics(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let hours = req.query_int("hours").unwrap_or(DEFAULT_HOURS).clamp(1, 24 * 90);
        let tz = tz_offset(req);
        let window_ms = hours * 3_600_000;
        let since = now_ms - window_ms;
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
        let t = TirThresholds::default();
        let summary = GlucoseSummary::compute(&readings, &t);

        // Data sufficiency, GRI, time-weighted TIR, and advanced variability.
        let coverage = Coverage::compute(&readings, window_ms, DEFAULT_CADENCE_MS, tz);
        // GRI 0 means "perfect glycemia", so an empty window must report null — not a
        // fabricated best-possible score — like every other metric here.
        let gri = (summary.n > 0).then(|| GlycemiaRiskIndex::from_tir(&summary.tir));
        let weighted = TimeInRange::compute_weighted(&readings, &t, DEFAULT_MAX_GAP_MS);
        let j_index = analytics::j_index(summary.mean_mgdl, summary.sd_mgdl);
        let mage = analytics::mage(&readings);
        let conga = analytics::conga(&readings, CONGA_HOURS, LAG_TOLERANCE_MS);
        let modd = analytics::modd(&readings, LAG_TOLERANCE_MS);
        let patterns = analytics::time_of_day_patterns(&readings, &t, tz);

        // Episodes: events/day are normalised over the days that actually carry data.
        let days = coverage.distinct_days.max(1) as f64;
        let gap = DEFAULT_EPISODE_GAP_MS;
        let lows = analytics::detect_episodes(&readings, t.low, true, tz, gap);
        let very_lows = analytics::detect_episodes(&readings, t.very_low, true, tz, gap);
        let highs = analytics::detect_episodes(&readings, t.high, false, tz, gap);
        let very_highs = analytics::detect_episodes(&readings, t.very_high, false, tz, gap);

        Ok(ApiResponse::json(
            200,
            &json!({
                "hours": hours,
                "tzOffset": tz,
                "n": summary.n,
                "meanMgdl": summary.mean_mgdl,
                "sdMgdl": summary.sd_mgdl,
                "gmiPercent": summary.gmi_percent,
                "estimatedA1cPercent": summary.estimated_a1c_percent,
                "cvPercent": summary.cv_percent,
                "coverage": coverage_json(&coverage),
                "timeInRange": tir_json(&summary.tir),
                "timeInRangeWeighted": weighted.as_ref().map(tir_json),
                "gri": {
                    "value": gri.map(|g| g.gri),
                    "zone": gri.map(|g| g.zone.label()),
                    "hypoComponent": gri.map(|g| g.hypo_component),
                    "hyperComponent": gri.map(|g| g.hyper_component),
                },
                "variability": {
                    "jIndex": j_index,
                    "mage": mage,
                    "congaHours": CONGA_HOURS,
                    "conga": conga,
                    "modd": modd,
                },
                "patterns": patterns.iter().map(period_json).collect::<Vec<_>>(),
                "episodes": {
                    "low": episode_summary_json(&lows, days),
                    "veryLow": episode_summary_json(&very_lows, days),
                    "high": episode_summary_json(&highs, days),
                    "veryHigh": episode_summary_json(&very_highs, days),
                    "recent": recent_episodes_json(&lows, &highs),
                },
            }),
        ))
    }

    /// Ambulatory Glucose Profile: the 5/25/50/75/95 percentile bands of glucose by
    /// time of day, every day in the window overlaid onto one 24-hour axis. `?days=`
    /// (default 14), `?bin=` minutes (default 15 → 96 bins), `?tzOffset=` minutes.
    async fn v4_agp(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Read))?;
        let days = req.query_int("days").unwrap_or(14).clamp(1, 90);
        let bin = req.query_int("bin").unwrap_or(15).clamp(5, 60);
        let tz = tz_offset(req);
        let since = now_ms - days * 24 * 3_600_000;
        let docs = self
            .storage
            .search_documents(
                Collection::Entries,
                &principal.user.id,
                &DocQuery::new().doc_type("sgv").date_gte(since).limit(MAX_ANALYTICS_POINTS),
            )
            .await?;
        let readings: Vec<GlucoseReading> = docs.iter().filter_map(reading_from_doc).collect();
        let bins: Vec<Value> = analytics::agp_bins(&readings, bin, tz)
            .iter()
            .map(|b| {
                json!({
                    "minuteOfDay": b.minute_of_day,
                    "n": b.n,
                    "p05": b.p05,
                    "p25": b.p25,
                    "p50": b.p50,
                    "p75": b.p75,
                    "p95": b.p95,
                })
            })
            .collect();
        Ok(ApiResponse::json(
            200,
            &json!({ "days": days, "binMinutes": bin, "tzOffset": tz, "n": readings.len(), "bins": bins }),
        ))
    }

    /// Import a glucose CSV export (the raw CSV is the request body) into the caller's
    /// own account. The format — LibreView or Dexcom Clarity — is auto-detected from the
    /// header. `?tzOffset=` (minutes east of UTC) anchors the export's local timestamps;
    /// `?dateOrder=mdy|dmy` overrides LibreView's day/month auto-detection (ignored for
    /// Dexcom, whose timestamps are unambiguous). Every parsed reading goes through the
    /// normal validated, content-deduped write path, so re-importing overlapping data
    /// updates rather than duplicates points.
    async fn v4_import_csv(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(Permission::api("entries", Action::Create))?;
        let tz = tz_offset(req);
        let order = match req.query_get("dateOrder") {
            Some(s) if s.eq_ignore_ascii_case("dmy") || s.eq_ignore_ascii_case("day") => {
                Some(DateOrder::DayFirst)
            }
            Some(s) if s.eq_ignore_ascii_case("mdy") || s.eq_ignore_ascii_case("month") => {
                Some(DateOrder::MonthFirst)
            }
            _ => None,
        };
        let text = std::str::from_utf8(&req.body)
            .map_err(|_| ApiError::BadRequest("CSV body must be UTF-8 text".into()))?;
        let parsed =
            parse_glucose_csv(text, tz, order).map_err(|e| ApiError::BadRequest(e.to_string()))?;

        // Ingest resiliently: a single implausible row (bad timestamp/glucose) is
        // counted and skipped, never aborting the whole import.
        let (mut imported, mut duplicates, mut rejected) = (0usize, 0usize, 0usize);
        for entry in parsed.entries {
            match self.store_document(Collection::Entries, entry, principal, now_ms).await {
                Ok(o) if o.created() => imported += 1,
                Ok(_) => duplicates += 1,
                Err(_) => rejected += 1,
            }
        }
        Ok(ApiResponse::json(
            200,
            &json!({
                "source": parsed.source,
                "unit": parsed.unit,
                "dateOrder": match parsed.order {
                    DateOrder::MonthFirst => "mdy",
                    DateOrder::DayFirst => "dmy",
                },
                "rows": parsed.rows,
                "parsed": parsed.imported,
                "skippedRows": parsed.skipped,
                "imported": imported,
                "duplicates": duplicates,
                "rejected": rejected,
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
            "nightscout" => {
                // Pull entries from another Nightscout/NightKnight instance. The URL is
                // user-supplied, so validate it (https, public host) before storing.
                let url = req_field(&body, "url")?;
                if !nightknight_connectors::nightscout::is_safe_base(&url) {
                    return Err(ApiError::BadRequest(
                        "nightscout url must be https to a public host".into(),
                    ));
                }
                let secret = json!({
                    "url": nightknight_connectors::nightscout::normalize_base(&url),
                    "secret": req_field(&body, "secret")?,
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

/// The sensor's first-party trend arrow stored on an entry, if it carries a recognised
/// `direction` (Dexcom Share / LibreLinkUp persist one; manual entries usually don't).
fn direction_from_doc(d: &StoredDoc) -> Option<Direction> {
    let entry: Entry = serde_json::from_value(d.doc.clone()).ok()?;
    entry.direction_parsed()
}

/// The caller's UTC offset in minutes (east of UTC), for localising time-of-day
/// analytics. Defaults to 0 (UTC) and is clamped to the real-world ±14h range.
fn tz_offset(req: &ApiRequest) -> i64 {
    req.query_int("tzOffset").unwrap_or(0).clamp(-14 * 60, 14 * 60)
}

/// Serialise a Time-in-Range distribution (shared by count- and time-weighted TIR).
fn tir_json(tir: &nightknight_core::analytics::TimeInRange) -> Value {
    json!({
        "veryLowPct": tir.very_low_pct,
        "lowPct": tir.low_pct,
        "inRangePct": tir.in_range_pct,
        "highPct": tir.high_pct,
        "veryHighPct": tir.very_high_pct,
    })
}

/// Serialise data-sufficiency coverage.
fn coverage_json(c: &Coverage) -> Value {
    json!({
        "n": c.n,
        "firstReading": c.first_ms,
        "lastReading": c.last_ms,
        "daysCovered": c.days_covered,
        "distinctDays": c.distinct_days,
        "percentActive": c.percent_active,
        "sufficient": c.sufficient,
    })
}

/// Serialise one time-of-day period's summary.
fn period_json(p: &PeriodStats) -> Value {
    json!({
        "startHour": p.start_hour,
        "endHour": p.end_hour,
        "n": p.summary.n,
        "meanMgdl": p.summary.mean_mgdl,
        "inRangePct": p.summary.tir.in_range_pct,
        "cvPercent": p.summary.cv_percent,
    })
}

/// Serialise the roll-up of a set of episodes for one threshold.
fn episode_summary_json(episodes: &[GlucoseEpisode], days: f64) -> Value {
    let s = analytics::EpisodeSummary::of(episodes, days);
    json!({
        "count": s.count,
        "nocturnal": s.nocturnal_count,
        "perDay": s.per_day,
        "longestMin": s.longest_min,
        "totalMin": s.total_min,
    })
}

/// The most recent episodes (lows + highs interleaved, newest first) for the UI feed.
fn recent_episodes_json(lows: &[GlucoseEpisode], highs: &[GlucoseEpisode]) -> Vec<Value> {
    let mut all: Vec<(&str, &GlucoseEpisode)> = lows
        .iter()
        .map(|e| ("low", e))
        .chain(highs.iter().map(|e| ("high", e)))
        .collect();
    all.sort_by(|a, b| b.1.start_ms.cmp(&a.1.start_ms));
    all.into_iter()
        .take(RECENT_EPISODES)
        .map(|(kind, e)| {
            json!({
                "kind": kind,
                "start": e.start_ms,
                "end": e.end_ms,
                "durationMin": e.duration_min,
                "extremeMgdl": e.extreme_mgdl,
                "nocturnal": e.nocturnal,
            })
        })
        .collect()
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
