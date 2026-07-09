//! End-to-end integration tests for `tachi_shell` covering the full
//! brainstorm → plan → dispatch → status → review → ship flow lifecycle.
//!
//! These tests exercise the public dispatcher `handle_tachi_shell` through
//! a single persistent `flow_id` to verify:
//!   - instruction.md is rewritten at each stage
//!   - status.json advances stage / state and accumulates history
//!   - events.jsonl appends one event per stage transition
//!   - meta-skill injection is attempted and reported (loaded or warned)
//!   - status action can read back a live flow
//!
//! The tests deliberately do NOT enable async_dispatch — we never want the
//! integration suite to spawn real clanker subprocesses.

use super::{make_server, shell_params};
use crate::shell_ops::{handle_tachi_shell, shell_runs_root};
use serde_json::Value;

fn unique_flow_id(tag: &str) -> String {
    format!("flow_test_{}_{}", tag, uuid::Uuid::new_v4().simple())
}

fn read_events(run_dir: &std::path::Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid event json"))
        .collect()
}

fn read_status_json(run_dir: &std::path::Path) -> Value {
    let raw = std::fs::read_to_string(run_dir.join("status.json")).expect("status.json exists");
    serde_json::from_str(&raw).expect("valid status.json")
}

/// Runs the full brainstorm → plan → dispatch → status → review → ship chain
/// against a single flow_id and asserts cross-stage invariants.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn shell_full_lifecycle_brainstorm_to_ship() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let flow_id = unique_flow_id("lifecycle");
    let runs_root = shell_runs_root();
    let run_dir = runs_root.join(&flow_id);

    // Cleanup guard: remove the run dir even if an assertion panics mid-test.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(run_dir.clone());

    let stages = ["brainstorm", "plan", "dispatch", "review", "ship"];
    for (idx, stage) in stages.iter().enumerate() {
        let mut p = shell_params(stage);
        p.flow_id = Some(flow_id.clone());
        p.task = Some(format!("integration test — {stage}"));
        p.title = Some("lifecycle".to_string());
        p.notes = Some(format!("stage-{stage}-note"));
        p.validation = vec!["cargo test -p tachi-server".to_string()];
        p.allowed_scope = vec!["crates/tachi-server/**".to_string()];

        let raw = handle_tachi_shell(&server, p)
            .await
            .unwrap_or_else(|e| panic!("stage {stage} failed: {e}"));
        let resp: Value = serde_json::from_str(&raw).expect("stage response is json");

        assert_eq!(
            resp.get("flow_id").and_then(|v| v.as_str()),
            Some(flow_id.as_str()),
            "flow_id should round-trip at stage {stage}"
        );
        assert_eq!(
            resp.get("stage").and_then(|v| v.as_str()),
            Some(*stage),
            "response stage mismatch at stage {stage}"
        );
        // Only the first call creates the flow; subsequent stages must reuse it.
        let created = resp
            .get("created")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if idx == 0 {
            assert!(created, "first stage should create the flow");
        } else {
            assert!(!created, "stage {stage} should reuse existing flow");
        }
        // async never fires in the MVP integration path
        assert_eq!(
            resp.get("async").and_then(|v| v.as_bool()),
            Some(false),
            "async flag must be false when async_dispatch=false at stage {stage}"
        );

        // instruction.md always rewritten with the current stage header.
        let instr = std::fs::read_to_string(run_dir.join("instruction.md"))
            .expect("instruction.md written");
        assert!(
            instr.contains(&format!("Stage: **{stage}**")),
            "instruction.md missing stage header at stage {stage}"
        );
        assert!(
            instr.contains(&flow_id),
            "instruction.md missing flow_id at stage {stage}"
        );
        assert!(
            instr.contains(&format!("integration test — {stage}")),
            "instruction.md missing task body at stage {stage}"
        );

        // status.json reflects the latest stage and grows history by exactly 1.
        let status = read_status_json(&run_dir);
        assert_eq!(
            status.get("stage").and_then(|v| v.as_str()),
            Some(*stage),
            "status.stage mismatch at {stage}"
        );
        let expected_state = if *stage == "dispatch" {
            "dispatch_ready"
        } else {
            "instruction_ready"
        };
        assert_eq!(
            status.get("state").and_then(|v| v.as_str()),
            Some(expected_state),
            "status.state mismatch at {stage}"
        );
        let history = status
            .get("history")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            history.len(),
            idx + 1,
            "history length mismatch at stage {stage}"
        );
        assert_eq!(
            history
                .last()
                .and_then(|h| h.get("stage"))
                .and_then(|v| v.as_str()),
            Some(*stage),
            "history tail should be current stage at {stage}"
        );

        // events.jsonl appends exactly one entry per stage call.
        let events = read_events(&run_dir);
        assert_eq!(
            events.len(),
            idx + 1,
            "events.jsonl length mismatch at stage {stage}"
        );
        let last_evt = events.last().unwrap();
        let expected_evt = if idx == 0 {
            "flow_created"
        } else {
            "stage_entered"
        };
        assert_eq!(
            last_evt.get("event").and_then(|v| v.as_str()),
            Some(expected_evt),
            "event kind mismatch at {stage}"
        );
        assert_eq!(
            last_evt.get("stage").and_then(|v| v.as_str()),
            Some(*stage),
            "event stage mismatch at {stage}"
        );

        // Injected skill block is always reported; whether it loaded depends on
        // whether the `skill/superpowers/...` tree is present in the repo.
        // Either way `required=true` for every stage-bearing call.
        let injected = resp
            .get("injected_skill")
            .expect("injected_skill in response");
        assert_eq!(
            injected.get("required").and_then(|v| v.as_bool()),
            Some(true),
            "injected.required should be true for stage {stage}"
        );
        if injected.get("loaded").and_then(|v| v.as_bool()) == Some(true) {
            let path = injected
                .get("injected_path")
                .and_then(|v| v.as_str())
                .expect("injected_path on successful load");
            assert!(
                std::path::Path::new(path).exists(),
                "injected file should exist on disk: {path}"
            );
        } else {
            assert!(
                injected
                    .get("warning")
                    .and_then(|v| v.as_str())
                    .is_some_and(|w| !w.is_empty()),
                "injection.loaded=false must carry a warning at {stage}"
            );
        }
    }

    // ship stage includes PR-first release guidance.
    let final_instr = std::fs::read_to_string(run_dir.join("instruction.md")).unwrap();
    assert!(
        final_instr.contains("## Release Flow"),
        "ship instruction.md must carry Release Flow section"
    );
}

/// After a flow is progressed, `action="status"` with the same flow_id must
/// return the latest persisted state and no extra event should be appended.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn shell_status_action_reads_live_flow() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let flow_id = unique_flow_id("status");
    let runs_root = shell_runs_root();
    let run_dir = runs_root.join(&flow_id);

    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(run_dir.clone());

    // Seed a flow via brainstorm.
    let mut p = shell_params("brainstorm");
    p.flow_id = Some(flow_id.clone());
    p.task = Some("status readback".to_string());
    handle_tachi_shell(&server, p).await.expect("brainstorm ok");

    let events_before = read_events(&run_dir).len();

    let mut q = shell_params("status");
    q.flow_id = Some(flow_id.clone());
    let raw = handle_tachi_shell(&server, q).await.expect("status ok");
    let resp: Value = serde_json::from_str(&raw).expect("status response is json");

    assert_eq!(
        resp.get("found").and_then(|v| v.as_bool()),
        Some(true),
        "status should find live flow"
    );
    assert_eq!(
        resp.get("flow_id").and_then(|v| v.as_str()),
        Some(flow_id.as_str())
    );
    let inner = resp.get("status").expect("status envelope");
    assert_eq!(
        inner.get("stage").and_then(|v| v.as_str()),
        Some("brainstorm")
    );

    // status must be read-only — events.jsonl must not grow.
    let events_after = read_events(&run_dir).len();
    assert_eq!(
        events_before, events_after,
        "status action must not append events"
    );
}

/// `action="status"` with a bogus flow_id must report not-found instead of
/// creating a stray run directory.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn shell_status_action_missing_flow_is_not_found() {
    let _lock = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = make_server();
    let flow_id = unique_flow_id("missing");
    let runs_root = shell_runs_root();
    let run_dir = runs_root.join(&flow_id);

    let mut p = shell_params("status");
    p.flow_id = Some(flow_id.clone());
    let raw = handle_tachi_shell(&server, p).await.expect("status ok");
    let resp: Value = serde_json::from_str(&raw).expect("status response is json");

    assert_eq!(resp.get("found").and_then(|v| v.as_bool()), Some(false));
    assert!(
        !run_dir.exists(),
        "status lookup must not create a run directory"
    );
}
