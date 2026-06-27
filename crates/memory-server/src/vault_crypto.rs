// vault_crypto.rs — encryption primitives for Tachi Vault

use aes_gcm::{
    aead::{Aead, AeadInPlace, KeyInit},
    Aes256Gcm, Key, Nonce, Tag,
};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;

const VERIFIER_PLAINTEXT: &[u8] = b"tachi-vault-ok";
const AES_GCM_NONCE_LEN: usize = 12;
const AES_GCM_TAG_LEN: usize = 16;
#[cfg(not(test))]
const PRODUCTION_KDF_MEMORY_COST: u32 = 65_536;
#[cfg(not(test))]
const PRODUCTION_KDF_TIME_COST: u32 = 3;
#[cfg(not(test))]
const PRODUCTION_KDF_PARALLELISM: u32 = 4;
#[cfg(not(test))]
const PRODUCTION_KDF_PARAMS_JSON: &str = r#"{"m":65536,"t":3,"p":4}"#;

#[cfg(test)]
const TEST_KDF_MEMORY_COST: u32 = 64;
#[cfg(test)]
const TEST_KDF_TIME_COST: u32 = 1;
#[cfg(test)]
const TEST_KDF_PARALLELISM: u32 = 1;
#[cfg(test)]
const TEST_KDF_PARAMS_JSON: &str = r#"{"m":64,"t":1,"p":1}"#;

#[derive(Clone, Copy)]
struct VaultKdfParams {
    memory_cost: u32,
    time_cost: u32,
    parallelism: u32,
}

fn active_kdf_params() -> VaultKdfParams {
    #[cfg(test)]
    {
        VaultKdfParams {
            memory_cost: TEST_KDF_MEMORY_COST,
            time_cost: TEST_KDF_TIME_COST,
            parallelism: TEST_KDF_PARALLELISM,
        }
    }
    #[cfg(not(test))]
    {
        VaultKdfParams {
            memory_cost: PRODUCTION_KDF_MEMORY_COST,
            time_cost: PRODUCTION_KDF_TIME_COST,
            parallelism: PRODUCTION_KDF_PARALLELISM,
        }
    }
}

pub(crate) fn active_kdf_params_json() -> &'static str {
    #[cfg(test)]
    {
        TEST_KDF_PARAMS_JSON
    }
    #[cfg(not(test))]
    {
        PRODUCTION_KDF_PARAMS_JSON
    }
}

/// Derive a 32-byte encryption key from password + salt using Argon2id.
fn derive_key_into(password: &str, salt: &[u8], key: &mut [u8; 32]) -> Result<(), String> {
    let kdf = active_kdf_params();
    let params = Params::new(kdf.memory_cost, kdf.time_cost, kdf.parallelism, Some(32))
        .map_err(|e| format!("Argon2 params error: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon2
        .hash_password_into(password.as_bytes(), salt, key)
        .map_err(|e| format!("Key derivation failed: {e}"))?;
    Ok(())
}

pub struct DerivedVaultKey {
    bytes: [u8; 32],
}

impl DerivedVaultKey {
    pub fn derive(password: &str, salt: &[u8]) -> Result<Self, String> {
        let mut bytes = [0u8; 32];
        derive_key_into(password, salt, &mut bytes)?;
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
    let bytes = unsafe { value.as_mut_vec() };
    zero_bytes(bytes);
}

fn zero_bytes(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
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

/// Generate a random 12-byte nonce.
fn generate_nonce() -> [u8; 12] {
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

/// Create the verifier blob (encrypt known plaintext). Format: "nonce_b64:ciphertext_b64"
pub fn create_verifier(key: &[u8; 32]) -> Result<String, String> {
    let (ciphertext_b64, nonce_b64) = encrypt(key, VERIFIER_PLAINTEXT)?;
    Ok(format!("{}:{}", nonce_b64, ciphertext_b64))
}

/// Verify a password by decrypting the verifier blob.
///
/// Returns `Ok(true)` for correct password, `Ok(false)` for wrong password,
/// and `Err` only for malformed input (e.g. invalid verifier format).
/// This is intentional: callers should treat Ok(false) and Err differently —
/// Ok(false) means "try again", Err means "the vault is corrupted".
pub fn verify_password(key: &[u8; 32], verifier: &str) -> Result<bool, String> {
    let parts: Vec<&str> = verifier.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err("Invalid verifier format".into());
    }
    // Decrypt with the given key - if it fails, the key is wrong
    match decrypt(key, parts[1], parts[0]) {
        Ok(plaintext) => Ok(plaintext == VERIFIER_PLAINTEXT),
        Err(_) => Ok(false),
    }
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
    fn test_authentication_detects_tampered_aad() {
        let key = [7u8; 32];
        let aad = br#"{"bundle":"one"}"#;
        let (tag, nonce) = authenticate(&key, aad).unwrap();

        verify_authentication(&key, aad, &nonce, &tag).unwrap();
        assert!(verify_authentication(&key, br#"{"bundle":"two"}"#, &nonce, &tag).is_err());
        assert!(verify_authentication(&[8u8; 32], aad, &nonce, &tag).is_err());
    }

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
    fn test_validate_secret_name() {
        assert!(validate_secret_name("VALID_KEY").is_ok());
        assert!(validate_secret_name("valid.key").is_ok());
        assert!(validate_secret_name("valid_key_123").is_ok());
        assert!(validate_secret_name("invalid key").is_err());
        assert!(validate_secret_name("invalid-key!").is_err());
        assert!(validate_secret_name("").is_err());
        assert!(validate_secret_name("a".repeat(129).as_str()).is_err());
    }
}
