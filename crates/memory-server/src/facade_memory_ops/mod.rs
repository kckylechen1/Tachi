//! Business logic for the `tachi_memory` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_memory`].

mod briefing_ops;
mod checkpoint_ops;
mod evidence_format;
mod progress_ops;
mod readiness_ops;

use crate::facade_save_ops::handle_tachi_save;
use crate::tool_params::*;
use crate::MemoryServer;
use evidence_format::{
    format_extract_result, format_save_result, json_string, parse_json_or_empty, wants_json,
};
use serde_json::json;

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
                include_training: params.include_training,
                enable_rerank: params.enable_rerank,
                as_of: params.as_of.clone(),
            };
            if wants_json(params.format.as_deref()) {
                let (sections, scope_remapped, scope) =
                    crate::facade_search_ops::collect_tachi_search_sections(server, &search_params)
                        .await;
                let sections = sections
                    .into_iter()
                    .map(|(name, rows)| json!({ "name": name, "rows": rows }))
                    .collect::<Vec<_>>();
                return json_string(&json!({
                    "status": "completed",
                    "query": search_params.query,
                    "scope": scope,
                    "scope_remapped": scope_remapped,
                    "sections": sections,
                }));
            }
            crate::facade_search_ops::handle_tachi_search(server, search_params).await
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
                references: Vec::new(),
                topic: params.topic.clone(),
                source: params.source.clone(),
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                metadata: params.metadata.clone(),
                files: params.files.clone(),
            };
            let body = handle_tachi_save(server, save_params).await?;
            if wants_json(params.format.as_deref()) {
                return json_string(&parse_json_or_empty(body));
            }
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
                references: Vec::new(),
                topic: params.topic.clone(),
                source: params.source.clone(),
                valid_from: params.valid_from.clone(),
                valid_until: params.valid_until.clone(),
                metadata: params.metadata.clone(),
                files: params.files.clone(),
            };
            let body = handle_tachi_save(server, save_params).await?;
            if wants_json(params.format.as_deref()) {
                return json_string(&parse_json_or_empty(body));
            }
            Ok(format_extract_result(&body))
        }
        "briefing" => briefing_ops::handle_memory_briefing(server, &params).await,
        "checkpoint" => checkpoint_ops::handle_memory_checkpoint(server, params).await,
        "alerts" => readiness_ops::handle_memory_alerts(server, &params).await,
        "ask" => readiness_ops::handle_memory_ask(server, &params).await,
        "consolidate" => readiness_ops::handle_memory_consolidate(server, &params).await,
        "progress" => progress_ops::handle_memory_progress(server, &params).await,
        "readiness" => readiness_ops::handle_memory_readiness(server, &params).await,
        _ => Err(format!(
            "Invalid action '{}'. Use 'search', 'save', 'extract_facts', 'briefing', 'checkpoint', 'alerts', 'ask', 'consolidate', 'progress', or 'readiness'.",
            params.action
        )),
    }
}

// Re-export pub(crate) items that external modules reference.
pub(crate) use checkpoint_ops::{
    capture_latest_claude_jsonl_checkpoint, claude_jsonl_passive_watcher_status,
};
