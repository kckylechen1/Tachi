//! Recall-rerank cache job.
//!
//! Builds the small population of `recall-cache/{topic}` ephemeral memory
//! entries that the recall path consults to skip a full hybrid-search +
//! rerank round trip on the next equivalent query. Split out of
//! `maintenance.rs` (PR #2) so the cache lifecycle — query generation,
//! candidate fetch, rerank, persist — lives in one focused file.
//!
//! Design notes pinned here so the next contributor doesn't relitigate:
//!
//!   * Cache writes are deterministic by `(named_project, path_prefix,
//!     top_k, query)` — re-running the same job overwrites the same
//!     `foundry:recall-cache:{stable_hash}` row instead of growing the
//!     table. This is intentional; cache rows are ephemeral and meant
//!     to be reaped by the retention policy.
//!
//!   * We deliberately do NOT enqueue an enrichment job for cache rows
//!     (PR #2 / Q4). The cache entry already carries the query string,
//!     `result_ids`, and per-result scores in `metadata`; running the
//!     enrichment pipeline would (a) burn Voyage embedding budget on
//!     transient text, (b) trigger graph auto-link from a synthetic
//!     entry, and (c) feed the recall search itself with cache rows as
//!     candidates. The on-write `retain` filter excludes cache rows
//!     from candidates regardless, but skipping enrichment removes the
//!     temptation entirely.
//!
//!   * Query selection (PR #2 / Q2): operator-supplied `metadata.queries`
//!     wins. Otherwise we ask the reasoning lane LLM for one short query
//!     summarizing the source memories' shared topic. Only if that fails
//!     (no API key, network error, empty output) do we fall back to the
//!     keyword-bag heuristic — the heuristic is now a safety net rather
//!     than the primary path. This keeps recall-cache queries phrased
//!     the way a real user would ask, which improves rerank hit rate.

use super::capture::persist_capture_entry;
use super::helpers::*;
use super::maintenance::{
    job_metadata_string, job_metadata_usize, job_metadata_value, with_foundry_store_read,
};
use super::recall::{rerank_rows, value_id, value_path, value_relevance, value_topic};
use super::*;
use serde_json::json;

/// LLM prompt parameters for the query-generation step. Kept here (not
/// in `mod.rs`) because nothing else needs them and tuning is local.
const RECALL_QUERY_LLM_TEMPERATURE: f32 = 0.2;
const RECALL_QUERY_LLM_MAX_TOKENS: u32 = 80;
const RECALL_QUERY_LLM_MAX_SOURCES: usize = 6;
const RECALL_QUERY_LLM_SYSTEM: &str = "You generate ONE short natural-language search query (5–15 words, no quotes, no leading verbs like 'find' or 'search', just the query phrase) that a user would type to retrieve the given memory snippets later. Output ONLY the query phrase on a single line.";

/// Public entry-point used by the maintenance worker dispatcher.
pub(super) async fn process_recall_rerank_cache_job(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
) -> Result<usize, String> {
    let source_entries = with_foundry_store_read(server, item, |store| {
        let mut entries = Vec::new();
        for memory_id in &item.memory_ids {
            if let Some(entry) = store
                .get(memory_id)
                .map_err(|e| format!("Failed to load memory {memory_id} for recall cache: {e}"))?
            {
                entries.push(entry);
            }
        }
        Ok(entries)
    })?;

    let queries = resolve_recall_cache_queries(server, item, &source_entries).await;
    if queries.is_empty() {
        return Ok(0);
    }

    let top_k = job_metadata_usize(&item.job.metadata, "top_k", FOUNDRY_RECALL_RERANK_TOP_K).max(1);
    let candidate_multiplier = job_metadata_usize(
        &item.job.metadata,
        "candidate_multiplier",
        FOUNDRY_RECALL_RERANK_CANDIDATE_MULTIPLIER,
    )
    .max(1);
    let candidate_top_k = top_k.saturating_mul(candidate_multiplier);
    let path_prefix = job_metadata_string(&item.job.metadata, "path_prefix")
        .or_else(|| normalize_path_prefix_value(&item.path_prefix));
    let agent_role = job_metadata_string(&item.job.metadata, "agent_role");
    let project = job_metadata_string(&item.job.metadata, "project").or(item.named_project.clone());
    let scope = if item.target_db == DbScope::Project {
        "project".to_string()
    } else {
        "global".to_string()
    };

    let mut updated = 0usize;
    for query in queries {
        let mut rows = search_memory_rows(
            server,
            SearchMemoryParams {
                query: query.clone(),
                query_vec: None,
                top_k: candidate_top_k,
                path_prefix: path_prefix.clone(),
                include_archived: false,
                candidates_per_channel: candidate_top_k.max(20),
                mmr_threshold: None,
                graph_expand_hops: 0,
                graph_relation_filter: None,
                weights: None,
                agent_role: agent_role.clone(),
                project: project.clone(),
                domain: None,
                enable_rerank: false,
            },
        )
        .await?;
        // `search_memory_rows` applies a *loose* `starts_with` prefix filter at
        // the DB level (for performance). We re-apply the stricter
        // `path_is_within_prefix` here to avoid false positives when a prefix
        // is a partial segment (e.g. "/foo/b" matching "/foo/bar").
        if let Some(prefix) = path_prefix.as_deref() {
            rows.retain(|row| {
                let path = value_path(row);
                !path.is_empty() && path_is_within_prefix(&path, prefix)
            });
        }
        // Never feed cache rows back into the rerank pool — they are not
        // ground truth, just a previous run's projection of it.
        rows.retain(|row| {
            row.get("source").and_then(serde_json::Value::as_str)
                != Some(FOUNDRY_RECALL_RERANK_CACHE_SOURCE)
        });
        let reranked = rerank_rows(server, &query, rows, top_k).await;
        if reranked.is_empty() {
            continue;
        }

        let cache_seed = format!(
            "{}|{}|{}|{}",
            item.named_project.as_deref().unwrap_or("default"),
            item.path_prefix,
            top_k,
            query
        );
        let cache_id = format!("foundry:recall-cache:{}", stable_hash(&cache_seed));
        let cache_topic = sanitize_safe_path_name(&query)
            .chars()
            .take(64)
            .collect::<String>();
        let cache_path = format!(
            "{}/recall-cache/{}",
            item.path_prefix.trim_end_matches('/'),
            cache_topic
        );
        let result_ids = reranked.iter().map(value_id).collect::<Vec<_>>();
        let result_scores = reranked
            .iter()
            .map(|row| {
                json!({
                    "id": value_id(row),
                    "score": round3(value_relevance(row)),
                    "path": value_path(row),
                    "topic": value_topic(row),
                })
            })
            .collect::<Vec<_>>();
        let timestamp = Utc::now().to_rfc3339();
        let text = build_recall_cache_text(&query, &reranked);
        let metadata = crate::provenance::inject_provenance(
            server,
            json!({
                "query": query,
                "top_k": top_k,
                "candidate_multiplier": candidate_multiplier,
                "candidate_top_k": candidate_top_k,
                "source_memory_ids": item.memory_ids.clone(),
                "result_ids": result_ids.clone(),
                "result_scores": result_scores,
                "job_id": item.job.id.clone(),
                "path_prefix": item.path_prefix.clone(),
            }),
            "foundry_worker",
            "recall_rerank_cache",
            Some(scope.as_str()),
            item.target_db,
            json!({
                "agent_id": item.job.target_agent_id.clone(),
                "path_prefix": item.path_prefix.clone(),
            }),
        );

        let cache_entry = MemoryEntry {
            id: cache_id,
            path: cache_path,
            summary: text.chars().take(100).collect(),
            text,
            importance: 0.35,
            timestamp,
            category: "other".to_string(),
            topic: "recall_rerank_cache".to_string(),
            keywords: vec![
                "foundry".to_string(),
                "recall".to_string(),
                "rerank".to_string(),
                "cache".to_string(),
            ],
            persons: Vec::new(),
            entities: result_ids,
            location: item.path_prefix.clone(),
            source: FOUNDRY_RECALL_RERANK_CACHE_SOURCE.to_string(),
            scope: scope.clone(),
            archived: false,
            access_count: 0,
            last_access: None,
            revision: 1,
            metadata,
            vector: None,
            retention_policy: Some("ephemeral".to_string()),
            domain: None,
        };

        persist_capture_entry(
            server,
            item.target_db,
            item.named_project.as_deref(),
            &cache_entry,
        )?;
        // PR #2 / Q4: intentionally NOT calling queue_capture_enrichment.
        // See module-level docs for the rationale.
        updated += 1;
    }

    Ok(updated)
}

/// Resolve the query list for a recall-cache job, in priority order:
///   1. `metadata.queries` if the operator/orchestrator pre-supplied them.
///   2. One LLM-generated query phrased like a user search.
///   3. Heuristic keyword-bag fallback (legacy behavior, kept as safety net).
///
/// Always returns deduplicated, non-empty strings. Empty result means the
/// caller should skip the job (no source entries + no metadata queries).
async fn resolve_recall_cache_queries(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    source_entries: &[MemoryEntry],
) -> Vec<String> {
    let mut queries = recall_cache_queries_from_metadata(&item.job.metadata);

    if queries.is_empty() && !source_entries.is_empty() {
        match generate_recall_cache_query_via_llm(server, item, source_entries).await {
            Ok(Some(query)) => queries.push(query),
            Ok(None) => {
                // LLM returned empty after trim — fall through to heuristic.
                queries.push(default_recall_cache_query(item, source_entries));
            }
            Err(err) => {
                // LLM unavailable / errored — log once and fall back. We
                // do NOT propagate the error: the cache is best-effort.
                eprintln!(
                    "[recall_rerank_cache] LLM query generation failed (job {}); using heuristic fallback: {err}",
                    item.job.id
                );
                queries.push(default_recall_cache_query(item, source_entries));
            }
        }
    }

    dedup_strings(queries)
}

/// Ask the extract lane (front-line LLM) for a single natural-language query that
/// represents the source memories' shared topic. Returns:
///   * `Ok(Some(query))` — non-empty trimmed query.
///   * `Ok(None)` — LLM returned empty/whitespace; caller decides fallback.
///   * `Err(_)` — provider/network error; caller decides fallback.
async fn generate_recall_cache_query_via_llm(
    server: &MemoryServer,
    item: &FoundryMaintenanceItem,
    source_entries: &[MemoryEntry],
) -> Result<Option<String>, String> {
    let mut snippets = Vec::with_capacity(source_entries.len().min(RECALL_QUERY_LLM_MAX_SOURCES));
    for entry in source_entries.iter().take(RECALL_QUERY_LLM_MAX_SOURCES) {
        let snippet = if !entry.summary.trim().is_empty() {
            entry.summary.clone()
        } else {
            entry.text.chars().take(220).collect::<String>()
        };
        snippets.push(format!(
            "- topic={} | {}",
            if entry.topic.trim().is_empty() {
                "(none)"
            } else {
                entry.topic.as_str()
            },
            snippet.trim()
        ));
    }
    let user = format!(
        "Path prefix: {}\nMemories:\n{}",
        item.path_prefix,
        snippets.join("\n")
    );

    let raw = server
        .llm
        .call_extract_llm(
            RECALL_QUERY_LLM_SYSTEM,
            &user,
            None,
            RECALL_QUERY_LLM_TEMPERATURE,
            RECALL_QUERY_LLM_MAX_TOKENS,
        )
        .await?;

    // Defensive cleanup: strip leading list markers, surrounding quotes,
    // and any second line the model might have emitted despite the
    // "single line" instruction.
    let first_line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let cleaned = first_line
        .trim_start_matches(|c: char| c == '-' || c == '*' || c == '>' || c.is_whitespace())
        .trim_matches(|c: char| c == '"' || c == '\'')
        .trim();
    if cleaned.is_empty() {
        Ok(None)
    } else {
        Ok(Some(cleaned.to_string()))
    }
}

fn recall_cache_queries_from_metadata(metadata: &serde_json::Value) -> Vec<String> {
    job_metadata_value(metadata, "queries")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| {
            value
                .as_str()
                .or_else(|| value.get("query").and_then(serde_json::Value::as_str))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn default_recall_cache_query(
    item: &FoundryMaintenanceItem,
    source_entries: &[MemoryEntry],
) -> String {
    let topics = dedup_strings(
        source_entries
            .iter()
            .flat_map(|entry| {
                std::iter::once(entry.topic.clone())
                    .chain(entry.keywords.iter().cloned())
                    .chain(entry.entities.iter().cloned())
            })
            .collect::<Vec<_>>(),
    );
    let topic_hint = topics.into_iter().take(8).collect::<Vec<_>>().join(" ");
    if !topic_hint.trim().is_empty() {
        return format!("{} {}", item.path_prefix, topic_hint);
    }

    let agent = item
        .job
        .target_agent_id
        .as_deref()
        .unwrap_or("agent")
        .trim();
    format!("durable context for {agent} {}", item.path_prefix)
}

fn row_string(row: &serde_json::Value, key: &str) -> String {
    row.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn build_recall_cache_text(query: &str, rows: &[serde_json::Value]) -> String {
    let mut lines = vec![format!("Recall rerank cache for query: {query}")];
    for (idx, row) in rows.iter().enumerate() {
        let id = value_id(row);
        let path = value_path(row);
        let topic = value_topic(row);
        let score = value_relevance(row);
        let summary = row_string(row, "summary");
        let text = row_string(row, "text");
        lines.push(format!(
            "{}. id={} score={:.3} topic={} path={}",
            idx + 1,
            if id.is_empty() { "unknown" } else { &id },
            score,
            if topic.is_empty() { "unknown" } else { &topic },
            if path.is_empty() { "unknown" } else { &path },
        ));
        lines.push(format!(
            "   {}",
            if summary.trim().is_empty() {
                text.chars().take(180).collect::<String>()
            } else {
                summary
            }
        ));
    }
    lines.join("\n")
}
