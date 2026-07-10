//! Provider API key resolution: Vault, config.env `vault:` aliases, and env fallbacks.
//!
//! Single path for daemon, MCP, CLI backfill, and vector sweep so background jobs
//! do not re-implement Keychain/Vault reads with different behavior.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::vault_ops::load_unlocked_api_key_secret_pools;
use crate::MemoryServer;
pub use tachi_llm::{
    group_api_key_values_by_configured_rotations, is_vault_alias, parse_rotation_member_name,
    parse_vault_alias, vault_alias_line, MaterializeReport, VAULT_ALIAS_PREFIX,
};
use tachi_llm::{LlmClient, ProviderSecret};

pub(crate) fn provider_env_keys() -> HashSet<String> {
    crate::status_ops::status_health::provider_api_key_env_names()
}

/// Load API keys from an unlocked in-process Vault session.
pub fn vault_api_key_pools_from_server(
    server: &MemoryServer,
) -> Result<HashMap<String, Vec<ProviderSecret>>, String> {
    load_unlocked_api_key_secret_pools(server)
}

/// Load API keys via macOS Keychain + global DB (daemon/CLI when memory unlock is empty).
pub fn vault_api_key_pools_from_keychain(
    global_db_path: &Path,
) -> HashMap<String, Vec<ProviderSecret>> {
    let rotation_prefixes = rotation_prefixes_from_global_db(global_db_path);
    group_api_key_values_by_configured_rotations(
        crate::status_ops::status_health::load_keychain_vault_api_key_values(global_db_path)
            .unwrap_or_default(),
        &rotation_prefixes,
    )
}

fn resolve_vault_pools(
    server: Option<&MemoryServer>,
    global_db_path: &Path,
) -> HashMap<String, Vec<ProviderSecret>> {
    if let Some(server) = server {
        if let Ok(map) = vault_api_key_pools_from_server(server) {
            if !map.is_empty() {
                return map;
            }
        }
    }
    let pools = vault_api_key_pools_from_keychain(global_db_path);
    if !pools.is_empty() || vault_config_exists(global_db_path) {
        return pools;
    }

    let default_global = default_global_db_path();
    if paths_equal(global_db_path, &default_global) {
        return pools;
    }

    let fallback = vault_api_key_pools_from_keychain(&default_global);
    if !fallback.is_empty() {
        tracing::warn!(
            "[provider] global DB {} has no initialized Vault; using default Vault DB {} for provider key materialization",
            global_db_path.display(),
            default_global.display()
        );
    }
    fallback
}

fn default_global_db_path() -> std::path::PathBuf {
    crate::status_ops::resolve_app_home()
        .join("global")
        .join("memory.db")
}

fn vault_config_exists(global_db_path: &Path) -> bool {
    let Some(path) = global_db_path.to_str() else {
        return false;
    };
    let Ok(store) = memcore::MemoryStore::open_read_only(path) else {
        return false;
    };
    store.vault_get_config().ok().flatten().is_some()
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    std::fs::canonicalize(left)
        .ok()
        .zip(std::fs::canonicalize(right).ok())
        .map(|(left, right)| left == right)
        .unwrap_or(false)
}

fn rotation_prefixes_from_global_db(global_db_path: &Path) -> HashSet<String> {
    let Some(path) = global_db_path.to_str() else {
        return HashSet::new();
    };
    let Ok(store) = memcore::MemoryStore::open_read_only(path) else {
        return HashSet::new();
    };
    store
        .vault_list_rotations()
        .unwrap_or_default()
        .into_iter()
        .map(|rotation| rotation.prefix)
        .collect()
}

/// Apply Vault + config.env aliases into `LlmClient` without mutating process env.
pub fn materialize_provider_secrets(
    llm: &LlmClient,
    vault_pools: &HashMap<String, Vec<ProviderSecret>>,
) -> Result<MaterializeReport, String> {
    tachi_llm::materialize_provider_secrets(llm, vault_pools, provider_env_keys())
        .map_err(format_provider_materialization_error)
}

fn format_provider_materialization_error(err: String) -> String {
    if (err.starts_with("provider alias ") && err.contains(" could not be resolved from secret "))
        || (err.starts_with("Config key ") && err.contains("references Vault alias "))
    {
        format!(
            "{err}. The alias came from config.env or process env, but the referenced Vault secret is missing or Vault is locked. \
             Run vault_unlock and vault_set, or store the key in Vault under the referenced secret name."
        )
    } else {
        err
    }
}

pub fn materialize_for_server(server: &MemoryServer) -> Result<MaterializeReport, String> {
    let global = server.global_db_path_buf();
    let vault_pools = resolve_vault_pools(Some(server), &global);
    materialize_provider_secrets(server.llm.as_ref(), &vault_pools)
}

pub fn materialize_standalone(
    llm: &LlmClient,
    global_db_path: &Path,
) -> Result<MaterializeReport, String> {
    let vault_pools = resolve_vault_pools(None, global_db_path);
    materialize_provider_secrets(llm, &vault_pools)
}

/// Best-effort unlock from macOS Keychain (`tachi-vault` / `default`) and materialize
/// provider secrets into the running process. Used at daemon/MCP startup and after vault
/// auto-lock so background embed/search can keep working without a manual unlock.
pub fn keychain_vault_password_entry_available() -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }

    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
        ])
        .output()
        .map_err(|e| format!("keychain status check failed: {e}"))?;
    Ok(output.status.success())
}

pub fn auto_unlock_vault_from_keychain(server: &MemoryServer) -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    #[cfg(test)]
    if std::env::var_os("TACHI_TEST_ALLOW_KEYCHAIN_AUTO_UNLOCK").is_none() {
        return Ok(false);
    }

    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let config = server
        .with_global_store_read(|store| store.vault_get_config().map_err(|e| e.to_string()))?
        .ok_or_else(|| "vault not initialized".to_string())?;

    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "tachi-vault",
            "-a",
            "default",
            "-w",
        ])
        .output()
        .map_err(|e| format!("keychain read failed: {e}"))?;
    if !output.status.success() {
        return Ok(false);
    }

    let password = String::from_utf8(output.stdout)
        .map_err(|e| format!("keychain password is not UTF-8: {e}"))?
        .trim()
        .to_string();
    if password.is_empty() {
        return Ok(false);
    }

    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("invalid vault salt: {e}"))?;
    let key = crate::vault_crypto::DerivedVaultKey::derive(&password, &salt)
        .map_err(|e| format!("vault key derivation failed: {e}"))?;
    if !crate::vault_crypto::verify_password(key.bytes(), &config.verifier)
        .map_err(|e| format!("vault verifier check failed: {e}"))?
    {
        return Err("keychain password does not match vault".to_string());
    }

    {
        let mut v = server.vault_write();
        v.key = Some(crate::CachedVaultKey::copy_from(key.bytes()));
        v.unlock_time = Some(std::time::Instant::now());
    }

    let loaded = server.refresh_llm_provider_secrets_from_vault()?;
    tracing::info!("[vault] auto-unlocked from Keychain ({loaded} provider key(s))");
    server.requeue_auth_failed_enrichment_retries("Keychain auto-unlock");
    Ok(true)
}

/// Re-load provider secrets after the in-process vault session expires. Falls back to
/// Keychain when the cached unlock key is gone so vector/embed lanes stay alive headlessly.
pub fn re_materialize_provider_secrets_after_auto_lock(server: &MemoryServer) {
    match server.refresh_llm_provider_secrets_from_vault() {
        Ok(n) if n > 0 => {
            tracing::info!("[provider] re-materialized {n} provider key(s) after vault auto-lock")
        }
        Ok(_) => tracing::debug!("[provider] no provider keys available after vault auto-lock"),
        Err(err) => {
            tracing::warn!("[provider] provider key refresh failed after vault auto-lock: {err}")
        }
    }
}

/// Keychain auto-unlock (best effort) + provider secret materialization for any
/// short-lived server instance (CLI one-shots, MCP stdio, daemon startup).
pub fn bootstrap_provider_runtime(server: &MemoryServer) {
    match auto_unlock_vault_from_keychain(server) {
        Ok(true) => tracing::info!("[vault] auto-unlocked from Keychain"),
        Ok(false) => tracing::debug!(
            "[vault] auto-unlock skipped (no keychain entry or vault not initialized)"
        ),
        Err(err) => tracing::warn!("[vault] auto-unlock skipped: {err}"),
    }
    match server.refresh_llm_provider_secrets_from_vault() {
        Ok(n) if n > 0 => tracing::info!("[provider] {n} provider key(s) ready for LLM/embed"),
        Ok(_) => {
            tracing::debug!("[provider] no provider keys materialized (Vault locked or empty)")
        }
        Err(err) => tracing::warn!("[provider] secret materialization failed: {err}"),
    }
}

/// Parse `~/.tachi/config.env` (and peers) into key → value (non-empty values only).
pub fn collect_config_env_values() -> HashMap<String, String> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".tachi").join("config.env"));
        paths.push(home.join(".sigil").join("config.env"));
    }
    if let Ok(home) = std::env::var("TACHI_HOME") {
        paths.push(std::path::PathBuf::from(home).join("config.env"));
    }
    paths.push(std::path::PathBuf::from(".tachi/config.env"));
    paths.push(std::path::PathBuf::from(".sigil/config.env"));

    let mut values = HashMap::new();
    for path in paths {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();
                if !key.is_empty() && !value.is_empty() {
                    values.insert(key, value);
                }
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.original.as_ref() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn parse_vault_alias_accepts_colon_form() {
        assert_eq!(
            parse_vault_alias("vault:VOYAGE_API_KEY"),
            Some("VOYAGE_API_KEY")
        );
        assert_eq!(parse_vault_alias("  vault:foo  "), Some("foo"));
        assert!(parse_vault_alias("sk-live").is_none());
    }

    #[test]
    fn keychain_loader_only_groups_configured_rotation_members() {
        let mut rotations = HashSet::new();
        rotations.insert("VOYAGE_API_KEY".to_string());

        let grouped = group_api_key_values_by_configured_rotations(
            vec![
                ("VOYAGE_API_KEY_1".to_string(), "voyage-a".to_string()),
                ("VOYAGE_API_KEY_2".to_string(), "voyage-b".to_string()),
                ("SOME_API_KEY_2".to_string(), "standalone".to_string()),
            ],
            &rotations,
        );

        let voyage = grouped
            .get("VOYAGE_API_KEY")
            .expect("configured rotation members should be grouped");
        assert_eq!(voyage.len(), 2);
        assert_eq!(voyage[0].key_id, "VOYAGE_API_KEY_1");
        assert_eq!(voyage[1].key_id, "VOYAGE_API_KEY_2");
        assert!(!grouped.contains_key("SOME_API_KEY"));
        assert_eq!(
            grouped
                .get("SOME_API_KEY_2")
                .and_then(|entries| entries.first())
                .map(|entry| entry.value.as_str()),
            Some("standalone")
        );
    }

    #[test]
    fn materialize_provider_secrets_preserves_vault_alias_env() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = EnvGuard::set("VOYAGE_API_KEY", "vault:VOYAGE_API_KEY");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            "VOYAGE_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "VOYAGE_API_KEY".to_string(),
                value: "vault-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools).expect("materialize");

        assert_eq!(report.from_alias, 1);
        assert_eq!(report.env_fallbacks_bypassed, 1);
        assert_eq!(
            llm.provider_secret_for_tests(&["VOYAGE_API_KEY"])
                .as_deref(),
            Some("vault-secret")
        );
        assert_eq!(
            std::env::var("VOYAGE_API_KEY").as_deref(),
            Ok("vault:VOYAGE_API_KEY")
        );
    }

    #[test]
    fn materialize_provider_secrets_preserves_duplicate_plaintext_env_when_vault_wins() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = EnvGuard::set("OPENAI_API_KEY", "env-secret");
        let llm = LlmClient::new().expect("llm client");
        let vault_pools = HashMap::from([(
            "OPENAI_API_KEY".to_string(),
            vec![ProviderSecret {
                key_id: "OPENAI_API_KEY".to_string(),
                value: "vault-secret".to_string(),
            }],
        )]);

        let report = materialize_provider_secrets(&llm, &vault_pools).expect("materialize");

        assert_eq!(report.from_alias, 0);
        assert_eq!(report.env_fallbacks_bypassed, 1);
        assert_eq!(
            llm.provider_secret_for_tests(&["OPENAI_API_KEY"])
                .as_deref(),
            Some("vault-secret")
        );
        assert_eq!(std::env::var("OPENAI_API_KEY").as_deref(), Ok("env-secret"));
    }

    #[test]
    fn materialize_provider_secrets_formats_missing_alias_remediation() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = EnvGuard::set("VOYAGE_API_KEY", "vault:MISSING_VOYAGE");
        let llm = LlmClient::new().expect("llm client");
        let err = materialize_provider_secrets(&llm, &HashMap::new())
            .expect_err("missing alias should fail");

        assert!(err.contains("Config key 'VOYAGE_API_KEY' references Vault alias 'MISSING_VOYAGE'"));
        assert!(!err.contains("VOYAGE_API_KEY=vault:MISSING_VOYAGE"));
        assert!(err.contains("secret is missing or Vault is locked"));
        assert!(err.contains("vault_unlock"));
        assert!(err.contains("vault_set"));
    }

    #[test]
    fn re_materialize_after_auto_lock_restores_env_fallback_keys() {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env = EnvGuard::set("VOYAGE_API_KEY", "env-voyage-key");
        let db_path = std::env::temp_dir().join(format!(
            "memory-server-auto-lock-remat-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let server = crate::MemoryServer::new(db_path, None).expect("server");
        server.llm.clear_provider_secrets();

        crate::provider_config::re_materialize_provider_secrets_after_auto_lock(&server);

        assert_eq!(
            server
                .llm
                .provider_secret_for_tests(&["VOYAGE_API_KEY"])
                .as_deref(),
            Some("env-voyage-key")
        );
    }
}
