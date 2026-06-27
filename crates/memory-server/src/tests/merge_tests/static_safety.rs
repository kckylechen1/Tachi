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
fn approve_merge_rejects_branch_that_looks_like_git_option() {
    let err = crate::dispatch_ops::validate_merge_branch_name("--ff-only")
        .expect_err("option-like branch should be rejected");

    assert!(err.contains("git options"), "unexpected error: {err}");
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
