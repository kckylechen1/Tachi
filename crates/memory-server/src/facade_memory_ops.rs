//! Business logic for the `tachi_memory` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_memory`].

use crate::agent_markdown;
use crate::facade_save_ops::handle_tachi_save;
use crate::facade_search_ops::{collect_tachi_search_sections, handle_tachi_search};
use crate::memory_search_ops::{handle_search_memory, search_memory_rows};
use crate::tool_params::*;
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) async fn handle_tachi_memory(
    server: &MemoryServer,
    params: TachiMemoryParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    match action.as_str() {
        "search" => {
            let query = params
                .query
                .clone()
                .ok_or_else(|| "query is required when action='search'".to_string())?;
            let search_params = TachiSearchParams {
                query,
                scope: params.scope.clone().unwrap_or_else(|| "all".to_string()),
                top_k: params.top_k,
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
            handle_tachi_search(server, search_params).await
        }
        "save" => {
            if let Some(body) = crate::cli_client::maybe_forward_write(
                server.global_db_path.as_path(),
                "tachi_memory",
                &params,
            )
            .await
            {
                return Ok(body);
            }

            let text = params
                .text
                .clone()
                .ok_or_else(|| "text is required when action='save'".to_string())?;
            // kind="wiki" should go through tachi_wiki — reject it here
            if params
                .kind
                .as_deref()
                .map(|k| k.eq_ignore_ascii_case("wiki"))
                .unwrap_or(false)
            {
                return Err(
                    "kind='wiki' is not supported via tachi_memory. Use tachi_wiki with action='write' instead.".to_string()
                );
            }
            let scope_is_note = params
                .scope
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("note"))
                .unwrap_or(false);
            let kind = params
                .kind
                .clone()
                .or_else(|| (!scope_is_note).then(|| "memory".to_string()));
            let save_params = TachiSaveParams {
                text,
                id: params.id.clone(),
                kind,
                title: params.title.clone(),
                summary: params.summary.clone(),
                path: params.path.clone(),
                importance: params.importance,
                category: params.category.clone(),
                keywords: params.keywords.clone(),
                entities: params.entities.clone(),
                scope: params.scope.clone(),
                project: params.project.clone(),
                domain: params.domain.clone(),
                retention_policy: params.retention_policy.clone(),
                force: params.force,
                topic: params.topic.clone(),
                source: params.source.clone(),
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                metadata: params.metadata.clone(),
            };
            let body = handle_tachi_save(server, save_params).await?;
            Ok(format_save_result(&body, params.path.as_deref()))
        }
        "extract_facts" => {
            if let Some(body) = crate::cli_client::maybe_forward_write(
                server.global_db_path.as_path(),
                "tachi_memory",
                &params,
            )
            .await
            {
                return Ok(body);
            }

            let text = params
                .text
                .clone()
                .ok_or_else(|| "text is required when action='extract_facts'".to_string())?;
            let save_params = TachiSaveParams {
                text,
                id: params.id.clone(),
                kind: Some("extract_facts".to_string()),
                title: params.title.clone(),
                summary: params.summary.clone(),
                path: params.path.clone(),
                importance: params.importance,
                category: params.category.clone(),
                keywords: params.keywords.clone(),
                entities: params.entities.clone(),
                scope: params.scope.clone(),
                project: params.project.clone(),
                domain: params.domain.clone(),
                retention_policy: params.retention_policy.clone(),
                force: params.force,
                topic: params.topic.clone(),
                source: params.source.clone(),
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                metadata: params.metadata.clone(),
            };
            let body = handle_tachi_save(server, save_params).await?;
            Ok(format_extract_result(&body))
        }
        "briefing" => handle_memory_briefing(server, &params).await,
        "checkpoint" => handle_memory_checkpoint(server, params).await,
        "alerts" => handle_memory_alerts(server, &params).await,
        "ask" => handle_memory_ask(server, &params).await,
        "consolidate" => handle_memory_consolidate(server, &params).await,
        "progress" => handle_memory_progress(server, &params).await,
        "readiness" => handle_memory_readiness(server, &params).await,
        _ => Err(format!(
            "Invalid action '{}'. Use 'search', 'save', 'extract_facts', 'briefing', 'checkpoint', 'alerts', 'ask', 'consolidate', 'progress', or 'readiness'.",
            params.action
        )),
    }
}

async fn handle_memory_briefing(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let query = params
        .query
        .clone()
        .or_else(|| params.topic.clone())
        .or_else(|| params.title.clone())
        .unwrap_or_else(|| "current task recent decisions blockers next steps".to_string());
    let top_k = params.top_k.max(1).min(12);
    let include_wiki = !matches!(
        params
            .scope
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("memory")
    );

    // Memory + wiki searches run concurrently to reduce briefing latency.
    let mem_params = SearchMemoryParams {
        query: query.clone(),
        query_vec: None,
        top_k: top_k.saturating_mul(2).max(top_k),
        path_prefix: params.path_prefix.clone(),
        include_archived: params.include_archived,
        candidates_per_channel: 20,
        mmr_threshold: Some(0.85),
        graph_expand_hops: 1,
        graph_relation_filter: None,
        weights: None,
        agent_role: None,
        project: params.project.clone(),
        domain: params.domain.clone(),
        file_context: params.file_context.clone(),
        error_context: params.error_context.clone(),
        enable_rerank: params.enable_rerank,
        as_of: params.as_of.clone(),
    };

    let wiki_params = if include_wiki {
        Some(SearchMemoryParams {
            query: query.clone(),
            query_vec: None,
            top_k: top_k.min(5),
            path_prefix: Some(
                params
                    .path_prefix
                    .clone()
                    .unwrap_or_else(|| "/wiki".to_string()),
            ),
            include_archived: params.include_archived,
            candidates_per_channel: 20,
            mmr_threshold: Some(0.85),
            graph_expand_hops: 1,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: params.project.clone(),
            domain: params.domain.clone(),
            file_context: params.file_context.clone(),
            error_context: params.error_context.clone(),
            enable_rerank: false,
            as_of: params.as_of.clone(),
        })
    } else {
        None
    };

    let (memories_result, wiki_result) =
        tokio::join!(handle_search_memory(server, mem_params), async {
            if let Some(wp) = wiki_params {
                search_memory_rows(server, wp).await
            } else {
                Ok(vec![])
            }
        });

    let memories = slim_memory_rows(parse_evidence_array(memories_result?));
    let wiki = if include_wiki {
        slim_memory_rows(serde_json::Value::Array(wiki_result?))
    } else {
        json!([])
    };

    let (warnings_res, board_res, checkpoints_res, wiki_counts_res) = tokio::join!(
        crate::status_ops::collect_agent_warning_lines(server),
        crate::dispatch_ops::handle_tachi_board(
            server,
            TachiBoardParams {
                state_filter: Some("all".to_string()),
                limit: Some(top_k.min(5)),
                project: params.project.clone(),
            },
        ),
        async { crate::status_ops::list_recent_checkpoint_entries(server, 3) },
        crate::wiki_ops::wiki_hygiene_counts(server),
    );
    let warnings = warnings_res;
    let board = slim_kanban(parse_json_or_empty(board_res?));
    let checkpoints = json!(checkpoints_res);
    let wiki_counts = wiki_counts_res?;
    let health_summary = json!({
        "health_score": if warnings.is_empty() { 95 } else { 85 },
        "warnings": warnings.iter().take(6).cloned().collect::<Vec<_>>(),
        "wiki": wiki_counts,
    });

    Ok(agent_markdown::format_briefing(
        &query,
        &memories,
        &wiki,
        &health_summary,
        &board,
        &checkpoints,
    ))
}

async fn handle_memory_checkpoint(
    server: &MemoryServer,
    params: TachiMemoryParams,
) -> Result<String, String> {
    let (body, display_path, already_formatted) = save_memory_checkpoint(server, params).await?;
    Ok(checkpoint_message(
        &body,
        display_path.as_deref(),
        already_formatted,
    ))
}

async fn save_memory_checkpoint(
    server: &MemoryServer,
    mut params: TachiMemoryParams,
) -> Result<(String, Option<String>, bool), String> {
    if let Some(body) = crate::cli_client::maybe_forward_write(
        server.global_db_path.as_path(),
        "tachi_memory",
        &params,
    )
    .await
    {
        return Ok((body, None, true));
    }

    let text = params
        .text
        .take()
        .or_else(|| params.summary.clone())
        .ok_or_else(|| "text or summary is required when action='checkpoint'".to_string())?;
    let title = params
        .title
        .take()
        .unwrap_or_else(|| "Agent checkpoint".to_string());
    let checkpoint_text = format!(
        "Checkpoint: {title}\n\n{text}\n\nRecorded at: {}",
        Utc::now().to_rfc3339()
    );
    let path = params.path.take().or_else(|| {
        Some(format!(
            "/agent/checkpoints/{}",
            Utc::now().format("%Y-%m-%d")
        ))
    });
    let display_path = path.clone();
    let save_params = TachiSaveParams {
        text: checkpoint_text,
        id: params.id.take(),
        kind: Some("memory".to_string()),
        title: Some(title),
        summary: params.summary.take(),
        path,
        importance: params.importance.or(Some(0.8)),
        category: params
            .category
            .take()
            .or_else(|| Some("experience".to_string())),
        keywords: merge_keywords(
            std::mem::take(&mut params.keywords),
            &["checkpoint", "agent-session"],
        ),
        entities: std::mem::take(&mut params.entities),
        scope: params.scope.take().or_else(|| Some("project".to_string())),
        project: params.project.take(),
        domain: params.domain.take(),
        retention_policy: params
            .retention_policy
            .take()
            .or_else(|| Some("durable".to_string())),
        force: true,
        topic: params
            .topic
            .take()
            .or_else(|| Some("checkpoint".to_string())),
        source: params
            .source
            .take()
            .or_else(|| Some("tachi_checkpoint".to_string())),
        valid_from: params.valid_from.take(),
        valid_until: params.valid_until.take(),
        metadata: params.metadata.take(),
    };
    let body = handle_tachi_save(server, save_params).await?;
    Ok((body, display_path, false))
}

pub(crate) async fn capture_latest_claude_jsonl_checkpoint(
    server: &MemoryServer,
) -> Result<Option<Value>, String> {
    let watcher = claude_jsonl_passive_watcher_status();
    let Some(path) = watcher.get("latest_jsonl").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let raw = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("read {path}: {e}"))?;
    let summary = summarize_jsonl_tail(&raw);
    if summary.trim().is_empty() {
        return Ok(Some(json!({
            "status": "skipped",
            "reason": "latest JSONL transcript had no text content",
            "path": path,
        })));
    }
    let params = TachiMemoryParams {
        action: "checkpoint".to_string(),
        query: None,
        scope: Some("project".to_string()),
        top_k: 6,
        path_prefix: None,
        file_context: None,
        error_context: None,
        category: Some("experience".to_string()),
        include_archived: false,
        enable_rerank: false,
        synthesize: false,
        model: None,
        text: Some(summary),
        title: Some("Claude JSONL passive checkpoint".to_string()),
        summary: Some("Passive checkpoint from latest Claude JSONL transcript".to_string()),
        topic: Some("claude-jsonl-passive-watcher".to_string()),
        keywords: vec!["claude-jsonl".to_string(), "passive-watcher".to_string()],
        entities: Vec::new(),
        importance: Some(0.65),
        retention_policy: Some("durable".to_string()),
        kind: None,
        path: None,
        id: None,
        force: true,
        source: Some("claude_jsonl_passive_watcher".to_string()),
        as_of: None,
        valid_from: None,
        valid_until: None,
        flow_id: None,
        event: None,
        state: None,
        project: None,
        domain: Some("agent".to_string()),
        metadata: None,
    };
    let (saved, display_path, already_formatted) = save_memory_checkpoint(server, params).await?;
    Ok(Some(json!({
        "status": "captured",
        "path": path,
        "saved": checkpoint_saved_payload(&saved, already_formatted),
        "message": checkpoint_message(&saved, display_path.as_deref(), already_formatted),
    })))
}

async fn handle_memory_alerts(
    server: &MemoryServer,
    _params: &TachiMemoryParams,
) -> Result<String, String> {
    let warnings = crate::status_ops::collect_agent_warning_lines(server).await;
    let wiki_counts = crate::wiki_ops::wiki_hygiene_counts(server).await?;
    Ok(agent_markdown::format_alerts(&warnings, &wiki_counts))
}

async fn handle_memory_ask(
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

async fn handle_memory_consolidate(
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

async fn handle_memory_progress(
    _server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let flow_id = params
        .flow_id
        .clone()
        .unwrap_or_else(|| format!("memory_{}", Utc::now().format("%Y%m%d")));
    let event = params
        .event
        .clone()
        .unwrap_or_else(|| "progress".to_string());
    let raw_text = params
        .text
        .clone()
        .or_else(|| params.summary.clone())
        .ok_or_else(|| "text or summary is required when action='progress'".to_string())?;
    let (text, redactions) = crate::memory_search_ops::scrub_secrets(&raw_text);
    let run_dir = progress_run_dir(&flow_id)?;
    let now = Utc::now().to_rfc3339();
    let line = json!({
        "timestamp": now,
        "flow_id": flow_id,
        "event": event,
        "state": params.state,
        "title": params.title,
        "summary": params.summary,
        "text": text,
        "project": params.project,
        "domain": params.domain,
        "secret_redactions": redactions,
    });
    append_jsonl(&run_dir.join("progress.jsonl"), &line)?;
    update_progress_status(&run_dir, &line)?;
    Ok(format_agent_status(
        "Tachi progress",
        &[
            ("status", "recorded".to_string()),
            ("flow", flow_id),
            ("log", run_dir.join("progress.jsonl").display().to_string()),
            ("secret_redactions", redactions.to_string()),
        ],
        None,
        None,
    ))
}

async fn handle_memory_readiness(
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
                    .and_then(|r| {
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

fn parse_json_or_empty(raw: String) -> Value {
    serde_json::from_str(&raw).unwrap_or_else(|_| {
        let preview: String = raw.chars().take(500).collect();
        json!({ "raw_preview": preview, "parse_error": true })
    })
}

fn sections_to_evidence(sections: &[(String, Value)]) -> Result<Value, String> {
    let mut rows = Vec::new();
    for (section, value) in sections {
        match value {
            Value::Array(items) => {
                for item in items {
                    let mut row = item.clone();
                    if let Some(obj) = row.as_object_mut() {
                        obj.insert("section".to_string(), json!(section));
                    }
                    rows.push(row);
                }
            }
            Value::String(text) if text.starts_with("Error:") => {
                return Err(format!("search backend ({section}): {text}"));
            }
            Value::String(text) => rows.push(json!({ "section": section, "summary": text })),
            other => rows.push(json!({ "section": section, "value": other })),
        }
    }
    Ok(Value::Array(rows))
}

fn compact_line(text: &str, limit: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= limit {
        one_line
    } else {
        let keep = limit.saturating_sub(3);
        format!("{}...", one_line.chars().take(keep).collect::<String>())
    }
}

fn format_save_result(raw: &str, requested_path: Option<&str>) -> String {
    let value = parse_json_or_empty(raw.to_string());
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("saved");
    let id = value.get("id").and_then(Value::as_str).unwrap_or("?");
    let path = value
        .get("path")
        .and_then(Value::as_str)
        .or(requested_path)
        .unwrap_or("/");
    let enrichment = value
        .get("enrichment")
        .map(|v| format!("; enrichment {v}"))
        .unwrap_or_default();
    let warning = value
        .get("warning")
        .and_then(Value::as_str)
        .map(|w| format!("\nWarning: {w}"))
        .unwrap_or_default();
    format!("Saved -> `{path}` (id: `{id}`, status: {status}{enrichment}){warning}")
}

fn format_extract_result(raw: &str) -> String {
    let value = parse_json_or_empty(raw.to_string());
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let extracted = value
        .get("facts_extracted")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let saved = value
        .get("facts_saved")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut out = vec![format!(
        "Extract facts -> {status}; extracted {extracted}, saved {saved}"
    )];
    if let Some(facts) = value.get("facts").and_then(Value::as_array) {
        for (idx, fact) in facts.iter().enumerate() {
            let path = fact.get("path").and_then(Value::as_str).unwrap_or("/");
            let text = fact
                .get("summary")
                .or_else(|| fact.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            out.push(format!(
                "{}. `{path}` - {}",
                idx + 1,
                compact_line(text, 100)
            ));
        }
    }
    out.join("\n")
}

fn checkpoint_message(raw: &str, display_path: Option<&str>, already_formatted: bool) -> String {
    if already_formatted {
        raw.to_string()
    } else {
        format_save_result(raw, display_path)
    }
}

fn checkpoint_saved_payload(raw: &str, already_formatted: bool) -> Value {
    if already_formatted {
        Value::Null
    } else {
        parse_json_or_empty(raw.to_string())
    }
}

fn format_agent_status(
    title: &str,
    fields: &[(&str, String)],
    rows: Option<&Value>,
    synthesis: Option<&str>,
) -> String {
    let mut out = vec![format!("## {title}")];
    for (key, value) in fields {
        out.push(format!("{key}: {value}"));
    }
    if let Some(rows) = rows {
        let evidence = evidence_rows(rows);
        if !evidence.is_empty() {
            out.push("\n### Evidence".to_string());
            for (idx, row) in evidence.into_iter().take(6).enumerate() {
                let topic = row.get("topic").and_then(Value::as_str).unwrap_or("entry");
                let path = row.get("path").and_then(Value::as_str).unwrap_or("/");
                let summary = row
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(no summary)");
                let score = evidence_score(row);
                out.push(format!(
                    "{}. **{}** {:.3} `{}` - {}",
                    idx + 1,
                    topic,
                    score,
                    path,
                    compact_line(summary, 100)
                ));
            }
        }
    }
    if let Some(synthesis) = synthesis.filter(|s| !s.trim().is_empty()) {
        out.push("\n### Synthesis".to_string());
        out.push(compact_line(synthesis, 600));
    }
    out.join("\n")
}

fn synthesis_markdown_text(value: &Value) -> Option<String> {
    value
        .get("answer")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value.get("error").and_then(Value::as_str).map(|err| {
                let status = value
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                format!("{status}: {err}")
            })
        })
}

fn slim_memory_rows(value: Value) -> Value {
    let rows = match value {
        Value::Array(rows) => rows,
        other => vec![other],
    };
    Value::Array(
        rows.into_iter()
            .take(12)
            .map(|row| {
                json!({
                    "id": row.get("id"),
                    "db": row.get("db"),
                    "path": row.get("path"),
                    "summary": row.get("summary"),
                    "topic": row.get("topic"),
                    "relevance": row.get("relevance"),
                })
            })
            .collect(),
    )
}

fn slim_kanban(value: Value) -> Value {
    json!({
        "count": value.get("count"),
        "tasks": value
            .get("tasks")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .take(5)
            .map(|task| {
                json!({
                    "summary": task.get("summary"),
                    "state": task.get("state"),
                    "updated_at": task.get("updated_at"),
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn evidence_rows(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(rows) => rows.iter().filter(|v| !v.is_null()).collect(),
        Value::Object(map) => map
            .values()
            .flat_map(|value| match value {
                Value::Array(rows) => rows.iter().filter(|v| !v.is_null()).collect::<Vec<_>>(),
                other if !other.is_null() => vec![other],
                _ => vec![],
            })
            .collect(),
        other if !other.is_null() => vec![other],
        _ => vec![],
    }
}

fn evidence_score(row: &Value) -> f64 {
    row.get("relevance")
        .and_then(Value::as_f64)
        .or_else(|| {
            row.get("score")
                .and_then(|score| score.get("final"))
                .and_then(Value::as_f64)
        })
        .or_else(|| row.get("score").and_then(Value::as_f64))
        .unwrap_or(0.0)
}

fn evidence_ref(row: &Value) -> Value {
    let relevance = row
        .get("relevance")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| evidence_score(row));
    json!({
        "id": row.get("id"),
        "path": row.get("path"),
        "summary": row.get("summary"),
        "topic": row.get("topic"),
        "relevance": relevance,
    })
}

fn build_thinking_scaffold(mode: &str, query: &str, evidence: &Value) -> Value {
    let mut rows = evidence_rows(evidence);
    rows.sort_by(|a, b| {
        evidence_score(b)
            .partial_cmp(&evidence_score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let evidence_count = rows.len();
    let top_score = rows.first().map(|row| evidence_score(row)).unwrap_or(0.0);
    let confidence = if evidence_count == 0 {
        "none"
    } else if top_score >= 0.75 || evidence_count >= 5 {
        "high"
    } else if top_score >= 0.35 || evidence_count >= 2 {
        "medium"
    } else {
        "low"
    };
    let key_evidence = rows
        .iter()
        .take(3)
        .map(|row| evidence_ref(row))
        .collect::<Vec<_>>();

    let mut gaps = Vec::new();
    if evidence_count == 0 {
        gaps.push("No evidence rows were retrieved.".to_string());
    }
    if top_score < 0.35 && evidence_count > 0 {
        gaps.push("Top evidence relevance is weak; treat conclusions as tentative.".to_string());
    }
    if query.trim().len() < 8 {
        gaps.push("Query is short; refine it with topic, path, or error context.".to_string());
    }

    let next_steps = if mode == "consolidate" {
        vec![
            "Group candidates by topic/path before deciding canonical memories.",
            "Prefer archive/supersede actions over deletion unless data is clearly junk.",
        ]
    } else if evidence_count == 0 {
        vec![
            "Search again with a more specific query or path_prefix.",
            "If this should be known, save a checkpoint before relying on recall.",
        ]
    } else {
        vec![
            "Answer only from key_evidence unless LLM synthesis is explicitly enabled.",
            "Call out uncertainty when gaps is non-empty.",
        ]
    };

    json!({
        "mode": mode,
        "query": query,
        "confidence": confidence,
        "evidence_count": evidence_count,
        "top_relevance": top_score,
        "key_evidence": key_evidence,
        "gaps": gaps,
        "next_steps": next_steps,
    })
}

async fn synthesize_answer(
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

fn validate_progress_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("Invalid flow_id: '{id}'"));
    }
    Ok(())
}

fn progress_run_dir(flow_id: &str) -> Result<PathBuf, String> {
    validate_progress_id(flow_id)?;
    let root = crate::shell_ops::shell_runs_root();
    let run_dir = root.join(flow_id);
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("create progress run dir: {e}"))?;
    Ok(run_dir)
}

fn append_jsonl(path: &Path, value: &Value) -> Result<(), String> {
    let line =
        serde_json::to_string(value).map_err(|e| format!("serialize progress event: {e}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    writeln!(file, "{line}").map_err(|e| format!("write {}: {e}", path.display()))
}

fn update_progress_status(run_dir: &Path, line: &Value) -> Result<(), String> {
    let status_path = run_dir.join("status.json");
    with_progress_status_lock(&status_path, || {
        let mut status = std::fs::read_to_string(&status_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(|| json!({}));
        if !status.is_object() {
            status = json!({});
        }
        let obj = status
            .as_object_mut()
            .ok_or_else(|| "progress status was not a JSON object".to_string())?;
        obj.insert("flow_id".to_string(), line["flow_id"].clone());
        obj.insert("updated_at".to_string(), line["timestamp"].clone());
        obj.insert("last_event".to_string(), line["event"].clone());
        if !line["state"].is_null() {
            obj.insert("state".to_string(), line["state"].clone());
        }
        if !line["title"].is_null() {
            obj.insert("title".to_string(), line["title"].clone());
        }
        let body =
            serde_json::to_string_pretty(&status).map_err(|e| format!("serialize status: {e}"))?;
        // Atomic write: write to temp file then rename to avoid corruption on crash
        let tmp_path = status_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, &body)
            .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &status_path).map_err(|e| {
            format!(
                "rename {} -> {}: {e}",
                tmp_path.display(),
                status_path.display()
            )
        })
    })
}

#[cfg(unix)]
fn with_progress_status_lock<T>(
    status_path: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    let lock_path = status_path.with_extension("json.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;
    let fd = lock_file.as_raw_fd();
    // SAFETY: `fd` is borrowed from a valid open `File`. `flock(LOCK_EX)` only
    // passes integer flags to the OS and does not dereference Rust pointers.
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if rc != 0 {
        return Err(format!(
            "flock {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let result = f();
    // SAFETY: `fd` is still valid — the lock file remains in scope. `flock(LOCK_UN)`
    // is a pure kernel operation; failure is non-fatal because the descriptor close
    // will release the lock anyway.
    unsafe {
        libc::flock(fd, libc::LOCK_UN);
    }
    result
}

#[cfg(not(unix))]
fn with_progress_status_lock<T>(
    _status_path: &Path,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    f()
}

fn claude_jsonl_passive_watcher_status() -> Value {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let candidates = vec![
        home.join(".claude").join("projects"),
        home.join(".config").join("claude").join("projects"),
    ];
    let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;
    let mut visited = HashSet::new();
    for root in candidates {
        visit_jsonl_files(&root, &mut newest, 0, &mut visited);
    }
    match newest {
        Some((path, modified)) => {
            let age_seconds = modified
                .elapsed()
                .map(|dur| dur.as_secs())
                .unwrap_or_default();
            json!({
                "status": "detected",
                "latest_jsonl": path.display().to_string(),
                "age_seconds": age_seconds,
                "note": "passive watcher is diagnostic-only; use capture_session or checkpoint for durable writes",
            })
        }
        None => json!({
            "status": "not_detected",
            "note": "no Claude JSONL transcript files found under known roots",
        }),
    }
}

fn visit_jsonl_files(
    root: &Path,
    newest: &mut Option<(PathBuf, std::time::SystemTime)>,
    depth: usize,
    visited: &mut HashSet<PathBuf>,
) {
    if depth > 4 || !root.exists() {
        return;
    }
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if !visited.insert(canonical) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            visit_jsonl_files(&path, newest, depth + 1, visited);
            continue;
        }
        if file_type.is_symlink() {
            let Ok(link_meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if link_meta.is_dir() {
                visit_jsonl_files(&path, newest, depth + 1, visited);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                consider_jsonl_file(&path, newest);
            }
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        consider_jsonl_file(&path, newest);
    }
}

fn consider_jsonl_file(path: &Path, newest: &mut Option<(PathBuf, std::time::SystemTime)>) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let Ok(modified) = meta.modified() else {
        return;
    };
    let should_replace = newest
        .as_ref()
        .map(|(_, current)| modified > *current)
        .unwrap_or(true);
    if should_replace {
        *newest = Some((path.to_path_buf(), modified));
    }
}

fn summarize_jsonl_tail(raw: &str) -> String {
    let mut snippets = Vec::new();
    let tail_lines: Vec<&str> = raw.lines().collect();
    let start = tail_lines.len().saturating_sub(80);
    for line in &tail_lines[start..] {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(text) = extract_jsonl_text(&value) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                snippets.push(trimmed.chars().take(500).collect::<String>());
            }
        }
    }
    snippets
        .into_iter()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn extract_jsonl_text(value: &Value) -> Option<String> {
    value
        .pointer("/message/content")
        .and_then(text_from_jsonl_content)
        .or_else(|| value.get("content").and_then(text_from_jsonl_content))
        .or_else(|| {
            value
                .pointer("/message/text")
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .or_else(|| {
            value
                .get("text")
                .and_then(|v| v.as_str().map(str::to_string))
        })
}

fn text_from_jsonl_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("content").and_then(|v| v.as_str()))
                })
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}

fn parse_evidence_array(raw: String) -> Value {
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(rows)) => Value::Array(rows),
        Ok(value) => Value::Array(vec![value]),
        Err(_) => Value::Array(vec![json!({ "raw": raw })]),
    }
}

fn merge_keywords(mut keywords: Vec<String>, defaults: &[&str]) -> Vec<String> {
    for default in defaults {
        if !keywords.iter().any(|keyword| keyword == default) {
            keywords.push((*default).to_string());
        }
    }
    keywords
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_scaffold_summarizes_key_evidence() {
        let evidence = json!([
            {
                "id": "low",
                "path": "/project/low",
                "summary": "Low relevance",
                "topic": "memory",
                "relevance": 0.2
            },
            {
                "id": "high",
                "path": "/project/high",
                "summary": "High relevance",
                "topic": "memory",
                "relevance": 0.82
            }
        ]);

        let thinking = build_thinking_scaffold("ask", "what happened", &evidence);

        assert_eq!(thinking["confidence"], json!("high"));
        assert_eq!(thinking["evidence_count"], json!(2));
        assert_eq!(thinking["key_evidence"][0]["id"], json!("high"));
        assert!(thinking["gaps"].as_array().unwrap().is_empty());
    }

    #[test]
    fn thinking_scaffold_marks_missing_evidence() {
        let thinking = build_thinking_scaffold("ask", "why", &json!([]));

        assert_eq!(thinking["confidence"], json!("none"));
        assert_eq!(thinking["evidence_count"], json!(0));
        assert!(thinking["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap == "No evidence rows were retrieved."));
    }

    #[test]
    fn checkpoint_forwarded_message_is_preserved() {
        let body = "Saved -> `/agent/checkpoints/2026-05-31` (id: `cp-123`, status: saved)";

        assert_eq!(
            checkpoint_message(body, None, true),
            "Saved -> `/agent/checkpoints/2026-05-31` (id: `cp-123`, status: saved)"
        );
    }

    #[test]
    fn checkpoint_forwarded_saved_payload_is_null() {
        let body = "Saved -> `/agent/checkpoints/2026-05-31` (id: `cp-123`, status: saved)";

        assert_eq!(checkpoint_saved_payload(body, true), Value::Null);
    }
}
