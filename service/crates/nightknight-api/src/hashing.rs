//! Token hashing.
//!
//! A device token is a random secret shown once. We store hashes, never the secret.
//! To authenticate, a client presents *some* form of the secret, and we hash what
//! they present and look it up:
//!
//! * Modern client → presents the **raw** token (`Authorization: Bearer <raw>` or
//!   `api-secret: <raw>`). Stored as `token_hash = sha256(raw)`.
//! * Legacy Nightscout uploader (xDrip+) → SHA-1-hashes the secret first and sends
//!   the hex in `api-secret`. Stored as `legacy_hash = sha256(sha1hex(raw))`.
//!
//! In *both* cases the lookup key is `sha256(presented_value)`, which equals one of
//! the two stored columns — so a single hash + a single lookup covers every client.

use sha1::Sha1;
use sha2::{Digest, Sha256};

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// SHA-256 hex of a string.
pub fn sha256_hex(s: &str) -> String {
    to_hex(&Sha256::digest(s.as_bytes()))
}

/// SHA-1 hex of a string (legacy Nightscout `api-secret` form).
pub fn sha1_hex(s: &str) -> String {
    to_hex(&Sha1::digest(s.as_bytes()))
}

/// The `token_hash` to store for a freshly issued raw token.
pub fn token_hash(raw: &str) -> String {
    sha256_hex(raw)
}

/// The `legacy_hash` to store, so SHA-1-hashing uploaders can authenticate.
pub fn legacy_hash(raw: &str) -> String {
    sha256_hex(&sha1_hex(raw))
}

/// The lookup key for a value a client presented (raw token or SHA-1 hex). Compared
/// against both `token_hash` and `legacy_hash`.
pub fn lookup_hash(presented: &str) -> String {
    sha256_hex(presented)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lookup key for a raw token matches the stored modern `token_hash`.
    #[test]
    fn raw_token_lookup_matches_token_hash() {
        let raw = "nk_secret_abc123";
        assert_eq!(lookup_hash(raw), token_hash(raw));
    }

    /// The lookup key for a SHA-1-hashed secret (what xDrip+ sends) matches the
    /// stored `legacy_hash` — proving the legacy path resolves to the same token.
    #[test]
    fn sha1_presentation_matches_legacy_hash() {
        let raw = "nk_secret_abc123";
        let presented = sha1_hex(raw); // what the legacy client sends
        assert_eq!(lookup_hash(&presented), legacy_hash(raw));
    }

    /// The two stored hashes differ, so the two presentation forms never collide.
    #[test]
    fn modern_and_legacy_hashes_differ() {
        let raw = "nk_secret_abc123";
        assert_ne!(token_hash(raw), legacy_hash(raw));
    }
}
