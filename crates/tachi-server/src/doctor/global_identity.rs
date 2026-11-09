//! #2228: surface the condition under which workspace registration fails
//! closed on an ambiguous global store, naming each candidate file. Uses the
//! registration's own candidate set so the two can never disagree.
//! Report-only: never moves or deletes anything.

use std::path::Path;

use super::DoctorWarning;

pub fn global_store_ambiguity_warnings(tachi_home: &Path) -> Vec<DoctorWarning> {
    let candidates = crate::project_db_ops::global_store_candidates(tachi_home);
    if candidates.len() <= 1 {
        return Vec::new();
    }
    let described = candidates
        .iter()
        .map(|(path, canonical)| describe_candidate(path, canonical))
        .collect::<Vec<_>>()
        .join("; ");
    vec![DoctorWarning {
        code: "ambiguous_global_store".to_string(),
        path: tachi_home.display().to_string(),
        message: format!(
            "{} distinct global store candidates under {}: {described}. Workspace binding and project registration fail closed until only one remains",
            candidates.len(),
            tachi_home.display()
        ),
        remediation: "keep the canonical global store (global/tachi-memory.db); after confirming no process holds it (lsof), move the stray candidate aside (often a 0-byte legacy memory.db). A compat symlink to the canonical store is not a candidate".to_string(),
    }]
}

fn describe_candidate(path: &Path, canonical: &Path) -> String {
    let kind = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            format!("symlink -> {}", canonical.display())
        }
        Ok(meta) if meta.is_file() => "regular file".to_string(),
        Ok(_) => "non-regular file".to_string(),
        Err(_) => "unreadable".to_string(),
    };
    let meta = std::fs::metadata(path).ok();
    let size = meta
        .as_ref()
        .map(|meta| format!("{} bytes", meta.len()))
        .unwrap_or_else(|| "size unknown".to_string());
    let modified = meta
        .and_then(|meta| meta.modified().ok())
        .map(|time| {
            chrono::DateTime::<chrono::Utc>::from(time)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|| "mtime unknown".to_string());
    format!("{} ({kind}, {size}, modified {modified})", path.display())
}
