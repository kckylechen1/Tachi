use crate::server_state::MemoryServer;
use crate::tool_params::WikiWriteParams;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tachi_llm::PersistedModelInvocationReceiptV1;

use super::DailyPipelineReport;

pub(crate) const DAILY_MODEL_INVOCATIONS_SCHEMA_V1: &str = "daily-model-invocations-v1";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DailyModelInvocationsSidecarV1 {
    pub schema: String,
    pub date: String,
    pub revision: i64,
    pub payload_basename: String,
    pub report_content_hash: String,
    pub health: Option<PersistedModelInvocationReceiptV1>,
    pub routing: Option<PersistedModelInvocationReceiptV1>,
}

pub(crate) fn daily_report_generation_sidecar_path(report_path: &Path, revision: i64) -> PathBuf {
    let stem = report_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("daily");
    report_path.with_file_name(format!("{stem}.r{revision}.model-invocations-v1.json"))
}

pub(crate) fn daily_report_payload_path(report_path: &Path, revision: i64) -> PathBuf {
    let stem = report_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("daily");
    report_path.with_file_name(format!("{stem}.r{revision}.md"))
}

pub(crate) fn serialize_daily_json_section(value: &Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|e| format!("serialize daily JSON section: {e}"))
}

pub(crate) fn render_daily_report_markdown(
    report: &DailyPipelineReport,
    health_section: &str,
    truth_section: &str,
    routing_section: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Tachi Daily Pipeline - {}\n\n", report.date));
    out.push_str("## Summary\n\n");
    out.push_str(&format!(
        "- Health Check: {}\n",
        report.health_check.summary
    ));
    out.push_str(&format!(
        "- Truth Maintenance: {}\n",
        report.truth_maintenance.summary
    ));
    out.push_str(&format!(
        "- Routing Analysis: {}\n\n",
        report.routing_analysis.summary
    ));

    out.push_str("## Health Check\n\n");
    out.push_str("```json\n");
    out.push_str(health_section);
    out.push_str("\n```\n\n");

    out.push_str("## Truth Maintenance\n\n");
    out.push_str("```json\n");
    out.push_str(truth_section);
    out.push_str("\n```\n\n");

    out.push_str("## Routing Analysis\n\n");
    out.push_str("```json\n");
    out.push_str(routing_section);
    out.push_str("\n```\n");
    out
}

pub(crate) fn next_daily_report_revision(sidecar_path: &Path) -> i64 {
    let name = sidecar_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("daily");
    let stem = name
        .strip_suffix(".model-invocations-v1.json")
        .or_else(|| sidecar_path.file_stem().and_then(|value| value.to_str()))
        .unwrap_or("daily");
    let stem = stem
        .rsplit_once(".r")
        .filter(|(_, revision)| revision.parse::<i64>().is_ok())
        .map(|(base, _)| base)
        .unwrap_or(stem);
    let parent = sidecar_path.parent().unwrap_or_else(|| Path::new("."));
    let prefix = format!("{stem}.r");
    let mut max_revision = 0_i64;
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            let rest = rest
                .strip_suffix(".md")
                .or_else(|| rest.strip_suffix(".model-invocations-v1.json"));
            let Some(rest) = rest else { continue };
            if let Ok(revision) = rest.parse::<i64>() {
                max_revision = max_revision.max(revision);
            }
        }
    }
    max_revision.saturating_add(1).max(1)
}

pub(crate) fn build_daily_model_invocations_sidecar(
    date: &str,
    revision: i64,
    markdown: &str,
    health_section: &str,
    routing_section: &str,
    health_invocation: &PersistedModelInvocationReceiptV1,
    routing_invocation: Option<&PersistedModelInvocationReceiptV1>,
) -> DailyModelInvocationsSidecarV1 {
    let health = health_invocation.bound_to_content(
        health_section,
        format!("daily-report:{date}:health"),
        revision,
    );
    let routing = routing_invocation.map(|invocation| {
        invocation.bound_to_content(
            routing_section,
            format!("daily-report:{date}:routing"),
            revision,
        )
    });
    DailyModelInvocationsSidecarV1 {
        schema: DAILY_MODEL_INVOCATIONS_SCHEMA_V1.to_string(),
        date: date.to_string(),
        revision,
        payload_basename: format!("{date}.r{revision}.md"),
        report_content_hash: PersistedModelInvocationReceiptV1::content_hash_for(markdown),
        health: Some(health),
        routing,
    }
}

const MAX_DAILY_PUBLISH_RETRIES: usize = 8;

/// The sidecar is the immutable generation commit point. Keep every same-date
/// generation: lock-free keep-one-prior garbage collection can race with a
/// reader or publisher, so retention needs its own synchronization contract.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DailyPublishFailurePoint {
    None,
    P1AfterPayloadBytesBeforePayloadFsync,
    P2AfterPayloadFsyncBeforeSidecarTemp,
    P3AfterSidecarTempFsyncBeforeHardLink,
    P4AfterHardLinkBeforeDirectoryFsync,
}

/// Publish one immutable payload and expose its generation sidecar as the
/// commit point. The hard link is no-replace: an existing generation is a
/// collision for the caller to rescan/retry, never an overwrite.
pub(crate) fn publish_daily_report_pair(
    report_path: &Path,
    markdown: &str,
    sidecar_path: &Path,
    sidecar: &DailyModelInvocationsSidecarV1,
) -> Result<(), String> {
    publish_daily_report_pair_inner(
        report_path,
        markdown,
        sidecar_path,
        sidecar,
        #[cfg(test)]
        DailyPublishFailurePoint::None,
    )
}

/// Test-only: inject a failure at one of the publication durability points.
#[cfg(test)]
pub(crate) fn publish_daily_report_pair_with_failure(
    report_path: &Path,
    markdown: &str,
    sidecar_path: &Path,
    sidecar: &DailyModelInvocationsSidecarV1,
    failure_point: DailyPublishFailurePoint,
) -> Result<(), String> {
    publish_daily_report_pair_inner(report_path, markdown, sidecar_path, sidecar, failure_point)
}

fn publish_daily_report_pair_inner(
    report_path: &Path,
    markdown: &str,
    sidecar_path: &Path,
    sidecar: &DailyModelInvocationsSidecarV1,
    #[cfg(test)] failure_point: DailyPublishFailurePoint,
) -> Result<(), String> {
    let sidecar_bytes = serde_json::to_vec_pretty(sidecar)
        .map_err(|e| format!("serialize daily model-invocations sidecar: {e}"))?;
    let report_bytes = markdown.as_bytes();
    let payload_path = report_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(&sidecar.payload_basename);

    if let Some(parent) = report_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create daily report dir: {e}"))?;
    }

    let sidecar_tmp = temp_sibling(sidecar_path, "json")?;

    let cleanup_temps = |sidecar_tmp: &Path| {
        let _ = std::fs::remove_file(sidecar_tmp);
    };

    if let Err(error) = write_owner_only_create_new(
        &payload_path,
        report_bytes,
        #[cfg(test)]
        failure_point,
    ) {
        cleanup_temps(&sidecar_tmp);
        return Err(error);
    }
    if let Err(error) = write_temp_owner_only(
        &sidecar_tmp,
        &sidecar_bytes,
        #[cfg(test)]
        failure_point,
    ) {
        cleanup_temps(&sidecar_tmp);
        return Err(error);
    }

    if let Err(error) = std::fs::hard_link(&sidecar_tmp, sidecar_path) {
        cleanup_temps(&sidecar_tmp);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err("daily generation collision; rescan and retry".to_string());
        }
        return Err(format!(
            "hard-link daily sidecar temp {} -> {}: {error}",
            sidecar_tmp.display(),
            sidecar_path.display()
        ));
    }
    cleanup_temps(&sidecar_tmp);

    #[cfg(test)]
    if failure_point == DailyPublishFailurePoint::P4AfterHardLinkBeforeDirectoryFsync {
        return Err(
            "injected daily report publish failure after hard-link before directory fsync"
                .to_string(),
        );
    }

    crate::utils::sync_parent_dir(&payload_path)?;
    Ok(())
}

pub(crate) fn publish_daily_report_with_retry(
    report_path: &Path,
    date: &str,
    markdown: &str,
    health_section: &str,
    routing_section: &str,
    health_invocation: &PersistedModelInvocationReceiptV1,
    routing_invocation: Option<&PersistedModelInvocationReceiptV1>,
) -> Result<(i64, DailyModelInvocationsSidecarV1), String> {
    publish_daily_report_with_retry_inner(
        report_path,
        date,
        markdown,
        health_section,
        routing_section,
        health_invocation,
        routing_invocation,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
pub(crate) fn publish_daily_report_with_retry_after_first_allocation(
    report_path: &Path,
    date: &str,
    markdown: &str,
    health_section: &str,
    routing_section: &str,
    health_invocation: &PersistedModelInvocationReceiptV1,
    routing_invocation: Option<&PersistedModelInvocationReceiptV1>,
    first_allocation_barrier: &std::sync::Barrier,
) -> Result<(i64, DailyModelInvocationsSidecarV1), String> {
    publish_daily_report_with_retry_inner(
        report_path,
        date,
        markdown,
        health_section,
        routing_section,
        health_invocation,
        routing_invocation,
        Some(first_allocation_barrier),
    )
}

fn publish_daily_report_with_retry_inner(
    report_path: &Path,
    date: &str,
    markdown: &str,
    health_section: &str,
    routing_section: &str,
    health_invocation: &PersistedModelInvocationReceiptV1,
    routing_invocation: Option<&PersistedModelInvocationReceiptV1>,
    #[cfg(test)] first_allocation_barrier: Option<&std::sync::Barrier>,
) -> Result<(i64, DailyModelInvocationsSidecarV1), String> {
    for _attempt in 0..MAX_DAILY_PUBLISH_RETRIES {
        let revision = next_daily_report_revision(report_path);
        let sidecar_path = daily_report_generation_sidecar_path(report_path, revision);
        let sidecar = build_daily_model_invocations_sidecar(
            date,
            revision,
            markdown,
            health_section,
            routing_section,
            health_invocation,
            routing_invocation,
        );
        #[cfg(test)]
        if _attempt == 0 {
            if let Some(barrier) = first_allocation_barrier {
                barrier.wait();
            }
        }
        match publish_daily_report_pair(report_path, markdown, &sidecar_path, &sidecar) {
            Ok(()) => return Ok((revision, sidecar)),
            Err(error) if error.contains("daily generation collision") => continue,
            Err(error) => return Err(error),
        }
    }
    Err("daily report generation collision retry budget exhausted".to_string())
}

fn temp_sibling(path: &Path, label: &str) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(label);
    Ok(path.with_file_name(format!(
        "{file_name}.tmp.{}",
        uuid::Uuid::new_v4().as_simple()
    )))
}

fn write_temp_owner_only(
    path: &Path,
    bytes: &[u8],
    #[cfg(test)] failure_point: DailyPublishFailurePoint,
) -> Result<(), String> {
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("open temp {}: {e}", path.display()))?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("open temp {}: {e}", path.display()))?;

    use std::io::Write;
    file.write_all(bytes)
        .map_err(|e| format!("write temp {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("fsync temp {}: {e}", path.display()))?;
    #[cfg(test)]
    if failure_point == DailyPublishFailurePoint::P3AfterSidecarTempFsyncBeforeHardLink {
        return Err(
            "injected daily report publish failure after sidecar temp fsync before hard-link"
                .to_string(),
        );
    }
    Ok(())
}

fn write_owner_only_create_new(
    path: &Path,
    bytes: &[u8],
    #[cfg(test)] failure_point: DailyPublishFailurePoint,
) -> Result<(), String> {
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    "daily generation collision; rescan and retry".to_string()
                } else {
                    format!("open immutable report {}: {e}", path.display())
                }
            })?
    };
    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                "daily generation collision; rescan and retry".to_string()
            } else {
                format!("open immutable report {}: {e}", path.display())
            }
        })?;

    use std::io::Write;
    file.write_all(bytes)
        .map_err(|e| format!("write immutable report {}: {e}", path.display()))?;
    #[cfg(test)]
    if failure_point == DailyPublishFailurePoint::P1AfterPayloadBytesBeforePayloadFsync {
        return Err(
            "injected daily report publish failure after payload bytes before payload fsync"
                .to_string(),
        );
    }
    file.sync_all()
        .map_err(|e| format!("fsync immutable report {}: {e}", path.display()))?;
    #[cfg(test)]
    if failure_point == DailyPublishFailurePoint::P2AfterPayloadFsyncBeforeSidecarTemp {
        return Err(
            "injected daily report publish failure after payload fsync before sidecar temp"
                .to_string(),
        );
    }
    Ok(())
}

fn json_section(markdown: &str, heading: &str) -> Option<String> {
    let marker = format!("## {heading}\n\n```json\n");
    let start = markdown.find(&marker)? + marker.len();
    let end = markdown[start..].find("\n```\n")? + start;
    Some(markdown[start..end].to_string())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedModelInvocationReceiptV1Wire {
    schema: String,
    lane: String,
    engine_kind: String,
    effective_provider: Option<String>,
    effective_model: Option<String>,
    effective_version: Option<String>,
    fallback_chain: Vec<String>,
    degraded: bool,
    completion_status: String,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    latency_ms: Option<u64>,
    content_hash: String,
    memory_id: String,
    revision: i64,
}

fn receipt_binding_matches(
    value: Option<&Value>,
    content: &str,
    memory_id: &str,
    revision: i64,
) -> bool {
    let Some(value) = value else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    for key in [
        "schema",
        "lane",
        "engine_kind",
        "effective_provider",
        "effective_model",
        "effective_version",
        "fallback_chain",
        "degraded",
        "completion_status",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "latency_ms",
        "content_hash",
        "memory_id",
        "revision",
    ] {
        if !object.contains_key(key) {
            return false;
        }
    }
    let Ok(receipt) =
        serde_json::from_value::<PersistedModelInvocationReceiptV1Wire>(value.clone())
    else {
        return false;
    };
    // These fields are intentionally validated by deserialization even though
    // the daily reader does not interpret their values beyond their wire type.
    let _ = (
        receipt.degraded,
        receipt.prompt_tokens,
        receipt.completion_tokens,
        receipt.total_tokens,
        receipt.latency_ms,
    );
    receipt.schema == tachi_llm::MODEL_INVOCATION_SCHEMA_V1
        && receipt.lane == "reasoning"
        && matches!(receipt.engine_kind.as_str(), "provider_http" | "claude_cli")
        && [
            &receipt.effective_provider,
            &receipt.effective_model,
            &receipt.effective_version,
        ]
        .into_iter()
        .flatten()
        .all(|identity| !identity.trim().is_empty())
        && receipt.fallback_chain.len() <= 4
        && receipt.fallback_chain.iter().all(|step| {
            matches!(
                step.as_str(),
                "provider_http_fallback" | "claude_cli_to_provider_http"
            )
        })
        && matches!(receipt.completion_status.as_str(), "complete" | "unknown")
        && receipt.content_hash == PersistedModelInvocationReceiptV1::content_hash_for(content)
        && receipt.memory_id == memory_id
        && receipt.revision == revision
}

fn validated_payload_from_sidecar(sidecar_path: &Path) -> Option<(String, i64, PathBuf)> {
    let raw = std::fs::read_to_string(sidecar_path).ok()?;
    let sidecar: Value = serde_json::from_str(&raw).ok()?;
    let schema = sidecar.get("schema").and_then(Value::as_str)?;
    let date = sidecar.get("date").and_then(Value::as_str)?;
    let revision = sidecar.get("revision").and_then(Value::as_i64)?;
    let payload_basename = sidecar.get("payload_basename").and_then(Value::as_str)?;
    if schema != DAILY_MODEL_INVOCATIONS_SCHEMA_V1 || revision < 1 {
        return None;
    }
    let expected = format!("{date}.r{revision}.md");
    if payload_basename != expected
        || Path::new(payload_basename)
            .file_name()
            .and_then(|v| v.to_str())
            != Some(payload_basename)
    {
        return None;
    }
    let parent = sidecar_path.parent()?;
    let payload_path = parent.join(payload_basename);
    if !payload_path.is_file() {
        return None;
    }
    let markdown = std::fs::read_to_string(&payload_path).ok()?;
    if sidecar.get("report_content_hash").and_then(Value::as_str)
        != Some(PersistedModelInvocationReceiptV1::content_hash_for(&markdown).as_str())
    {
        return None;
    }
    let health = json_section(&markdown, "Health Check")?;
    let health_value = sidecar.get("health")?;
    if !receipt_binding_matches(
        Some(health_value),
        &health,
        &format!("daily-report:{date}:health"),
        revision,
    ) {
        return None;
    }
    let routing_section = json_section(&markdown, "Routing Analysis")?;
    let routing_details: Value = serde_json::from_str(&routing_section).ok()?;
    let routing = sidecar.get("routing")?;
    let is_non_model_skip =
        routing_details.get("disposition").and_then(Value::as_str) == Some("non_model_skip");
    match (is_non_model_skip, routing.is_null()) {
        (true, true) => {}
        (false, false) => {
            if !receipt_binding_matches(
                Some(routing),
                &routing_section,
                &format!("daily-report:{date}:routing"),
                revision,
            ) {
                return None;
            }
        }
        // A skip cannot carry a model receipt, and model-derived content
        // cannot omit one. Either mismatch makes the generation invalid.
        _ => return None,
    }
    Some((date.to_string(), revision, payload_path))
}

pub(crate) fn validated_latest_daily_report(app_home: &Path) -> Option<String> {
    let reports_dir = app_home.join("reports").join("daily");
    let entries = std::fs::read_dir(&reports_dir).ok()?;
    let mut sidecar_dates = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    let mut legacy = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(generation) = name.strip_suffix(".model-invocations-v1.json") {
            let Some((date, _revision)) =
                generation.rsplit_once(".r").and_then(|(date, revision)| {
                    revision
                        .parse::<i64>()
                        .ok()
                        .map(|revision| (date, revision))
                })
            else {
                continue;
            };
            sidecar_dates.insert(date.to_string());
            if let Some((date, revision, payload)) = validated_payload_from_sidecar(&path) {
                candidates.push((date, revision, payload));
            }
        } else if let Some(date) = name.strip_suffix(".md") {
            if !date.contains(".r") && path.is_file() {
                legacy.push((date.to_string(), 0_i64, path));
            }
        }
    }
    candidates.extend(
        legacy
            .into_iter()
            .filter(|(date, _, _)| !sidecar_dates.contains(date)),
    );
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    candidates
        .pop()
        .map(|(_, _, path)| path.display().to_string())
}

pub(crate) async fn save_daily_health_wiki(
    server: &MemoryServer,
    date: &str,
    health_body: &str,
    health_invocation: PersistedModelInvocationReceiptV1,
) -> Result<(), String> {
    // Official wiki+invocation seam. Path `/tachi/daily-health` normalizes to
    // `/wiki/tachi/daily-health`. Single metadata slot carries the health
    // receipt; routing remains independently named in the markdown sidecar.
    let _ = crate::copilot_ops::handle_tachi_wiki_write_with_model_invocation(
        server,
        WikiWriteParams {
            title: format!("Tachi Daily Health {date}"),
            text: health_body.to_string(),
            path: Some("/tachi/daily-health".to_string()),
            topic: Some("daily-health".to_string()),
            summary: Some(format!("Daily Pipeline report for {date}")),
            category: "experience".to_string(),
            keywords: vec![
                "tachi".to_string(),
                "daily-pipeline".to_string(),
                "health-check".to_string(),
            ],
            entities: vec!["Tachi".to_string()],
            importance: 0.85,
            scope: "global".to_string(),
            retention_policy: "permanent".to_string(),
            domain: Some("wiki".to_string()),
            project: None,
            metadata: None,
            force: true,
            references: Vec::new(),
            include_patterns: false,
            pattern_query: None,
            pattern_top_k: None,
        },
        health_invocation,
    )
    .await?;
    Ok(())
}

pub(crate) fn parse_llm_json(raw: &str) -> Result<Value, String> {
    let stripped = tachi_llm::LlmClient::strip_code_fence(raw);
    serde_json::from_str(stripped)
        .or_else(|_| {
            let start = stripped.find('{').unwrap_or(0);
            let end = stripped
                .rfind('}')
                .map(|idx| idx + 1)
                .unwrap_or(stripped.len());
            serde_json::from_str(&stripped[start..end])
        })
        .map_err(|e| {
            // Do NOT embed raw LLM response — it may contain sensitive content
            // or internal state that should not surface to callers.
            format!(
                "parse daily health JSON failed: {e}; raw response length={} chars (check server logs for full payload)",
                raw.len()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daily_pipeline::{DailyPipelineReport, DailyStageReport};
    use serde_json::json;

    fn stage(summary: &str) -> DailyStageReport {
        DailyStageReport {
            status: "completed".to_string(),
            summary: summary.to_string(),
            details: json!({ "scope_accounting": { "expected_exclusions": [] } }),
        }
    }

    #[test]
    fn daily_report_persists_truth_maintenance_accounting() {
        let report = DailyPipelineReport {
            date: "2026-07-16".to_string(),
            report_path: None,
            health_check: stage("healthy"),
            truth_maintenance: stage("1 expected exclusion"),
            routing_analysis: stage("none"),
        };

        let markdown = render_daily_report_markdown(
            &report,
            "{}",
            &serialize_daily_json_section(&report.truth_maintenance.details).unwrap(),
            "{}",
        );
        assert!(markdown.contains("Truth Maintenance: 1 expected exclusion"));
        assert!(markdown.contains("## Truth Maintenance"));
        assert!(markdown.contains("expected_exclusions"));
    }
}
