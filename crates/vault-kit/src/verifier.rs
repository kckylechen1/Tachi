//! Vault verifier blob: create + three-state verify.
//!
//! `create_verifier` is unchanged from `tachi-server/src/vault_crypto.rs`.
//! `verify_password` is the one deliberate v1 behavior change (tachi#1080):
//! today both Sigil/tachi-server and the HyperTachi fork collapse *every*
//! decrypt failure — malformed base64, a bad nonce length, or an actual
//! wrong-password AEAD failure — into the same `Ok(false)` "wrong password"
//! outcome, which callers feed straight into brute-force lockout counters.
//! That conflates vault corruption with a mistyped password.
//!
//! This restructures the function (same `Result<bool, String>` signature —
//! zero call-site changes) so every parse/decode step happens directly here,
//! structurally, *before* any AEAD call: split, base64-decode both fields,
//! check the nonce length. Any failure there is unambiguously corruption
//! and returns `Err`. Only once the input is structurally valid do we call
//! the AEAD cipher; at that point the only way left to fail is
//! authentication itself, so that failure unambiguously means "wrong
//! password" and stays `Ok(false)`. This matches
//! Hyperion-HyperTachi@84ed97a5's `vault_crypto.rs` structure exactly, so
//! both products now classify corruption the same way.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};

use crate::cipher::{encrypt, AES_GCM_NONCE_LEN};

const VERIFIER_PLAINTEXT: &[u8] = b"tachi-vault-ok";

/// Create the verifier blob (encrypt known plaintext). Format: "nonce_b64:ciphertext_b64"
pub fn create_verifier(key: &[u8; 32]) -> Result<String, String> {
    let (ciphertext_b64, nonce_b64) = encrypt(key, VERIFIER_PLAINTEXT)?;
    Ok(format!("{}:{}", nonce_b64, ciphertext_b64))
}

/// Verify a password by decrypting the verifier blob.
///
/// Three-state: `Ok(true)` for a correct password, `Ok(false)` only for a
/// well-formed verifier that fails AEAD authentication with the given key
/// (wrong password — cryptographically indistinguishable from tampering),
/// and `Err` for anything structurally malformed (bad `nonce:ciphertext`
/// shape, invalid base64, wrong nonce length). Callers must treat `Ok(false)`
/// and `Err` differently: `Ok(false)` means "try again"; `Err` means "the
/// vault is corrupted" and must not be counted against a brute-force lockout.
pub fn verify_password(key: &[u8; 32], verifier: &str) -> Result<bool, String> {
    let parts: Vec<&str> = verifier.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err("Invalid verifier format".into());
    }
    let nonce_b64 = parts[0];
    let ciphertext_b64 = parts[1];

    let nonce_bytes = B64
        .decode(nonce_b64)
        .map_err(|e| format!("Bad nonce base64: {e}"))?;
    if nonce_bytes.len() != AES_GCM_NONCE_LEN {
        return Err(format!(
            "Bad nonce length: {} (expected {AES_GCM_NONCE_LEN})",
            nonce_bytes.len()
        ));
    }
    let ciphertext = B64
        .decode(ciphertext_b64)
        .map_err(|e| format!("Bad ciphertext base64: {e}"))?;

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    match cipher.decrypt(nonce, ciphertext.as_ref()) {
        Ok(plaintext) => Ok(plaintext == VERIFIER_PLAINTEXT),
        // Every failure reaching this point is now an AEAD authentication
        // failure (wrong key / tampered ciphertext, cryptographically
        // indistinguishable from each other) — the only remaining path is
        // "wrong password".
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verifier_roundtrip() {
        let key = [0u8; 32];
        let verifier = create_verifier(&key).unwrap();
        assert!(verify_password(&key, &verifier).unwrap());
    }

    #[test]
    fn test_verifier_wrong_password() {
        let key1 = [0u8; 32];
        let key2 = [1u8; 32];
        let verifier = create_verifier(&key1).unwrap();
        assert!(!verify_password(&key2, &verifier).unwrap());
    }

    #[test]
    fn wrong_key_stays_ok_false_not_err() {
        // A well-formed verifier decrypted with the wrong (but well-formed)
        // key fails AEAD authentication and must stay the normal `Ok(false)`
        // "wrong password" path, not escalate to `Err`.
        let key1 = [0u8; 32];
        let key2 = [1u8; 32];
        let verifier = create_verifier(&key1).unwrap();
        assert_eq!(verify_password(&key2, &verifier), Ok(false));
    }

    #[test]
    fn corrupted_verifier_is_err_not_wrong_password() {
        // Right "nonce:ciphertext" shape, but the base64 fields themselves
        // are corrupted. This is vault corruption, not a wrong password —
        // callers (lockout counters) must be able to tell the two apart.
        let key = [0u8; 32];
        let verifier = "not-valid-base64!!!:also-not-valid-base64!!!";
        let result = verify_password(&key, verifier);
        assert!(
            result.is_err(),
            "corrupted verifier fields should be Err (corruption), got {result:?}"
        );
    }

    #[test]
    fn verifier_with_bad_nonce_length_is_err_not_wrong_password() {
        let key = [0u8; 32];
        let short_nonce_b64 = B64.encode([0u8; 8]);
        let ciphertext_b64 = B64.encode(b"irrelevant-ciphertext-bytes");
        let verifier = format!("{short_nonce_b64}:{ciphertext_b64}");
        let err = verify_password(&key, &verifier)
            .expect_err("bad nonce length in a verifier is corruption, not a wrong password");
        assert!(err.contains("Bad nonce length"), "{err}");
    }

    #[test]
    fn missing_colon_is_err() {
        let key = [0u8; 32];
        let err = verify_password(&key, "no-colon-here").expect_err("must reject malformed shape");
        assert!(err.contains("Invalid verifier format"), "{err}");
    }

    /// Golden vector: fixed key + verifier string, contract-anchoring the
    /// verifier's `nonce_b64:ciphertext_b64` wire format.
    #[test]
    fn golden_vector_verifier_matches_pinned_key() {
        let key: [u8; 32] = [
            0xa0, 0x59, 0xf9, 0x0c, 0x88, 0x70, 0x71, 0xc3, 0x27, 0x18, 0x48, 0x0f, 0x10, 0xb0,
            0x78, 0x5f, 0xb0, 0xa0, 0x97, 0xc8, 0x57, 0x38, 0x8a, 0x15, 0x82, 0x71, 0xd5, 0x07,
            0xc8, 0x09, 0xeb, 0xd4,
        ];
        let verifier = "DA0ODxAREhMUFRYX:tIwb3wSWBIbpCyBQ/AFauWbOzF+EefFdjXDXBeay";
        assert_eq!(verify_password(&key, verifier), Ok(true));

        let wrong_key: [u8; 32] = [
            0xb1, 0x07, 0xa6, 0x8e, 0x95, 0x81, 0x9c, 0x28, 0xb7, 0xf6, 0x65, 0x82, 0x1b, 0x81,
            0x66, 0x00, 0x17, 0x24, 0x24, 0x85, 0x2c, 0x45, 0x7b, 0x09, 0xa3, 0x86, 0x4c, 0x7a,
            0x11, 0xd0, 0x29, 0x24,
        ];
        assert_eq!(verify_password(&wrong_key, verifier), Ok(false));
    }
}
