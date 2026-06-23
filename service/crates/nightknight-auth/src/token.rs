//! RS256 JWT verification and identity extraction.
//!
//! Used for both deployment targets' edge identity:
//! * **Cloudflare Access** — the `Cf-Access-Jwt-Assertion` header. Humans carry an
//!   `email`; service tokens carry a `common_name`. Verified against CF's JWKS and
//!   the application **AUD**.
//! * **Self-hosted OIDC** — a bearer JWT from a configured issuer, verified against
//!   the issuer's JWKS (and optionally its `aud`).
//!
//! Verification is pure: the caller supplies the already-fetched [`Jwks`] and the
//! current time. We only support RS256 (what Cloudflare Access and Pocket ID use);
//! `alg: none` and HS256 are rejected outright.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rsa::pkcs1v15::{Signature, VerifyingKey};
// Imported anonymously: we need the trait's `verify` method in scope, but the name
// `Verifier` is our own config struct below.
use rsa::signature::Verifier as _;
use serde::Deserialize;
use sha2::Sha256;

use crate::jwks::Jwks;
use crate::AuthError;

/// Whether the verified principal is a person or a machine credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrincipalKind {
    /// A human (OIDC / passkey / OTP) — identified by email.
    Human,
    /// A machine (Cloudflare Access service token) — identified by common-name.
    Service,
}

/// The verified identity extracted from a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The canonical subject string used to key the app's user (email, else
    /// common-name, else `sub`).
    pub subject: String,
    pub kind: PrincipalKind,
    pub email: Option<String>,
    pub common_name: Option<String>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    #[serde(default)]
    aud: serde_json::Value,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    common_name: Option<String>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    exp: Option<i64>,
    #[serde(default)]
    nbf: Option<i64>,
}

/// Configurable JWT verifier.
#[derive(Clone, Debug)]
pub struct Verifier {
    /// Required `aud` value (`None` skips the check — only for providers that don't
    /// set a meaningful audience).
    pub expected_aud: Option<String>,
    /// Required `iss` value (`None` skips the check).
    pub expected_iss: Option<String>,
    /// Clock-skew tolerance, in seconds, applied to `exp`/`nbf`.
    pub leeway_secs: i64,
}

impl Verifier {
    /// A verifier for Cloudflare Access: enforce the application AUD; CF signs with a
    /// rotating key set so `iss` is not pinned here.
    pub fn cloudflare_access(aud: impl Into<String>) -> Verifier {
        Verifier {
            expected_aud: Some(aud.into()),
            expected_iss: None,
            leeway_secs: 60,
        }
    }

    /// A verifier for a self-hosted OIDC issuer.
    pub fn oidc(issuer: impl Into<String>, aud: Option<String>) -> Verifier {
        Verifier {
            expected_aud: aud,
            expected_iss: Some(issuer.into()),
            leeway_secs: 60,
        }
    }

    /// Verify `token` against `jwks` at time `now_secs` (Unix seconds). On success
    /// returns the verified [`Identity`].
    pub fn verify(&self, token: &str, jwks: &Jwks, now_secs: i64) -> Result<Identity, AuthError> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(AuthError::MalformedToken);
        }

        // Header → algorithm + key id. Only RS256 is accepted.
        let header_bytes = URL_SAFE_NO_PAD
            .decode(parts[0])
            .map_err(|e| AuthError::Decode(e.to_string()))?;
        let header: Header =
            serde_json::from_slice(&header_bytes).map_err(|e| AuthError::Decode(e.to_string()))?;
        if header.alg != "RS256" {
            return Err(AuthError::UnsupportedAlg(header.alg));
        }

        // Resolve the signing key and verify the signature over "header.payload".
        let jwk = jwks
            .find(header.kid.as_deref())
            .ok_or(AuthError::NoMatchingKey)?;
        let public_key = jwk.rsa_public_key()?;
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = URL_SAFE_NO_PAD
            .decode(parts[2])
            .map_err(|e| AuthError::Decode(e.to_string()))?;
        let signature =
            Signature::try_from(sig_bytes.as_slice()).map_err(|_| AuthError::InvalidSignature)?;
        let verifying_key = VerifyingKey::<Sha256>::new(public_key);
        verifying_key
            .verify(signing_input.as_bytes(), &signature)
            .map_err(|_| AuthError::InvalidSignature)?;

        // Signature is valid — now check the claims.
        let payload_bytes = URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|e| AuthError::Decode(e.to_string()))?;
        let claims: Claims =
            serde_json::from_slice(&payload_bytes).map_err(|e| AuthError::Decode(e.to_string()))?;

        if let Some(exp) = claims.exp {
            if now_secs > exp + self.leeway_secs {
                return Err(AuthError::Expired);
            }
        }
        if let Some(nbf) = claims.nbf {
            if now_secs + self.leeway_secs < nbf {
                return Err(AuthError::NotYetValid);
            }
        }
        if let Some(expected) = &self.expected_aud {
            if !aud_contains(&claims.aud, expected) {
                return Err(AuthError::AudienceMismatch);
            }
        }
        if let Some(expected) = &self.expected_iss {
            if claims.iss.as_deref() != Some(expected.as_str()) {
                return Err(AuthError::IssuerMismatch);
            }
        }

        // Build the identity. Prefer email (human), else common_name (service token),
        // else the opaque subject.
        let (subject, kind) = if let Some(email) = &claims.email {
            (email.clone(), PrincipalKind::Human)
        } else if let Some(cn) = &claims.common_name {
            (cn.clone(), PrincipalKind::Service)
        } else if let Some(sub) = &claims.sub {
            (sub.clone(), PrincipalKind::Human)
        } else {
            return Err(AuthError::NoSubject);
        };

        Ok(Identity {
            subject,
            kind,
            email: claims.email,
            common_name: claims.common_name,
        })
    }
}

/// Does the JWT `aud` claim (a string or an array of strings) contain `expected`?
fn aud_contains(aud: &serde_json::Value, expected: &str) -> bool {
    match aud {
        serde_json::Value::String(s) => s == expected,
        serde_json::Value::Array(items) => items
            .iter()
            .any(|v| v.as_str().map(|s| s == expected).unwrap_or(false)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use rsa::traits::PublicKeyParts;
    use rsa::{RsaPrivateKey, RsaPublicKey};

    /// Build a JWKS (with one key, `kid = "test"`) from a public key.
    fn jwks_for(pubkey: &RsaPublicKey) -> Jwks {
        let n = URL_SAFE_NO_PAD.encode(pubkey.n().to_bytes_be());
        let e = URL_SAFE_NO_PAD.encode(pubkey.e().to_bytes_be());
        let json = format!(
            r#"{{ "keys": [ {{ "kty": "RSA", "kid": "test", "alg": "RS256", "n": "{n}", "e": "{e}" }} ] }}"#
        );
        Jwks::parse(&json).unwrap()
    }

    /// Mint a signed RS256 JWT with the given claims JSON and `kid`.
    fn sign_jwt(sk: &SigningKey<Sha256>, kid: &str, claims_json: &str) -> String {
        let header = format!(r#"{{"alg":"RS256","kid":"{kid}"}}"#);
        let h = URL_SAFE_NO_PAD.encode(header.as_bytes());
        let p = URL_SAFE_NO_PAD.encode(claims_json.as_bytes());
        let signing_input = format!("{h}.{p}");
        let sig = sk.sign(signing_input.as_bytes());
        let s = URL_SAFE_NO_PAD.encode(sig.to_bytes());
        format!("{signing_input}.{s}")
    }

    fn keypair() -> (SigningKey<Sha256>, RsaPublicKey) {
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, 2048).expect("generate key");
        let pub_key = RsaPublicKey::from(&priv_key);
        (SigningKey::<Sha256>::new(priv_key), pub_key)
    }

    const NOW: i64 = 1_700_000_000;

    /// A valid human (email) token from Cloudflare Access verifies and yields a
    /// Human identity. This is the everyday browser login path.
    #[test]
    fn verifies_human_access_token() {
        let (sk, pk) = keypair();
        let jwks = jwks_for(&pk);
        let claims = format!(
            r#"{{ "aud": ["app-aud-tag"], "email": "alice@cooney.be", "sub": "abc", "exp": {} }}"#,
            NOW + 600
        );
        let token = sign_jwt(&sk, "test", &claims);
        let id = Verifier::cloudflare_access("app-aud-tag")
            .verify(&token, &jwks, NOW)
            .unwrap();
        assert_eq!(id.kind, PrincipalKind::Human);
        assert_eq!(id.subject, "alice@cooney.be");
    }

    /// A service-token (common_name, no email) verifies as a Service principal —
    /// this is how an uploader device passes the Access gate.
    #[test]
    fn verifies_service_token() {
        let (sk, pk) = keypair();
        let jwks = jwks_for(&pk);
        let claims = format!(
            r#"{{ "aud": "app-aud-tag", "common_name": "phone-uploader.token", "exp": {} }}"#,
            NOW + 600
        );
        let token = sign_jwt(&sk, "test", &claims);
        let id = Verifier::cloudflare_access("app-aud-tag")
            .verify(&token, &jwks, NOW)
            .unwrap();
        assert_eq!(id.kind, PrincipalKind::Service);
        assert_eq!(id.subject, "phone-uploader.token");
    }

    /// An expired token is rejected — a stale session must not keep working.
    #[test]
    fn rejects_expired_token() {
        let (sk, pk) = keypair();
        let jwks = jwks_for(&pk);
        let claims = format!(
            r#"{{ "aud": "app-aud-tag", "email": "a@b.c", "exp": {} }}"#,
            NOW - 3600
        );
        let token = sign_jwt(&sk, "test", &claims);
        let err = Verifier::cloudflare_access("app-aud-tag")
            .verify(&token, &jwks, NOW)
            .unwrap_err();
        assert!(matches!(err, AuthError::Expired));
    }

    /// A token minted for a different application (wrong AUD) is rejected — this is
    /// what stops a valid token for another Access app being replayed here.
    #[test]
    fn rejects_wrong_audience() {
        let (sk, pk) = keypair();
        let jwks = jwks_for(&pk);
        let claims = format!(r#"{{ "aud": "some-other-app", "email": "a@b.c", "exp": {} }}"#, NOW + 600);
        let token = sign_jwt(&sk, "test", &claims);
        let err = Verifier::cloudflare_access("app-aud-tag")
            .verify(&token, &jwks, NOW)
            .unwrap_err();
        assert!(matches!(err, AuthError::AudienceMismatch));
    }

    /// A token signed by a DIFFERENT key fails signature verification — the core
    /// defence against forged identities (incl. the *.pages.dev / preview bypass).
    #[test]
    fn rejects_tampered_or_foreign_signature() {
        let (sk, _pk) = keypair();
        let (_sk2, pk2) = keypair(); // unrelated key published in the JWKS
        let jwks = jwks_for(&pk2);
        let claims = format!(r#"{{ "aud": "app-aud-tag", "email": "a@b.c", "exp": {} }}"#, NOW + 600);
        let token = sign_jwt(&sk, "test", &claims);
        let err = Verifier::cloudflare_access("app-aud-tag")
            .verify(&token, &jwks, NOW)
            .unwrap_err();
        assert!(matches!(err, AuthError::InvalidSignature));
    }

    /// `alg: none` (and other non-RS256) tokens are rejected outright — a classic JWT
    /// downgrade attack.
    #[test]
    fn rejects_alg_none() {
        let (_sk, pk) = keypair();
        let jwks = jwks_for(&pk);
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"email":"a@b.c"}"#);
        let token = format!("{header}.{payload}.");
        let err = Verifier::cloudflare_access("app-aud-tag")
            .verify(&token, &jwks, NOW)
            .unwrap_err();
        assert!(matches!(err, AuthError::UnsupportedAlg(_)));
    }
}
