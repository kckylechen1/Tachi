#[test]
fn delete_worktree_is_blocked_when_branch_needs_human_review() {
    let no_upstream = tachi_merge_ops::evaluate_delete_worktree_safety(true, false, false);
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

    let local_only = tachi_merge_ops::evaluate_delete_worktree_safety(true, true, true);
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
