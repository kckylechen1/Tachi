//! Filesystem helpers for the LLM-call recorder: label sanitization and
//! the owner-only write helpers that back `prompt.md` / `result.md` /
//! `status.json`. Migrated verbatim from the pre-#1261 `claude_pool::files`
//! — the on-disk artifact contract is unchanged by the decommission.

use std::path::PathBuf;

use serde_json::Value;

use crate::runtime_files::{write_owner_only_file, write_owner_only_file_atomic};

const MAX_LABEL_LEN: usize = 48;
const HASH_LEN: usize = 8;

pub(super) fn sanitize_label(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        return "run".to_string();
    }
    if trimmed.chars().count() <= MAX_LABEL_LEN {
        return trimmed.to_string();
    }
    // Label exceeds the limit: keep a readable prefix and append a short hash
    // of the full label so two distinct labels that share the first 48 chars
    // don't collide on disk.
    let prefix_len = MAX_LABEL_LEN - HASH_LEN - 1;
    let prefix = readable_truncated_prefix(trimmed, prefix_len);
    format!("{}-{}", prefix, short_hash(label))
}

fn readable_truncated_prefix(label: &str, max_chars: usize) -> String {
    let hard_prefix: String = label.chars().take(max_chars).collect();
    if hard_prefix.chars().count() < max_chars {
        return hard_prefix.trim_end_matches(['-', '_']).to_string();
    }

    if let Some((idx, _)) = hard_prefix
        .char_indices()
        .rev()
        .find(|(_, c)| *c == '-' || *c == '_')
    {
        let boundary_prefix = hard_prefix[..idx].trim_end_matches(['-', '_']);
        // Avoid reducing labels like "aaaaaaaa..." to a tiny prefix just
        // because an early separator exists; fall back to the hard cap there.
        if boundary_prefix.chars().count() >= max_chars / 2 {
            return boundary_prefix.to_string();
        }
    }

    hard_prefix.trim_end_matches(['-', '_']).to_string()
}

fn short_hash(s: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

pub(super) async fn write_owner_only_file_blocking(
    path: PathBuf,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let display = path.display().to_string();
    tokio::task::spawn_blocking(move || write_owner_only_file(&path, &bytes))
        .await
        .map_err(|error| format!("owner-only write task failed for {display}: {error}"))?
}

pub(super) async fn write_run_file_blocking(path: PathBuf, body: String) -> Result<(), String> {
    let display = path.display().to_string();
    tokio::task::spawn_blocking(move || write_owner_only_file_atomic(&path, body.as_bytes()))
        .await
        .map_err(|error| format!("run-file write task failed for {display}: {error}"))?
}

pub(super) async fn write_run_status_file_blocking(
    run_dir: PathBuf,
    status: Value,
) -> Result<(), String> {
    let display = run_dir.display().to_string();
    tokio::task::spawn_blocking(move || {
        crate::runtime_files::write_run_status_file(&run_dir, &status)
    })
    .await
    .map_err(|error| format!("status write task failed for {display}: {error}"))?
}
