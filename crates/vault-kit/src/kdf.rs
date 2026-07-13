//! Argon2id key derivation + key/string zeroization.
//!
//! Moved verbatim from `tachi-server/src/vault_crypto.rs` (tachi#1080 v1).
//! `derive`/`active_kdf_params_json` keep the exact compile-time-constant
//! behavior every existing caller relies on (zero call-site changes on
//! adoption). The versioned, storage-driven counterpart lives in
//! [`crate::kdf_params`] and is new, additive API — see that module for why
//! `derive` itself is intentionally left alone.

use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore;

use crate::kdf_params::{KdfParams, KdfParamsError};

// The weak (lightweight) test profile must NEVER be reachable from a
// release build, no matter what feature flags a downstream consumer or a
// `--all-features` sweep turns on. `feature = "test-support"` alone is not
// enough of a guard: `cargo build --release --all-features` (or a
// downstream crate mistakenly enabling the feature in its own
// `[dependencies]`, not just `[dev-dependencies]`) would flip it on in a
// real release binary. Gating on `debug_assertions` too closes that: Cargo
// disables `debug_assertions` in `--release` profiles by default, so the
// weak branch is compiled out of every release build regardless of which
// features are active — `test-support` only has an effect in a `dev`
// (debug_assertions-on) build, which is the only place it's meant to help
// (a downstream crate's own `cargo test`).
#[cfg(not(any(test, all(feature = "test-support", debug_assertions))))]
const PRODUCTION_KDF_MEMORY_COST: u32 = 65_536;
#[cfg(not(any(test, all(feature = "test-support", debug_assertions))))]
const PRODUCTION_KDF_TIME_COST: u32 = 3;
#[cfg(not(any(test, all(feature = "test-support", debug_assertions))))]
const PRODUCTION_KDF_PARALLELISM: u32 = 4;
#[cfg(not(any(test, all(feature = "test-support", debug_assertions))))]
const PRODUCTION_KDF_PARAMS_JSON: &str = r#"{"m":65536,"t":3,"p":4}"#;

#[cfg(any(test, all(feature = "test-support", debug_assertions)))]
const TEST_KDF_MEMORY_COST: u32 = 64;
#[cfg(any(test, all(feature = "test-support", debug_assertions)))]
const TEST_KDF_TIME_COST: u32 = 1;
#[cfg(any(test, all(feature = "test-support", debug_assertions)))]
const TEST_KDF_PARALLELISM: u32 = 1;
#[cfg(any(test, all(feature = "test-support", debug_assertions)))]
const TEST_KDF_PARAMS_JSON: &str = r#"{"m":64,"t":1,"p":1}"#;

#[derive(Clone, Copy)]
struct VaultKdfParams {
    memory_cost: u32,
    time_cost: u32,
    parallelism: u32,
}

fn active_kdf_params() -> VaultKdfParams {
    #[cfg(any(test, all(feature = "test-support", debug_assertions)))]
    {
        VaultKdfParams {
            memory_cost: TEST_KDF_MEMORY_COST,
            time_cost: TEST_KDF_TIME_COST,
            parallelism: TEST_KDF_PARALLELISM,
        }
    }
    #[cfg(not(any(test, all(feature = "test-support", debug_assertions))))]
    {
        VaultKdfParams {
            memory_cost: PRODUCTION_KDF_MEMORY_COST,
            time_cost: PRODUCTION_KDF_TIME_COST,
            parallelism: PRODUCTION_KDF_PARALLELISM,
        }
    }
}

/// The `vault_config.kdf_params` JSON this build's `derive()` implicitly
/// uses. Existing callers write this value at vault-init time but (today)
/// never read it back at unlock — see [`crate::kdf_params`] for the
/// versioned type that closes that gap for new call sites.
pub fn active_kdf_params_json() -> &'static str {
    #[cfg(any(test, all(feature = "test-support", debug_assertions)))]
    {
        TEST_KDF_PARAMS_JSON
    }
    #[cfg(not(any(test, all(feature = "test-support", debug_assertions))))]
    {
        PRODUCTION_KDF_PARAMS_JSON
    }
}

/// Derive a 32-byte encryption key from password + salt using Argon2id,
/// with the given KDF parameters (already validated by the caller).
fn derive_key_into_with(
    password: &str,
    salt: &[u8],
    kdf: VaultKdfParams,
    key: &mut [u8; 32],
) -> Result<(), String> {
    let params = Params::new(kdf.memory_cost, kdf.time_cost, kdf.parallelism, Some(32))
        .map_err(|e| format!("Argon2 params error: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password.as_bytes(), salt, key)
        .map_err(|e| format!("Key derivation failed: {e}"))?;
    Ok(())
}

/// Derive a 32-byte encryption key from password + salt using Argon2id.
fn derive_key_into(password: &str, salt: &[u8], key: &mut [u8; 32]) -> Result<(), String> {
    derive_key_into_with(password, salt, active_kdf_params(), key)
}

/// A derived vault encryption key. Zeroizes its bytes on drop.
pub struct DerivedVaultKey {
    bytes: [u8; 32],
}

/// Hand-written, redacting `Debug` impl — deliberately **not**
/// `#[derive(Debug)]`. A derive would format `bytes` (the raw 32-byte key
/// material) verbatim, letting `{:?}`/`expect_err`/panic messages leak the
/// vault key into logs or test failure output. Secret key material must
/// never reach a `Debug` formatter.
impl std::fmt::Debug for DerivedVaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DerivedVaultKey([REDACTED; 32])")
    }
}

impl DerivedVaultKey {
    /// Derive using the compile-time production/test Argon2 profile
    /// (`active_kdf_params_json()`), exactly as every existing caller does
    /// today. Unchanged on adoption.
    pub fn derive(password: &str, salt: &[u8]) -> Result<Self, String> {
        let mut bytes = [0u8; 32];
        derive_key_into(password, salt, &mut bytes)?;
        Ok(Self { bytes })
    }

    /// Derive using explicit, versioned KDF parameters (e.g. the parsed
    /// `vault_config.kdf_params` column) instead of the compile-time
    /// constant. Fail-closed: parameters outside [`KdfParams`]'s
    /// known-supported set are rejected *before* Argon2 runs.
    ///
    /// New API surface — no existing call site uses this yet; wiring a
    /// product's unlock path to read+enforce the stored value is deferred
    /// (tachi#1080 v1 comment, "separately specced before adoption").
    pub fn derive_with_params(
        password: &str,
        salt: &[u8],
        params: &KdfParams,
    ) -> Result<Self, KdfParamsError> {
        params.validate()?;
        let kdf = VaultKdfParams {
            memory_cost: params.memory_cost_kib,
            time_cost: params.time_cost,
            parallelism: params.parallelism,
        };
        let mut bytes = [0u8; 32];
        derive_key_into_with(password, salt, kdf, &mut bytes)
            .map_err(KdfParamsError::Derivation)?;
        Ok(Self { bytes })
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl Drop for DerivedVaultKey {
    fn drop(&mut self) {
        zero_key(&mut self.bytes);
    }
}

/// Overwrite key bytes before dropping stack buffers or cached key material.
pub fn zero_key(key: &mut [u8; 32]) {
    zero_bytes(key);
}

/// Overwrite sensitive string contents before dropping the allocation.
pub fn zero_string(value: &mut String) {
    // `String` stores UTF-8 bytes; writing NUL bytes preserves UTF-8 validity
    // while clearing the previous password contents in-place.
    // SAFETY: `value` is an exclusive `&mut String`, so the underlying
    // `Vec<u8>` is uniquely reachable through this reference. Writing NUL
    // bytes keeps the buffer valid UTF-8 (NUL is a valid UTF-8 code point),
    // so the `String` invariant is preserved. We do not reallocate or change
    // the length, so capacity/length stay consistent.
    let bytes = unsafe { value.as_mut_vec() };
    zero_bytes(bytes);
}

fn zero_bytes(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a unique `&mut u8` within a validly-initialized
        // slice; `write_volatile(byte, 0)` writes a single fully-initialized
        // byte and the volatile store prevents the compiler from eliding the
        // zeroization. The fence below orders the stores against later reads.
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Generate a random 32-byte salt.
pub fn generate_salt() -> [u8; 32] {
    let mut salt = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kdf_params_use_lightweight_test_profile() {
        let params = active_kdf_params();
        assert_eq!(params.memory_cost, TEST_KDF_MEMORY_COST);
        assert_eq!(params.time_cost, TEST_KDF_TIME_COST);
        assert_eq!(params.parallelism, TEST_KDF_PARALLELISM);
        assert_eq!(active_kdf_params_json(), TEST_KDF_PARAMS_JSON);
    }

    #[test]
    fn test_derive_key_deterministic() {
        let password = "test_password";
        let salt = b"0123456789abcdef0123456789abcdef";
        let key1 = DerivedVaultKey::derive(password, salt).unwrap();
        let key2 = DerivedVaultKey::derive(password, salt).unwrap();
        assert_eq!(key1.bytes(), key2.bytes());
    }

    #[test]
    fn test_derive_key_different_passwords() {
        let salt = b"0123456789abcdef0123456789abcdef";
        let key1 = DerivedVaultKey::derive("password1", salt).unwrap();
        let key2 = DerivedVaultKey::derive("password2", salt).unwrap();
        assert_ne!(key1.bytes(), key2.bytes());
    }

    #[test]
    fn zero_string_overwrites_contents_in_place() {
        let mut value = String::from("correct horse battery staple");
        let len = value.len();

        zero_string(&mut value);

        assert_eq!(value.len(), len);
        assert!(value.as_bytes().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn derive_with_params_rejects_unsupported_params_before_running_argon2() {
        let forged = KdfParams {
            memory_cost_kib: 1,
            time_cost: 9,
            parallelism: 9,
        };
        let err = DerivedVaultKey::derive_with_params("irrelevant", b"0123456789abcdef", &forged)
            .expect_err("unsupported KDF params must be rejected fail-closed");
        assert!(
            matches!(err, KdfParamsError::Unsupported { .. }),
            "expected a versioned Unsupported error, got {err:?}"
        );
    }

    #[test]
    fn derive_with_params_accepts_production_default() {
        // The stored value every healthy vault has today
        // (`active_kdf_params_json()`'s production constant) must still
        // derive successfully through the new enforced path — day-one
        // unlock is not affected by adding enforcement.
        let key = DerivedVaultKey::derive_with_params(
            "day-one-password",
            b"0123456789abcdef",
            &KdfParams::PRODUCTION,
        )
        .expect("production KdfParams must remain derivable");
        assert_eq!(key.bytes().len(), 32);
    }

    #[test]
    fn debug_never_formats_the_raw_key_bytes() {
        // Regression pin for the hand-written `Debug` impl above: if anyone
        // ever switches this back to `#[derive(Debug)]`, this test fails
        // instead of silently starting to leak key material into
        // `{:?}`/`expect_err`/panic output.
        let key = DerivedVaultKey::derive("regression-pin-password", b"0123456789abcdef")
            .expect("derive must succeed");
        assert_eq!(format!("{key:?}"), "DerivedVaultKey([REDACTED; 32])");
    }
}
