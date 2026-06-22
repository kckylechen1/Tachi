use crate::server_state::MemoryServer;
use crate::tool_params::InitProjectDbParams;
use crate::utils::find_git_root;
use serde_json::json;
use std::path::PathBuf;

pub(crate) async fn handle_tachi_init_project_db(
    server: &MemoryServer,
    params: InitProjectDbParams,
) -> Result<String, String> {
    let project_root = match params.project_root.as_deref() {
        Some(raw) => PathBuf::from(raw),
        None => find_git_root().ok_or_else(|| {
            "No git repository detected. Provide project_root explicitly.".to_string()
        })?,
    };

    if !project_root.join(".git").exists() {
        return Err(format!(
            "Target project root '{}' is not a git repository",
            project_root.display()
        ));
    }

    let rel = PathBuf::from(&params.db_relpath);
    let db_path = crate::path_utils::resolve_project_db_path(&project_root, &rel)?;
    let existed = db_path.exists();
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create project db dir: {e}"))?;
    }

    // Hot-activate the project DB on the running server (no restart needed)
    let was_new_activation = server.activate_project_db(db_path.clone())?;

    let mut plan_c_note: Option<String> = None;
    if let Some(safe_name) = crate::path_utils::plan_c_dir_name_from_root(&project_root) {
        let global_link = crate::path_utils::plan_c_global_db_path(&safe_name);
        #[cfg(unix)]
        {
            match crate::path_utils::ensure_plan_c_symlink(&db_path, &project_root) {
                crate::path_utils::PlanCLinkOutcome::SplitBrain(issue) => {
                    plan_c_note = Some(issue.warning_message());
                }
                crate::path_utils::PlanCLinkOutcome::Failed { path, error } => {
                    plan_c_note = Some(format!(
                        "Plan C global symlink failed at {}: {}",
                        path.display(),
                        error
                    ));
                }
                _ => {
                    plan_c_note = Some(format!(
                        "Global symlink: {} -> {}",
                        global_link.display(),
                        db_path.display()
                    ));
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = safe_name;
            plan_c_note = Some(
                "Plan C global symlink skipped on non-Unix hosts; use db_path directly."
                    .to_string(),
            );
        }
    }

    let activation_note = if was_new_activation {
        "Project DB is now active on this server instance. No restart needed."
    } else {
        "Project DB was already active; re-opened with latest state."
    };
    let note = match plan_c_note {
        Some(plan_c) => format!("{activation_note} {plan_c}"),
        None => activation_note.to_string(),
    };

    serde_json::to_string(&json!({
        "initialized": true,
        "created": !existed,
        "active": true,
        "hot_activated": was_new_activation,
        "project_root": project_root.display().to_string(),
        "db_path": db_path.display().to_string(),
        "db_relpath": rel.display().to_string(),
        "plan_c_split_brain": crate::path_utils::plan_c_split_brain(&db_path, &project_root),
        "note": note,
    }))
    .map_err(|e| format!("serialize: {e}"))
}
