use super::*;

// ─── GitHub status / events helpers ──────────────────────────────────
//
// Relocated from `shell_ops/tests/github_status_events.rs` alongside the
// implementation move in kckylechen1/tachi#1490. The assertions are unchanged;
// they now prove the canonical Task-lifecycle owner preserves the same
// persisted `status.json` / `events.jsonl` shape, deep-merge/null-clear
// semantics, allow-lists, reserved framing keys, timestamps, and error text.

fn read_events_jsonl(run_dir: &Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("event line is JSON"))
        .collect()
}

fn read_status_obj(run_dir: &Path) -> Value {
    let raw = std::fs::read_to_string(run_dir.join("status.json")).unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or(json!({}))
}

#[test]
fn merge_github_status_creates_block_when_absent() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-create");
    std::fs::create_dir_all(&run_dir).unwrap();

    let merged = merge_github_status(
        &run_dir,
        json!({
            "repo": "kckylec/sigil",
            "issue_number": 42,
            "issue_url": "https://github.com/kckylec/sigil/issues/42",
        }),
    )
    .expect("merge should succeed on empty status");

    assert_eq!(merged["repo"], json!("kckylec/sigil"));
    assert_eq!(merged["issue_number"], json!(42));

    let on_disk = read_status_obj(&run_dir);
    assert_eq!(on_disk["github"]["repo"], json!("kckylec/sigil"));
    assert!(
        on_disk["updated_at"].is_string(),
        "merge_github_status must stamp top-level updated_at"
    );
}

#[test]
fn merge_github_status_deep_merges_partial_patches() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-merge");
    std::fs::create_dir_all(&run_dir).unwrap();

    // Seed: PR created with full checks block.
    merge_github_status(
        &run_dir,
        json!({
            "repo": "owner/repo",
            "pr_number": 7,
            "pr_url": "https://github.com/owner/repo/pull/7",
            "merge_state": "pending",
            "checks": { "state": "pending", "updated_at": "T0" },
        }),
    )
    .unwrap();

    // Patch: only the checks.state changes — checks.updated_at must
    // survive (deep merge), and pr_number / repo must be untouched.
    let merged = merge_github_status(
        &run_dir,
        json!({
            "checks": { "state": "success", "updated_at": "T1" },
        }),
    )
    .unwrap();

    assert_eq!(merged["repo"], json!("owner/repo"));
    assert_eq!(merged["pr_number"], json!(7));
    assert_eq!(merged["checks"]["state"], json!("success"));
    assert_eq!(merged["checks"]["updated_at"], json!("T1"));
    assert_eq!(merged["merge_state"], json!("pending"));

    // Patch: advance merge_state without touching anything else.
    let merged = merge_github_status(&run_dir, json!({ "merge_state": "ready" })).unwrap();
    assert_eq!(merged["merge_state"], json!("ready"));
    assert_eq!(merged["pr_number"], json!(7), "pr_number must persist");
}

#[test]
fn merge_github_status_null_value_clears_field() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-clear");
    std::fs::create_dir_all(&run_dir).unwrap();
    merge_github_status(&run_dir, json!({ "issue_number": 1, "pr_number": 2 })).unwrap();
    let merged = merge_github_status(&run_dir, json!({ "issue_number": null })).unwrap();
    assert!(
        merged.get("issue_number").is_none(),
        "null patch value must remove the key, got: {merged}"
    );
    assert_eq!(merged["pr_number"], json!(2), "pr_number must persist");
}

#[test]
fn merge_github_status_rejects_non_object_patch() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-bad-shape");
    std::fs::create_dir_all(&run_dir).unwrap();
    let err = merge_github_status(&run_dir, json!("not-an-object"))
        .expect_err("string patch must be rejected");
    assert!(err.contains("must be a JSON object"), "got: {err}");
    let err = merge_github_status(&run_dir, json!(null)).expect_err("null patch must be rejected");
    assert!(err.contains("must be a JSON object"), "got: {err}");
}

#[test]
fn merge_github_status_rejects_invalid_merge_state() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-bad-state");
    std::fs::create_dir_all(&run_dir).unwrap();
    let err = merge_github_status(&run_dir, json!({ "merge_state": "exploded" }))
        .expect_err("invalid merge_state must be rejected");
    assert!(err.contains("invalid merge_state"), "got: {err}");
    // No status.json should have been written.
    assert!(
        !run_dir.join("status.json").exists(),
        "rejected patch must not partially write status.json"
    );
}

#[test]
fn append_github_event_writes_typed_event_with_framing() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-event");
    std::fs::create_dir_all(&run_dir).unwrap();

    append_github_event(
        &run_dir,
        "flow-abc",
        "github_pr_created",
        json!({ "pr_number": 99, "pr_url": "https://github.com/o/r/pull/99" }),
    )
    .unwrap();
    append_github_event(
        &run_dir,
        "flow-abc",
        "github_checks_polled",
        json!({ "state": "pending" }),
    )
    .unwrap();

    let events = read_events_jsonl(&run_dir);
    assert_eq!(events.len(), 2, "two events expected, got: {events:?}");

    assert_eq!(events[0]["event"], json!("github_pr_created"));
    assert_eq!(events[0]["flow_id"], json!("flow-abc"));
    assert_eq!(events[0]["pr_number"], json!(99));
    assert!(
        events[0]["timestamp"].is_string(),
        "event must carry an RFC3339 timestamp"
    );

    assert_eq!(events[1]["event"], json!("github_checks_polled"));
    assert_eq!(events[1]["state"], json!("pending"));
}

#[test]
fn append_github_event_rejects_unknown_kind() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-bad-kind");
    std::fs::create_dir_all(&run_dir).unwrap();
    let err = append_github_event(&run_dir, "flow", "github_nukes_launched", json!({}))
        .expect_err("unknown kind must be rejected");
    assert!(err.contains("unknown kind"), "got: {err}");
    assert!(
        !run_dir.join("events.jsonl").exists(),
        "rejected event must not be partially written"
    );
}

#[test]
fn append_github_event_reserved_keys_cannot_be_overridden() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-reserved");
    std::fs::create_dir_all(&run_dir).unwrap();
    append_github_event(
        &run_dir,
        "real-flow",
        "github_pr_merged",
        json!({
            "event": "spoofed",
            "flow_id": "spoofed",
            "timestamp": "spoofed",
            "merge_sha": "deadbeef",
        }),
    )
    .unwrap();
    let events = read_events_jsonl(&run_dir);
    assert_eq!(events[0]["event"], json!("github_pr_merged"));
    assert_eq!(events[0]["flow_id"], json!("real-flow"));
    assert_ne!(events[0]["timestamp"], json!("spoofed"));
    assert_eq!(events[0]["merge_sha"], json!("deadbeef"));
}

#[test]
fn github_block_coexists_with_existing_status_fields() {
    let _root = temp_runs_root();
    let run_dir = _root.join("flow-gh-coexist");
    std::fs::create_dir_all(&run_dir).unwrap();
    // Pre-seed a status.json that mimics a flow already in `dispatch`.
    crate::utils::write_run_status_file(
        &run_dir,
        &json!({
            "flow_id": "flow-coexist",
            "stage": "dispatch",
            "state": "dispatch_ready",
            "history": [{"stage": "dispatch", "from": "plan", "at": "T0"}],
        }),
    )
    .unwrap();

    merge_github_status(
        &run_dir,
        json!({ "repo": "o/r", "pr_number": 1, "merge_state": "pending" }),
    )
    .unwrap();

    let on_disk = read_status_obj(&run_dir);
    // Pre-existing fields must survive.
    assert_eq!(on_disk["flow_id"], json!("flow-coexist"));
    assert_eq!(on_disk["stage"], json!("dispatch"));
    assert_eq!(on_disk["history"][0]["stage"], json!("dispatch"));
    // New github block was added.
    assert_eq!(on_disk["github"]["repo"], json!("o/r"));
    assert_eq!(on_disk["github"]["pr_number"], json!(1));
}
