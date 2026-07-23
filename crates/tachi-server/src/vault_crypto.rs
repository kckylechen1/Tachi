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
    zero_key, zero_string, DerivedVaultKey, KdfParams, AES_GCM_NONCE_LEN,
};
#[cfg(test)]
pub use vault_kit::{cheap_kdf_params_json, derive_cheap};

use aes_gcm::{
    aead::{AeadInPlace, KeyInit},
    Aes256Gcm, Key, Nonce, Tag,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use vault_kit::generate_nonce;

const AES_GCM_TAG_LEN: usize = 16;

/// The only `vault_config.kdf_algorithm` value this build's derivation
/// function implements. Every writer in this repo always sets this literal
/// (mirrors `bootstrap::vault_sync::ensure_importable_kdf`'s day-one-brick-fix
/// check, tachi#1080); there is no other supported value today.
const SUPPORTED_KDF_ALGORITHM: &str = "argon2id";

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

/// A stored `vault_config.kdf_algorithm` value that is not one this build's
/// derivation function implements (tachi#1080 attack-pass finding A1). Typed
/// **peer** of [`KdfParamsFormatError`] for the same reason: algorithm-axis
/// drift (e.g. a fork writes a different KDF with the same `{m,t,p}` param
/// shape) must never be misdiagnosed as a wrong password and must never feed
/// the unlock lockout counter. The `Display` form names both the stored value
/// and the supported algorithm, and is deliberately NOT phrased as a password
/// error.
#[derive(Debug, Clone)]
pub struct KdfAlgorithmMismatchError {
    message: String,
}

impl KdfAlgorithmMismatchError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for KdfAlgorithmMismatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KdfAlgorithmMismatchError {}

/// Exhaustive failures from deriving and verifying a key against a stored
/// vault configuration.
///
/// Callers that intentionally treat a wrong candidate password as a benign
/// miss must match [`Self::WrongPassword`] explicitly. Every stored-config
/// integrity or derivation failure remains distinguishable and loud.
#[derive(Debug)]
pub enum StoredVaultKeyDerivationError {
    InvalidSalt(String),
    KdfAlgorithmMismatch(KdfAlgorithmMismatchError),
    KdfParamsFormat(KdfParamsFormatError),
    Derivation(String),
    WrongPassword,
    CorruptVerifier(String),
}

impl std::fmt::Display for StoredVaultKeyDerivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSalt(err) => write!(f, "Invalid vault salt: {err}"),
            Self::KdfAlgorithmMismatch(err) => std::fmt::Display::fmt(err, f),
            Self::KdfParamsFormat(err) => std::fmt::Display::fmt(err, f),
            Self::Derivation(err) => write!(f, "Vault key derivation failed: {err}"),
            Self::WrongPassword => f.write_str("Wrong password"),
            Self::CorruptVerifier(err) => write!(f, "Invalid vault verifier: {err}"),
        }
    }
}

impl std::error::Error for StoredVaultKeyDerivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::KdfAlgorithmMismatch(err) => Some(err),
            Self::KdfParamsFormat(err) => Some(err),
            _ => None,
        }
    }
}

/// Parse the `vault_config.kdf_params` JSON column into a validated
/// `KdfParams` (fail-closed). Called by every unlock/verify path that derives
/// a key from a *stored* config (tachi#1080): as of the full wiring this is the
/// 8 call sites that read a stored `VaultConfig` and derive before verifying —
/// the MCP `handle_vault_unlock` handler, the CLI central verifier
/// (`vault_cli::derive_verified_vault_key_from_password`), the in-process
/// Keychain auto-unlock (`provider_config::auto_unlock_vault_from_keychain`),
/// the status-health Keychain loader (`status_health::vault`), the setup-wizard
/// unlock-existing-vault verifier (stores freshly collected API keys into an
/// already-initialized vault), both `env_cmd` unlock entry points (materialize + legacy),
/// and the stateless `vault_cli` session verifier. Malformed JSON or
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
pub fn parse_stored_kdf_params(config_kdf_params: &str) -> Result<KdfParams, KdfParamsFormatError> {
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
/// site calls (tachi#1080): it checks the stored `kdf_algorithm`, parses the
/// stored `kdf_params`, derives, and verifies the password, so those steps
/// are not re-implemented at each call site.
///
/// The exhaustive [`StoredVaultKeyDerivationError`] is the discriminator: a
/// caller may choose a benign outcome for [`StoredVaultKeyDerivationError::WrongPassword`]
/// without accidentally swallowing corrupt stored configuration or a local
/// derivation failure.
///
/// Checks `config.kdf_algorithm` fail-closed BEFORE deriving (tachi#1080
/// attack-pass finding A1): this function only implements Argon2id, so a
/// stored config naming any other algorithm — e.g. algorithm-axis drift where
/// a fork writes a different KDF with the same `{m,t,p}` param shape —
/// derives with Argon2id anyway and silently misdiagnoses as a wrong
/// password if left unchecked. An empty stored value is treated as the
/// implicit historical default (mirrors
/// `bootstrap::vault_sync::ensure_importable_kdf`'s same day-one-brick-fix
/// carve-out: `kdf_algorithm` is a `NOT NULL DEFAULT 'argon2id'` column with
/// no `#[serde(default)]`, so the only reachable "no algorithm recorded"
/// shape is an explicit empty string in an older/hand-crafted row). This is
/// an exact-empty check, not a trimmed one (tachi#1210): a whitespace-only
/// value is not a reachable "no algorithm recorded" shape from the schema
/// default, so it falls through to the mismatch branch below rather than
/// silently widening the default carve-out. Any other
/// mismatch returns [`StoredVaultKeyDerivationError::KdfAlgorithmMismatch`] —
/// a typed peer of [`StoredVaultKeyDerivationError::KdfParamsFormat`], so it
/// is never misread as [`StoredVaultKeyDerivationError::WrongPassword`] and
/// so never feeds a caller's brute-force lockout counter (mirrors how
/// `vault_ops::handlers::lifecycle::handle_vault_unlock` already keeps
/// `KdfParamsFormat` failures out of `record_vault_unlock_failure`).
pub fn derive_verified_key_from_stored_config(
    config: &memcore::vault::VaultConfig,
    password: &str,
) -> Result<DerivedVaultKey, StoredVaultKeyDerivationError> {
    let algorithm = if config.kdf_algorithm.is_empty() {
        SUPPORTED_KDF_ALGORITHM
    } else {
        config.kdf_algorithm.as_str()
    };
    if algorithm != SUPPORTED_KDF_ALGORITHM {
        return Err(StoredVaultKeyDerivationError::KdfAlgorithmMismatch(
            KdfAlgorithmMismatchError::new(format!(
                "vault_config.kdf_algorithm is not a supported KDF algorithm; refusing to \
                 derive (this is not a password error). stored kdf_algorithm={stored:?}; \
                 supported={supported:?}",
                stored = config.kdf_algorithm,
                supported = SUPPORTED_KDF_ALGORITHM,
            )),
        ));
    }
    let salt = B64
        .decode(&config.salt)
        .map_err(|err| StoredVaultKeyDerivationError::InvalidSalt(err.to_string()))?;
    let params = parse_stored_kdf_params(&config.kdf_params)
        .map_err(StoredVaultKeyDerivationError::KdfParamsFormat)?;
    let key = DerivedVaultKey::derive_with_params(password, &salt, &params)
        .map_err(|err| StoredVaultKeyDerivationError::Derivation(err.to_string()))?;
    let matches = verify_password(key.bytes(), &config.verifier)
        .map_err(StoredVaultKeyDerivationError::CorruptVerifier)?;
    if !matches {
        return Err(StoredVaultKeyDerivationError::WrongPassword);
    }
    Ok(key)
}

/// Read the Vault master password from macOS Keychain (service `tachi-vault`,
/// account `default`). Single low-level Keychain-read primitive (tachi#1175):
/// the CLI `--keychain` flag (`vault_cli::read_vault_password`) and the MCP
/// `vault_unlock` `use_keychain` parameter both call this instead of each
/// shelling out to `security` with their own copy of the args and error
/// handling. Returns `Err` with a message naming the exact failure
/// (unsupported platform, `security` invocation failure, missing entry, or
/// non-UTF8 value) — never a bare io/process error.
///
/// Background auto-unlock wraps this primitive and explicitly downgrades only
/// missing/empty entries to a benign miss; explicit unlock callers surface
/// every failure loudly.
pub fn read_password_from_macos_keychain() -> Result<String, String> {
    // Test builds only: never shell out to the real `security` binary from a
    // unit test (that would read/depend on whatever `tachi-vault`/`default`
    // Keychain entry happens to exist on the machine running the test suite
    // — non-hermetic, and on a dev box that has actually run `tachi vault
    // unlock --keychain` it would silently succeed against real state).
    // These overrides make both the success and the missing-entry paths
    // deterministic. Never compiled into a release build.
    #[cfg(test)]
    if let Some(result) = test_keychain_override() {
        return result;
    }
    if !cfg!(target_os = "macos") {
        return Err(
            "Keychain unlock is only supported on macOS; use --password-file (CLI) or a direct \
             password on Linux/Windows"
                .to_string(),
        );
    }
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
            "-w",
        ])
        .output()
        .map_err(|e| format!("failed to invoke `security` for Keychain read: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "no vault password found in Keychain (service: tachi-vault, account: default): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // #1175 codex 3.5: `.trim().to_string()` on the raw `security` stdout
    // makes a *second* copy of the password (the trimmed one returned below)
    // without clearing the untrimmed intermediate — that intermediate would
    // otherwise be dropped and freed with the plaintext password still
    // sitting in its backing buffer. Keep the untrimmed String bound so it
    // can be zeroed with the same `zero_string` primitive `password.rs`
    // already uses for its own password buffers, after the trimmed copy is
    // taken.
    let mut raw = String::from_utf8(output.stdout)
        .map_err(|e| format!("Keychain password is not valid UTF-8: {e}"))?;
    let password = raw.trim().to_string();
    zero_string(&mut raw);
    if password.is_empty() {
        return Err("Keychain entry for tachi-vault/default is empty".to_string());
    }
    Ok(password)
}

/// Test-only injection seam for [`read_password_from_macos_keychain`] (tachi#1175).
/// `TACHI_TEST_KEYCHAIN_PASSWORD=<value>` returns `Ok(value)` (or the
/// real empty-entry error if `value` is empty); `TACHI_TEST_FORCE_KEYCHAIN_MISSING=1`
/// returns the real missing-entry error text without invoking `security`.
/// Neither var set (the default for every other test) falls through to `None`
/// and the function runs its normal platform/`security` logic.
#[cfg(test)]
fn test_keychain_override() -> Option<Result<String, String>> {
    if std::env::var_os("TACHI_TEST_FORCE_KEYCHAIN_MISSING").is_some() {
        return Some(Err(
            "no vault password found in Keychain (service: tachi-vault, account: default): \
             test override (TACHI_TEST_FORCE_KEYCHAIN_MISSING)"
                .to_string(),
        ));
    }
    if let Ok(value) = std::env::var("TACHI_TEST_KEYCHAIN_PASSWORD") {
        return Some(if value.is_empty() {
            Err("Keychain entry for tachi-vault/default is empty".to_string())
        } else {
            Ok(value)
        });
    }
    None
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

    /// Feature unification must not weaken ordinary APIs; fixture speed needs
    /// an explicit test-support helper.
    #[test]
    fn test_support_is_explicit_under_feature_unification() {
        assert_eq!(active_kdf_params_json(), r#"{"m":65536,"t":3,"p":4}"#);
        assert_eq!(cheap_kdf_params_json(), r#"{"m":64,"t":1,"p":1}"#);
    }

    // --- tachi#1080 in-process seam discrimination tests ---
    //
    // These exercise the PURE seam `derive_verified_key_from_stored_config`
    // (parse stored kdf_params -> derive_with_params -> verify_password) with
    // zero system dependencies: no Keychain, no DB, no `security` CLI. The
    // discriminator is the exhaustive `StoredVaultKeyDerivationError`, so a
    // password mismatch can never share a branch with stored-data corruption.

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
            salt: B64.encode(salt),
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
            matches!(&err, StoredVaultKeyDerivationError::KdfParamsFormat(_)),
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
            matches!(&err, StoredVaultKeyDerivationError::KdfParamsFormat(_)),
            "malformed kdf_params JSON must surface as a typed KdfParamsFormatError; \
             got: {err}"
        );
    }

    /// (d) Legal config + WRONG password -> a password error, NOT a
    /// KdfParamsFormatError. This nails the two error classes apart: a stored
    /// format problem must never be misread as "wrong password", and a wrong
    /// password must never masquerade as a format problem.
    #[test]
    fn seam_wrong_password_has_dedicated_error_variant() {
        let config = make_stored_config_for_password("correct-pw");
        let err = derive_verified_key_from_stored_config(&config, "wrong-pw")
            .expect_err("wrong password must fail verification");
        assert!(
            matches!(&err, StoredVaultKeyDerivationError::WrongPassword),
            "wrong password must have its dedicated variant; got: {err}"
        );
    }

    #[test]
    fn seam_invalid_salt_is_not_a_password_miss() {
        let mut config = make_stored_config_for_password("correct-pw");
        config.salt = "not base64!".to_string();
        let err = derive_verified_key_from_stored_config(&config, "correct-pw")
            .expect_err("invalid stored salt must fail the seam");
        assert!(
            matches!(&err, StoredVaultKeyDerivationError::InvalidSalt(_)),
            "invalid stored salt must remain a typed integrity error; got: {err}"
        );
    }

    #[test]
    fn seam_corrupt_verifier_is_not_a_password_miss() {
        let mut config = make_stored_config_for_password("correct-pw");
        config.verifier = "not-a-verifier".to_string();
        let err = derive_verified_key_from_stored_config(&config, "correct-pw")
            .expect_err("corrupt stored verifier must fail the seam");
        assert!(
            matches!(&err, StoredVaultKeyDerivationError::CorruptVerifier(_)),
            "corrupt stored verifier must remain a typed integrity error; got: {err}"
        );
    }

    // --- tachi#1080 attack-pass finding A1: kdf_algorithm axis ---
    //
    // Pre-fix, `derive_verified_key_from_stored_config` never read
    // `config.kdf_algorithm` at all: algorithm-axis drift (a stored config
    // naming a KDF this build doesn't implement, but with a param shape that
    // still parses) fell straight through to `derive_with_params`, which
    // always derives with Argon2id regardless of the label. With the
    // *correct* password that silently produced `Ok(key)` instead of
    // surfacing the mismatch; with a differently-derived stored verifier
    // (the real-world shape — the fork actually derived under its own KDF)
    // it produces `Ok(false)` -> `WrongPassword`, feeding the brute-force
    // lockout counter for a config problem, not a password problem.
    //
    // #1187 fix-round correction (codex cross-vendor review, checkpoint 3):
    // only (e) and (f) below are genuinely RED on pre-fix `origin/main` — a
    // stored config using the fixture's own real password always verifies
    // successfully pre-fix (no algorithm check exists to short-circuit
    // before parsing/deriving), so `.expect_err(...)` in (e) and the
    // `Ok(_) => panic!(...)` arm in (f) both fail loudly. (g) is GREEN on
    // BOTH pre-fix and post-fix code — pre-fix already ignored
    // `kdf_algorithm` entirely, so an empty value already derived
    // successfully; (g) is compatibility regression coverage for the
    // legacy carve-out, not a discriminator. See also the live-handler
    // test `vault_unlock_kdf_algorithm_mismatch_does_not_feed_lockout_counter`
    // in `vault_ops/tests.rs`, added in the same fix-round, which is the
    // test that actually proves the production `handle_vault_unlock`
    // lockout counter stays untouched — (f) below only proves it against a
    // synthetic local match, not the real dispatch.

    /// (e) `kdf_algorithm` naming an unsupported KDF, with an otherwise VALID
    /// `kdf_params` shape and the objectively CORRECT password -> a typed
    /// `KdfAlgorithmMismatch`, never `Ok(_)` and never `WrongPassword`. Using
    /// the correct password (rather than a wrong one) is deliberate: it
    /// proves the algorithm check fires before any derivation is attempted at
    /// all, exactly like the existing `kdf_params`-format tests (b)/(c) above
    /// — a stored-config axis problem must be caught before password
    /// verification ever runs, not conflated with its outcome.
    #[test]
    fn seam_rejects_algorithm_mismatch_as_typed_error_not_wrong_password() {
        let mut config = make_stored_config_for_password("correct-pw");
        config.kdf_algorithm = "argon2i".to_string();
        let err = derive_verified_key_from_stored_config(&config, "correct-pw").expect_err(
            "unsupported kdf_algorithm must fail the seam even with the correct password",
        );
        assert!(
            matches!(&err, StoredVaultKeyDerivationError::KdfAlgorithmMismatch(_)),
            "algorithm-axis drift must surface as a typed KdfAlgorithmMismatch \
             (not a password error, not a silent Ok); got: {err}"
        );
        assert!(
            !matches!(&err, StoredVaultKeyDerivationError::WrongPassword),
            "algorithm-axis drift must never be misdiagnosed as WrongPassword; got: {err}"
        );
    }

    /// (f) Same shape as (e), documenting the specific claim from the
    /// attack-pass verdict: this error class must never feed a caller's
    /// brute-force lockout counter. This exercises only the PURE seam with a
    /// local synthetic counter/match, not the real production dispatch —
    /// #1187 fix-round correction (codex checkpoint 3/4): despite the match
    /// arm below being shaped like `handle_vault_unlock`'s real dispatch
    /// (now literally the seam it calls, after the fix-round wired the
    /// handler to this function), a synthetic local counter proves only
    /// itself. The production-lockout-counter claim is proven by
    /// `vault_unlock_kdf_algorithm_mismatch_does_not_feed_lockout_counter`
    /// (`vault_ops/tests.rs`), which drives the real `handle_vault_unlock`
    /// handler and asserts the real `failed_attempts` counter.
    #[test]
    fn seam_algorithm_mismatch_does_not_feed_lockout_counter() {
        let mut config = make_stored_config_for_password("correct-pw");
        config.kdf_algorithm = "argon2i".to_string();

        let mut lockout_counter = 0u32;
        let outcome = derive_verified_key_from_stored_config(&config, "correct-pw");
        match outcome {
            Ok(_) => panic!("algorithm-axis drift must not silently derive a usable key"),
            Err(StoredVaultKeyDerivationError::WrongPassword) => {
                // The ONLY branch production code increments the lockout
                // counter on (mirrors `record_vault_unlock_failure`'s sole
                // call site in `handle_vault_unlock`).
                lockout_counter += 1;
            }
            Err(_) => {
                // KdfAlgorithmMismatch (and every other typed integrity
                // error) reaches here and must NOT touch the counter.
            }
        }
        assert_eq!(
            lockout_counter, 0,
            "algorithm-axis drift must not increment the vault unlock lockout counter"
        );
    }

    /// (g) An empty stored `kdf_algorithm` is the historical implicit
    /// default, not a mismatch (mirrors
    /// `bootstrap::vault_sync::ensure_importable_kdf`'s identical carve-out):
    /// legacy/hand-crafted rows predating the column's introduction must keep
    /// unlocking, so this must NOT regress into a false-positive
    /// `KdfAlgorithmMismatch`.
    #[test]
    fn seam_empty_kdf_algorithm_is_treated_as_implicit_argon2id_default() {
        let mut config = make_stored_config_for_password("correct-pw");
        config.kdf_algorithm = String::new();
        let key = derive_verified_key_from_stored_config(&config, "correct-pw");
        assert!(
            key.is_ok(),
            "empty stored kdf_algorithm must fall back to the implicit argon2id default, not fail; got: {:?}",
            key.err()
        );
    }

    /// (h) tachi#1210: a WHITESPACE-ONLY stored `kdf_algorithm` is not the
    /// schema's "no algorithm recorded" shape (the column is `NOT NULL
    /// DEFAULT 'argon2id'` with no serde default, so the only reachable
    /// empty-shape row is an exact empty string) and must NOT silently widen
    /// the (g) default carve-out. It must be refused as a typed
    /// `KdfAlgorithmMismatch`, the same fail-closed branch as any other
    /// unrecognized algorithm label — never misdiagnosed as `WrongPassword`.
    #[test]
    fn seam_whitespace_only_kdf_algorithm_is_refused_not_defaulted() {
        let mut config = make_stored_config_for_password("correct-pw");
        config.kdf_algorithm = " ".to_string();
        let err = derive_verified_key_from_stored_config(&config, "correct-pw")
            .expect_err("whitespace-only kdf_algorithm must not silently default to argon2id");
        assert!(
            matches!(&err, StoredVaultKeyDerivationError::KdfAlgorithmMismatch(_)),
            "whitespace-only kdf_algorithm must surface as a typed KdfAlgorithmMismatch \
             (not a password error, not a silent default); got: {err}"
        );
    }
}
