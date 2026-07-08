use super::*;
use std::io::Write;

/// Write a fake `tachi-clean` binary (a POSIX sh script) to `bin_path` that:
///   - appends its argv to `marker_path` (so tests can assert it was called),
///   - removes the worktree path passed as `wt-remove <path>`,
///   - prints the `--json` report the real cleaner emits.
///
/// Returns the marker path the test reads back to assert invocation.
fn install_fake_cleaner(
    bin_dir: &std::path::Path,
    report_removed: bool,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let bin_path = bin_dir.join(format!("fake-tachi-clean{}", std::env::consts::EXE_SUFFIX));
    let marker_path = bin_dir.join("cleaner-invocations.log");
    // The cleaner receives `wt-remove <path> --force --json`. We capture the
    // <path>, rmdir it, and emit the JSON shape CleanerRemoveReport expects.
    let removed_json = if report_removed { "true" } else { "false" };
    let script = format!(
        r#"#!/bin/sh
# fake tachi-clean for safe_merge reclamation tests
echo "$*" >> "{marker}"
case "$1" in
  wt-remove)
    worktree="$2"
    if [ -d "$worktree" ]; then
      rm -rf "$worktree"
    fi
    ;;
esac
printf '{{"removed":{removed},"warnings":[],"errors":[]}}\n'
exit 0
"#,
        marker = marker_path.display(),
        removed = removed_json,
    );
    let mut file = std::fs::File::create(&bin_path).unwrap();
    file.write_all(script.as_bytes()).unwrap();
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (bin_path, marker_path)
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_reclaims_worktree_after_successful_merge() {
    // RED/GREEN: on the pre-fix code, handle_github_safe_merge had no
    // reclamation hook, so the cleaner is never invoked and the worktree dir
    // survives the merge. After the fix, the cleaner runs and the worktree is
    // removed.
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let (bin_path, marker_path) = install_fake_cleaner(&bin_dir, true);
    let original_clean_bin = std::env::var_os("TACHI_CLEAN_BIN");
    std::env::set_var("TACHI_CLEAN_BIN", &bin_path);

    // Create a worktree dir the fake cleaner will remove.
    let worktree = tmp.path().join("wt-484");
    std::fs::create_dir_all(&worktree).unwrap();

    let flow = "flow_reclaim-success";
    write_verification(tmp.path(), flow, "passed", "deadbeef");
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false, // not a dry-run — a real merge
        Some(flow),
        &[],
        MergeGatePolicy::standard(),
        Some(worktree.to_str().unwrap()),
        true, // reclaim_worktree
    )
    .await
    .expect("merge + reclaim ok");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_executed"], true, "merge should have executed");
    assert_eq!(v["reclamation"]["attempted"], true);
    assert_eq!(v["reclamation"]["reclaimed"], true);

    // The cleaner was invoked with `wt-remove <worktree> --force --json`.
    let invocations = std::fs::read_to_string(&marker_path).unwrap();
    assert!(
        invocations.contains("wt-remove") && invocations.contains(worktree.to_str().unwrap()),
        "expected cleaner to be invoked with wt-remove; got: {invocations}"
    );
    // The worktree was actually removed.
    assert!(
        !worktree.exists(),
        "worktree should have been removed by the cleaner"
    );
    // The reclaim event was recorded in the flow ledger with the success kind.
    let events = std::fs::read_to_string(tmp.path().join(flow).join("events.jsonl")).unwrap();
    let reclaim_event = events
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .find(|e| {
            e.get("event").and_then(|v| v.as_str()).is_some_and(|s| {
                s == "github_safe_merge_reclaimed" || s == "github_safe_merge_reclaim_skipped"
            })
        })
        .expect("expected a reclamation event");
    assert_eq!(
        reclaim_event["event"], "github_safe_merge_reclaimed",
        "genuine success should record github_safe_merge_reclaimed; got: {events}"
    );
    assert_eq!(client.merge_calls().len(), 1);

    if let Some(v) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
    match original_clean_bin {
        Some(v) => std::env::set_var("TACHI_CLEAN_BIN", v),
        None => std::env::remove_var("TACHI_CLEAN_BIN"),
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_dry_run_does_not_reclaim_worktree() {
    // A dry-run (preview) must NOT reclaim — even when a worktree is supplied
    // and reclaim_worktree=true. The merge never executes, so there is nothing
    // terminal to reclaim from.
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let (bin_path, marker_path) = install_fake_cleaner(&bin_dir, true);
    let original_clean_bin = std::env::var_os("TACHI_CLEAN_BIN");
    std::env::set_var("TACHI_CLEAN_BIN", &bin_path);

    let worktree = tmp.path().join("wt-dryrun");
    std::fs::create_dir_all(&worktree).unwrap();

    let flow = "flow_reclaim-dry-run";
    write_verification(tmp.path(), flow, "passed", "deadbeef");
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        true, // dry-run / preview
        Some(flow),
        &[],
        MergeGatePolicy::standard(),
        Some(worktree.to_str().unwrap()),
        true, // reclaim_worktree requested — but dry-run gates it
    )
    .await
    .expect("preview ok");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_executed"], false);
    assert_eq!(v["reclamation"]["attempted"], false);
    assert_eq!(v["reclamation"]["skipped"], "dry_run");

    // The cleaner was never invoked.
    assert!(!marker_path.exists(), "dry-run must not invoke the cleaner");
    // The worktree survived.
    assert!(worktree.exists(), "worktree must survive a dry-run");
    // No reclaim event was recorded (neither success nor skipped kind).
    let events_path = tmp.path().join(flow).join("events.jsonl");
    if events_path.exists() {
        let events = std::fs::read_to_string(&events_path).unwrap();
        assert!(
            !events.contains("github_safe_merge_reclaimed")
                && !events.contains("github_safe_merge_reclaim_skipped"),
            "dry-run must not record a reclaim event; got: {events}"
        );
    }
    assert!(client.merge_calls().is_empty());

    if let Some(v) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
    match original_clean_bin {
        Some(v) => std::env::set_var("TACHI_CLEAN_BIN", v),
        None => std::env::remove_var("TACHI_CLEAN_BIN"),
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn safe_merge_missing_worktree_warns_does_not_fail_merge() {
    // When the supplied worktree does not exist locally (the PR was opened
    // from a non-Tachi checkout), reclamation is skipped with a warning — it
    // must NOT fail the already-succeeded merge.
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let original_run_root = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", tmp.path());

    // A worktree path that does not exist on disk.
    let missing_worktree = tmp.path().join("does-not-exist-wt");

    let flow = "flow_reclaim-missing";
    write_verification(tmp.path(), flow, "passed", "deadbeef");
    let client = MockGhClient::new()
        .with_pr("o/r", ready_pr())
        .with_checks("o/r", 42, vec![]);
    let out = handle_github_safe_merge(
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false, // real merge
        Some(flow),
        &[],
        MergeGatePolicy::standard(),
        Some(missing_worktree.to_str().unwrap()),
        true, // reclaim_worktree
    )
    .await
    .expect("merge must succeed even though worktree is missing");

    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["merge_executed"], true, "merge still succeeds");
    assert_eq!(v["reclamation"]["attempted"], false);
    assert_eq!(v["reclamation"]["reclaimed"], false);
    assert_eq!(v["reclamation"]["skipped"], "worktree_missing");
    // The best-effort skip is recorded as a reclamation event with the skipped
    // kind (not the success kind, which would over-count successful reclaims).
    let events = std::fs::read_to_string(tmp.path().join(flow).join("events.jsonl")).unwrap();
    let reclaim_event = events
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .find(|e| {
            e.get("event").and_then(|v| v.as_str()).is_some_and(|s| {
                s == "github_safe_merge_reclaimed" || s == "github_safe_merge_reclaim_skipped"
            })
        })
        .expect("expected a reclamation event");
    assert_eq!(
        reclaim_event["event"], "github_safe_merge_reclaim_skipped",
        "worktree_missing skip should record github_safe_merge_reclaim_skipped; got: {events}"
    );
    assert_eq!(client.merge_calls().len(), 1);

    if let Some(v) = original_run_root {
        std::env::set_var("TACHI_RUN_ROOT", v);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
