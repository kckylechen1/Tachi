use super::password::{read_vault_init_password, read_vault_password};
use super::{open_cli_store, open_cli_store_read_only};
use std::path::{Path, PathBuf};

pub(super) fn vault_config_exists_cli(
    global_db_path: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !global_db_path.exists() {
        return Ok(false);
    }
    let store = open_cli_store_read_only(&global_db_path.to_path_buf())?;
    Ok(store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .is_some())
}

pub(super) fn read_vault_config_for_key(
    global_db_path: &PathBuf,
) -> Result<memcore::vault::VaultConfig, Box<dyn std::error::Error>> {
    let store = open_cli_store_read_only(global_db_path)?;
    store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .ok_or_else(|| "Vault not initialized. Run `tachi vault init` first.".into())
}

pub(in crate::bootstrap) fn read_verified_vault_key(
    config: &memcore::vault::VaultConfig,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<crate::vault_crypto::DerivedVaultKey, Box<dyn std::error::Error>> {
    let mut password = read_vault_password(
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )?;
    derive_verified_vault_key_from_password(config, &mut password)
}

pub(super) fn derive_verified_vault_key_from_password(
    config: &memcore::vault::VaultConfig,
    password: &mut String,
) -> Result<crate::vault_crypto::DerivedVaultKey, Box<dyn std::error::Error>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    let key_result = crate::vault_crypto::DerivedVaultKey::derive(password, &salt);
    crate::vault_crypto::zero_string(password);
    let key = key_result?;
    if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)? {
        return Err("Wrong password".into());
    }
    Ok(key)
}

/// Initialize the vault from a plaintext master password, reusing the exact
/// crypto used by `VaultAction::Init`. Returns the derived key so the caller can
/// immediately upsert secrets without re-deriving. The password is zeroed.
///
/// Callers MUST verify the vault is not already initialized before calling this.
pub(in crate::bootstrap) fn vault_init_with_password(
    global_db_path: &PathBuf,
    mut password: String,
) -> Result<crate::vault_crypto::DerivedVaultKey, Box<dyn std::error::Error>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let salt = crate::vault_crypto::generate_salt();
    let key_result = crate::vault_crypto::DerivedVaultKey::derive(&password, &salt);
    crate::vault_crypto::zero_string(&mut password);
    let key = key_result?;
    let verifier = crate::vault_crypto::create_verifier(key.bytes())?;
    let salt_b64 = B64.encode(salt);
    let now = chrono::Utc::now().to_rfc3339();

    let store = open_cli_store(global_db_path)?;
    store
        .vault_set_config(&memcore::vault::VaultConfig {
            salt: salt_b64,
            verifier,
            kdf_algorithm: "argon2id".to_string(),
            kdf_params: crate::vault_crypto::active_kdf_params_json().to_string(),
            cipher: memcore::vault::VaultCipher::Aes256Gcm,
            created_at: now.clone(),
            updated_at: now,
        })
        .map_err(|e| format!("vault_set_config: {e}"))?;
    Ok(key)
}

/// Encrypt + upsert a single secret using an already-derived vault key, reusing
/// the same crypto/entry shape as `VaultAction::Set`. `secret_value` is zeroed.
/// Returns true when a new entry was created (vs. updating an existing one).
pub(in crate::bootstrap) fn vault_upsert_secret_with_key(
    global_db_path: &PathBuf,
    key: &crate::vault_crypto::DerivedVaultKey,
    name: &str,
    secret_type: &str,
    description: &str,
    mut secret_value: String,
) -> Result<bool, Box<dyn std::error::Error>> {
    crate::vault_crypto::validate_secret_name(name)?;
    if secret_value.is_empty() {
        return Err("Secret value cannot be empty".into());
    }

    let encrypt_result = crate::vault_crypto::encrypt(key.bytes(), secret_value.as_bytes());
    crate::vault_crypto::zero_string(&mut secret_value);
    let (encrypted_value, nonce) = encrypt_result?;

    let is_new = !open_cli_store_read_only(global_db_path)?
        .vault_entry_exists(name)
        .map_err(|e| format!("vault_entry_exists: {e}"))?;

    let now = chrono::Utc::now().to_rfc3339();
    let entry = memcore::vault::VaultEntry {
        name: name.to_string(),
        encrypted_value,
        nonce,
        secret_type: secret_type.to_string(),
        description: description.to_string(),
        allowed_agents: None,
        created_at: if is_new { now.clone() } else { String::new() },
        updated_at: now,
        accessed_at: String::new(),
        access_count: 0,
    };

    let store = open_cli_store(global_db_path)?;
    store
        .vault_upsert_entry(&entry)
        .map_err(|e| format!("vault_upsert_entry: {e}"))?;
    Ok(is_new)
}

/// Canonical provider API keys (deduped, in declaration order) used by both the
/// setup wizard and `tachi vault setup-keys`. Reuses the shared `API_KEY_DEFS`
/// list so the funnel stays aligned with `tachi status`.
pub(super) fn canonical_provider_key_defs(
    include_deprecated: bool,
) -> Vec<(&'static str, &'static str)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for def in crate::status_ops::status_health::API_KEY_DEFS {
        if def.deprecated && !include_deprecated {
            continue;
        }
        if seen.insert(def.key) {
            out.push((def.key, def.label));
        }
    }
    out
}

/// Bulk key-entry: iterate the canonical provider keys, prompt for each via
/// rpassword (blank skips), and upsert non-empty values encrypted. Initializes
/// the vault first when needed, reusing existing init/upsert helpers.
pub(super) fn run_vault_setup_keys(
    global_db_path: &PathBuf,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    confirm_password_file: Option<&Path>,
    insecure_password_file: bool,
    include_deprecated: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let existing_config = if vault_config_exists_cli(global_db_path)? {
        Some(read_vault_config_for_key(global_db_path)?)
    } else {
        None
    };

    let key = if let Some(config) = existing_config {
        println!("Vault already initialized; unlocking to add provider keys.");
        read_verified_vault_key(
            &config,
            stdin_password,
            keychain,
            password_file,
            insecure_password_file,
        )?
    } else {
        println!("No vault found; initializing a new master-password vault.");
        let password = read_vault_init_password(
            stdin_password,
            keychain,
            password_file,
            confirm_password_file,
            insecure_password_file,
        )?;
        vault_init_with_password(global_db_path, password)?
    };

    let defs = canonical_provider_key_defs(include_deprecated);
    let mut stored = 0usize;
    let mut skipped = 0usize;
    println!(
        "Enter values for {} provider key(s). Press Enter on a blank line to skip a key.",
        defs.len()
    );
    for (name, label) in defs {
        let value = rpassword::prompt_password(format!("{name} ({label}) [blank=skip]: "))?;
        let value = value.trim().to_string();
        if value.is_empty() {
            skipped += 1;
            continue;
        }
        let is_new =
            vault_upsert_secret_with_key(global_db_path, &key, name, "api_key", "", value)?;
        // Never print the secret value; only the name and create/update status.
        println!("  {name}: {}", if is_new { "stored" } else { "updated" });
        stored += 1;
    }

    println!("Vault setup-keys complete: {stored} stored/updated, {skipped} skipped.");
    if stored > 0 {
        println!(
            "Tip: reference these in ~/.tachi/config.env as `KEY=vault:KEY` (e.g. {}).",
            crate::provider_config::vault_alias_line("VOYAGE_API_KEY")
        );
    }
    Ok(())
}

pub(super) fn decrypt_profile_secret_values(
    global_db_path: &PathBuf,
    profile: &crate::credential_profile::CredentialProfile,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&Path>,
    insecure_password_file: bool,
) -> Result<std::collections::HashMap<String, String>, Box<dyn std::error::Error>> {
    let store = open_cli_store_read_only(global_db_path)?;
    let config = store
        .vault_get_config()
        .map_err(|e| format!("vault_get_config: {e}"))?
        .ok_or("Vault not initialized. Run `tachi vault init` first.")?;
    let key = read_verified_vault_key(
        &config,
        stdin_password,
        keychain,
        password_file,
        insecure_password_file,
    )?;

    let mut values = std::collections::HashMap::new();
    for name in crate::credential_profile::profile_secret_names(profile) {
        let entry = store
            .vault_get_entry(&name)
            .map_err(|e| format!("vault_get_entry: {e}"))?
            .ok_or_else(|| format!("Vault secret '{name}' is missing"))?;
        let decrypted =
            crate::vault_crypto::decrypt(key.bytes(), &entry.encrypted_value, &entry.nonce)?;
        let value = String::from_utf8(decrypted)
            .map_err(|e| format!("Vault secret '{name}' is not valid UTF-8: {e}"))?;
        values.insert(name, value);
    }
    Ok(values)
}
