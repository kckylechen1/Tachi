//! SFT Factory — Automated Supervised Fine-Tuning dataset generator.
//!
//! Converts distilled (`consolidated` / `pattern` tier) memories into
//! high-quality SFT JSONL dialogue pairs using an LLM rewrite step.
//!
//! Output layout:
//!   ~/.tachi/foundry-runs/sft/
//!     sft_v3.jsonl           — OpenAI chat format (all types merged)
//!     sft_data_chat.jsonl    — same, incremental appends
//!     sft_data_hf.jsonl      — HuggingFace conversations format
//!
//! Called once per daily pipeline run after truth maintenance completes.

use std::path::PathBuf;

use chrono::Utc;
use serde_json::{json, Value};

use crate::foundry_runtime_ops::scrub_agent_noise;
use crate::MemoryServer;
use memory_core::types::MemoryEntry;

// ─── Constants ───────────────────────────────────────────────────────────────

/// System prompt injected into every SFT dialogue pair.
const SFT_SYSTEM_PROMPT: &str = "You are Tachi, an expert software engineer working on Sigil \
(Tachi), a Rust-based agent memory system. You write production Rust/Python/TypeScript, debug \
MCP servers, design agent architectures, and optimize memory systems.";

/// LLM prompt that generates the SFT dialogue pair from a raw distilled memory.
const SFT_GENERATION_SYSTEM: &str = r#"You are converting distilled programming session knowledge into high-quality SFT (Supervised Fine-Tuning) training data.

## Your Task
Read the distilled memory below. Convert it into a natural programming dialogue suitable for fine-tuning a coding agent.

## Output Format
Return ONLY a single valid JSON object (no markdown, no wrapping):
{
  "user": "<natural engineering question that would lead to this answer, ≤120 chars>",
  "assistant": "<concise, natural engineering response, 300-1200 chars>",
  "type": "debug|architecture|process|knowledge"
}

## Rules
1. Rewrite the user message as a natural engineering question — do NOT copy the memory title.
2. Synthesize the assistant response from the memory text. Sound like a real engineer explaining to a colleague.
3. Remove all noise: tool traces, file paths as standalone lines, JSON blobs, thinking markers.
4. Keep concrete details: file paths, error messages, code patterns if they add value.
5. type: "debug"=bug/error, "architecture"=design/structure, "process"=workflow/strategy, "knowledge"=facts/insights.
6. Output ONLY the JSON object — no prose before or after.
"#;

/// Maximum consolidated memories to process per daily run (budget control).
const SFT_MAX_PER_RUN: usize = 120;

/// Minimum importance threshold for SFT-worthiness.
const SFT_MIN_IMPORTANCE: f64 = 0.65;

// ─── Public entry point ───────────────────────────────────────────────────────

/// Run the daily SFT distillation pass.
/// Scans for consolidated/pattern memories that have not yet been SFT-processed,
/// generates LLM dialogue pairs, and appends them to the JSONL export files.
pub(crate) async fn run_daily_sft_distillation(
    server: &MemoryServer,
) -> Result<(), String> {
    let candidates = collect_sft_candidates(server)?;
    if candidates.is_empty() {
        return Ok(());
    }

    let out_dir = tachi_app_home().join("foundry-runs").join("sft");
    tokio::fs::create_dir_all(&out_dir)
        .await
        .map_err(|e| format!("create SFT output dir: {e}"))?;

    let v3_path = out_dir.join("sft_v3.jsonl");
    let chat_path = out_dir.join("sft_data_chat.jsonl");
    let hf_path = out_dir.join("sft_data_hf.jsonl");

    let mut v3_lines = Vec::new();
    let mut chat_lines = Vec::new();
    let mut hf_lines = Vec::new();
    let mut processed = 0usize;
    let mut processed_entries = Vec::new();

    for entry in candidates.iter().take(SFT_MAX_PER_RUN) {
        let text = scrub_agent_noise(&entry.text);
        if text.trim().len() < 80 {
            continue; // too short after noise removal
        }
        match generate_sft_pair(server, &text, &entry.summary).await {
            Ok((user_msg, assistant_msg, pair_type)) => {
                let chat_obj = build_chat_jsonl(
                    &user_msg, &assistant_msg, &pair_type, SFT_SYSTEM_PROMPT,
                );
                let hf_obj = build_hf_jsonl(
                    &user_msg, &assistant_msg, &pair_type, SFT_SYSTEM_PROMPT,
                );
                v3_lines.push(serde_json::to_string(&chat_obj).unwrap_or_default());
                chat_lines.push(serde_json::to_string(&chat_obj).unwrap_or_default());
                hf_lines.push(serde_json::to_string(&hf_obj).unwrap_or_default());
                processed += 1;
                processed_entries.push(entry);
            }
            Err(e) => {
                eprintln!("[sft_factory] skipped {}: {e}", entry.id);
            }
        }
    }

    if processed == 0 {
        return Ok(());
    }

    // Append to output files
    append_jsonl_lines(&v3_path, &v3_lines).await?;
    append_jsonl_lines(&chat_path, &chat_lines).await?;
    append_jsonl_lines(&hf_path, &hf_lines).await?;

    // Mark processed entries to avoid re-processing
    mark_sft_processed(server, processed_entries.into_iter())?;

    eprintln!("[sft_factory] exported {processed} SFT pairs → {}", out_dir.display());
    Ok(())
}

// ─── Candidate collection ─────────────────────────────────────────────────────

fn collect_sft_candidates(server: &MemoryServer) -> Result<Vec<MemoryEntry>, String> {
    server.with_project_store_read(|store| {
        let conn = store.connection();
        let mut stmt = conn
            .prepare(
                "SELECT id,path,summary,text,importance,timestamp,valid_from,valid_until,
                        category,topic,keywords,persons,entities,location,source,scope,archived,
                        access_count,last_access,revision,metadata,retention_policy,domain,
                        recall_count,query_diversity,tier
                 FROM memories
                 WHERE archived = 0
                   AND tier IN ('consolidated','pattern')
                   AND importance >= ?1
                   AND (json_extract(metadata, '$.sft.processed') IS NULL
                        OR json_extract(metadata, '$.sft.processed') = 0)
                 ORDER BY importance DESC, access_count DESC
                 LIMIT ?2",
            )
            .map_err(|e| format!("prepare SFT candidate query: {e}"))?;
        let rows = stmt
            .query_map(
                rusqlite::params![SFT_MIN_IMPORTANCE, SFT_MAX_PER_RUN * 2],
                memory_core::row_to_entry,
            )
            .map_err(|e| format!("query SFT candidates: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    })
    .map_err(|e| format!("SFT candidate collection: {e}"))
}

// ─── LLM dialogue generation ─────────────────────────────────────────────────

async fn generate_sft_pair(
    server: &MemoryServer,
    text: &str,
    summary: &str,
) -> Result<(String, String, String), String> {
    let user_payload = format!(
        "Summary: {summary}\n\nDistilled memory:\n{text}"
    );
    let raw = server
        .llm
        .call_distill_llm(SFT_GENERATION_SYSTEM, &user_payload, None, 0.4, 800)
        .await
        .map_err(|e| format!("SFT LLM call: {e}"))?;

    // Parse the returned JSON object
    let json_str = extract_json_object(&raw);
    let obj: Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("parse SFT response JSON: {e} — raw: {raw}"))?;

    let user_msg = obj
        .get("user")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing 'user' field")?
        .to_string();
    let assistant_msg = obj
        .get("assistant")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing 'assistant' field")?
        .to_string();
    let pair_type = obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("knowledge")
        .to_string();

    Ok((user_msg, assistant_msg, pair_type))
}

// ─── JSONL builders ───────────────────────────────────────────────────────────

fn build_chat_jsonl(
    user: &str,
    assistant: &str,
    pair_type: &str,
    system: &str,
) -> Value {
    json!({
        "messages": [
            {"role": "system", "content": system},
            {"role": "user",   "content": user},
            {"role": "assistant", "content": assistant}
        ],
        "metadata": {
            "type": pair_type,
            "source": "tachi_sft_factory",
            "generated_at": Utc::now().to_rfc3339()
        }
    })
}

fn build_hf_jsonl(
    user: &str,
    assistant: &str,
    pair_type: &str,
    system: &str,
) -> Value {
    json!({
        "conversations": [
            {"from": "system",    "value": system},
            {"from": "user",      "value": user},
            {"from": "assistant", "value": assistant}
        ],
        "metadata": {
            "type": pair_type,
            "source": "tachi_sft_factory",
            "generated_at": Utc::now().to_rfc3339()
        }
    })
}

// ─── Post-processing ──────────────────────────────────────────────────────────

fn mark_sft_processed<'a>(
    server: &MemoryServer,
    entries: impl Iterator<Item = &'a MemoryEntry>,
) -> Result<(), String> {
    let ids: Vec<String> = entries.map(|e| e.id.clone()).collect();
    if ids.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().to_rfc3339();
    server.with_project_store(|store| {
        let conn = store.connection();
        for id in &ids {
            conn.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.sft.processed', 1,
                         '$.sft.processed_at', ?1
                       )
                   WHERE id = ?2"#,
                rusqlite::params![now, id],
            ).map_err(|e| format!("update SFT marker for {id}: {e}"))?;
        }
        Ok(())
    })
    .map_err(|e| format!("mark SFT processed: {e}"))
}

async fn append_jsonl_lines(
    path: &PathBuf,
    lines: &[String],
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    for line in lines {
        if !line.is_empty() {
            file.write_all(line.as_bytes())
                .await
                .map_err(|e| format!("write JSONL line: {e}"))?;
            file.write_all(b"\n")
                .await
                .map_err(|e| format!("write JSONL newline: {e}"))?;
        }
    }
    Ok(())
}

// ─── Utilities ────────────────────────────────────────────────────────────────

fn extract_json_object(raw: &str) -> String {
    // Strip markdown fences if present
    let raw = raw.trim();
    let raw = raw
        .strip_prefix("```json")
        .or_else(|| raw.strip_prefix("```"))
        .map(|s| s.trim_end_matches("```").trim())
        .unwrap_or(raw);
    // Find first '{' and last '}' to extract the object
    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) {
        raw[start..=end].to_string()
    } else {
        raw.to_string()
    }
}

fn tachi_app_home() -> PathBuf {
    std::env::var("TACHI_APP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".tachi")
        })
}
