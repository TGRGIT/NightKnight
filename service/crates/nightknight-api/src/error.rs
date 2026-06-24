//! API error type and its mapping to HTTP responses.

use serde_json::json;

use nightknight_core::DocumentError;
use nightknight_storage::StorageError;

use crate::http::ApiResponse;

/// Everything that can go wrong handling a request, mapped to an HTTP status.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    pub fn status(&self) -> u16 {
        match self {
            ApiError::Unauthorized => 401,
            ApiError::Forbidden(_) => 403,
            ApiError::NotFound => 404,
            ApiError::BadRequest(_) => 400,
            ApiError::PayloadTooLarge => 413,
            ApiError::Conflict(_) => 409,
            ApiError::Storage(_) | ApiError::Internal(_) => 500,
        }
    }

    /// Render as a JSON error body `{ "status": <code>, "message": "..." }`.
    pub fn into_response(self) -> ApiResponse {
        let status = self.status();
        // Don't leak internal/storage detail to clients; log-worthy detail stays in
        // the Display impl used by the runtime's logger.
        let message = match &self {
            ApiError::Storage(_) | ApiError::Internal(_) => "internal error".to_string(),
            other => other.to_string(),
        };
        ApiResponse::json(status, &json!({ "status": status, "message": message }))
    }
}

impl From<StorageError> for ApiError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::NotFound => ApiError::NotFound,
            other => ApiError::Storage(other.to_string()),
        }
    }
}

impl From<DocumentError> for ApiError {
    fn from(e: DocumentError) -> Self {
        ApiError::BadRequest(e.to_string())
    }
}
