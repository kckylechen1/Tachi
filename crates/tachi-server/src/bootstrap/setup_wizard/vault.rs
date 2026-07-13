use std::error::Error;
use std::path::PathBuf;

use dialoguer::theme::ColorfulTheme;
use dialoguer::Password;

use super::super::{open_cli_store, open_cli_store_read_only, vault_cli};

/// Inline vault initializer used by step 5. Mirrors `vault_cli::VaultAction::Init`
/// but keeps the wizard self-contained; on any error we surface it to the caller
/// so the wizard can continue gracefully.
pub(super) fn init_vault_inline(
    global_db_path: &PathBuf,
    theme: &ColorfulTheme,
) -> Result<(), Box<dyn Error>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    if global_db_path.exists() {
        let store = open_cli_store_read_only(global_db_path)?;
        if store
            .vault_get_config()
            .map_err(|e| format!("vault_get_config: {e}"))?
            .is_some()
        {
            return Err("vault already initialized".into());
        }
        drop(store);
    }

    let password = Password::with_theme(theme)
        .with_prompt("    New vault password")
        .with_confirmation("    Confirm password", "    Passwords do not match")
        .interact()?;
    if password.is_empty() {
        return Err("password cannot be empty".into());
    }

    let salt = crate::vault_crypto::generate_salt();
    let key = crate::vault_crypto::DerivedVaultKey::derive(&password, &salt)?;
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

    Ok(())
}

/// Store the freshly-entered API keys (`key_names`) in the encrypted vault and
/// rewrite their `new_entries` rows from plaintext `KEY=VALUE` to the
/// `KEY=vault:KEY` alias convention, so `config.env` never persists the secret
/// value. Reuses the existing vault init + upsert crypto from `vault_cli`.
///
/// On a fresh vault we prompt for a new master password; on an existing vault we
/// prompt for the current password to unlock. Returns the number of keys stored.
pub(super) fn store_collected_keys_in_vault(
    global_db_path: &PathBuf,
    vault_already: bool,
    key_names: &[String],
    new_entries: &mut [(String, String)],
    theme: &ColorfulTheme,
) -> Result<usize, Box<dyn Error>> {
    // Derive the vault key: init a new vault, or unlock the existing one.
    let key = if vault_already {
        let config = open_cli_store_read_only(global_db_path)?
            .vault_get_config()
            .map_err(|e| format!("vault_get_config: {e}"))?
            .ok_or("vault reported initialized but config is missing")?;
        let mut password = Password::with_theme(theme)
            .with_prompt("    Current vault password")
            .interact()?;
        if password.is_empty() {
            crate::vault_crypto::zero_string(&mut password);
            return Err("password cannot be empty".into());
        }
        derive_verified_vault_key_for_wizard(&config, &mut password)?
    } else {
        let password = Password::with_theme(theme)
            .with_prompt("    New vault password")
            .with_confirmation("    Confirm password", "    Passwords do not match")
            .interact()?;
        if password.is_empty() {
            return Err("password cannot be empty".into());
        }
        vault_cli::vault_init_with_password(global_db_path, password)?
    };

    upsert_keys_and_rewrite_aliases(global_db_path, &key, key_names, new_entries)
}

/// For each entry whose key is in `key_names`, move its plaintext value into the
/// vault (encrypted) and replace the `new_entries` row with the non-secret
/// `vault:KEY` alias so `config.env` never persists the value. Returns the count
/// stored. Split out so it can be unit-tested with a pre-derived key (no prompt).
pub(super) fn upsert_keys_and_rewrite_aliases(
    global_db_path: &PathBuf,
    key: &crate::vault_crypto::DerivedVaultKey,
    key_names: &[String],
    new_entries: &mut [(String, String)],
) -> Result<usize, Box<dyn Error>> {
    let mut stored = 0usize;
    for (name, value) in new_entries.iter_mut() {
        if !key_names.iter().any(|k| k == name) {
            continue;
        }
        let secret_value = value.clone();
        vault_cli::vault_upsert_secret_with_key(
            global_db_path,
            key,
            name,
            "api_key",
            "",
            secret_value,
        )?;
        crate::vault_crypto::zero_string(value);
        *value = format!("{}{}", crate::provider_config::VAULT_ALIAS_PREFIX, name);
        stored += 1;
    }
    Ok(stored)
}

/// Derive + verify a vault key from a plaintext password, zeroing the password.
/// Mirrors `vault_cli::derive_verified_vault_key_from_password` (which is
/// private to that module) while keeping the wizard self-contained.
fn derive_verified_vault_key_for_wizard(
    config: &memcore::vault::VaultConfig,
    password: &mut String,
) -> Result<crate::vault_crypto::DerivedVaultKey, Box<dyn Error>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    // tachi#1080: derive using the STORED `vault_config.kdf_params`, not a
    // compile-time constant (mirrors vault_cli::derive_verified_vault_key_from_password).
    // A malformed or unsupported stored value fails loud and versioned before
    // `verify_password`, so it is never misread as "Wrong password".
    //
    // The parse error is propagated as a TYPED `KdfParamsFormatError` (NOT
    // stringified) so the wizard's outer catch-all in wizard.rs can
    // `downcast_ref` and abort instead of falling through to the
    // plaintext-config.env fallback. The derive error stays stringified (its
    // existing behavior). `zero_string(password)` still runs on BOTH paths
    // (the match builds the result without early-returning; zero runs after).
    let key_result: Result<crate::vault_crypto::DerivedVaultKey, Box<dyn std::error::Error>> =
        match crate::vault_crypto::parse_stored_kdf_params(&config.kdf_params) {
            Ok(params) => crate::vault_crypto::DerivedVaultKey::derive_with_params(password, &salt, &params)
                .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string())),
            Err(err) => Err(Box::<dyn std::error::Error>::from(err)),
        };
    crate::vault_crypto::zero_string(password);
    let key = key_result?;
    if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)? {
        return Err("Wrong password".into());
    }
    Ok(key)
}
