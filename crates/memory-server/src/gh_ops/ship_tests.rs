use super::*;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

fn init_repo(files: &[(&str, &str)], branch: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp repo");
    run_git(tmp.path(), &["init"]);
    run_git(tmp.path(), &["config", "user.email", "ship@example.test"]);
    run_git(tmp.path(), &["config", "user.name", "Ship Tester"]);
    for (path, body) in files {
        write_file(tmp.path(), path, body);
    }
    run_git(tmp.path(), &["add", "--all"]);
    run_git(tmp.path(), &["commit", "-m", "initial"]);
    run_git(tmp.path(), &["branch", "-M", "main"]);
    if branch != "main" {
        run_git(tmp.path(), &["checkout", "-b", branch]);
    }
    tmp
}

fn ship_params(repo: &std::path::Path, files: Vec<&str>, message: Option<&str>) -> TachiGhParams {
    TachiGhParams {
        action: "ship".to_string(),
        files: files.into_iter().map(str::to_string).collect(),
        commit_message: message.map(str::to_string),
        cwd: Some(repo.to_string_lossy().to_string()),
        ..Default::default()
    }
}

fn write_file(repo: &std::path::Path, path: &str, body: &str) {
    let path = repo.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write file");
}

fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn git_ok(repo: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("run git")
        .success()
}

fn head(repo: &std::path::Path) -> String {
    run_git(repo, &["rev-parse", "HEAD"]).trim().to_string()
}

fn changed_files(repo: &Path) -> BTreeSet<String> {
    run_git(
        repo,
        &[
            "-c",
            "core.quotePath=false",
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "HEAD",
        ],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

#[tokio::test]
async fn ship_gb1_dry_run_returns_plan_and_creates_nothing() {
    let repo = init_repo(&[("a.txt", "one\n")], "feature/ship-gb1");
    write_file(repo.path(), "a.txt", "one changed\n");
    let before = head(repo.path());
    let params = ship_params(repo.path(), vec!["a.txt"], Some("Ship dry run\n"));

    let raw = handle_github_ship_inner(None, &params)
        .await
        .expect("dry-run ok");
    let value: Value = serde_json::from_str(&raw).expect("ship dry-run JSON");

    assert_eq!(value["status"], "dry_run");
    assert_eq!(value["plan"]["branch"], "feature/ship-gb1");
    assert_eq!(value["plan"]["files"][0]["path"], "a.txt");
    assert_eq!(value["plan"]["files"][0]["state"], "modified");
    assert_eq!(value["plan"]["commit_message_len"], "Ship dry run\n".len());
    assert_eq!(head(repo.path()), before);
    assert!(git_ok(repo.path(), &["diff", "--cached", "--quiet"]));
}

#[tokio::test]
async fn ship_gb2_commits_exact_list_and_verbatim_message() {
    let repo = init_repo(
        &[("a.txt", "a\n"), ("b.txt", "b\n"), ("unlisted.txt", "c\n")],
        "feature/ship-gb2",
    );
    write_file(repo.path(), "a.txt", "a changed\n");
    write_file(repo.path(), "b.txt", "b changed\n");
    write_file(repo.path(), "unlisted.txt", "c changed\n");
    let message = "\nShip selected files\n# keep this literal comment line\n\nBody line one.\nBody line two.\n\nCo-Authored-By: Ship Bot <ship@example.test>\n\n";
    let mut params = ship_params(repo.path(), vec!["a.txt", "b.txt"], Some(message));
    params.confirm = true;

    let raw = handle_github_ship_inner(None, &params)
        .await
        .expect("ship ok");
    let value: Value = serde_json::from_str(&raw).expect("ship response JSON");

    assert_eq!(value["status"], "partial");
    assert_eq!(value["steps"]["pushed"], "no_remote");
    assert_eq!(
        changed_files(repo.path()),
        BTreeSet::from(["a.txt".to_string(), "b.txt".to_string()])
    );
    let status = run_git(
        repo.path(),
        &["status", "--porcelain=v1", "--", "unlisted.txt"],
    );
    assert!(status.starts_with(" M unlisted.txt"), "{status}");
    let object = run_git(repo.path(), &["cat-file", "commit", "HEAD"]);
    let committed_message = object
        .split_once("\n\n")
        .map(|(_, body)| body)
        .expect("commit object body");
    assert_eq!(committed_message, message);
    assert_eq!(
        run_git(repo.path(), &["log", "-1", "--format=%B"]),
        format!("{message}\n")
    );
}

#[tokio::test]
async fn ship_gb3_unchanged_listed_file_errors_without_commit_or_index() {
    let repo = init_repo(&[("a.txt", "one\n")], "feature/ship-gb3");
    let before = head(repo.path());
    let mut params = ship_params(repo.path(), vec!["a.txt"], Some("No unchanged\n"));
    params.confirm = true;

    let err = handle_github_ship_inner(None, &params)
        .await
        .expect_err("unchanged file should fail");

    assert!(err.contains("unchanged"), "{err}");
    assert!(err.contains("a.txt"), "{err}");
    assert_eq!(head(repo.path()), before);
    assert!(git_ok(repo.path(), &["diff", "--cached", "--quiet"]));
}

#[tokio::test]
async fn ship_gb4_main_branch_is_protected_even_when_confirmed() {
    let repo = init_repo(&[("a.txt", "one\n")], "main");
    write_file(repo.path(), "a.txt", "one changed\n");
    let mut params = ship_params(repo.path(), vec!["a.txt"], Some("No main\n"));
    params.confirm = true;

    let err = handle_github_ship_inner(None, &params)
        .await
        .expect_err("main should be protected");

    assert!(err.contains("protected_branch"), "{err}");
    assert!(git_ok(repo.path(), &["diff", "--cached", "--quiet"]));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ship_gb5_flow_id_appends_github_event_with_step_statuses() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let run_root = tempfile::tempdir().expect("run root");
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", run_root.path());
    let repo = init_repo(&[("a.txt", "one\n")], "feature/ship-gb5");
    write_file(repo.path(), "a.txt", "one changed\n");
    let mut params = ship_params(repo.path(), vec!["a.txt"], Some("Flow ship\n"));
    params.confirm = true;
    params.flow_id = Some("flow_ship_gb5".to_string());

    let raw = handle_github_ship_inner(None, &params)
        .await
        .expect("ship ok");
    let value: Value = serde_json::from_str(&raw).expect("ship response JSON");

    assert_eq!(value["steps"]["event_appended"], true);
    let run_dir = run_root.path().join("flow_ship_gb5");
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    assert!(events.contains("\"event\":\"github_ship_completed\""));
    assert!(events.contains("\"pushed\":\"no_remote\""));
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status JSON");
    assert_eq!(status["github"]["ship"]["status"], "partial");
    assert_eq!(status["github"]["ship"]["steps"]["pushed"], "no_remote");
    assert_eq!(status["github"]["ship"]["steps"]["event_appended"], true);
    assert!(events.contains("\"event_appended\":true"));

    if let Some(value) = original {
        std::env::set_var("TACHI_RUN_ROOT", value);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}

#[tokio::test]
async fn ship_gb6_no_origin_commits_and_returns_partial_json() {
    let repo = init_repo(&[("a.txt", "one\n")], "feature/ship-gb6");
    write_file(repo.path(), "a.txt", "one changed\n");
    let mut params = ship_params(repo.path(), vec!["a.txt"], Some("Local only\n"));
    params.confirm = true;

    let raw = handle_github_ship_inner(None, &params)
        .await
        .expect("ship without origin should not panic");
    let value: Value = serde_json::from_str(&raw).expect("valid JSON");

    assert_eq!(value["status"], "partial");
    assert_eq!(value["steps"]["pushed"], "no_remote");
    assert!(value["steps"]["committed"]["sha"]
        .as_str()
        .is_some_and(|sha| !sha.is_empty()));
}

#[tokio::test]
async fn ship_gb7_missing_commit_message_requires_caller_authorship() {
    let repo = init_repo(&[("a.txt", "one\n")], "feature/ship-gb7");
    write_file(repo.path(), "a.txt", "one changed\n");
    let mut params = ship_params(repo.path(), vec!["a.txt"], None);
    params.confirm = true;

    let err = handle_github_ship_inner(None, &params)
        .await
        .expect_err("missing commit message should fail");

    assert!(err.contains("commit_message"), "{err}");
    assert!(err.contains("caller must author"), "{err}");
    assert!(git_ok(repo.path(), &["diff", "--cached", "--quiet"]));
}

#[tokio::test]
async fn ship_gb8_rejects_absolute_and_parent_traversal_paths() {
    let repo = init_repo(&[("a.txt", "one\n")], "feature/ship-gb8");
    write_file(repo.path(), "a.txt", "one changed\n");
    let before = head(repo.path());

    let mut absolute = ship_params(repo.path(), vec!["/tmp/outside.txt"], Some("No absolute\n"));
    absolute.confirm = true;
    let err = handle_github_ship_inner(None, &absolute)
        .await
        .expect_err("absolute path should fail before staging");
    assert!(err.contains("absolute path"), "{err}");

    let mut traversal = ship_params(repo.path(), vec!["../outside.txt"], Some("No traversal\n"));
    traversal.confirm = true;
    let err = handle_github_ship_inner(None, &traversal)
        .await
        .expect_err("parent traversal should fail before staging");
    assert!(err.contains("path traversal"), "{err}");

    assert_eq!(head(repo.path()), before);
    assert!(git_ok(repo.path(), &["diff", "--cached", "--quiet"]));
}

#[cfg(unix)]
#[tokio::test]
async fn ship_gb8b_broken_symlink_is_detected_as_shippable() {
    let repo = init_repo(&[("a.txt", "one\n")], "feature/ship-gb8b");
    std::os::unix::fs::symlink("missing-target.txt", repo.path().join("broken-link"))
        .expect("create broken symlink");
    let mut params = ship_params(
        repo.path(),
        vec!["broken-link"],
        Some("Ship broken symlink\n"),
    );
    params.confirm = true;

    let raw = handle_github_ship_inner(None, &params)
        .await
        .expect("broken symlink should be shippable");
    let value: Value = serde_json::from_str(&raw).expect("ship response JSON");

    assert_eq!(value["status"], "partial");
    assert_eq!(
        changed_files(repo.path()),
        BTreeSet::from(["broken-link".to_string()])
    );
}

#[test]
fn ship_gb8c_pr_url_parser_supports_github_enterprise_output() {
    let raw = "Creating pull request...\nhttps://github.company.test/acme/project/pull/42\n";

    let (number, url) = parse_created_pr_url(raw).expect("parse enterprise PR URL");

    assert_eq!(number, 42);
    assert_eq!(url, "https://github.company.test/acme/project/pull/42");
}

#[tokio::test]
async fn ship_gb9_chinese_filename_ships_with_unquoted_staged_compare() {
    let repo = init_repo(&[("中文文件.txt", "one\n")], "feature/ship-gb9");
    write_file(repo.path(), "中文文件.txt", "one changed\n");
    let mut params = ship_params(
        repo.path(),
        vec!["中文文件.txt"],
        Some("Ship Chinese path\n"),
    );
    params.confirm = true;

    let raw = handle_github_ship_inner(None, &params)
        .await
        .expect("ship Chinese filename ok");
    let value: Value = serde_json::from_str(&raw).expect("ship response JSON");

    assert_eq!(value["status"], "partial");
    assert_eq!(value["steps"]["pushed"], "no_remote");
    assert!(value["steps"]["committed"]["sha"]
        .as_str()
        .is_some_and(|sha| !sha.is_empty()));
    assert_eq!(
        changed_files(repo.path()),
        BTreeSet::from(["中文文件.txt".to_string()])
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn ship_gb10_push_failure_after_commit_returns_partial_and_records_flow_event() {
    let _guard = crate::shell_ops::tachi_run_root_env_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let run_root = tempfile::tempdir().expect("run root");
    let original = std::env::var_os("TACHI_RUN_ROOT");
    std::env::set_var("TACHI_RUN_ROOT", run_root.path());
    let repo = init_repo(&[("a.txt", "one\n")], "feature/ship-gb10");
    write_file(repo.path(), "a.txt", "one changed\n");
    let missing_remote = repo.path().join("missing-remote.git");
    run_git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            missing_remote.to_str().expect("utf8 temp path"),
        ],
    );
    let mut params = ship_params(
        repo.path(),
        vec!["a.txt"],
        Some("Push fails after commit\n"),
    );
    params.confirm = true;
    params.flow_id = Some("flow_ship_gb10".to_string());

    let raw = handle_github_ship_inner(None, &params)
        .await
        .expect("push failure after commit returns partial JSON");
    let value: Value = serde_json::from_str(&raw).expect("ship response JSON");
    let sha = value["steps"]["committed"]["sha"]
        .as_str()
        .expect("committed sha");

    assert_eq!(value["status"], "partial");
    assert!(!sha.is_empty());
    assert!(value["steps"]["pushed"]["error"]
        .as_str()
        .is_some_and(|err| !err.is_empty()));
    assert_eq!(value["steps"]["pr"], "skipped");
    assert_eq!(value["steps"]["linked"], "skipped");
    assert_eq!(value["steps"]["event_appended"], true);
    assert_eq!(
        value["warnings"][0],
        format!(
            "commit {sha} created locally but push failed; push manually and open the PR yourself"
        )
    );
    let run_dir = run_root.path().join("flow_ship_gb10");
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
    let event: Value =
        serde_json::from_str(events.lines().next().expect("one event")).expect("event JSON");
    assert_eq!(event["event"], "github_ship_completed");
    assert_eq!(event["commit_sha"], sha);
    assert_eq!(event["steps"]["committed"]["sha"], sha);
    assert!(event["steps"]["pushed"]["error"]
        .as_str()
        .is_some_and(|err| !err.is_empty()));
    let status: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).unwrap())
            .expect("status JSON");
    assert_eq!(status["github"]["ship"]["commit_sha"], sha);
    assert_eq!(status["github"]["ship"]["steps"]["event_appended"], true);

    if let Some(value) = original {
        std::env::set_var("TACHI_RUN_ROOT", value);
    } else {
        std::env::remove_var("TACHI_RUN_ROOT");
    }
}
