//! Nightscout **v3** generic-CRUD router.
//!
//! One uniform interface over every collection, with the `{ status, result }`
//! envelope, content-derived deduplication, soft-delete, incremental `history`, and
//! `lastModified`. `version` is unauthenticated; everything else requires a principal
//! and the matching `{api}:{collection}:{action}` scope.

use serde_json::{json, Value};

use nightknight_auth::Action;
use nightknight_storage::{Collection, DocQuery, Storage};

use super::{enrich, perm, ApiError, ApiRequest, ApiResponse, ApiService, EdgeIdentity, Principal};
use crate::http::Method;
use crate::{SERVICE_NAME, SERVICE_VERSION};

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 131_072;
const DEFAULT_HISTORY_LIMIT: i64 = 10_000;

/// Wrap a result in the Nightscout v3 `{ status, result }` envelope.
fn env(status: u16, result: Value) -> ApiResponse {
    ApiResponse::json(status, &json!({ "status": status, "result": result }))
}

impl<S: Storage> ApiService<S> {
    pub(crate) async fn route_v3(
        &self,
        req: &ApiRequest,
        now_ms: i64,
        edge: Option<EdgeIdentity>,
        tail: &[&str],
    ) -> Result<ApiResponse, ApiError> {
        // `version` requires no authentication.
        if req.method == Method::Get && tail == ["version"] {
            return Ok(env(
                200,
                json!({ "version": SERVICE_VERSION, "apiVersion": "3.0", "name": SERVICE_NAME }),
            ));
        }

        let principal = self.resolve_principal(req, edge, now_ms).await?;

        match (req.method, tail) {
            (Method::Get, ["status"]) => self.v3_status(&principal),
            (Method::Get, ["lastModified"]) => self.v3_last_modified(&principal).await,

            (Method::Get, [coll]) => self.v3_search(req, &principal, collection(coll)?).await,
            (Method::Post, [coll]) => {
                self.v3_create(req, &principal, collection(coll)?, now_ms).await
            }

            (Method::Get, [coll, "history"]) => {
                self.v3_history(&principal, collection(coll)?, 0).await
            }
            (Method::Get, [coll, "history", since]) => {
                let since_ms = since
                    .parse::<i64>()
                    .map_err(|_| ApiError::BadRequest("history timestamp must be epoch ms".into()))?;
                self.v3_history(&principal, collection(coll)?, since_ms).await
            }

            (Method::Get, [coll, id]) => self.v3_read(&principal, collection(coll)?, id).await,
            (Method::Delete, [coll, id]) => {
                self.v3_delete(&principal, collection(coll)?, id, now_ms).await
            }
            (Method::Put, [coll, id]) => {
                self.v3_put(req, &principal, collection(coll)?, id, now_ms).await
            }
            (Method::Patch, [coll, id]) => {
                self.v3_patch(req, &principal, collection(coll)?, id, now_ms).await
            }

            _ => Err(ApiError::NotFound),
        }
    }

    async fn v3_search(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        collection: Collection,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Read))?;
        let limit = req.query_int("limit").unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let mut q = DocQuery::new().limit(limit);
        if let Some(g) = req.query_int("date$gte") {
            q = q.date_gte(g);
        }
        if let Some(l) = req.query_int("date$lte") {
            q = q.date_lte(l);
        }
        if let Some(t) = req.query_get("type") {
            q = q.doc_type(t);
        }
        let docs = self
            .storage
            .search_documents(collection, &principal.user.id, &q)
            .await?;
        let arr: Vec<Value> = docs.iter().map(|d| enrich(d, true)).collect();
        Ok(env(200, json!(arr)))
    }

    async fn v3_create(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        collection: Collection,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Create))?;
        let body = req.body_json()?;
        let outcome = self.store_document(collection, body, principal, now_ms).await?;
        let identifier = outcome.doc().identifier.clone();
        let status = if outcome.created() { 201 } else { 200 };
        let resp = env(status, json!({ "identifier": identifier }));
        Ok(if outcome.created() {
            resp.with_header("Location", format!("/api/v3/{}/{identifier}", collection.table()))
        } else {
            resp
        })
    }

    async fn v3_read(
        &self,
        principal: &Principal,
        collection: Collection,
        identifier: &str,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Read))?;
        let doc = self
            .storage
            .get_document(collection, &principal.user.id, identifier)
            .await?
            .filter(|d| d.is_valid)
            .ok_or(ApiError::NotFound)?;
        Ok(env(200, enrich(&doc, true)))
    }

    async fn v3_delete(
        &self,
        principal: &Principal,
        collection: Collection,
        identifier: &str,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Delete))?;
        let deleted = self
            .storage
            .soft_delete_document(collection, &principal.user.id, identifier, now_ms)
            .await?;
        if deleted {
            Ok(env(200, json!({ "identifier": identifier })))
        } else {
            Err(ApiError::NotFound)
        }
    }

    async fn v3_put(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        collection: Collection,
        identifier: &str,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Update))?;
        let mut body = req.body_json()?;
        force_identifier(&mut body, identifier);
        let outcome = self.store_document(collection, body, principal, now_ms).await?;
        Ok(env(200, json!({ "identifier": outcome.doc().identifier })))
    }

    async fn v3_patch(
        &self,
        req: &ApiRequest,
        principal: &Principal,
        collection: Collection,
        identifier: &str,
        now_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Update))?;
        let existing = self
            .storage
            .get_document(collection, &principal.user.id, identifier)
            .await?
            .filter(|d| d.is_valid)
            .ok_or(ApiError::NotFound)?;
        let patch = req.body_json()?;
        let mut merged = existing.doc.clone();
        merge_objects(&mut merged, &patch);
        force_identifier(&mut merged, identifier);
        let outcome = self.store_document(collection, merged, principal, now_ms).await?;
        Ok(env(200, enrich(outcome.doc(), true)))
    }

    async fn v3_history(
        &self,
        principal: &Principal,
        collection: Collection,
        since_ms: i64,
    ) -> Result<ApiResponse, ApiError> {
        principal.require(perm(collection, Action::Read))?;
        let docs = self
            .storage
            .history_since(collection, &principal.user.id, since_ms, DEFAULT_HISTORY_LIMIT)
            .await?;
        let last = docs.iter().map(|d| d.srv_modified).max().unwrap_or(since_ms);
        let arr: Vec<Value> = docs.iter().map(|d| enrich(d, true)).collect();
        Ok(env(200, json!(arr)).with_header("Last-Modified", last.to_string()))
    }

    async fn v3_last_modified(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        let mut collections = serde_json::Map::new();
        for c in Collection::all() {
            let lm = self.storage.last_modified(*c, &principal.user.id).await?;
            collections.insert(c.table().to_string(), json!(lm));
        }
        Ok(env(200, json!({ "collections": collections })))
    }

    fn v3_status(&self, principal: &Principal) -> Result<ApiResponse, ApiError> {
        let perms: Vec<String> = principal.scopes.scopes().iter().map(|s| s.to_string()).collect();
        Ok(env(
            200,
            json!({
                "version": SERVICE_VERSION,
                "apiVersion": "3.0",
                "name": SERVICE_NAME,
                "srvDate": 0,
                "apiPermissions": perms,
            }),
        ))
    }
}

fn collection(name: &str) -> Result<Collection, ApiError> {
    Collection::from_path(name).ok_or(ApiError::NotFound)
}

/// Force the `identifier` field of a body to the path identifier (PUT/PATCH target).
fn force_identifier(body: &mut Value, identifier: &str) {
    if let Value::Object(map) = body {
        map.insert("identifier".into(), json!(identifier));
    }
}

/// Shallow-merge the fields of `patch` (an object) into `target`.
fn merge_objects(target: &mut Value, patch: &Value) {
    if let (Value::Object(t), Value::Object(p)) = (target, patch) {
        for (k, v) in p {
            t.insert(k.clone(), v.clone());
        }
    }
}
