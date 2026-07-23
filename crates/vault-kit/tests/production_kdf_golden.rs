//! Integration test — NOT a `#[cfg(test)]` unit test inside `src/`.
//!
//! Cargo compiles this file as a separate crate that links `vault-kit`'s
//! library target as an ordinary external dependency. Run standalone
//! (`cargo test -p vault-kit`), vault-kit's `test-support` feature is off
//! for this crate. Run under a `--workspace` build, Cargo's feature
//! unification can pull `test-support` in here too, since tachi-server's
//! `[dev-dependencies]` entry requests it elsewhere in the same graph —
//! but that no longer changes what this test proves: `test-support` is
//! purely additive now, gating only the separate, opt-in
//! `test_support::derive_cheap` helper. It never swaps what
//! `DerivedVaultKey::derive()` / `active_kdf_params_json()` return, in
//! either build. Both run through the REAL production Argon2 profile
//! (m=65536 KiB, t=3, p=4), exactly as a release build would, whether the
//! feature is unified in or not. This is where a drift in the production
//! constants themselves gets caught, at the cost of one real ~100-300ms
//! Argon2id run.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use vault_kit::{active_kdf_params_json, create_verifier, DerivedVaultKey};

const GOLDEN_PASSWORD: &str = "vault-kit-golden-password";
const GOLDEN_SALT: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

// Cross-checked against an independent Argon2id implementation
// (argon2-cffi, Python) at m=65536 KiB / t=3 / p=4 / Argon2id / version
// 0x13, for the fixed (password, salt) pair above — the same reference
// vector `derive_with_params(..., &KdfParams::PRODUCTION)` is pinned
// against in `src/lib.rs`'s golden test. `derive()`'s compile-time
// production profile must produce the identical key.
const GOLDEN_PRODUCTION_KEY: [u8; 32] = [
    0xa0, 0x59, 0xf9, 0x0c, 0x88, 0x70, 0x71, 0xc3, 0x27, 0x18, 0x48, 0x0f, 0x10, 0xb0, 0x78, 0x5f,
    0xb0, 0xa0, 0x97, 0xc8, 0x57, 0x38, 0x8a, 0x15, 0x82, 0x71, 0xd5, 0x07, 0xc8, 0x09, 0xeb, 0xd4,
];

#[test]
fn derive_uses_the_real_production_argon2_profile() {
    assert_eq!(
        active_kdf_params_json(),
        r#"{"m":65536,"t":3,"p":4}"#,
        "an integration-test build (no cfg(test), no test-support feature) must see the \
         production KDF profile, not the lightweight test one"
    );

    let key = DerivedVaultKey::derive(GOLDEN_PASSWORD, &GOLDEN_SALT)
        .expect("production Argon2id derive must succeed");
    assert_eq!(
        key.bytes(),
        &GOLDEN_PRODUCTION_KEY,
        "derive()'s production Argon2 constants drifted from the pinned reference vector — \
         this is the one code path vault-kit's own #[cfg(test)] unit tests can never exercise, \
         since those always compile in the weak test profile"
    );
}

/// `create_verifier`'s output *shape* is a stable contract even though its
/// ciphertext/nonce bytes can't be pinned as a golden vector: AES-GCM uses
/// a fresh random nonce every call by design, and the public API doesn't
/// expose a way to inject a fixed one, so a pinned *writer* golden vector
/// is not possible here. The *read* side (decrypt / verify_password)
/// already has pinned golden vectors anchoring the wire format
/// (src/cipher.rs, src/verifier.rs, src/lib.rs) — this test only pins the
/// shape a writer must keep producing.
#[test]
fn create_verifier_output_shape_is_nonce_colon_ciphertext() {
    let key = [7u8; 32];
    let verifier = create_verifier(&key).expect("create_verifier must succeed");

    let parts: Vec<&str> = verifier.splitn(2, ':').collect();
    assert_eq!(
        parts.len(),
        2,
        "verifier must be exactly nonce_b64:ciphertext_b64, got {verifier:?}"
    );

    let nonce_bytes = B64
        .decode(parts[0])
        .expect("nonce field must be valid base64");
    assert_eq!(nonce_bytes.len(), 12, "AES-GCM nonce must be 12 raw bytes");

    B64.decode(parts[1])
        .expect("ciphertext field must be valid base64");
}
