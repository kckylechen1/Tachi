use super::*;
use async_trait::async_trait;

#[derive(Clone)]
struct FixedCheckReader {
    result: Result<CheckStateRead, GhError>,
}

#[async_trait]
impl CheckStateReader for FixedCheckReader {
    async fn read_check_state(
        &self,
        _repo: &str,
        _pr_number: u64,
    ) -> Result<CheckStateRead, GhError> {
        self.result.clone()
    }
}

fn check(name: &str, status: &str, conclusion: Option<&str>) -> CheckRun {
    CheckRun {
        name: name.to_string(),
        status: status.to_string(),
        conclusion: conclusion.map(str::to_string),
    }
}

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
            transition: None,
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
            transition: None,
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
            transition: None,
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

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn check_state_ingest_reader_records_pending_to_failed_transition() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let request = CheckStateIngestRequest {
        flow_id: "flow_check-state-transition",
        repo: "o/r",
        pr_number: 42,
        pr_ref: Some("o/r#42"),
        head_ref: Some("feat/check-state"),
        expected_head_sha: Some("head-1"),
        source: "ci_state.watch",
    };

    let pending = FixedCheckReader {
        result: Ok(CheckStateRead {
            checks: vec![check("ci", "in_progress", None)],
            observed_head_sha: Some("head-1".to_string()),
        }),
    };
    let first = ingest_check_state_transition(&pending, &request)
        .await
        .expect("ingest pending checks");
    assert_eq!(first.state, CheckStateLedgerState::Pending);
    assert_eq!(first.previous_state, None);
    assert!(first.changed);

    let failed = FixedCheckReader {
        result: Ok(CheckStateRead {
            checks: vec![check("ci", "completed", Some("failure"))],
            observed_head_sha: Some("head-1".to_string()),
        }),
    };
    let second = ingest_check_state_transition(&failed, &request)
        .await
        .expect("ingest failed checks");
    assert_eq!(second.state, CheckStateLedgerState::Failed);
    assert_eq!(second.previous_state.as_deref(), Some("pending"));
    assert!(second.changed);

    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            tmp.path()
                .join("flow_check-state-transition")
                .join("check_state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(artifact["transition"]["previous_state"], json!("pending"));
    assert_eq!(artifact["transition"]["state"], json!("failed"));
    assert_eq!(artifact["transition"]["changed"], json!(true));
    assert_eq!(artifact["transition"]["expected_head_sha"], json!("head-1"));
    assert_eq!(artifact["transition"]["observed_head_sha"], json!("head-1"));
    assert_eq!(artifact["failed_checks_recorded_only"], json!(true));
    assert_eq!(artifact["repair_attempted"], json!(false));
    assert_eq!(artifact["merge_attempted"], json!(false));

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn check_state_ingest_distinguishes_no_checks_stale_and_reader_error() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let no_checks = FixedCheckReader {
        result: Ok(CheckStateRead {
            checks: Vec::new(),
            observed_head_sha: Some("head-1".to_string()),
        }),
    };
    let no_checks_request = CheckStateIngestRequest {
        flow_id: "flow_no-checks",
        repo: "o/r",
        pr_number: 42,
        pr_ref: Some("o/r#42"),
        head_ref: None,
        expected_head_sha: Some("head-1"),
        source: "ci_state.watch",
    };
    let no_checks_result = ingest_check_state_transition(&no_checks, &no_checks_request)
        .await
        .expect("ingest no checks");
    assert_eq!(no_checks_result.state, CheckStateLedgerState::NoChecks);

    let stale = FixedCheckReader {
        result: Ok(CheckStateRead {
            checks: vec![check("ci", "completed", Some("success"))],
            observed_head_sha: Some("old-head".to_string()),
        }),
    };
    let stale_request = CheckStateIngestRequest {
        flow_id: "flow_stale-checks",
        repo: "o/r",
        pr_number: 43,
        pr_ref: Some("o/r#43"),
        head_ref: None,
        expected_head_sha: Some("new-head"),
        source: "ci_state.watch",
    };
    let stale_result = ingest_check_state_transition(&stale, &stale_request)
        .await
        .expect("ingest stale checks");
    assert_eq!(stale_result.state, CheckStateLedgerState::Stale);
    let stale_artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            tmp.path()
                .join("flow_stale-checks")
                .join("check_state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(stale_artifact["aggregate"]["state"], json!("success"));
    assert_eq!(stale_artifact["transition"]["state"], json!("stale"));
    assert_eq!(
        stale_artifact["transition"]["expected_head_sha"],
        json!("new-head")
    );
    assert_eq!(
        stale_artifact["transition"]["observed_head_sha"],
        json!("old-head")
    );

    let reader_error = FixedCheckReader {
        result: Err(GhError::Sanitized(
            "gh pr checks failed: redacted".to_string(),
        )),
    };
    let error_request = CheckStateIngestRequest {
        flow_id: "flow_reader-error",
        repo: "o/r",
        pr_number: 44,
        pr_ref: Some("o/r#44"),
        head_ref: None,
        expected_head_sha: None,
        source: "ci_state.watch",
    };
    let error_result = ingest_check_state_transition(&reader_error, &error_request)
        .await
        .expect("record reader error");
    assert_eq!(error_result.state, CheckStateLedgerState::ReaderError);
    let error_artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            tmp.path()
                .join("flow_reader-error")
                .join("check_state.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(error_artifact["aggregate"]["state"], json!("none"));
    assert_eq!(error_artifact["transition"]["state"], json!("reader_error"));
    assert_eq!(
        error_artifact["transition"]["read_error"],
        json!("gh client error: gh pr checks failed: redacted")
    );

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

/// End-to-end guard for the dry-run `checks_list` error semantics.
///
/// Before #816, a `checks_list` failure during a dry-run ingest propagated via
/// `?` and `handle_github_safe_merge` returned `Err`. The check-state recorder
/// now converts that failure into a `reader_error` ledger state and returns
/// `Ok`, so the dry-run still produces an envelope. This test pins that
/// behavior and asserts the degraded-input marker is surfaced so a permissive
/// `Ready` (computed from `pr_view`'s independent checks snapshot) cannot be
/// mistaken for "all checks confirmed green" by the operator.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_dry_run_returns_ok_with_reader_error_marker_when_checks_list_fails() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    // pr_view succeeds with green checks; checks_list fails (rate-limited).
    // The merge decision is computed from `pr.checks` (Success), which under a
    // permissive policy yields `Ready` — exactly the misleading case the
    // reader_error marker exists to flag.
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks_list_error(GhError::RateLimited("secondary rate limit".to_string()));
    let flow = "flow_dry-run-reader-error";
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &[],
        MergeGatePolicy::permissive(),
    )
    .await
    .expect("dry-run must return Ok with a reader_error ledger state");

    // New behavior: Ok (NOT Err) — the error was converted to a ledger state.
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    // Degraded-input marker is surfaced in the envelope.
    assert_eq!(v["check_state_ingest"]["reader_error"], json!(true));
    assert_eq!(v["check_state_ingest"]["state"], json!("reader_error"));
    assert_eq!(v["check_state_ingest"]["persisted"], json!(true));
    // Decision still computed from pr_view's independent checks snapshot.
    assert_eq!(v["merge_state"], json!("ready"));
    assert_eq!(v["merge_attempted"], json!(false));
    assert!(client.merge_calls().is_empty());

    // reader_error transition was written to the check_state artifact.
    let run_dir = tmp.path().join(flow);
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("check_state.json")).unwrap())
            .unwrap();
    assert_eq!(artifact["transition"]["state"], json!("reader_error"));
    assert_eq!(artifact["checks"].as_array().unwrap().len(), 0);
    assert_eq!(artifact["buckets"]["total"], json!(0));
    assert_eq!(
        artifact["transition"]["read_error"],
        json!("rate limited: secondary rate limit")
    );

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
