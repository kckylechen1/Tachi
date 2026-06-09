use super::*;
use crate::cli::EnvAction;
use memory_core::vault::VaultEntry;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

fn is_shell_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_upper_snake_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvPlan {
    pub cwd: String,
    pub bindings_path: Option<String>,
    pub output_path: Option<String>,
    pub binding_count: usize,
    pub missing_count: usize,
    pub bindings: Vec<ProjectEnvBindingStatus>,
    pub ignored_lines: Vec<ProjectEnvIgnoredLine>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectEnvBindingStatus {
    pub env_name: String,
    pub secret_name: String,
    pub line: usize,
    pub status: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
struct ProjectEnvSyncReport {
    cwd: String,
    bindings_path: String,
    output_path: String,
    binding_count: usize,
    written: bool,
    dry_run: bool,
}

struct UnlockedVaultStore {
    store: memory_core::MemoryStore,
    key: [u8; 32],
}

// ─── `tachi env` handler ────────────────────────────────────────────────────

pub(super) async fn run_env_command(
    global_db_path: &PathBuf,
    action: Option<EnvAction>,
    filter: Option<&str>,
    env_only: bool,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        Some(EnvAction::Plan { cwd, json }) => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let store = open_cli_store_read_only(global_db_path)?;
            let plan = build_project_env_plan(&store, &cwd, None)?;
            print_project_env_plan(&plan, json)?;
            Ok(())
        }
        Some(EnvAction::Export { cwd, json }) => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let unlocked =
                unlock_cli_vault(global_db_path, stdin_password, keychain, password_file)?;
            let exports = filter_project_exports(
                resolve_project_env_values(&unlocked, &cwd)?,
                filter,
                env_only,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&exports)?);
            } else {
                print_shell_exports(&exports);
                eprintln!(
                    "# tachi env export: {} project secret(s) emitted",
                    exports.len()
                );
            }
            Ok(())
        }
        Some(EnvAction::Sync {
            cwd,
            output,
            dry_run,
            force,
            json,
        }) => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let unlocked =
                unlock_cli_vault(global_db_path, stdin_password, keychain, password_file)?;
            let report = sync_project_env(
                &unlocked,
                &cwd,
                output.as_deref(),
                dry_run,
                force,
                filter,
                env_only,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else if dry_run {
                println!("Tachi env sync dry-run.");
                println!("  cwd: {}", report.cwd);
                println!("  bindings: {}", report.bindings_path);
                println!("  output: {}", report.output_path);
                println!("  project secrets: {}", report.binding_count);
            } else {
                println!("Tachi env sync complete.");
                println!("  output: {}", report.output_path);
                println!("  project secrets: {}", report.binding_count);
            }
            Ok(())
        }
        Some(EnvAction::Run { cwd, command }) => {
            let cwd = resolve_cwd(cwd.as_deref())?;
            let unlocked =
                unlock_cli_vault(global_db_path, stdin_password, keychain, password_file)?;
            let exports = filter_project_exports(
                resolve_project_env_values(&unlocked, &cwd)?,
                filter,
                env_only,
            )?;
            run_with_project_env(&cwd, &command, &exports)
        }
        None => {
            run_legacy_env_export(
                global_db_path,
                filter,
                env_only,
                stdin_password,
                keychain,
                password_file,
            )
            .await
        }
    }
}

fn resolve_cwd(cwd: Option<&Path>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(match cwd {
        Some(path) => std::fs::canonicalize(path)
            .map_err(|e| format!("Failed to resolve cwd {}: {e}", path.display()))?,
        None => std::env::current_dir()?,
    })
}

fn unlock_cli_vault(
    global_db_path: &PathBuf,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&std::path::Path>,
) -> Result<UnlockedVaultStore, Box<dyn std::error::Error>> {
    let store = open_cli_store(global_db_path)?;
    let config = store
        .vault_get_config()
        .map_err(|e| format!("Failed to read vault config: {e}"))?
        .ok_or_else(|| {
            "Vault not initialized. Run `tachi vault init` first, or initialize it via MCP."
                .to_string()
        })?;

    let password = super::vault_cli::read_vault_password(stdin_password, keychain, password_file)?;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    let key = crate::vault_crypto::derive_key(&password, &salt)?;
    if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
        return Err("Wrong password".into());
    }

    Ok(UnlockedVaultStore { store, key })
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

fn project_root_from_bindings_path(bindings_path: &Path) -> PathBuf {
    bindings_path
        .parent()
        .and_then(|dir| dir.parent())
        .unwrap_or(bindings_path)
        .to_path_buf()
}

fn default_project_env_output_path(bindings_path: &Path) -> PathBuf {
    bindings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("env.generated")
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
        if !is_shell_env_name(name) {
            ignored.push(ProjectEnvIgnoredLine {
                line: line_no,
                reason: format!("invalid env name '{name}'"),
            });
            continue;
        }
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

fn load_project_env_bindings(
    cwd: &Path,
) -> Result<(PathBuf, Vec<ProjectEnvBinding>, Vec<ProjectEnvIgnoredLine>), Box<dyn std::error::Error>>
{
    let bindings_path = find_project_vault_env_file(cwd).ok_or_else(|| {
        format!(
            "No project Vault env binding file found from {}. Create .tachi/vault.env with lines like OPENAI_API_KEY=vault:OPENAI_API_KEY.",
            cwd.display()
        )
    })?;
    let contents = std::fs::read_to_string(&bindings_path)
        .map_err(|e| format!("Failed to read {}: {e}", bindings_path.display()))?;
    let (bindings, ignored) = parse_project_vault_env_bindings_detailed(&contents);
    Ok((bindings_path, bindings, ignored))
}

fn build_project_env_plan(
    store: &memory_core::MemoryStore,
    cwd: &Path,
    output_path: Option<&Path>,
) -> Result<ProjectEnvPlan, Box<dyn std::error::Error>> {
    let Some(bindings_path) = find_project_vault_env_file(cwd) else {
        return Ok(ProjectEnvPlan {
            cwd: cwd.to_string_lossy().to_string(),
            bindings_path: None,
            output_path: output_path.map(|path| path.to_string_lossy().to_string()),
            binding_count: 0,
            missing_count: 0,
            bindings: Vec::new(),
            ignored_lines: Vec::new(),
        });
    };
    let contents = std::fs::read_to_string(&bindings_path)
        .map_err(|e| format!("Failed to read {}: {e}", bindings_path.display()))?;
    let (bindings, ignored_lines) = parse_project_vault_env_bindings_detailed(&contents);
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("Failed to list vault entries: {e}"))?;
    let names = entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<HashSet<_>>();
    let mut statuses = Vec::new();
    let mut missing_count = 0usize;
    for binding in bindings {
        let (status, source) = if names.contains(binding.secret_name.as_str()) {
            ("ok".to_string(), "secret".to_string())
        } else if store
            .vault_get_rotation(&binding.secret_name)
            .map_err(|e| format!("vault_get_rotation: {e}"))?
            .is_some()
        {
            ("ok".to_string(), "pool".to_string())
        } else {
            missing_count += 1;
            ("missing".to_string(), "missing".to_string())
        };
        statuses.push(ProjectEnvBindingStatus {
            env_name: binding.env_name,
            secret_name: binding.secret_name,
            line: binding.line,
            status,
            source,
        });
    }
    let output_path = output_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_project_env_output_path(&bindings_path));
    Ok(ProjectEnvPlan {
        cwd: cwd.to_string_lossy().to_string(),
        bindings_path: Some(bindings_path.to_string_lossy().to_string()),
        output_path: Some(output_path.to_string_lossy().to_string()),
        binding_count: statuses.len(),
        missing_count,
        bindings: statuses,
        ignored_lines,
    })
}

fn print_project_env_plan(
    plan: &ProjectEnvPlan,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    println!("Tachi project env plan:");
    println!("  cwd: {}", plan.cwd);
    match plan.bindings_path.as_deref() {
        Some(path) => println!("  bindings: {path}"),
        None => {
            println!("  bindings: not found");
            return Ok(());
        }
    }
    if let Some(path) = plan.output_path.as_deref() {
        println!("  generated env: {path}");
    }
    println!("  project secrets: {}", plan.binding_count);
    if plan.missing_count > 0 {
        println!("  missing: {}", plan.missing_count);
    }
    for binding in &plan.bindings {
        println!(
            "  - {} <= vault:{} [{}:{}]",
            binding.env_name, binding.secret_name, binding.source, binding.status
        );
    }
    for ignored in &plan.ignored_lines {
        println!("  ignored line {}: {}", ignored.line, ignored.reason);
    }
    Ok(())
}

fn resolve_project_env_values(
    unlocked: &UnlockedVaultStore,
    cwd: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let (_, bindings, ignored_lines) = load_project_env_bindings(cwd)?;
    if !ignored_lines.is_empty() {
        for ignored in ignored_lines {
            eprintln!(
                "WARNING: ignored .tachi/vault.env line {}: {}",
                ignored.line, ignored.reason
            );
        }
    }
    let entries = unlocked
        .store
        .vault_list_entries()
        .map_err(|e| format!("Failed to list vault entries: {e}"))?;
    let by_name = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let mut exports = Vec::new();
    for binding in bindings {
        let value = resolve_bound_secret_value(unlocked, &by_name, &binding.secret_name)
            .map_err(|e| format!("{} (line {})", e, binding.line))?;
        upsert_env_secret(&mut exports, binding.env_name, value);
    }
    Ok(exports)
}

fn filter_project_exports(
    exports: Vec<(String, String)>,
    filter: Option<&str>,
    env_only: bool,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let glob_pattern = filter.map(glob::Pattern::new).transpose()?;
    Ok(exports
        .into_iter()
        .filter(|(name, _)| {
            if env_only && !is_upper_snake_env_name(name) {
                return false;
            }
            if let Some(pattern) = glob_pattern.as_ref() {
                return pattern.matches(name);
            }
            true
        })
        .collect())
}

fn resolve_bound_secret_value(
    unlocked: &UnlockedVaultStore,
    entries: &HashMap<&str, &VaultEntry>,
    secret_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(entry) = entries.get(secret_name).copied() {
        return decrypt_entry_value(entry, &unlocked.key);
    }
    let (_, value) =
        super::vault_cli::lease_api_key_from_store(&unlocked.store, &unlocked.key, secret_name)?;
    Ok(value)
}

fn decrypt_entry_value(
    entry: &VaultEntry,
    key: &[u8; 32],
) -> Result<String, Box<dyn std::error::Error>> {
    if entry
        .allowed_agents
        .as_ref()
        .is_some_and(|agents| !agents.is_empty())
    {
        return Err(format!("Vault secret '{}' is agent-restricted", entry.name).into());
    }
    let decrypted = crate::vault_crypto::decrypt(key, &entry.encrypted_value, &entry.nonce)?;
    let value = String::from_utf8(decrypted)
        .map_err(|e| format!("Vault secret '{}' is not valid UTF-8: {e}", entry.name))?;
    if value.trim().is_empty() {
        return Err(format!("Vault secret '{}' is empty", entry.name).into());
    }
    Ok(value)
}

fn sync_project_env(
    unlocked: &UnlockedVaultStore,
    cwd: &Path,
    output_path: Option<&Path>,
    dry_run: bool,
    force: bool,
    filter: Option<&str>,
    env_only: bool,
) -> Result<ProjectEnvSyncReport, Box<dyn std::error::Error>> {
    let (bindings_path, _, _) = load_project_env_bindings(cwd)?;
    let resolved_output_path = output_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_project_env_output_path(&bindings_path));
    let exports =
        filter_project_exports(resolve_project_env_values(unlocked, cwd)?, filter, env_only)?;
    let report = ProjectEnvSyncReport {
        cwd: project_root_from_bindings_path(&bindings_path)
            .to_string_lossy()
            .to_string(),
        bindings_path: bindings_path.to_string_lossy().to_string(),
        output_path: resolved_output_path.to_string_lossy().to_string(),
        binding_count: exports.len(),
        written: !dry_run,
        dry_run,
    };
    if dry_run {
        return Ok(report);
    }
    if resolved_output_path.exists() && !force {
        return Err(format!(
            "{} already exists; pass --force to overwrite",
            resolved_output_path.display()
        )
        .into());
    }
    if let Some(parent) = resolved_output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut content = String::new();
    content
        .push_str("# Generated by Tachi from .tachi/vault.env. Do not commit plaintext secrets.\n");
    content.push_str("# Source bindings contain vault:<secret> aliases; this file contains materialized values.\n");
    for (name, value) in &exports {
        content.push_str(&shell_export_line(name, value));
        content.push('\n');
    }
    write_secret_file(&resolved_output_path, content.as_bytes())?;
    Ok(report)
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("env.generated"),
        uuid::Uuid::new_v4().as_simple()
    ));
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    if let Err(err) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(Box::new(err));
    }
    drop(file);
    if let Err(err) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(Box::new(err));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn run_with_project_env(
    cwd: &Path,
    command: &[String],
    exports: &[(String, String)],
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((program, args)) = command.split_first() else {
        return Err("No command provided".into());
    };
    let mut child = std::process::Command::new(program);
    child.args(args).current_dir(cwd);
    for (name, value) in exports {
        child.env(name, value);
    }
    let status = child.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Command exited with status {status}").into())
    }
}

async fn run_legacy_env_export(
    global_db_path: &PathBuf,
    filter: Option<&str>,
    env_only: bool,
    stdin_password: bool,
    keychain: bool,
    password_file: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = open_cli_store_read_only(global_db_path)?;

    // 1. Check vault is initialized
    let config = store
        .vault_get_config()
        .map_err(|e| format!("Failed to read vault config: {e}"))?
        .ok_or_else(|| {
            "Vault not initialized. Run `tachi serve` and call vault_init first, \
             or set up the vault via an MCP client."
                .to_string()
        })?;

    // 2. Resolve password from the requested portable source.
    let password = super::vault_cli::read_vault_password(stdin_password, keychain, password_file)?;

    // 3. Derive key and verify
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    let salt = B64
        .decode(&config.salt)
        .map_err(|e| format!("Invalid vault salt: {e}"))?;
    let key = crate::vault_crypto::derive_key(&password, &salt)?;

    if !crate::vault_crypto::verify_password(&key, &config.verifier)? {
        return Err("Wrong password".into());
    }

    // 4. List and decrypt all entries
    let entries = store
        .vault_list_entries()
        .map_err(|e| format!("Failed to list vault entries: {e}"))?;

    // Build glob pattern if provided
    let glob_pattern = filter.map(glob::Pattern::new).transpose()?;

    let mut emitted = 0usize;
    for entry in entries {
        // Skip agent-restricted secrets — those aren't meant for env injection
        if entry
            .allowed_agents
            .as_ref()
            .is_some_and(|agents| !agents.is_empty())
        {
            continue;
        }

        if !is_shell_env_name(&entry.name) {
            eprintln!(
                "WARNING: skipped secret '{}' because it is not a valid shell environment name",
                entry.name
            );
            continue;
        }

        // --env-only: skip names that don't look like env vars (UPPER_SNAKE_CASE)
        if env_only && !is_upper_snake_env_name(&entry.name) {
            continue;
        }

        // --filter: apply glob pattern
        if let Some(ref pat) = glob_pattern {
            if !pat.matches(&entry.name) {
                continue;
            }
        }

        let value = match decrypt_entry_value(&entry, &key) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("WARNING: failed to decrypt '{}': {}", entry.name, e);
                continue;
            }
        };

        print!("{}", shell_export_line(&entry.name, &value));
        println!();
        emitted += 1;
    }

    eprintln!("# tachi env: {} secret(s) emitted", emitted);
    Ok(())
}

fn shell_export_line(name: &str, value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("export {name}='{escaped}'")
}

fn print_shell_exports(exports: &[(String, String)]) {
    for (name, value) in exports {
        println!("{}", shell_export_line(name, value));
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_project_vault_env_bindings_with_diagnostics() {
        let (bindings, ignored) = parse_project_vault_env_bindings_detailed(
            "\
# comment
export OPENAI_API_KEY=vault:OPENAI_API_KEY
BAD-NAME=vault:bad
LITERAL=value
BROKEN
",
        );
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].env_name, "OPENAI_API_KEY");
        assert_eq!(bindings[0].secret_name, "OPENAI_API_KEY");
        assert_eq!(ignored.len(), 3);
    }

    #[test]
    fn shell_export_escapes_single_quotes() {
        assert_eq!(shell_export_line("A", "x'y"), "export A='x'\\''y'");
    }

    fn temp_store() -> memory_core::MemoryStore {
        let db_path = std::env::temp_dir().join(format!(
            "tachi-env-cmd-test-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        memory_core::MemoryStore::open(db_path.to_str().expect("utf8 temp db"))
            .expect("open temp memory store")
    }

    fn put_secret(store: &memory_core::MemoryStore, key: &[u8; 32], name: &str, value: &str) {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(key, value.as_bytes()).expect("encrypt test secret");
        store
            .vault_upsert_entry(&memory_core::vault::VaultEntry {
                name: name.to_string(),
                encrypted_value,
                nonce,
                secret_type: "api_key".to_string(),
                description: "test secret".to_string(),
                allowed_agents: None,
                created_at: "2026-06-09T00:00:00Z".to_string(),
                updated_at: "2026-06-09T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("upsert test secret");
    }

    #[test]
    fn project_env_plan_marks_secret_pool_and_missing() {
        let store = temp_store();
        let key =
            crate::vault_crypto::derive_key("test-password", b"1234567890123456").expect("key");
        put_secret(&store, &key, "direct.secret", "direct-value");
        store
            .vault_set_rotation(&memory_core::vault::VaultKeyRotation {
                prefix: "POOL_API_KEY".to_string(),
                current_index: 1,
                total_keys: 2,
                rotation_strategy: "round_robin".to_string(),
                created_at: "2026-06-09T00:00:00Z".to_string(),
                updated_at: "2026-06-09T00:00:00Z".to_string(),
            })
            .expect("set rotation");

        let temp = tempfile::tempdir().expect("temp project");
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi");
        std::fs::write(
            project.join(".tachi/vault.env"),
            "\
PROJECT_DIRECT=vault:direct.secret
PROJECT_POOL=vault:POOL_API_KEY
PROJECT_MISSING=vault:missing.secret
BAD-NAME=vault:direct.secret
",
        )
        .expect("write bindings");

        let plan = build_project_env_plan(&store, &project, None).expect("plan");
        assert_eq!(plan.binding_count, 3);
        assert_eq!(plan.missing_count, 1);
        assert_eq!(plan.ignored_lines.len(), 1);
        assert_eq!(plan.bindings[0].source, "secret");
        assert_eq!(plan.bindings[1].source, "pool");
        assert_eq!(plan.bindings[2].status, "missing");
    }

    #[test]
    fn project_env_sync_writes_generated_exports() {
        let store = temp_store();
        let key =
            crate::vault_crypto::derive_key("test-password", b"1234567890123456").expect("key");
        put_secret(&store, &key, "direct.secret", "direct-value");
        put_secret(&store, &key, "POOL_API_KEY_1", "pool-value-1");
        store
            .vault_set_rotation(&memory_core::vault::VaultKeyRotation {
                prefix: "POOL_API_KEY".to_string(),
                current_index: 1,
                total_keys: 1,
                rotation_strategy: "round_robin".to_string(),
                created_at: "2026-06-09T00:00:00Z".to_string(),
                updated_at: "2026-06-09T00:00:00Z".to_string(),
            })
            .expect("set rotation");
        let unlocked = UnlockedVaultStore { store, key };

        let temp = tempfile::tempdir().expect("temp project");
        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi");
        std::fs::write(
            project.join(".tachi/vault.env"),
            "\
PROJECT_DIRECT=vault:direct.secret
PROJECT_POOL=vault:POOL_API_KEY
",
        )
        .expect("write bindings");

        let report = sync_project_env(&unlocked, &project, None, false, false, None, false)
            .expect("sync project env");
        assert!(report.written);
        let generated = project.join(".tachi/env.generated");
        let content = std::fs::read_to_string(&generated).expect("read generated env");
        assert!(content.contains("export PROJECT_DIRECT='direct-value'"));
        assert!(content.contains("export PROJECT_POOL='pool-value-1'"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&generated)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }
}
