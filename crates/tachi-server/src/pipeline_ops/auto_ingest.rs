use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore, SearchOptions};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
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

const AUTO_INGEST_JOB_WORKER: &str = "auto_ingest_job";
const AUTO_INGEST_CLAIM_WORKER: &str = "auto_ingest_job_claim";
const AUTO_INGEST_DEAD_LETTER_WORKER: &str = "auto_ingest_job_dead_letter";
const AUTO_INGEST_RECEIPT_WORKER: &str = "auto_ingest_job_receipt";
const AUTO_INGEST_JOB_LABEL: &str = "auto_ingest_job";
const AUTO_INGEST_REPLAY_BATCH_SIZE: usize = 16;
const AUTO_INGEST_REPLAY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
const AUTO_INGEST_FORENSIC_PAYLOAD_MAX_BYTES: usize = 4096;
const AUTO_INGEST_FORENSIC_ERROR_MAX_BYTES: usize = 512;

/// Named limits for the already-acquired MCP text admission boundary. This
/// path owns no fetch, filesystem, or command authority.
const ADMITTED_INGEST_RAW_PAYLOAD_MAX_BYTES: usize = 2 * 1024 * 1024;
const ADMITTED_INGEST_CHUNK_SIZE_MAX_CHARS: usize = 8192;
const ADMITTED_INGEST_CHUNK_COUNT_MAX: usize = 2048;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StagedMcpSourceV1 {
    capability_id: String,
    tool_name: String,
    label: Option<String>,
    url: Option<String>,
    arguments: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StagedIngestTargetV1 {
    scope: String,
    project: Option<String>,
    domain: Option<String>,
    path_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct StagedIngestPolicyV1 {
    auto_chunk: bool,
    auto_summarize: bool,
    auto_link: bool,
    importance: f64,
    chunk_size_chars: usize,
    chunk_overlap_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedMcpIngestJobV1 {
    schema: String,
    job_id: String,
    source: StagedMcpSourceV1,
    content_digest: String,
    target: StagedIngestTargetV1,
    target_digest: String,
    policy: StagedIngestPolicyV1,
    policy_digest: String,
    idempotency_key: String,
    request: IngestSourceParams,
}

impl StagedMcpIngestJobV1 {
    fn from_request(
        capability_id: &str,
        tool_name: &str,
        request: IngestSourceParams,
    ) -> Result<Self, String> {
        let source = staged_source_from_request(capability_id, tool_name, &request);
        let target = staged_target_from_request(&request);
        let policy = staged_policy_from_request(&request);
        let source_digest = stable_hash(
            &serde_json::to_string(&source)
                .map_err(|error| format!("serialize admitted MCP source identity: {error}"))?,
        );
        let target_digest = stable_hash(
            &serde_json::to_string(&target)
                .map_err(|error| format!("serialize admitted ingest target: {error}"))?,
        );
        let policy_digest = stable_hash(
            &serde_json::to_string(&policy)
                .map_err(|error| format!("serialize admitted ingest policy: {error}"))?,
        );
        let content_digest = stable_hash(&request.content);
        let idempotency_key = stable_hash(&format!(
            "mcp-admitted-ingest:{source_digest}:{content_digest}:{target_digest}:{policy_digest}"
        ));
        let job_id = stable_hash(&format!("mcp-admitted-ingest-job:{idempotency_key}"));
        Ok(Self {
            schema: "tachi.admitted_mcp_ingest.v1".to_string(),
            job_id,
            source,
            content_digest,
            target,
            target_digest,
            policy,
            policy_digest,
            idempotency_key,
            request,
        })
    }
}

/// A sealed mutation request. Only this module can construct it, and only
/// after re-reading and validating the durable staged MCP envelope.
pub(in crate::pipeline_ops) struct AdmittedIngestRequest {
    job_id: String,
    idempotency_key: String,
    request: IngestSourceParams,
}

impl AdmittedIngestRequest {
    pub(in crate::pipeline_ops) fn into_parts(self) -> (IngestSourceParams, String, String) {
        (self.request, self.job_id, self.idempotency_key)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StagedAutoIngest {
    pub(crate) job_id: String,
}

pub(crate) async fn build_similarity_edges(
    server: &MemoryServer,
    target_db: DbScope,
    project: Option<&str>,
    domain: Option<&str>,
    saved_entries: &[MemoryEntry],
    lease: &RetryableIngestLease,
    edges_written: &mut usize,
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
            *edges_written += 1;
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
) -> Result<Option<StagedMcpIngestJobV1>, String> {
    if result.is_error.unwrap_or(false) {
        return Ok(None);
    }
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
    if content.len() > ADMITTED_INGEST_RAW_PAYLOAD_MAX_BYTES {
        return Err(format!(
            "admitted ingest payload exceeds {} byte limit",
            ADMITTED_INGEST_RAW_PAYLOAD_MAX_BYTES
        ));
    }

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

    let request = IngestSourceParams {
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
    validate_admitted_ingest_bounds(&request)?;
    StagedMcpIngestJobV1::from_request(capability_id, tool_name, request).map(Some)
}

fn validate_admitted_ingest_bounds(request: &IngestSourceParams) -> Result<(), String> {
    if request.content.len() > ADMITTED_INGEST_RAW_PAYLOAD_MAX_BYTES {
        return Err(format!(
            "admitted ingest payload exceeds {} byte limit",
            ADMITTED_INGEST_RAW_PAYLOAD_MAX_BYTES
        ));
    }
    if request.chunk_size_chars == 0
        || request.chunk_size_chars > ADMITTED_INGEST_CHUNK_SIZE_MAX_CHARS
    {
        return Err(format!(
            "admitted ingest chunk_size_chars must be in 1..={}",
            ADMITTED_INGEST_CHUNK_SIZE_MAX_CHARS
        ));
    }
    if request.chunk_overlap_chars >= request.chunk_size_chars {
        return Err(
            "admitted ingest chunk_overlap_chars must be smaller than chunk_size_chars".to_string(),
        );
    }
    let chunks = if request.auto_chunk {
        admitted_chunk_count(
            request.content.trim(),
            request.chunk_size_chars,
            request.chunk_overlap_chars,
        )
    } else if request.content.trim().is_empty() {
        0
    } else {
        1
    };
    if chunks > ADMITTED_INGEST_CHUNK_COUNT_MAX {
        return Err(format!(
            "admitted ingest expands to {chunks} chunks, above {} chunk limit",
            ADMITTED_INGEST_CHUNK_COUNT_MAX
        ));
    }
    Ok(())
}

fn admitted_chunk_count(content: &str, chunk_size: usize, overlap: usize) -> usize {
    let chars = content.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return 0;
    }
    let step = chunk_size - overlap;
    let mut start = 0usize;
    let mut count = 0usize;
    while start < chars.len() {
        let end = (start + chunk_size).min(chars.len());
        if chars[start..end]
            .iter()
            .any(|character| !character.is_whitespace())
        {
            count += 1;
            if count > ADMITTED_INGEST_CHUNK_COUNT_MAX {
                return count;
            }
        }
        if end == chars.len() {
            break;
        }
        start += step;
    }
    count
}

#[cfg(test)]
pub(crate) fn validate_admitted_ingest_bounds_for_test(
    content: String,
    chunk_size_chars: usize,
    chunk_overlap_chars: usize,
) -> Result<(), String> {
    validate_admitted_ingest_bounds(&IngestSourceParams {
        content,
        source_url: None,
        source: Some("bounds-fixture".to_string()),
        path_prefix: Some("/bounds-fixture".to_string()),
        auto_chunk: true,
        auto_summarize: false,
        auto_link: false,
        importance: 0.7,
        scope: "global".to_string(),
        project: None,
        domain: Some("general".to_string()),
        chunk_size_chars,
        chunk_overlap_chars,
        metadata: None,
    })
}

fn validate_staged_job(job: &StagedMcpIngestJobV1, expected_job_id: &str) -> Result<(), String> {
    if job.schema != "tachi.admitted_mcp_ingest.v1" {
        return Err("admitted ingest staged-job schema drift".to_string());
    }
    validate_admitted_ingest_bounds(&job.request)
        .map_err(|error| format!("admitted ingest staged-job policy drift: {error}"))?;
    let rebuilt = prepare_job_digests(job)?;
    if job.job_id != expected_job_id
        || rebuilt.job_id != job.job_id
        || rebuilt.source != job.source
        || rebuilt.content_digest != job.content_digest
        || rebuilt.target != job.target
        || rebuilt.target_digest != job.target_digest
        || rebuilt.policy != job.policy
        || rebuilt.policy_digest != job.policy_digest
        || rebuilt.idempotency_key != job.idempotency_key
        || job.request.metadata.as_ref() != Some(&staged_metadata(&job.source))
    {
        return Err("admitted ingest staged-job source/content/target/policy drift".to_string());
    }
    Ok(())
}

fn prepare_job_digests(job: &StagedMcpIngestJobV1) -> Result<StagedMcpIngestJobV1, String> {
    StagedMcpIngestJobV1::from_request(
        &job.source.capability_id,
        &job.source.tool_name,
        job.request.clone(),
    )
}

fn staged_source_from_request(
    capability_id: &str,
    tool_name: &str,
    request: &IngestSourceParams,
) -> StagedMcpSourceV1 {
    StagedMcpSourceV1 {
        capability_id: capability_id.to_string(),
        tool_name: tool_name.to_string(),
        label: request.source.clone(),
        url: request.source_url.clone(),
        arguments: request
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("arguments"))
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default(),
    }
}

fn staged_target_from_request(request: &IngestSourceParams) -> StagedIngestTargetV1 {
    StagedIngestTargetV1 {
        scope: request.scope.clone(),
        project: request.project.clone(),
        domain: request.domain.clone(),
        path_prefix: request.path_prefix.clone(),
    }
}

fn staged_policy_from_request(request: &IngestSourceParams) -> StagedIngestPolicyV1 {
    StagedIngestPolicyV1 {
        auto_chunk: request.auto_chunk,
        auto_summarize: request.auto_summarize,
        auto_link: request.auto_link,
        importance: request.importance,
        chunk_size_chars: request.chunk_size_chars,
        chunk_overlap_chars: request.chunk_overlap_chars,
    }
}

fn staged_metadata(source: &StagedMcpSourceV1) -> serde_json::Value {
    json!({
        "capability_id": source.capability_id,
        "tool_name": source.tool_name,
        "arguments": source.arguments,
        "auto_ingest": true,
    })
}

fn load_auto_ingest_receipt(server: &MemoryServer, job_id: &str) -> Result<Option<String>, String> {
    server.with_global_store_read(|store| {
        store
            .connection()
            .query_row(
                "SELECT event_id FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                rusqlite::params![job_id, AUTO_INGEST_RECEIPT_WORKER],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("load durable auto-ingest receipt: {error}"))
    })
}

async fn run_admitted_ingest_from_staged_job(
    server: &MemoryServer,
    request: AdmittedIngestRequest,
) -> Result<String, String> {
    super::ingest::handle_admitted_ingest_source(server, request).await
}

pub(crate) fn stage_auto_ingest_from_mcp(
    server: &MemoryServer,
    capability_id: &str,
    tool_name: &str,
    definition: &serde_json::Value,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    result: &rmcp::model::CallToolResult,
) -> Result<Option<StagedAutoIngest>, String> {
    let Some(job) =
        prepare_auto_ingest_from_mcp(capability_id, tool_name, definition, arguments, result)?
    else {
        return Ok(None);
    };
    let payload_json = serde_json::to_string(&job)
        .map_err(|error| format!("serialize durable auto-ingest job: {error}"))?;
    let job_id = job.job_id.clone();
    let audit_key = ingest_audit_key(AUTO_INGEST_JOB_LABEL, DbScope::Global, None, &job_id);
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
                    job_id,
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
    Ok(Some(StagedAutoIngest { job_id }))
}

pub(crate) async fn run_staged_auto_ingest(
    server: &MemoryServer,
    staged: StagedAutoIngest,
) -> Result<Option<String>, String> {
    let audit_key = ingest_audit_key(AUTO_INGEST_JOB_LABEL, DbScope::Global, None, &staged.job_id);
    let Some(claim) = claim_retryable_ingest_event(
        server,
        DbScope::Global,
        None,
        AUTO_INGEST_JOB_LABEL,
        &audit_key,
        AUTO_INGEST_CLAIM_WORKER,
        &staged.job_id,
        &staged.job_id,
    )?
    else {
        let Some(receipt) = load_auto_ingest_receipt(server, &staged.job_id)? else {
            return Ok(None);
        };
        let mut receipt: serde_json::Value = serde_json::from_str(&receipt)
            .map_err(|error| format!("decode durable auto-ingest receipt: {error}"))?;
        receipt["status"] = json!("replayed");
        return serde_json::to_string(&receipt)
            .map(Some)
            .map_err(|error| format!("serialize replayed auto-ingest receipt: {error}"));
    };
    let lease = RetryableIngestLease::start(
        server,
        DbScope::Global,
        None,
        AUTO_INGEST_CLAIM_WORKER,
        &staged.job_id,
        claim,
    );

    let persisted_payload = match server.with_global_store_read(|store| {
        store
            .connection()
            .query_row(
                "SELECT event_id FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                rusqlite::params![staged.job_id, AUTO_INGEST_JOB_WORKER],
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
    let job: StagedMcpIngestJobV1 = match serde_json::from_str(&persisted_payload) {
        Ok(job) => job,
        Err(decode_error) => {
            let decode_error = format!("decode durable auto-ingest job: {decode_error}");
            let quarantine = quarantine_malformed_auto_ingest(
                &lease,
                &staged.job_id,
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

    if let Err(error) = validate_staged_job(&job, &staged.job_id) {
        return Err(lease
            .fail(
                AUTO_INGEST_JOB_LABEL,
                &audit_key,
                "auto_ingest_admission_drift",
                error,
            )
            .await);
    }
    let params = job.request.clone();
    let source_event_hash = job.idempotency_key.clone();
    let admitted_request = AdmittedIngestRequest {
        job_id: job.job_id,
        idempotency_key: job.idempotency_key,
        request: job.request,
    };

    let outcome: Result<(String, bool), String> = async {
        let (target_db, _) = server.resolve_write_scope(&params.scope);
        let source_audit_key = ingest_audit_key(
            "ingest_source",
            target_db,
            params.project.as_deref(),
            &source_event_hash,
        );
        let response = run_admitted_ingest_from_staged_job(server, admitted_request).await?;
        let response_json: serde_json::Value = serde_json::from_str(&response)
            .map_err(|error| format!("decode admitted ingest accounting: {error}"))?;
        if response_json.get("status") == Some(&json!("partial")) {
            return Ok((response, true));
        }
        let durable_receipt = serde_json::to_string(&response_json)
            .map_err(|error| format!("serialize durable auto-ingest receipt: {error}"))?;
        if !ingest_success_audit_exists(
            server,
            "ingest_source",
            &source_audit_key,
            &source_event_hash,
        )? {
            return Err("auto-ingest source returned without a durable success audit".to_string());
        }
        if let Err(error) = lease
            .write_owned(|store| {
                store
                    .connection()
                    .execute(
                        "INSERT INTO processed_events (event_hash, event_id, worker, created_at) \
                         VALUES (?1, ?2, ?3, STRFTIME('%Y-%m-%dT%H:%M:%fZ', 'now')) \
                         ON CONFLICT(event_hash, worker) DO UPDATE SET \
                           event_id = excluded.event_id, created_at = excluded.created_at",
                        rusqlite::params![
                            staged.job_id,
                            durable_receipt,
                            AUTO_INGEST_RECEIPT_WORKER,
                        ],
                    )
                    .map_err(|error| format!("persist durable auto-ingest receipt: {error}"))?;
                let rows_changed = store
                    .connection()
                    .execute(
                        "DELETE FROM processed_events WHERE event_hash = ?1 AND worker = ?2",
                        rusqlite::params![staged.job_id, AUTO_INGEST_JOB_WORKER],
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
            .await
        {
            let response = serde_json::to_string(&json!({
                "status": "partial",
                "failed_stage": "completion_receipt",
                "chunks_saved": response_json.get("chunks_saved").cloned().unwrap_or(json!(0)),
                "ids": response_json.get("ids").cloned().unwrap_or(json!([])),
                "enrichments_enqueued": response_json
                    .get("enrichments_enqueued")
                    .cloned()
                    .unwrap_or(json!(0)),
                "edges_written": response_json
                    .get("edges_written")
                    .cloned()
                    .unwrap_or(json!(0)),
                "error": error,
            }))
            .map_err(|error| format!("serialize admitted completion partial: {error}"))?;
            return Ok((response, true));
        }
        let mut response_json = response_json;
        let source_status = response_json
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("completed");
        if source_status == "skipped" {
            response_json["status"] = json!("replayed");
        }
        serde_json::to_string(&response_json)
            .map(|response| (response, false))
            .map_err(|error| format!("serialize admitted ingest receipt: {error}"))
    }
    .await;

    match outcome {
        Ok((response, false)) => {
            lease.finish().await?;
            Ok(Some(response))
        }
        Ok((response, true)) => {
            let recovery_message =
                "admitted ingest remains replayable after partial stage".to_string();
            let recovery = lease
                .fail(
                    AUTO_INGEST_JOB_LABEL,
                    &audit_key,
                    "auto_ingest_partial",
                    recovery_message.clone(),
                )
                .await;
            if recovery == recovery_message {
                Ok(Some(response))
            } else {
                let mut response: serde_json::Value = serde_json::from_str(&response)
                    .map_err(|error| format!("decode partial recovery accounting: {error}"))?;
                response["recovery_error"] = json!(recovery);
                serde_json::to_string(&response)
                    .map(Some)
                    .map_err(|error| format!("serialize partial recovery accounting: {error}"))
            }
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
    job_id: &str,
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
                        job_id,
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
                    rusqlite::params![job_id, AUTO_INGEST_JOB_WORKER],
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
            row.map(|job_id| StagedAutoIngest { job_id })
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
        let job_id = job.job_id.clone();
        match run_staged_auto_ingest(server, job).await {
            Ok(Some(_)) => completed += 1,
            Ok(None) => {}
            Err(error) => failures.push(format!("{job_id}: {error}")),
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
