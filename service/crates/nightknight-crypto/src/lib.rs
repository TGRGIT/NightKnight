//! # nightknight-crypto
//!
//! Authenticated symmetric encryption (AES-256-GCM) for secrets at rest — namely the
//! vendor connector credentials (Dexcom / LibreLinkUp username+password) that users
//! enter in the UI. A leaked database alone cannot reveal them: each value is sealed
//! with a 96-bit random nonce under a 256-bit key held only in the runtime
//! environment (a Worker secret / container env var).
//!
//! Wire format (base64): `nonce(12) || ciphertext || tag(16)`. Decryption fails
//! (tamper-evident) if the ciphertext, tag, nonce, or key is wrong.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

/// Encryption errors. Messages are deliberately coarse so failures don't leak which
/// part was wrong (a padding/MAC oracle).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
    #[error("malformed ciphertext")]
    Malformed,
    #[error("invalid key (need 32 bytes as base64 or hex)")]
    BadKey,
}

const NONCE_LEN: usize = 12;

fn random_nonce() -> Result<[u8; NONCE_LEN], CryptoError> {
    let mut n = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut n).map_err(|_| CryptoError::Encrypt)?;
    Ok(n)
}

/// Parse a 256-bit key from base64 (44 chars) or hex (64 chars).
pub fn parse_key(s: &str) -> Result<[u8; 32], CryptoError> {
    let s = s.trim();
    // base64
    if let Ok(b) = STANDARD.decode(s) {
        if b.len() == 32 {
            let mut k = [0u8; 32];
            k.copy_from_slice(&b);
            return Ok(k);
        }
    }
    // hex
    if s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()) {
        let mut k = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16).ok_or(CryptoError::BadKey)?;
            let lo = (chunk[1] as char).to_digit(16).ok_or(CryptoError::BadKey)?;
            k[i] = (hi * 16 + lo) as u8;
        }
        return Ok(k);
    }
    Err(CryptoError::BadKey)
}

/// Seal `plaintext` under `key`. Output is base64 `nonce || ciphertext+tag`.
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<String, CryptoError> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce_bytes = random_nonce()?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher.encrypt(nonce, plaintext).map_err(|_| CryptoError::Encrypt)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(STANDARD.encode(out))
}

/// Open a value produced by [`encrypt`].
pub fn decrypt(key: &[u8; 32], b64: &str) -> Result<Vec<u8>, CryptoError> {
    let data = STANDARD.decode(b64.trim()).map_err(|_| CryptoError::Malformed)?;
    if data.len() < NONCE_LEN {
        return Err(CryptoError::Malformed);
    }
    let (nonce_bytes, ciphertext) = data.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| CryptoError::Decrypt)
}

/// Convenience: encrypt/decrypt UTF-8 strings (connector creds are JSON strings).
pub fn encrypt_str(key: &[u8; 32], plaintext: &str) -> Result<String, CryptoError> {
    encrypt(key, plaintext.as_bytes())
}
pub fn decrypt_str(key: &[u8; 32], b64: &str) -> Result<String, CryptoError> {
    String::from_utf8(decrypt(key, b64)?).map_err(|_| CryptoError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> [u8; 32] {
        [7u8; 32]
    }

    /// A sealed secret round-trips back to the original under the same key.
    #[test]
    fn round_trips() {
        let key = test_key();
        let secret = r#"{"email":"a@b.c","password":"hunter2"}"#;
        let sealed = encrypt_str(&key, secret).unwrap();
        assert_ne!(sealed, secret, "stored form is not plaintext");
        assert_eq!(decrypt_str(&key, &sealed).unwrap(), secret);
    }

    /// Two encryptions of the same plaintext differ (random nonce) — no leakage of
    /// equal credentials across rows.
    #[test]
    fn nonce_makes_ciphertexts_unique() {
        let key = test_key();
        let a = encrypt_str(&key, "same").unwrap();
        let b = encrypt_str(&key, "same").unwrap();
        assert_ne!(a, b);
    }

    /// The wrong key cannot decrypt — a leaked DB without the key is useless.
    #[test]
    fn wrong_key_fails() {
        let sealed = encrypt_str(&test_key(), "secret").unwrap();
        let wrong = [9u8; 32];
        assert_eq!(decrypt_str(&wrong, &sealed), Err(CryptoError::Decrypt));
    }

    /// Tampering with the ciphertext is detected by the GCM tag.
    #[test]
    fn tamper_is_detected() {
        let key = test_key();
        let sealed = encrypt_str(&key, "secret").unwrap();
        let mut raw = STANDARD.decode(&sealed).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0xFF; // flip a bit in the tag
        let tampered = STANDARD.encode(raw);
        assert_eq!(decrypt_str(&key, &tampered), Err(CryptoError::Decrypt));
    }

    /// Keys parse from both base64 and hex; junk is rejected.
    #[test]
    fn parses_keys() {
        let raw = [1u8; 32];
        let b64 = STANDARD.encode(raw);
        assert_eq!(parse_key(&b64).unwrap(), raw);
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(parse_key(&hex).unwrap(), raw);
        assert_eq!(parse_key("too-short"), Err(CryptoError::BadKey));
    }
}
