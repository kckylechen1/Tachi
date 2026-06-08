//! Provider API key resolution: Vault, config.env `vault:` aliases, and env fallbacks.
//!
//! Single path for daemon, MCP, CLI backfill, and vector sweep so background jobs
//! do not re-implement Keychain/Vault reads with different behavior.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::llm::{LlmClient, ProviderSecret};
use crate::status_ops::status_health::API_KEY_DEFS;
use crate::vault_ops::load_unlocked_api_key_secret_pools;
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
    vault_api_key_pools_from_keychain(global_db_path)
}

fn flatten_pools(pools: &HashMap<String, Vec<ProviderSecret>>) -> HashMap<String, String> {
    pools
        .iter()
        .filter_map(|(name, entries)| {
            entries
                .first()
                .map(|entry| (name.clone(), entry.value.clone()))
        })
        .collect()
}

pub(crate) fn parse_rotation_member_name(name: &str) -> Option<(&str, u32)> {
    let (prefix, suffix) = name.rsplit_once('_')?;
    let index = suffix.parse::<u32>().ok()?;
    if prefix.is_empty() {
        None
    } else {
        Some((prefix, index))
    }
}

fn rotation_prefixes_from_global_db(global_db_path: &Path) -> HashSet<String> {
    let Some(path) = global_db_path.to_str() else {
        return HashSet::new();
    };
    let Ok(store) = memory_core::MemoryStore::open_read_only(path) else {
        return HashSet::new();
    };
    store
        .vault_list_rotations()
        .unwrap_or_default()
        .into_iter()
        .map(|rotation| rotation.prefix)
        .collect()
}

fn group_api_key_values_by_configured_rotations(
    values: Vec<(String, String)>,
    rotation_prefixes: &HashSet<String>,
) -> HashMap<String, Vec<ProviderSecret>> {
    values
        .into_iter()
        .fold(HashMap::new(), |mut acc, (name, value)| {
            if let Some((prefix, _)) = parse_rotation_member_name(&name) {
                if rotation_prefixes.contains(prefix) {
                    acc.entry(prefix.to_string())
                        .or_insert_with(Vec::new)
                        .push(ProviderSecret {
                            key_id: name,
                            value,
                        });
                    return acc;
                }
            }

            acc.entry(name.clone())
                .or_insert_with(Vec::new)
                .push(ProviderSecret {
                    key_id: name,
                    value,
                });
            acc
        })
}

/// Apply Vault + config.env aliases into `LlmClient` and strip `vault:` placeholders from env.
pub fn materialize_provider_secrets(
    llm: &LlmClient,
    vault_pools: &HashMap<String, Vec<ProviderSecret>>,
) -> Result<MaterializeReport, String> {
    llm.clear_provider_secrets();
    let vault_map = flatten_pools(vault_pools);
    let mut resolved_pools: HashMap<String, Vec<ProviderSecret>> = vault_pools.clone();
    let mut report = MaterializeReport {
        from_vault: vault_pools.len(),
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
            let pool = vault_pools
                .get(vault_name)
                .cloned()
                .or_else(|| {
                    vault_map.get(vault_name).cloned().map(|secret| {
                        vec![ProviderSecret {
                            key_id: vault_name.to_string(),
                            value: secret,
                        }]
                    })
                })
                .ok_or_else(|| {
                    format!(
                        "{key}={trimmed} in config.env but Vault secret '{vault_name}' is missing or Vault is locked. \
                         Run vault_unlock and vault_set, or store the key in Vault as '{vault_name}'."
                    )
                })?;
            resolved_pools.insert(key.clone(), pool);
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

        resolved_pools.insert(
            key.clone(),
            vec![ProviderSecret {
                key_id: key,
                value: trimmed.to_string(),
            }],
        );
    }

    report.loaded = llm.set_provider_secret_pools(resolved_pools);
    Ok(report)
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
}
