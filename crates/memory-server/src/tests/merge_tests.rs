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

#[test]
fn approve_merge_uses_explicit_cleaner_binary_override() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let old_value = std::env::var_os("TACHI_CLEAN_BIN");
    std::env::set_var("TACHI_CLEAN_BIN", "/tmp/custom-tachi-clean");

    let resolved = crate::dispatch_ops::resolve_tachi_clean_bin();

    match old_value {
        Some(value) => std::env::set_var("TACHI_CLEAN_BIN", value),
        None => std::env::remove_var("TACHI_CLEAN_BIN"),
    }
    assert_eq!(
        resolved,
        std::path::PathBuf::from("/tmp/custom-tachi-clean")
    );
}
