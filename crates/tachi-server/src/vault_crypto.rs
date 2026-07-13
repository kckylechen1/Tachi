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

/// A stored `vault_config.kdf_params` value that could not be parsed into a
/// supported `KdfParams`. This is a **typed** error (not a bare `String`) so an
/// outer catch-all can `downcast_ref::<KdfParamsFormatError>()` and refuse to
/// degrade — e.g. the setup wizard must NOT swallow a stored-format failure
/// into a plaintext-config.env fallback (tachi#1080). The `Display` form is the
/// loud, versioned message used everywhere else; it names both the stored value
/// and the supported set, and is deliberately NOT phrased as a password error.
#[derive(Debug, Clone)]
pub struct KdfParamsFormatError {
    message: String,
}

impl KdfParamsFormatError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for KdfParamsFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KdfParamsFormatError {}

/// Parse the `vault_config.kdf_params` JSON column into a validated
/// `KdfParams` (fail-closed). Called by every unlock/verify path that derives
/// a key from a *stored* config (tachi#1080): as of the full wiring this is the
/// 8 call sites that read a stored `VaultConfig` and derive before verifying —
/// the MCP `handle_vault_unlock` handler, the CLI central verifier
/// (`vault_cli::derive_verified_vault_key_from_password`), the in-process
/// Keychain auto-unlock (`provider_config::auto_unlock_vault_from_keychain`),
/// the status-health Keychain loader (`status_health::vault`), the setup-wizard
/// unlock-existing-vault verifier (stores freshly collected API keys into an
/// already-initialized vault), both `env_cmd` unlock entry points (materialize
/// + legacy), and the stateless `vault_cli` session verifier. Malformed JSON or
/// an unsupported parameter combination surfaces as a loud, versioned error
/// naming both the stored value and the supported set.
///
/// Deliberately NOT phrased as a password error — a stored-parameter mismatch
/// must never be misread as "wrong password" (and so must never count against
/// the brute-force lockout counter; callers return this error before reaching
/// `verify_password`). Never silently falls back to a compile-time default.
///
/// Returns a TYPED `KdfParamsFormatError` (not `String`) so outer catch-alls
/// can downcast and refuse to degrade. Most call sites stringify it via
/// `to_string()` (preserving the exact message); the setup wizard propagates
/// it un-stringified so its catch-all can downcast and abort instead of
/// falling through to the plaintext-persistence fallback.
pub fn parse_stored_kdf_params(
    config_kdf_params: &str,
) -> Result<KdfParams, KdfParamsFormatError> {
    KdfParams::from_stored_json(config_kdf_params).map_err(|err| {
        KdfParamsFormatError::new(format!(
            "vault_config.kdf_params is not a supported KDF parameter format; refusing to derive \
             (this is not a password error). stored kdf_params={stored:?}; \
             supported={supported:?}; classified as: {err}",
            stored = config_kdf_params,
            supported = KdfParams::supported(),
        ))
    })
}

/// Derive a verified vault key from a STORED `VaultConfig` + a candidate
/// password, using the config's own `kdf_params` (not a compile-time
/// constant). This is the in-process seam every stored-config unlock/verify
/// site calls (tachi#1080): it parses the stored kdf_params, derives, and
/// verifies the password, so the three inline steps are not re-implemented at
/// each call site.
///
/// Error classes are deliberately separable by downcast:
/// - a stored-format/version failure surfaces as a TYPED
///   [`KdfParamsFormatError`] (kept un-stringified inside the `Box<dyn Error>`);
/// - a password mismatch surfaces as a boxed `String` ("Wrong password");
/// - a salt/derivation failure surfaces as a boxed `String`.
/// A caller must never confuse a stored-format failure for a wrong password
/// (and vice versa) — the [`KdfParamsFormatError`] downcast is the discriminator.
pub fn derive_verified_key_from_stored_config(
    config: &memcore::vault::VaultConfig,
    password: &str,
) -> Result<DerivedVaultKey, Box<dyn std::error::Error>> {
    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    // parse_stored_kdf_params returns a TYPED KdfParamsFormatError; `?` keeps
    // it downcastable inside the Box<dyn Error> so a caller can distinguish a
    // stored-format failure from a password mismatch (tachi#1080).
    let params = parse_stored_kdf_params(&config.kdf_params)?;
    let key = DerivedVaultKey::derive_with_params(password, &salt, &params)
        .map_err(|e| e.to_string())?;
    if !verify_password(key.bytes(), &config.verifier)? {
        return Err("Wrong password".into());
    }
    Ok(key)
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

    // --- tachi#1080 in-process seam discrimination tests ---
    //
    // These exercise the PURE seam `derive_verified_key_from_stored_config`
    // (parse stored kdf_params -> derive_with_params -> verify_password) with
    // zero system dependencies: no Keychain, no DB, no `security` CLI. The
    // discriminator is a typed `KdfParamsFormatError` downcast, which a
    // password mismatch must NEVER satisfy.

    use memcore::vault::{VaultCipher, VaultConfig};

    /// Build a real `VaultConfig` (test-profile kdf_params) for a given
    /// password, so the seam has a legitimate salt + verifier to derive
    /// against. Mirrors what `vault_init` writes.
    fn make_stored_config_for_password(password: &str) -> VaultConfig {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let salt = generate_salt();
        let key = DerivedVaultKey::derive(password, &salt).expect("derive");
        let verifier = create_verifier(key.bytes()).expect("verifier");
        VaultConfig {
            salt: B64.encode(&salt),
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: active_kdf_params_json().to_string(),
            cipher: VaultCipher::Aes256Gcm,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// (a) Happy path: legal config (default kdf_params) + correct password -> Ok.
    #[test]
    fn seam_derives_ok_for_legal_config_and_correct_password() {
        let config = make_stored_config_for_password("correct-pw");
        let key = derive_verified_key_from_stored_config(&config, "correct-pw");
        assert!(
            key.is_ok(),
            "legal config + correct password must derive; got: {:?}",
            key.err()
        );
    }

    /// (b) Unsupported kdf_params -> a TYPED KdfParamsFormatError, NOT a
    /// password error. `{"m":1,"t":1,"p":1}` is outside the supported set in
    /// every build mode, so parse fails before Argon2 runs.
    #[test]
    fn seam_rejects_unsupported_kdf_params_as_format_error() {
        let mut config = make_stored_config_for_password("correct-pw");
        config.kdf_params = r#"{"m":1,"t":1,"p":1}"#.to_string();
        let err = derive_verified_key_from_stored_config(&config, "correct-pw")
            .expect_err("unsupported kdf_params must fail the seam");
        assert!(
            err.downcast_ref::<KdfParamsFormatError>().is_some(),
            "unsupported kdf_params must surface as a typed KdfParamsFormatError \
             (not a password error); got: {err}"
        );
    }

    /// (c) Malformed kdf_params JSON -> the SAME typed format error class.
    /// Pins that JSON corruption is a format/version failure, not a password
    /// failure.
    #[test]
    fn seam_rejects_malformed_kdf_json_as_format_error() {
        let mut config = make_stored_config_for_password("correct-pw");
        config.kdf_params = "not json".to_string();
        let err = derive_verified_key_from_stored_config(&config, "correct-pw")
            .expect_err("malformed kdf_params JSON must fail the seam");
        assert!(
            err.downcast_ref::<KdfParamsFormatError>().is_some(),
            "malformed kdf_params JSON must surface as a typed KdfParamsFormatError; \
             got: {err}"
        );
    }

    /// (d) Legal config + WRONG password -> a password error, NOT a
    /// KdfParamsFormatError. This nails the two error classes apart: a stored
    /// format problem must never be misread as "wrong password", and a wrong
    /// password must never masquerade as a format problem.
    #[test]
    fn seam_wrong_password_is_not_a_format_error() {
        let config = make_stored_config_for_password("correct-pw");
        let err = derive_verified_key_from_stored_config(&config, "wrong-pw")
            .expect_err("wrong password must fail verification");
        assert!(
            err.downcast_ref::<KdfParamsFormatError>().is_none(),
            "wrong password must NOT surface as a KdfParamsFormatError (would \
             conflate password mismatch with stored-format failure); got: {err}"
        );
    }
}
