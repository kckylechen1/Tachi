use super::*;

fn issue_snapshot(
    number: u64,
    doc_paths: Vec<&str>,
    spec_paths: Vec<&str>,
) -> crate::task_lifecycle::IssueSnapshot {
    crate::task_lifecycle::IssueSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number,
        title: "Project cycle read model".to_string(),
        body: Some("## Acceptance criteria\n- Cycle status reports linked contracts.".to_string()),
        labels: Vec::new(),
        state: Some("OPEN".to_string()),
        url: format!("https://github.com/kckylechen1/tachi/issues/{number}"),
        doc_paths: doc_paths.into_iter().map(str::to_string).collect(),
        spec_paths: spec_paths.into_iter().map(str::to_string).collect(),
    }
}

fn pr_snapshot(number: u64) -> crate::task_lifecycle::PrSnapshot {
    crate::task_lifecycle::PrSnapshot {
        repo: "kckylechen1/tachi".to_string(),
        number,
        title: "Add project cycle read model".to_string(),
        state: Some("OPEN".to_string()),
        url: format!("https://github.com/kckylechen1/tachi/pull/{number}"),
        head_ref: Some("feat/project-cycle-read-model".to_string()),
        base_ref: Some("main".to_string()),
        review_decision: Some("APPROVED".to_string()),
        mergeable: Some("MERGEABLE".to_string()),
    }
}

fn write_intake_flow(flow_id: &str, issue: &crate::task_lifecycle::IssueSnapshot) {
    let automation_plan = crate::task_lifecycle::build_issue_automation_plan(issue, None);
    crate::task_lifecycle::write_intake_flow_artifacts(
        flow_id,
        "Project cycle read model",
        issue,
        &automation_plan,
    )
    .expect("write intake artifacts");
}

fn write_passed_verification(server: &crate::MemoryServer, flow_id: &str, head_sha: &str) {
    let run_dir = crate::task_lifecycle::run_dir_for_flow_id(flow_id).expect("run dir");
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow_id,
            "overall": "passed",
            "items": [
                {
                    "id": "cargo-test-cycle-status",
                    "kind": "cargo_test",
                    "command": "cargo test -p tachi-server cycle_status",
                    "status": "passed",
                    "required": true
                }
            ]
        }))
        .expect("verification json"),
    )
    .expect("write verification");
    // #1454 F1/G2/F6: the cycle verdict is the gate result over the
    // server-owned receipt store, evaluated against the server-known GitHub
    // head. Seed the full canonical receipt set (executor's G2 shape) so the
    // happy path stays green (the ledger alone no longer mints "verified").
    let home = server.tachi_home_dir();
    for kind in crate::verify_ops::MERGE_REQUIRED_RUN_KINDS {
        let receipt = json!({
            "flow_id": flow_id,
            "kind": kind,
            "head_sha": head_sha,
            "status": "passed",
            "reason": null,
            "exit_code": 0,
            "log_path": "/tmp/cycle-status-seed.log",
            "duration_ms": 1,
            "ran_at": "2026-08-18T00:00:00Z",
            "timed_out": false,
            "kill_abandoned": false,
            "source_head": head_sha,
            "executed_in_detached_copy": true,
            "copy_head_before": head_sha,
            "copy_head_after": head_sha,
            "copy_clean_before": true,
            "copy_clean_after": true,
            "tool_version": "seed-tool-1.0",
        });
        crate::verify_ops::seed_run_receipt_for_test(&home, flow_id, kind, &receipt)
            .expect("seed receipt");
    }
}

mod f2_gh_lifecycle_coaching;
mod flow_read_model;
mod issue_lookup;
mod next_action_field;
mod spec_drift;
mod validation;
