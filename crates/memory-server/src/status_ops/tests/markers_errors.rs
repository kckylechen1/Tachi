use super::*;

#[test]
fn truncate_honors_max() {
    assert_eq!(truncate("abc", 5), "abc");
    assert_eq!(truncate("abcdefghij", 5), "ab...");
}

#[test]
fn read_distill_marker_parses_json_quality_summary() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let runs = app_home.path().join("foundry-runs");
    std::fs::create_dir_all(&runs).expect("runs dir");
    std::fs::write(
        runs.join(".last_distill_run"),
        serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "groups_distilled": 7,
            "groups_skipped": 2,
            "fallback_used": 1,
            "errors": 3,
        })
        .to_string(),
    )
    .expect("write marker");

    let marker = read_distill_marker(app_home.path()).expect("marker parsed");
    assert_eq!(marker.groups_distilled, Some(7));
    assert_eq!(marker.groups_skipped, Some(2));
    assert_eq!(marker.fallback_used, Some(1));
    assert_eq!(marker.errors, Some(3));
    assert!(!marker.is_stale, "fresh marker must not be stale");
}

#[test]
fn read_distill_marker_accepts_legacy_bare_timestamp() {
    let app_home = tempfile::tempdir().expect("temp app home");
    let runs = app_home.path().join("foundry-runs");
    std::fs::create_dir_all(&runs).expect("runs dir");
    // Pre-JSON markers were a bare RFC3339 string; they must still parse with
    // every quality field left None (not be mistaken for a JSON document).
    std::fs::write(runs.join(".last_distill_run"), Utc::now().to_rfc3339())
        .expect("write legacy marker");

    let marker = read_distill_marker(app_home.path()).expect("legacy marker parsed");
    assert_eq!(marker.groups_distilled, None);
    assert_eq!(marker.errors, None);
    assert!(!marker.is_stale);
}

#[test]
fn distill_hard_errors_dock_health_but_fallbacks_do_not() {
    let base = DistillMarkerStatus {
        path: "/tmp/marker".to_string(),
        last_run_at: "2026-06-20T00:00:00Z".to_string(),
        age_seconds: 0,
        age: "0s ago".to_string(),
        is_stale: false,
        groups_distilled: Some(5),
        groups_skipped: Some(4),
        fallback_used: Some(9),
        errors: Some(0),
    };
    let with_errors = DistillMarkerStatus {
        errors: Some(2),
        ..base.clone()
    };
    let daemon = DaemonStatus::Running {
        pid: 1,
        lock_path: PathBuf::from("/tmp/tachi.lock"),
    };
    let healthy =
        status_health::calculate_health_score(&daemon, &[], Some(&base), &[], Some(&[]), Some(&[]));
    let errored = status_health::calculate_health_score(
        &daemon,
        &[],
        Some(&with_errors),
        &[],
        Some(&[]),
        Some(&[]),
    );
    // High fallback/skip counts alone keep a perfect score (graceful degradation).
    assert_eq!(
        healthy, 100,
        "fallback_used/groups_skipped must not be scored"
    );
    assert!(
        errored < healthy,
        "hard distill errors must dock the health score (got {errored} vs {healthy})"
    );
}

#[test]
fn infer_provider_from_auth_error_maps_real_failures() {
    assert_eq!(
        status_health::infer_provider_from_failed_job(
            "recall_rerank_cache",
            Some("rerank"),
            "SiliconFlow 403 forbidden"
        ),
        Some("SILICONFLOW".to_string())
    );
    assert_eq!(
        status_health::infer_provider_from_auth_error("Voyage API error: 403 Forbidden"),
        Some("VOYAGE".to_string())
    );
    assert_eq!(
        status_health::infer_provider_from_failed_job(
            "memory_distill",
            Some("distill"),
            "403 Forbidden"
        ),
        Some("SILICONFLOW".to_string())
    );
    assert_eq!(
        status_health::infer_provider_from_auth_error("network timeout"),
        None
    );
}
