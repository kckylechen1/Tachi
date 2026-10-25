use super::handler::handle_save_memory;
use crate::memory_search_ops::text_scrub::{scrub_secrets, scrub_think_tags};
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

/// Sanitize the full logical remember input once before any split.
///
/// tachi#2120: think/secret scrubbing must run on the whole body first.
/// Splitting first can sever `<think>…</think>` or a long bearer across
/// chunk boundaries so per-child scrub misses the match and leaks content.
fn sanitize_logical_input(text: &str) -> (String, usize) {
    let without_think = scrub_think_tags(text);
    scrub_secrets(&without_think)
}

/// Split long remember text into answer-sized chunks.
///
/// Prefer turn/message boundaries (`user:` / `assistant:` / `system:` line
/// starts) when present; otherwise fall back to overlapping fixed windows
/// via [`chunk_text`]. Texts at or under [`AUTO_CHUNK_THRESHOLD_CHARS`] are
/// returned unchanged as a single element.
///
/// Callers that feed untrusted model output MUST run [`sanitize_logical_input`]
/// first (see [`handle_remember`]).
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

/// True when a child `handle_save_memory` JSON body represents a persisted row.
///
/// tachi#2120: `Ok({saved:false,…})` (noise / capture-gate / duplicate) must
/// never be counted as saved.
fn child_outcome_saved(parsed: &Value) -> bool {
    if parsed.get("saved").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    matches!(
        parsed.get("status").and_then(Value::as_str),
        Some("saved")
    )
}

fn child_status_label(parsed: &Value) -> String {
    if let Some(status) = parsed.get("status").and_then(Value::as_str) {
        return status.to_string();
    }
    if parsed.get("saved").and_then(Value::as_bool) == Some(false) {
        if parsed.get("noise").and_then(Value::as_bool) == Some(true) {
            return "rejected_noise".to_string();
        }
        if parsed.get("rejected_by").is_some() {
            return "rejected_capture_gate".to_string();
        }
        return "rejected".to_string();
    }
    "unknown".to_string()
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
///
/// tachi#2120: sanitization runs once on the full logical input **before**
/// split; aggregate receipts report honest saved/partial/rejected status and
/// expose per-chunk metadata (the top-level `id` is the first *saved* child,
/// not a reconstruction of the original input).
pub(crate) async fn handle_remember(
    server: &MemoryServer,
    mut params: RememberParams,
) -> Result<String, String> {
    // Default path = /notes/{YYYY-MM-DD} so quick captures land in a
    // predictable, browsable bucket without forcing the caller to choose one.
    let inferred_path = params.path.clone().unwrap_or_else(|| {
        let date = Utc::now().format("%Y-%m-%d");
        format!("/notes/{date}")
    });

    let (safe_text, pre_secret_redactions) = sanitize_logical_input(&params.text);
    params.text = safe_text;
    if !params.summary.is_empty() {
        let (safe_summary, _) = sanitize_logical_input(&params.summary);
        params.summary = safe_summary;
    }

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
        let raw = handle_save_memory(server, save_params).await?;
        // Preserve pre-split redaction evidence even on the single-child path
        // when the child response omitted it (idempotent re-scrub may report 0).
        if pre_secret_redactions == 0 {
            return Ok(raw);
        }
        let mut parsed: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("remember save returned non-JSON: {e}; raw={raw}"))?;
        if let Some(obj) = parsed.as_object_mut() {
            let existing = obj
                .get("secret_redactions")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let total = existing.max(pre_secret_redactions);
            obj.insert("secret_redactions".into(), json!(total));
            obj.insert(
                "secret_redaction_warning".into(),
                json!("Potential secrets were redacted before persistence."),
            );
        }
        return Ok(parsed.to_string());
    }

    let chunk_total = chunks.len();
    let mut chunk_receipts: Vec<Value> = Vec::with_capacity(chunk_total);
    let mut saved_ids: Vec<String> = Vec::new();
    let mut first_saved_id = String::new();
    let mut first_saved_path = String::new();
    let mut first_db: Option<Value> = None;
    let mut scope_warning: Option<String> = None;
    let mut scope_requested: Option<String> = None;
    let mut warning: Option<String> = None;
    let mut aggregate_secret_redactions = pre_secret_redactions;

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

        let persisted = child_outcome_saved(&parsed);
        let status_label = child_status_label(&parsed);
        let child_id = parsed
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        if persisted {
            if first_saved_id.is_empty() {
                first_saved_id = child_id.clone();
                first_saved_path = parsed
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(&child_path)
                    .to_string();
                if let Some(db) = parsed.get("db").cloned() {
                    first_db = Some(db);
                }
            }
            saved_ids.push(child_id.clone());
            if scope_warning.is_none() {
                if let Some(sw) = parsed
                    .get("scope_warning")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    scope_warning = Some(sw);
                    scope_requested = parsed
                        .get("scope")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            }
            if warning.is_none() {
                warning = parsed
                    .get("warning")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }

        if let Some(n) = parsed.get("secret_redactions").and_then(Value::as_u64) {
            aggregate_secret_redactions = aggregate_secret_redactions.max(n as usize);
        }

        let mut receipt = json!({
            "id": if child_id.is_empty() { Value::Null } else { json!(child_id) },
            "path": parsed.get("path").cloned().unwrap_or(json!(child_path)),
            "chunk_index": index,
            "chunk_total": chunk_total,
            "saved": persisted,
            "status": status_label,
        });
        if let Some(obj) = receipt.as_object_mut() {
            if let Some(reason) = parsed.get("reason").cloned() {
                obj.insert("reason".into(), reason);
            }
            if let Some(rejected_by) = parsed.get("rejected_by").cloned() {
                obj.insert("rejected_by".into(), rejected_by);
            }
            if let Some(noise) = parsed.get("noise").cloned() {
                obj.insert("noise".into(), noise);
            }
        }
        chunk_receipts.push(receipt);
    }

    let saved_count = saved_ids.len();
    let aggregate_status = if saved_count == 0 {
        "rejected"
    } else if saved_count == chunk_total {
        "saved"
    } else {
        "partial"
    };

    let mut response = json!({
        "id": if first_saved_id.is_empty() { Value::Null } else { json!(first_saved_id) },
        "path": if first_saved_path.is_empty() { Value::Null } else { json!(first_saved_path) },
        "parent_path": inferred_path,
        "status": aggregate_status,
        "saved": saved_count > 0,
        "auto_chunked": true,
        "chunk_count": saved_count,
        "chunk_attempted": chunk_total,
        "chunk_ids": saved_ids,
        "chunks": chunk_receipts,
        // Explicit: top-level id is the first persisted child only. A later
        // get_memory(id) returns that child body, not the original input.
        "id_is_first_saved_chunk": true,
        "original_input_reconstructible": false,
    });
    if let Some(obj) = response.as_object_mut() {
        if let Some(db) = first_db {
            obj.insert("db".into(), db);
        }
        if let Some(sw) = scope_warning {
            if let Some(scope) = scope_requested {
                obj.insert("scope".into(), json!(scope));
            }
            obj.insert("scope_warning".into(), json!(sw));
        }
        if let Some(w) = warning {
            obj.insert("warning".into(), json!(w));
        }
        if aggregate_secret_redactions > 0 {
            obj.insert(
                "secret_redactions".into(),
                json!(aggregate_secret_redactions),
            );
            obj.insert(
                "secret_redaction_warning".into(),
                json!("Potential secrets were redacted before persistence."),
            );
        }
        if aggregate_status != "saved" {
            obj.insert(
                "hint".into(),
                json!(match aggregate_status {
                    "rejected" => {
                        "No chunks were persisted (noise/capture-gate/duplicate). Retry with force=true if intentional, or inspect chunks[]."
                    }
                    "partial" => {
                        "Some chunks were rejected. Inspect chunks[] for per-child status; chunk_count counts only persisted rows."
                    }
                    _ => "",
                }),
            );
        }
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
    use crate::tool_params::RememberParams;

    fn remember_params(text: String, force: bool, path: &str) -> RememberParams {
        RememberParams {
            text,
            summary: String::new(),
            tags: vec!["auto-chunk-test".to_string()],
            topic: String::new(),
            importance: Some(0.6),
            scope: Some("project".to_string()),
            project: None,
            project_explicit: false,
            path: Some(path.to_string()),
            category: None,
            domain: None,
            retention_policy: None,
            valid_from: None,
            valid_until: None,
            force,
        }
    }

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
        let parent = "/facts/lme/qid/abc";
        let child = chunk_child_path(parent, 3);
        assert_eq!(child, "/facts/lme/qid/abc/c003");
        assert!(child.contains("abc"));
    }

    /// tachi#2120: the old cap test used tiny turns that merge into ~12
    /// chunks and never exercised the 64-cap. Force >64 post-merge parts.
    #[test]
    fn max_chunks_cap_collapses_tail_at_true_64() {
        // Oversized turns (> target) so merge cannot collapse them.
        let filler = "x".repeat(AUTO_CHUNK_SIZE_CHARS + 50);
        let mut body = String::from("hdr\n");
        for i in 0..80 {
            body.push_str(&format!("user: turn-{i} {filler}\n"));
            body.push_str(&format!("assistant: reply-{i} NEEDLE_{i}\n"));
        }
        let pre_cap = {
            let turn_chunks = split_on_turn_markers(body.trim());
            merge_small_chunks(turn_chunks, AUTO_CHUNK_SIZE_CHARS).len()
        };
        assert!(
            pre_cap > AUTO_CHUNK_MAX_CHUNKS,
            "fixture must exceed cap before collapse, got {pre_cap}"
        );
        let chunks = split_remember_text(&body);
        assert_eq!(
            chunks.len(),
            AUTO_CHUNK_MAX_CHUNKS,
            "cap must land exactly at {AUTO_CHUNK_MAX_CHUNKS}, got {}",
            chunks.len()
        );
        assert!(
            chunks.iter().any(|c| c.contains("NEEDLE_79")),
            "final needle must survive tail collapse"
        );
        // Tail chunk is the oversized collapse of remaining turns.
        let tail = chunks.last().expect("tail");
        assert!(
            tail.chars().count() > AUTO_CHUNK_SIZE_CHARS,
            "collapsed tail should be oversized, got {}",
            tail.chars().count()
        );
    }

    #[test]
    fn sanitize_before_split_removes_cross_boundary_think() {
        // Place open/close think tags so a naive 1200-char window would split
        // between them. SECRET_INNER must not survive sanitize+split.
        let prefix = "safe-prefix-".repeat(80); // ~960 chars
        let think_open = "<think>";
        let secret_inner = "LEAKED_REASONING_SHOULD_NOT_PERSIST_ABCDEF";
        let mid = "y".repeat(400);
        let think_close = "</think>";
        let suffix = "safe-suffix-VISIBLE_NEEDLE and more factual content ".repeat(40);
        let raw = format!("{prefix}{think_open}{secret_inner}{mid}{think_close}{suffix}");
        assert!(raw.chars().count() > AUTO_CHUNK_THRESHOLD_CHARS);

        // Demonstrate the pre-fix hazard: unsanitized split can strand tags.
        let unsanitized_chunks = split_remember_text(&raw);
        let unsanitized_leak = unsanitized_chunks.iter().any(|c| {
            c.contains(secret_inner)
                && !(c.contains("<think>") && c.contains("</think>"))
        });
        assert!(
            unsanitized_leak || unsanitized_chunks.iter().any(|c| c.contains(secret_inner)),
            "fixture must put think payload at risk across unsanitized split"
        );

        let (safe, _) = sanitize_logical_input(&raw);
        assert!(
            !safe.contains(secret_inner),
            "sanitize must strip think interior"
        );
        assert!(!safe.contains("<think>"));
        let chunks = split_remember_text(&safe);
        assert!(chunks.len() >= 2);
        assert!(
            chunks.iter().all(|c| !c.contains(secret_inner)),
            "no chunk may retain think interior after sanitize-before-split"
        );
        assert!(
            chunks.iter().any(|c| c.contains("VISIBLE_NEEDLE")),
            "safe content must survive"
        );
    }

    #[test]
    fn sanitize_before_split_redacts_long_bearer_spanning_windows() {
        let prefix = "note ".repeat(250); // ~1250 chars → forces multi-chunk
        // Long synthetic bearer that a mid-token window cut would break.
        let token = format!(
            "Authorization: Bearer sk-{}",
            "abc123def456ghi789jkl012mno345pqr678stu901vwx234yz"
        );
        let suffix = " trailing factual payload UNIQUE_AFTER_TOKEN ".repeat(30);
        let raw = format!("{prefix}{token}{suffix}");
        assert!(raw.chars().count() > AUTO_CHUNK_THRESHOLD_CHARS);

        let (safe, redactions) = sanitize_logical_input(&raw);
        assert!(redactions > 0, "bearer must be detected on full input");
        assert!(
            !safe.contains("sk-abc123"),
            "token material must not survive sanitize"
        );
        let chunks = split_remember_text(&safe);
        assert!(chunks.len() >= 2);
        assert!(
            chunks.iter().all(|c| !c.contains("sk-abc123")),
            "no chunk may retain bearer material"
        );
        assert!(chunks.iter().any(|c| c.contains("UNIQUE_AFTER_TOKEN")));
    }

    #[test]
    fn child_outcome_saved_rejects_saved_false() {
        assert!(!child_outcome_saved(&json!({
            "saved": false,
            "noise": true,
            "reason": "Text detected as noise"
        })));
        assert!(!child_outcome_saved(&json!({
            "saved": false,
            "status": "duplicate",
            "id": "abc"
        })));
        assert!(child_outcome_saved(&json!({
            "status": "saved",
            "id": "abc"
        })));
        // status saved + explicit saved:false must not count (defensive).
        assert!(!child_outcome_saved(&json!({
            "status": "saved",
            "saved": false,
            "id": "abc"
        })));
    }

    #[tokio::test]
    async fn remember_all_rejected_noise_is_not_status_saved() {
        let server = crate::tests::make_server();
        // Denial pattern matches at any length — pad past auto-chunk threshold
        // with repeated denial turns so we exercise the multi-chunk aggregator.
        let mut body = String::new();
        for i in 0..12 {
            body.push_str(&format!(
                "user: please recall item {i}\nassistant: I don't remember anything about topic {i} at all, sorry.\n"
            ));
            body.push_str(&"padding ".repeat(40));
            body.push('\n');
        }
        assert!(body.chars().count() > AUTO_CHUNK_THRESHOLD_CHARS);

        let raw = handle_remember(
            &server,
            remember_params(body, false, "/scratch/tests/auto-chunk-reject-all"),
        )
        .await
        .expect("remember returns Ok envelope even when nothing persisted");
        let parsed: Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(parsed["status"], json!("rejected"));
        assert_eq!(parsed["saved"], json!(false));
        assert_eq!(parsed["chunk_count"], json!(0));
        assert!(parsed["chunk_attempted"].as_u64().unwrap() >= 2);
        assert!(parsed["id"].is_null() || parsed["id"] == json!(""));
        let chunks = parsed["chunks"].as_array().expect("chunks");
        assert!(chunks.iter().all(|c| c["saved"] == json!(false)));
    }

    #[tokio::test]
    async fn remember_partial_rejection_reports_honest_counts() {
        let server = crate::tests::make_server();
        let mut body = String::new();
        // Good factual turns
        for i in 0..6 {
            body.push_str(&format!(
                "user: fact question {i} with enough detail to stay concrete\n"
            ));
            body.push_str(&format!(
                "assistant: The concrete fact NEEDLE_GOOD_{i} is that project Orion ships on Tuesday with checklist item {i}.\n"
            ));
            body.push_str(&"detail ".repeat(50));
            body.push('\n');
        }
        // Denial turns that should reject without force
        for i in 0..6 {
            body.push_str(&format!(
                "user: noise question {i}\nassistant: I don't remember anything about noise topic {i}.\n"
            ));
            body.push_str(&"noisepad ".repeat(40));
            body.push('\n');
        }
        assert!(body.chars().count() > AUTO_CHUNK_THRESHOLD_CHARS);

        let raw = handle_remember(
            &server,
            remember_params(body, false, "/scratch/tests/auto-chunk-partial"),
        )
        .await
        .expect("ok envelope");
        let parsed: Value = serde_json::from_str(&raw).expect("json");
        let attempted = parsed["chunk_attempted"].as_u64().unwrap();
        let saved = parsed["chunk_count"].as_u64().unwrap();
        assert!(attempted >= 2, "expected multi-chunk, attempted={attempted}");
        assert!(
            saved > 0 && saved < attempted,
            "expected partial: saved={saved} attempted={attempted} body={parsed}"
        );
        assert_eq!(parsed["status"], json!("partial"));
        assert_eq!(parsed["saved"], json!(true));
        assert!(parsed["id"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
        assert_eq!(parsed["id_is_first_saved_chunk"], json!(true));
        assert_eq!(parsed["original_input_reconstructible"], json!(false));
        let ids = parsed["chunk_ids"].as_array().expect("chunk_ids");
        assert_eq!(ids.len() as u64, saved);
    }

    #[tokio::test]
    async fn remember_cross_boundary_sanitize_persists_no_think_or_bearer() {
        let server = crate::tests::make_server();
        let prefix = "session factual lead-in ".repeat(60);
        let think_secret = "PRIVATE_CHAIN_OF_THOUGHT_XYZ_SHOULD_NEVER_STORE";
        let bearer = "Authorization: Bearer sk-persistleak999888777666555444333222111000aaabbbccc";
        let suffix = " visible answer NEEDLE_SAFE lives here after the scrub ".repeat(50);
        let raw_text = format!(
            "{prefix}<think>{think_secret}</think>\n{bearer}\n{suffix}"
        );
        assert!(raw_text.chars().count() > AUTO_CHUNK_THRESHOLD_CHARS);

        let raw = handle_remember(
            &server,
            remember_params(raw_text, true, "/scratch/tests/auto-chunk-sanitize"),
        )
        .await
        .expect("remember ok");
        let parsed: Value = serde_json::from_str(&raw).expect("json");
        assert!(
            parsed["status"] == json!("saved") || parsed["status"] == json!("partial"),
            "expected persistence, got {parsed}"
        );
        assert!(
            parsed["secret_redactions"].as_u64().unwrap_or(0) >= 1,
            "redaction evidence required: {parsed}"
        );
        let chunks = parsed["chunks"].as_array().expect("chunks");
        assert!(chunks.len() >= 2, "expected multi-chunk sanitize path");

        // Fetch each saved child and assert no leak.
        for chunk in chunks {
            if chunk["saved"] != json!(true) {
                continue;
            }
            let id = chunk["id"].as_str().expect("saved id");
            let got = server
                .get_memory(rmcp::handler::server::wrapper::Parameters(
                    crate::tool_params::GetMemoryParams {
                        id: id.to_string(),
                        include_archived: false,
                        project: None,
                    },
                ))
                .await
                .expect("get_memory");
            let got_json: Value = serde_json::from_str(&got).expect("get json");
            let text = got_json["text"].as_str().unwrap_or("");
            assert!(
                !text.contains(think_secret),
                "think interior leaked in {id}: {text}"
            );
            assert!(
                !text.contains("sk-persistleak"),
                "bearer leaked in {id}: {text}"
            );
            assert!(
                !text.contains("<think>"),
                "think tag leaked in {id}: {text}"
            );
        }
    }
}
