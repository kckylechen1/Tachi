use super::super::helpers::dedup_strings;
use super::super::FoundryMaintenanceItem;
use crate::server_state::MemoryServer;
use memory_core::MemoryEntry;
use tachi_foundry::FoundryJobMetadata;

/// LLM prompt parameters for the query-generation step. Kept here (not
/// in `mod.rs`) because nothing else needs them and tuning is local.
const RECALL_QUERY_LLM_TEMPERATURE: f32 = 0.2;
const RECALL_QUERY_LLM_MAX_TOKENS: u32 = 80;
const RECALL_QUERY_LLM_MAX_SOURCES: usize = 6;
const RECALL_QUERY_LLM_SYSTEM: &str = "You generate ONE short natural-language search query (5–15 words, no quotes, no leading verbs like 'find' or 'search', just the query phrase) that a user would type to retrieve the given memory snippets later. Output ONLY the query phrase on a single line.";

/// Resolve the query list for a recall-cache job, in priority order:
///   1. `metadata.queries` if the operator/orchestrator pre-supplied them.
///   2. One LLM-generated query phrased like a user search.
///   3. Heuristic keyword-bag fallback (legacy behavior, kept as safety net).
///
/// Always returns deduplicated, non-empty strings. Empty result means the
/// caller should skip the job (no source entries + no metadata queries).
pub(in crate::foundry_runtime_ops::recall_cache) async fn resolve_recall_cache_queries(
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
                tracing::warn!(
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
    FoundryJobMetadata::new(metadata)
        .value("queries")
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
