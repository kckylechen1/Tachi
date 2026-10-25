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
    matches!(parsed.get("status").and_then(Value::as_str), Some("saved"))
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
    // tachi#2125: normalize temporal cues once from the full remember text so
    // auto-chunk children inherit the same validity window (pass-through).
    // Shared helper — not buried only here — so save_memory gets the same path.
    {
        let ingest = memcore::temporal_normalize::resolve_ingest_validity(
            params.valid_from.as_deref(),
            params.valid_until.as_deref(),
            &params.text,
            Utc::now(),
        );
        if ingest.source == memcore::temporal_normalize::IngestValiditySource::Cue {
            params.valid_from = ingest.valid_from.or(params.valid_from.take());
            params.valid_until = ingest.valid_until.or(params.valid_until.take());
        }
    }

    // Default path = /notes/{YYYY-MM-DD} so quick captures land in a
    // predictable, browsable bucket without forcing the caller to choose one.
    let inferred_path = params.path.clone().unwrap_or_else(|| {
        let date = Utc::now().format("%Y-%m-%d");
        format!("/notes/{date}")
    });

    let (safe_text, mut pre_secret_redactions) = sanitize_logical_input(&params.text);
    params.text = safe_text;
    if !params.summary.is_empty() {
        let (safe_summary, redactions) = sanitize_logical_input(&params.summary);
        params.summary = safe_summary;
        pre_secret_redactions += redactions;
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
    let mut receipts = Vec::with_capacity(chunk_total);
    let mut ids = Vec::new();
    let mut saved_count = 0;
    let mut existing_count = 0;
    let mut failed = false;
    let mut redactions = pre_secret_redactions;

    for (index, chunk) in chunks.into_iter().enumerate() {
        let path = chunk_child_path(&inferred_path, index);
        let meta = json!({
            "shortcut": "remember", "auto_chunked": true,
            "parent_path": inferred_path, "chunk_index": index, "chunk_total": chunk_total,
        });
        let save_params = build_save_params(&params, path.clone(), chunk, Some(meta));
        let result = handle_save_memory(server, save_params)
            .await
            .and_then(|raw| {
                serde_json::from_str::<Value>(&raw)
                    .map_err(|e| format!("invalid child save receipt: {e}"))
            });
        let mut child = match result {
            Ok(Value::Object(object)) => Value::Object(object),
            other => json!({
                "status": "error", "saved": false, "persistence": "unknown",
                "error": other.err().unwrap_or_else(|| "child save receipt is not an object".into()),
            }),
        };
        let id = child
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let saved = child_outcome_saved(&child) && id.is_some();
        let existing = child["status"] == "duplicate" && id.is_some();
        let confirmed = saved || existing;
        let mut status = child_status_label(&child);
        if status == "saved" && !saved {
            status = "error".into();
            child["error"] = json!("saved receipt has no confirmed row id");
            child["persistence"] = json!("unknown");
        }
        failed = status == "error";
        if confirmed {
            ids.push(id.expect("confirmed child has id"));
            saved_count += usize::from(saved);
            existing_count += usize::from(existing);
            // Read locators come from the child, after write-affinity routing.
            // Preserve a fallback warning when its resolved DB differs in scope.
            if let Some(db) = child.get("db").and_then(Value::as_str).map(str::to_owned) {
                let requested = params.scope.as_deref().unwrap_or("project");
                if requested != db && child.get("scope_warning").is_none() {
                    child["scope"] = json!(requested);
                    child["scope_warning"] =
                        json!(crate::memory_search_ops::scope_downgrade_warning(
                            requested,
                            &db,
                            server.session_project().is_some(),
                        ));
                }
            }
        }
        redactions += child
            .get("secret_redactions")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        // Keep the child's original diagnostics, visibility and read locator.
        child["path"] = child.get("path").cloned().unwrap_or(json!(path));
        child["chunk_index"] = json!(index);
        child["chunk_total"] = json!(chunk_total);
        child["saved"] = json!(saved);
        child["persisted"] = json!(confirmed);
        child["status"] = json!(status);
        receipts.push(child);
        if failed {
            break;
        }
    }

    let attempted = receipts.len();
    let complete = ids.len() == chunk_total;
    let status = if complete {
        if saved_count == 0 {
            "duplicate"
        } else {
            "saved"
        }
    } else if !ids.is_empty() {
        "partial"
    } else if failed {
        "error"
    } else {
        "rejected"
    };
    let mut response = json!({
        "id": ids.first(), "path": Value::Null, "parent_path": inferred_path,
        "status": status, "saved": saved_count > 0, "complete": complete,
        "auto_chunked": true, "chunk_count": ids.len(), "chunk_saved": saved_count,
        "chunk_existing": existing_count, "chunk_total": chunk_total,
        "chunk_attempted": attempted, "chunk_unattempted": chunk_total - attempted,
        "chunk_ids": ids, "id_is_first_saved_chunk": !ids.is_empty(),
        "original_input_reconstructible": false,
    });
    if let Some(first) = receipts.iter().find(|child| child["persisted"] == true) {
        for key in [
            "path",
            "db",
            "read_target",
            "confirm",
            "enrichment",
            "visibility",
            "recall_fence",
        ] {
            if let Some(value) = first.get(key) {
                response[key] = value.clone();
            }
        }
    }
    for key in ["scope", "scope_warning", "warning"] {
        if let Some(value) = receipts.iter().find_map(|child| child.get(key)) {
            response[key] = value.clone();
        }
    }
    for index in attempted..chunk_total {
        receipts.push(json!({
            "path": chunk_child_path(&inferred_path, index), "chunk_index": index,
            "chunk_total": chunk_total, "status": "not_attempted", "saved": false,
            "persisted": false,
        }));
    }
    response["chunks"] = json!(receipts);
    if redactions > 0 {
        response["secret_redactions"] = json!(redactions);
        response["secret_redaction_warning"] =
            json!("Potential secrets were redacted before persistence.");
    }
    if !complete {
        response["hint"] = json!("Inspect chunks for confirmed rows, rejection diagnostics and unattempted work. A failed write may have an unknown commit outcome; retrying identical input reuses existing rows.");
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

    fn boundary_input() -> String {
        format!(
            "Visible introduction. <think>{}</think>\nAuthorization: Bearer {}\n{}",
            "PRIVATE_REASONING_MARKER ".repeat(140),
            "X".repeat(1_500),
            "SAFE_ANSWER_NEEDLE has a documented launch date on Tuesday. ".repeat(70)
        )
    }

    fn distinct_turns(count: usize) -> String {
        (0..count)
            .map(|i| format!("user: answer {i} {}\n", format!("fact-{i} ").repeat(170)))
            .collect()
    }

    async fn public_note(server: &MemoryServer, text: String, path: &str) -> Value {
        let params = serde_json::from_value(json!({
            "action": "save", "kind": "note", "text": text,
            "path": path, "scope": "project", "force": true,
        }))
        .expect("public memory params");
        let raw = crate::facade_memory_ops::handle_tachi_memory(server, params)
            .await
            .expect("public save");
        serde_json::from_str(&raw).expect("default compact JSON")
    }

    async fn read_child(server: &MemoryServer, id: &str) -> Value {
        let raw = server
            .get_memory(rmcp::handler::server::wrapper::Parameters(
                crate::tool_params::GetMemoryParams {
                    id: id.to_string(),
                    include_archived: false,
                    project: None,
                },
            ))
            .await
            .expect("read persisted child");
        let row: Value = serde_json::from_str(&raw).expect("get JSON");
        assert_eq!(row["id"], id, "receipt must locate an actual row: {row}");
        assert!(!row["text"].as_str().expect("stored body").is_empty());
        row
    }

    fn stored_rows(server: &MemoryServer, parent: &str) -> u64 {
        server
            .with_global_store_read(|store| {
                store
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM memories WHERE path = ?1 OR path LIKE ?2",
                        rusqlite::params![parent, format!("{parent}/%")],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())
            })
            .expect("count actual fixture rows")
            .try_into()
            .expect("row count is nonnegative")
    }

    #[tokio::test]
    async fn public_note_keeps_chunk_receipts_and_reads_sanitized_rows() {
        let server = crate::tests::make_server();
        let input = boundary_input();
        let leaked_before_fix: String = split_remember_text(&input)
            .iter()
            .map(|s| sanitize_logical_input(s).0)
            .collect();
        assert!(leaked_before_fix.contains("PRIVATE_REASONING_MARKER"));
        assert!(leaked_before_fix.contains(&"X".repeat(100)));

        let receipt = public_note(&server, input, "receipt-boundaries.md").await;
        assert_eq!(receipt["auto_chunked"], true);
        assert_eq!(receipt["complete"], true);
        assert_eq!(receipt["original_input_reconstructible"], false);
        assert_eq!(receipt["id_is_first_saved_chunk"], true);
        assert!(
            receipt["secret_redactions"]
                .as_u64()
                .expect("redaction count")
                > 0
        );
        assert!(receipt["secret_redaction_warning"].is_string());
        assert!(receipt["scope_warning"].is_string());
        let children = receipt["chunks"]
            .as_array()
            .expect("compact child receipts");
        assert!(children.len() > 1);
        assert_eq!(
            receipt["chunk_ids"].as_array().unwrap().len(),
            children.len()
        );
        let mut answer_seen = false;
        for (index, child) in children.iter().enumerate() {
            assert_eq!(child["read_target"]["scope"], "global");
            assert_eq!(child["confirm"]["id"], child["id"]);
            assert!(child["scope_warning"].is_string());
            assert!(!child["capture_gate_warnings"]
                .as_array()
                .expect("capture warnings")
                .is_empty());
            let row = read_child(&server, child["id"].as_str().unwrap()).await;
            let text = row["text"].as_str().unwrap();
            assert!(!text.contains("PRIVATE_REASONING_MARKER"));
            assert!(!text.contains(&"X".repeat(100)));
            answer_seen |= text.contains("SAFE_ANSWER_NEEDLE");
            let stored = server
                .with_global_store_read(|store| {
                    store
                        .get(child["id"].as_str().unwrap())
                        .map_err(|e| e.to_string())
                })
                .unwrap()
                .expect("stored child");
            assert_eq!(stored.metadata["parent_path"], receipt["parent_path"]);
            assert_eq!(stored.metadata["chunk_index"], index);
            assert_eq!(stored.metadata["chunk_total"], children.len());
        }
        assert!(answer_seen, "safe answer must remain retrievable");
        assert_eq!(
            stored_rows(&server, receipt["parent_path"].as_str().unwrap()),
            children.len() as u64
        );

        let params = serde_json::from_value(json!({
            "action":"save", "kind":"note", "text":distinct_turns(2),
            "path":"text-receipt.md", "force":true, "format":"markdown",
        }))
        .unwrap();
        let text_receipt = crate::facade_memory_ops::handle_tachi_memory(&server, params)
            .await
            .unwrap();
        let (_, payload) = text_receipt
            .split_once('\n')
            .expect("text receipt keeps structured locators");
        let payload: Value = serde_json::from_str(payload).expect("text receipt payload");
        assert_eq!(payload["complete"], true);
        for id in payload["chunk_ids"].as_array().unwrap() {
            read_child(&server, id.as_str().unwrap()).await;
        }
    }

    #[tokio::test]
    async fn repeated_and_mixed_remember_retain_existing_child_locations() {
        let server = crate::tests::make_server();
        let path = "/scratch/remember-replay";
        let body = distinct_turns(2);
        let first: Value = serde_json::from_str(
            &handle_remember(&server, remember_params(body.clone(), true, path))
                .await
                .unwrap(),
        )
        .unwrap();
        let replay: Value = serde_json::from_str(
            &handle_remember(&server, remember_params(body.clone(), true, path))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(replay["status"], "duplicate");
        assert_eq!(replay["complete"], true);
        assert_eq!(replay["chunk_saved"], 0);
        assert_eq!(replay["chunk_existing"], 2);
        assert_eq!(replay["chunk_ids"], first["chunk_ids"]);
        assert_eq!(stored_rows(&server, path), 2, "retry must not add rows");
        for child in replay["chunks"].as_array().unwrap() {
            read_child(&server, child["id"].as_str().unwrap()).await;
            assert!(child["scope_warning"].is_string());
            assert_eq!(child["read_target"]["scope"], "global");
        }
        let mixed: Value = serde_json::from_str(
            &handle_remember(&server, remember_params(distinct_turns(3), true, path))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(mixed["status"], "saved");
        assert_eq!(mixed["complete"], true);
        assert_eq!(mixed["chunk_saved"], 1);
        assert_eq!(mixed["chunk_existing"], 2);
        assert_eq!(mixed["chunk_count"], 3);
        assert_eq!(stored_rows(&server, path), 3);
    }

    #[tokio::test]
    async fn public_note_rejection_after_scrub_never_defaults_to_saved() {
        let server = crate::tests::make_server();
        for text in [
            format!("<think>{}</think>", "private ".repeat(400)),
            format!(
                "user: I don't recall {}\nassistant: I don't recall {}",
                "padding ".repeat(180),
                "padding ".repeat(180)
            ),
        ] {
            let params = serde_json::from_value(json!({
                "action":"save", "kind":"note", "text":text,
                "path":"rejected-note.md", "force":false,
            }))
            .unwrap();
            let raw = crate::facade_memory_ops::handle_tachi_memory(&server, params)
                .await
                .unwrap();
            let value: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(value["status"], "rejected");
            assert_eq!(value["saved"], false);
            assert_eq!(value["ok"], false);
            assert!(value.get("id").is_none_or(Value::is_null));
            let parent = format!("/notes/{}", value["note_path"].as_str().unwrap());
            assert_eq!(stored_rows(&server, &parent), 0);
        }
    }

    #[test]
    fn remember_preserves_capture_rejection_diagnostics() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _gate = crate::test_support::EnvRestore::set("TACHI_CAPTURE_GATE", "enforce");
        let server = crate::tests::make_server();
        let mut params =
            remember_params(distinct_turns(2), false, "/disallowed-remember-test/gate");
        params.domain = Some("not-a-valid-domain".into());
        let raw = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(handle_remember(&server, params))
            .unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["status"], "rejected");
        assert_eq!(stored_rows(&server, "/disallowed-remember-test/gate"), 0);
        for child in value["chunks"].as_array().unwrap() {
            assert_eq!(child["rejected_by"], "capture_gate");
            assert_eq!(child["mode"], "enforce");
            assert!(!child["violations"].as_array().unwrap().is_empty());
            assert!(child["hint"].is_string());
        }
    }

    #[tokio::test]
    async fn remember_db_error_keeps_committed_prefix_and_stops() {
        let server = crate::tests::make_server();
        let old_limit = server
            .with_global_store(|store| {
                store
                    .connection()
                    .set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 8_000)
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        let body = format!(
            "user: first {}\nassistant: oversized {}\nuser: last {}",
            "FIRST_VISIBLE ".repeat(80),
            "second ".repeat(4_000),
            "UNATTEMPTED ".repeat(100)
        );
        let raw = handle_remember(
            &server,
            remember_params(body, true, "/scratch/remember-write-error"),
        )
        .await;
        server
            .with_global_store(|store| {
                store
                    .connection()
                    .set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, old_limit)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        let value: Value =
            serde_json::from_str(&raw.expect("partial receipt, not a lost prefix")).unwrap();
        assert_eq!(value["status"], "partial");
        assert_eq!(value["complete"], false);
        assert_eq!(value["chunk_count"], 1);
        assert_eq!(value["chunk_attempted"], 2);
        assert_eq!(value["chunk_unattempted"], 1);
        assert_eq!(value["chunks"][1]["status"], "error");
        assert_eq!(value["chunks"][1]["persistence"], "unknown");
        assert!(value["chunks"][1]["error"]
            .as_str()
            .unwrap()
            .contains("too big"));
        let row = read_child(&server, value["chunk_ids"][0].as_str().unwrap()).await;
        assert!(row["text"].as_str().unwrap().contains("FIRST_VISIBLE"));
        assert_eq!(stored_rows(&server, "/scratch/remember-write-error"), 1);
    }

    #[tokio::test]
    async fn remember_summary_redaction_is_reported_on_single_child() {
        let server = crate::tests::make_server();
        let mut params = remember_params(
            "A concrete public fact about the Tuesday launch.".into(),
            true,
            "/scratch/summary-scrub",
        );
        params.summary = format!("Authorization: Bearer {}", "X".repeat(100));
        let value: Value =
            serde_json::from_str(&handle_remember(&server, params).await.unwrap()).unwrap();
        assert!(value["secret_redactions"].as_u64().unwrap_or(0) > 0);
        let row = read_child(&server, value["id"].as_str().unwrap()).await;
        assert!(!row["summary"]
            .as_str()
            .expect("stored summary")
            .contains(&"X".repeat(30)));
    }

    #[tokio::test]
    async fn receipt_duplicate_uses_actual_affinity_project() {
        let (server, _) = crate::tests::make_server_with_project_fixture("bound-a");
        let routed_db = server.tachi_home_dir().join("projects/routed-b/memory.db");
        std::fs::create_dir_all(routed_db.parent().unwrap()).unwrap();
        drop(
            memcore::MemoryStore::open_with_label(routed_db.to_str().unwrap(), "routed-b").unwrap(),
        );
        std::fs::write(
            server.tachi_home_dir().join("routing.json"),
            r#"{"domain_routes":[{"project":"routed-b","domains":["agent_os"]}]}"#,
        )
        .unwrap();
        let mut first_ids = Value::Null;
        for attempt in 0..2 {
            let mut params: crate::tool_params::TachiMemoryParams = serde_json::from_value(json!({
                "action":"save", "kind":"note", "text":distinct_turns(2),
                "path":"affinity-replay.md", "scope":"project", "project":"bound-a",
                "domain":"agent_os", "force":true,
            }))
            .unwrap();
            // Match a transport-injected session default, which remains routable.
            params.project_explicit = false;
            let raw = crate::facade_memory_ops::handle_tachi_memory(&server, params)
                .await
                .unwrap();
            let receipt: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(receipt["complete"], true);
            assert_eq!(
                receipt["status"],
                if attempt == 0 { "saved" } else { "duplicate" }
            );
            assert_eq!(receipt["chunk_count"], 2);
            if attempt == 0 {
                first_ids = receipt["chunk_ids"].clone();
            } else {
                assert_eq!(receipt["chunk_saved"], 0);
                assert_eq!(receipt["chunk_ids"], first_ids);
            }
            for child in receipt["chunks"].as_array().unwrap() {
                assert_eq!(child["read_target"]["project"], "routed-b");
                assert_eq!(child["confirm"]["project"], "routed-b");
                let read = crate::memory_ops::handle_get_memory(
                    &server,
                    crate::tool_params::GetMemoryParams {
                        id: child["confirm"]["id"].as_str().unwrap().into(),
                        project: child["confirm"]["project"].as_str().map(str::to_owned),
                        include_archived: false,
                    },
                )
                .await
                .unwrap();
                let row: Value = serde_json::from_str(&read).unwrap();
                assert_eq!(row["id"], child["id"]);
                assert!(!row["text"].as_str().expect("stored routed body").is_empty());
            }
            for (project, expected) in [("bound-a", 0_i64), ("routed-b", 2_i64)] {
                let count = server.with_named_project_store_read(project, |store| {
                    store.connection().query_row(
                        "SELECT COUNT(*) FROM memories WHERE path LIKE '/notes/affinity-replay.md/%'",
                        [], |row| row.get::<_, i64>(0),
                    ).map_err(|e| e.to_string())
                }).unwrap();
                assert_eq!(
                    count, expected,
                    "actual rows in {project} after attempt {attempt}"
                );
            }
        }
    }

    #[tokio::test]
    async fn receipt_single_child_markdown_keeps_sanitization_diagnostics() {
        let server = crate::tests::make_server();
        let safe = "SAFE_SINGLE_FACT: the launch is scheduled for Tuesday at noon.";
        for (index, (text, summary)) in [
            (
                format!(
                    "<think>{}</think>\nAuthorization: Bearer {}\n{safe}",
                    "PRIVATE_REASONING_MARKER ".repeat(140),
                    "X".repeat(1500)
                ),
                String::new(),
            ),
            (
                safe.to_string(),
                format!("Authorization: Bearer {}", "X".repeat(100)),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let path = format!("single-diagnostics-{index}.md");
            let params = serde_json::from_value(json!({
                "action":"save", "kind":"note", "text":text, "summary":summary,
                "path":path, "force":true, "format":"markdown",
            }))
            .unwrap();
            let receipt = crate::facade_memory_ops::handle_tachi_memory(&server, params)
                .await
                .unwrap();
            assert!(receipt.starts_with("Saved ->"), "{receipt}");
            assert!(receipt.contains("Secrets redacted: 1"), "{receipt}");
            assert!(
                receipt.contains("Potential secrets were redacted before persistence."),
                "{receipt}"
            );
            assert!(receipt.contains("Capture gate warnings:"), "{receipt}");
            let parent = format!("/notes/{path}");
            assert_eq!(stored_rows(&server, &parent), 1);
            let rows = server
                .with_global_store_read(|store| {
                    store
                        .list_by_path(&parent, 5, false)
                        .map_err(|e| e.to_string())
                })
                .unwrap();
            assert_eq!(rows.len(), 1);
            assert!(rows[0].text.contains("SAFE_SINGLE_FACT"));
            assert!(!rows[0].text.contains("PRIVATE_REASONING_MARKER"));
            assert!(!rows[0].text.contains(&"X".repeat(100)));
            assert!(!rows[0].summary.contains(&"X".repeat(30)));
            assert!(!rows[0]
                .metadata
                .get("auto_chunked")
                .and_then(Value::as_bool)
                .unwrap_or(false));
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
            c.contains(secret_inner) && !(c.contains("<think>") && c.contains("</think>"))
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
        // ~1250 chars → forces multi-chunk. Long synthetic bearer that a mid-token
        // window cut would break across chunks without sanitize-first.
        let prefix = "note ".repeat(250);
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
        assert!(
            attempted >= 2,
            "expected multi-chunk, attempted={attempted}"
        );
        assert!(
            saved > 0 && saved < attempted,
            "expected partial: saved={saved} attempted={attempted} body={parsed}"
        );
        assert_eq!(parsed["status"], json!("partial"));
        assert_eq!(parsed["saved"], json!(true));
        assert!(parsed["id"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false));
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
        let raw_text = format!("{prefix}<think>{think_secret}</think>\n{bearer}\n{suffix}");
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
                        as_of: None,
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
