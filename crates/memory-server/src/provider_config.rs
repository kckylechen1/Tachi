//! Provider API key resolution: Vault, config.env `vault:` aliases, and env fallbacks.
//!
//! Single path for daemon, MCP, CLI backfill, and vector sweep so background jobs
//! do not re-implement Keychain/Vault reads with different behavior.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::llm::LlmClient;
use crate::status_ops::status_health::API_KEY_DEFS;
use crate::vault_ops::load_unlocked_api_key_secrets;
use crate::MemoryServer;

pub const VAULT_ALIAS_PREFIX: &str = "vault:";

/// `vault:VOYAGE_API_KEY` → `Some("VOYAGE_API_KEY")`
pub fn parse_vault_alias(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    trimmed
        .strip_prefix(VAULT_ALIAS_PREFIX)
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

pub fn is_vault_alias(value: &str) -> bool {
    parse_vault_alias(value).is_some()
}

/// Recommended config.env line for a provider key stored in Vault.
#[allow(dead_code)]
pub fn vault_alias_line(env_key: &str) -> String {
    format!("{env_key}={VAULT_ALIAS_PREFIX}{env_key}")
}

#[derive(Debug, Clone, Default)]
pub struct MaterializeReport {
    pub loaded: usize,
    pub from_vault: usize,
    pub from_alias: usize,
    pub stripped_env_placeholders: usize,
}

fn provider_env_keys() -> HashSet<String> {
    let mut keys = HashSet::new();
    for def in API_KEY_DEFS {
        keys.insert(def.key.to_string());
        for alias in def.aliases {
            keys.insert((*alias).to_string());
        }
    }
    keys
}

/// Load API keys from an unlocked in-process Vault session.
pub fn vault_api_key_map_from_server(
    server: &MemoryServer,
) -> Result<HashMap<String, String>, String> {
    Ok(load_unlocked_api_key_secrets(server)?.into_iter().collect())
}

/// Load API keys via macOS Keychain + global DB (daemon/CLI when memory unlock is empty).
pub fn vault_api_key_map_from_keychain(global_db_path: &Path) -> HashMap<String, String> {
    crate::status_ops::status_health::load_keychain_vault_api_key_values(global_db_path)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn resolve_vault_map(
    server: Option<&MemoryServer>,
    global_db_path: &Path,
) -> HashMap<String, String> {
    if let Some(server) = server {
        if let Ok(map) = vault_api_key_map_from_server(server) {
            if !map.is_empty() {
                return map;
            }
        }
    }
    vault_api_key_map_from_keychain(global_db_path)
}

/// Apply Vault + config.env aliases into `LlmClient` and strip `vault:` placeholders from env.
pub fn materialize_provider_secrets(
    llm: &LlmClient,
    vault_map: &HashMap<String, String>,
) -> Result<MaterializeReport, String> {
    llm.clear_provider_secrets();
    let mut resolved: HashMap<String, String> = vault_map.clone();
    let mut report = MaterializeReport {
        from_vault: vault_map.len(),
        ..Default::default()
    };

    let provider_keys = provider_env_keys();
    for key in provider_keys {
        let Ok(env_val) = std::env::var(&key) else {
            continue;
        };
        let trimmed = env_val.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(vault_name) = parse_vault_alias(trimmed) {
            let secret = resolved
                .get(vault_name)
                .cloned()
                .or_else(|| vault_map.get(vault_name).cloned())
                .ok_or_else(|| {
                    format!(
                        "{key}={trimmed} in config.env but Vault secret '{vault_name}' is missing or Vault is locked. \
                         Run vault_unlock and vault_set, or store the key in Vault as '{vault_name}'."
                    )
                })?;
            resolved.insert(key.clone(), secret);
            std::env::remove_var(&key);
            report.from_alias += 1;
            report.stripped_env_placeholders += 1;
            continue;
        }

        if vault_map.contains_key(&key) {
            // Vault wins over duplicate plaintext in env/config.env.
            std::env::remove_var(&key);
            report.stripped_env_placeholders += 1;
            continue;
        }

        resolved.insert(key, trimmed.to_string());
    }

    report.loaded = llm.set_provider_secrets(resolved);
    Ok(report)
}

pub fn materialize_for_server(server: &MemoryServer) -> Result<MaterializeReport, String> {
    let global = server.global_db_path_buf();
    let vault_map = resolve_vault_map(Some(server), &global);
    materialize_provider_secrets(server.llm.as_ref(), &vault_map)
}

pub fn materialize_standalone(
    llm: &LlmClient,
    global_db_path: &Path,
) -> Result<MaterializeReport, String> {
    let vault_map = resolve_vault_map(None, global_db_path);
    materialize_provider_secrets(llm, &vault_map)
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

    #[test]
    fn parse_vault_alias_accepts_colon_form() {
        assert_eq!(
            parse_vault_alias("vault:VOYAGE_API_KEY"),
            Some("VOYAGE_API_KEY")
        );
        assert_eq!(parse_vault_alias("  vault:foo  "), Some("foo"));
        assert!(parse_vault_alias("sk-live").is_none());
    }
}
