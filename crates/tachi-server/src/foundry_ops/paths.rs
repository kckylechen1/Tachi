use crate::utils::find_git_root;

fn resolve_foundry_input_path(path: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Input path cannot be empty".to_string());
    }

    let raw = std::path::Path::new(trimmed);
    let candidates = if raw.is_absolute() {
        vec![raw.to_path_buf()]
    } else {
        let cwd = std::env::current_dir()
            .map_err(|e| format!("Failed to resolve current directory: {e}"))?;
        let mut candidates = vec![cwd.join(raw)];
        if let Some(git_root) = find_git_root() {
            let repo_candidate = git_root.join(raw);
            if !candidates
                .iter()
                .any(|candidate| candidate == &repo_candidate)
            {
                candidates.push(repo_candidate);
            }
        }
        candidates
    };

    let allowed_roots = projection_allowed_roots();
    for candidate in candidates {
        let canonical = match std::fs::canonicalize(&candidate) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !allowed_roots.iter().any(|root| canonical.starts_with(root)) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&candidate).map_err(|e| {
            format!(
                "Failed to stat evolution input path '{}': {e}",
                candidate.display()
            )
        })?;
        if metadata.file_type().is_symlink()
            && !allowed_roots.iter().any(|root| canonical.starts_with(root))
        {
            return Err(format!(
                "Evolution input path '{}' resolves outside allowed roots",
                candidate.display()
            ));
        }
        if !canonical.is_file() {
            return Err(format!(
                "Evolution input path '{}' is not a file",
                canonical.display()
            ));
        }
        return Ok(canonical);
    }

    Err(format!(
        "Evolution input path '{}' does not exist inside allowed roots",
        trimmed
    ))
}

pub(super) fn read_foundry_input_text(path: &str) -> Result<(String, String), String> {
    let resolved = resolve_foundry_input_path(path)?;
    let content = std::fs::read_to_string(&resolved).map_err(|e| {
        format!(
            "Failed to read evolution input file '{}': {e}",
            resolved.display()
        )
    })?;
    Ok((resolved.display().to_string(), content))
}

fn projection_allowed_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Some(workspace_root) = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
    {
        roots.push(workspace_root.to_path_buf());
    }
    if let Some(git_root) = find_git_root() {
        roots.push(git_root);
    }
    if let Some(home) = dirs::home_dir() {
        for suffix in [".openclaw", ".claude", ".codex", ".cursor", ".agents"] {
            let root = home.join(suffix);
            if root.exists() {
                roots.push(root);
            }
        }
    }

    let mut canonical = roots
        .into_iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect::<Vec<_>>();
    canonical.sort();
    canonical.dedup();
    canonical
}
