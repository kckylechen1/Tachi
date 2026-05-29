use super::helpers::*;
use super::maintenance::*;
use super::*;

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
            let persons = value_string_array(row, "persons").join(", ");

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
                format!("Keywords: {} | Persons: {}", keywords, persons),
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
    let json_str = llm::LlmClient::extract_json_payload(raw)?;
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
    let json_str = llm::LlmClient::extract_json_payload(raw)?;
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
) -> (Vec<Value>, super::RerankOutcome) {
    if rows.len() <= 1 {
        return (
            rows.into_iter().take(top_k).collect(),
            super::RerankOutcome::NotNeeded,
        );
    }

    let docs = rows.iter().map(build_rerank_document).collect::<Vec<_>>();
    match server.llm.rerank_voyage(query, &docs, top_k).await {
        Ok(order) => {
            let mut out = Vec::with_capacity(order.len());
            for (index, score) in order {
                let Some(mut row) = rows.get(index).cloned() else {
                    continue;
                };
                if let Value::Object(map) = &mut row {
                    map.insert("relevance".into(), json!(round3(score)));
                    map.insert("rerank_score".into(), json!(round3(score)));
                    if let Some(Value::Object(score_map)) = map.get_mut("score") {
                        score_map.insert("final".into(), json!(round3(score)));
                    }
                }
                out.push(row);
            }
            let outcome = if out.is_empty() {
                super::RerankOutcome::Fallback
            } else {
                super::RerankOutcome::Applied
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
                super::RerankOutcome::Fallback,
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
    let allowed_prefixes = vec![openclaw_root.clone(), foundry_root.clone()];

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
