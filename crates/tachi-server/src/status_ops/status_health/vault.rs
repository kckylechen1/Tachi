use std::path::Path;

pub(crate) fn load_keychain_vault_api_key_values(
    vault_db_path: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    if !cfg!(target_os = "macos") {
        return Ok(Vec::new());
    }

    use base64::{engine::general_purpose::STANDARD as B64, Engine};

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

    let salt = B64.decode(&config.salt)?;
    let key = crate::vault_crypto::DerivedVaultKey::derive(&password, &salt)?;
    if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)? {
        return Ok(Vec::new());
    }

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
