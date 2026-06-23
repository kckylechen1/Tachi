use super::*;

fn db_status(label: &str, failed: usize, stuck: usize, coverage: f64) -> DbStatus {
    DbStatus {
        path: format!("/tmp/{label}.db"),
        label: label.to_string(),
        orphan: false,
        memory_total: 100,
        vector_count: (100.0 * coverage) as usize,
        vector_missing: 0,
        vector_orphans: 0,
        vector_coverage: coverage,
        vector_dimension: Some(EXPECTED_EMBEDDING_DIM),
        namespace: NamespaceHealth::default(),
        continuity: memory_core::ContinuityMetrics::default(),
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
        health_score: 95,
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
fn build_status_warnings_lists_db_names_for_low_coverage() {
    let snapshot = empty_snapshot(vec![
        db_status("global", 0, 0, 0.95),
        db_status("sigil", 0, 0, 0.42),
        db_status("hyperion", 0, 0, 0.81),
    ]);
    let warnings = build_status_warnings(&snapshot, &daemon_running());
    let low_cov = warnings
        .iter()
        .find(|w| w.contains("vector coverage below 90%"))
        .expect("low-coverage warning present");
    assert!(
        low_cov.contains("sigil") && low_cov.contains("hyperion"),
        "low-coverage warning should name the affected dbs, got: {low_cov}"
    );
    assert!(
        !low_cov.contains("global"),
        "healthy dbs must not appear in low-coverage warning, got: {low_cov}"
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
