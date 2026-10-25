use super::handler::handle_save_memory;
use crate::pipeline_ops::helpers::chunk_text;
use crate::tool_params::{RememberParams, SaveMemoryParams};
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};

/// Long remembers above this char count are split into answer-sized units
/// (tachi#2115). LoCoMo turn-level ingest (F1≈0.51 single-hop) vs LongMemEval
/// whole-session blobs (F1≈0.05 under short excerpts) shows write granularity
/// dominates QA. Competitors: Graphiti CHUNK_* (~3k tokens), Mem0/Hy extract.
const AUTO_CHUNK_THRESHOLD_CHARS: usize = 2_000;

/// Target chunk size once auto-chunking kicks in (chars, not tokens).
const AUTO_CHUNK_SIZE_CHARS: usize = 1_200;

/// Overlap between adjacent fixed-window chunks.
const AUTO_CHUNK_OVERLAP_CHARS: usize = 120;

/// Soft cap so a pathological paste cannot explode into thousands of rows.
const AUTO_CHUNK_MAX_CHUNKS: usize = 64;

fn auto_chunk_enabled() -> bool {
    !matches!(
        std::env::var("TACHI_REMEMBER_AUTO_CHUNK")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Split long remember text into answer-sized chunks.
///
/// Prefer turn/message boundaries (`user:` / `assistant:` / `system:` line
/// starts) when present; otherwise fall back to overlapping fixed windows
/// via [`chunk_text`]. Texts at or under [`AUTO_CHUNK_THRESHOLD_CHARS`] are
/// returned unchanged as a single element.
pub(crate) fn split_remember_text(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.chars().count() <= AUTO_CHUNK_THRESHOLD_CHARS {
        return vec![trimmed.to_string()];
    }

    let turn_chunks = split_on_turn_markers(trimmed);
    let mut chunks = if turn_chunks.len() > 1 {
        merge_small_chunks(turn_chunks, AUTO_CHUNK_SIZE_CHARS)
    } else {
        chunk_text(trimmed, AUTO_CHUNK_SIZE_CHARS, AUTO_CHUNK_OVERLAP_CHARS)
    };

    if chunks.len() > AUTO_CHUNK_MAX_CHUNKS {
        // Collapse the tail into one final chunk so we stay bounded.
        let mut head: Vec<String> = chunks.drain(..AUTO_CHUNK_MAX_CHUNKS - 1).collect();
        let tail = chunks.join("\n\n");
        head.push(tail);
        chunks = head;
    }

    if chunks.is_empty() {
        chunks.push(trimmed.to_string());
    }
    chunks
}

fn split_on_turn_markers(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let is_turn_start = {
            let trimmed = line.trim_start();
            let lower = trimmed.to_ascii_lowercase();
            lower.starts_with("user:")
                || lower.starts_with("assistant:")
                || lower.starts_with("system:")
                || lower.starts_with("human:")
                || lower.starts_with("ai:")
        };
        if is_turn_start && !current.trim().is_empty() {
            chunks.push(current.trim().to_string());
            current.clear();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    chunks
}

fn merge_small_chunks(parts: Vec<String>, target_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    for part in parts {
        let part_len = part.chars().count();
        let buf_len = buf.chars().count();
        if !buf.is_empty() && buf_len + 1 + part_len > target_chars {
            out.push(buf);
            buf = part;
        } else if buf.is_empty() {
            buf = part;
        } else {
            buf.push('\n');
            buf.push_str(&part);
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

fn chunk_child_path(parent: &str, index: usize) -> String {
    let parent = parent.trim_end_matches('/');
    format!("{parent}/c{index:03}")
}

/// Low-friction shortcut over `handle_save_memory`. Infers `path`, `category`,
/// and `importance` so callers only need to pass `text` (and optionally
/// `tags`). Internally constructs `SaveMemoryParams` and delegates, so noise
/// filter, capture gate, provenance injection, auto-link, and the enrichment
/// batcher all run identically to a direct save_memory call.
///
/// When `text` exceeds [`AUTO_CHUNK_THRESHOLD_CHARS`] (and auto-chunk is not
/// disabled via `TACHI_REMEMBER_AUTO_CHUNK=0`), the body is split into
/// answer-sized child rows under `{path}/cNNN` with `metadata.parent_path` /
/// `chunk_index` / `chunk_total` linking them (tachi#2115).
pub(crate) async fn handle_remember(
    server: &MemoryServer,
    params: RememberParams,
) -> Result<String, String> {
    // Default path = /notes/{YYYY-MM-DD} so quick captures land in a
    // predictable, browsable bucket without forcing the caller to choose one.
    let inferred_path = params.path.clone().unwrap_or_else(|| {
        let date = Utc::now().format("%Y-%m-%d");
        format!("/notes/{date}")
    });

    let chunks = if auto_chunk_enabled() {
        split_remember_text(&params.text)
    } else {
        vec![params.text.trim().to_string()]
    };

    if chunks.len() <= 1 {
        let text = chunks
            .into_iter()
            .next()
            .unwrap_or_else(|| params.text.trim().to_string());
        let save_params = build_save_params(&params, inferred_path, text, None);
        return handle_save_memory(server, save_params).await;
    }

    let chunk_total = chunks.len();
    let mut saved: Vec<Value> = Vec::with_capacity(chunk_total);
    let mut first_id = String::new();
    let mut first_path = String::new();
    let mut first_db: Option<Value> = None;

    for (index, chunk) in chunks.into_iter().enumerate() {
        let child_path = chunk_child_path(&inferred_path, index);
        let meta = json!({
            "shortcut": "remember",
            "auto_chunked": true,
            "parent_path": inferred_path,
            "chunk_index": index,
            "chunk_total": chunk_total,
        });
        let save_params = build_save_params(&params, child_path.clone(), chunk, Some(meta));
        let raw = handle_save_memory(server, save_params).await?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("auto-chunk save returned non-JSON: {e}; raw={raw}"))?;
        if index == 0 {
            first_id = parsed
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            first_path = child_path.clone();
        }
        saved.push(json!({
            "id": parsed.get("id"),
            "path": parsed.get("path").cloned().unwrap_or(json!(child_path)),
            "chunk_index": index,
            "status": parsed.get("status"),
        }));
        if index == 0 {
            if let Some(db) = parsed.get("db").cloned() {
                first_db = Some(db);
            }
        }
    }

    let mut response = json!({
        "id": first_id,
        "path": first_path,
        "parent_path": inferred_path,
        "status": "saved",
        "auto_chunked": true,
        "chunk_count": chunk_total,
        "chunks": saved,
    });
    if let Some(db) = first_db {
        response
            .as_object_mut()
            .expect("response object")
            .insert("db".into(), db);
    }
    Ok(response.to_string())
}

fn build_save_params(
    params: &RememberParams,
    path: String,
    text: String,
    metadata: Option<Value>,
) -> SaveMemoryParams {
    let metadata = metadata.unwrap_or_else(|| json!({ "shortcut": "remember" }));
    SaveMemoryParams {
        text,
        summary: params.summary.clone(),
        path,
        importance: params.importance.unwrap_or(0.6).clamp(0.0, 1.0),
        category: params
            .category
            .clone()
            .unwrap_or_else(|| "fact".to_string()),
        topic: params.topic.clone(),
        keywords: params.tags.clone(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: String::new(),
        scope: params
            .scope
            .clone()
            .unwrap_or_else(|| "project".to_string()),
        vector: None,
        id: None,
        force: params.force,
        auto_link: true,
        project: params.project.clone(),
        project_explicit: params.project_explicit,
        retention_policy: params.retention_policy.clone(),
        domain: params.domain.clone(),
        timestamp: None,
        valid_from: params.valid_from.clone(),
        valid_until: params.valid_until.clone(),
        metadata: Some(metadata),
        emit_continuity: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_not_chunked() {
        let text = "short note under threshold";
        let chunks = split_remember_text(text);
        assert_eq!(chunks, vec![text.to_string()]);
    }

    #[test]
    fn long_plain_text_uses_fixed_windows_with_overlap_shape() {
        let text = "a".repeat(AUTO_CHUNK_THRESHOLD_CHARS + 500);
        let chunks = split_remember_text(&text);
        assert!(
            chunks.len() >= 2,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        assert!(chunks.len() <= AUTO_CHUNK_MAX_CHUNKS);
        // Each non-final chunk should be near the target size.
        for (i, chunk) in chunks
            .iter()
            .enumerate()
            .take(chunks.len().saturating_sub(1))
        {
            let n = chunk.chars().count();
            assert!(n <= AUTO_CHUNK_SIZE_CHARS, "chunk {i} too large: {n}");
            assert!(
                n >= AUTO_CHUNK_SIZE_CHARS / 2,
                "chunk {i} unexpectedly tiny: {n}"
            );
        }
        let rejoined: String = chunks.concat();
        assert!(rejoined.contains("aaa"));
    }

    #[test]
    fn turn_markers_drive_split_and_parent_link_paths() {
        let mut body = String::from("SESSION abc\nsession_id=abc\ndate=2023-01-01\n");
        for i in 0..30 {
            body.push_str(&format!(
                "user: question number {i} with filler text to grow\n"
            ));
            body.push_str(&format!(
                "assistant: answer number {i} containing UNIQUE_NEEDLE_{i} and more filler\n"
            ));
        }
        assert!(body.chars().count() > AUTO_CHUNK_THRESHOLD_CHARS);
        let chunks = split_remember_text(&body);
        assert!(chunks.len() >= 2, "turn split should yield multiple chunks");
        assert!(
            chunks.iter().any(|c| c.contains("UNIQUE_NEEDLE_29")),
            "last needle must survive chunking"
        );
        assert!(
            chunks.iter().any(|c| c.contains("UNIQUE_NEEDLE_0")),
            "first needle must survive chunking"
        );
        // Child path helper keeps parent id discoverable in path prefix.
        let parent = "/facts/lme/qid/abc";
        let child = chunk_child_path(parent, 3);
        assert_eq!(child, "/facts/lme/qid/abc/c003");
        assert!(child.contains("abc"));
    }

    #[test]
    fn max_chunks_cap_collapses_tail() {
        // Force many tiny turn markers so merge still exceeds max after merge.
        let mut body = String::from("hdr\n");
        for i in 0..500 {
            body.push_str(&format!("user: u{i}\nassistant: a{i}\n"));
        }
        let chunks = split_remember_text(&body);
        assert!(chunks.len() <= AUTO_CHUNK_MAX_CHUNKS);
        assert!(
            chunks.last().unwrap().contains("a499") || chunks.iter().any(|c| c.contains("a499"))
        );
    }
}
