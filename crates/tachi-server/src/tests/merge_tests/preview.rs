use super::*;

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

    let raw = tachi_merge_ops::handle_approve_merge(tachi_params::TachiApproveMergeParams {
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
