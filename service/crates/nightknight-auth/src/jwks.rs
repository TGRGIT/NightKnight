//! JSON Web Key Set parsing → RSA public keys.
//!
//! Cloudflare Access and OIDC providers publish their signing keys as a JWKS at a
//! well-known URL. The runtime fetches and caches that JSON; this module turns it
//! into verifiable [`RsaPublicKey`]s. It does no I/O itself, so it stays pure and
//! testable.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rsa::{BigUint, RsaPublicKey};
use serde::Deserialize;

use crate::AuthError;

/// One key from a JWKS. Only the RSA fields we need are modelled.
#[derive(Clone, Debug, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    /// RSA modulus, base64url.
    #[serde(default)]
    pub n: Option<String>,
    /// RSA public exponent, base64url.
    #[serde(default)]
    pub e: Option<String>,
}

impl Jwk {
    /// Reconstruct the RSA public key from the `n`/`e` parameters.
    pub fn rsa_public_key(&self) -> Result<RsaPublicKey, AuthError> {
        if self.kty != "RSA" {
            return Err(AuthError::UnsupportedKey(self.kty.clone()));
        }
        let n = self.n.as_deref().ok_or(AuthError::MalformedJwk)?;
        let e = self.e.as_deref().ok_or(AuthError::MalformedJwk)?;
        let n = URL_SAFE_NO_PAD.decode(n).map_err(|_| AuthError::MalformedJwk)?;
        let e = URL_SAFE_NO_PAD.decode(e).map_err(|_| AuthError::MalformedJwk)?;
        RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e))
            .map_err(|e| AuthError::Crypto(e.to_string()))
    }
}

/// A set of JSON Web Keys.
#[derive(Clone, Debug, Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

impl Jwks {
    /// Parse a JWKS document.
    pub fn parse(json: &str) -> Result<Jwks, AuthError> {
        serde_json::from_str(json).map_err(|e| AuthError::Decode(e.to_string()))
    }

    /// Find the key matching `kid`. If the token carries no `kid`, fall back to the
    /// sole key (common for single-key providers).
    pub fn find(&self, kid: Option<&str>) -> Option<&Jwk> {
        match kid {
            Some(k) => self.keys.iter().find(|j| j.kid.as_deref() == Some(k)),
            None => {
                if self.keys.len() == 1 {
                    self.keys.first()
                } else {
                    None
                }
            }
        }
    }
}
