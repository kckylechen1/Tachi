use super::*;

pub(crate) async fn handle_extract_facts(
    server: &MemoryServer,
    params: ExtractFactsParams,
) -> Result<String, String> {
    let (target_db, _warning) = server.resolve_write_scope("project");
    let source = params.source.clone();

    let facts = match server.llm.extract_facts(&params.text).await {
        Ok(facts) => facts,
        Err(err) => {
            return serialize_json(serde_json::json!({
                "status": "failed",
                "reason": "llm_extraction_failed",
                "error": err,
                "source": source,
                "facts_extracted": 0,
                "facts_saved": 0
            }));
        }
    };

    if facts.is_empty() {
        return serialize_json(serde_json::json!({
            "status": "completed",
            "source": source,
            "facts_extracted": 0,
            "facts_saved": 0
        }));
    }

    let count = facts.len();
    let mut saved_facts = Vec::new();
    let mut dropped: Vec<serde_json::Value> = Vec::new();
    let saved = server
        .with_store_for_scope(target_db, |store| {
            let mut saved = 0;
            for fact in &facts {
                let metadata = crate::provenance::inject_provenance(
                    server,
                    serde_json::json!({"source": source.clone()}),
                    "extract_facts",
                    "fact_extraction",
                    Some("project"),
                    target_db,
                    serde_json::json!({
                        "extract_source": source.clone(),
                    }),
                );
                // Apply capture_gate filters (min-length and noise assessment) and
                // surface the rejection reason so facts_extracted vs facts_saved
                // gaps are explainable instead of silently vanishing.
                let mut entry = match fact_to_entry_with_reason(fact, "extraction", metadata) {
                    Ok(entry) => entry,
                    Err(reason) => {
                        dropped.push(serde_json::json!({
                            "reason": reason,
                            "text": fact.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                        }));
                        continue;
                    }
                };
                if is_lazy_source(&entry.source) && entry.importance < 0.5 {
                    entry.retention_policy = Some("ephemeral".to_string());
                }
                let fact_summary = serde_json::json!({
                    "id": entry.id.clone(),
                    "path": entry.path.clone(),
                    "topic": entry.topic.clone(),
                    "summary": entry.summary.clone(),
                    "importance": entry.importance,
                });
                if store.upsert(&entry).is_ok() {
                    saved += 1;
                    saved_facts.push(fact_summary);
                }
            }
            Ok(saved)
        })
        .map_err(|e| format!("DB write failed: {e}"))?;

    serialize_json(serde_json::json!({
        "status": "completed",
        "source": source,
        "facts_extracted": count,
        "facts_saved": saved,
        "facts_dropped": dropped.len(),
        "facts": saved_facts,
        "dropped": dropped,
    }))
}
