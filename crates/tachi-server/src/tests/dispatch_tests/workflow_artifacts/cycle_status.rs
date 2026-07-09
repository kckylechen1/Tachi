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

fn write_passed_verification(flow_id: &str) {
    let run_dir = crate::shell_ops::run_dir_for_flow_id(flow_id).expect("run dir");
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
}

mod cycle_plan;
mod f2_gh_lifecycle_coaching;
mod flow_read_model;
mod issue_lookup;
mod spec_drift;
mod validation;
