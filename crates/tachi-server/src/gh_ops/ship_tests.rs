use super::*;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

struct IsolatedRepo {
    dir: tempfile::TempDir,
}

impl IsolatedRepo {
    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// A genuinely empty directory, created once per test binary and never
/// written into. Used as `GIT_TEMPLATE_DIR` so `git init` cannot pick up
/// ambient hook/config templates from the host's real template dir.
fn empty_template_dir() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("empty git template dir"))
        .path()
}

/// Applies per-subprocess git isolation env to a single `Command` invocation.
/// Every fixture git call (from `git init` onward) goes through `run_git` /
/// `git_ok`, both of which call this — so isolation is per-process-spawn, not
/// a mutation of the shared test-process env (which would race under `cargo
/// test`'s default same-process multithreading; nextest's per-test process
/// model made the old process-level guard *usually* safe but not correctly
/// so).
///
/// Invariant: exact commit-message assertions must not depend on the host's
/// `prepare-commit-msg` / trailer tooling or ambient hook templates (#1377).
fn apply_git_isolation(cmd: &mut Command) {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TEMPLATE_DIR", empty_template_dir());
}

fn pin_empty_hooks_path(repo: &Path) {
    // Must be a real empty directory — `hooksPath=/dev/null` is not a dir and
    // can stall git on some platforms. Keep it under `.git/` so `git add --all`
    // never stages it into the fixture commits.
    let hooks = repo.join(".git").join("tachi-empty-hooks");
    std::fs::create_dir_all(&hooks).expect("create empty hooks dir");
    run_git(
        repo,
        &[
            "config",
            "core.hooksPath",
            hooks.to_str().expect("utf8 hooks path"),
        ],
    );
}

fn init_repo(files: &[(&str, &str)], branch: &str) -> IsolatedRepo {
    let tmp = tempfile::tempdir().expect("temp repo");
    run_git(tmp.path(), &["init"]);
    run_git(tmp.path(), &["config", "user.email", "ship@example.test"]);
    run_git(tmp.path(), &["config", "user.name", "Ship Tester"]);
    pin_empty_hooks_path(tmp.path());
    for (path, body) in files {
        write_file(tmp.path(), path, body);
    }
    run_git(tmp.path(), &["add", "--all"]);
    run_git(tmp.path(), &["commit", "-m", "initial"]);
    run_git(tmp.path(), &["branch", "-M", "main"]);
    if branch != "main" {
        run_git(tmp.path(), &["checkout", "-b", branch]);
    }
    IsolatedRepo { dir: tmp }
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
    let mut cmd = Command::new("git");
    apply_git_isolation(&mut cmd);
    let output = cmd
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
    let mut cmd = Command::new("git");
    apply_git_isolation(&mut cmd);
    cmd.arg("-C")
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

fn init_contract_repo(subjects: &[&str], branch: &str) -> IsolatedRepo {
    let tmp = tempfile::tempdir().expect("temp repo");
    run_git(tmp.path(), &["init"]);
    run_git(tmp.path(), &["config", "user.email", "ship@example.test"]);
    run_git(tmp.path(), &["config", "user.name", "Ship Tester"]);
    pin_empty_hooks_path(tmp.path());
    write_file(tmp.path(), "README.md", "seed\n");
    run_git(tmp.path(), &["add", "--all"]);
    run_git(tmp.path(), &["commit", "-m", "initial"]);
    run_git(tmp.path(), &["branch", "-M", "main"]);
    if branch != "main" {
        run_git(tmp.path(), &["checkout", "-b", branch]);
    }
    for (index, subject) in subjects.iter().enumerate() {
        write_file(tmp.path(), "file.txt", &format!("content {index}\n"));
        run_git(tmp.path(), &["add", "--all"]);
        run_git(tmp.path(), &["commit", "-m", subject]);
    }
    IsolatedRepo { dir: tmp }
}

fn contract_params(repo: &Path, issue_ref: Option<&str>) -> TachiGhParams {
    TachiGhParams {
        action: "ship".to_string(),
        cwd: Some(repo.to_string_lossy().to_string()),
        issue_ref: issue_ref.map(str::to_string),
        ..Default::default()
    }
}

fn setup_origin_remote(repo: &Path) -> tempfile::TempDir {
    let remote = tempfile::tempdir().expect("bare remote");
    run_git(remote.path(), &["init", "--bare"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            remote.path().to_str().expect("utf8 remote path"),
        ],
    );
    run_git(repo, &["push", "-u", "origin", "main"]);
    remote
}

#[tokio::test]
async fn ship_g2_contract_dry_run_discriminates_from_mechanical() {
    let repo = init_contract_repo(
        &["First contract commit", "Second contract commit"],
        "feature/contract-g2",
    );
    let params = contract_params(repo.path(), Some("#521"));

    let raw = handle_github_ship_inner(None, &params)
        .await
        .expect("contract dry-run ok");
    let value: Value = serde_json::from_str(&raw).expect("contract dry-run JSON");

    assert_eq!(value["status"], "dry_run");
    assert_eq!(value["mode"], "contract");
    assert_eq!(value["base"], "main");
    assert_eq!(value["baseref"], "main");
    assert_eq!(value["commit_count"], 2);
    assert_eq!(value["pr_title"], "Second contract commit");
    assert_eq!(value["pr_exists_check"], "deferred");
    let preview = value["pr_body_preview"].as_str().expect("body preview");
    assert!(preview.contains("Refs #521"), "{preview}");
    assert!(preview.contains("First contract commit"), "{preview}");
    assert!(preview.contains("Second contract commit"), "{preview}");
    assert!(
        preview.contains("_(not run — record the full-suite command via tests_run)_"),
        "{preview}"
    );
}

#[test]
fn ship_g3_resolve_base_ref_prefers_origin_then_local_then_missing() {
    let repo = init_contract_repo(&[], "feature/contract-g3");
    let _remote = setup_origin_remote(repo.path());

    let origin = resolve_base_ref(repo.path(), "main").expect("origin main");
    assert_eq!(
        origin,
        ResolvedBase {
            base: "main".to_string(),
            baseref: "origin/main".to_string(),
            base_pushed_needed: false,
        }
    );

    let local_repo = init_contract_repo(&[], "feature/contract-g3-local");
    let local = resolve_base_ref(local_repo.path(), "main").expect("local main");
    assert_eq!(
        local,
        ResolvedBase {
            base: "main".to_string(),
            baseref: "main".to_string(),
            base_pushed_needed: true,
        }
    );

    let missing = resolve_base_ref(local_repo.path(), "goal/521").expect_err("missing base");
    assert!(missing.starts_with("base_not_found:"), "{missing}");
}

#[tokio::test]
async fn ship_g4_contract_mode_on_base_branch_is_refused() {
    let repo = init_contract_repo(&["extra on main"], "main");
    let params = contract_params(repo.path(), Some("#521"));

    let err = handle_github_ship_inner(None, &params)
        .await
        .expect_err("contract on main should fail");

    assert!(err.contains("protected_branch"), "{err}");
}

#[tokio::test]
async fn ship_g4b_contract_mode_self_pr_refused() {
    let repo = init_contract_repo(&["slice commit"], "goal/522");
    let mut params = contract_params(repo.path(), Some("#522"));
    params.pr_base = Some("goal/522".to_string());

    let err = handle_github_ship_inner(None, &params)
        .await
        .expect_err("self-PR should fail");

    assert!(err.starts_with("self_pr:"), "{err}");
}

#[tokio::test]
async fn ship_g5_contract_mode_zero_commits_returns_nothing_to_ship() {
    let repo = init_contract_repo(&[], "feature/contract-g5");
    let params = contract_params(repo.path(), None);

    let err = handle_github_ship_inner(None, &params)
        .await
        .expect_err("zero commits should fail");

    assert!(err.starts_with("nothing_to_ship:"), "{err}");
}

#[test]
fn ship_g6_build_contract_pr_body_section_order_and_placeholders() {
    let with_issue = build_contract_pr_body(
        Some("#521"),
        &[
            "abc1234 First commit".to_string(),
            "def5678 Second commit".to_string(),
        ],
        &["cargo test -p tachi-server".to_string()],
    );
    assert_eq!(
        with_issue,
        "Refs #521\n\n\
## Commits (one bounded contract, batched)\n\
- abc1234 First commit\n\
- def5678 Second commit\n\
\n\
## Tested (full suite, once)\n\
- cargo test -p tachi-server\n\
\n\
## Not-tested\n\
_fill honest gaps_\n"
    );
    assert!(with_issue.starts_with("Refs #521\n\n"));
    assert!(with_issue.contains("## Commits (one bounded contract, batched)\n"));
    assert!(with_issue.contains("- abc1234 First commit\n"));
    assert!(with_issue.contains("- def5678 Second commit\n"));
    assert!(with_issue.contains("## Tested (full suite, once)\n"));
    assert!(with_issue.contains("- cargo test -p tachi-server\n"));
    assert!(with_issue.contains("## Not-tested\n"));
    assert!(with_issue.contains("_fill honest gaps_\n"));

    let without_tests =
        build_contract_pr_body(Some("#521"), &["abc1234 Only commit".to_string()], &[]);
    assert!(
        without_tests.contains("_(not run — record the full-suite command via tests_run)_"),
        "{without_tests}"
    );

    let without_issue = build_contract_pr_body(None, &["abc1234 Only commit".to_string()], &[]);
    assert!(!without_issue.contains("Refs "));
    assert!(without_issue.starts_with("## Commits (one bounded contract, batched)\n"));
}
