use crate::server_state::MemoryServer;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::access::{
    load_unlocked_api_key_secret_pools, load_unlocked_vault_secrets, read_unlocked_vault_secret,
};

pub(super) fn load_unlocked_env_secrets(
    server: &MemoryServer,
) -> Result<Vec<(String, String)>, String> {
    let pools = load_unlocked_api_key_secret_pools(server)?;
    let rotation_member_names: HashSet<String> = pools
        .values()
        .flat_map(|entries| entries.iter().map(|entry| entry.key_id.clone()))
        .collect();
    let mut secrets = load_unlocked_vault_secrets(server, |entry| {
        crate::utils::is_shell_env_name(&entry.name) && !rotation_member_names.contains(&entry.name)
    })?;
    for (logical_name, entries) in pools {
        if !crate::utils::is_shell_env_name(&logical_name) {
            continue;
        }
        if let Some(entry) = entries.first() {
            upsert_env_secret(&mut secrets, logical_name, entry.value.clone());
        }
    }
    Ok(secrets)
}

fn load_unlocked_provider_env_secrets(
    server: &MemoryServer,
) -> Result<Vec<(String, String)>, String> {
    let provider_keys = crate::provider_config::provider_env_keys();
    let pools = load_unlocked_api_key_secret_pools(server)?;
    let mut secrets = Vec::new();
    for (logical_name, entries) in pools {
        if !provider_keys.contains(&logical_name) || !crate::utils::is_shell_env_name(&logical_name)
        {
            continue;
        }
        if let Some(entry) = entries.first() {
            upsert_env_secret(&mut secrets, logical_name, entry.value.clone());
        }
    }
    Ok(secrets)
}

pub(crate) fn load_unlocked_env_secrets_for_child_env(
    server: &MemoryServer,
    cwd: Option<&Path>,
) -> Result<Vec<(String, String)>, String> {
    load_unlocked_env_secrets_for_child_env_with_consumer(server, cwd, None)
        .map(|report| report.secrets)
}

/// The dispatch and CLI execution paths share this one child-environment
/// resolver. `unavailable` records project binding failures by environment name
/// so an explicit CLI requirement can fail loudly without re-parsing bindings.
pub(crate) struct ChildEnvSecretLoad {
    pub(crate) secrets: Vec<(String, String)>,
    pub(crate) unavailable: HashMap<String, String>,
}

pub(crate) fn load_unlocked_env_secrets_for_child_env_with_consumer(
    server: &MemoryServer,
    cwd: Option<&Path>,
    consumer: Option<&str>,
) -> Result<ChildEnvSecretLoad, String> {
    let child_env_mode = std::env::var("TACHI_VAULT_CHILD_ENV")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "project".to_string());
    let include_all_env_secrets = matches!(
        child_env_mode.as_str(),
        "all"
            | "full"
            | "legacy"
            | "legacy_all"
            | "override"
            | "fill_missing"
            | "missing_only"
            | "preserve_env"
    );

    let mut secrets = if include_all_env_secrets {
        load_unlocked_env_secrets(server)?
    } else {
        load_unlocked_provider_env_secrets(server)?
    };
    let Some(cwd) = cwd else {
        return Ok(ChildEnvSecretLoad {
            secrets,
            unavailable: HashMap::new(),
        });
    };
    let Some(bindings_path) = find_project_vault_env_file(cwd) else {
        return Ok(ChildEnvSecretLoad {
            secrets,
            unavailable: HashMap::new(),
        });
    };

    let contents = std::fs::read_to_string(&bindings_path)
        .map_err(|e| format!("Failed to read {}: {e}", bindings_path.display()))?;
    let consumer = consumer.filter(|value| !value.trim().is_empty());
    let mut unavailable = HashMap::new();
    for (env_name, secret_name) in parse_project_vault_env_bindings(&contents) {
        let value = match read_unlocked_vault_secret(server, &secret_name, consumer, false) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    "[vault] skipped project env binding {}={} from {}: {}",
                    env_name,
                    crate::provider_config::vault_alias_line(&secret_name),
                    bindings_path.display(),
                    err
                );
                unavailable.insert(env_name, err);
                continue;
            }
        };
        upsert_env_secret(&mut secrets, env_name, value);
    }

    Ok(ChildEnvSecretLoad {
        secrets,
        unavailable,
    })
}

fn find_project_vault_env_file(cwd: &Path) -> Option<PathBuf> {
    let start = if cwd.is_file() {
        cwd.parent().unwrap_or(cwd)
    } else {
        cwd
    };

    for dir in start.ancestors() {
        for rel_path in [".tachi/vault.env", ".tachi/vault-bindings.env"] {
            let candidate = dir.join(rel_path);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn parse_project_vault_env_bindings(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim();
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            if !crate::utils::is_shell_env_name(name) {
                return None;
            }
            crate::provider_config::parse_vault_alias(value)
                .map(|secret_name| (name.to_string(), secret_name.to_string()))
        })
        .collect()
}

fn upsert_env_secret(secrets: &mut Vec<(String, String)>, name: String, value: String) {
    if let Some((_, existing_value)) = secrets
        .iter_mut()
        .find(|(existing_name, _)| existing_name == &name)
    {
        *existing_value = value;
    } else {
        secrets.push((name, value));
    }
}

pub(super) fn attach_provider_refresh_warning(
    server: &MemoryServer,
    body: String,
) -> Result<String, String> {
    match server.refresh_llm_provider_secrets_from_vault() {
        // Clean refresh with no skipped aliases: pass the response through unchanged.
        Ok(report) if report.skipped_aliases.is_empty() => Ok(body),
        // #1279: the refresh succeeded but one or more `vault:` aliases were skipped
        // (missing/locked secret). The vault_set/unlock caller must not read a bare
        // success — surface the skipped aliases (with remediation) into the response.
        Ok(report) => attach_refresh_warning_field(
            body,
            format!(
                "Vault operation succeeded, but {}",
                crate::provider_config::describe_skipped_aliases(&report.skipped_aliases)
            ),
        ),
        Err(err) => attach_refresh_warning_field(
            body,
            format!("Vault operation succeeded, but provider key cache refresh failed: {err}"),
        ),
    }
}

fn attach_refresh_warning_field(body: String, warning: String) -> Result<String, String> {
    let mut value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("serialize provider refresh warning: {e}"))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "provider_secret_refresh_warning".to_string(),
            json!(warning),
        );
    }
    serde_json::to_string(&value).map_err(|e| format!("serialize: {e}"))
}
