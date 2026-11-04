use super::types::{ProjectEnvBindingStatus, ProjectEnvPlan};
use crate::vault_ops::{find_project_vault_env_file, parse_project_vault_env_bindings_detailed};
use std::collections::HashSet;
use std::path::Path;

pub(super) fn build_project_env_plan(
    store: &memcore::MemoryStore,
    cwd: &Path,
) -> Result<ProjectEnvPlan, Box<dyn std::error::Error>> {
    let Some(bindings_path) = find_project_vault_env_file(cwd) else {
        return Ok(ProjectEnvPlan {
            cwd: cwd.to_string_lossy().to_string(),
            bindings_path: None,
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
    Ok(ProjectEnvPlan {
        cwd: cwd.to_string_lossy().to_string(),
        bindings_path: Some(bindings_path.to_string_lossy().to_string()),
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
