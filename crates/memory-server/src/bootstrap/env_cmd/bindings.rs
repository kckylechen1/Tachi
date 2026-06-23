use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::types::{
    ProjectEnvBinding, ProjectEnvBindingStatus, ProjectEnvIgnoredLine, ProjectEnvPlan,
};

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

pub(super) fn project_root_from_bindings_path(bindings_path: &Path) -> PathBuf {
    bindings_path
        .parent()
        .and_then(|dir| dir.parent())
        .unwrap_or(bindings_path)
        .to_path_buf()
}

pub(super) fn default_project_env_output_path(bindings_path: &Path) -> PathBuf {
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
        if !crate::utils::is_shell_env_name(name) {
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

type ProjectEnvBindingLoad = Result<
    (PathBuf, Vec<ProjectEnvBinding>, Vec<ProjectEnvIgnoredLine>),
    Box<dyn std::error::Error>,
>;

pub(super) fn load_project_env_bindings(cwd: &Path) -> ProjectEnvBindingLoad {
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

pub(super) fn build_project_env_plan(
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

pub(super) fn print_project_env_plan(
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
