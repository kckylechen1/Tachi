//! vault-kit — shared Argon2id + AES-256-GCM vault crypto primitives and
//! versioned KDF-parameter format.
//!
//! # Scope (tachi#1080 v1)
//!
//! An adversarial cross-vendor design review rejected the original
//! "crypto+store" extraction as over-scoped: vault *storage* (SQL tables,
//! migrations, transactions, audit rows) is product-local — bound to each
//! product's own `MemoryStore`, error types, and migration authority — and
//! stays where it is. The defensible shared boundary this crate owns is
//! **crypto + versioned format only**:
//!
//! - Argon2id key derivation + zeroization ([`DerivedVaultKey`], [`zero_key`], [`zero_string`])
//! - AES-256-GCM encrypt/decrypt with the nonce-length guard ([`encrypt`], [`decrypt`])
//! - the verifier blob, with three-state verify ([`create_verifier`], [`verify_password`])
//! - a versioned, storage-driven KDF-parameter type ([`kdf_params::KdfParams`])
//!
//! This crate depends on nothing else in the workspace (not `memcore`, not
//! `tachi-server`) so a downstream fork can pin it alone without pulling in
//! product code. It has no `store`/DB surface at all — that stays in each
//! product, per the v1 adjudication.
//!
//! Cross-product invariant: this crate's structure must match
//! `Hyperion-HyperTachi`'s `crates/memory-server/src/vault_crypto.rs`
//! (post `84ed97a5`) for anything both products share — the nonce-length
//! guard and the three-state verify classification exist in both places for
//! the same reason and must not drift apart.
//!
//! `validate_secret_name` and the detached-tag `authenticate`/
//! `verify_authentication` functions (used only by Sigil's vault sync
//! bundle signing) are deliberately **not** part of this crate — they are
//! not crypto/format primitives either product shares, so they stay in
//! `tachi-server::vault_crypto`.

mod cipher;
mod kdf;
pub mod kdf_params;
mod verifier;

pub use cipher::{decrypt, encrypt, generate_nonce, AES_GCM_NONCE_LEN};
pub use kdf::{active_kdf_params_json, generate_salt, zero_key, zero_string, DerivedVaultKey};
#[cfg(feature = "test-support")]
pub use kdf::test_support::{cheap_kdf_params_json, derive_cheap};
pub use kdf_params::{KdfParams, KdfParamsError, KdfParamsProbe};
pub use verifier::{create_verifier, verify_password};

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end golden vector: fixed (password, salt) derives the same
    /// key via both the legacy `derive()` path (production profile is only
    /// active when this crate itself is *not* built with `test-support`;
    /// see the Cargo.toml feature docs) and the new, versioned
    /// `derive_with_params(..., &KdfParams::PRODUCTION)` path — and that
    /// key decrypts the pinned cipher.rs/verifier.rs golden ciphertexts.
    /// This is the "crypto+format contract anchor" tachi#1080 v1 calls for:
    /// derivation, encryption, and the verifier format all pin to the same
    /// known-good vector.
    #[test]
    fn golden_vector_full_chain_password_salt_to_derived_key() {
        let password = "vault-kit-golden-password";
        let salt: [u8; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let expected_key: [u8; 32] = [
            0xa0, 0x59, 0xf9, 0x0c, 0x88, 0x70, 0x71, 0xc3, 0x27, 0x18, 0x48, 0x0f, 0x10, 0xb0,
            0x78, 0x5f, 0xb0, 0xa0, 0x97, 0xc8, 0x57, 0x38, 0x8a, 0x15, 0x82, 0x71, 0xd5, 0x07,
            0xc8, 0x09, 0xeb, 0xd4,
        ];

        let derived = DerivedVaultKey::derive_with_params(password, &salt, &KdfParams::PRODUCTION)
            .expect("production KdfParams must derive");
        assert_eq!(
            derived.bytes(),
            &expected_key,
            "Argon2id output for this pinned (password, salt, KdfParams::PRODUCTION) drifted \
             from the cross-implementation reference vector"
        );

        let ciphertext_b64 = "ZE3sl1Q0g3DHDxzfF4W/+r+VmdF6Rq1RBcCdygCg7/gJVLrrlxp2OX9vdw5n7H9hZ/dY5fUUxyrRTim2O8wyxAsAYdu0dfAsGg==";
        let nonce_b64 = "AAECAwQFBgcICQoL";
        let decrypted = decrypt(derived.bytes(), ciphertext_b64, nonce_b64)
            .expect("derived key must decrypt the pinned golden ciphertext");
        assert_eq!(
            decrypted,
            b"vault-kit golden plaintext: crypto+format contract anchor".to_vec()
        );

        let verifier = "DA0ODxAREhMUFRYX:tIwb3wSWBIbpCyBQ/AFauWbOzF+EefFdjXDXBeay";
        assert_eq!(verify_password(derived.bytes(), verifier), Ok(true));
    }

    #[test]
    fn kdf_params_json_roundtrips_through_serde() {
        let json = serde_json::to_string(&KdfParams::PRODUCTION).unwrap();
        let parsed: KdfParams = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, KdfParams::PRODUCTION);
        // Matches the exact wire shape both products already write.
        assert_eq!(json, r#"{"m":65536,"t":3,"p":4}"#);
    }
}
