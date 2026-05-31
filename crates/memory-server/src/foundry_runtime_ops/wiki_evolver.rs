//! REM Wiki Evolver — weekly synthesis of `tier=pattern` memories into
//! draft wiki entries.
//!
//! ## What this does
//! 1. Collects `tier=pattern` memories from the last 7 days across
//!    global + project DBs.
//! 2. Clusters them by topic/keyword overlap (lightweight; no embeddings).
//! 3. For each cluster with ≥ 2 members, calls an LLM to synthesize a
//!    structured draft wiki entry.
//! 4. Writes each draft to the wiki project DB at `path=/wiki/drafts/<slug>`
//!    with `review_status=pending` in metadata.
//!
//! ## What this does NOT do
//! - Auto-activate: all output is `review_status=pending`. Humans (or a
//!   future gate step) must explicitly promote drafts to `/wiki/<slug>`.
//! - Modify existing wiki entries: drafts are always new entries.
//! - Run during daily pipeline: called once per week (Sunday 05:00 Shanghai).
//!
//! ## Safety
//! The LLM synthesis step uses low temperature and produces *pattern
//! summaries*, NOT causal inference claims. The module never writes to
//! the "active" wiki path space (`/wiki/` without `drafts/` prefix).

use std::collections::HashMap;

use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};

use super::*;
use crate::tool_params::TachiSaveParams;
use memory_core::types::MemoryEntry;

// ─── Constants ────────────────────────────────────────────────────────────────

/// Minimum cluster size to attempt LLM synthesis.
const MIN_CLUSTER_SIZE: usize = 2;

/// Maximum number of clusters to synthesize per weekly run (budget cap).
const MAX_CLUSTERS_PER_RUN: usize = 20;

/// Maximum memory entries per cluster sent to the LLM (keep prompt small).
const MAX_ENTRIES_PER_CLUSTER: usize = 8;

/// Minimum text length after noise scrubbing to include in synthesis.
const MIN_TEXT_LEN: usize = 60;

/// Importance assigned to pending draft entries (below "active" wiki threshold).
const DRAFT_IMPORTANCE: f64 = 0.65;

/// System prompt for wiki synthesis.
const WIKI_SYNTHESIS_SYSTEM: &str = r#"You are synthesizing programming session patterns into a structured wiki reference entry.

## Your Task
Read the clustered memory entries below. Produce a single cohesive wiki entry that a future agent can use as a reference — not a session log, but a clean how-to / decision record.

## Output Format
Return ONLY a single valid JSON object (no markdown, no wrapping):
{
  "title": "<clear, noun-phrase title, ≤80 chars>",
  "body": "<structured Markdown content, 300–1200 chars; use headers if helpful>",
  "summary": "<one sentence TL;DR, ≤120 chars>",
  "keywords": ["<tag1>", "<tag2>", ...],
  "entities": ["<project>", "<tool>", ...],
  "domain": "<primary domain, e.g. rust, mcp, agent, memory, infra>"
}

## Rules
1. Title must be a noun phrase describing the pattern — NOT a session headline.
2. Body must be written as forward-looking reference: what to do, why, and known gotchas.
3. Remove session noise: timestamps, agent tool traces, JSON blobs, file-path lists.
4. Keep concrete details: command patterns, crate names, error signatures, config keys.
5. Output ONLY the JSON object — no prose before or after.
"#;

// ─── Public entry point ───────────────────────────────────────────────────────

/// Run the weekly REM wiki evolution pass.
///
/// Called once per week (Sunday 05:00 Asia/Shanghai) from the daemon
/// bootstrap loop. Safe to call multiple times; dedup is handled via
/// metadata marker `rem.processed=1` on source entries.
pub(crate) async fn run_weekly_wiki_evolution(server: &MemoryServer) -> Result<WikiEvolverReport, String> {
    let candidates = collect_pattern_memories(server)?;
    if candidates.is_empty() {
        return Ok(WikiEvolverReport {
            clusters_found: 0,
            drafts_written: 0,
            skipped: 0,
            errors: 0,
        });
    }

    let clusters = cluster_by_topic(&candidates);
    let mut drafts_written = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for (topic, members) in clusters.iter().take(MAX_CLUSTERS_PER_RUN) {
        if members.len() < MIN_CLUSTER_SIZE {
            skipped += 1;
            continue;
        }

        match synthesize_and_save(server, topic, members).await {
            Ok(()) => {
                drafts_written += 1;
                // Mark source entries so they are not re-processed next week.
                if let Err(e) = mark_rem_processed(server, members.iter().map(|e| e.id.as_str())) {
                    eprintln!("[wiki_evolver] warn: failed to mark entries as processed for cluster '{topic}': {e}");
                }
            }
            Err(e) => {
                errors += 1;
                eprintln!("[wiki_evolver] error synthesizing cluster '{topic}': {e}");
            }
        }
    }

    let report = WikiEvolverReport {
        clusters_found: clusters.len(),
        drafts_written,
        skipped,
        errors,
    };
    eprintln!(
        "[wiki_evolver] weekly run complete: clusters={} drafts={} skipped={} errors={}",
        report.clusters_found, report.drafts_written, report.skipped, report.errors
    );
    Ok(report)
}

/// Summary returned to the caller (daily_pipeline / bootstrap loop).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WikiEvolverReport {
    pub clusters_found: usize,
    pub drafts_written: usize,
    pub skipped: usize,
    pub errors: usize,
}

// ─── Candidate collection ─────────────────────────────────────────────────────

fn collect_pattern_memories(server: &MemoryServer) -> Result<Vec<MemoryEntry>, String> {
    let collect = |store: &mut memory_core::MemoryStore| -> Result<Vec<MemoryEntry>, String> {
        let conn = store.connection();
        let mut stmt = conn
            .prepare(
                "SELECT id,path,summary,text,importance,timestamp,valid_from,valid_until,
                        category,topic,keywords,'[]' AS persons,entities,location,source,scope,archived,
                        access_count,last_access,revision,metadata,retention_policy,domain,
                        recall_count,query_diversity,tier
                 FROM memories
                 WHERE archived = 0
                   AND tier = 'pattern'
                   AND created_at > datetime('now', '-7 day')
                   AND (json_extract(metadata, '$.rem.processed') IS NULL
                        OR json_extract(metadata, '$.rem.processed') = 0)
                 ORDER BY importance DESC, access_count DESC
                 LIMIT 200",
            )
            .map_err(|e| format!("prepare pattern query: {e}"))?;
        let rows = stmt
            .query_map([], memory_core::row_to_entry)
            .map_err(|e| format!("query pattern memories: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    };

    let mut entries = Vec::new();
    if let Ok(global) = server.with_global_store_read(collect) {
        entries.extend(global);
    }
    if server.has_project_db() {
        if let Ok(project) = server.with_project_store_read(collect) {
            entries.extend(project);
        }
    }
    Ok(entries)
}

// ─── Clustering ────────────────────────────────────────────────────────────────

/// Lightweight topic clustering. Groups by:
///   1. Non-empty `topic` field (exact match).
///   2. First keyword that appears in ≥ 2 entries (frequency-based fallback).
///   3. Falls into `"_misc"` bucket (excluded from synthesis — too heterogeneous).
fn cluster_by_topic(entries: &[MemoryEntry]) -> HashMap<String, Vec<&MemoryEntry>> {
    let mut clusters: HashMap<String, Vec<&MemoryEntry>> = HashMap::new();

    // Pass 1 — group by explicit topic
    let mut remaining: Vec<&MemoryEntry> = Vec::new();
    for entry in entries {
        let topic = entry.topic.trim().to_string();
        if !topic.is_empty() {
            clusters.entry(topic).or_default().push(entry);
        } else {
            remaining.push(entry);
        }
    }

    // Pass 2 — count keyword frequencies across remaining entries
    let mut kw_freq: HashMap<String, usize> = HashMap::new();
    for entry in &remaining {
        let seen_in_entry: std::collections::HashSet<&str> =
            entry.keywords.iter().map(|k| k.as_str()).collect();
        for kw in seen_in_entry {
            *kw_freq.entry(kw.to_string()).or_insert(0) += 1;
        }
    }

    // Pass 3 — assign remaining entries to the highest-frequency keyword
    for entry in remaining {
        let best_kw = entry
            .keywords
            .iter()
            .filter_map(|kw| kw_freq.get(kw.as_str()).map(|freq| (kw, *freq)))
            .filter(|(_, freq)| *freq >= 2)
            .max_by_key(|(_, freq)| *freq)
            .map(|(kw, _)| kw.clone());

        let cluster_key = best_kw.unwrap_or_else(|| "_misc".to_string());
        clusters.entry(cluster_key).or_default().push(entry);
    }

    // Remove the misc bucket — too heterogeneous for synthesis
    clusters.remove("_misc");

    clusters
}

// ─── Synthesis ────────────────────────────────────────────────────────────────

async fn synthesize_and_save(
    server: &MemoryServer,
    topic: &str,
    members: &[&MemoryEntry],
) -> Result<(), String> {
    let draft = synthesize_wiki_draft(server, topic, members).await?;
    save_wiki_draft(server, topic, draft).await
}

async fn synthesize_wiki_draft(
    server: &MemoryServer,
    topic: &str,
    members: &[&MemoryEntry],
) -> Result<WikiDraft, String> {
    let scrubbed = members
        .iter()
        .take(MAX_ENTRIES_PER_CLUSTER)
        .filter_map(|e| {
            let text = crate::foundry_runtime_ops::scrub_agent_noise(&e.text);
            if text.trim().len() < MIN_TEXT_LEN {
                return None;
            }
            Some(json!({
                "summary": e.summary,
                "text": text.chars().take(600).collect::<String>(),
                "importance": e.importance,
                "keywords": e.keywords,
            }))
        })
        .collect::<Vec<_>>();

    if scrubbed.is_empty() {
        return Err("all cluster entries too short after noise scrubbing".to_string());
    }

    let user_payload = serde_json::to_string_pretty(&json!({
        "cluster_topic": topic,
        "entry_count": scrubbed.len(),
        "entries": scrubbed,
    }))
    .unwrap_or_default();

    let raw = server
        .llm
        .call_distill_llm(WIKI_SYNTHESIS_SYSTEM, &user_payload, None, 0.2, 1200)
        .await
        .map_err(|e| format!("wiki synthesis LLM call: {e}"))?;

    parse_wiki_draft(&raw, topic)
}

fn parse_wiki_draft(raw: &str, fallback_topic: &str) -> Result<WikiDraft, String> {
    let stripped = crate::llm::LlmClient::strip_code_fence(raw);
    let start = stripped.find('{').unwrap_or(0);
    let end = stripped.rfind('}').map(|i| i + 1).unwrap_or(stripped.len());
    let obj: Value = serde_json::from_str(&stripped[start..end])
        .map_err(|e| format!("parse wiki draft JSON: {e} — raw={}", &raw.chars().take(300).collect::<String>()))?;

    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback_topic)
        .to_string();
    let body = obj
        .get("body")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or("missing 'body' field in wiki draft")?
        .to_string();
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let keywords: Vec<String> = obj
        .get("keywords")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let entities: Vec<String> = obj
        .get("entities")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let domain = obj
        .get("domain")
        .and_then(Value::as_str)
        .unwrap_or("wiki")
        .to_string();

    Ok(WikiDraft { title, body, summary, keywords, entities, domain })
}

// ─── Persistence ─────────────────────────────────────────────────────────────

async fn save_wiki_draft(
    server: &MemoryServer,
    topic: &str,
    draft: WikiDraft,
) -> Result<(), String> {

    // Slugify: lowercase, spaces → hyphens, strip non-alphanumeric
    let slug = draft
        .title
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        topic
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
    } else {
        slug
    };

    let path = format!("/wiki/drafts/{slug}");

    // Build the text with a pending-review header so readers know the status.
    let full_text = format!(
        "<!-- review_status: pending | generated: {} -->\n\n{}",
        Utc::now().format("%Y-%m-%d"),
        draft.body,
    );

    let mut kw = draft.keywords;
    kw.push("rem-draft".to_string());
    kw.push("pending-review".to_string());
    kw.dedup();

    let result = server
        .tachi_save(Parameters(TachiSaveParams {
            text: full_text,
            id: None,
            kind: Some("wiki".to_string()),
            title: Some(draft.title.clone()),
            summary: Some(draft.summary),
            path: Some(path.clone()),
            importance: Some(DRAFT_IMPORTANCE),
            category: Some("experience".to_string()),
            keywords: kw,
            entities: draft.entities,
            scope: Some("global".to_string()),
            project: Some("wiki".to_string()),
            domain: Some(draft.domain),
            retention_policy: Some("durable".to_string()),
            force: true,
            topic: Some(topic.to_string()),
            source: Some("rem_wiki_evolver".to_string()),
            valid_from: None,
            valid_until: None,
            metadata: None,
        }))
        .await;

    match result {
        Ok(_) => {
            // Patch metadata to set review_status=pending. The tachi_save route
            // does not expose a metadata field, so we update directly after save.
            let now = Utc::now().to_rfc3339();
            if let Err(e) = server.with_named_project_store("wiki", |store| {
                store
                    .connection()
                    .execute(
                        r#"UPDATE memories
                           SET metadata = json_set(
                                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                                 '$.review_status', 'pending',
                                 '$.rem.generated_at', ?1
                               )
                           WHERE path = ?2
                             AND (json_extract(metadata, '$.review_status') IS NULL)"#,
                        rusqlite::params![now, path],
                    )
                    .map_err(|e| format!("patch review_status: {e}"))
            }) {
                eprintln!("[wiki_evolver] warn: could not patch review_status on draft '{path}': {e}");
            }
            Ok(())
        }
        Err(e) => Err(format!("tachi_save draft '{}': {e}", draft.title)),
    }
}

fn mark_rem_processed<'a>(
    server: &MemoryServer,
    ids: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    let ids: Vec<String> = ids.map(str::to_string).collect();
    if ids.is_empty() {
        return Ok(());
    }
    let now = Utc::now().to_rfc3339();

    let update = |store: &mut memory_core::MemoryStore| -> Result<(), String> {
        let conn = store.connection();
        for id in &ids {
            conn.execute(
                r#"UPDATE memories
                   SET metadata = json_set(
                         CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                         '$.rem.processed', 1,
                         '$.rem.processed_at', ?1
                       )
                   WHERE id = ?2"#,
                rusqlite::params![now, id],
            )
            .map_err(|e| format!("mark rem.processed for {id}: {e}"))?;
        }
        Ok(())
    };

    // Try both stores; ignore errors on the project store (read-only setups).
    let _ = server.with_global_store(update);
    if server.has_project_db() {
        let _ = server.with_project_store(|store: &mut memory_core::MemoryStore| {
            let conn = store.connection();
            for id in &ids {
                let _ = conn.execute(
                    r#"UPDATE memories
                       SET metadata = json_set(
                             CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                             '$.rem.processed', 1,
                             '$.rem.processed_at', ?1
                           )
                       WHERE id = ?2"#,
                    rusqlite::params![now, id],
                );
            }
            Ok(())
        });
    }
    Ok(())
}

// ─── Internal types ───────────────────────────────────────────────────────────

struct WikiDraft {
    title: String,
    body: String,
    summary: String,
    keywords: Vec<String>,
    entities: Vec<String>,
    domain: String,
}
