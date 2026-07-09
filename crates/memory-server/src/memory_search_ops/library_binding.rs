//! Library binding receipts for agent-facing memory surfaces (#898 / #896 Phase 1).
//!
//! Agents frequently conclude "there is no memory" when the process is
//! global-only (`--no-project-db`) while a workspace repo still has a real
//! `<repo>/.tachi/memory.db`. This module reports **which libraries a call
//! actually addresses** and emits loud, stable warnings when that posture is
//! unsafe for coding sessions.

use crate::MemoryServer;
use serde_json::{json, Value};

/// Stable warning id when the process is single-DB while a workspace local DB exists.
pub(crate) const WARN_SINGLE_DB_WITH_WORKSPACE_DB: &str =
    "single_db_mode while workspace has .tachi/memory.db — project memories are invisible unless you pass project=… or restart without --no-project-db";

/// Stable warning when no project library is bound and no named project could be resolved.
pub(crate) const WARN_UNSCOPED_NO_WORKSPACE: &str =
    "unscoped memory session: no project DB bound and no resolvable workspace named project";

/// Build the binding receipt for the current server + optional explicit `project=` arg.
pub(crate) fn library_binding_receipt(
    server: &MemoryServer,
    explicit_project: Option<&str>,
) -> Value {
    let global_path = server.global_db_path_buf();
    let project_path = server.project_db_path_buf();
    let single_db_mode = !server.has_project_db();
    let session_project = server.session_project();
    let explicit = explicit_project
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|s| s.to_string());
    let effective_named_project =
        crate::memory_search_ops::resolve_effective_named_project(server, explicit.as_deref());

    let workspace_git_root = crate::utils::find_project_git_root();
    let workspace_local_db = workspace_git_root
        .as_ref()
        .map(|root| root.join(".tachi").join("memory.db"));
    let workspace_local_db_exists = workspace_local_db
        .as_ref()
        .is_some_and(|path| path.exists());
    let workspace_plan_c_alias = workspace_git_root
        .as_ref()
        .and_then(|root| crate::path_utils::plan_c_dir_name_from_root(root));

    let mut warnings: Vec<String> = Vec::new();
    if single_db_mode && workspace_local_db_exists {
        warnings.push(WARN_SINGLE_DB_WITH_WORKSPACE_DB.to_string());
    }
    if single_db_mode
        && effective_named_project.is_none()
        && explicit.is_none()
        && session_project.is_none()
    {
        // Avoid double-warning when the stronger single_db+workspace warning already fired.
        if !workspace_local_db_exists {
            warnings.push(WARN_UNSCOPED_NO_WORKSPACE.to_string());
        }
    }
    if let (Some(alias), Some(explicit_name)) = (
        workspace_plan_c_alias.as_deref(),
        explicit.as_deref().or(session_project.as_deref()),
    ) {
        // Soft hint only: explicit name that is neither the plan-c alias nor a
        // known symlink target can still be a deliberate named library.
        let _ = (alias, explicit_name);
    }

    let resolved_named_path = effective_named_project.as_ref().and_then(|name| {
        crate::MemoryServer::resolve_named_project_db_path(name)
            .ok()
            .map(|path| path.display().to_string())
    });

    json!({
        "global_path": global_path.display().to_string(),
        "project_path": project_path.as_ref().map(|p| p.display().to_string()),
        "single_db_mode": single_db_mode,
        "explicit_project": explicit,
        "session_project": session_project,
        "effective_named_project": effective_named_project,
        "resolved_named_path": resolved_named_path,
        "workspace_git_root": workspace_git_root.as_ref().map(|p| p.display().to_string()),
        "workspace_local_db": workspace_local_db.as_ref().map(|p| p.display().to_string()),
        "workspace_local_db_exists": workspace_local_db_exists,
        "workspace_plan_c_alias": workspace_plan_c_alias,
        "warnings": warnings,
    })
}

/// True when the binding receipt carries the single_db+workspace warning.
#[cfg(test)]
pub(crate) fn receipt_has_single_db_workspace_warning(receipt: &Value) -> bool {
    receipt
        .get("warnings")
        .and_then(Value::as_array)
        .is_some_and(|warnings| {
            warnings.iter().any(|w| {
                w.as_str()
                    .is_some_and(|s| s.contains("single_db_mode while workspace has"))
            })
        })
}

/// Format a short markdown block for human briefing surfaces.
pub(crate) fn format_binding_markdown(receipt: &Value) -> String {
    let single = receipt
        .get("single_db_mode")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let project_path = receipt
        .get("project_path")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let effective = receipt
        .get("effective_named_project")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let global = receipt
        .get("global_path")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let mut lines = vec![format!(
        "Library binding: single_db_mode={single} project_path=`{project_path}` effective_named=`{effective}` global=`{global}`"
    )];
    if let Some(warnings) = receipt.get("warnings").and_then(Value::as_array) {
        for w in warnings {
            if let Some(text) = w.as_str() {
                lines.push(format!("[!] binding: {text}"));
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryServer;
    use memory_core::MemoryStore;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    fn workspace_local_db_for_root(root: &Path) -> PathBuf {
        root.join(".tachi").join("memory.db")
    }

    /// Serialise env-mutating tests — `TACHI_PROJECT_ROOT` is process-global.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn format_binding_markdown_includes_warnings() {
        let receipt = json!({
            "single_db_mode": true,
            "project_path": null,
            "effective_named_project": null,
            "global_path": "/tmp/global/memory.db",
            "warnings": [WARN_SINGLE_DB_WITH_WORKSPACE_DB],
        });
        let md = format_binding_markdown(&receipt);
        assert!(md.contains("single_db_mode=true"));
        assert!(md.contains("[!] binding:"));
        assert!(md.contains("single_db_mode while workspace has"));
    }

    #[test]
    fn workspace_local_db_path_is_stable() {
        let root = Path::new("/tmp/repo");
        assert_eq!(
            workspace_local_db_for_root(root),
            PathBuf::from("/tmp/repo/.tachi/memory.db")
        );
    }

    #[test]
    fn receipt_warning_detector_matches_stable_phrase() {
        let with = json!({"warnings": [WARN_SINGLE_DB_WITH_WORKSPACE_DB]});
        let without = json!({"warnings": [WARN_UNSCOPED_NO_WORKSPACE]});
        assert!(receipt_has_single_db_workspace_warning(&with));
        assert!(!receipt_has_single_db_workspace_warning(&without));
    }

    /// Discrimination (#898): single_db process + existing workspace local DB
    /// MUST warn. Project-bound process MUST NOT emit that warning.
    #[test]
    fn single_db_mode_warns_when_workspace_local_db_exists_project_bound_does_not() {
        let _guard = env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace-repo");
        std::fs::create_dir_all(workspace.join(".git")).expect("git dir");
        std::fs::create_dir_all(workspace.join(".tachi")).expect("tachi dir");
        let local_db = workspace.join(".tachi/memory.db");
        {
            // Touch a real sqlite file so `exists()` is true.
            let _store = MemoryStore::open(local_db.to_str().expect("utf8"))
                .expect("open workspace local db");
        }

        std::env::set_var("TACHI_PROJECT_ROOT", &workspace);

        let global_db = tmp.path().join("global-memory.db");
        {
            let _store =
                MemoryStore::open(global_db.to_str().expect("utf8")).expect("open global db");
        }

        // RED→GREEN for single_db posture.
        let single = MemoryServer::new(global_db.clone(), None).expect("single_db server");
        let single_receipt = library_binding_receipt(&single, None);
        assert_eq!(single_receipt["single_db_mode"], json!(true));
        assert_eq!(single_receipt["workspace_local_db_exists"], json!(true));
        assert!(
            receipt_has_single_db_workspace_warning(&single_receipt),
            "single_db + workspace db must warn; receipt={single_receipt}"
        );

        // Project-bound: same workspace, no single_db warning.
        let project_db = tmp.path().join("project-memory.db");
        {
            let _store =
                MemoryStore::open(project_db.to_str().expect("utf8")).expect("open project db");
        }
        let dual =
            MemoryServer::new(global_db, Some(project_db.clone())).expect("project-bound server");
        let dual_receipt = library_binding_receipt(&dual, None);
        assert_eq!(dual_receipt["single_db_mode"], json!(false));
        assert_eq!(
            dual_receipt["project_path"].as_str(),
            Some(project_db.to_str().expect("utf8"))
        );
        assert!(
            !receipt_has_single_db_workspace_warning(&dual_receipt),
            "project-bound must not emit single_db workspace warning; receipt={dual_receipt}"
        );

        std::env::remove_var("TACHI_PROJECT_ROOT");
    }
}
