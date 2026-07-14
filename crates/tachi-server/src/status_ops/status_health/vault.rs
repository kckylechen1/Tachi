use std::path::Path;

fn derive_status_vault_key(
    config: &memcore::vault::VaultConfig,
    password: &str,
) -> Result<
    Option<crate::vault_crypto::DerivedVaultKey>,
    crate::vault_crypto::StoredVaultKeyDerivationError,
> {
    match crate::vault_crypto::derive_verified_key_from_stored_config(config, password) {
        Ok(key) => Ok(Some(key)),
        Err(crate::vault_crypto::StoredVaultKeyDerivationError::WrongPassword) => Ok(None),
        Err(err) => Err(err),
    }
}

pub(crate) fn load_keychain_vault_api_key_values(
    vault_db_path: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    if !cfg!(target_os = "macos") {
        return Ok(Vec::new());
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
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }

    let password = String::from_utf8(output.stdout)?.trim().to_string();
    if password.is_empty() {
        return Ok(Vec::new());
    }

    let vault_db_str = vault_db_path.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Vault DB path contains invalid UTF-8: {}",
                vault_db_path.display()
            ),
        )
    })?;
    let store = memcore::MemoryStore::open_read_only(vault_db_str)?;
    let Some(config) = store.vault_get_config()? else {
        return Ok(Vec::new());
    };

    // tachi#1080: a wrong Keychain password is the sole benign miss. Invalid
    // salt, KDF format/parameters, derivation failure, and verifier corruption
    // are stored-config integrity failures and must stay loud.
    let key = match derive_status_vault_key(&config, &password)? {
        Some(key) => key,
        None => {
            return Ok(Vec::new());
        }
    };

    let mut out = Vec::new();
    for entry in store.vault_list_entries()? {
        let is_provider_key = entry.name.ends_with("_API_KEY")
            || crate::provider_config::parse_rotation_member_name(&entry.name)
                .is_some_and(|(prefix, _)| prefix.ends_with("_API_KEY"));
        if entry.secret_type != "api_key" || !is_provider_key || entry.allowed_agents.is_some() {
            continue;
        }
        let decrypted =
            crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)?;
        if !value.trim().is_empty() {
            out.push((entry.name, value));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use memcore::vault::{VaultCipher, VaultConfig};

    fn stored_config(password: &str) -> VaultConfig {
        let salt = crate::vault_crypto::generate_salt();
        let key = crate::vault_crypto::DerivedVaultKey::derive(password, &salt).expect("derive");
        let verifier = crate::vault_crypto::create_verifier(key.bytes()).expect("verifier");
        VaultConfig {
            salt: B64.encode(salt),
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: crate::vault_crypto::active_kdf_params_json().to_string(),
            cipher: VaultCipher::Aes256Gcm,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn status_wrong_keychain_password_is_a_benign_miss() {
        let config = stored_config("correct-pw");
        let result = derive_status_vault_key(&config, "wrong-pw")
            .expect("wrong Keychain password must not fail status health");
        assert!(result.is_none(), "wrong password must map to empty status");
    }

    #[test]
    fn status_invalid_stored_salt_stays_loud() {
        let mut config = stored_config("correct-pw");
        config.salt = "not base64!".to_string();
        let err = derive_status_vault_key(&config, "correct-pw")
            .expect_err("invalid stored salt must fail status health");
        assert!(
            matches!(
                err,
                crate::vault_crypto::StoredVaultKeyDerivationError::InvalidSalt(_)
            ),
            "invalid salt must not degrade to empty status"
        );
    }

    #[test]
    fn status_corrupt_stored_verifier_stays_loud() {
        let mut config = stored_config("correct-pw");
        config.verifier = "not-a-verifier".to_string();
        let err = derive_status_vault_key(&config, "correct-pw")
            .expect_err("corrupt stored verifier must fail status health");
        assert!(
            matches!(
                err,
                crate::vault_crypto::StoredVaultKeyDerivationError::CorruptVerifier(_)
            ),
            "corrupt verifier must not degrade to empty status"
        );
    }
}
