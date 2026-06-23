//! Alert, ask, consolidate, and readiness handlers for `tachi_memory`.

use super::evidence_format::{
    build_thinking_scaffold, evidence_rows, format_agent_status, json_string, parse_json_or_empty,
    sections_to_evidence, synthesis_markdown_text, wants_json,
};
use crate::agent_markdown;
use crate::facade_search_ops::collect_tachi_search_sections;
use crate::tool_params::*;
use crate::MemoryServer;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Alerts
// ---------------------------------------------------------------------------

pub(crate) async fn handle_memory_alerts(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let warnings = crate::status_ops::collect_agent_warning_lines(server).await;
    let wiki_counts = crate::wiki_ops::wiki_hygiene_counts(server).await?;
    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": "completed",
            "warnings": warnings,
            "wiki_counts": wiki_counts,
        }));
    }
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
        context_symbols: Vec::new(),
        category: params.category.clone(),
        include_archived: params.include_archived,
        include_training: params.include_training,
        enable_rerank: params.enable_rerank,
        as_of: params.as_of.clone(),
    };
    let (sections, _, _) = collect_tachi_search_sections(server, &search_params).await;
    let evidence = sections_to_evidence(&sections)?;
    // Tag each evidence row with the store it came from, and surface a warning
    // when the user didn't pin `project` and evidence spans both global and
    // project stores. Without this, ask synthesis can blend unrelated context
    // (e.g. "what is the current health status?" returns another project's API).
    let has_project_db = evidence_rows(&evidence)
        .iter()
        .any(|row| row.get("db").and_then(Value::as_str) == Some("project"));
    let uses_global = evidence_rows(&evidence)
        .iter()
        .any(|row| row.get("db").and_then(Value::as_str) == Some("global"));
    let cross_store = params.project.is_none() && has_project_db && uses_global;
    let evidence = inject_project_tags(evidence);
    let thinking = build_thinking_scaffold("ask", &query, &evidence);
    let synthesis = if params.synthesize {
        Some(synthesize_answer(server, &query, &evidence, params.model.as_deref()).await)
    } else {
        None
    };
    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": "completed",
            "query": query,
            "evidence": evidence,
            "thinking": thinking,
            "synthesis": synthesis,
            "cross_store": cross_store,
            "cross_store_hint": if cross_store {
                Some("evidence spans global and project stores; pass `project=...` to pin a library or restrict `scope` to one store".to_string())
            } else {
                None
            },
        }));
    }
    let synthesis_text = synthesis.as_ref().and_then(synthesis_markdown_text);
    let mut fields: Vec<(&str, String)> = vec![
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
    ];
    if cross_store {
        fields.push((
            "cross_store",
            "evidence spans global and project stores; pin `project=...` to scope to one library"
                .to_string(),
        ));
    }
    Ok(format_agent_status(
        "Tachi ask",
        &fields,
        Some(&evidence),
        synthesis_text.as_deref(),
    ))
}

/// Annotate each evidence row with the project DB it came from so consumers
/// (and the LLM synthesis prompt) can distinguish global vs project evidence
/// at a glance. We surface this as a `db` field that the synthesis prompt
/// already knows to honor; downstream markdown rendering reads it back via
/// [`format_agent_status`].
fn inject_project_tags(evidence: Value) -> Value {
    let Value::Array(rows) = evidence else {
        return evidence;
    };
    let tagged: Vec<Value> = rows
        .into_iter()
        .map(|mut row| {
            let db = row
                .get("db")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            if let Some(obj) = row.as_object_mut() {
                obj.insert("project_db".to_string(), Value::String(db));
            }
            row
        })
        .collect();
    Value::Array(tagged)
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
        context_symbols: Vec::new(),
        category: params.category.clone(),
        include_archived: params.include_archived,
        include_training: params.include_training,
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
    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": "dry_run",
            "candidates": candidates,
            "thinking": thinking,
            "synthesis": synthesis,
        }));
    }
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
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let status = parse_json_or_empty(crate::status_ops::handle_tachi_status_full(server).await?);
    let runtime = parse_json_or_empty(crate::memory_ops::handle_runtime_info(server).await?);
    let core_tool_names = [
        "tachi_tools",
        "runtime_info",
        "tachi_status",
        "tachi_memory",
        "tachi_save",
        "tachi_wiki",
        "tachi_briefing",
    ];
    let advanced_tool_names = ["tachi_shell", "tachi_orchestrator", "tachi_doctor_scan"];
    let native_tools = server.native_tool_visibility();
    let total_tools = native_tools.len();
    let tool_rows = native_tools
        .iter()
        .map(|(name, description, visible)| {
            let tier = if core_tool_names.contains(&name.as_str()) {
                "core"
            } else if advanced_tool_names.contains(&name.as_str()) {
                "advanced"
            } else {
                "native"
            };
            json!({
                "name": name,
                "tier": tier,
                "visible": *visible,
                "description": description,
                "reason": if *visible {
                    "exposed by active profile/TACHI_EXPOSED_TOOLS".to_string()
                } else {
                    "filtered out by active profile or TACHI_EXPOSED_TOOLS".to_string()
                },
            })
        })
        .collect::<Vec<_>>();
    let recent_kanban = crate::status_ops::list_recent_kanban_entries(server, 5);
    let kanban_count = recent_kanban.len();
    let health_score = status
        .get("health_score")
        .map(Value::to_string)
        .unwrap_or_else(|| "?".to_string());
    let visible_tools = tool_rows
        .iter()
        .filter(|tool| {
            tool.get("visible")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    let hidden_tools: Vec<String> = tool_rows
        .iter()
        .filter(|tool| {
            !tool
                .get("visible")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect();
    let hidden_core_tools: Vec<String> = tool_rows
        .iter()
        .filter(|tool| tool.get("tier").and_then(Value::as_str) == Some("core"))
        .filter(|tool| {
            !tool
                .get("visible")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect();
    let hidden_advanced_tools: Vec<String> = tool_rows
        .iter()
        .filter(|tool| tool.get("tier").and_then(Value::as_str) == Some("advanced"))
        .filter(|tool| {
            !tool
                .get("visible")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect();
    let tool_profile = server
        .active_tool_profile()
        .map(|profile| profile.as_str())
        .unwrap_or_else(|| crate::profiles::default_tool_profile().as_str());
    let suggestions = readiness_suggestions(&hidden_core_tools, &hidden_advanced_tools);
    let vector_health =
        crate::status_ops::database_vector_health_json(&server.global_db_path_buf());
    let pending_vectors = vector_health
        .get("pending_vectors")
        .or_else(|| vector_health.get("missing_vectors"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let readiness_warnings = status
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if wants_json(params.format.as_deref()) {
        return json_string(&json!({
            "status": "completed",
            "health": status,
            "runtime": runtime,
            "readiness_warnings": readiness_warnings,
            "tool_profile": tool_profile,
            "tools": tool_rows,
            "tool_visibility_summary": {
                "visible_count": visible_tools,
                "total_count": total_tools,
                "hidden": hidden_tools,
                "hidden_core": hidden_core_tools,
                "hidden_advanced": hidden_advanced_tools,
            },
            "suggestions": suggestions,
            "vector_health": vector_health,
            "recent_kanban": recent_kanban,
        }));
    }
    let tool_summary = if hidden_tools.is_empty() {
        format!("{visible_tools}/{total_tools} visible (all exposed)")
    } else {
        format!(
            "{visible_tools}/{} visible — hidden: {}",
            total_tools,
            hidden_tools.join(", ")
        )
    };
    let core_summary = if hidden_core_tools.is_empty() {
        "ok".to_string()
    } else {
        format!("missing: {}", hidden_core_tools.join(", "))
    };
    let advanced_summary = if hidden_advanced_tools.is_empty() {
        "visible".to_string()
    } else {
        format!("hidden by profile: {}", hidden_advanced_tools.join(", "))
    };
    let warning_summary = if readiness_warnings.is_empty() {
        "none".to_string()
    } else {
        readiness_warnings
            .iter()
            .filter_map(Value::as_str)
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ")
    };
    Ok(format_agent_status(
        "Tachi readiness",
        &[
            ("status", "completed".to_string()),
            ("health_score", health_score),
            ("warnings", warning_summary),
            ("tool_profile", tool_profile),
            ("tool_visibility", tool_summary),
            ("core_tools", core_summary),
            ("advanced_tools", advanced_summary),
            ("pending_vectors", pending_vectors.to_string()),
            ("recent_kanban", kanban_count.to_string()),
            (
                "runtime",
                runtime
                    .get("runtime")
                    .map(|r: &serde_json::Value| {
                        let name = r.get("name").and_then(Value::as_str).unwrap_or("tachi");
                        let ver = r.get("version").and_then(Value::as_str).unwrap_or("?");
                        format!("{name} v{ver}")
                    })
                    .unwrap_or_else(|| "available".to_string()),
            ),
        ],
        None,
        Some(&suggestions.join("\n")),
    ))
}

fn readiness_suggestions(
    hidden_core_tools: &[String],
    hidden_advanced_tools: &[String],
) -> Vec<String> {
    let mut suggestions = Vec::new();
    if !hidden_core_tools.is_empty() {
        suggestions.push(format!(
            "- Core tools are hidden: {}. Check TACHI_PROFILE/TACHI_EXPOSED_TOOLS or run `tachi setup`.",
            hidden_core_tools.join(", ")
        ));
    }
    if !hidden_advanced_tools.is_empty() {
        suggestions.push(
            "- Heavy coordination tools are intentionally hidden in minimal profiles; use `TACHI_PROFILE=coordinate` or `admin` for shell/orchestrator workflows."
                .to_string(),
        );
    }
    if suggestions.is_empty() {
        suggestions.push(
            "- Start with `tachi_briefing()` or `tachi_memory(action='briefing')`; call `tachi_tools()` before unfamiliar tool names."
                .to_string(),
        );
    }
    suggestions
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
    let system = "Answer using only the supplied Tachi evidence. Each evidence row carries a `db` field — values are `global` for the shared library and `project` for a workspace/named project DB. When evidence spans both stores, prefer rows most relevant to the question and explicitly call out claims grounded in cross-store evidence. If evidence is insufficient, say what is missing. Keep the answer concise and cite memory ids or paths when present.";
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
