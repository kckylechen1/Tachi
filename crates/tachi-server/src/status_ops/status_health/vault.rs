use std::path::Path;

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

    // tachi#1080: derive+verify through the shared in-process seam. A stored
    // kdf_params format failure stays loud (Err) so the status-health consumer
    // warns with the versioned message; a password mismatch / derivation
    // failure degrades to an empty Vec (the pre-seam contract: a wrong
    // keychain password returned Ok(empty), never an error to the caller).
    let key = match crate::vault_crypto::derive_verified_key_from_stored_config(&config, &password) {
        Ok(key) => key,
        Err(err) => {
            if err
                .downcast_ref::<crate::vault_crypto::KdfParamsFormatError>()
                .is_some()
            {
                return Err(err);
            }
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
