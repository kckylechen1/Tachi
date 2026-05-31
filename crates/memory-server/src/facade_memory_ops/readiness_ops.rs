//! Alert, ask, consolidate, and readiness handlers for `tachi_memory`.

use crate::agent_markdown;
use crate::facade_search_ops::collect_tachi_search_sections;
use crate::tool_params::*;
use crate::MemoryServer;
use super::evidence_format::{
    build_thinking_scaffold, evidence_rows, format_agent_status,
    parse_json_or_empty, sections_to_evidence, synthesis_markdown_text,
};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Alerts
// ---------------------------------------------------------------------------

pub(crate) async fn handle_memory_alerts(
    server: &MemoryServer,
    _params: &TachiMemoryParams,
) -> Result<String, String> {
    let warnings = crate::status_ops::collect_agent_warning_lines(server).await;
    let wiki_counts = crate::wiki_ops::wiki_hygiene_counts(server).await?;
    Ok(agent_markdown::format_alerts(&warnings, &wiki_counts))
}

// ---------------------------------------------------------------------------
// Ask
// ---------------------------------------------------------------------------

pub(crate) async fn handle_memory_ask(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let query = params
        .query
        .clone()
        .or_else(|| params.text.clone())
        .ok_or_else(|| "query or text is required when action='ask'".to_string())?;
    let search_params = TachiSearchParams {
        query: query.clone(),
        scope: params.scope.clone().unwrap_or_else(|| "all".to_string()),
        top_k: params.top_k.max(3).min(10),
        path_prefix: params.path_prefix.clone(),
        project: params.project.clone(),
        domain: params.domain.clone(),
        file_context: params.file_context.clone(),
        error_context: params.error_context.clone(),
        category: params.category.clone(),
        include_archived: params.include_archived,
        enable_rerank: true,
        as_of: params.as_of.clone(),
    };
    let (sections, _, _) = collect_tachi_search_sections(server, &search_params).await;
    let evidence = sections_to_evidence(&sections)?;
    let thinking = build_thinking_scaffold("ask", &query, &evidence);
    let synthesis = if params.synthesize {
        Some(synthesize_answer(server, &query, &evidence, params.model.as_deref()).await)
    } else {
        None
    };
    let synthesis_text = synthesis.as_ref().and_then(synthesis_markdown_text);
    Ok(format_agent_status(
        "Tachi ask",
        &[
            ("status", "completed".to_string()),
            ("query", query),
            (
                "evidence",
                format!("{} hit(s)", evidence_rows(&evidence).len()),
            ),
            (
                "confidence",
                thinking
                    .get("confidence")
                    .and_then(Value::as_str)
                    .unwrap_or("none")
                    .to_string(),
            ),
        ],
        Some(&evidence),
        synthesis_text.as_deref(),
    ))
}

// ---------------------------------------------------------------------------
// Consolidate
// ---------------------------------------------------------------------------

pub(crate) async fn handle_memory_consolidate(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let query = params
        .query
        .clone()
        .or_else(|| params.topic.clone())
        .unwrap_or_else(|| "duplicate stale superseded related memories".to_string());
    let search_params = TachiSearchParams {
        query,
        scope: params.scope.clone().unwrap_or_else(|| "all".to_string()),
        top_k: params.top_k.max(5).min(20),
        path_prefix: params.path_prefix.clone(),
        project: params.project.clone(),
        domain: params.domain.clone(),
        file_context: params.file_context.clone(),
        error_context: params.error_context.clone(),
        category: params.category.clone(),
        include_archived: params.include_archived,
        enable_rerank: params.enable_rerank,
        as_of: params.as_of.clone(),
    };
    let (sections, _, _) = collect_tachi_search_sections(server, &search_params).await;
    let candidates = sections_to_evidence(&sections)?;
    let thinking = build_thinking_scaffold(
        "consolidate",
        "Identify duplicate, stale, superseded, or merge-worthy memory consolidation candidates.",
        &candidates,
    );
    let synthesis = if params.synthesize {
        Some(
            synthesize_answer(
                server,
                "Identify duplicate, stale, superseded, or merge-worthy memory consolidation candidates. Return concise actions only.",
                &candidates,
                params.model.as_deref(),
            )
            .await,
        )
    } else {
        None
    };
    let synthesis_text = synthesis.as_ref().and_then(synthesis_markdown_text);
    Ok(format_agent_status(
        "Tachi consolidate",
        &[
            ("status", "dry_run".to_string()),
            (
                "candidates",
                format!("{} hit(s)", evidence_rows(&candidates).len()),
            ),
            (
                "confidence",
                thinking
                    .get("confidence")
                    .and_then(Value::as_str)
                    .unwrap_or("none")
                    .to_string(),
            ),
        ],
        Some(&candidates),
        synthesis_text.as_deref(),
    ))
}

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

pub(crate) async fn handle_memory_readiness(
    server: &MemoryServer,
    _params: &TachiMemoryParams,
) -> Result<String, String> {
    let status = parse_json_or_empty(crate::status_ops::handle_tachi_status_full(server).await?);
    let runtime = parse_json_or_empty(crate::memory_ops::handle_runtime_info(server).await?);
    let tools = [
        "tachi_status",
        "tachi_memory",
        "tachi_task",
        "tachi_wiki",
        "tachi_doctor_scan",
        "runtime_info",
    ];
    let env_patterns = std::env::var("TACHI_EXPOSED_TOOLS")
        .ok()
        .map(|raw| crate::profiles::parse_tool_patterns_csv(&raw))
        .filter(|patterns| !patterns.is_empty());
    let required_tools = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool,
                "visible": crate::profiles::tool_visible(
                    tool,
                    server.active_tool_profile(),
                    env_patterns.as_deref(),
                )
            })
        })
        .collect::<Vec<_>>();
    let kanban_count = crate::status_ops::list_recent_kanban_entries(server, 5).len();
    let health_score = status
        .get("health_score")
        .map(Value::to_string)
        .unwrap_or_else(|| "?".to_string());
    let visible_tools = required_tools
        .iter()
        .filter(|tool| {
            tool.get("visible")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    let vector_health =
        crate::status_ops::database_vector_health_json(&server.global_db_path_buf());
    let pending_vectors = vector_health
        .get("pending_vectors")
        .or_else(|| vector_health.get("missing_vectors"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(format_agent_status(
        "Tachi readiness",
        &[
            ("status", "completed".to_string()),
            ("health_score", health_score),
            (
                "visible_required_tools",
                format!("{visible_tools}/{}", tools.len()),
            ),
            ("pending_vectors", pending_vectors.to_string()),
            ("recent_kanban", kanban_count.to_string()),
            (
                "runtime",
                runtime
                    .get("runtime")
                    .and_then(|r: &serde_json::Value| {
                        let name = r.get("name").and_then(Value::as_str).unwrap_or("tachi");
                        let ver = r.get("version").and_then(Value::as_str).unwrap_or("?");
                        Some(format!("{name} v{ver}"))
                    })
                    .unwrap_or_else(|| "available".to_string()),
            ),
        ],
        None,
        None,
    ))
}

// ---------------------------------------------------------------------------
// Synthesis helper (shared by ask + consolidate)
// ---------------------------------------------------------------------------

pub(crate) async fn synthesize_answer(
    server: &MemoryServer,
    query: &str,
    evidence: &Value,
    model: Option<&str>,
) -> Value {
    let system = "Answer using only the supplied Tachi evidence. If evidence is insufficient, say what is missing. Keep it concise and cite memory ids or paths when present.";
    let evidence_text = serde_json::to_string(evidence).unwrap_or_else(|_| "[]".to_string());
    let user = format!("Question:\n{query}\n\nEvidence JSON:\n{evidence_text}");
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        server.llm.call_extract_llm(system, &user, model, 0.2, 700),
    )
    .await
    {
        Ok(Ok(answer)) => json!({
            "status": "completed",
            "answer": answer,
        }),
        Ok(Err(err)) => json!({
            "status": "failed",
            "error": err,
        }),
        Err(_) => json!({
            "status": "timeout",
            "error": "LLM synthesis timed out after 30s",
        }),
    }
}
