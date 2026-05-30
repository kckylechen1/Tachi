use super::*;

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
    if rel.is_absolute() {
        return Err("db_relpath must be relative to project_root".to_string());
    }

    let db_path = project_root.join(&rel);
    let existed = db_path.exists();
    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create project db dir: {e}"))?;
    }

    // Hot-activate the project DB on the running server (no restart needed)
    let was_new_activation = server.activate_project_db(db_path.clone())?;

    // Implement Plan C: Symlink at ~/.tachi/projects/{name}/memory.db -> project-local DB
    if let Some(project_name_os) = project_root.file_name() {
        if let Some(project_name) = project_name_os.to_str() {
            let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
            let app_home = std::env::var("TACHI_HOME")
                .map(|v| {
                    if v.starts_with("~/") {
                        home.join(&v[2..])
                    } else {
                        PathBuf::from(v)
                    }
                })
                .unwrap_or_else(|_| home.join(".tachi"));
            let global_project_dir = app_home.join("projects").join(project_name);
            tokio::fs::create_dir_all(&global_project_dir)
                .await
                .map_err(|e| format!("create global project dir: {e}"))?;
            let global_link = global_project_dir.join("memory.db");
            if global_link.exists() || global_link.is_symlink() {
                let _ = tokio::fs::remove_file(&global_link).await;
            }
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&db_path, &global_link)
                    .map_err(|e| format!("create project symlink: {e}"))?;
            }
        }
    }

    serde_json::to_string(&json!({
        "initialized": true,
        "created": !existed,
        "active": true,
        "hot_activated": was_new_activation,
        "project_root": project_root.display().to_string(),
        "db_path": db_path.display().to_string(),
        "db_relpath": rel.display().to_string(),
        "note": if was_new_activation {
            "Project DB is now active on this server instance. No restart needed."
        } else {
            "Project DB was already active; re-opened with latest state."
        },
    }))
    .map_err(|e| format!("serialize: {e}"))
}
