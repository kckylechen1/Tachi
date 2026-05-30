//! REM Wiki Evolver — weekly synthesis of `tier IN ('consolidated','pattern')` memories into
//! wiki entries and LATEST_TRUTHS.
//!
//! ## What this does
//! 1. Collects `tier IN ('consolidated','pattern')` memories from the last 7 days across
//!    global + project DBs.
//! 2. Clusters them by topic/keyword overlap (lightweight; no embeddings).
//! 3. For each cluster with ≥ 2 members, calls the REM LLM to synthesize a
//!    structured wiki entry.
//! 4. Writes each entry directly to `/wiki/<domain>/<slug>` (no drafts/review).
//! 5. Updates LATEST_TRUTHS at `/wiki/truths/<domain>` (merge with existing).
//! 6. Writes the synthesis back as a new `tier=pattern` memory for next cycle.
//!
//! ## What this does NOT do
//! - Modify existing wiki entries (except LATEST_TRUTHS which are merged).
//! - Run during daily pipeline: called once per week (Sunday 05:00 Shanghai).
//!
//! ## Safety
//! The LLM synthesis step uses low temperature and produces *pattern
//! summaries*, NOT causal inference claims.

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
                        category,topic,keywords,persons,entities,location,source,scope,archived,
                        access_count,last_access,revision,metadata,retention_policy,domain,
                        recall_count,query_diversity,tier
                 FROM memories
                 WHERE archived = 0
                   AND tier IN ('consolidated', 'pattern')
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
    let domain = draft.domain.clone();
    let body_for_truths = draft.body.clone();
    let title = draft.title.clone();
    let summary = draft.summary.clone();
    let keywords = draft.keywords.clone();
    let entities = draft.entities.clone();
    let draft_domain = draft.domain.clone();
    save_wiki_entry(server, topic, draft).await?;
    // Write LATEST_TRUTHS for the domain
    update_latest_truths(server, &domain, &body_for_truths).await?;
    // REM writeback: persist synthesis as a pattern memory for next cycle
    rem_writeback_pattern(server, topic, &title, &summary, &keywords, &entities, &draft_domain, &body_for_truths).await
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
        .call_rem_llm(WIKI_SYNTHESIS_SYSTEM, &user_payload, None, 0.2, 4096)
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

async fn save_wiki_entry(
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

    let path = format!("/wiki/{}/{}", draft.domain, slug);

    let full_text = draft.body.clone();

    let mut kw = draft.keywords;
    kw.push("rem-wiki".to_string());
    kw.dedup();

    let result = server
        .tachi_save(Parameters(TachiSaveParams {
            text: full_text,
            id: None,
            kind: Some("wiki".to_string()),
            title: Some(draft.title.clone()),
            summary: Some(draft.summary),
            path: Some(path.clone()),
            importance: Some(0.85),
            category: Some("experience".to_string()),
            keywords: kw,
            entities: draft.entities,
            scope: Some("global".to_string()),
            project: None,
            domain: Some(draft.domain.clone()),
            retention_policy: Some("durable".to_string()),
            force: true,
            topic: Some(topic.to_string()),
            source: Some("rem_wiki_evolver".to_string()),
            valid_from: None,
            valid_until: None,
        }))
        .await;

    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("tachi_save wiki entry '{}': {e}", draft.title)),
    }
}

/// Update the LATEST_TRUTHS wiki entry for a given domain.
///
/// Reads any existing truths entry at `/wiki/truths/{domain}`, merges new
/// insights into it, and writes back via `tachi_save` with `force=true`.
/// Scope is determined by whether the domain contains project-specific keywords.
async fn update_latest_truths(
    server: &MemoryServer,
    domain: &str,
    new_insights: &str,
) -> Result<(), String> {
    let path = format!("/wiki/truths/{domain}");

    // Determine scope: project-specific domains → project, else → global.
    let project_keywords = ["project", "app", "client", "frontend", "backend", "repo"];
    let scope = if project_keywords.iter().any(|kw| domain.to_ascii_lowercase().contains(kw)) {
        "project"
    } else {
        "global"
    };

    // Try to read existing truths entry from both stores
    let existing = server.with_global_store_read(|store| {
        memory_core::db::find_active_wiki_entry_by_path_or_topic(
            store.connection(), &path, &path,
        ).map_err(|e| format!("find existing truths: {e}"))
    }).ok().flatten()
    .or_else(|| {
        if server.has_project_db() {
            server.with_project_store_read(|store| {
                memory_core::db::find_active_wiki_entry_by_path_or_topic(
                    store.connection(), &path, &path,
                ).map_err(|e| format!("find existing truths (project): {e}"))
            }).ok().flatten()
        } else {
            None
        }
    });

    let merged_text = if let Some(existing) = existing {
        // Append new insights to existing, keeping previous content
        format!(
            "{}\n\n## Updated {}\n\n{}",
            existing.text.trim(),
            Utc::now().format("%Y-%m-%d"),
            new_insights,
        )
    } else {
        format!(
            "# Latest Truths: {}\n\nGenerated: {}\n\n{}",
            domain,
            Utc::now().format("%Y-%m-%d"),
            new_insights,
        )
    };

    let title = format!("Latest Truths: {domain}");
    server
        .tachi_save(Parameters(TachiSaveParams {
            text: merged_text,
            id: None,
            kind: Some("wiki".to_string()),
            title: Some(title),
            summary: Some(format!("Core truths for domain: {domain}")),
            path: Some(path.clone()),
            importance: Some(0.90),
            category: Some("experience".to_string()),
            keywords: vec!["latest-truths".to_string(), domain.to_string()],
            entities: Vec::new(),
            scope: Some(scope.to_string()),
            project: None,
            domain: Some(domain.to_string()),
            retention_policy: Some("permanent".to_string()),
            force: true,
            topic: Some(format!("truths-{domain}")),
            source: Some("rem_wiki_evolver".to_string()),
            valid_from: None,
            valid_until: None,
        }))
        .await
        .map_err(|e| format!("tachi_save truths '{path}': {e}"))?;

    eprintln!("[wiki_evolver] updated LATEST_TRUTHS for domain '{domain}' at {path}");
    Ok(())
}

/// REM writeback: persist the synthesized wiki insights as a new pattern memory
/// so that the next weekly cycle can build upon it.
async fn rem_writeback_pattern(
    server: &MemoryServer,
    topic: &str,
    title: &str,
    summary: &str,
    keywords: &[String],
    entities: &[String],
    domain: &str,
    body: &str,
) -> Result<(), String> {
    let text = format!(
        "# {}\n\n{}",
        title,
        body.chars().take(1200).collect::<String>()
    );

    server
        .tachi_save(Parameters(TachiSaveParams {
            text,
            id: None,
            kind: None,
            title: Some(title.to_string()),
            summary: Some(summary.to_string()),
            path: None,
            importance: Some(0.80),
            category: Some("experience".to_string()),
            keywords: keywords.to_vec(),
            entities: entities.to_vec(),
            scope: Some("global".to_string()),
            project: None,
            domain: Some(domain.to_string()),
            retention_policy: Some("durable".to_string()),
            force: false,
            topic: Some(topic.to_string()),
            source: Some("rem_wiki_evolver".to_string()),
            valid_from: None,
            valid_until: None,
        }))
        .await
        .map_err(|e| format!("rem writeback pattern: {e}"))?;

    Ok(())
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
