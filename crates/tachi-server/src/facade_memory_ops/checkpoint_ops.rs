//! Checkpoint handler and Claude JSONL passive watcher.

use super::evidence_format::{
    checkpoint_message, checkpoint_saved_payload, merge_keywords, shape_save_facade_response,
    wants_json,
};
use crate::facade_save_ops::handle_tachi_save;
use crate::tool_params::*;
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub(crate) async fn handle_memory_checkpoint(
    server: &MemoryServer,
    params: TachiMemoryParams,
) -> Result<String, String> {
    let format = params.format.clone();
    let (body, display_path, already_formatted, echo) =
        save_memory_checkpoint(server, params).await?;
    if already_formatted {
        return Ok(body);
    }
    if wants_json(format.as_deref())
        || crate::facade_memory_ops::wants_full_format(format.as_deref())
    {
        return shape_save_facade_response(
            &body,
            format.as_deref(),
            echo.as_deref(),
            display_path.as_deref(),
        );
    }
    Ok(checkpoint_message(
        &body,
        display_path.as_deref(),
        false,
        echo.as_deref(),
        format.as_deref(),
    ))
}

pub(crate) async fn save_memory_checkpoint(
    server: &MemoryServer,
    mut params: TachiMemoryParams,
) -> Result<(String, Option<String>, bool, Option<String>), String> {
    if let Some(body) =
        crate::cli_client::maybe_forward_server_write(server, "tachi_memory", &params).await?
    {
        return Ok((body, None, true, params.summary.clone()));
    }

    let text = params
        .text
        .take()
        .or_else(|| params.summary.clone())
        .ok_or_else(|| "text or summary is required when action='checkpoint'".to_string())?;
    let title = params
        .title
        .take()
        .unwrap_or_else(|| "Agent checkpoint".to_string());
    let checkpoint_text = format!(
        "Checkpoint: {title}\n\n{text}\n\nRecorded at: {}",
        Utc::now().to_rfc3339()
    );
    let echo = params.summary.clone().or_else(|| Some(text.clone()));
    let path = params.path.take().or_else(|| {
        Some(format!(
            "/agent/checkpoints/{}",
            Utc::now().format("%Y-%m-%d")
        ))
    });
    let display_path = path.clone();
    let save_params = TachiSaveParams {
        text: checkpoint_text,
        id: params.id.take(),
        kind: Some("memory".to_string()),
        title: Some(title),
        summary: params.summary.take(),
        path,
        importance: params.importance.or(Some(0.8)),
        category: params
            .category
            .take()
            .or_else(|| Some("experience".to_string())),
        keywords: merge_keywords(
            std::mem::take(&mut params.keywords),
            &["checkpoint", "agent-session"],
        ),
        entities: std::mem::take(&mut params.entities),
        scope: params.scope.take().or_else(|| Some("project".to_string())),
        project: params.project.take(),
        // #1041 F2: `project` here may be genuinely caller-explicit or a
        // transport-injected session default (`checkpoint` is one of the
        // project-defaulting actions in
        // `session_identity::project_defaults_to_bound_project`) —
        // propagate the incoming signal instead of hardcoding it.
        project_explicit: params.project_explicit,
        domain: params.domain.take(),
        retention_policy: params
            .retention_policy
            .take()
            .or_else(|| Some("durable".to_string())),
        force: true,
        // tachi#1288 (Fix B): TachiMemoryParams gained a `references` field
        // mirroring `files` below — this used to be an unconditional
        // `Vec::new()` with no caller-facing field to source it from.
        references: std::mem::take(&mut params.references),
        topic: params
            .topic
            .take()
            .or_else(|| Some("checkpoint".to_string())),
        source: params
            .source
            .take()
            .or_else(|| Some("tachi_checkpoint".to_string())),
        valid_from: params.valid_from.take(),
        valid_until: params.valid_until.take(),
        metadata: params.metadata.take(),
        emit_continuity: false,
        files: Vec::new(),
        format: params.format.clone(),
    };
    let raw = handle_tachi_save(server, save_params).await?;
    Ok((raw, display_path, false, echo))
}

pub(crate) async fn capture_latest_claude_jsonl_checkpoint(
    server: &MemoryServer,
) -> Result<Option<Value>, String> {
    let watcher = claude_jsonl_passive_watcher_status();
    let Some(path) = watcher.get("latest_jsonl").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let raw = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("read {path}: {e}"))?;
    let summary = summarize_jsonl_tail(&raw);
    if summary.trim().is_empty() {
        return Ok(Some(json!({
            "status": "skipped",
            "reason": "latest JSONL transcript had no text content",
            "path": path,
        })));
    }
    let params = TachiMemoryParams {
        action: "checkpoint".to_string(),
        query: None,
        format: None,
        scope: Some("project".to_string()),
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: Some("experience".to_string()),
        include_archived: false,
        include_training: false,
        enable_rerank: false,
        synthesize: false,
        model: None,
        agent_role: None,
        text: Some(summary),
        title: Some("Claude JSONL passive checkpoint".to_string()),
        summary: Some("Passive checkpoint from latest Claude JSONL transcript".to_string()),
        topic: Some("claude-jsonl-passive-watcher".to_string()),
        keywords: vec!["claude-jsonl".to_string(), "passive-watcher".to_string()],
        entities: Vec::new(),
        importance: Some(0.65),
        retention_policy: Some("durable".to_string()),
        kind: None,
        path: None,
        id: None,
        force: true,
        source: Some("claude_jsonl_passive_watcher".to_string()),
        as_of: None,
        valid_from: None,
        valid_until: None,
        flow_id: None,
        event: None,
        state: None,
        project: None,
        project_explicit: false,
        domain: Some("agent".to_string()),
        metadata: None,
        emit_continuity: false,
        files: Vec::new(),
        references: Vec::new(),
        compact: false,
        proposal_id: None,
        review_status: None,
        notes: None,
        confirm: false,
        state_filter: None,
        content: None,
        ingest_type: "source".to_string(),
        source_url: None,
        auto_chunk: true,
        auto_summarize: true,
        auto_link: true,
        chunk_size_chars: 1200,
        chunk_overlap_chars: 120,
        conversation_id: None,
        turn_id: None,
        event_type: None,
        messages: Vec::new(),
        to: None,
        ttl_days: None,
        include_read: false,
        agent_id: None,
    };
    let (saved, display_path, already_formatted, echo) =
        save_memory_checkpoint(server, params).await?;
    Ok(Some(json!({
        "status": "captured",
        "path": path,
        "saved": checkpoint_saved_payload(&saved, already_formatted),
        "echo": echo.clone(),
        "message": checkpoint_message(&saved, display_path.as_deref(), already_formatted, echo.as_deref(), None),
    })))
}

// ---------------------------------------------------------------------------
// Claude JSONL passive watcher helpers
// ---------------------------------------------------------------------------

pub(crate) fn claude_jsonl_passive_watcher_status() -> Value {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let candidates = vec![
        home.join(".claude").join("projects"),
        home.join(".config").join("claude").join("projects"),
    ];
    let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;
    let mut visited = HashSet::new();
    for root in candidates {
        visit_jsonl_files(&root, &mut newest, 0, &mut visited);
    }
    match newest {
        Some((path, modified)) => {
            let age_seconds = modified
                .elapsed()
                .map(|dur| dur.as_secs())
                .unwrap_or_default();
            json!({
                "status": "detected",
                "latest_jsonl": path.display().to_string(),
                "age_seconds": age_seconds,
                "note": "passive watcher is diagnostic-only; use capture_session or checkpoint for durable writes",
            })
        }
        None => json!({
            "status": "not_detected",
            "note": "no Claude JSONL transcript files found under known roots",
        }),
    }
}

fn visit_jsonl_files(
    root: &Path,
    newest: &mut Option<(PathBuf, std::time::SystemTime)>,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
) {
    if depth > 4 || !root.exists() {
        return;
    }
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if !visited.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            visit_jsonl_files(&path, newest, depth + 1, visited);
            continue;
        }
        if file_type.is_symlink() {
            let Ok(link_meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if link_meta.is_dir() {
                visit_jsonl_files(&path, newest, depth + 1, visited);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                consider_jsonl_file(&path, newest);
            }
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        consider_jsonl_file(&path, newest);
    }
}

fn consider_jsonl_file(path: &Path, newest: &mut Option<(PathBuf, std::time::SystemTime)>) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let Ok(modified) = meta.modified() else {
        return;
    };
    let should_replace = newest
        .as_ref()
        .map(|(_, current)| modified > *current)
        .unwrap_or(true);
    if should_replace {
        *newest = Some((path.to_path_buf(), modified));
    }
}

fn summarize_jsonl_tail(raw: &str) -> String {
    let mut snippets = Vec::new();
    let tail_lines: Vec<&str> = raw.lines().collect();
    let start = tail_lines.len().saturating_sub(80);
    for line in &tail_lines[start..] {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(text) = extract_jsonl_text(&value) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                snippets.push(trimmed.chars().take(500).collect::<String>());
            }
        }
    }
    snippets
        .into_iter()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn extract_jsonl_text(value: &Value) -> Option<String> {
    value
        .pointer("/message/content")
        .and_then(text_from_jsonl_content)
        .or_else(|| value.get("content").and_then(text_from_jsonl_content))
        .or_else(|| {
            value
                .pointer("/message/text")
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .or_else(|| {
            value
                .get("text")
                .and_then(|v| v.as_str().map(str::to_string))
        })
}

fn text_from_jsonl_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("content").and_then(|v| v.as_str()))
                })
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facade_memory_ops::evidence_format::checkpoint_saved_payload;

    #[test]
    fn checkpoint_forwarded_message_is_preserved() {
        let body = "Saved -> `/agent/checkpoints/2026-05-31` (id: `cp-123`, status: saved)";

        assert_eq!(
            checkpoint_message(body, None, true, Some("ignored forwarded echo"), None),
            "Saved -> `/agent/checkpoints/2026-05-31` (id: `cp-123`, status: saved)"
        );
    }

    #[test]
    fn checkpoint_forwarded_saved_payload_is_null() {
        let body = "Saved -> `/agent/checkpoints/2026-05-31` (id: `cp-123`, status: saved)";

        assert_eq!(checkpoint_saved_payload(body, true), Value::Null);
    }
}
