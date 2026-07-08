use super::types::{HealthDeduction, ProviderProbeResult, ProviderRotationGroupProbe};
use crate::status_ops::{ApiKeyStatus, DbStatus};

#[cfg(test)]
pub(crate) fn calculate_health_score(
    daemon: &crate::status_ops::DaemonStatus,
    dbs: &[DbStatus],
    distill_marker: Option<&crate::status_ops::DistillMarkerStatus>,
    api_keys: &[ApiKeyStatus],
    probe_results: Option<&[ProviderProbeResult]>,
    rotation_group_results: Option<&[ProviderRotationGroupProbe]>,
) -> u8 {
    let deductions = calculate_health_deductions(
        daemon,
        dbs,
        distill_marker,
        api_keys,
        probe_results,
        rotation_group_results,
    );
    health_score_from_deductions(&deductions)
}

pub(crate) fn health_score_from_deductions(deductions: &[HealthDeduction]) -> u8 {
    let mut score = 100i32;
    for deduction in deductions {
        score -= i32::from(deduction.points);
    }
    score.clamp(0, 100) as u8
}

pub(crate) fn calculate_health_deductions(
    _daemon: &crate::status_ops::DaemonStatus,
    dbs: &[DbStatus],
    distill_marker: Option<&crate::status_ops::DistillMarkerStatus>,
    api_keys: &[ApiKeyStatus],
    probe_results: Option<&[ProviderProbeResult]>,
    rotation_group_results: Option<&[ProviderRotationGroupProbe]>,
) -> Vec<HealthDeduction> {
    let mut deductions = Vec::new();
    // A missing daemon is visible in the daemon/status surfaces, but it is not
    // itself a health failure. Tachi often runs as stdio MCP servers or short
    // CLI invocations; penalize the concrete consequences instead (stale
    // distill, failed jobs, provider failures, vector gaps).
    // Only dead-lettered (retry-exhausted) jobs count as genuine, current
    // failures. Transient failures are auto-retried with backoff and self-heal,
    // so they no longer drag the score down for their full GC-retention window.
    let failed_jobs: usize = dbs.iter().map(|db| db.dead_lettered).sum();
    push_count_deduction(
        &mut deductions,
        "dead_lettered_foundry_jobs",
        "dead-lettered foundry jobs",
        failed_jobs.min(25),
        format!(
            "{} dead-lettered job(s) across {}",
            failed_jobs,
            labels_for(dbs, |db| db.dead_lettered > 0)
        ),
    );
    let stuck_jobs: usize = dbs.iter().map(|db| db.stuck_in_progress).sum();
    push_count_deduction(
        &mut deductions,
        "stuck_foundry_jobs",
        "stuck foundry jobs",
        (stuck_jobs * 5).min(20),
        format!(
            "{} stuck running job(s) across {}",
            stuck_jobs,
            labels_for(dbs, |db| db.stuck_in_progress > 0)
        ),
    );
    let low_vector_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::low_vector_coverage(db))
        .collect::<Vec<_>>();
    push_count_deduction(
        &mut deductions,
        "low_vector_coverage",
        "low vector coverage",
        (low_vector_dbs.len() * 10).min(25),
        format!(
            "{} db(s) below 99% vector coverage: {}",
            low_vector_dbs.len(),
            labels_for_refs(&low_vector_dbs)
        ),
    );
    let dim_mismatch_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::vector_dimension_mismatch(db))
        .collect::<Vec<_>>();
    push_count_deduction(
        &mut deductions,
        "vector_dimension_mismatch",
        "vector dimension mismatch",
        (dim_mismatch_dbs.len() * 15).min(30),
        format!(
            "{} db(s) have unexpected vector dimensions: {}",
            dim_mismatch_dbs.len(),
            labels_for_refs(&dim_mismatch_dbs)
        ),
    );
    let enrichment_failed_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::has_enrichment_failures(db))
        .collect::<Vec<_>>();
    let enrichment_failed_total: usize = dbs.iter().map(|db| db.enrichment_failed_recent).sum();
    push_count_deduction(
        &mut deductions,
        "enrichment_failures",
        "enrichment failures",
        ((enrichment_failed_dbs.len() * 5) + (enrichment_failed_total / 10)).min(20),
        format!(
            "{} recent enrichment failure marker(s) across {}",
            enrichment_failed_total,
            labels_for_refs(&enrichment_failed_dbs)
        ),
    );
    let vector_orphans: usize = dbs.iter().map(|db| db.vector_orphans).sum();
    push_count_deduction(
        &mut deductions,
        "vector_orphans",
        "orphan vector rows",
        (vector_orphans * 3).min(10),
        format!(
            "{} orphan vector row(s) across {}",
            vector_orphans,
            labels_for(dbs, |db| db.vector_orphans > 0)
        ),
    );
    if distill_marker.map(|m| m.is_stale).unwrap_or(true) {
        push_deduction(
            &mut deductions,
            "stale_distill_marker",
            "stale distill marker",
            10,
            distill_marker
                .map(|marker| {
                    if let Some(reason) = &marker.error_reason {
                        format!("last distill failed: {reason} ({})", marker.age)
                    } else {
                        format!("last distill marker is stale: {}", marker.age)
                    }
                })
                .unwrap_or_else(|| "no distill marker found".to_string()),
        );
    }
    // Hard errors inside the last distill batch are not foundry_jobs rows, so unlike
    // agent-evolution failures (which dead-letter) they would otherwise never reach
    // the health score. `fallback_used`/`groups_skipped` are graceful degradation,
    // not failure, so they stay informational (surfaced in status, not scored).
    if let Some(distill_errors) = distill_marker.and_then(|m| m.errors) {
        push_count_deduction(
            &mut deductions,
            "distill_errors",
            "distill errors",
            (distill_errors * 3).min(10),
            format!("{distill_errors} hard error(s) in the last distill marker"),
        );
    }
    let missing_required_keys = api_keys
        .iter()
        .filter(|key| key.required && key.status == "missing")
        .collect::<Vec<_>>();
    push_count_deduction(
        &mut deductions,
        "missing_required_api_keys",
        "missing required API keys",
        (missing_required_keys.len() * 10).min(20),
        format!(
            "{} required API key(s) missing: {}",
            missing_required_keys.len(),
            missing_required_keys
                .iter()
                .map(|key| key.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    // Vault+env duplicate keys are informational (see api_keys.drift), not health defects.
    let inferred_invalid_keys = api_keys
        .iter()
        .filter(|key| key.inferred_invalid_provider.is_some())
        .collect::<Vec<_>>();
    push_count_deduction(
        &mut deductions,
        "inferred_invalid_provider_keys",
        "inferred invalid provider keys",
        (inferred_invalid_keys.len() * 10).min(20),
        format!(
            "{} API key(s) inferred invalid from recent failures: {}",
            inferred_invalid_keys.len(),
            inferred_invalid_keys
                .iter()
                .map(|key| key.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    // Provider probe failures indicate misconfigured or expired API keys.
    if let Some(probes) = probe_results {
        let failed_probes = probes.iter().filter(|p| p.status != "ok").count();
        push_count_deduction(
            &mut deductions,
            "provider_probe_failures",
            "provider probe failures",
            (failed_probes * 8).min(24),
            format!("{failed_probes} provider probe(s) failed"),
        );
    }
    if let Some(groups) = rotation_group_results {
        let rate_limited_keys: i64 = groups.iter().map(|group| group.rate_limited_keys).sum();
        let auth_failed_keys: i64 = groups.iter().map(|group| group.auth_failed_keys).sum();
        push_count_deduction(
            &mut deductions,
            "provider_rate_limited_keys",
            "provider rate-limited keys",
            ((rate_limited_keys as usize) * 4).min(16),
            format!("{rate_limited_keys} provider rotation key(s) are rate-limited"),
        );
        push_count_deduction(
            &mut deductions,
            "provider_auth_failed_keys",
            "provider auth-failed keys",
            ((auth_failed_keys as usize) * 8).min(24),
            format!("{auth_failed_keys} provider rotation key(s) failed auth"),
        );
    }
    deductions
}

fn push_count_deduction(
    deductions: &mut Vec<HealthDeduction>,
    code: &str,
    label: &str,
    points: usize,
    detail: String,
) {
    if points == 0 {
        return;
    }
    push_deduction(deductions, code, label, points.min(100) as u8, detail);
}

fn push_deduction(
    deductions: &mut Vec<HealthDeduction>,
    code: &str,
    label: &str,
    points: u8,
    detail: impl Into<String>,
) {
    if points == 0 {
        return;
    }
    deductions.push(HealthDeduction {
        code: code.to_string(),
        label: label.to_string(),
        points,
        detail: detail.into(),
    });
}

fn labels_for(dbs: &[DbStatus], predicate: impl Fn(&DbStatus) -> bool) -> String {
    let labels = dbs
        .iter()
        .filter(|db| predicate(db))
        .map(|db| db.label.as_str())
        .collect::<Vec<_>>();
    if labels.is_empty() {
        "(none)".to_string()
    } else {
        labels.join(", ")
    }
}

fn labels_for_refs(dbs: &[&DbStatus]) -> String {
    if dbs.is_empty() {
        "(none)".to_string()
    } else {
        dbs.iter()
            .map(|db| db.label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}
