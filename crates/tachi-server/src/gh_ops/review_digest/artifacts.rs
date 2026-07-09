use super::super::*;
use super::render::render_pr_review_digest_markdown;

pub(in crate::gh_ops) fn review_digest_root() -> PathBuf {
    if let Ok(root) = std::env::var("TACHI_REVIEW_ROOT") {
        return PathBuf::from(root);
    }
    if let Ok(cwd) = std::env::current_dir() {
        return cwd.join(".tachi").join("reviews");
    }
    std::env::temp_dir().join("tachi").join("reviews")
}

pub(in crate::gh_ops) fn safe_path_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

pub(in crate::gh_ops) fn repo_review_segment(repo: &str) -> String {
    repo.split('/')
        .map(safe_path_segment)
        .collect::<Vec<_>>()
        .join("__")
}

pub(in crate::gh_ops) fn write_pr_review_digest_artifacts(digest: &Value) -> Result<Value, String> {
    let repo = digest
        .get("repo")
        .and_then(Value::as_str)
        .ok_or("digest missing repo")?;
    let pr_number = digest
        .get("pr_number")
        .and_then(Value::as_u64)
        .ok_or("digest missing pr_number")?;
    let dir = review_digest_root()
        .join(repo_review_segment(repo))
        .join(format!("pr-{pr_number}"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create review digest dir {}: {e}", dir.display()))?;
    let digest_json_path = dir.join("digest.json");
    let digest_md_path = dir.join("digest.md");
    let serialized =
        serde_json::to_string_pretty(digest).map_err(|e| format!("serialize digest: {e}"))?;
    crate::utils::write_owner_only_file_atomic(
        &digest_json_path,
        format!("{serialized}\n").as_bytes(),
    )
    .map_err(|e| format!("write {}: {e}", digest_json_path.display()))?;
    let markdown = render_pr_review_digest_markdown(digest);
    crate::utils::write_owner_only_file_atomic(&digest_md_path, markdown.as_bytes())
        .map_err(|e| format!("write {}: {e}", digest_md_path.display()))?;
    Ok(json!({
        "digest_dir": dir,
        "digest_json_path": digest_json_path,
        "digest_md_path": digest_md_path,
    }))
}
