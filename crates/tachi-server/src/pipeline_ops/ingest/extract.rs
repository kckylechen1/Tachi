use super::*;

/// Real entry point for LLM atomization. Both the standalone `extract_facts`
/// tool (`tools/pipeline_facade.rs`) and the `tachi_memory(action='extract_facts')`
/// facade action call this same function with the same `ExtractFactsParams`
/// shape (#1043 D1) — there is exactly one atomization pipeline, not a
/// facade-specific single-row imposter.
///
/// On LLM failure the response is `atomized: false` with a `reason` — the
/// degrade behavior is unchanged (no facts saved) but is no longer silent:
/// callers can distinguish "atomizer ran, found nothing" from "atomizer
/// didn't run at all".
pub(crate) async fn handle_extract_facts(
    server: &MemoryServer,
    params: ExtractFactsParams,
) -> Result<String, String> {
    let source = params.source.clone();
    let project = params.project.clone();

    let facts = match server.llm.extract_facts(&params.text).await {
        Ok(facts) => facts,
        Err(err) => return llm_extraction_failed_response(&source, &err),
    };

    save_atomized_facts(server, facts, source, project)
}

/// Degrade-path response when the LLM atomizer itself is unavailable/errors.
/// Behavior is unchanged from before #1043 (no facts saved), but the
/// response is no longer silent about it — `atomized: false` lets a caller
/// tell "the atomizer ran and found nothing" apart from "the atomizer never
/// ran". Split out so this labeling is unit-testable without a live LLM call.
fn llm_extraction_failed_response(source: &str, err: &str) -> Result<String, String> {
    serialize_json(serde_json::json!({
        "status": "failed",
        "atomized": false,
        "reason": "llm_extraction_failed",
        "error": err,
        "source": source,
        "facts_extracted": 0,
        "facts_saved": 0
    }))
}

/// Shared save+response-shaping base for a batch of already-atomized facts.
/// Split out of `handle_extract_facts` so the multi-row save behavior is
/// directly unit-testable with a hand-built facts array — no live LLM call
/// required to exercise "N facts in -> N rows out".
pub(crate) fn save_atomized_facts(
    server: &MemoryServer,
    facts: Vec<serde_json::Value>,
    source: String,
    project: Option<String>,
) -> Result<String, String> {
    let (target_db, warning) = if project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope("project")
    };

    if facts.is_empty() {
        return serialize_json(serde_json::json!({
            "status": "completed",
            "atomized": true,
            "source": source,
            "facts_extracted": 0,
            "facts_saved": 0
        }));
    }

    let count = facts.len();
    let mut saved_facts = Vec::new();
    let mut dropped: Vec<serde_json::Value> = Vec::new();
    let mut write_errors: Vec<serde_json::Value> = Vec::new();
    let save_facts = |store: &mut memcore::MemoryStore| {
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
            match store.upsert(&entry) {
                Ok(()) => {
                    saved += 1;
                    saved_facts.push(fact_summary);
                }
                Err(err) => {
                    write_errors.push(serde_json::json!({
                        "id": entry.id,
                        "path": entry.path,
                        "error": err.to_string(),
                    }));
                }
            }
        }
        Ok(saved)
    };
    let saved = if let Some(ref project_name) = project {
        server.with_named_project_store(project_name, save_facts)
    } else {
        server.with_store_for_scope(target_db, save_facts)
    }
    .map_err(|e| format!("DB write failed: {e}"))?;

    let status = if write_errors.is_empty() {
        "completed"
    } else {
        "partial"
    };
    serialize_json(serde_json::json!({
        "status": status,
        "atomized": true,
        "source": source,
        "db": if target_db == DbScope::Project { "project" } else { "global" },
        "project": project,
        "warning": warning,
        "facts_extracted": count,
        "facts_saved": saved,
        "facts_dropped": dropped.len(),
        "write_errors": write_errors,
        "facts": saved_facts,
        "dropped": dropped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_server;

    fn fact(text: &str, topic: &str) -> serde_json::Value {
        serde_json::json!({
            "text": text,
            "topic": topic,
            "importance": 0.6,
        })
    }

    /// #1043 D1 judgement test 1: 3 independent facts atomize into 3 rows.
    /// Falsifies the regression this ticket names: a facade path that
    /// silently stores the whole source text as ONE entry regardless of how
    /// many facts the atomizer actually found.
    #[test]
    fn three_facts_atomize_into_three_rows_not_one() {
        let server = make_server();
        let count_rows = |server: &crate::MemoryServer| -> usize {
            server
                .with_store_for_scope(DbScope::Global, |store| {
                    store
                        .get_all(1000)
                        .map(|rows| rows.len())
                        .map_err(|e| e.to_string())
                })
                .expect("read back store")
        };
        let before = count_rows(&server);

        let facts = vec![
            fact(
                "The nightly database migration script runs at 2am UTC every day.",
                "ops",
            ),
            fact(
                "Payment retries are capped at three attempts before alerting on-call.",
                "payments",
            ),
            fact(
                "The staging environment shares its Redis instance with QA.",
                "infra",
            ),
        ];

        let body = save_atomized_facts(&server, facts, "test-source".to_string(), None)
            .expect("save_atomized_facts should succeed");
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json body");

        assert_eq!(value["atomized"], serde_json::json!(true));
        assert_eq!(value["facts_extracted"], serde_json::json!(3));
        assert_eq!(
            value["facts_saved"],
            serde_json::json!(3),
            "3 independent facts must save as 3 rows, not collapse to 1: {value}"
        );

        let after = count_rows(&server);
        assert_eq!(
            after - before,
            3,
            "store must gain 3 distinct rows after atomizing 3 facts, gained {}",
            after - before
        );
    }

    /// #1043 D1 judgement test 2: LLM-unavailable degrade path is labeled,
    /// not silent. A caller must be able to tell "the atomizer never ran"
    /// apart from "the atomizer ran and found nothing" from the response
    /// shape alone.
    #[test]
    fn llm_unavailable_labels_atomized_false() {
        let body = llm_extraction_failed_response("test-source", "provider unreachable")
            .expect("failure response always serializes");
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json body");

        assert_eq!(value["status"], serde_json::json!("failed"));
        assert_eq!(value["atomized"], serde_json::json!(false));
        assert_eq!(value["reason"], serde_json::json!("llm_extraction_failed"));
        assert_eq!(value["facts_saved"], serde_json::json!(0));
    }
}
