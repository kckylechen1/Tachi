use super::*;
use crate::gh_safe_merge::{CheckRun, MockGhClient};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn ready_pr() -> PrState {
    PrState {
        number: 42,
        state: PrLifecycleState::Open,
        mergeable: Mergeable::Mergeable,
        review_decision: Some(ReviewDecision::Approved),
        checks: ChecksState::Success,
        is_draft: false,
        head_sha: "deadbeef".to_string(),
        head_ref: Some("feat/source-branch".to_string()),
        linked_issue_refs: Vec::new(),
        closing_issue_labels: Vec::new(),
        head_consistent: None,
    }
}

fn already_merged_pr() -> PrState {
    PrState {
        state: PrLifecycleState::Merged,
        ..ready_pr()
    }
}

fn blocked_draft_pr() -> PrState {
    PrState {
        is_draft: true,
        ..ready_pr()
    }
}

fn pending_pr() -> PrState {
    PrState {
        mergeable: Mergeable::Unknown,
        ..ready_pr()
    }
}

fn skipped_checks_pr() -> PrState {
    PrState {
        checks: ChecksState::Skipped,
        ..ready_pr()
    }
}

fn write_verification(root: &std::path::Path, flow: &str, status: &str, head_sha: &str) {
    let run_dir = root.join(flow);
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::write(
        run_dir.join("verification.json"),
        serde_json::to_string_pretty(&json!({
            "flow_id": flow,
            "head_sha": head_sha,
            "overall": status,
            "updated_at": "2026-06-08T00:00:00Z",
            "items": [
                {
                    "id": "gitleaks",
                    "kind": "gitleaks",
                    "status": status,
                    "head_sha": head_sha,
                    "required": true,
                    "summary": "verification fixture"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
}

mod auth_env;
mod check_state_artifact;
mod flow_events;
mod merge_gate;
mod parsers;
mod review_digest;
mod strict_verification;
