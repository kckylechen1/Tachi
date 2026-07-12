//! Business logic for the `tachi_memory` facade tool.
//!
//! Extracted from `tools.rs` (Stage 4 of large-rust-files refactor) to keep
//! the `#[tool]` wrapper thin. The wrapper in `impl MemoryServer` simply
//! delegates to [`handle_tachi_memory`].

mod briefing_ops;
mod checkpoint_ops;
mod consolidate_ops;
mod evidence_format;
mod pattern_feedback_ops;
mod progress_ops;
mod readiness_ops;
mod recall_proposal_ops;
mod recall_simulate_ops;

use crate::facade_save_ops::finalize_tachi_save_response;
use crate::facade_save_ops::handle_tachi_save;
use crate::tool_params::*;
use crate::MemoryServer;
use evidence_format::{format_extract_result, json_string, parse_json_or_empty};
pub(crate) use evidence_format::{
    shape_complete_response, shape_save_facade_response, wants_full_format, wants_json,
};
pub(crate) use recall_simulate_ops::build_recall_simulation_report;
use serde_json::json;

pub(crate) async fn handle_tachi_memory(
    server: &MemoryServer,
    params: TachiMemoryParams,
) -> Result<String, String> {
    let action = params.action.to_ascii_lowercase();
    if should_forward_facade_read(&action) {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(server, "tachi_memory", &params).await?
        {
            return Ok(body);
        }
    }

    match action.as_str() {
        "search" => {
            let query = params
                .query
                .clone()
                .ok_or_else(|| "query is required when action='search'".to_string())?;
            let top_k = crate::clamp_facade_top_k(params.top_k);
            let search_params = TachiSearchParams {
                query,
                scope: params.scope.clone().unwrap_or_else(|| "all".to_string()),
                top_k,
                path_prefix: params.path_prefix.clone(),
                project: params.project.clone(),
                domain: params.domain.clone(),
                file_context: params.file_context.clone(),
                error_context: params.error_context.clone(),
                context_symbols: Vec::new(),
                agent_role: params.agent_role.clone(),
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
                let binding = crate::memory_search_ops::library_binding_receipt(
                    server,
                    search_params.project.as_deref(),
                );
                return json_string(&json!({
                    "status": "completed",
                    "query": search_params.query,
                    "scope": scope,
                    "scope_remapped": scope_remapped,
                    "sections": sections,
                    "binding": binding,
                }));
            }
            crate::facade_search_ops::handle_tachi_search(server, search_params).await
        }
        "get" => {
            let id = params
                .id
                .clone()
                .ok_or_else(|| "id is required when action='get'".to_string())?;
            let body = crate::memory_ops::handle_get_memory(
                server,
                GetMemoryParams {
                    id,
                    project: params.project.clone(),
                    include_archived: params.include_archived,
                },
            )
            .await?;
            if wants_json(params.format.as_deref()) {
                return json_string(&parse_json_or_empty(body));
            }
            Ok(body)
        }
        "save" => {
            if let Some(body) =
                crate::cli_client::maybe_forward_server_write(server, "tachi_memory", &params)
                    .await?
            {
                return Ok(body);
            }

            let text = params
                .text
                .clone()
                .ok_or_else(|| "text is required when action='save'".to_string())?;
            let scope_is_note = params
                .scope
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case("note"))
                .unwrap_or(false);
            let kind = params
                .kind
                .clone()
                .or_else(|| (!scope_is_note).then(|| "memory".to_string()));
            let mut path = params.path.clone();
            let mut category = params.category.clone();
            let mut keywords = params.keywords.clone();
            let mut metadata = params.metadata.clone();
            crate::feedback_rule_ops::normalize_feedback_rule_save(
                &kind,
                &mut path,
                &mut category,
                &mut keywords,
                &mut metadata,
            );
            let save_params = TachiSaveParams {
                text: text.clone(),
                id: params.id.clone(),
                kind,
                title: params.title.clone(),
                summary: params.summary.clone(),
                path,
                importance: params.importance,
                category,
                keywords,
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
                metadata,
                emit_continuity: params.emit_continuity,
                files: params.files.clone(),
                format: params.format.clone(),
            };
            let raw = handle_tachi_save(server, save_params.clone()).await?;
            finalize_tachi_save_response(&save_params, &raw, Some(text.as_str()))
        }
        "extract_facts" => {
            if let Some(body) =
                crate::cli_client::maybe_forward_server_write(server, "tachi_memory", &params)
                    .await?
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
                emit_continuity: false,
                files: params.files.clone(),
                format: None,
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
        "consolidate" => consolidate_ops::handle_memory_consolidate(server, &params).await,
        "recall_simulate" => {
            recall_simulate_ops::handle_memory_recall_simulate(server, &params).await
        }
        "recall_proposals" => {
            recall_proposal_ops::handle_recall_config_proposals(server, &params).await
        }
        "review_recall_proposal" => {
            recall_proposal_ops::handle_recall_config_review(server, &params)
        }
        "apply_recall_proposals" => recall_proposal_ops::handle_recall_config_apply(server, &params),
        "pattern_feedback" => {
            if let Some(body) =
                crate::cli_client::maybe_forward_server_write(server, "tachi_memory", &params)
                    .await?
            {
                return Ok(body);
            }
            pattern_feedback_ops::handle_pattern_feedback(server, &params)
        }
        "progress" => progress_ops::handle_memory_progress(server, &params).await,
        "readiness" => readiness_ops::handle_memory_readiness(server, &params).await,
        // #1001: manual claim/release backstop for harness-native work that
        // never routes through the briefing/intake/dispatch auto-hooks. The
        // zero-ceremony auto-register/heartbeat path lives in
        // `claims_ops::auto_register_or_heartbeat_claim`, called directly from
        // those call sites; these two actions are the fallback entrypoint.
        "claim" => {
            let result = crate::claims_ops::handle_manual_claim(
                server,
                params.issue_ref.clone(),
                params.flow_id.clone(),
                params.branch.clone(),
                if params.declared_file_scope.is_empty() {
                    None
                } else {
                    Some(params.declared_file_scope.clone())
                },
            )?;
            json_string(&result)
        }
        "release" => {
            let result = crate::claims_ops::handle_manual_release(
                server,
                params.claim_id.clone(),
                params.dispatch_id.clone(),
                params.release_reason.clone(),
            )?;
            json_string(&result)
        }
        // #757 fold: standalone memory-admin + pipeline tools re-fronted as
        // tachi_memory actions. Each arm delegates to the SAME handler the old
        // #[tool] entry point used — re-fronting, not re-implementation.
        "delete" => {
            if let Some(body) =
                crate::cli_client::maybe_forward_server_write(server, "tachi_memory", &params)
                    .await?
            {
                return Ok(body);
            }
            let id = params
                .id
                .clone()
                .ok_or_else(|| "id is required when action='delete'".to_string())?;
            crate::memory_ops::handle_delete_memory(
                server,
                crate::tool_params::DeleteMemoryParams {
                    id,
                    project: params.project.clone(),
                },
            )
            .await
        }
        "gc" => crate::memory_ops::handle_memory_gc(server).await,
        "doctor_scan" => crate::doctor_ops::handle_tachi_doctor_scan().await,
        "ingest" => {
            crate::pipeline_ops::handle_ingest(
                server,
                crate::tool_params::IngestParams {
                    ingest_type: params.ingest_type.clone(),
                    content: params.content.clone(),
                    source_url: params.source_url.clone(),
                    source: params.source.clone(),
                    path_prefix: params.path_prefix.clone(),
                    auto_chunk: params.auto_chunk,
                    auto_summarize: params.auto_summarize,
                    auto_link: params.auto_link,
                    importance: params.importance.unwrap_or(0.7),
                    scope: params
                        .scope
                        .clone()
                        .unwrap_or_else(|| "project".to_string()),
                    project: params.project.clone(),
                    domain: params.domain.clone(),
                    chunk_size_chars: params.chunk_size_chars,
                    chunk_overlap_chars: params.chunk_overlap_chars,
                    conversation_id: params.conversation_id.clone(),
                    turn_id: params.turn_id.clone(),
                    event_type: params.event_type.clone(),
                    messages: params.messages.clone(),
                    metadata: params.metadata.clone(),
                },
            )
            .await
        }
        "ingest_source" => {
            // #757-fold fix (gpt-5.6-terra review): the standalone
            // `ingest_source` tool required `content: String` — a missing or
            // non-string `content` failed deserialization before the handler
            // ever ran (tachi-params/src/memory/ingest.rs `IngestSourceParams`).
            // The fold silently widened that to "any JSON value, coerced to
            // text, defaulting to empty on omission" — restore the original
            // boundary: reject omission and non-string content with a clear
            // error instead of coercing. (Handler-level whitespace/empty
            // *string* content is still a legitimate, deterministic no-op
            // "skipped" response — that behavior belongs to
            // `handle_ingest_source` itself, see
            // `ingest_source_empty_content_records_skip_audit`, and is
            // unaffected by this type-level check.)
            let content = match params.content.clone() {
                Some(serde_json::Value::String(text)) => text,
                Some(_) => {
                    return Err(
                        "content must be a string when action='ingest_source' (matches the \
                         standalone ingest_source tool's required content: String contract)"
                            .to_string(),
                    )
                }
                None => {
                    return Err(
                        "content is required when action='ingest_source' (matches the \
                         standalone ingest_source tool's required content: String contract)"
                            .to_string(),
                    )
                }
            };
            crate::pipeline_ops::handle_ingest_source(
                server,
                crate::tool_params::IngestSourceParams {
                    content,
                    source_url: params.source_url.clone(),
                    source: params.source.clone(),
                    path_prefix: params.path_prefix.clone(),
                    auto_chunk: params.auto_chunk,
                    auto_summarize: params.auto_summarize,
                    auto_link: params.auto_link,
                    importance: params.importance.unwrap_or(0.7),
                    scope: params
                        .scope
                        .clone()
                        .unwrap_or_else(|| "project".to_string()),
                    project: params.project.clone(),
                    domain: params.domain.clone(),
                    chunk_size_chars: params.chunk_size_chars,
                    chunk_overlap_chars: params.chunk_overlap_chars,
                    metadata: params.metadata.clone(),
                },
            )
            .await
        }
        "sticky_leave" => {
            let text = params
                .text
                .clone()
                .ok_or_else(|| "text is required when action='sticky_leave'".to_string())?;
            crate::sticky_ops::handle_sticky_leave(
                server,
                crate::sticky_ops::StickyLeaveInput {
                    text,
                    to: params.to.clone(),
                    ttl_days: params.ttl_days,
                },
            )
            .await
        }
        "sticky_check" => {
            crate::sticky_ops::handle_sticky_check(
                server,
                crate::sticky_ops::StickyCheckInput {
                    agent_id: params.agent_id.clone(),
                    include_read: params.include_read,
                    limit: Some(crate::clamp_facade_top_k(params.top_k)),
                },
            )
            .await
        }
        _ => Err(format!(
            "Invalid action '{}'. Use 'search', 'get', 'save', 'extract_facts', 'briefing', 'checkpoint', 'alerts', 'ask', 'consolidate', 'recall_simulate', 'recall_proposals', 'review_recall_proposal', 'apply_recall_proposals', 'pattern_feedback', 'progress', 'readiness', 'claim', 'release', 'delete', 'gc', 'doctor_scan', 'ingest', 'ingest_source', 'sticky_leave', or 'sticky_check'.",
            params.action
        )),
    }
}

fn should_forward_facade_read(action: &str) -> bool {
    matches!(
        action,
        "search" | "get" | "briefing" | "alerts" | "ask" | "recall_simulate" | "readiness"
    )
}

// Re-export pub(crate) items that external modules reference.
pub(crate) use checkpoint_ops::{
    capture_latest_claude_jsonl_checkpoint, claude_jsonl_passive_watcher_status,
};

#[cfg(test)]
mod tests {
    use super::should_forward_facade_read;

    #[test]
    fn facade_read_actions_include_briefing_forwarding() {
        for action in [
            "search",
            "get",
            "briefing",
            "alerts",
            "ask",
            "recall_simulate",
            "readiness",
        ] {
            assert!(
                should_forward_facade_read(action),
                "{action} should use daemon read forwarding"
            );
        }
        // consolidate propose is dry_run but review/apply mutate; do not
        // treat the whole action as a pure read-forward.
        assert!(!should_forward_facade_read("consolidate"));
    }

    #[test]
    fn facade_write_actions_do_not_use_read_forwarding() {
        for action in [
            "save",
            "extract_facts",
            "checkpoint",
            "pattern_feedback",
            "progress",
            "consolidate",
        ] {
            assert!(
                !should_forward_facade_read(action),
                "{action} should keep its write/state-specific forwarding path"
            );
        }
    }
}
