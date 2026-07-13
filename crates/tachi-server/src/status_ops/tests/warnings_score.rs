use super::*;

fn db_status(label: &str, failed: usize, stuck: usize, coverage: f64) -> DbStatus {
    let memory_total = 100;
    let vector_count = (memory_total as f64 * coverage) as usize;
    DbStatus {
        path: format!("/tmp/{label}.db"),
        label: label.to_string(),
        orphan: false,
        memory_total,
        vector_count,
        vector_missing: memory_total.saturating_sub(vector_count),
        vector_orphans: 0,
        vector_coverage: coverage,
        vector_dimension: Some(EXPECTED_EMBEDDING_DIM),
        vector_sweep: None,
        vector_sweep_error: None,
        namespace: NamespaceHealth::default(),
        continuity: memcore::ContinuityMetrics::default(),
        pending_enrichment: 0,
        enrichment_failed_recent: 0,
        enrichment_failures: Vec::new(),
        pending: 0,
        running: 0,
        active_jobs: 0,
        completed: 0,
        failed,
        dead_lettered: 0,
        skipped: 0,
        terminal_jobs: failed,
        gc_eligible: 0,
        stuck_in_progress: stuck,
        latest_active_job: None,
        latest_terminal_job: None,
        latest_job: None,
        latest_failed_job: None,
        error: None,
    }
}

fn empty_snapshot(dbs: Vec<DbStatus>) -> StatusSnapshot {
    StatusSnapshot {
        daemon: DaemonStatus::None,
        daemon_inventory: Vec::new(),
        dbs,
        manifest_path: String::new(),
        dispatches: Vec::new(),
        recent_evals: Vec::new(),
        last_daily_report: None,
        distill_marker: None,
        api_keys: Vec::new(),
        provider_probe_cache: None,
        project_warnings: Vec::new(),
        plan_c_split_brain: Vec::new(),
        health_deductions: Vec::new(),
        health_score: 95,
        disk: empty_disk_status(),
    }
}

fn empty_disk_status() -> disk::DiskStatus {
    let volume = disk::DiskVolumeStatus {
        label: "test",
        path: String::new(),
        free_bytes: None,
        total_bytes: None,
        free_percent: None,
        warning: None,
        error: Some("not probed in this fixture".to_string()),
    };
    disk::DiskStatus {
        worktrees_root: volume.clone(),
        shared_target_dir: volume,
        top_consumers: Vec::new(),
    }
}

fn fresh_distill_marker() -> DistillMarkerStatus {
    DistillMarkerStatus {
        path: "/tmp/marker".to_string(),
        last_run_at: "2026-06-09T00:00:00Z".to_string(),
        age_seconds: 0,
        age: "0s ago".to_string(),
        is_stale: false,
        groups_distilled: Some(4),
        groups_skipped: Some(0),
        fallback_used: Some(0),
        errors: Some(0),
        error_reason: None,
    }
}

#[test]
fn build_status_warnings_names_foreign_daemon() {
    let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0)]);
    let daemon_state = serde_json::json!({
        "running": false,
        "foreign": true,
        "reason": "daemon global_db /tmp/openclaw.db does not match /tmp/tachi.db",
    });

    let warnings = build_status_warnings(&snapshot, &daemon_state);

    assert!(
        warnings
            .iter()
            .any(|w| w.contains("foreign daemon detected") && w.contains("openclaw.db")),
        "foreign daemon warning should name the mismatch, got: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .all(|w| !w.starts_with("daemon not running")),
        "foreign daemon should not be reported as a generic missing daemon: {warnings:?}"
    );
}

#[test]
fn build_status_warnings_emits_bounded_action_for_needed_backfill() {
    let snapshot = empty_snapshot(vec![
        db_status("global", 0, 0, 1.0),
        db_status("sigil", 0, 0, 0.98),
        db_status("hyperion", 0, 0, 0.81),
    ]);
    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let low_cov = warnings
        .iter()
        .find(|w| w.starts_with("WARNING: vector backfill needed"))
        .expect("backfill warning present");
    assert!(
        low_cov.contains("sigil") && low_cov.contains("hyperion"),
        "backfill warning should name the affected dbs, got: {low_cov}"
    );
    assert!(
        !low_cov.contains("global"),
        "healthy dbs must not appear in backfill warning, got: {low_cov}"
    );
    assert!(
        low_cov.contains("missing=2")
            && low_cov.contains("missing=19")
            && low_cov.contains("tachi backfill-vectors"),
        "backfill warning should include missing counts and action, got: {low_cov}"
    );
    assert!(
        low_cov.len() <= 240,
        "backfill warning must stay size-bounded, got {} chars: {low_cov}",
        low_cov.len()
    );
}

#[test]
fn build_status_warnings_includes_project_warnings() {
    let mut snapshot = empty_snapshot(vec![]);
    snapshot.project_warnings = vec![
        "/repo/.tachi/env.generated is tracked by git and may contain plaintext secrets"
            .to_string(),
    ];

    let warnings = build_status_warnings(&snapshot, &daemon_running());

    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains(".tachi/env.generated")),
        "project warning should be surfaced in status warnings, got: {warnings:?}"
    );
}

#[test]
fn build_status_warnings_lists_db_names_for_failed_jobs() {
    let mut sigil = db_status("sigil", 3, 0, 0.95);
    sigil.latest_failed_job = Some(LatestFailedJob {
        id: "job-1".to_string(),
        kind: "enrich".to_string(),
        lane: None,
        updated_at: None,
        reason: Some("401 invalid api key".to_string()),
        inferred_invalid_provider: Some("openai".to_string()),
    });
    let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 0.95), sigil]);
    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let failed = warnings
        .iter()
        .find(|w| w.contains("foundry job(s) failed"))
        .expect("failed-jobs warning present");
    assert!(
        failed.contains("sigil"),
        "failed-jobs warning should name the affected dbs, got: {failed}"
    );
    let auth = warnings
        .iter()
        .find(|w| w.contains("auth/API-key errors"))
        .expect("auth-failure warning present");
    assert!(
        auth.contains("sigil"),
        "auth-failure warning should name the affected dbs, got: {auth}"
    );
}

#[test]
fn build_status_warnings_lists_db_names_for_stuck_jobs() {
    let snapshot = empty_snapshot(vec![
        db_status("global", 0, 0, 0.95),
        db_status("sigil", 0, 2, 0.95),
    ]);
    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let stuck = warnings
        .iter()
        .find(|w| w.contains("stuck running"))
        .expect("stuck-jobs warning present");
    assert!(
        stuck.contains("sigil"),
        "stuck-jobs warning should name the affected dbs, got: {stuck}"
    );
}

#[test]
fn build_status_warnings_lists_enrichment_failures_and_vector_orphans() {
    let mut hyperion = db_status("hyperion", 0, 0, 1.0);
    hyperion.enrichment_failed_recent = 42;
    let mut sigil = db_status("sigil", 0, 0, 1.0);
    sigil.vector_orphans = 2;
    let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0), hyperion, sigil]);

    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let enrichment = warnings
        .iter()
        .find(|w| w.contains("memory enrichment failure"))
        .expect("enrichment-failure warning present");
    assert!(
        enrichment.contains("hyperion") && enrichment.contains("42"),
        "enrichment warning should name affected db and count, got: {enrichment}"
    );
    let orphans = warnings
        .iter()
        .find(|w| w.contains("orphan vector row"))
        .expect("vector-orphan warning present");
    assert!(
        orphans.contains("sigil") && orphans.contains("2"),
        "vector orphan warning should name affected db and count, got: {orphans}"
    );
}

#[test]
fn health_score_drops_for_background_enrichment_failures_and_orphans() {
    let mut hyperion = db_status("hyperion", 0, 0, 1.0);
    hyperion.enrichment_failed_recent = 42;
    let mut sigil = db_status("sigil", 0, 0, 1.0);
    sigil.vector_orphans = 1;
    let dbs = vec![hyperion, sigil];
    let score = status_health::calculate_health_score(
        &DaemonStatus::Running {
            pid: 1,
            lock_path: PathBuf::from("/tmp/tachi.lock"),
        },
        &dbs,
        Some(&DistillMarkerStatus {
            path: "/tmp/marker".to_string(),
            last_run_at: "2026-06-09T00:00:00Z".to_string(),
            age_seconds: 0,
            age: "0s ago".to_string(),
            is_stale: false,
            groups_distilled: Some(4),
            groups_skipped: Some(0),
            fallback_used: Some(0),
            errors: Some(0),
            error_reason: None,
        }),
        &[],
        Some(&[]),
        Some(&[]),
    );

    assert!(
        score < 100,
        "background failures/orphans must prevent perfect health score"
    );
}

#[test]
fn health_deductions_do_not_penalize_missing_daemon() {
    let deductions = status_health::calculate_health_deductions(
        &DaemonStatus::None,
        &[db_status("global", 0, 0, 1.0)],
        Some(&fresh_distill_marker()),
        &[],
        Some(&[]),
        Some(&[]),
    );

    assert_eq!(
        status_health::health_score_from_deductions(&deductions),
        100
    );
    assert!(
        deductions
            .iter()
            .all(|deduction| deduction.code != "daemon_not_running"),
        "missing daemon is a runtime mode, not a health failure: {deductions:?}"
    );
}

#[test]
fn health_deductions_explain_enrichment_score_loss_without_daemon_noise() {
    let mut quant = db_status("quant", 0, 0, 1.0);
    quant.enrichment_failed_recent = 1;
    quant.enrichment_failures = vec![EnrichmentFailureSummary {
        stage: "embedding".to_string(),
        last_error: "Missing API key".to_string(),
        count: 1,
    }];
    let marker = fresh_distill_marker();

    let deductions = status_health::calculate_health_deductions(
        &DaemonStatus::None,
        &[quant],
        Some(&marker),
        &[],
        Some(&[]),
        Some(&[]),
    );
    let score = status_health::health_score_from_deductions(&deductions);

    assert_eq!(score, 95);
    assert!(deductions
        .iter()
        .all(|deduction| { deduction.code != "daemon_not_running" }));
    assert!(deductions.iter().any(|deduction| {
        deduction.code == "enrichment_failures"
            && deduction.points == 5
            && deduction.detail.contains("quant")
    }));
}

fn stale_distill_marker_with_error(reason: &str) -> DistillMarkerStatus {
    DistillMarkerStatus {
        path: "/tmp/marker".to_string(),
        last_run_at: "2026-06-01T00:00:00Z".to_string(),
        age_seconds: 999_999,
        age: "11d ago".to_string(),
        is_stale: true,
        groups_distilled: Some(0),
        groups_skipped: Some(0),
        fallback_used: Some(0),
        errors: Some(0),
        error_reason: Some(reason.to_string()),
    }
}

fn stale_distill_marker_no_error() -> DistillMarkerStatus {
    stale_distill_marker_with_error("dummy").map_error_reason_to_none()
}

// Small helper to null out error_reason without repeating the whole struct.
impl DistillMarkerStatus {
    fn map_error_reason_to_none(self) -> Self {
        Self {
            error_reason: None,
            ..self
        }
    }
}

#[test]
fn distill_failure_marker_shows_error_in_warning() {
    let mut snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0)]);
    snapshot.distill_marker = Some(stale_distill_marker_with_error(
        "provider timeout after 30s",
    ));

    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let distill_warning = warnings
        .iter()
        .find(|w| w.contains("daily distill"))
        .expect("distill warning should be present");

    assert!(
        distill_warning.contains("failed"),
        "failure marker should say 'failed', got: {distill_warning}"
    );
    assert!(
        distill_warning.contains("provider timeout after 30s"),
        "failure marker should include the error reason, got: {distill_warning}"
    );
    assert!(
        distill_warning.contains("tachi doctor --run-daily"),
        "failure marker should include remediation verb, got: {distill_warning}"
    );
}

#[test]
fn distill_stale_marker_without_error_shows_remediation_hint() {
    let mut snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0)]);
    snapshot.distill_marker = Some(stale_distill_marker_no_error());

    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let distill_warning = warnings
        .iter()
        .find(|w| w.contains("daily distill"))
        .expect("distill warning should be present");

    assert!(
        !distill_warning.contains("failed"),
        "stale-without-error should not say 'failed', got: {distill_warning}"
    );
    assert!(
        distill_warning.contains("stale"),
        "stale marker should say 'stale', got: {distill_warning}"
    );
    assert!(
        distill_warning.contains("tachi doctor --run-daily"),
        "stale marker should include remediation verb, got: {distill_warning}"
    );
}

#[test]
fn missing_distill_marker_shows_never_run() {
    let snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0)]);
    // distill_marker is None by default in empty_snapshot

    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let distill_warning = warnings
        .iter()
        .find(|w| w.contains("daily distill"))
        .expect("distill warning should be present for missing marker");

    assert!(
        distill_warning.contains("never run"),
        "missing marker should say 'never run', got: {distill_warning}"
    );
    assert!(
        distill_warning.contains("tachi doctor --run-daily"),
        "missing marker should include remediation verb, got: {distill_warning}"
    );
}

#[test]
fn fresh_failure_marker_still_warns_and_docks_health() {
    // A just-failed distill writes ts=now, so is_stale=false. Without the
    // error_reason branch the failure would be invisible for 36h. This test
    // proves the fresh failure surfaces in both warnings and health deductions.
    let fresh_failure = DistillMarkerStatus {
        path: "/tmp/marker".to_string(),
        last_run_at: "2026-07-09T00:00:00Z".to_string(),
        age_seconds: 5,
        age: "5s ago".to_string(),
        is_stale: false, // fresh — the bug was that this hid the failure
        groups_distilled: Some(0),
        groups_skipped: Some(0),
        fallback_used: Some(0),
        errors: Some(0),
        error_reason: Some("provider timeout".to_string()),
    };

    // Warning must surface even though is_stale is false.
    let mut snapshot = empty_snapshot(vec![db_status("global", 0, 0, 1.0)]);
    snapshot.distill_marker = Some(fresh_failure.clone());
    let warnings = build_status_warnings(&snapshot, &daemon_running());
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("daily distill failed") && w.contains("provider timeout")),
        "fresh failure marker must still warn, got: {warnings:?}"
    );

    // Health deduction must fire even though is_stale is false.
    let deductions = status_health::calculate_health_deductions(
        &DaemonStatus::Running {
            pid: 1,
            lock_path: PathBuf::from("/tmp/tachi.lock"),
        },
        &[db_status("global", 0, 0, 1.0)],
        Some(&fresh_failure),
        &[],
        Some(&[]),
        Some(&[]),
    );
    assert!(
        deductions.iter().any(|d| d.code == "stale_distill_marker"),
        "fresh failure marker must dock health score, got deductions: {deductions:?}"
    );
}
