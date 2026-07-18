use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use serde_json::json;

use crate::server_state::MemoryServer;
use tachi_llm::LlmClient;

use super::candidates::collect_candidate_groups;
use super::config::{resolve_batch_size, resolve_distill_backend, DistillBackend};
use super::consolidate_prepass::consolidate_duplicate_candidates;
use super::parser::parse_distill_response;
use super::persist::persist_distill_memory;
use super::prompt::{
    build_batch_prompt, build_batch_user_payload, fallback_distill, DISTILL_DAILY_SYSTEM_PROMPT,
};
use super::types::{CandidateGroup, DistillBatchReport, GroupPayload, SourceManifestEntry};

/// Entry point invoked by the bootstrap scheduler.
///
/// Distills the daemon's bound project DB **and** every other named-project DB
/// in the manifest, so their `derived_items` populate too — previously only the
/// single bound project was ever scanned, leaving named-project/agent DBs with
/// permanently-empty derived items.
///
/// Runs the #1043 D3 consolidate pre-pass (byte-identical duplicate collapse
/// within each candidate bucket) before selection by default. Callers that
/// need the pre-#1043 behavior (e.g. a `--no-consolidate` escape hatch) use
/// [`run_daily_batch_distill_with_options`].
pub async fn run_daily_batch_distill(server: &MemoryServer) -> Result<DistillBatchReport, String> {
    run_daily_batch_distill_with_options(server, true).await
}

/// Same as [`run_daily_batch_distill`], with the consolidate pre-pass
/// toggleable — `run_consolidate=false` reproduces the pre-#1043 behavior
/// (selection sees the raw, undeduplicated candidate pool).
pub async fn run_daily_batch_distill_with_options(
    server: &MemoryServer,
    run_consolidate: bool,
) -> Result<DistillBatchReport, String> {
    let mut report = DistillBatchReport::default();

    let bound_name = server
        .project_db_path_buf()
        .and_then(|p| crate::path_utils::named_project_for_db_path(&p));

    // 1. The daemon's bound project DB (existing behavior).
    if server.has_project_db() {
        report.projects_scanned += 1;
        distill_one_project(
            server,
            None,
            derive_project_label(server),
            run_consolidate,
            &mut report,
        )
        .await;
    }

    // 2. Every other named-project DB in the manifest.
    for name in crate::path_utils::list_named_projects() {
        if name.eq_ignore_ascii_case("wiki") {
            continue; // wiki has its own curation path
        }
        if bound_name
            .as_deref()
            .is_some_and(|bound| bound.eq_ignore_ascii_case(&name))
        {
            continue; // already handled as the bound project above
        }
        report.projects_scanned += 1;
        // Best-effort per project: a failure on one must not abort the rest.
        distill_one_project(
            server,
            Some(&name),
            name.clone(),
            run_consolidate,
            &mut report,
        )
        .await;
    }

    // Opportunistically prune stale foundry-runs subdirs once per run.
    let _ = server.claude_pool.cleanup_expired();

    Ok(report)
}

/// Run the distill batch against one target DB: the bound project when
/// `project` is `None`, otherwise the named project. Errors are recorded in
/// `report.errors` rather than propagated so one project cannot abort the run.
async fn distill_one_project(
    server: &MemoryServer,
    project: Option<&str>,
    project_label: String,
    run_consolidate: bool,
    report: &mut DistillBatchReport,
) {
    let mut candidates = match collect_candidate_groups(server, project) {
        Ok(candidates) => candidates,
        Err(err) => {
            report
                .errors
                .push(format!("collect candidates [{project_label}]: {err}"));
            return;
        }
    };
    if candidates.is_empty() {
        return;
    }

    if run_consolidate {
        report.consolidated += consolidate_duplicate_candidates(server, project, &mut candidates);
    }

    let batch_run_id = format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S"),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let runs_root = server
        .claude_pool
        .runs_dir()
        .join("distill")
        .join(&project_label)
        .join(&batch_run_id);
    let _ = std::fs::create_dir_all(&runs_root);

    let mut manifest: Vec<SourceManifestEntry> = Vec::new();
    let backend = resolve_distill_backend();
    let batch_size = resolve_batch_size();

    for (chunk_idx, chunk) in candidates.chunks(batch_size).enumerate() {
        match backend {
            DistillBackend::ClaudeCli => {
                process_claude_batch(
                    server,
                    chunk,
                    chunk_idx,
                    &project_label,
                    &batch_run_id,
                    report,
                    &mut manifest,
                    project,
                )
                .await;
            }
            DistillBackend::RawApi => {
                process_api_batch(
                    server,
                    chunk,
                    chunk_idx,
                    report,
                    &mut manifest,
                    &batch_run_id,
                    project,
                )
                .await;
            }
        }
    }

    // Best-effort: write the audit manifest without failing the distill run.
    let manifest_path = runs_root.join("source_manifest.json");
    match serde_json::to_string_pretty(&json!({
        "batch_run_id": batch_run_id,
        "project": project_label,
        "backend": backend.as_str(),
        "batch_size": batch_size,
        "generated_at": Utc::now().to_rfc3339(),
        "report": &*report,
        "groups": manifest,
    })) {
        Ok(body) => {
            if let Err(err) =
                crate::utils::write_owner_only_file_atomic(&manifest_path, body.as_bytes())
            {
                tracing::warn!(
                    "failed to write distill source manifest {}: {err}",
                    manifest_path.display()
                );
            }
        }
        Err(err) => tracing::warn!("failed to serialize distill source manifest: {err}"),
    }
}

async fn process_claude_batch(
    server: &MemoryServer,
    chunk: &[CandidateGroup],
    chunk_idx: usize,
    project_label: &str,
    batch_run_id: &str,
    report: &mut DistillBatchReport,
    manifest: &mut Vec<SourceManifestEntry>,
    project: Option<&str>,
) {
    report.batches_dispatched += 1;
    let label = format!("distill-{}-b{}", project_label, chunk_idx);
    let prompt = build_batch_prompt(chunk);
    let call_result = call_claude_batch(server, &label, &prompt, chunk).await;
    match call_result {
        Ok(outcome) => match parse_distill_response(&outcome.text) {
            Ok(per_group) => {
                apply_parsed_groups(
                    server,
                    chunk,
                    &per_group,
                    batch_run_id,
                    DistillBackend::ClaudeCli,
                    false,
                    report,
                    manifest,
                    project,
                )
                .await;
            }
            Err(err) => {
                report
                    .errors
                    .push(format!("parse claude batch {chunk_idx}: {err}"));
                for group in chunk {
                    fallback_one_group(server, group, batch_run_id, report, manifest, project)
                        .await;
                }
            }
        },
        Err(err) => {
            report
                .errors
                .push(format!("claude batch {chunk_idx}: {err}"));
            for group in chunk {
                fallback_one_group(server, group, batch_run_id, report, manifest, project).await;
            }
        }
    }
}

async fn process_api_batch(
    server: &MemoryServer,
    chunk: &[CandidateGroup],
    chunk_idx: usize,
    report: &mut DistillBatchReport,
    manifest: &mut Vec<SourceManifestEntry>,
    batch_run_id: &str,
    project: Option<&str>,
) {
    let mut pending: Vec<(&[CandidateGroup], usize)> = vec![(chunk, 0)];
    let max_depth = 8;
    while let Some((batch, depth)) = pending.pop() {
        if batch.is_empty() {
            continue;
        }
        report.batches_dispatched += 1;
        match call_api_batch_distill(&server.llm, batch).await {
            Ok(raw) => match parse_distill_response(&raw) {
                Ok(per_group) => {
                    apply_parsed_groups(
                        server,
                        batch,
                        &per_group,
                        batch_run_id,
                        DistillBackend::RawApi,
                        false,
                        report,
                        manifest,
                        project,
                    )
                    .await;
                }
                Err(err) if batch.len() > 1 && depth < max_depth => {
                    report.errors.push(format!(
                        "parse api batch {chunk_idx} ({} groups): {err}; splitting",
                        batch.len()
                    ));
                    let mid = batch.len() / 2;
                    pending.push((&batch[mid..], depth + 1));
                    pending.push((&batch[..mid], depth + 1));
                }
                Err(err) => {
                    report
                        .errors
                        .push(format!("parse api batch {chunk_idx}: {err}"));
                    for group in batch {
                        fallback_one_group(server, group, batch_run_id, report, manifest, project)
                            .await;
                    }
                }
            },
            Err(err) if batch.len() > 1 && depth < max_depth => {
                report.errors.push(format!(
                    "api batch {chunk_idx} ({} groups): {err}; splitting",
                    batch.len()
                ));
                let mid = batch.len() / 2;
                pending.push((&batch[mid..], depth + 1));
                pending.push((&batch[..mid], depth + 1));
            }
            Err(err) => {
                report.errors.push(format!("api batch {chunk_idx}: {err}"));
                for group in batch {
                    fallback_one_group(server, group, batch_run_id, report, manifest, project)
                        .await;
                }
            }
        }
    }
}

async fn apply_parsed_groups(
    server: &MemoryServer,
    chunk: &[CandidateGroup],
    per_group: &HashMap<String, GroupPayload>,
    batch_run_id: &str,
    backend: DistillBackend,
    fallback_used: bool,
    report: &mut DistillBatchReport,
    manifest: &mut Vec<SourceManifestEntry>,
    project: Option<&str>,
) {
    let backend_label = backend.as_str();
    for group in chunk {
        let item = per_group.get(&group.group_id);
        match item {
            Some(payload) if !payload.text.trim().is_empty() => {
                match persist_distill_memory(
                    server,
                    group,
                    payload,
                    batch_run_id,
                    backend_label,
                    fallback_used,
                    project,
                ) {
                    Ok(id) => {
                        report.groups_distilled += 1;
                        if fallback_used {
                            report.fallback_used += 1;
                        }
                        manifest.push(SourceManifestEntry {
                            group_id: group.group_id.clone(),
                            path_prefix: group.path_prefix.clone(),
                            coherence_key: group.coherence_key.clone(),
                            source_memory_ids: group.entries.iter().map(|e| e.id.clone()).collect(),
                            written_memory_id: Some(id),
                            backend: backend_label,
                            fallback_used,
                            skip_reason: None,
                        });
                    }
                    Err(err) => {
                        report
                            .errors
                            .push(format!("persist {}: {err}", group.group_id));
                    }
                }
            }
            Some(payload) => {
                report.groups_skipped += 1;
                manifest.push(SourceManifestEntry {
                    group_id: group.group_id.clone(),
                    path_prefix: group.path_prefix.clone(),
                    coherence_key: group.coherence_key.clone(),
                    source_memory_ids: group.entries.iter().map(|e| e.id.clone()).collect(),
                    written_memory_id: None,
                    backend: backend_label,
                    fallback_used,
                    skip_reason: Some(
                        payload
                            .skip_reason
                            .clone()
                            .unwrap_or_else(|| "empty_text".to_string()),
                    ),
                });
            }
            None => {
                fallback_one_group(server, group, batch_run_id, report, manifest, project).await;
            }
        }
    }
}

/// Run the distill batch call via the provider executor. The run-directory
/// artifact contract (`prompt.md`/`result.md`/`status.json`) is preserved,
/// since the path goes through `ClaudePool::call_via_provider`. #1261 step
/// 2/3: the CLI fallback branch was removed; the only path now is the
/// provider executor.
pub(crate) async fn call_claude_batch(
    server: &MemoryServer,
    label: &str,
    prompt: &str,
    chunk: &[CandidateGroup],
) -> Result<tachi_llm::claude_pool::ClaudeCallOutcome, String> {
    let llm = server.llm.clone();
    let user_payload = build_batch_user_payload(chunk);
    let max_tokens = batch_max_tokens(chunk.len());
    server
        .claude_pool
        .call_via_provider(label, prompt, move || async move {
            llm.call_distill_llm(
                DISTILL_DAILY_SYSTEM_PROMPT,
                &user_payload,
                None,
                0.3,
                max_tokens,
            )
            .await
        })
        .await
}

async fn call_api_batch_distill(
    llm: &LlmClient,
    groups: &[CandidateGroup],
) -> Result<String, String> {
    let user = build_batch_user_payload(groups);
    let max_tokens = batch_max_tokens(groups.len());
    llm.call_distill_llm(DISTILL_DAILY_SYSTEM_PROMPT, &user, None, 0.3, max_tokens)
        .await
}

fn batch_max_tokens(group_count: usize) -> u32 {
    let estimated = group_count.saturating_mul(700).saturating_add(512);
    estimated.min(8192) as u32
}

async fn fallback_one_group(
    server: &MemoryServer,
    group: &CandidateGroup,
    batch_run_id: &str,
    report: &mut DistillBatchReport,
    manifest: &mut Vec<SourceManifestEntry>,
    project: Option<&str>,
) {
    match fallback_distill(&server.llm, group).await {
        Ok(payload) => {
            match persist_distill_memory(
                server,
                group,
                &payload,
                batch_run_id,
                "raw_api",
                true,
                project,
            ) {
                Ok(id) => {
                    report.groups_distilled += 1;
                    report.fallback_used += 1;
                    manifest.push(SourceManifestEntry {
                        group_id: group.group_id.clone(),
                        path_prefix: group.path_prefix.clone(),
                        coherence_key: group.coherence_key.clone(),
                        source_memory_ids: group.entries.iter().map(|e| e.id.clone()).collect(),
                        written_memory_id: Some(id),
                        backend: "raw_api",
                        fallback_used: true,
                        skip_reason: None,
                    });
                }
                Err(err) => {
                    report
                        .errors
                        .push(format!("persist fallback {}: {err}", group.group_id));
                }
            }
        }
        Err(err) => {
            report.groups_skipped += 1;
            report
                .errors
                .push(format!("fallback {}: {err}", group.group_id));
            manifest.push(SourceManifestEntry {
                group_id: group.group_id.clone(),
                path_prefix: group.path_prefix.clone(),
                coherence_key: group.coherence_key.clone(),
                source_memory_ids: group.entries.iter().map(|e| e.id.clone()).collect(),
                written_memory_id: None,
                backend: "raw_api",
                fallback_used: true,
                skip_reason: Some(err),
            });
        }
    }
}

fn derive_project_label(server: &MemoryServer) -> String {
    server
        .project_db_path_buf()
        .and_then(|p: PathBuf| {
            p.parent()
                .and_then(|parent| parent.file_name())
                .and_then(|os| os.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "project".to_string())
}
