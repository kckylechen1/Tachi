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
        _expected_head_sha: Option<&str>,
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
    let _guard = crate::utils::global_test_lock()
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

    // Consume the production writer's actual wire shape through Task status.
    // Compact output must keep the evidence reference and pending/null
    // conclusion, not infer a replacement state from the display label.
    let server = crate::tests::make_server();
    let params = serde_json::from_value(json!({
        "action": "status", "flow_id": flow, "compact": true,
    }))
    .unwrap();
    let compact: serde_json::Value = serde_json::from_str(
        &crate::task_lifecycle::handle_task_cycle_status(&server, &params)
            .await
            .unwrap(),
    )
    .unwrap();
    for key in [
        "state",
        "status",
        "conclusion",
        "source",
        "artifact",
        "failed_checks_recorded_only",
        "updated_at",
    ] {
        assert!(
            compact["github"]["cached"]["checks"].get(key).is_some(),
            "missing {key}"
        );
        assert_eq!(
            compact["github"]["cached"]["checks"][key],
            status["github"]["checks"][key]
        );
    }

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn check_state_artifact_records_red_checks_without_merge_or_repair_side_effects() {
    let _guard = crate::utils::global_test_lock()
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
    let _guard = crate::utils::global_test_lock()
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
        .with_pr(
            "o/r",
            PrState {
                checks: ChecksState::Failure,
                ..ready_pr()
            },
        )
        .with_checks("o/r", 42, checks);
    let flow = "flow_safe-merge-check-state";
    let mut policy = MergeGatePolicy::standard();
    policy.allow_missing_checks = true;
    let out = handle_github_safe_merge(
        &test_server(),
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &[],
        policy,
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
        json!(false)
    );
    assert!(client.merge_calls().is_empty());

    let run_dir = tmp.path().join(flow);
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("check_state.json")).unwrap())
            .unwrap();
    assert_eq!(artifact["pr"]["head_ref"], json!("feat/source-branch"));
    assert_eq!(artifact["aggregate"]["conclusion"], json!("success"));
    assert_eq!(artifact["failed_checks_recorded_only"], json!(false));
    assert_eq!(artifact["repair_attempted"], json!(false));
    assert_eq!(artifact["merge_attempted"], json!(false));
    assert!(!run_dir.join("repair.json").exists());
    assert!(!run_dir.join("merge.json").exists());
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(
        status["github"]["checks"]["source"],
        json!("safe_merge.dry_run")
    );
    assert_eq!(status["github"]["checks"]["state"], json!("success"));
    assert_eq!(status["github"]["checks"]["status"], json!("completed"));
    assert_eq!(status["github"]["checks"]["conclusion"], json!("success"));
    assert_eq!(status["github"]["checks"]["required"], json!(true));
    assert_eq!(status["github"]["checks"]["allow_missing"], json!(true));

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_already_merged_dry_run_still_records_check_state_artifact() {
    let _guard = crate::utils::global_test_lock()
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
        .with_pr(
            "o/r",
            PrState {
                checks: ChecksState::Failure,
                ..already_merged_pr()
            },
        )
        .with_checks("o/r", 42, checks);
    let flow = "flow_already-merged-check-state";
    let mut policy = MergeGatePolicy::strict();
    policy.allow_missing_checks = true;
    let out = handle_github_safe_merge(
        &test_server(),
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &[],
        policy,
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
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .unwrap();
    assert_eq!(
        status["github"]["checks"]["source"],
        json!("safe_merge.dry_run")
    );
    assert_eq!(status["github"]["checks"]["state"], json!("success"));
    assert_eq!(status["github"]["checks"]["status"], json!("completed"));
    assert_eq!(status["github"]["checks"]["conclusion"], json!("success"));
    assert_eq!(status["github"]["checks"]["required"], json!(true));
    assert_eq!(status["github"]["checks"]["allow_missing"], json!(true));

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
        &test_server(),
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
    let _guard = crate::utils::global_test_lock()
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
    let _guard = crate::utils::global_test_lock()
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
    let _guard = crate::utils::global_test_lock()
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
        &test_server(),
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        Some(flow),
        &[],
        MergeGatePolicy::permissive(),
        None,
        false,
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

/// A transport that exposes the check runs behind `pr_view` (like the CLI and
/// HTTP clients) and counts follow-up reads, with a configurable live head.
struct SnapshotCountingClient {
    inner: MockGhClient,
    snapshot_runs: Vec<CheckRun>,
    live_head: String,
    checks_list_calls: std::sync::atomic::AtomicUsize,
    head_calls: std::sync::atomic::AtomicUsize,
}

impl SnapshotCountingClient {
    fn new(snapshot_runs: Vec<CheckRun>, live_head: &str) -> Self {
        Self {
            inner: MockGhClient::new().with_pr("o/r", ready_pr()),
            snapshot_runs,
            live_head: live_head.to_string(),
            checks_list_calls: Default::default(),
            head_calls: Default::default(),
        }
    }
    fn checks_list_calls(&self) -> usize {
        self.checks_list_calls
            .load(std::sync::atomic::Ordering::SeqCst)
    }
    fn head_calls(&self) -> usize {
        self.head_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl GhClient for SnapshotCountingClient {
    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrState, GhError> {
        self.inner.pr_view(repo, number).await
    }
    async fn pr_view_snapshot(&self, repo: &str, number: u64) -> Result<PrViewSnapshot, GhError> {
        Ok(PrViewSnapshot {
            pr: self.inner.pr_view(repo, number).await?,
            check_runs: Some(self.snapshot_runs.clone()),
        })
    }
    async fn pr_head_sha(&self, _repo: &str, _number: u64) -> Result<String, GhError> {
        self.head_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.live_head.clone())
    }
    async fn pr_merge(
        &self,
        repo: &str,
        number: u64,
        strategy: MergeStrategy,
        expected_head_sha: &str,
    ) -> Result<MergeResult, GhError> {
        self.inner
            .pr_merge(repo, number, strategy, expected_head_sha)
            .await
    }
    async fn issue_create(
        &self,
        repo: &str,
        title: &str,
        body: Option<&str>,
        labels: &[String],
    ) -> Result<tachi_gh_safe_merge::IssueState, GhError> {
        self.inner.issue_create(repo, title, body, labels).await
    }
    async fn checks_list(&self, _repo: &str, _pr_number: u64) -> Result<Vec<CheckRun>, GhError> {
        self.checks_list_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(GhError::Sanitized(
            "checks_list must not be re-read when the snapshot has the runs".to_string(),
        ))
    }
}

async fn dry_run_with_snapshot_client(
    client: &SnapshotCountingClient,
    flow: Option<&str>,
) -> serde_json::Value {
    let out = handle_github_safe_merge(
        &test_server(),
        client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true,
        flow,
        &[],
        MergeGatePolicy::permissive(),
        None,
        false,
    )
    .await
    .expect("dry run");
    serde_json::from_str(&out).unwrap()
}

/// E2: the dry-run ledger reuses the check runs from the same `pr_view` read,
/// but still re-reads the live head AFTER them, so a head that moved after
/// the snapshot is recorded as `Stale` (not silently as the snapshot head).
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_dry_run_reuses_snapshot_checks_and_still_detects_moved_head() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());
    let runs = vec![check("ci", "completed", Some("failure"))];

    // Head unchanged: ledger records the snapshot's (red) checks.
    let same_head = SnapshotCountingClient::new(runs.clone(), "deadbeef");
    let flow = "flow_snapshot-same-head";
    let v = dry_run_with_snapshot_client(&same_head, Some(flow)).await;
    assert_eq!(v["check_state_ingest"]["state"], json!("failed"));
    assert_eq!(v["check_state_ingest"]["reader_error"], json!(false));
    assert_eq!(same_head.checks_list_calls(), 0, "no duplicate checks read");
    assert_eq!(same_head.head_calls(), 1, "live head is still re-read");
    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(flow).join("check_state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(artifact["checks"].as_array().unwrap().len(), 1);
    assert_eq!(
        artifact["transition"]["observed_head_sha"],
        json!("deadbeef")
    );

    // Head moved after the snapshot: Stale, never attributed to the old head.
    let moved = SnapshotCountingClient::new(runs.clone(), "newhead");
    let v = dry_run_with_snapshot_client(&moved, Some("flow_snapshot-moved-head")).await;
    assert_eq!(v["check_state_ingest"]["state"], json!("stale"));
    assert_eq!(moved.checks_list_calls(), 0);
    assert_eq!(moved.head_calls(), 1);

    // No flow: the non-auditable artifact also reuses the snapshot runs, and
    // no head read is needed.
    let no_flow = SnapshotCountingClient::new(runs, "deadbeef");
    let v = dry_run_with_snapshot_client(&no_flow, None).await;
    assert_eq!(v["check_state_ingest"]["persisted"], json!(false));
    assert_eq!(no_flow.checks_list_calls(), 0);
    assert_eq!(no_flow.head_calls(), 0);

    if let Some(v) = original {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
