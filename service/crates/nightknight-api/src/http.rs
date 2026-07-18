//! A tiny transport-agnostic HTTP request/response model.
//!
//! The API logic is written against these types, not against any web framework, so
//! the exact same handlers run under the Cloudflare Worker runtime and under `axum`
//! in the container — and can be unit-tested with no server at all.

use serde::Serialize;

use crate::error::ApiError;

/// HTTP method (only the ones the API uses are distinguished).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Head,
    Other,
}

impl Method {
    pub fn parse(s: &str) -> Method {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "OPTIONS" => Method::Options,
            "HEAD" => Method::Head,
            _ => Method::Other,
        }
    }
}

/// Case-insensitive header collection.
#[derive(Clone, Debug, Default)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn new() -> Headers {
        Headers(Vec::new())
    }

    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Headers {
        Headers(pairs.into_iter().collect())
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.push((name.into(), value.into()));
    }

    /// Case-insensitive header lookup (first match).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// An incoming request, normalised for the API layer.
#[derive(Clone, Debug)]
pub struct ApiRequest {
    pub method: Method,
    /// Path only (no query string), e.g. `/api/v1/entries.json`.
    pub path: String,
    /// Decoded query parameters.
    pub query: Vec<(String, String)>,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl ApiRequest {
    /// First query value for `key`, if present.
    pub fn query_get(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Parse a query value as an integer, if present and valid.
    pub fn query_int(&self, key: &str) -> Option<i64> {
        self.query_get(key).and_then(|v| v.parse().ok())
    }

    /// Parse the JSON body into a [`serde_json::Value`].
    pub fn body_json(&self) -> Result<serde_json::Value, ApiError> {
        if self.body.is_empty() {
            return Err(ApiError::BadRequest("empty request body".into()));
        }
        serde_json::from_slice(&self.body)
            .map_err(|e| ApiError::BadRequest(format!("invalid JSON body: {e}")))
    }
}

/// An outgoing response.
#[derive(Clone, Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl ApiResponse {
    /// A JSON response from any serialisable value.
    pub fn json<T: Serialize>(status: u16, value: &T) -> ApiResponse {
        let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
        ApiResponse {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body,
        }
    }

    /// A response carrying a raw byte body under an explicit content type — for file
    /// downloads (CSV, a pretty-printed JSON export) where the caller then attaches a
    /// `Content-Disposition` via [`with_header`](Self::with_header).
    pub fn bytes(status: u16, content_type: impl Into<String>, body: Vec<u8>) -> ApiResponse {
        ApiResponse {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            body,
        }
    }

    /// An empty response with just a status code.
    pub fn empty(status: u16) -> ApiResponse {
        ApiResponse {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// Attach (or append) a response header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> ApiResponse {
        self.headers.push((name.into(), value.into()));
        self
    }
}
