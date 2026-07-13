// vault_crypto.rs — encryption primitives for Tachi Vault.
//
// tachi#1080 v1: the shared crypto+versioned-format boundary now lives in
// the `vault-kit` leaf crate (Argon2id KDF, zeroization, AES-256-GCM
// encrypt/decrypt with the nonce-length guard, versioned `KdfParams`, and
// the three-state verifier). This file re-exports that surface so every
// existing `crate::vault_crypto::*` call site keeps working unchanged.
//
// Two things stay here rather than moving into vault-kit, because they are
// Sigil-local, not part of the cross-product crypto/format boundary the
// adversarial design review adjudicated (see the issue): the detached-tag
// `authenticate`/`verify_authentication` pair (used only by this product's
// vault sync bundle signing — HyperTachi has no such bundle format), and
// `validate_secret_name` (a naming-convention validator, not a crypto or
// wire-format primitive).

pub use vault_kit::{
    active_kdf_params_json, create_verifier, decrypt, encrypt, generate_salt, verify_password,
    zero_key, zero_string, DerivedVaultKey, KdfParams, KdfParamsError, AES_GCM_NONCE_LEN,
};

use aes_gcm::{
    aead::{AeadInPlace, KeyInit},
    Aes256Gcm, Key, Nonce, Tag,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use vault_kit::generate_nonce;

const AES_GCM_TAG_LEN: usize = 16;

/// Parse the `vault_config.kdf_params` JSON column into a validated
/// `KdfParams` (fail-closed). Called by every unlock/verify path that derives
/// a key from a *stored* config (tachi#1080): malformed JSON or an
/// unsupported parameter combination surfaces as a loud, versioned error
/// naming both the stored value and the supported set.
///
/// Deliberately NOT phrased as a password error — a stored-parameter mismatch
/// must never be misread as "wrong password" (and so must never count against
/// the brute-force lockout counter; callers return this error before reaching
/// `verify_password`). Never silently falls back to a compile-time default.
pub fn parse_stored_kdf_params(config_kdf_params: &str) -> Result<KdfParams, String> {
    KdfParams::from_stored_json(config_kdf_params).map_err(|err| {
        format!(
            "vault_config.kdf_params is not a supported KDF parameter format; refusing to derive \
             (this is not a password error). stored kdf_params={stored:?}; \
             supported={supported:?}; classified as: {err}",
            stored = config_kdf_params,
            supported = KdfParams::supported(),
        )
    })
}

/// Authenticate associated data with AES-256-GCM detached tag and no ciphertext.
///
/// This is used for Vault sync bundles where the payload is already encrypted
/// row-by-row but the JSON wrapper still needs keyed tamper detection.
pub fn authenticate(key: &[u8; 32], aad: &[u8]) -> Result<(String, String), String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce_bytes = generate_nonce();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let tag = cipher
        .encrypt_in_place_detached(nonce, aad, &mut [])
        .map_err(|e| format!("Authentication failed: {e}"))?;
    Ok((B64.encode(tag), B64.encode(nonce_bytes)))
}

/// Verify a detached AES-256-GCM authentication tag over associated data.
pub fn verify_authentication(
    key: &[u8; 32],
    aad: &[u8],
    nonce_b64: &str,
    tag_b64: &str,
) -> Result<(), String> {
    let nonce_bytes = B64
        .decode(nonce_b64)
        .map_err(|e| format!("Bad authentication nonce base64: {e}"))?;
    if nonce_bytes.len() != AES_GCM_NONCE_LEN {
        return Err(format!(
            "Bad authentication nonce length: {} (expected {AES_GCM_NONCE_LEN})",
            nonce_bytes.len()
        ));
    }
    let tag_bytes = B64
        .decode(tag_b64)
        .map_err(|e| format!("Bad authentication tag base64: {e}"))?;
    if tag_bytes.len() != AES_GCM_TAG_LEN {
        return Err(format!(
            "Bad authentication tag length: {} (expected {AES_GCM_TAG_LEN})",
            tag_bytes.len()
        ));
    }

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt_in_place_detached(
            Nonce::from_slice(&nonce_bytes),
            aad,
            &mut [],
            Tag::from_slice(&tag_bytes),
        )
        .map_err(|_| "Authentication tag verification failed".to_string())
}

/// Validate a secret name. Returns Ok(()) if valid, Err otherwise.
pub fn validate_secret_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Secret name cannot be empty".into());
    }
    if name.len() > 128 {
        return Err("Secret name too long (max 128 chars)".into());
    }
    // Allow alphanumeric, underscore, dot, hyphen
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
    {
        return Err(
            "Secret name contains invalid characters (allowed: a-z, A-Z, 0-9, _, ., -)".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authentication_detects_tampered_aad() {
        let key = [7u8; 32];
        let aad = br#"{"bundle":"one"}"#;
        let (tag, nonce) = authenticate(&key, aad).unwrap();

        verify_authentication(&key, aad, &nonce, &tag).unwrap();
        assert!(verify_authentication(&key, br#"{"bundle":"two"}"#, &nonce, &tag).is_err());
        assert!(verify_authentication(&[8u8; 32], aad, &nonce, &tag).is_err());
    }

    #[test]
    fn test_validate_secret_name() {
        assert!(validate_secret_name("VALID_KEY").is_ok());
        assert!(validate_secret_name("valid.key").is_ok());
        assert!(validate_secret_name("valid_key_123").is_ok());
        assert!(validate_secret_name("invalid key").is_err());
        assert!(validate_secret_name("invalid-key!").is_err());
        assert!(validate_secret_name("").is_err());
        assert!(validate_secret_name("a".repeat(129).as_str()).is_err());
    }

    /// De-risks the dev-dependency feature-unification trick in
    /// `Cargo.toml` (`vault-kit` plain in `[dependencies]`, `test-support`
    /// only in `[dev-dependencies]`): tachi-server's own test binary must
    /// see vault-kit's lightweight Argon2 profile (m=64 KiB), not the
    /// production default (m=65536 KiB, ~100-300ms/derive) — otherwise
    /// every `DerivedVaultKey::derive` call across this crate's test suite
    /// silently starts paying real Argon2 cost.
    #[test]
    fn active_kdf_params_json_uses_test_support_profile_in_tachi_server_tests() {
        assert_eq!(active_kdf_params_json(), r#"{"m":64,"t":1,"p":1}"#);
    }
}
