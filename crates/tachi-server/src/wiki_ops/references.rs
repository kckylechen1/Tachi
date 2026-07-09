use super::*;

pub(super) fn validate_reference_format(reference: &str) -> Result<(), String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err("Reference cannot be empty".to_string());
    }

    if let Some(rest) = trimmed.strip_prefix("http://") {
        if !rest.is_empty() {
            return Ok(());
        }
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        if !rest.is_empty() {
            return Ok(());
        }
    } else if let Some(rest) = trimmed.strip_prefix("file://") {
        if !rest.is_empty() {
            return Ok(());
        }
    }

    if trimmed.starts_with('/') && trimmed.len() > 1 {
        return Ok(());
    }

    // Repo-relative spec paths (Issue → Doc edges in #150)
    if trimmed.starts_with("docs/")
        || trimmed.starts_with("docs\\")
        || trimmed.starts_with("skill/")
        || trimmed.starts_with("skill\\")
    {
        return Ok(());
    }

    static WIN_PATH_RE: OnceLock<regex::Regex> = OnceLock::new();
    let win_re = WIN_PATH_RE.get_or_init(|| regex::Regex::new(r"^[a-zA-Z]:[/\\]").unwrap());
    if win_re.is_match(trimmed) {
        return Ok(());
    }

    static GH_SHORTHAND_RE: OnceLock<regex::Regex> = OnceLock::new();
    let gh_re = GH_SHORTHAND_RE.get_or_init(|| {
        regex::Regex::new(r"^(?:#\d+|[a-zA-Z0-9_.-]+#\d+|[a-zA-Z0-9_-]+/[a-zA-Z0-9_.-]+#\d+)$")
            .unwrap()
    });
    if gh_re.is_match(trimmed) {
        return Ok(());
    }

    Err(format!(
        "Invalid reference format: '{trimmed}'. Expected URL (http/https/file), absolute path, or GitHub shorthand (#N, repo#N, owner/repo#N)"
    ))
}

pub(crate) fn validate_references(references: &[String]) -> Result<(), String> {
    for (i, reference) in references.iter().enumerate() {
        validate_reference_format(reference).map_err(|e| format!("references[{i}]: {e}"))?;
    }
    Ok(())
}
