use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore, SearchOptions};
use serde_json::json;
use std::collections::HashSet;

use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::IngestSourceParams;
use crate::utils::sanitize_safe_path_name;

use super::audit::RetryableIngestLease;
use super::helpers::{default_ingest_chunk_overlap, default_ingest_chunk_size, resolve_domain};
use super::ingest::handle_ingest_source;

pub(crate) async fn build_similarity_edges(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    domain: Option<&str>,
    saved_entries: &[MemoryEntry],
    lease: &RetryableIngestLease,
) -> Result<(), String> {
    for entry in saved_entries {
        let query = entry.text.chars().take(480).collect::<String>();
        if query.trim().is_empty() {
            continue;
        }

        let search_action = |store: &mut MemoryStore| {
            store
                .search(
                    &query,
                    Some(SearchOptions {
                        top_k: 4,
                        domain: domain.map(|value| value.to_string()),
                        record_access: false,
                        ..Default::default()
                    }),
                )
                .map_err(|e| format!("{e}"))
        };

        let search_results = if let Some(project_name) = project {
            server.with_named_project_store_read(project_name, search_action)
        } else {
            server.with_store_for_scope_read(target_db, search_action)
        }
        .map_err(|error| format!("search source link candidates: {error}"))?;
        let load_existing = |store: &mut MemoryStore| {
            store
                .get_edges(&entry.id, "outgoing", Some("similar_to"))
                .map(|edges| {
                    edges
                        .into_iter()
                        .map(|edge| edge.target_id)
                        .collect::<HashSet<_>>()
                })
                .map_err(|error| format!("read existing source links: {error}"))
        };
        let mut existing_targets = if let Some(project_name) = project {
            server.with_named_project_store_read(project_name, load_existing)
        } else {
            server.with_store_for_scope_read(target_db, load_existing)
        }?;

        for result in search_results {
            if result.entry.id == entry.id || existing_targets.contains(&result.entry.id) {
                continue;
            }
            let edge = memcore::MemoryEdge {
                source_id: entry.id.clone(),
                target_id: result.entry.id.clone(),
                relation: "similar_to".to_string(),
                weight: result.score.final_score.clamp(0.15, 1.0),
                metadata: json!({
                    "auto_ingest": true,
                    "score": result.score.final_score,
                    "path": entry.path,
                }),
                created_at: Utc::now().to_rfc3339(),
                valid_from: String::new(),
                valid_to: None,
            };
            lease
                .write_owned(|store| store.add_edge(&edge).map_err(|e| format!("{e}")))
                .await
                .map_err(|error| format!("persist source similarity link: {error}"))?;
            existing_targets.insert(result.entry.id);
        }
    }
    Ok(())
}

pub(crate) fn extract_text_from_tool_result(
    result: &rmcp::model::CallToolResult,
) -> Option<String> {
    let texts: Vec<String> = result
        .content
        .iter()
        .filter_map(|item| {
            serde_json::to_value(item).ok().and_then(|value| {
                value
                    .get("text")
                    .and_then(|text| text.as_str())
                    .map(String::from)
            })
        })
        .collect();

    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n\n"))
    }
}

pub(crate) async fn schedule_auto_ingest_from_mcp(
    server: &MemoryServer,
    capability_id: &str,
    tool_name: &str,
    definition: &serde_json::Value,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    result: &rmcp::model::CallToolResult,
) -> Result<Option<String>, String> {
    let enabled = definition
        .get("auto_ingest")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !enabled {
        return Ok(None);
    }

    let Some(content) = extract_text_from_tool_result(result) else {
        return Ok(None);
    };

    let resolved_server = capability_id.strip_prefix("mcp:").unwrap_or(capability_id);
    let source_url = arguments
        .and_then(|args| args.get("url"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .or_else(|| {
            arguments
                .and_then(|args| args.get("source_url"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        });

    let domain = resolve_domain(
        definition
            .get("ingest_domain")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
    );
    let path_prefix = definition
        .get("ingest_path_prefix")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .unwrap_or_else(|| {
            format!(
                "/wiki/{}/{}/{}",
                sanitize_safe_path_name(domain.as_deref().unwrap_or("general")),
                sanitize_safe_path_name(resolved_server),
                sanitize_safe_path_name(tool_name),
            )
        });
    let scope = definition
        .get("ingest_scope")
        .and_then(|value| value.as_str())
        .unwrap_or("global")
        .to_string();
    let source = format!("{}:{}", resolved_server, tool_name);
    let metadata = json!({
        "capability_id": capability_id,
        "tool_name": tool_name,
        "arguments": arguments.cloned().unwrap_or_default(),
        "auto_ingest": true,
    });

    let params = IngestSourceParams {
        content,
        source_url,
        source: Some(source),
        path_prefix: Some(path_prefix),
        auto_chunk: true,
        auto_summarize: true,
        auto_link: true,
        importance: 0.7,
        scope,
        project: None,
        domain,
        chunk_size_chars: default_ingest_chunk_size(),
        chunk_overlap_chars: default_ingest_chunk_overlap(),
        metadata: Some(metadata),
    };

    handle_ingest_source(server, params).await.map(Some)
}
