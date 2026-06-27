//! Nightscout **v1** compatibility router.
//!
//! Implements the endpoints existing uploaders and follower apps depend on:
//! `entries` (+ `/sgv`, `/current`), `treatments`, `devicestatus`, `profile`, and
//! `status`. Responses match the legacy shape (bare JSON arrays/objects) so xDrip+,
//! Loop, AndroidAPS and Trio interoperate without changes. The `.json` suffix on any
//! resource is accepted and ignored.

use serde_json::{json, Value};

use nightknight_auth::Action;
use nightknight_core::timeutil;
use nightknight_storage::{Collection, DocQuery, Storage};

use super::{enrich, perm, ApiError, ApiRequest, ApiResponse, ApiService, EdgeIdentity, Principal};
use crate::http::Method;
use crate::{SERVICE_NAME, SERVICE_VERSION};

/// Default number of records returned when the client gives no `count`.
const DEFAULT_COUNT: i64 = 10;
/// Hard cap on how many records a single query may return.
const MAX_COUNT: i64 = 131_072;

fn strip_json(seg: &str) -> &str {
    seg.strip_suffix(".json").unwrap_or(seg)
}

impl<S: Storage> ApiService<S> {
    pub(crate) async fn route_v1(
        &self,
        req: &ApiRequest,
        now_ms: i64,
        edge: Option<EdgeIdentity>,
        tail: &[&str],
    ) -> Result<ApiResponse, ApiError> {
        let principal = self.resolve_principal(req, edge, now_ms).await?;
        let parts: Vec<&str> = tail.iter().map(|s| strip_json(s)).collect();

        match (req.method, parts.as_slice()) {
            (Method::Get, ["status"]) => self.v1_status(&principal),

            (Method::Get, ["entries"]) => self.v1_list(req, &principal, Collection::Entries, None, None).await,
            (Method::Get, ["entries", "current"]) => {
                self.v1_list(req, &principal, Collection::Entries, Some("sgv"), Some(1)).await
            }
            (Method::Get, ["entries", "sgv"]) => {
                self.v1_list(req, &principal, Collection::Entries, Some("sgv"), None).await
            }
            (Method::Post, ["entries"]) => self.v1_post(req, &principal, Collection::Entries, now_ms).await,

            (Method::Get, ["treatments"]) => self.v1_list(req, &principal, Collection::Treatments, None, None).await,
            (Method::Post, ["treatments"]) => self.v1_post(req, &principal, Collection::Treatments, now_ms).await,

            (Method::Get, ["devicestatus"]) => self.v1_list(req, &principal, Collection::DeviceStatus, None, None).await,
            (Method::Post, ["devicestatus"]) => self.v1_post(req, &principal, Collection::DeviceStatus, now_ms).await,

            (Method::Get, ["profile"]) => self.v1_list(req, &principal, Collection::Profile, None, None).await,
            (Method::Post, ["profile"]) => self.v1_post(req, &principal, Collection::Profile, now_ms).await,

            _ => Err(ApiError::NotFound),
        }
    }

    /// GET a collection as a bare JSON array (legacy shape), newest first.
    async fn v1_list(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        collection: Collection,
        type_filter: Option<&str>,
        force_count: Option<i64>,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Read))?;

        let count = force_count
            .or_else(|| req.query_int("count"))
            .unwrap_or(DEFAULT_COUNT)
            .clamp(1, MAX_COUNT);
        let (gte, lte) = v1_date_filters(req);

        let mut q = DocQuery::new().limit(count);
        if let Some(g) = gte {
            q = q.date_gte(g);
        }
        if let Some(l) = lte {
            q = q.date_lte(l);
        }
        if let Some(t) = type_filter {
            q = q.doc_type(t);
        }

        let docs = self
            .storage
            .search_documents(collection, &principal.user.id, &q)
            .await?;
        let arr: Vec<Value> = docs.iter().map(|d| enrich(d, false)).collect();
        Ok(ApiResponse::json(200, &arr))
    }

    /// POST one document or an array of documents to a collection (legacy shape).
    async fn v1_post(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        collection: Collection,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Create))?;
        let body = req.body_json()?;
        let items = match body {
            Value::Array(items) => items,
            other => vec![other],
        };
        // Resilient, per-item ingest — matching the connector path ([`ingest_resilient`]):
        // a single invalid reading (out-of-range glucose, future/old timestamp, malformed
        // body) must NOT abort the batch and discard every good reading after it. A backfill
        // upload from xDrip+/Loop/Trio can carry the odd dirty row, and fail-fast here used
        // to silently truncate the whole import at the first bad record.
        let mut stored = Vec::with_capacity(items.len());
        let mut first_err: Option<ApiError> = None;
        for item in items {
            match self.store_document(collection, item, principal, now_ms).await {
                Ok(outcome) => stored.push(enrich(outcome.doc(), false)),
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        // If nothing stored, surface the error (a single bad doc still gets its 400, and an
        // all-bad batch reports why); otherwise return the readings that did land, in the
        // legacy bare-array shape clients expect.
        if stored.is_empty() {
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        Ok(ApiResponse::json(200, &stored))
    }

    /// The Nightscout `status` document — clients read `settings.units` from here.
    fn v1_status(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        let units = &principal.user.preferred_unit;
        let body = json!({
            "status": "ok",
            "name": SERVICE_NAME,
            "version": SERVICE_VERSION,
            "apiEnabled": true,
            "careportalEnabled": true,
            "settings": {
                "units": units,
                "thresholds": {
                    "bgHigh": 260,
                    "bgTargetTop": 180,
                    "bgTargetBottom": 70,
                    "bgLow": 55
                }
            },
            "authorized": { "read": true, "write": true }
        });
        Ok(ApiResponse::json(200, &body))
    }
}

/// Parse the Nightscout `find[date][$gte|$lte]` (epoch ms) and `find[dateString]`
/// (ISO) query filters into an epoch-ms window.
fn v1_date_filters(req: &ApiRequest) -> (Option<i64>, Option<i64>) {
    let mut gte = req.query_get("find[date][$gte]").and_then(|v| v.parse().ok());
    let mut lte = req.query_get("find[date][$lte]").and_then(|v| v.parse().ok());
    if gte.is_none() {
        gte = req
            .query_get("find[dateString][$gte]")
            .and_then(timeutil::parse_iso8601_ms);
    }
    if lte.is_none() {
        lte = req
            .query_get("find[dateString][$lte]")
            .and_then(timeutil::parse_iso8601_ms);
    }
    (gte, lte)
}
