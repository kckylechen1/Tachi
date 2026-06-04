use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub enum RegisterOutputFormat {
    Text,
    Json,
}

#[derive(Debug)]
pub struct RegisterOptions {
    pub path: PathBuf,
    pub repo_root: PathBuf,
    pub branch: String,
    pub dispatch_id: Option<String>,
    pub pr: Option<String>,
    pub output: RegisterOutputFormat,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct WorktreeRegistry {
    version: u32,
    worktrees: Vec<WorktreeRecord>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct WorktreeRecord {
    path: String,
    repo_root: String,
    branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dispatch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, serde::Serialize)]
struct RegisterReport {
    action: &'static str,
    path: String,
    repo_root: String,
    branch: String,
    registry_path: String,
    marker_path: String,
    registered: bool,
}

pub fn run_wt_register(options: RegisterOptions) -> Result<(), String> {
    let path = canonicalize_existing(&options.path, "worktree path")?;
    let repo_root = canonicalize_existing(&options.repo_root, "repo root")?;
    if path == repo_root {
        return Err("refusing to register repository root/main worktree".to_string());
    }

    let registry_path = registry_path()?;
    let marker_path = path.join(".tachi-worktree.json");
    let now = chrono::Utc::now().to_rfc3339();
    let mut registry = read_registry(&registry_path)?;
    let path_string = path.display().to_string();
    let record = WorktreeRecord {
        path: path_string.clone(),
        repo_root: repo_root.display().to_string(),
        branch: options.branch,
        dispatch_id: options.dispatch_id,
        pr: options.pr,
        created_at: now.clone(),
        updated_at: now,
    };

    upsert_record(&mut registry, record.clone());
    write_registry(&registry_path, &registry)?;
    write_marker(&marker_path, &record)?;

    let report = RegisterReport {
        action: "wt-register",
        path: path_string,
        repo_root: repo_root.display().to_string(),
        branch: record.branch,
        registry_path: registry_path.display().to_string(),
        marker_path: marker_path.display().to_string(),
        registered: true,
    };
    emit_register_report(&report, options.output)
}

pub fn registry_contains(worktree_root: &Path) -> bool {
    let Ok(registry_path) = registry_path() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(registry_path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let needle = canonical_string(worktree_root);
    value_contains_path(&value, &needle)
}

pub fn remove_registry_entry(worktree_root: &Path) -> Result<bool, String> {
    let registry_path = registry_path()?;
    if !registry_path.exists() {
        return Ok(false);
    }
    let mut registry = read_registry(&registry_path)?;
    let before = registry.worktrees.len();
    registry
        .worktrees
        .retain(|record| !paths_equal(Path::new(&record.path), worktree_root));
    if registry.worktrees.len() == before {
        return Ok(false);
    }
    write_registry(&registry_path, &registry)?;
    Ok(true)
}

fn upsert_record(registry: &mut WorktreeRegistry, record: WorktreeRecord) {
    if let Some(existing) = registry
        .worktrees
        .iter_mut()
        .find(|existing| paths_equal(Path::new(&existing.path), Path::new(&record.path)))
    {
        let created_at = existing.created_at.clone();
        *existing = record;
        existing.created_at = created_at;
    } else {
        registry.worktrees.push(record);
    }
}

fn read_registry(path: &Path) -> Result<WorktreeRegistry, String> {
    if !path.exists() {
        return Ok(WorktreeRegistry {
            version: 1,
            worktrees: Vec::new(),
        });
    }
    let raw = std::fs::read_to_string(path).map_err(|err| format!("read registry: {err}"))?;
    serde_json::from_str(&raw).map_err(|err| format!("parse registry: {err}"))
}

fn write_registry(path: &Path, registry: &WorktreeRegistry) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("create registry dir: {err}"))?;
    }
    let tmp_path = path.with_extension("json.tmp");
    let raw = serde_json::to_string_pretty(registry)
        .map_err(|err| format!("serialize registry: {err}"))?;
    std::fs::write(&tmp_path, format!("{raw}\n"))
        .map_err(|err| format!("write registry tmp: {err}"))?;
    std::fs::rename(&tmp_path, path).map_err(|err| format!("rename registry tmp: {err}"))
}

fn write_marker(path: &Path, record: &WorktreeRecord) -> Result<(), String> {
    let raw =
        serde_json::to_string_pretty(record).map_err(|err| format!("serialize marker: {err}"))?;
    std::fs::write(path, format!("{raw}\n")).map_err(|err| format!("write marker: {err}"))
}

fn emit_register_report(
    report: &RegisterReport,
    output: RegisterOutputFormat,
) -> Result<(), String> {
    match output {
        RegisterOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|err| format!("serialize report: {err}"))?
        ),
        RegisterOutputFormat::Text => {
            println!("tachi-clean wt-register");
            println!("  path: {}", report.path);
            println!("  repo_root: {}", report.repo_root);
            println!("  branch: {}", report.branch);
            println!("  registered: {}", report.registered);
            println!("  registry: {}", report.registry_path);
            println!("  marker: {}", report.marker_path);
        }
    }
    Ok(())
}

fn registry_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".tachi").join("worktrees.json"))
}

fn canonicalize_existing(path: &Path, label: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|err| format!("cannot canonicalize {label}: {err}"))
}

fn value_contains_path(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(s) => paths_equal(Path::new(s), Path::new(needle)),
        serde_json::Value::Array(items) => {
            items.iter().any(|item| value_contains_path(item, needle))
        }
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            (key == "path"
                && value
                    .as_str()
                    .is_some_and(|s| paths_equal(Path::new(s), Path::new(needle))))
                || value_contains_path(value, needle)
        }),
        _ => false,
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    let a = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let b = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a == b
}

fn canonical_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn nested_registry_path_is_detected() {
        let value = serde_json::json!({
            "worktrees": [
                {"path": "/tmp/a"},
                {"metadata": {"path": "/tmp/b"}}
            ]
        });

        assert!(value_contains_path(&value, "/tmp/a"));
        assert!(value_contains_path(&value, "/tmp/b"));
        assert!(!value_contains_path(&value, "/tmp/c"));
    }

    #[test]
    fn register_then_remove_updates_registry() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let old_home = std::env::var_os("HOME");
        let root =
            std::env::temp_dir().join(format!("tachi-clean-registry-test-{}", std::process::id()));
        let home = root.join("home");
        let repo = root.join("repo");
        let worktree = root.join("wt");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::env::set_var("HOME", &home);

        run_wt_register(RegisterOptions {
            path: worktree.clone(),
            repo_root: repo,
            branch: "feature/test".to_string(),
            dispatch_id: Some("dispatch-1".to_string()),
            pr: None,
            output: RegisterOutputFormat::Json,
        })
        .unwrap();

        assert!(registry_contains(&worktree));
        assert!(worktree.join(".tachi-worktree.json").exists());
        assert!(remove_registry_entry(&worktree).unwrap());
        assert!(!registry_contains(&worktree));

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
