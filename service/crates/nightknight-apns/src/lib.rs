//! # nightknight-apns
//!
//! The Apple Push Notification service (APNs) provider for NightKnight's **silent
//! push** background-refresh path. NightKnight is a *follower*: glucose lands on the
//! server (a connector sync, an uploader, an import) and the phone has to be told to
//! wake up and fetch it. A silent push — a background notification carrying only
//! `content-available: 1` — is the timely, server-driven way to do that.
//!
//! This crate is the pure, transport-agnostic core of that mechanism:
//!
//! * [`provider_token`] / [`cached_provider_token`] — sign the short-lived **ES256
//!   JWT** APNs wants for token-based provider auth (ECDSA P-256 + SHA-256). Signing is
//!   deterministic (RFC 6979), so it needs no RNG and cross-compiles cleanly to the
//!   Worker's `wasm32` target.
//! * [`device_url`] / [`silent_push_headers`] / [`SILENT_PUSH_BODY`] — build the exact
//!   request a silent push requires (`apns-push-type: background`, `apns-priority: 5`,
//!   no alert/sound).
//! * [`classify`] — turn an APNs HTTP status into a [`SendOutcome`] so the caller knows
//!   whether to prune an unregistered token, back off, or log an auth error.
//!
//! The crate performs **no I/O**: the runtime (the Worker's `fetch`, the container's
//! `reqwest`) sends the request. That keeps it unit-testable end-to-end and keeps the
//! `.p8` secret handling and HTTP/2 quirks at the edge where they belong. See
//! `docs/SILENT-PUSH.md` for the full design.

use std::cell::RefCell;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;

/// The app's bundle id — the APNs **topic** and the default for stored device tokens
/// that don't carry one. Must match the iOS target's bundle identifier.
pub const DEFAULT_BUNDLE_ID: &str = "be.cooney.nightknight.NightKnight";

/// Production APNs host (TestFlight / App Store builds, `aps-environment=production`).
pub const PRODUCTION_HOST: &str = "https://api.push.apple.com";
/// Sandbox APNs host (Xcode debug / direct installs, `aps-environment=development`).
pub const SANDBOX_HOST: &str = "https://api.sandbox.push.apple.com";

/// How long a signed provider token is reused before re-signing. APNs rejects tokens
/// older than 60 minutes *and* rejects re-signing "too often" (`TooManyProviderToken
/// Updates`), so we sit comfortably between: refresh at ~50 minutes, never per-push.
const TOKEN_TTL_S: i64 = 50 * 60;

/// Which APNs environment a device token was minted under. A token minted by a
/// `development`-entitled build only works against the sandbox host; a TestFlight /
/// App Store build against production. Mixing them yields `400 BadDeviceToken`, which is
/// why each stored token carries its own environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApnsEnv {
    Sandbox,
    Production,
}

impl ApnsEnv {
    /// Parse the wire value; anything that isn't exactly `"production"` is sandbox (the
    /// safe default for a development build).
    pub fn parse(s: &str) -> ApnsEnv {
        if s == "production" {
            ApnsEnv::Production
        } else {
            ApnsEnv::Sandbox
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ApnsEnv::Sandbox => "sandbox",
            ApnsEnv::Production => "production",
        }
    }

    /// The APNs host for this environment.
    pub fn host(self) -> &'static str {
        match self {
            ApnsEnv::Production => PRODUCTION_HOST,
            ApnsEnv::Sandbox => SANDBOX_HOST,
        }
    }
}

/// The APNs provider configuration: the downloaded `.p8` auth key plus the identifiers
/// that name it and the app. The key is a **secret** (held in a Worker secret /
/// container env var); the rest are public identifiers.
#[derive(Clone, Debug)]
pub struct ApnsConfig {
    /// The full PEM of the APNs `.p8` auth key (`-----BEGIN PRIVATE KEY----- …`).
    pub key_p8: String,
    /// The 10-char Key ID of the auth key.
    pub key_id: String,
    /// The 10-char Apple Team ID.
    pub team_id: String,
    /// The app's bundle id (APNs topic).
    pub bundle_id: String,
    /// Default environment for device tokens that didn't report one.
    pub default_env: ApnsEnv,
}

impl ApnsConfig {
    /// Build a config from raw string parts, returning `None` unless the three required
    /// secrets (key PEM, key id, team id) are all present and non-empty — so a partial
    /// configuration disables push rather than failing later at send time. `bundle_id`
    /// falls back to [`DEFAULT_BUNDLE_ID`]; `default_env` to sandbox.
    pub fn from_parts(
        key_p8: Option<String>,
        key_id: Option<String>,
        team_id: Option<String>,
        bundle_id: Option<String>,
        default_env: Option<String>,
    ) -> Option<ApnsConfig> {
        let nonempty = |s: Option<String>| s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let key_p8 = nonempty(key_p8)?;
        let key_id = nonempty(key_id)?;
        let team_id = nonempty(team_id)?;
        Some(ApnsConfig {
            key_p8,
            key_id,
            team_id,
            bundle_id: nonempty(bundle_id).unwrap_or_else(|| DEFAULT_BUNDLE_ID.to_string()),
            default_env: default_env.map(|e| ApnsEnv::parse(&e)).unwrap_or(ApnsEnv::Sandbox),
        })
    }
}

/// Errors from this crate. Only key parsing can fail — everything else is pure string
/// building. The message is deliberately coarse so it never echoes key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApnsError {
    #[error("invalid APNs auth key (.p8): not a valid PKCS#8 EC private key")]
    BadKey,
}

/// What APNs said about a single push. The caller acts on these: prune the token on
/// [`SendOutcome::Unregistered`], back off on [`SendOutcome::RateLimited`], surface the
/// rest for diagnosis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    /// 200 — APNs accepted the notification for delivery.
    Delivered,
    /// 410 — the device is no longer registered; delete the token.
    Unregistered,
    /// 400 — bad/wrong-environment device token (`BadDeviceToken`, `DeviceTokenNot
    /// ForTopic`, …). The token is unusable as-is.
    BadDeviceToken,
    /// 403 — provider-auth or topic problem (`InvalidProviderToken`, `ExpiredProvider
    /// Token`, `TopicDisallowed`). A configuration error, not a per-device one.
    AuthError,
    /// 429 — `TooManyRequests` for this device token; back off.
    RateLimited,
    /// Any other status (5xx server errors, unexpected codes).
    Other(u16),
}

impl SendOutcome {
    /// Whether this outcome means the stored device token should be deleted.
    pub fn should_prune(self) -> bool {
        matches!(self, SendOutcome::Unregistered)
    }
}

/// Map an APNs HTTP response status to a [`SendOutcome`]. APNs puts a machine-readable
/// `reason` in the JSON body on failure; the caller logs the body, this only needs the
/// status to decide what to *do*.
pub fn classify(status: u16) -> SendOutcome {
    match status {
        200 => SendOutcome::Delivered,
        400 => SendOutcome::BadDeviceToken,
        403 => SendOutcome::AuthError,
        410 => SendOutcome::Unregistered,
        429 => SendOutcome::RateLimited,
        other => SendOutcome::Other(other),
    }
}

/// The per-device APNs endpoint for an environment and hex device token.
pub fn device_url(env: ApnsEnv, token: &str) -> String {
    format!("{}/3/device/{token}", env.host())
}

/// The body of a *silent* push — `content-available: 1` and nothing else. No `alert`,
/// `sound`, or `badge`, so iOS wakes the app in the background without showing anything.
pub const SILENT_PUSH_BODY: &str = r#"{"aps":{"content-available":1}}"#;

/// The headers a silent push requires. `apns-push-type: background` + `apns-priority: 5`
/// is the *definition* of a silent push (priority 10, or a missing push-type, is
/// rejected). `apns-expiration: 0` drops a "new data" nudge that can't be delivered now
/// rather than storing a stale one; `apns-collapse-id` coalesces a burst of readings
/// into a single wake-up.
pub fn silent_push_headers(jwt: &str, bundle_id: &str) -> Vec<(String, String)> {
    vec![
        ("authorization".to_string(), format!("bearer {jwt}")),
        ("apns-topic".to_string(), bundle_id.to_string()),
        ("apns-push-type".to_string(), "background".to_string()),
        ("apns-priority".to_string(), "5".to_string()),
        ("apns-expiration".to_string(), "0".to_string()),
        ("apns-collapse-id".to_string(), "glucose".to_string()),
    ]
}

/// Sign a fresh APNs provider JWT (ES256). The token authenticates the *provider* (us)
/// to APNs and is valid for one hour, reusable across every push in that window — so the
/// caller should prefer [`cached_provider_token`]. `now_s` is the current time in epoch
/// **seconds** (the JWT `iat`).
pub fn provider_token(cfg: &ApnsConfig, now_s: i64) -> Result<String, ApnsError> {
    // Header/claims as compact JSON. Key order is irrelevant to APNs (it parses JSON);
    // serde_json emits object keys deterministically, which keeps the output stable.
    let header = serde_json::json!({ "alg": "ES256", "kid": cfg.key_id }).to_string();
    let claims = serde_json::json!({ "iss": cfg.team_id, "iat": now_s }).to_string();
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.as_bytes()),
        URL_SAFE_NO_PAD.encode(claims.as_bytes())
    );
    let key = SigningKey::from_pkcs8_pem(cfg.key_p8.trim()).map_err(|_| ApnsError::BadKey)?;
    // Deterministic ECDSA (RFC 6979): the 64-byte P1363 `r || s` signature APNs expects.
    let sig: Signature = key.sign(signing_input.as_bytes());
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(sig.to_bytes())
    ))
}

thread_local! {
    /// Per-isolate / per-thread cache of `(key_id, jwt, signed_at_s)`. The Worker reuses
    /// one isolate across cron ticks, so this caches the token across them; on the native
    /// runtime each worker thread keeps its own, signing at most once per [`TOKEN_TTL_S`].
    static TOKEN_CACHE: RefCell<Option<(String, String, i64)>> = const { RefCell::new(None) };
}

/// Like [`provider_token`], but reuses a recently-signed token (keyed by Key ID) until it
/// is ~50 minutes old. This avoids re-signing on every push — APNs penalises providers
/// that mint new tokens too frequently. The cache is process-local and best-effort; a
/// fresh process simply signs once more.
pub fn cached_provider_token(cfg: &ApnsConfig, now_s: i64) -> Result<String, ApnsError> {
    if let Some((kid, jwt, signed_at)) = TOKEN_CACHE.with(|c| c.borrow().clone()) {
        if kid == cfg.key_id && now_s - signed_at < TOKEN_TTL_S {
            return Ok(jwt);
        }
    }
    let jwt = provider_token(cfg, now_s)?;
    TOKEN_CACHE.with(|c| *c.borrow_mut() = Some((cfg.key_id.clone(), jwt.clone(), now_s)));
    Ok(jwt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    use p256::pkcs8::DecodePrivateKey;

    /// A throwaway P-256 PKCS#8 key, generated only for these tests — it authenticates
    /// nothing (no Key ID / Team ID is registered with Apple for it).
    const TEST_P8: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg+UX1vSS9hu87cb+j\n\
8IYJh/1gPjxMFJ++fcBqWz3VPeyhRANCAAQmIzIwzHseC+ITSgkQp2hZohMI9Jr3\n\
nohMe+5Ung2D+0iRphHJkTEAN8j5Tr6H/MBVZRlUTEYkn+wYRxPPW3kR\n\
-----END PRIVATE KEY-----\n";

    fn test_cfg() -> ApnsConfig {
        ApnsConfig {
            key_p8: TEST_P8.to_string(),
            key_id: "ABC1234DEF".to_string(),
            team_id: "XYZ9876WUV".to_string(),
            bundle_id: DEFAULT_BUNDLE_ID.to_string(),
            default_env: ApnsEnv::Sandbox,
        }
    }

    fn b64url_decode(s: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(s).expect("valid base64url")
    }

    /// GUARANTEE: the provider token is a real, verifiable ES256 JWT — three base64url
    /// segments, an `{alg:ES256,kid}` header, an `{iss,iat}` claim set, and a signature
    /// that verifies under the key's public half. If this drifts, APNs returns
    /// `InvalidProviderToken` and every push silently fails.
    #[test]
    fn provider_token_is_a_verifiable_es256_jwt() {
        let cfg = test_cfg();
        let jwt = provider_token(&cfg, 1_700_000_000).expect("signs");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "header.claims.signature");

        let header: serde_json::Value =
            serde_json::from_slice(&b64url_decode(parts[0])).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "ABC1234DEF");

        let claims: serde_json::Value =
            serde_json::from_slice(&b64url_decode(parts[1])).unwrap();
        assert_eq!(claims["iss"], "XYZ9876WUV");
        assert_eq!(claims["iat"], 1_700_000_000);

        // The signature covers exactly "header.claims" and verifies under the public key.
        let signing_key = SigningKey::from_pkcs8_pem(TEST_P8.trim()).unwrap();
        let verifying_key = VerifyingKey::from(&signing_key);
        let sig = Signature::from_slice(&b64url_decode(parts[2])).expect("64-byte P1363 sig");
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        verifying_key
            .verify(signing_input.as_bytes(), &sig)
            .expect("signature verifies over header.claims");
    }

    /// A malformed `.p8` is reported as [`ApnsError::BadKey`], never a panic — so a
    /// fat-fingered secret disables push instead of crashing the sync.
    #[test]
    fn bad_key_is_an_error_not_a_panic() {
        let mut cfg = test_cfg();
        cfg.key_p8 = "not a pem".to_string();
        assert_eq!(provider_token(&cfg, 1).unwrap_err(), ApnsError::BadKey);
    }

    /// The cached token is reused within the TTL and re-signed after it. ECDSA here is
    /// deterministic (RFC 6979), so a same-second re-sign is byte-identical; the cache
    /// must still hand back the *same* token within the window and a token with the new
    /// `iat` once the window passes.
    #[test]
    fn cached_token_refreshes_after_ttl() {
        let cfg = test_cfg();
        let t0 = 1_700_000_000;
        let a = cached_provider_token(&cfg, t0).unwrap();
        // Within the TTL: same cached token even though wall-clock advanced.
        let b = cached_provider_token(&cfg, t0 + TOKEN_TTL_S - 1).unwrap();
        assert_eq!(a, b, "token reused within the refresh window");
        // Past the TTL: a new token carrying the later `iat`.
        let c = cached_provider_token(&cfg, t0 + TOKEN_TTL_S + 1).unwrap();
        assert_ne!(a, c, "token re-signed after the refresh window");
    }

    /// Environments map to the right host, and anything but `"production"` is sandbox —
    /// the safe default, since a wrong host is a hard `BadDeviceToken`.
    #[test]
    fn environments_select_hosts() {
        assert_eq!(ApnsEnv::parse("production"), ApnsEnv::Production);
        assert_eq!(ApnsEnv::parse("sandbox"), ApnsEnv::Sandbox);
        assert_eq!(ApnsEnv::parse(""), ApnsEnv::Sandbox);
        assert_eq!(ApnsEnv::parse("Production"), ApnsEnv::Sandbox, "case-sensitive");
        assert_eq!(ApnsEnv::Production.host(), PRODUCTION_HOST);
        assert_eq!(ApnsEnv::Sandbox.host(), SANDBOX_HOST);
        assert!(device_url(ApnsEnv::Sandbox, "deadbeef")
            .ends_with("/3/device/deadbeef"));
    }

    /// The silent-push request is exactly what APNs requires: background type, priority
    /// 5, an expiry of 0, a collapse id, the bearer JWT, the bundle-id topic, and a body
    /// carrying ONLY `content-available` (no alert/sound/badge).
    #[test]
    fn silent_push_request_shape_is_correct() {
        let headers = silent_push_headers("the.jwt.here", "be.cooney.nightknight.NightKnight");
        let get = |k: &str| headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.as_str());
        assert_eq!(get("authorization"), Some("bearer the.jwt.here"));
        assert_eq!(get("apns-topic"), Some("be.cooney.nightknight.NightKnight"));
        assert_eq!(get("apns-push-type"), Some("background"));
        assert_eq!(get("apns-priority"), Some("5"));
        assert_eq!(get("apns-expiration"), Some("0"));
        assert_eq!(get("apns-collapse-id"), Some("glucose"));

        let body: serde_json::Value = serde_json::from_str(SILENT_PUSH_BODY).unwrap();
        assert_eq!(body["aps"]["content-available"], 1);
        assert!(body["aps"].get("alert").is_none(), "silent: no alert");
        assert!(body["aps"].get("sound").is_none(), "silent: no sound");
    }

    /// Status codes map to the right action — 410 prunes, others don't.
    #[test]
    fn classify_maps_statuses_to_outcomes() {
        assert_eq!(classify(200), SendOutcome::Delivered);
        assert_eq!(classify(410), SendOutcome::Unregistered);
        assert!(classify(410).should_prune());
        assert!(!classify(200).should_prune());
        assert_eq!(classify(400), SendOutcome::BadDeviceToken);
        assert_eq!(classify(403), SendOutcome::AuthError);
        assert_eq!(classify(429), SendOutcome::RateLimited);
        assert_eq!(classify(503), SendOutcome::Other(503));
        assert!(!classify(400).should_prune(), "a bad token is not auto-pruned");
    }

    /// `from_parts` disables push unless all three secrets are present, and fills sane
    /// defaults for the public bits.
    #[test]
    fn config_from_parts_requires_secrets() {
        assert!(ApnsConfig::from_parts(None, None, None, None, None).is_none());
        assert!(ApnsConfig::from_parts(
            Some("k".into()),
            Some("".into()),
            Some("t".into()),
            None,
            None
        )
        .is_none(), "blank key id disables push");
        let cfg = ApnsConfig::from_parts(
            Some(TEST_P8.into()),
            Some("ABC1234DEF".into()),
            Some("XYZ9876WUV".into()),
            None,
            Some("production".into()),
        )
        .expect("complete config");
        assert_eq!(cfg.bundle_id, DEFAULT_BUNDLE_ID, "default bundle id");
        assert_eq!(cfg.default_env, ApnsEnv::Production);
    }
}
