use super::helpers::{
    build_foundry_agent_root, build_openclaw_agent_root, dedup_strings,
    normalize_path_prefix_value, path_is_within_prefix, round3,
};
use super::{CompactContextDraft, RecallScope, RerankOutcome, SessionCaptureDraft};
use crate::server_state::MemoryServer;
use serde_json::{json, Value};
use tachi_foundry::build_foundry_distill_root;

fn value_text(row: &Value) -> String {
    row.get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn value_summary(row: &Value) -> String {
    row.get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn value_id(row: &Value) -> String {
    row.get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn value_path(row: &Value) -> String {
    row.get("path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn value_topic(row: &Value) -> String {
    row.get("topic")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn value_relevance(row: &Value) -> f64 {
    row.get("relevance")
        .and_then(Value::as_f64)
        .or_else(|| {
            row.get("score")
                .and_then(Value::as_object)
                .and_then(|score| score.get("final"))
                .and_then(Value::as_f64)
        })
        .unwrap_or(0.0)
}

fn value_string_array(row: &Value, key: &str) -> Vec<String> {
    row.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(super) fn build_rerank_document(row: &Value) -> String {
    let text = value_text(row);
    let topic = value_topic(row);
    let keywords = value_string_array(row, "keywords");
    [text, topic, keywords.join(", ")]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Provisional: minimum fraction of `top_k` hybrid-head items guaranteed to
/// survive rerank. Lower = more promotion room for tail items; raise to protect
/// more head items. Named threshold per AGENTS.md; tunable.
const HYBRID_HEAD_FRACTION: f64 = 0.5;

/// Stamp the relevance fields so the value rendered into agent context matches
/// the actual sort key. `relevance`/`score.final` reflect `blend_score` (the key
/// the output is ordered by); `rerank_score` keeps the raw provider signal for
/// observability when present.
fn apply_blend_relevance(mut row: Value, blend_score: f64, rerank_score: Option<f64>) -> Value {
    let blend = round3(blend_score);
    if let Value::Object(map) = &mut row {
        map.insert("relevance".into(), json!(blend));
        if let Some(raw) = rerank_score {
            map.insert("rerank_score".into(), json!(round3(raw)));
        }
        if let Some(Value::Object(score_map)) = map.get_mut("score") {
            score_map.insert("final".into(), json!(blend));
        }
    }
    row
}

pub(super) fn merge_rerank_order_with_hybrid_floor(
    rows: &[Value],
    order: &[(usize, f64)],
    top_k: usize,
) -> Vec<Value> {
    let output_len = top_k.min(rows.len());
    if output_len == 0 {
        return Vec::new();
    }

    // rerank_by_index: index -> (rerank_rank, provider_score). Keep first
    // occurrence per index; rerank_rank is the 1-based enumerate position.
    let mut rerank_by_index: std::collections::HashMap<usize, (usize, f64)> =
        std::collections::HashMap::new();
    for (rerank_rank, &(index, score)) in order.iter().enumerate() {
        if index < rows.len() {
            rerank_by_index
                .entry(index)
                .or_insert((rerank_rank + 1, score));
        }
    }
    let missing_rerank_rank = rows.len() + 1;

    // blend_score = reciprocal rank fusion of hybrid rank and rerank rank.
    let blend_score = |index: usize| -> f64 {
        let original_rank = index + 1;
        let rerank_rank = rerank_by_index
            .get(&index)
            .map(|(rank, _)| *rank)
            .unwrap_or(missing_rerank_rank);
        (1.0 / original_rank as f64) + (1.0 / rerank_rank as f64)
    };

    // Seatbelt: a guaranteed prefix of the hybrid head always survives, but its
    // position may shift down if tail items out-score it. head_floor is strictly
    // <= output_len, leaving room for tail promotions.
    let head_floor = ((output_len as f64) * HYBRID_HEAD_FRACTION).round() as usize;
    let head_floor = head_floor.min(output_len);

    // Guaranteed head indices 0..head_floor.
    let mut selected: Vec<usize> = (0..head_floor).collect();

    // Remaining slots filled by the top blend-scoring indices from the pool
    // head_floor..rows.len() (tail + any non-guaranteed head).
    let remaining = output_len.saturating_sub(head_floor);
    if remaining > 0 {
        let mut pool: Vec<usize> = (head_floor..rows.len()).collect();
        pool.sort_by(|&a, &b| {
            blend_score(b)
                .partial_cmp(&blend_score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.cmp(&b))
        });
        for index in pool.into_iter().take(remaining) {
            selected.push(index);
        }
    }

    // Order the selected set by blend_score descending (stable tiebreak: index
    // ascending) for the final output sequence.
    selected.sort_by(|&a, &b| {
        blend_score(b)
            .partial_cmp(&blend_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(&b))
    });

    selected
        .into_iter()
        .map(|index| {
            let rerank_score = rerank_by_index.get(&index).map(|(_, score)| *score);
            apply_blend_relevance(rows[index].clone(), blend_score(index), rerank_score)
        })
        .collect()
}

pub(super) fn build_prepend_context(rows: &[Value]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let memory_lines = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let id = row.get("id").and_then(Value::as_str).unwrap_or("unknown");
            let topic = value_topic(row);
            let relevance = value_relevance(row);
            let summary = value_summary(row);
            let text = value_text(row);
            let keywords = value_string_array(row, "keywords").join(", ");
            let entities = value_string_array(row, "entities").join(", ");

            [
                format!(
                    "M-ENTRY #{} [ID={}] [Topic={}] [Score={:.2}]",
                    idx + 1,
                    id,
                    if topic.is_empty() { "unknown" } else { &topic },
                    relevance
                ),
                format!(
                    "Summary: {}",
                    if summary.is_empty() {
                        text.chars().take(80).collect::<String>()
                    } else {
                        summary
                    }
                ),
                format!("Keywords: {} | Entities: {}", keywords, entities),
            ]
            .join("\n")
        })
        .collect::<Vec<_>>();

    let mut entity_links = Vec::new();
    for row in rows {
        let entities = value_string_array(row, "entities");
        if entities.len() >= 2 {
            entity_links.push(entities.join(" ↔ "));
        }
    }

    let mut block = format!(
        "\n<relevant-structured-memories>\n{}\n",
        memory_lines.join("\n\n")
    );
    if !entity_links.is_empty() {
        let deduped = dedup_strings(entity_links);
        block.push_str(&format!("\nEntity connections: {}\n", deduped.join(", ")));
    }
    block.push_str("</relevant-structured-memories>\n");
    block
}

/// Build a dedicated wiki knowledge context block from wiki search results.
pub(super) fn build_wiki_context(rows: &[Value]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let wiki_lines = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let path = row.get("path").and_then(Value::as_str).unwrap_or("unknown");
            let topic = value_topic(row);
            let relevance = value_relevance(row);
            let summary = value_summary(row);
            let text = value_text(row);
            let keywords = value_string_array(row, "keywords").join(", ");

            // Extract category from path for cleaner display
            let category = path
                .strip_prefix("/wiki/")
                .unwrap_or(path)
                .replace('/', " > ");

            [
                format!(
                    "W-ENTRY #{} [Category={}] [Topic={}] [Score={:.2}]",
                    idx + 1,
                    category,
                    if topic.is_empty() { "unknown" } else { &topic },
                    relevance
                ),
                format!(
                    "Summary: {}",
                    if summary.is_empty() {
                        text.chars().take(120).collect::<String>()
                    } else {
                        summary
                    }
                ),
                if keywords.is_empty() {
                    String::new()
                } else {
                    format!("Keywords: {}", keywords)
                },
            ]
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
        })
        .collect::<Vec<_>>();

    format!(
        "\n<wiki-knowledge>\n{}\n</wiki-knowledge>\n",
        wiki_lines.join("\n\n")
    )
}

pub(super) fn parse_compact_context_response(raw: &str) -> Result<CompactContextDraft, String> {
    let json_str = tachi_llm::LlmClient::extract_json_payload(raw)?;
    let parsed: CompactContextDraft = serde_json::from_str(json_str).map_err(|e| {
        format!(
            "Failed to parse compact_context JSON: {e} — response was: {}",
            json_str
        )
    })?;
    Ok(parsed)
}

pub(super) async fn run_compaction_model(
    server: &MemoryServer,
    prompt: &str,
    payload: &serde_json::Value,
    max_output_tokens: usize,
) -> Result<CompactContextDraft, String> {
    let request = serde_json::to_string_pretty(payload)
        .map_err(|e| format!("Failed to serialize compaction payload: {e}"))?;
    let raw = server
        .llm
        .call_extract_llm(
            prompt,
            &request,
            None,
            0.1,
            max_output_tokens.max(128).min(u32::MAX as usize) as u32,
        )
        .await?;
    parse_compact_context_response(&raw)
}

pub(super) fn parse_session_capture_response(
    raw: &str,
) -> Result<Vec<SessionCaptureDraft>, String> {
    let json_str = tachi_llm::LlmClient::extract_json_payload(raw)?;
    let parsed: Vec<SessionCaptureDraft> = serde_json::from_str(json_str).map_err(|e| {
        format!(
            "Failed to parse session capture JSON: {e} — response was: {}",
            json_str
        )
    })?;

    Ok(parsed
        .into_iter()
        .filter(|draft| !draft.text.trim().is_empty())
        .collect())
}

pub(crate) async fn rerank_rows_with_outcome(
    server: &MemoryServer,
    query: &str,
    rows: Vec<Value>,
    top_k: usize,
) -> (Vec<Value>, RerankOutcome) {
    if rows.len() <= 1 {
        return (
            rows.into_iter().take(top_k).collect(),
            RerankOutcome::NotNeeded,
        );
    }

    let docs = rows.iter().map(build_rerank_document).collect::<Vec<_>>();
    match server.llm.rerank_voyage(query, &docs, top_k).await {
        Ok(order) => {
            let out = merge_rerank_order_with_hybrid_floor(&rows, &order, top_k);
            let outcome = if out.is_empty() {
                RerankOutcome::Fallback
            } else {
                RerankOutcome::Applied
            };
            let rows = if out.is_empty() {
                rows.into_iter().take(top_k).collect()
            } else {
                out
            };
            (rows, outcome)
        }
        Err(err) => {
            tracing::warn!("[recall_context] rerank failed, falling back to hybrid ranking: {err}");
            (
                rows.into_iter().take(top_k).collect(),
                RerankOutcome::Fallback,
            )
        }
    }
}

pub(super) fn resolve_recall_scope(
    path_prefix: Option<&str>,
    agent_id: Option<&str>,
) -> RecallScope {
    let requested = path_prefix.and_then(normalize_path_prefix_value);
    let Some(agent_id) = agent_id else {
        return RecallScope {
            search_prefixes: vec![requested],
            allowed_prefixes: Vec::new(),
            warning: None,
        };
    };

    let openclaw_root = build_openclaw_agent_root(agent_id);
    let foundry_root = build_foundry_agent_root(agent_id);
    let foundry_distill_root = build_foundry_distill_root(agent_id);
    let allowed_prefixes = vec![openclaw_root.clone(), foundry_root];

    match requested {
        Some(prefix)
            if allowed_prefixes
                .iter()
                .any(|allowed| path_is_within_prefix(&prefix, allowed)) =>
        {
            RecallScope {
                search_prefixes: vec![Some(prefix)],
                allowed_prefixes,
                warning: None,
            }
        }
        Some(prefix) => RecallScope {
            search_prefixes: vec![Some(openclaw_root.clone()), Some(foundry_distill_root)],
            allowed_prefixes,
            warning: Some(format!(
                "path_prefix '{}' was outside agent scope; clamped to {}",
                prefix, openclaw_root
            )),
        },
        None => RecallScope {
            search_prefixes: vec![Some(openclaw_root), Some(foundry_distill_root)],
            allowed_prefixes,
            warning: None,
        },
    }
}
