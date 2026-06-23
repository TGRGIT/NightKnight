//! # nightknight-auth
//!
//! Authentication and authorization primitives for NightKnight, shared by both
//! deployment targets. Pure (no I/O): the runtime fetches/caches JWKS and supplies
//! the current time; this crate verifies tokens and decides permissions.
//!
//! * [`token`] — RS256 JWT verification ([`Verifier`]) → [`Identity`]
//!   (Cloudflare Access *and* self-hosted OIDC).
//! * [`jwks`] — JWKS parsing → RSA public keys.
//! * [`scope`] — the Nightscout v3 `{api}:{collection}:{action}` permission model.
//!
//! ## Tokens are header-only
//!
//! NightKnight never accepts a credential in a URL query string (the legacy
//! Nightscout `?token=` / `?secret=` pattern), because query strings leak into
//! logs, history, and referrers. Use [`extract_bearer`] / the `api-secret` header.
//! This is a deliberate, documented deviation from legacy Nightscout.

pub mod jwks;
pub mod scope;
pub mod token;

pub use jwks::{Jwk, Jwks};
pub use scope::{Action, Permission, Scope, ScopeSet};
pub use token::{Identity, PrincipalKind, Verifier};

/// Authentication / authorization failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("no credential supplied")]
    MissingToken,
    #[error("malformed token")]
    MalformedToken,
    #[error("unsupported JWT algorithm: {0}")]
    UnsupportedAlg(String),
    #[error("no matching signing key for token")]
    NoMatchingKey,
    #[error("malformed JWK")]
    MalformedJwk,
    #[error("unsupported key type: {0}")]
    UnsupportedKey(String),
    #[error("invalid signature")]
    InvalidSignature,
    #[error("token expired")]
    Expired,
    #[error("token has no expiry claim")]
    MissingExpiry,
    #[error("token not yet valid")]
    NotYetValid,
    #[error("audience mismatch")]
    AudienceMismatch,
    #[error("issuer mismatch")]
    IssuerMismatch,
    #[error("token has no usable subject")]
    NoSubject,
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("decode error: {0}")]
    Decode(String),
}

/// Extract a bearer token from an `Authorization` header value, if present and
/// well-formed (`"Bearer <token>"`, case-insensitive scheme). Returns `None`
/// otherwise — callers must NOT fall back to reading the query string.
pub fn extract_bearer(authorization_header: Option<&str>) -> Option<&str> {
    let value = authorization_header?.trim();
    let (scheme, rest) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !rest.trim().is_empty() {
        Some(rest.trim())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed bearer header yields the token; the scheme is case-insensitive.
    #[test]
    fn extracts_bearer_token() {
        assert_eq!(extract_bearer(Some("Bearer abc.def.ghi")), Some("abc.def.ghi"));
        assert_eq!(extract_bearer(Some("bearer xyz")), Some("xyz"));
    }

    /// Anything that isn't a bearer credential yields nothing — we never guess.
    #[test]
    fn ignores_non_bearer_headers() {
        assert_eq!(extract_bearer(None), None);
        assert_eq!(extract_bearer(Some("Basic dXNlcjpwYXNz")), None);
        assert_eq!(extract_bearer(Some("Bearer ")), None);
        assert_eq!(extract_bearer(Some("token-without-scheme")), None);
    }
}
