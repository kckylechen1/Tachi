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

#[tokio::test]
async fn approve_merge_preview_does_not_touch_repo_merge_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let worktree = tmp.path().join("feature-wt");

    run_git(tmp.path(), &["init", "repo"]);
    run_git(&repo, &["config", "user.email", "tachi@example.com"]);
    run_git(&repo, &["config", "user.name", "Tachi Test"]);
    run_git(&repo, &["checkout", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "base\n").expect("write base");
    run_git(&repo, &["add", "README.md"]);
    run_git(&repo, &["commit", "-m", "base"]);
    run_git(&repo, &["checkout", "-b", "feature-preview"]);
    std::fs::write(repo.join("feature.txt"), "feature\n").expect("write feature");
    run_git(&repo, &["add", "feature.txt"]);
    run_git(&repo, &["commit", "-m", "feature"]);
    run_git(&repo, &["checkout", "main"]);
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            worktree.to_str().unwrap(),
            "feature-preview",
        ],
    );

    let raw = crate::dispatch_ops::handle_approve_merge(crate::TachiApproveMergeParams {
        worktree: worktree.to_string_lossy().to_string(),
        branch: Some("feature-preview".to_string()),
        strategy: None,
        delete_worktree: false,
        confirm: false,
    })
    .await
    .expect("preview should succeed");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("preview JSON");

    assert_eq!(json["preview"], serde_json::json!(true));
    assert_eq!(json["can_merge"], serde_json::json!(true));
    assert_eq!(json["preview_engine"], "git merge-tree --write-tree");
    assert_eq!(json["requested_strategy"], "recursive");
    assert_eq!(json["preview_strategy"], "git merge-tree default");
    assert!(
        json["diff_stat"]
            .as_str()
            .is_some_and(|stat| stat.contains("feature.txt")),
        "expected diff stat to mention feature.txt: {json:#}"
    );
    assert!(
        !repo.join(".git/MERGE_HEAD").exists(),
        "non-mutating preview must not leave MERGE_HEAD in repo root"
    );
}

fn run_git(cwd: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
