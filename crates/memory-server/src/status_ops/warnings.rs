//! Warning synthesis for `tachi status` / `tachi_memory action=alerts`: fold the
//! per-DB health, daemon state, and provider-key snapshot into the operator
//! warning lines, plus the small DbStatus predicates they key off. Extracted
//! from `status_ops::mod` (no behavior change); shared types come via the parent.

use super::*;

/// Lightweight warning lines for `tachi_memory action=alerts` — no provider keys, models, or skill matrices.
pub(crate) async fn collect_agent_warning_lines(server: &crate::MemoryServer) -> Vec<String> {
    let global_db = server.global_db_path_buf();
    let project_db = server.project_db_path_buf();
    tokio::task::spawn_blocking(move || {
        let app_home = resolve_app_home();
        let snapshot = collect_snapshot(&app_home, &global_db, project_db.as_deref());
        let daemon_state = match &snapshot.daemon {
            DaemonStatus::Running { .. } => json!({ "running": true }),
            DaemonStatus::Foreign { reason, .. } => {
                json!({ "running": false, "foreign": true, "reason": reason })
            }
            DaemonStatus::StalePid { .. } => json!({ "running": false, "stale": true }),
            DaemonStatus::None => json!({ "running": false }),
        };
        build_status_warnings(&snapshot, &daemon_state)
    })
    .await
    .unwrap_or_default()
}

pub(crate) fn build_status_warnings(
    snapshot: &StatusSnapshot,
    daemon_state: &serde_json::Value,
) -> Vec<String> {
    let total_dbs = snapshot.dbs.len();
    let total_failed: usize = snapshot.dbs.iter().map(|d| d.failed).sum();
    let total_stuck: usize = snapshot.dbs.iter().map(|d| d.stuck_in_progress).sum();
    let low_coverage_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| low_vector_coverage(d))
        .map(|d| d.label.as_str())
        .collect();
    let low_coverage_count = low_coverage_dbs.len();
    let vector_dimension_mismatch_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| vector_dimension_mismatch(d))
        .map(|d| d.label.as_str())
        .collect();
    let vector_dimension_mismatch_count = vector_dimension_mismatch_dbs.len();
    let vector_orphan_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| has_vector_orphans(d))
        .map(|d| d.label.as_str())
        .collect();
    let vector_orphan_count: usize = snapshot.dbs.iter().map(|d| d.vector_orphans).sum();
    let enrichment_failed_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| has_enrichment_failures(d))
        .map(|d| d.label.as_str())
        .collect();
    let enrichment_failed_count: usize = snapshot
        .dbs
        .iter()
        .map(|d| d.enrichment_failed_recent)
        .sum();
    let auth_failure_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| {
            d.latest_failed_job
                .as_ref()
                .and_then(|job| job.inferred_invalid_provider.as_ref())
                .is_some()
        })
        .map(|d| d.label.as_str())
        .collect();
    let auth_failures = auth_failure_dbs.len();

    let mut warnings: Vec<String> = Vec::new();
    if daemon_state["foreign"].as_bool().unwrap_or(false) {
        let reason = daemon_state["reason"].as_str().unwrap_or("scope mismatch");
        warnings.push(format!(
            "foreign daemon detected ({reason}); Tachi background tasks for this DB are paused"
        ));
    } else if !daemon_state["running"].as_bool().unwrap_or(false) {
        warnings.push(
            "daemon not running — background tasks (enrichment, distill, GC) are paused"
                .to_string(),
        );
    }
    warnings.extend(snapshot.project_warnings.iter().cloned());
    for issue in &snapshot.plan_c_split_brain {
        warnings.push(issue.warning_message());
    }
    if low_coverage_count > 0 {
        warnings.push(format!(
            "{low_coverage_count} db(s) have vector coverage below 90%: {}",
            low_coverage_dbs.join(", ")
        ));
    }
    if vector_dimension_mismatch_count > 0 {
        warnings.push(format!(
            "{vector_dimension_mismatch_count} db(s) have vector dimension metadata that differs from expected {EXPECTED_EMBEDDING_DIM}: {}",
            vector_dimension_mismatch_dbs.join(", ")
        ));
    }
    if vector_orphan_count > 0 {
        warnings.push(format!(
            "{vector_orphan_count} orphan vector row(s) found in {}",
            vector_orphan_dbs.join(", ")
        ));
    }
    if enrichment_failed_count > 0 {
        warnings.push(format!(
            "{enrichment_failed_count} memory enrichment failure(s) remain in {}",
            enrichment_failed_dbs.join(", ")
        ));
    }
    let recall_cache_rows: usize = snapshot
        .dbs
        .iter()
        .map(|d| d.namespace.recall_cache_rows)
        .sum();
    if recall_cache_rows > 0 {
        let affected: Vec<&str> = snapshot
            .dbs
            .iter()
            .filter(|d| d.namespace.recall_cache_rows > 0)
            .map(|d| d.label.as_str())
            .collect();
        warnings.push(format!(
            "{recall_cache_rows} recall-cache row(s) remain in durable memories across: {}",
            affected.join(", ")
        ));
    }
    let wiki_non_source_rows: usize = snapshot
        .dbs
        .iter()
        .map(|d| d.namespace.wiki_non_source_rows)
        .sum();
    if wiki_non_source_rows > 0 {
        let affected: Vec<&str> = snapshot
            .dbs
            .iter()
            .filter(|d| d.namespace.wiki_non_source_rows > 0)
            .map(|d| d.label.as_str())
            .collect();
        warnings.push(format!(
            "{wiki_non_source_rows} untagged wiki row(s) (no domain, non-wiki source) in: {}",
            affected.join(", ")
        ));
    }
    let graph_orphan_edges: usize = snapshot
        .dbs
        .iter()
        .map(|d| d.namespace.graph_orphan_edges)
        .sum();
    if graph_orphan_edges > 0 {
        let affected: Vec<&str> = snapshot
            .dbs
            .iter()
            .filter(|d| d.namespace.graph_orphan_edges > 0)
            .map(|d| d.label.as_str())
            .collect();
        warnings.push(format!(
            "{graph_orphan_edges} memory graph orphan edge(s) found in: {}",
            affected.join(", ")
        ));
    }
    // derived_items is populated by consolidation/distill, which most DBs never
    // run — so empty is normal for small/inactive DBs and flagging all of them
    // is pure noise. Only warn for substantial DBs where empty derivation is
    // genuinely surprising and worth investigating.
    const DERIVED_ITEMS_EXPECTED_MIN_ROWS: usize = 1000;
    let derived_empty_dbs: Vec<&str> = snapshot
        .dbs
        .iter()
        .filter(|d| {
            d.memory_total >= DERIVED_ITEMS_EXPECTED_MIN_ROWS && d.namespace.derived_items == 0
        })
        .map(|d| d.label.as_str())
        .collect();
    if !derived_empty_dbs.is_empty() {
        warnings.push(format!(
            "derived_items is empty in {} large active db(s) (>={DERIVED_ITEMS_EXPECTED_MIN_ROWS} rows): {}",
            derived_empty_dbs.len(),
            derived_empty_dbs.join(", ")
        ));
    }
    if total_failed > 0 {
        let failed_dbs: Vec<&str> = snapshot
            .dbs
            .iter()
            .filter(|d| d.failed > 0)
            .map(|d| d.label.as_str())
            .collect();
        let db_list = if failed_dbs.is_empty() {
            format!("across {total_dbs} db(s)")
        } else {
            format!("[{}] across {} db(s)", failed_dbs.join(", "), total_dbs)
        };
        warnings.push(format!("{total_failed} foundry job(s) failed {db_list}"));
    }
    if auth_failures > 0 {
        warnings.push(format!(
            "latest foundry failures include provider auth/API-key errors in: {}",
            auth_failure_dbs.join(", ")
        ));
    }
    if let Some(cache) = &snapshot.provider_probe_cache {
        if cache.is_stale() {
            warnings.push(format!(
                "provider key probe cache is older than {}h; run `tachi status --probe-keys` or wait for the daily pipeline",
                cache.ttl_seconds / 3600
            ));
        } else {
            for probe in cache.probes.iter().filter(|probe| probe.status != "ok") {
                let message = probe
                    .message
                    .as_deref()
                    .map(truncate_probe_warning)
                    .unwrap_or_else(|| "no detail".to_string());
                warnings.push(format!(
                    "provider probe {} is {} ({message})",
                    probe.name, probe.status
                ));
            }
            for group in &cache.rotation_groups {
                if group.auth_failed_keys > 0 {
                    let failed = group
                        .keys
                        .iter()
                        .filter(|key| key.status == "auth_failed")
                        .map(|key| key.name.as_str())
                        .collect::<Vec<_>>();
                    warnings.push(format!(
                        "provider rotation group {} has {} auth-failed key(s): {}",
                        group.logical_name,
                        group.auth_failed_keys,
                        failed.join(", ")
                    ));
                }
                if group.rate_limited_keys >= group.configured_keys && group.configured_keys > 0 {
                    warnings.push(format!(
                        "provider rotation group {} has all {} configured key(s) rate-limited",
                        group.logical_name, group.configured_keys
                    ));
                }
            }
        }
    }
    if total_stuck > 0 {
        let stuck_dbs: Vec<&str> = snapshot
            .dbs
            .iter()
            .filter(|d| d.stuck_in_progress > 0)
            .map(|d| d.label.as_str())
            .collect();
        let db_list = if stuck_dbs.is_empty() {
            String::new()
        } else {
            format!(" — affected: {}", stuck_dbs.join(", "))
        };
        warnings.push(format!(
            "{total_stuck} foundry job(s) stuck running for over {STUCK_THRESHOLD_SECS}s{db_list}"
        ));
    }
    if snapshot
        .distill_marker
        .as_ref()
        .map(|marker| marker.is_stale)
        .unwrap_or(true)
    {
        warnings.push("daily distill marker is missing or stale".to_string());
    }
    for key in &snapshot.api_keys {
        if key.required && key.status == "missing" {
            warnings.push(format!(
                "required provider key {} is missing (add to Tachi Vault or config env)",
                key.name
            ));
        }
        if let Some(provider) = &key.inferred_invalid_provider {
            warnings.push(format!(
                "provider key {} appears invalid for provider {} based on latest failed jobs",
                key.name, provider
            ));
        }
    }
    warnings
}

fn truncate_probe_warning(message: &str) -> String {
    truncate(message, 120)
}

pub(crate) fn vector_dimension_mismatch(db: &DbStatus) -> bool {
    db.vector_count > 0 && db.vector_dimension != Some(EXPECTED_EMBEDDING_DIM)
}

/// Coverage threshold below which a populated DB is flagged as having low
/// vector coverage. Single source of truth for the hand-copied `0.9` literal
/// that previously lived inline in health scoring, status warnings, and the CLI.
pub(crate) const LOW_VECTOR_COVERAGE: f64 = 0.9;

pub(crate) fn low_vector_coverage(db: &DbStatus) -> bool {
    db.memory_total > 0 && db.vector_coverage < LOW_VECTOR_COVERAGE
}

pub(crate) fn has_vector_orphans(db: &DbStatus) -> bool {
    db.vector_orphans > 0
}

pub(crate) fn has_enrichment_failures(db: &DbStatus) -> bool {
    db.enrichment_failed_recent > 0
}
