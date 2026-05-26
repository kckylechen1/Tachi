//! Business logic for the `tachi_memory` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_memory`].

use crate::facade_save_ops::handle_tachi_save;
use crate::facade_search_ops::handle_tachi_search;
use crate::memory_ops::handle_memory_stats;
use crate::tool_params::*;
use crate::wiki_ops::handle_wiki_lint;
use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
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
            };
            handle_tachi_save(server, save_params).await
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
            };
            handle_tachi_save(server, save_params).await
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

    let search_params = TachiSearchParams {
        query,
        scope: params.scope.clone().unwrap_or_else(|| "all".to_string()),
        top_k,
        path_prefix: params.path_prefix.clone(),
        project: params.project.clone(),
        domain: params.domain.clone(),
        file_context: params.file_context.clone(),
        error_context: params.error_context.clone(),
        category: params.category.clone(),
        include_archived: params.include_archived,
        enable_rerank: params.enable_rerank,
    };

    let hits = parse_json_or_empty(handle_tachi_search(server, search_params).await?);
    let status = parse_json_or_empty(crate::status_ops::handle_tachi_status(server).await?);
    let stats = parse_json_or_empty(handle_memory_stats(server).await?);
    let wiki_lint = parse_json_or_empty(
        handle_wiki_lint(
            server,
            WikiLintParams {
                path_prefix: Some("/wiki".to_string()),
                checks: vec![
                    "orphans".to_string(),
                    "stale".to_string(),
                    "dirty_data".to_string(),
                    "duplicates".to_string(),
                    "missing_edges".to_string(),
                ],
                limit: top_k.max(10).min(50),
                stale_days: 90,
                missing_edge_threshold: 0.72,
                contradiction_threshold: 0.75,
            },
        )
        .await?,
    );
    let board = parse_json_or_empty(
        crate::dispatch_ops::handle_tachi_board(
            server,
            TachiBoardParams {
                state_filter: Some("all".to_string()),
                limit: Some(top_k.min(10)),
                project: params.project.clone(),
            },
        )
        .await?,
    );
    let compressed_health = compress_health(&status, &wiki_lint, &board);
    let passive_watcher = tokio::task::spawn_blocking(claude_jsonl_passive_watcher_status)
        .await
        .unwrap_or_else(|err| {
            json!({
                "status": "error",
                "error": err.to_string(),
            })
        });

    serde_json::to_string(&json!({
        "status": "completed",
        "mode": "briefing",
        "generated_at": Utc::now().to_rfc3339(),
        "health_summary": compressed_health,
        "health": status,
        "stats": stats,
        "wiki_lint": wiki_lint,
        "kanban": board,
        "recent_checkpoints": crate::status_ops::list_recent_checkpoint_entries(server, 5),
        "passive_watcher": passive_watcher,
        "context": hits,
        "recommended_agent_habit": [
            "Call tachi_memory action='briefing' near session start when the task is non-trivial.",
            "Call action='checkpoint' before handoff or after important decisions.",
            "Call action='alerts' when health warnings or wiki hygiene may affect work."
        ]
    }))
    .map_err(|e| format!("serialize briefing: {e}"))
}

async fn handle_memory_checkpoint(
    server: &MemoryServer,
    params: TachiMemoryParams,
) -> Result<String, String> {
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
        .or_else(|| params.summary.clone())
        .ok_or_else(|| "text or summary is required when action='checkpoint'".to_string())?;
    let title = params
        .title
        .clone()
        .unwrap_or_else(|| "Agent checkpoint".to_string());
    let checkpoint_text = format!(
        "Checkpoint: {title}\n\n{text}\n\nRecorded at: {}",
        Utc::now().to_rfc3339()
    );
    let path = params.path.clone().or_else(|| {
        Some(format!(
            "/agent/checkpoints/{}",
            Utc::now().format("%Y-%m-%d")
        ))
    });
    let save_params = TachiSaveParams {
        text: checkpoint_text,
        id: params.id.clone(),
        kind: Some("memory".to_string()),
        title: Some(title),
        summary: params.summary.clone(),
        path,
        importance: params.importance.or(Some(0.8)),
        category: params
            .category
            .clone()
            .or_else(|| Some("experience".to_string())),
        keywords: merge_keywords(params.keywords.clone(), &["checkpoint", "agent-session"]),
        entities: params.entities.clone(),
        scope: params.scope.clone().or_else(|| Some("project".to_string())),
        project: params.project.clone(),
        domain: params.domain.clone(),
        retention_policy: params
            .retention_policy
            .clone()
            .or_else(|| Some("durable".to_string())),
        force: true,
        topic: params
            .topic
            .clone()
            .or_else(|| Some("checkpoint".to_string())),
        source: params
            .source
            .clone()
            .or_else(|| Some("tachi_checkpoint".to_string())),
    };
    handle_tachi_save(server, save_params).await
}

pub(crate) async fn capture_latest_claude_jsonl_checkpoint(
    server: &MemoryServer,
) -> Result<Option<Value>, String> {
    let watcher = claude_jsonl_passive_watcher_status();
    let Some(path) = watcher.get("latest_jsonl").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
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
        flow_id: None,
        event: None,
        state: None,
        project: None,
        domain: Some("agent".to_string()),
    };
    let saved = handle_memory_checkpoint(server, params).await?;
    Ok(Some(json!({
        "status": "captured",
        "path": path,
        "saved": parse_json_or_empty(saved),
    })))
}

async fn handle_memory_alerts(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let status = parse_json_or_empty(crate::status_ops::handle_tachi_status(server).await?);
    let lint_params = WikiLintParams {
        path_prefix: params
            .path_prefix
            .clone()
            .or_else(|| Some("/wiki".to_string())),
        checks: vec![
            "orphans".to_string(),
            "stale".to_string(),
            "dirty_data".to_string(),
            "duplicates".to_string(),
        ],
        limit: params.top_k.max(10).min(100),
        stale_days: 90,
        missing_edge_threshold: 0.72,
        contradiction_threshold: 0.75,
    };
    let wiki_alerts = parse_json_or_empty(handle_wiki_lint(server, lint_params).await?);
    serde_json::to_string(&json!({
        "status": "completed",
        "mode": "alerts",
        "health": status,
        "wiki": wiki_alerts,
    }))
    .map_err(|e| format!("serialize alerts: {e}"))
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
    };
    let evidence = parse_evidence_array(handle_tachi_search(server, search_params).await?);
    let synthesis = if params.synthesize {
        Some(synthesize_answer(server, &query, &evidence, params.model.as_deref()).await)
    } else {
        None
    };
    serde_json::to_string(&json!({
        "status": "completed",
        "mode": "ask",
        "query": query,
        "answer_policy": "Use the evidence array below; if evidence is insufficient, say so instead of inventing details.",
        "synthesis": synthesis,
        "evidence": evidence,
    }))
    .map_err(|e| format!("serialize ask: {e}"))
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
    };
    let candidates = parse_json_or_empty(handle_tachi_search(server, search_params).await?);
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
    serde_json::to_string(&json!({
        "status": "dry_run",
        "mode": "consolidate",
        "candidates": candidates,
        "synthesis": synthesis,
        "next_steps": [
            "Review candidates and decide canonical entries before mutating memory.",
            "Use archive_memory/delete_memory or a dedicated repair command only after explicit approval."
        ]
    }))
    .map_err(|e| format!("serialize consolidate: {e}"))
}

async fn handle_memory_progress(
    server: &MemoryServer,
    params: &TachiMemoryParams,
) -> Result<String, String> {
    let flow_id = params
        .flow_id
        .clone()
        .unwrap_or_else(|| format!("memory_{}", Utc::now().format("%Y%m%d")));
    let event = params.event.clone().unwrap_or_else(|| "progress".to_string());
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
    Ok(json!({
        "status": "recorded",
        "mode": "progress",
        "flow_id": flow_id,
        "run_dir": run_dir.display().to_string(),
        "progress_log": run_dir.join("progress.jsonl").display().to_string(),
        "secret_redactions": redactions,
        "next_actions": [
            "For long-running work, append progress after each validated step.",
            "Use tachi_memory action='checkpoint' for durable handoff summaries."
        ],
        "vector_health": crate::status_ops::database_vector_health_json(&server.global_db_path_buf()),
    })
    .to_string())
}

async fn handle_memory_readiness(
    server: &MemoryServer,
    _params: &TachiMemoryParams,
) -> Result<String, String> {
    let status = parse_json_or_empty(crate::status_ops::handle_tachi_status(server).await?);
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
    serde_json::to_string(&json!({
        "status": "completed",
        "mode": "readiness",
        "runtime": runtime,
        "health": status,
        "required_tools": required_tools,
        "recent_checkpoints": crate::status_ops::list_recent_checkpoint_entries(server, 5),
        "recent_kanban": crate::status_ops::list_recent_kanban_entries(server, 5),
    }))
    .map_err(|e| format!("serialize readiness: {e}"))
}

fn parse_json_or_empty(raw: String) -> Value {
    serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "raw": raw }))
}

fn compress_health(status: &Value, wiki_lint: &Value, board: &Value) -> Value {
    let warnings = status
        .get("warnings")
        .and_then(|v| v.as_array())
        .map(|rows| rows.iter().take(6).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "health_score": status.get("health_score"),
        "warnings": warnings,
        "failed_jobs": status.pointer("/databases/failed_jobs"),
        "low_vector_coverage_count": status.pointer("/databases/low_vector_coverage").and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0),
        "provider_auth_failure_count": status.pointer("/databases/provider_auth_failures").and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0),
        "wiki": {
            "orphans": array_len(wiki_lint.get("orphans")),
            "stale_nodes": array_len(wiki_lint.get("stale_nodes")),
            "dirty_data": array_len(wiki_lint.get("dirty_data")),
            "duplicates": array_len(wiki_lint.get("duplicates")),
            "missing_edge_hints": array_len(wiki_lint.get("missing_edge_hints")),
        },
        "kanban": {
            "count": board.get("count"),
            "filter": board.get("filter"),
        }
    })
}

fn array_len(value: Option<&Value>) -> usize {
    value.and_then(|v| v.as_array()).map(|rows| rows.len()).unwrap_or(0)
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
        server
            .llm
            .call_extract_llm(system, &user, model, 0.2, 700),
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
    let line = serde_json::to_string(value).map_err(|e| format!("serialize progress event: {e}"))?;
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
        std::fs::write(&status_path, body)
            .map_err(|e| format!("write {}: {e}", status_path.display()))
    })
}

#[cfg(unix)]
fn with_progress_status_lock<T>(status_path: &Path, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    let lock_path = status_path.with_extension("json.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;
    let fd = lock_file.as_raw_fd();
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
    if rc != 0 {
        return Err(format!(
            "flock {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let result = f();
    unsafe {
        libc::flock(fd, libc::LOCK_UN);
    }
    result
}

#[cfg(not(unix))]
fn with_progress_status_lock<T>(_status_path: &Path, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    f()
}

fn claude_jsonl_passive_watcher_status() -> Value {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let candidates = vec![
        home.join(".claude").join("projects"),
        home.join(".config").join("claude").join("projects"),
    ];
    let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;
    for root in candidates {
        visit_jsonl_files(&root, &mut newest, 0);
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
) {
    if depth > 4 || !root.exists() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_jsonl_files(&path, newest, depth + 1);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let should_replace = newest
            .as_ref()
            .map(|(_, current)| modified > *current)
            .unwrap_or(true);
        if should_replace {
            *newest = Some((path, modified));
        }
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
        .or_else(|| value.pointer("/message/text").and_then(|v| v.as_str().map(str::to_string)))
        .or_else(|| value.get("text").and_then(|v| v.as_str().map(str::to_string)))
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
