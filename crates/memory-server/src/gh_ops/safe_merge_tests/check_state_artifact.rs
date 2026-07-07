use super::*;

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn check_state_artifact_writes_machine_readable_flow_snapshot() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let checks = vec![
        CheckRun {
            name: "fmt".to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
        },
        CheckRun {
            name: "integration".to_string(),
            status: "in_progress".to_string(),
            conclusion: None,
        },
    ];
    let flow = "flow_check-state-artifact";
    let result = write_check_state_artifact(
        Some(flow),
        &CheckStateArtifactInput {
            repo: "o/r",
            pr_number: 42,
            pr_ref: Some("o/r#42"),
            head_ref: Some("feat/check-state"),
            observed_at: "2026-07-07T00:00:00Z",
            source: "safe_merge.dry_run",
            dry_run: true,
            checks: &checks,
        },
    )
    .expect("write artifact");

    assert!(result.persisted);
    assert!(result.non_auditable_reason.is_none());
    assert!(!result.failed_checks_recorded_only);
    let artifact_path = result.artifact_path.expect("artifact path");
    assert!(
        artifact_path.ends_with("check_state.json"),
        "{artifact_path}"
    );

    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(flow).join("check_state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(artifact["schema"], json!("tachi.github.check_state.v1"));
    assert_eq!(artifact["flow_id"], json!(flow));
    assert_eq!(artifact["repo"], json!("o/r"));
    assert_eq!(artifact["pr"]["number"], json!(42));
    assert_eq!(artifact["pr"]["ref"], json!("o/r#42"));
    assert_eq!(artifact["pr"]["head_ref"], json!("feat/check-state"));
    assert_eq!(artifact["observed_at"], json!("2026-07-07T00:00:00Z"));
    assert_eq!(artifact["source"], json!("safe_merge.dry_run"));
    assert_eq!(artifact["dry_run"], json!(true));
    assert_eq!(artifact["aggregate"]["status"], json!("pending"));
    assert_eq!(artifact["aggregate"]["conclusion"], serde_json::Value::Null);
    assert_eq!(artifact["buckets"]["success"], json!(1));
    assert_eq!(artifact["buckets"]["pending"], json!(1));
    assert_eq!(artifact["checks"].as_array().unwrap().len(), 2);
    assert_eq!(artifact["failed_checks_recorded_only"], json!(false));

    let status: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(flow).join("status.json")).unwrap(),
    )
    .unwrap();
    assert!(status["artifacts"]["check_state"]["path"]
        .as_str()
        .is_some_and(|path| path.ends_with("check_state.json")));
    assert_eq!(status["artifacts"]["check_state"]["exists"], json!(true));
    assert_eq!(status["github"]["checks"]["state"], json!("pending"));
    assert_eq!(
        status["github"]["checks"]["artifact"],
        json!("check_state.json")
    );

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn check_state_artifact_records_red_checks_without_merge_or_repair_side_effects() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let checks = vec![CheckRun {
        name: "test".to_string(),
        status: "completed".to_string(),
        conclusion: Some("failure".to_string()),
    }];
    let result = write_check_state_artifact(
        Some("flow_red-check-state"),
        &CheckStateArtifactInput {
            repo: "o/r",
            pr_number: 42,
            pr_ref: Some("o/r#42"),
            head_ref: None,
            observed_at: "2026-07-07T00:00:00Z",
            source: "safe_merge.dry_run",
            dry_run: true,
            checks: &checks,
        },
    )
    .expect("record red checks");
    assert!(result.persisted);
    assert!(result.failed_checks_recorded_only);

    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            tmp.path()
                .join("flow_red-check-state")
                .join("check_state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(artifact["aggregate"]["status"], json!("completed"));
    assert_eq!(artifact["aggregate"]["conclusion"], json!("failure"));
    assert_eq!(artifact["buckets"]["failure"], json!(1));
    assert_eq!(artifact["failed_checks_recorded_only"], json!(true));
    assert_eq!(artifact["repair_attempted"], json!(false));
    assert_eq!(artifact["merge_attempted"], json!(false));

    let run_dir = tmp.path().join("flow_red-check-state");
    assert!(!run_dir.join("repair.json").exists());
    assert!(!run_dir.join("merge.json").exists());

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn check_state_artifact_without_flow_id_is_explicitly_non_auditable() {
    let checks = vec![CheckRun {
        name: "test".to_string(),
        status: "completed".to_string(),
        conclusion: Some("failure".to_string()),
    }];
    let result = write_check_state_artifact(
        None,
        &CheckStateArtifactInput {
            repo: "o/r",
            pr_number: 42,
            pr_ref: None,
            head_ref: None,
            observed_at: "2026-07-07T00:00:00Z",
            source: "safe_merge.dry_run",
            dry_run: true,
            checks: &checks,
        },
    )
    .expect("missing flow_id should be a reportable non-auditable result");
    assert!(!result.persisted);
    assert!(result.artifact_path.is_none());
    assert_eq!(
        result.non_auditable_reason.as_deref(),
        Some("missing flow_id: check-state snapshot was observed but not written to a Tachi flow ledger")
    );
    assert!(result.failed_checks_recorded_only);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_dry_run_records_red_check_state_artifact_without_merge_or_repair() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let checks = vec![CheckRun {
        name: "ci".to_string(),
        status: "completed".to_string(),
        conclusion: Some("failure".to_string()),
    }];
    let client = MockGhClient::new()
        .with_pr(
            "o/r",
            PrState {
                checks: ChecksState::Failure,
                ..ready_pr()
            },
        )
        .with_checks("o/r", 42, checks);
    let flow = "flow_safe-merge-check-state";
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("safe_merge dry run records check state");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_state"], json!("blocked"));
    assert_eq!(v["merge_attempted"], json!(false));
    assert_eq!(v["merge_executed"], json!(false));
    assert_eq!(v["check_state_ingest"]["persisted"], json!(true));
    assert_eq!(
        v["check_state_ingest"]["failed_checks_recorded_only"],
        json!(true)
    );
    assert!(client.merge_calls().is_empty());

    let run_dir = tmp.path().join(flow);
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("check_state.json")).unwrap())
            .unwrap();
    assert_eq!(artifact["pr"]["head_ref"], json!("feat/source-branch"));
    assert_eq!(artifact["aggregate"]["conclusion"], json!("failure"));
    assert_eq!(artifact["failed_checks_recorded_only"], json!(true));
    assert_eq!(artifact["repair_attempted"], json!(false));
    assert_eq!(artifact["merge_attempted"], json!(false));
    assert!(!run_dir.join("repair.json").exists());
    assert!(!run_dir.join("merge.json").exists());

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_already_merged_dry_run_still_records_check_state_artifact() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let checks = vec![CheckRun {
        name: "ci".to_string(),
        status: "completed".to_string(),
        conclusion: Some("success".to_string()),
    }];
    let client = MockGhClient::new()
        .with_pr("o/r", already_merged_pr())
        .with_checks("o/r", 42, checks);
    let flow = "flow_already-merged-check-state";
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &[],
        MergeGatePolicy::strict(),
        None,
        false,
    )
    .await
    .expect("already merged dry run records check state");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"]["decision"], json!("already_merged"));
    assert_eq!(v["already_merged"], json!(true));
    assert_eq!(v["check_state_ingest"]["persisted"], json!(true));

    let run_dir = tmp.path().join(flow);
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("check_state.json")).unwrap())
            .unwrap();
    assert_eq!(artifact["pr"]["head_ref"], json!("feat/source-branch"));
    assert_eq!(artifact["source"], json!("safe_merge.dry_run"));
    assert_eq!(artifact["aggregate"]["conclusion"], json!("success"));
    assert_eq!(artifact["merge_attempted"], json!(false));
    assert!(client.merge_calls().is_empty());

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn safe_merge_dry_run_without_flow_id_reports_non_auditable_check_state() {
    let checks = vec![CheckRun {
        name: "ci".to_string(),
        status: "completed".to_string(),
        conclusion: Some("failure".to_string()),
    }];
    let client = MockGhClient::new()
        .with_pr(
            "o/r",
            PrState {
                checks: ChecksState::Failure,
                ..ready_pr()
            },
        )
        .with_checks("o/r", 42, checks);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        None,
        &[],
        MergeGatePolicy::standard(),
        None,
        false,
    )
    .await
    .expect("safe_merge dry run should report non-auditable ingest");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["check_state_ingest"]["persisted"], json!(false));
    assert_eq!(
        v["check_state_ingest"]["non_auditable_reason"],
        json!("missing flow_id: check-state snapshot was observed but not written to a Tachi flow ledger")
    );
    assert_eq!(
        v["check_state_ingest"]["failed_checks_recorded_only"],
        json!(true)
    );
    assert!(client.merge_calls().is_empty());
}
