use super::types::{ProviderProbeResult, ProviderRotationGroupProbe};
use crate::status_ops::{ApiKeyStatus, DbStatus};

pub(crate) fn calculate_health_score(
    daemon: &crate::status_ops::DaemonStatus,
    dbs: &[DbStatus],
    distill_marker: Option<&crate::status_ops::DistillMarkerStatus>,
    api_keys: &[ApiKeyStatus],
    probe_results: Option<&[ProviderProbeResult]>,
    rotation_group_results: Option<&[ProviderRotationGroupProbe]>,
) -> u8 {
    let mut score = 100i32;
    if !matches!(daemon, crate::status_ops::DaemonStatus::Running { .. }) {
        score -= 20;
    }
    // Only dead-lettered (retry-exhausted) jobs count as genuine, current
    // failures. Transient failures are auto-retried with backoff and self-heal,
    // so they no longer drag the score down for their full GC-retention window.
    let failed_jobs: usize = dbs.iter().map(|db| db.dead_lettered).sum();
    score -= (failed_jobs as i32).min(25);
    let stuck_jobs: usize = dbs.iter().map(|db| db.stuck_in_progress).sum();
    score -= ((stuck_jobs as i32) * 5).min(20);
    let low_vector_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::low_vector_coverage(db))
        .count();
    score -= ((low_vector_dbs as i32) * 10).min(25);
    let dim_mismatch_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::vector_dimension_mismatch(db))
        .count();
    score -= ((dim_mismatch_dbs as i32) * 15).min(30);
    let enrichment_failed_dbs = dbs
        .iter()
        .filter(|db| crate::status_ops::has_enrichment_failures(db))
        .count();
    let enrichment_failed_total: usize = dbs.iter().map(|db| db.enrichment_failed_recent).sum();
    score -= (((enrichment_failed_dbs as i32) * 5) + (enrichment_failed_total as i32 / 10)).min(20);
    let vector_orphans: usize = dbs.iter().map(|db| db.vector_orphans).sum();
    score -= ((vector_orphans as i32) * 3).min(10);
    if distill_marker.map(|m| m.is_stale).unwrap_or(true) {
        score -= 10;
    }
    // Hard errors inside the last distill batch are not foundry_jobs rows, so unlike
    // agent-evolution failures (which dead-letter) they would otherwise never reach
    // the health score. `fallback_used`/`groups_skipped` are graceful degradation,
    // not failure, so they stay informational (surfaced in status, not scored).
    if let Some(distill_errors) = distill_marker.and_then(|m| m.errors) {
        score -= ((distill_errors as i32) * 3).min(10);
    }
    let missing_required_keys = api_keys
        .iter()
        .filter(|key| key.required && key.status == "missing")
        .count();
    score -= ((missing_required_keys as i32) * 10).min(20);
    // Vault+env duplicate keys are informational (see api_keys.drift), not health defects.
    let inferred_invalid_keys = api_keys
        .iter()
        .filter(|key| key.inferred_invalid_provider.is_some())
        .count();
    score -= ((inferred_invalid_keys as i32) * 10).min(20);
    // Provider probe failures indicate misconfigured or expired API keys.
    if let Some(probes) = probe_results {
        let failed_probes = probes.iter().filter(|p| p.status != "ok").count();
        score -= ((failed_probes as i32) * 8).min(24);
    }
    if let Some(groups) = rotation_group_results {
        let rate_limited_keys: i64 = groups.iter().map(|group| group.rate_limited_keys).sum();
        let auth_failed_keys: i64 = groups.iter().map(|group| group.auth_failed_keys).sum();
        score -= ((rate_limited_keys as i32) * 4).min(16);
        score -= ((auth_failed_keys as i32) * 8).min(24);
    }
    score.clamp(0, 100) as u8
}
