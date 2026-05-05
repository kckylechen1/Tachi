#[test]
fn approve_merge_rejects_protected_branch() {
    let err = crate::dispatch_ops::validate_static_merge_safety(
        "main",
        "/tmp/feature-worktree",
        "/tmp/repo-root",
    )
    .expect_err("protected branch must be rejected");

    assert!(err.contains("protected branch"), "unexpected error: {err}");
}

#[test]
fn approve_merge_rejects_repo_root_as_worktree() {
    let err = crate::dispatch_ops::validate_static_merge_safety(
        "feature/safe-merge",
        "/tmp/repo-root",
        "/tmp/repo-root",
    )
    .expect_err("repo root must not be accepted as merge worktree");

    assert!(
        err.contains("repository root/main worktree"),
        "unexpected error: {err}"
    );
    assert!(crate::dispatch_ops::worktree_equals_repo_root(
        "/tmp/repo-root",
        "/tmp/repo-root"
    ));
}

#[test]
fn delete_worktree_is_blocked_when_branch_needs_human_review() {
    let no_upstream = crate::dispatch_ops::evaluate_delete_worktree_safety(true, false, false);
    assert!(
        !no_upstream.allow_delete_worktree,
        "delete should be disabled when upstream is missing"
    );
    assert!(
        no_upstream.requires_human_review,
        "missing upstream should require human review"
    );
    assert!(
        no_upstream
            .safety_warnings
            .iter()
            .any(|w| w.contains("no upstream") || w.contains("upstream")),
        "expected upstream warning"
    );

    let local_only = crate::dispatch_ops::evaluate_delete_worktree_safety(true, true, true);
    assert!(
        !local_only.allow_delete_worktree,
        "delete should be disabled when local-only commits exist"
    );
    assert!(
        local_only.requires_human_review,
        "local-only commits should require human review"
    );
    assert!(
        local_only
            .safety_warnings
            .iter()
            .any(|w| w.contains("upstream") || w.contains("commits")),
        "expected local-only warning"
    );
}
