use crate::vault_ops::ctx::*;
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};

use super::access::{load_unlocked_vault_secrets, read_unlocked_vault_secret};
use super::load_unlocked_api_key_secret_pools;

pub(super) fn load_unlocked_env_secrets(
    server: &impl VaultCtx,
) -> Result<Vec<(String, String)>, String> {
    let pools = load_unlocked_api_key_secret_pools(server)?;
    let rotation_member_names: HashSet<String> = pools
        .values()
        .flat_map(|entries| entries.iter().map(|entry| entry.key_id.clone()))
        .collect();
    let mut secrets = load_unlocked_vault_secrets(server, |entry| {
        crate::utils::is_shell_env_name(&entry.name)
            && !rotation_member_names.contains(&entry.name)
            && !crate::vault_ops::is_lane_slot_secret_name(&entry.name)
            && !crate::provider_config::parse_rotation_member_name(&entry.name)
                .is_some_and(|(prefix, _)| crate::vault_ops::is_lane_slot_secret_name(prefix))
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
    server: &impl VaultCtx,
) -> Result<Vec<(String, String)>, String> {
    // #1680/D3: lane env injection must keep delivering every provider-key
    // class (e.g. TAVILY_API_KEY rotation members), not just the narrowed
    // ModelApi materialization allowlist — see
    // `provider_config::admitted_provider_env_keys` doc.
    let provider_keys = crate::provider_config::admitted_provider_env_keys();
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
    server: &impl VaultCtx,
    cwd: Option<&Path>,
) -> Result<Vec<(String, String)>, String> {
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
        return Ok(secrets);
    };
    let Some(project) = load_project_vault_env(cwd)? else {
        return Ok(secrets);
    };
    for (env_name, secret_name) in parse_project_vault_env_bindings(&project.contents) {
        let value = match read_unlocked_vault_secret(server, &secret_name, None, false) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    "[vault] skipped project env binding {}={} from {}: {}",
                    env_name,
                    crate::provider_config::vault_alias_line(&secret_name),
                    project.path.display(),
                    err
                );
                continue;
            }
        };
        upsert_env_secret(&mut secrets, env_name, value);
    }

    Ok(secrets)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvBinding {
    pub env_name: String,
    pub secret_name: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvIgnoredLine {
    pub line: usize,
    pub reason: String,
}

/// Project env binding files, in the order they are preferred inside `.tachi`.
const PROJECT_ENV_FILE_NAMES: [&str; 2] = ["vault.env", "vault-bindings.env"];
/// A binding file holds `NAME=vault:SECRET` lines; anything larger is not one.
const MAX_PROJECT_VAULT_ENV_BYTES: usize = 256 * 1024;

pub(crate) struct ProjectVaultEnv {
    pub(crate) path: PathBuf,
    pub(crate) contents: String,
}

/// Read the nearest project env binding file at or above `cwd`.
///
/// The search stops at the enclosing checkout: once a directory that owns a
/// `.git` has been checked, a nested checkout does not inherit the bindings of
/// the project around it. The file is read through directory descriptors, so
/// neither it nor `.tachi` can be a symlink leading out of that directory.
pub(crate) fn load_project_vault_env(cwd: &Path) -> Result<Option<ProjectVaultEnv>, String> {
    let start = project_vault_env_start(cwd).map_err(|err| {
        format!(
            "Failed to resolve project location {}: {err}",
            cwd.display()
        )
    })?;
    for dir in start.ancestors() {
        for name in PROJECT_ENV_FILE_NAMES {
            let path = dir.join(".tachi").join(name);
            if let Some(contents) = read_project_vault_env_file(dir, &path)? {
                return Ok(Some(ProjectVaultEnv { path, contents }));
            }
        }
        let marker = dir.join(".git");
        match fs::symlink_metadata(&marker) {
            Ok(_) => return Ok(None),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(format!("Failed to inspect {}: {err}", marker.display())),
        }
    }
    Ok(None)
}

fn project_vault_env_start(cwd: &Path) -> io::Result<PathBuf> {
    if cwd.components().any(|part| part == Component::ParentDir) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "project location must not contain '..'",
        ));
    }
    let absolute = std::path::absolute(cwd)?;
    // Normalize separators so symlink_metadata sees a dangling link even
    // when the caller supplied a trailing slash.
    let mut candidate: PathBuf = absolute.components().collect();
    let mut allow_file = true;
    loop {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                let resolved = candidate.canonicalize()?;
                let metadata = fs::metadata(&resolved)?;
                if metadata.is_dir() {
                    return Ok(resolved);
                }
                if allow_file && metadata.is_file() {
                    return resolved
                        .parent()
                        .map(Path::to_path_buf)
                        .ok_or_else(|| ErrorKind::NotADirectory.into());
                }
                return Err(io::Error::new(
                    ErrorKind::NotADirectory,
                    format!("{} is not a directory", candidate.display()),
                ));
            }
            Err(err) if err.kind() == ErrorKind::NotFound && candidate.file_name().is_some() => {
                candidate.pop();
                allow_file = false;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn read_project_vault_env_file(project_dir: &Path, path: &Path) -> Result<Option<String>, String> {
    crate::dispatch_ops::read_text_file_within(project_dir, path, MAX_PROJECT_VAULT_ENV_BYTES)
}

#[cfg(not(unix))]
fn read_project_vault_env_file(_project_dir: &Path, path: &Path) -> Result<Option<String>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(format!(
                "refusing to read a project env binding file that is not a regular file: {}",
                path.display()
            ))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("Failed to read {}: {err}", path.display())),
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|err| format!("Failed to read {}: {err}", path.display()))
}

pub(crate) fn parse_project_vault_env_bindings_detailed(
    contents: &str,
) -> (Vec<ProjectEnvBinding>, Vec<ProjectEnvIgnoredLine>) {
    let mut bindings = Vec::new();
    let mut ignored = Vec::new();
    for (idx, raw_line) in contents.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((name, value)) = line.split_once('=') else {
            ignored.push(ProjectEnvIgnoredLine {
                line: line_no,
                reason: "missing '='".to_string(),
            });
            continue;
        };
        let name = name.trim();
        if !crate::utils::is_shell_env_name(name) {
            ignored.push(ProjectEnvIgnoredLine {
                line: line_no,
                reason: format!("invalid env name '{name}'"),
            });
            continue;
        }
        if crate::utils::is_execution_environment_name(name) {
            ignored.push(ProjectEnvIgnoredLine {
                line: line_no,
                reason: format!("refused: '{name}' chooses what the child loads or executes"),
            });
            continue;
        }
        let value = value.split_once(" #").map_or(value, |(v, _)| v).trim();
        let Some(secret_name) = crate::provider_config::parse_vault_alias(value) else {
            ignored.push(ProjectEnvIgnoredLine {
                line: line_no,
                reason: "value is not a vault:<secret> alias".to_string(),
            });
            continue;
        };
        bindings.push(ProjectEnvBinding {
            env_name: name.to_string(),
            secret_name: secret_name.to_string(),
            line: line_no,
        });
    }
    (bindings, ignored)
}

type ProjectEnvBindingLoad = Result<
    (PathBuf, Vec<ProjectEnvBinding>, Vec<ProjectEnvIgnoredLine>),
    Box<dyn std::error::Error>,
>;

pub(crate) fn load_project_env_bindings(cwd: &Path) -> ProjectEnvBindingLoad {
    let project = load_project_vault_env(cwd)?.ok_or_else(|| {
        format!(
            "No project Vault env binding file found from {}. Create .tachi/vault.env with lines like OPENAI_API_KEY=vault:OPENAI_API_KEY.",
            cwd.display()
        )
    })?;
    let (bindings, ignored) = parse_project_vault_env_bindings_detailed(&project.contents);
    Ok((project.path, bindings, ignored))
}

fn parse_project_vault_env_bindings(contents: &str) -> Vec<(String, String)> {
    parse_project_vault_env_bindings_detailed(contents)
        .0
        .into_iter()
        .map(|binding| (binding.env_name, binding.secret_name))
        .collect()
}

/// Resolved child environment; values are cleared on every return path.
pub(crate) struct VaultEnvironment(Vec<(String, String)>);

impl VaultEnvironment {
    pub(crate) fn as_slice(&self) -> &[(String, String)] {
        &self.0
    }
}

impl std::fmt::Debug for VaultEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultEnvironment")
            .field(
                "names",
                &self.0.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Drop for VaultEnvironment {
    fn drop(&mut self) {
        for (_, value) in &mut self.0 {
            crate::vault_crypto::zero_string(value);
        }
    }
}

pub(crate) fn resolve_explicit_environment(
    store: &mut memcore::MemoryStore,
    key: &[u8; 32],
    bindings: impl IntoIterator<Item = (String, String)>,
) -> Result<VaultEnvironment, String> {
    let bindings: std::collections::BTreeMap<_, _> = bindings.into_iter().collect();
    let mut resolved_secrets: std::collections::BTreeMap<
        String,
        crate::vault_crypto::ZeroizingString,
    > = std::collections::BTreeMap::new();
    let mut environment = VaultEnvironment(Vec::new());
    for (name, secret) in bindings {
        if !crate::utils::is_shell_env_name(&name) {
            return Err("Environment binding has an invalid variable name".to_string());
        }
        let value = if let Some(cached) = resolved_secrets.get(&secret) {
            (**cached).to_string()
        } else {
            let fetched =
                super::read_usable_vault_secret_from_store_direct(store, key, &secret, true)?;
            let fetched = crate::vault_crypto::ZeroizingString::new(fetched);
            if fetched.trim().is_empty() {
                return Err(format!("Vault secret {secret} is empty"));
            }
            if fetched
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .trim()
                .get(..6)
                .is_some_and(|p| p.eq_ignore_ascii_case("vault:"))
            {
                return Err(format!(
                    "Vault secret {secret} contains an unresolved vault: pointer"
                ));
            }
            let out = (*fetched).to_string();
            resolved_secrets.insert(secret, fetched);
            out
        };
        environment.0.push((name, value));
    }
    Ok(environment)
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
    server: &impl VaultCtx,
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
