//! Phase 1 — Daily Batch Distill.
//!
//! Replaces the legacy per-capture `MemoryDistill` foundry job. Once per
//! day the bootstrap scheduler invokes [`run_daily_batch_distill`], which:
//!
//! 1. Scans the project DB for unprocessed source memories (those whose
//!    id does not appear in any existing distill memory's
//!    `metadata.source_memory_ids` array).
//! 2. Groups them by (path_prefix, coherence_key) using the same
//!    `coherent_distill_buckets` logic as the legacy scheduler.
//! 3. Builds one mega-prompt per batch (≤ [`resolve_batch_size`] groups)
//!    and dispatches it through the configured backend ([`DistillBackend`]).
//! 4. Parses the JSON array response, persists one distill `MemoryEntry`
//!    per group (with full provenance metadata), and writes a
//!    `source_manifest.json` audit file under
//!    `~/.tachi/foundry-runs/distill/<project>/<batch_run_id>/`.
//! 5. On error or unparseable response, splits the batch (API path) or
//!    falls back per-group to single-group [`LlmClient::call_distill_llm`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};

use super::helpers::dedup_strings;
use super::maintenance::{
    coherence_bucket_key, coherent_distill_buckets, scheduled_distill_path_prefix,
};
use super::*;
use crate::llm::LlmClient;

/// Default batch size when `FOUNDRY_DISTILL_BATCH_SIZE` is unset.
pub const DEFAULT_GROUPS_PER_BATCH: usize = 6;

/// Legacy alias kept for external references.
#[allow(dead_code)]
pub const MAX_GROUPS_PER_BATCH: usize = DEFAULT_GROUPS_PER_BATCH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistillBackend {
    ClaudeCli,
    RawApi,
}

impl DistillBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            DistillBackend::ClaudeCli => "claude_cli",
            DistillBackend::RawApi => "raw_api",
        }
    }
}

/// Resolve distill backend from `FOUNDRY_DISTILL_BACKEND`.
/// Defaults to `raw_api` so daemon runs do not require Claude Code CLI.
pub fn resolve_distill_backend() -> DistillBackend {
    match std::env::var("FOUNDRY_DISTILL_BACKEND")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("claude_cli" | "claude-cli" | "cli") => DistillBackend::ClaudeCli,
        Some("raw_api" | "raw-api" | "rawapi" | "api") => DistillBackend::RawApi,
        None | Some(_) => DistillBackend::RawApi,
    }
}

/// Batch size for one LLM call. Override with `FOUNDRY_DISTILL_BATCH_SIZE`.
pub fn resolve_batch_size() -> usize {
    std::env::var("FOUNDRY_DISTILL_BATCH_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| (1..=20).contains(&n))
        .unwrap_or(DEFAULT_GROUPS_PER_BATCH)
}

/// Minimum bucket size before we ask the LLM to distill — matches the
/// scheduler's threshold so we don't stitch tiny noisy groups.
const MIN_BUCKET_SIZE: usize = 3;

/// Max characters of the batch user payload before dispatching. If the
/// serialized JSON exceeds this, groups are dropped from the tail to stay
/// within the limit and avoid token overflow on the model side.
const MAX_BATCH_PAYLOAD_CHARS: usize = 60_000;

/// System prompt for the batch distill mega-call. Mirrors the design doc
/// (Phase 1 mega-prompt) — instructs the model to return a JSON array
/// with one object per input group.
const DISTILL_DAILY_SYSTEM_PROMPT: &str = r#"You are Tachi's batch memory distiller. You will receive a list of memory groups; each group contains 3+ related memories from a single coherence bucket (shared topic or entity, scoped to a single path prefix).

For EACH group, write a concise, faithful synthesis that:
- Preserves the most important durable facts (decisions, identifiers, file paths, commands, error signatures).
- Drops chit-chat, redundant restatements, and time-sensitive scratch notes.
- Uses neutral third-person prose. Do NOT invent facts not present in the inputs.
- Stays under ~400 words per group.

Return ONLY a JSON array. No prose before or after. Each element MUST be:
{
  "group_id": "<the group_id you were given>",
  "summary": "<one-line ≤120 chars>",
  "text": "<the full distilled synthesis>",
  "keywords": ["<lower-case tag>", ...]
}

If a group cannot be coherently distilled, return an object with an empty "text" and a "skip_reason" field; that group will be skipped.
"#;

/// Per-batch outcome surfaced to the scheduler/log.
#[derive(Debug, Default, Serialize)]
pub struct DistillBatchReport {
    pub projects_scanned: usize,
    pub batches_dispatched: usize,
    pub groups_distilled: usize,
    pub groups_skipped: usize,
    pub fallback_used: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
struct CandidateGroup {
    group_id: String,
    path_prefix: String,
    coherence_key: String,
    entries: Vec<MemoryEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct SourceManifestEntry {
    group_id: String,
    path_prefix: String,
    coherence_key: String,
    source_memory_ids: Vec<String>,
    written_memory_id: Option<String>,
    backend: &'static str,
    fallback_used: bool,
    skip_reason: Option<String>,
}

/// Entry point invoked by the bootstrap scheduler.
pub async fn run_daily_batch_distill(server: &MemoryServer) -> Result<DistillBatchReport, String> {
    let mut report = DistillBatchReport::default();

    if !server.has_project_db() {
        return Ok(report);
    }
    report.projects_scanned = 1;

    let candidates = collect_candidate_groups(server)?;
    if candidates.is_empty() {
        return Ok(report);
    }

    let project_label = derive_project_label(server);
    let batch_run_id = format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S"),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let runs_root = server
        .claude_pool
        .runs_dir()
        .join("distill")
        .join(&project_label)
        .join(&batch_run_id);
    let _ = std::fs::create_dir_all(&runs_root);

    let mut manifest: Vec<SourceManifestEntry> = Vec::new();
    let backend = resolve_distill_backend();
    let batch_size = resolve_batch_size();

    for (chunk_idx, chunk) in candidates.chunks(batch_size).enumerate() {
        match backend {
            DistillBackend::ClaudeCli => {
                process_claude_batch(
                    server,
                    chunk,
                    chunk_idx,
                    &project_label,
                    &batch_run_id,
                    &mut report,
                    &mut manifest,
                )
                .await;
            }
            DistillBackend::RawApi => {
                process_api_batch(
                    server,
                    chunk,
                    chunk_idx,
                    &mut report,
                    &mut manifest,
                    &batch_run_id,
                )
                .await;
            }
        }
    }

    // Best-effort: write the audit manifest.
    let manifest_path = runs_root.join("source_manifest.json");
    if let Ok(body) = serde_json::to_string_pretty(&json!({
        "batch_run_id": batch_run_id,
        "project": project_label,
        "backend": backend.as_str(),
        "batch_size": batch_size,
        "generated_at": Utc::now().to_rfc3339(),
        "report": &report,
        "groups": manifest,
    })) {
        let _ = std::fs::write(&manifest_path, body);
    }

    // Opportunistically prune stale foundry-runs subdirs.
    let _ = server.claude_pool.cleanup_expired();

    Ok(report)
}

async fn process_claude_batch(
    server: &MemoryServer,
    chunk: &[CandidateGroup],
    chunk_idx: usize,
    project_label: &str,
    batch_run_id: &str,
    report: &mut DistillBatchReport,
    manifest: &mut Vec<SourceManifestEntry>,
) {
    report.batches_dispatched += 1;
    let label = format!("distill-{}-b{}", project_label, chunk_idx);
    let prompt = build_batch_prompt(chunk);
    match server.claude_pool.call(&label, &prompt).await {
        Ok(outcome) => match parse_distill_response(&outcome.text) {
            Ok(per_group) => {
                apply_parsed_groups(
                    server,
                    chunk,
                    &per_group,
                    batch_run_id,
                    DistillBackend::ClaudeCli,
                    false,
                    report,
                    manifest,
                )
                .await;
            }
            Err(err) => {
                report
                    .errors
                    .push(format!("parse claude batch {chunk_idx}: {err}"));
                for group in chunk {
                    fallback_one_group(server, group, batch_run_id, report, manifest).await;
                }
            }
        },
        Err(err) => {
            report
                .errors
                .push(format!("claude batch {chunk_idx}: {err}"));
            for group in chunk {
                fallback_one_group(server, group, batch_run_id, report, manifest).await;
            }
        }
    }
}

async fn process_api_batch(
    server: &MemoryServer,
    chunk: &[CandidateGroup],
    chunk_idx: usize,
    report: &mut DistillBatchReport,
    manifest: &mut Vec<SourceManifestEntry>,
    batch_run_id: &str,
) {
    let mut pending: Vec<(&[CandidateGroup], usize)> = vec![(chunk, 0)];
    let max_depth = 8;
    while let Some((batch, depth)) = pending.pop() {
        if batch.is_empty() {
            continue;
        }
        report.batches_dispatched += 1;
        match call_api_batch_distill(&server.llm, batch).await {
            Ok(raw) => match parse_distill_response(&raw) {
                Ok(per_group) => {
                    apply_parsed_groups(
                        server,
                        batch,
                        &per_group,
                        batch_run_id,
                        DistillBackend::RawApi,
                        false,
                        report,
                        manifest,
                    )
                    .await;
                }
                Err(err) if batch.len() > 1 && depth < max_depth => {
                    report.errors.push(format!(
                        "parse api batch {chunk_idx} ({} groups): {err}; splitting",
                        batch.len()
                    ));
                    let mid = batch.len() / 2;
                    pending.push((&batch[mid..], depth + 1));
                    pending.push((&batch[..mid], depth + 1));
                }
                Err(err) => {
                    report
                        .errors
                        .push(format!("parse api batch {chunk_idx}: {err}"));
                    for group in batch {
                        fallback_one_group(server, group, batch_run_id, report, manifest).await;
                    }
                }
            },
            Err(err) if batch.len() > 1 && depth < max_depth => {
                report.errors.push(format!(
                    "api batch {chunk_idx} ({} groups): {err}; splitting",
                    batch.len()
                ));
                let mid = batch.len() / 2;
                pending.push((&batch[mid..], depth + 1));
                pending.push((&batch[..mid], depth + 1));
            }
            Err(err) => {
                report.errors.push(format!("api batch {chunk_idx}: {err}"));
                for group in batch {
                    fallback_one_group(server, group, batch_run_id, report, manifest).await;
                }
            }
        }
    }
}

async fn apply_parsed_groups(
    server: &MemoryServer,
    chunk: &[CandidateGroup],
    per_group: &HashMap<String, GroupPayload>,
    batch_run_id: &str,
    backend: DistillBackend,
    fallback_used: bool,
    report: &mut DistillBatchReport,
    manifest: &mut Vec<SourceManifestEntry>,
) {
    let backend_label = backend.as_str();
    for group in chunk {
        let item = per_group.get(&group.group_id);
        match item {
            Some(payload) if !payload.text.trim().is_empty() => {
                match persist_distill_memory(
                    server,
                    group,
                    payload,
                    batch_run_id,
                    backend_label,
                    fallback_used,
                ) {
                    Ok(id) => {
                        report.groups_distilled += 1;
                        if fallback_used {
                            report.fallback_used += 1;
                        }
                        manifest.push(SourceManifestEntry {
                            group_id: group.group_id.clone(),
                            path_prefix: group.path_prefix.clone(),
                            coherence_key: group.coherence_key.clone(),
                            source_memory_ids: group.entries.iter().map(|e| e.id.clone()).collect(),
                            written_memory_id: Some(id),
                            backend: backend_label,
                            fallback_used,
                            skip_reason: None,
                        });
                    }
                    Err(err) => {
                        report
                            .errors
                            .push(format!("persist {}: {err}", group.group_id));
                    }
                }
            }
            Some(payload) => {
                report.groups_skipped += 1;
                manifest.push(SourceManifestEntry {
                    group_id: group.group_id.clone(),
                    path_prefix: group.path_prefix.clone(),
                    coherence_key: group.coherence_key.clone(),
                    source_memory_ids: group.entries.iter().map(|e| e.id.clone()).collect(),
                    written_memory_id: None,
                    backend: backend_label,
                    fallback_used,
                    skip_reason: Some(
                        payload
                            .skip_reason
                            .clone()
                            .unwrap_or_else(|| "empty_text".to_string()),
                    ),
                });
            }
            None => {
                fallback_one_group(server, group, batch_run_id, report, manifest).await;
            }
        }
    }
}

async fn call_api_batch_distill(
    llm: &LlmClient,
    groups: &[CandidateGroup],
) -> Result<String, String> {
    let user = build_batch_user_payload(groups);
    let max_tokens = batch_max_tokens(groups.len());
    llm.call_distill_llm(DISTILL_DAILY_SYSTEM_PROMPT, &user, None, 0.3, max_tokens)
        .await
}

fn batch_max_tokens(group_count: usize) -> u32 {
    let estimated = group_count.saturating_mul(700).saturating_add(512);
    estimated.min(8192) as u32
}

async fn fallback_one_group(
    server: &MemoryServer,
    group: &CandidateGroup,
    batch_run_id: &str,
    report: &mut DistillBatchReport,
    manifest: &mut Vec<SourceManifestEntry>,
) {
    match fallback_distill(&server.llm, group).await {
        Ok(payload) => {
            match persist_distill_memory(server, group, &payload, batch_run_id, "raw_api", true) {
                Ok(id) => {
                    report.groups_distilled += 1;
                    report.fallback_used += 1;
                    manifest.push(SourceManifestEntry {
                        group_id: group.group_id.clone(),
                        path_prefix: group.path_prefix.clone(),
                        coherence_key: group.coherence_key.clone(),
                        source_memory_ids: group.entries.iter().map(|e| e.id.clone()).collect(),
                        written_memory_id: Some(id),
                        backend: "raw_api",
                        fallback_used: true,
                        skip_reason: None,
                    });
                }
                Err(err) => {
                    report
                        .errors
                        .push(format!("persist fallback {}: {err}", group.group_id));
                }
            }
        }
        Err(err) => {
            report.groups_skipped += 1;
            report
                .errors
                .push(format!("fallback {}: {err}", group.group_id));
            manifest.push(SourceManifestEntry {
                group_id: group.group_id.clone(),
                path_prefix: group.path_prefix.clone(),
                coherence_key: group.coherence_key.clone(),
                source_memory_ids: group.entries.iter().map(|e| e.id.clone()).collect(),
                written_memory_id: None,
                backend: "raw_api",
                fallback_used: true,
                skip_reason: Some(err),
            });
        }
    }
}

fn derive_project_label(server: &MemoryServer) -> String {
    server
        .project_db_path_buf()
        .and_then(|p: PathBuf| {
            p.parent()
                .and_then(|parent| parent.file_name())
                .and_then(|os| os.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "project".to_string())
}

fn collect_candidate_groups(server: &MemoryServer) -> Result<Vec<CandidateGroup>, String> {
    let (processed_ids, candidate_entries) = server.with_project_store_read(|store| {
        let conn = store.connection();
        let mut processed_ids: HashSet<String> = HashSet::new();
        let mut stmt = conn
            .prepare("SELECT metadata FROM memories WHERE archived = 0 AND source = ?1")
            .map_err(|e| format!("prepare distill metadata query: {e}"))?;
        let rows = stmt
            .query_map([FOUNDRY_DISTILL_SOURCE], |row| row.get::<_, String>(0))
            .map_err(|e| format!("query distill metadata rows: {e}"))?;
        for row in rows {
            let raw = row.map_err(|e| format!("read distill metadata row: {e}"))?;
            let metadata: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
            let ids = metadata
                .get("source_memory_ids")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
                .map(ToOwned::to_owned);
            processed_ids.extend(ids);
        }

        let mut stmt = conn
            .prepare(
                "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,persons,entities,location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
                  FROM memories
                  WHERE archived = 0 AND source != ?1
                  ORDER BY timestamp ASC",
            )
            .map_err(|e| format!("prepare candidate query: {e}"))?;
        let rows = stmt
            .query_map([FOUNDRY_DISTILL_SOURCE], memory_core::row_to_entry)
            .map_err(|e| format!("query candidate rows: {e}"))?;

        let mut entries: Vec<MemoryEntry> = Vec::new();
        for row in rows {
            let entry = row.map_err(|e| format!("read candidate row: {e}"))?;
            if processed_ids.contains(&entry.id) {
                continue;
            }
            if !entry.archived && entry.source != FOUNDRY_DISTILL_SOURCE {
                entries.push(entry);
            }
        }
        Ok((processed_ids, entries))
    })?;

    // processed_ids is only used inside the closure above; ignore here.
    let _ = processed_ids;

    if candidate_entries.is_empty() {
        return Ok(Vec::new());
    }

    let buckets = coherent_distill_buckets(candidate_entries);
    let mut groups = Vec::new();
    for (bucket_key, entries) in buckets {
        if entries.len() < MIN_BUCKET_SIZE {
            continue;
        }
        let (path_prefix, coherence_key) = bucket_key
            .split_once('#')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or_else(|| {
                let path_prefix = entries
                    .first()
                    .map(|e| scheduled_distill_path_prefix(&e.path))
                    .unwrap_or_else(|| "/".to_string());
                let coherence_key = entries
                    .first()
                    .and_then(|e| coherence_bucket_key(&e.topic, &e.entities))
                    .unwrap_or_else(|| "unknown".to_string());
                (path_prefix, coherence_key)
            });
        let group_id = format!(
            "{}|{}",
            sanitize_id_segment(&path_prefix),
            sanitize_id_segment(&coherence_key)
        );
        groups.push(CandidateGroup {
            group_id,
            path_prefix,
            coherence_key,
            entries,
        });
    }
    Ok(groups)
}

fn sanitize_id_segment(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').chars().take(40).collect::<String>()
}

fn build_batch_prompt(groups: &[CandidateGroup]) -> String {
    format!(
        "<system>\n{}\n</system>\n\n{}",
        DISTILL_DAILY_SYSTEM_PROMPT,
        build_batch_user_payload(groups)
    )
}

fn build_batch_user_payload(groups: &[CandidateGroup]) -> String {
    let mut payload = Vec::with_capacity(groups.len());
    for group in groups {
        let entries: Vec<Value> = group
            .entries
            .iter()
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "topic": entry.topic,
                    "path": entry.path,
                    "importance": entry.importance,
                    "summary": entry.summary,
                    "text": entry.text.chars().take(800).collect::<String>(),
                    "keywords": entry.keywords,
                    "entities": entry.entities,
                })
            })
            .collect();
        payload.push(json!({
            "group_id": group.group_id,
            "path_prefix": group.path_prefix,
            "coherence_key": group.coherence_key,
            "memories": entries,
        }));
    }
    let groups_json = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".to_string());
    // If payload exceeds token budget, drop groups from the tail to stay within limits.
    let (groups_json, actual_count) = if groups_json.len() > MAX_BATCH_PAYLOAD_CHARS {
        let mut trimmed = payload;
        while trimmed.len() > 1
            && serde_json::to_string_pretty(&trimmed)
                .unwrap_or_default()
                .len()
                > MAX_BATCH_PAYLOAD_CHARS
        {
            trimmed.pop();
        }
        let count = trimmed.len();
        (
            serde_json::to_string_pretty(&trimmed).unwrap_or_else(|_| "[]".to_string()),
            count,
        )
    } else {
        (groups_json, groups.len())
    };
    format!(
        "Here are {} groups to distill:\n\n{}",
        actual_count, groups_json
    )
}

#[derive(Debug, Clone)]
pub(crate) struct GroupPayload {
    summary: String,
    text: String,
    keywords: Vec<String>,
    skip_reason: Option<String>,
}

/// Parse the model's JSON-array response into a map keyed by group_id.
/// Tolerates ```json fences and leading prose.
pub(crate) fn parse_distill_response(raw: &str) -> Result<HashMap<String, GroupPayload>, String> {
    let json_text = LlmClient::extract_json_payload(raw)?;
    let arr: Value = serde_json::from_str(json_text).map_err(|e| {
        format!(
            "invalid distill JSON: {e} (snippet: {})",
            snippet(json_text)
        )
    })?;
    let arr = arr
        .as_array()
        .ok_or_else(|| "distill response must be a JSON array".to_string())?;

    let mut out = HashMap::with_capacity(arr.len());
    for item in arr {
        let Some(group_id) = item.get("group_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let text = item
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let summary = item
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let keywords = item
            .get("keywords")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect();
        let skip_reason = item
            .get("skip_reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        out.insert(
            group_id.to_string(),
            GroupPayload {
                summary,
                text,
                keywords,
                skip_reason,
            },
        );
    }
    Ok(out)
}

fn snippet(s: &str) -> String {
    s.chars().take(200).collect()
}

async fn fallback_distill(llm: &LlmClient, group: &CandidateGroup) -> Result<GroupPayload, String> {
    let user = build_fallback_user_payload(group);
    let text = llm
        .call_distill_llm(DISTILL_DAILY_SYSTEM_PROMPT_SINGLE, &user, None, 0.4, 600)
        .await?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err("fallback llm returned empty text".to_string());
    }
    let summary: String = trimmed.chars().take(120).collect();
    // Extract keywords from the distilled text and source group metadata
    let mut keywords: Vec<String> = Vec::new();
    for entry in &group.entries {
        keywords.extend(
            entry
                .keywords
                .iter()
                .filter(|k| !k.trim().is_empty())
                .cloned(),
        );
    }
    keywords.sort();
    keywords.dedup();
    keywords.truncate(12);
    Ok(GroupPayload {
        summary,
        text: trimmed,
        keywords,
        skip_reason: None,
    })
}

/// Same instructions as the batch prompt but for a single group — used as
/// the LLM fallback when Claude CLI fails or returns garbage.
const DISTILL_DAILY_SYSTEM_PROMPT_SINGLE: &str = r#"You are Tachi's memory distiller. Synthesize the provided related memories into a single concise, faithful summary under ~400 words. Drop chit-chat, keep durable facts (decisions, identifiers, file paths, commands, error signatures). Return ONLY the distilled prose, no JSON, no preamble."#;

fn build_fallback_user_payload(group: &CandidateGroup) -> String {
    let mut buf = String::new();
    buf.push_str(&format!(
        "path_prefix: {}\ncoherence_key: {}\n\n",
        group.path_prefix, group.coherence_key
    ));
    for (idx, entry) in group.entries.iter().enumerate() {
        buf.push_str(&format!(
            "[{}] topic={} importance={:.2}\nSummary: {}\nText: {}\n\n",
            idx + 1,
            if entry.topic.is_empty() {
                "unknown"
            } else {
                &entry.topic
            },
            entry.importance,
            entry.summary,
            entry.text.chars().take(600).collect::<String>()
        ));
    }
    buf
}

fn persist_distill_memory(
    server: &MemoryServer,
    group: &CandidateGroup,
    payload: &GroupPayload,
    batch_run_id: &str,
    backend: &str,
    fallback_used: bool,
) -> Result<String, String> {
    let agent_id = server
        .agent_runtime_read()
        .agent_profile
        .as_ref()
        .map(|p| p.agent_id.clone())
        .unwrap_or_else(|| "tachi_scheduler".to_string());
    let distill_root = super::maintenance::build_foundry_distill_root(&agent_id);
    let timestamp = Utc::now().to_rfc3339();
    let memory_id = uuid::Uuid::new_v4().to_string();
    let source_ids: Vec<String> = group.entries.iter().map(|e| e.id.clone()).collect();

    let bucket_key = format!("{}#{}", group.path_prefix, group.coherence_key);
    let namespace_key = group.path_prefix.clone();

    let metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "source_memory_ids": source_ids,
            "source_path_prefix": group.path_prefix,
            "namespace_key": namespace_key,
            "coherence_key": group.coherence_key,
            "bucket_key": bucket_key,
            "batch_run_id": batch_run_id,
            "group_id": group.group_id,
            "backend": backend,
            "fallback_used": fallback_used,
        }),
        "foundry_worker",
        "memory_distill",
        Some("project"),
        DbScope::Project,
        json!({
            "agent_id": agent_id,
            "path_prefix": group.path_prefix,
        }),
    );

    let summary = if payload.summary.trim().is_empty() {
        payload.text.chars().take(100).collect::<String>()
    } else {
        payload.summary.clone()
    };

    let mut keywords = vec!["foundry".to_string(), "distill".to_string()];
    keywords.extend(payload.keywords.iter().cloned());
    for entry in &group.entries {
        keywords.extend(entry.keywords.iter().cloned());
    }
    let keywords = dedup_strings(keywords);
    let entities = dedup_strings(
        group
            .entries
            .iter()
            .flat_map(|e| e.entities.clone())
            .collect(),
    );

    let entry = MemoryEntry {
        id: memory_id.clone(),
        path: format!("{distill_root}/{}", Utc::now().format("%Y%m%dT%H%M%S")),
        summary,
        text: payload.text.clone(),
        importance: 0.75,
        timestamp,
        valid_from: String::new(),
        valid_until: None,
        category: "other".to_string(),
        topic: "foundry_distill".to_string(),
        keywords,
        persons: Vec::new(),
        entities,
        location: group.path_prefix.clone(),
        source: FOUNDRY_DISTILL_SOURCE.to_string(),
        scope: "project".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy: None,
        domain: None,
    };

    server.with_project_store(|store| {
        store
            .upsert(&entry)
            .map_err(|e| format!("upsert distill memory: {e}"))
    })?;

    Ok(memory_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_distill_response_handles_array() {
        let raw = r#"[
            {"group_id":"g1","summary":"s1","text":"text one","keywords":["a","b"]},
            {"group_id":"g2","summary":"s2","text":"","skip_reason":"no signal"}
        ]"#;
        let map = parse_distill_response(raw).unwrap();
        assert_eq!(map.len(), 2);
        let g1 = map.get("g1").unwrap();
        assert_eq!(g1.text, "text one");
        assert_eq!(g1.keywords, vec!["a", "b"]);
        let g2 = map.get("g2").unwrap();
        assert_eq!(g2.text, "");
        assert_eq!(g2.skip_reason.as_deref(), Some("no signal"));
    }

    #[test]
    fn parse_distill_response_strips_fences() {
        let raw = "```json\n[{\"group_id\":\"x\",\"text\":\"hi\",\"summary\":\"\"}]\n```";
        let map = parse_distill_response(raw).unwrap();
        assert!(map.contains_key("x"));
    }

    #[test]
    fn parse_distill_response_rejects_non_array() {
        let err = parse_distill_response(r#"{"group_id":"x"}"#).unwrap_err();
        assert!(err.contains("must be a JSON array"), "got: {err}");
    }

    #[test]
    fn sanitize_id_segment_keeps_safe_chars() {
        assert_eq!(sanitize_id_segment("topic:foo bar"), "topic_foo_bar");
        assert_eq!(sanitize_id_segment("/project/x"), "project_x");
    }

    #[test]
    fn resolve_distill_backend_defaults_to_raw_api() {
        with_backend_env(None, || {
            assert_eq!(resolve_distill_backend(), DistillBackend::RawApi);
        });
    }

    #[test]
    fn resolve_distill_backend_recognises_claude_cli() {
        with_backend_env(Some("claude_cli"), || {
            assert_eq!(resolve_distill_backend(), DistillBackend::ClaudeCli);
        });
    }

    #[test]
    fn resolve_batch_size_defaults_to_six() {
        with_batch_size_env(None, || {
            assert_eq!(resolve_batch_size(), DEFAULT_GROUPS_PER_BATCH);
        });
    }

    fn with_backend_env<F: FnOnce()>(value: Option<&str>, f: F) {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let key = "FOUNDRY_DISTILL_BACKEND";
        let previous = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match previous {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn with_batch_size_env<F: FnOnce()>(value: Option<&str>, f: F) {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let key = "FOUNDRY_DISTILL_BATCH_SIZE";
        let previous = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match previous {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
