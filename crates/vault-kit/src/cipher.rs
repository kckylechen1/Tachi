//! AES-256-GCM encrypt/decrypt. Moved verbatim from
//! `tachi-server/src/vault_crypto.rs` (tachi#1080 v1) — same signatures,
//! same nonce-length guard (`AES_GCM_NONCE_LEN`) the fork was missing before
//! Hyperion-HyperTachi@84ed97a5 (a malformed nonce reaching
//! `Nonce::from_slice` directly panics; a corrupted vault row must not be
//! able to take a daemon down).

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;

pub const AES_GCM_NONCE_LEN: usize = 12;

/// Generate a random 12-byte AES-GCM nonce.
pub fn generate_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    nonce
}

/// Encrypt plaintext with AES-256-GCM. Returns (ciphertext_b64, nonce_b64).
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<(String, String), String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce_bytes = generate_nonce();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("Encryption failed: {e}"))?;
    Ok((B64.encode(&ciphertext), B64.encode(nonce_bytes)))
}

/// Decrypt ciphertext (base64) with AES-256-GCM.
pub fn decrypt(key: &[u8; 32], ciphertext_b64: &str, nonce_b64: &str) -> Result<Vec<u8>, String> {
    let ciphertext = B64
        .decode(ciphertext_b64)
        .map_err(|e| format!("Bad ciphertext base64: {e}"))?;
    let nonce_bytes = B64
        .decode(nonce_b64)
        .map_err(|e| format!("Bad nonce base64: {e}"))?;
    if nonce_bytes.len() != AES_GCM_NONCE_LEN {
        return Err(format!(
            "Bad nonce length: {} (expected {AES_GCM_NONCE_LEN})",
            nonce_bytes.len()
        ));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|e| format!("Decryption failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = [0u8; 32];
        let plaintext = b"Hello, World!";
        let (ciphertext_b64, nonce_b64) = encrypt(&key, plaintext).unwrap();
        let decrypted = decrypt(&key, &ciphertext_b64, &nonce_b64).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_decrypt_wrong_key() {
        let key1 = [0u8; 32];
        let key2 = [1u8; 32];
        let plaintext = b"Secret message";
        let (ciphertext_b64, nonce_b64) = encrypt(&key1, plaintext).unwrap();
        let result = decrypt(&key2, &ciphertext_b64, &nonce_b64);
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_rejects_bad_nonce_length() {
        let err = decrypt(&[0u8; 32], "", &B64.encode([0u8; 11]))
            .expect_err("bad nonce length should be rejected before AEAD decrypt");
        assert!(err.contains("Bad nonce length"), "{err}");
    }

    #[test]
    fn decrypt_rejects_short_nonce_without_panicking() {
        // Same guard, exercised the way a corrupted DB row would trigger it:
        // valid base64, wrong-length raw nonce. Before this guard existed
        // (the bug Hyperion-HyperTachi@84ed97a5 fixed downstream) this
        // reached `Nonce::from_slice` directly and panicked.
        let key = [0u8; 32];
        let short_nonce_b64 = B64.encode([0u8; 8]);
        let ciphertext_b64 = B64.encode(b"irrelevant-ciphertext-bytes");
        let err = decrypt(&key, &ciphertext_b64, &short_nonce_b64)
            .expect_err("short nonce must be rejected, not panic");
        assert!(err.contains("Bad nonce length"), "{err}");
    }

    /// Golden vector: fixed key/nonce/plaintext, hardcoded expected
    /// ciphertext. Pins the AES-256-GCM wire format (nonce length, tag
    /// placement, base64 alphabet) as a cross-product contract anchor — any
    /// accidental format change breaks this loudly instead of only being
    /// caught by a same-run roundtrip that can't detect drift.
    #[test]
    fn golden_vector_decrypt_matches_pinned_ciphertext() {
        let key: [u8; 32] = [
            0xa0, 0x59, 0xf9, 0x0c, 0x88, 0x70, 0x71, 0xc3, 0x27, 0x18, 0x48, 0x0f, 0x10, 0xb0,
            0x78, 0x5f, 0xb0, 0xa0, 0x97, 0xc8, 0x57, 0x38, 0x8a, 0x15, 0x82, 0x71, 0xd5, 0x07,
            0xc8, 0x09, 0xeb, 0xd4,
        ];
        let nonce_b64 = "AAECAwQFBgcICQoL";
        let ciphertext_b64 = "ZE3sl1Q0g3DHDxzfF4W/+r+VmdF6Rq1RBcCdygCg7/gJVLrrlxp2OX9vdw5n7H9hZ/dY5fUUxyrRTim2O8wyxAsAYdu0dfAsGg==";
        let expected_plaintext =
            b"vault-kit golden plaintext: crypto+format contract anchor".to_vec();

        let decrypted = decrypt(&key, ciphertext_b64, nonce_b64)
            .expect("pinned golden vector must decrypt with vault-kit's own cipher");
        assert_eq!(decrypted, expected_plaintext);
    }
}
