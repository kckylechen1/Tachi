use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore, SearchOptions};
use serde_json::json;
use std::collections::HashSet;

use crate::server_state::{DbScope, MemoryServer};
use crate::tool_params::IngestSourceParams;
use crate::utils::{sanitize_safe_path_name, stable_hash};

use super::audit::{
    claim_retryable_ingest_event, ingest_audit_key, ingest_success_audit_exists,
    RetryableIngestLease,
};
use super::helpers::{default_ingest_chunk_overlap, default_ingest_chunk_size, resolve_domain};
use super::ingest::handle_ingest_source;

const AUTO_INGEST_JOB_WORKER: &str = "auto_ingest_job";
const AUTO_INGEST_CLAIM_WORKER: &str = "auto_ingest_job_claim";
const AUTO_INGEST_DEAD_LETTER_WORKER: &str = "auto_ingest_job_dead_letter";
const AUTO_INGEST_JOB_LABEL: &str = "auto_ingest_job";
const AUTO_INGEST_REPLAY_BATCH_SIZE: usize = 16;
const AUTO_INGEST_REPLAY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
const AUTO_INGEST_FORENSIC_PAYLOAD_MAX_BYTES: usize = 4096;
const AUTO_INGEST_FORENSIC_ERROR_MAX_BYTES: usize = 512;

pub(crate) struct StagedAutoIngest {
    job_hash: String,
}

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
            // tachi#1646: `similar_to` links are a search-score heuristic
            // Tachi computed itself.
            lease
                .write_owned(|store| {
                    store
                        .add_edge_with_provenance_within_tx(
                            &edge,
                            &memcore::db::EdgeProvenance {
                                authority: Some(memcore::db::EdgeAuthority::DerivedHeuristic),
                                ..Default::default()
                            },
                        )
                        .map_err(|e| format!("{e}"))
                })
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

fn prepare_auto_ingest_from_mcp(
    capability_id: &str,
    tool_name: &str,
    definition: &serde_json::Value,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    result: &rmcp::model::CallToolResult,
) -> Option<IngestSourceParams> {
    let enabled = definition
        .get("auto_ingest")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !enabled {
        return None;
    }

    let content = extract_text_from_tool_result(result)?;

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

    Some(IngestSourceParams {
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
    })
}

pub(crate) fn stage_auto_ingest_from_mcp(
    server: &MemoryServer,
    capability_id: &str,
    tool_name: &str,
    definition: &serde_json::Value,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    result: &rmcp::model::CallToolResult,
) -> Result<Option<StagedAutoIngest>, String> {
    let Some(params) =
        prepare_auto_ingest_from_mcp(capability_id, tool_name, definition, arguments, result)
    else {
        return Ok(None);
    };
    let payload = json!({
        "content": &params.content,
        "source_url": &params.source_url,
        "source": &params.source,
        "path_prefix": &params.path_prefix,
        "auto_chunk": params.auto_chunk,
        "auto_summarize": params.auto_summarize,
        "auto_link": params.auto_link,
        "importance": params.importance,
        "scope": &params.scope,
        "project": &params.project,
        "domain": &params.domain,
        "chunk_size_chars": params.chunk_size_chars,
        "chunk_overlap_chars": params.chunk_overlap_chars,
        "metadata": &params.metadata,
    });
    let payload_json = serde_json::to_string(&payload)
        .map_err(|error| format!("serialize durable auto-ingest job: {error}"))?;
    let job_hash = stable_hash(&format!("auto-ingest-job:{payload_json}"));
    let audit_key = ingest_audit_key(AUTO_INGEST_JOB_LABEL, DbScope::Global, None, &job_hash);
    let staged = server.with_global_store(|store| {
        store
            .connection()
            .execute(
                "INSERT INTO processed_events (event_hash, event_id, worker, created_at) \
                 SELECT ?1, ?2, ?3, STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM audit_log \
                     WHERE server_id = 'ingest' AND tool_name = ?4 \
                       AND args_hash IN (?5, ?1) AND success = 1 \
                 ) AND NOT EXISTS ( \
                     SELECT 1 FROM processed_events \
                     WHERE event_hash = ?1 AND worker = ?6 \
                 ) \
                 ON CONFLICT(event_hash, worker) DO UPDATE SET event_id = excluded.event_id",
                rusqlite::params![
                    job_hash,
                    payload_json,
                    AUTO_INGEST_JOB_WORKER,
                    AUTO_INGEST_JOB_LABEL,
                    audit_key,
                    AUTO_INGEST_DEAD_LETTER_WORKER,
                ],
            )
            .map_err(|error| format!("stage durable auto-ingest job: {error}"))
    })?;
    if staged == 0 {
        return Ok(None);
    }
    Ok(Some(StagedAutoIngest { job_hash }))
}

pub(crate) async fn run_staged_auto_ingest(
    server: &MemoryServer,
    staged: StagedAutoIngest,
) -> Result<Option<String>, String> {
    let audit_key = ingest_audit_key(
        AUTO_INGEST_JOB_LABEL,
        DbScope::Global,
        None,
        &staged.job_hash,
    );
    let Some(claim) = claim_retryable_ingest_event(
        server,
        DbScope::Global,
        None,
        AUTO_INGEST_JOB_LABEL,
        &audit_key,
        AUTO_INGEST_CLAIM_WORKER,
        &staged.job_hash,
        &staged.job_hash,
    )?
    else {
        return Ok(None);
    };
    let lease = RetryableIngestLease::start(
        server,
        DbScope::Global,
        None,
        AUTO_INGEST_CLAIM_WORKER,
        &staged.job_hash,
        claim,
    );

    let persisted_payload = match server.with_global_store_read(|store| {
        store
            .connection()
            .query_row(
                "SELECT event_id FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                rusqlite::params![staged.job_hash, AUTO_INGEST_JOB_WORKER],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| format!("load durable auto-ingest job: {error}"))
    }) {
        Ok(payload) => payload,
        Err(error) => {
            return Err(lease
                .fail(
                    AUTO_INGEST_JOB_LABEL,
                    &audit_key,
                    "auto_ingest_replay_failed",
                    error,
                )
                .await);
        }
    };
    let params: IngestSourceParams = match serde_json::from_str(&persisted_payload) {
        Ok(params) => params,
        Err(decode_error) => {
            let decode_error = format!("decode durable auto-ingest job: {decode_error}");
            let quarantine = quarantine_malformed_auto_ingest(
                &lease,
                &staged.job_hash,
                &persisted_payload,
                &decode_error,
            )
            .await;
            return match quarantine {
                Ok(()) => {
                    lease.finish().await?;
                    Ok(None)
                }
                Err(error) => Err(lease
                    .fail(
                        AUTO_INGEST_JOB_LABEL,
                        &audit_key,
                        "auto_ingest_quarantine_failed",
                        error,
                    )
                    .await),
            };
        }
    };

    let outcome: Result<String, String> = async {
        let content = params.content.trim();
        let source_label = params
            .source
            .as_deref()
            .or(params.source_url.as_deref())
            .unwrap_or("ingest_source");
        let path_prefix = params.path_prefix.as_deref().unwrap_or("/");
        let source_event_hash = stable_hash(&format!("{source_label}:{path_prefix}:{content}"));
        let (target_db, _) = server.resolve_write_scope(&params.scope);
        let source_audit_key = ingest_audit_key(
            "ingest_source",
            target_db,
            params.project.as_deref(),
            &source_event_hash,
        );
        let response = handle_ingest_source(server, params).await?;
        if !ingest_success_audit_exists(
            server,
            "ingest_source",
            &source_audit_key,
            &source_event_hash,
        )? {
            return Err("auto-ingest source returned without a durable success audit".to_string());
        }
        lease
            .write_owned(|store| {
                let rows_changed = store
                    .connection()
                    .execute(
                        "DELETE FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                        rusqlite::params![staged.job_hash, AUTO_INGEST_JOB_WORKER],
                    )
                    .map_err(|error| format!("complete durable auto-ingest job: {error}"))?;
                if rows_changed == 1 {
                    store
                        .audit_log_insert(
                            &Utc::now().to_rfc3339(),
                            "ingest",
                            AUTO_INGEST_JOB_LABEL,
                            &audit_key,
                            true,
                            0,
                            None,
                        )
                        .map_err(|error| format!("record durable auto-ingest completion: {error}"))
                } else {
                    Err("durable auto-ingest payload disappeared before completion".to_string())
                }
            })
            .await?;
        Ok(response)
    }
    .await;

    match outcome {
        Ok(response) => {
            lease.finish().await?;
            Ok(Some(response))
        }
        Err(error) => Err(lease
            .fail(
                AUTO_INGEST_JOB_LABEL,
                &audit_key,
                "auto_ingest_replay_failed",
                error,
            )
            .await),
    }
}

async fn quarantine_malformed_auto_ingest(
    lease: &RetryableIngestLease,
    job_hash: &str,
    payload: &str,
    decode_error: &str,
) -> Result<(), String> {
    let (payload, payload_truncated) =
        bounded_utf8(payload, AUTO_INGEST_FORENSIC_PAYLOAD_MAX_BYTES);
    let (decode_error, decode_error_truncated) =
        bounded_utf8(decode_error, AUTO_INGEST_FORENSIC_ERROR_MAX_BYTES);
    let forensic = serde_json::to_string(&json!({
        "classification": "auto_ingest_malformed_payload",
        "original_payload": payload,
        "payload_truncated": payload_truncated,
        "decode_error": decode_error,
        "decode_error_truncated": decode_error_truncated,
    }))
    .map_err(|error| format!("serialize auto-ingest quarantine forensics: {error}"))?;
    lease
        .write_owned(|store| {
            let quarantined = store
                .connection()
                .execute(
                    "INSERT INTO processed_events (event_hash, event_id, worker, created_at) \
                     SELECT event_hash, ?1, ?2, STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     FROM processed_events WHERE event_hash = ?3 AND worker = ?4 \
                     ON CONFLICT(event_hash, worker) DO UPDATE SET \
                         event_id = excluded.event_id, created_at = excluded.created_at",
                    rusqlite::params![
                        forensic,
                        AUTO_INGEST_DEAD_LETTER_WORKER,
                        job_hash,
                        AUTO_INGEST_JOB_WORKER,
                    ],
                )
                .map_err(|error| format!("quarantine malformed auto-ingest payload: {error}"))?;
            if quarantined != 1 {
                return Err(
                    "malformed auto-ingest payload disappeared before quarantine".to_string(),
                );
            }
            let removed = store
                .connection()
                .execute(
                    "DELETE FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                    rusqlite::params![job_hash, AUTO_INGEST_JOB_WORKER],
                )
                .map_err(|error| format!("retire malformed auto-ingest payload: {error}"))?;
            if removed == 1 {
                Ok(())
            } else {
                Err("malformed auto-ingest payload was not retired from pending work".to_string())
            }
        })
        .await
}

fn bounded_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn pending_auto_ingest_jobs(server: &MemoryServer) -> Result<Vec<StagedAutoIngest>, String> {
    server.with_global_store_read(|store| {
        let mut statement = store
            .connection()
            .prepare(
                "SELECT event_hash FROM processed_events \
                 WHERE worker = ?1 ORDER BY created_at, event_hash LIMIT ?2",
            )
            .map_err(|error| format!("prepare pending auto-ingest replay query: {error}"))?;
        let rows = statement
            .query_map(
                rusqlite::params![AUTO_INGEST_JOB_WORKER, AUTO_INGEST_REPLAY_BATCH_SIZE as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| format!("enumerate pending auto-ingest jobs: {error}"))?;
        rows.map(|row| {
            row.map(|job_hash| StagedAutoIngest { job_hash })
                .map_err(|error| format!("read pending auto-ingest job: {error}"))
        })
        .collect()
    })
}

pub(crate) async fn replay_pending_auto_ingest_once(
    server: &MemoryServer,
) -> Result<usize, String> {
    let jobs = pending_auto_ingest_jobs(server)?;
    let mut completed = 0;
    let mut failures = Vec::new();
    for job in jobs {
        let job_hash = job.job_hash.clone();
        match run_staged_auto_ingest(server, job).await {
            Ok(Some(_)) => completed += 1,
            Ok(None) => {}
            Err(error) => failures.push(format!("{job_hash}: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(completed)
    } else {
        Err(format!(
            "auto-ingest replay failed for {} job(s): {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

pub(crate) async fn run_auto_ingest_replay_consumer(server: MemoryServer) {
    let mut interval = tokio::time::interval(AUTO_INGEST_REPLAY_INTERVAL);
    loop {
        interval.tick().await;
        if let Err(error) = replay_pending_auto_ingest_once(&server).await {
            tracing::error!(error = %error, "durable auto-ingest replay cycle failed");
        }
    }
}
