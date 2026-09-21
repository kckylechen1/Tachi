//! Private Git fixtures for the accepted-head preservation boundary (#1907).
#[cfg(unix)]
use super::*;
use crate::test_support::EnvRestore;
use std::path::Path;

// Callers hold global_test_lock and keep their TempDir alive longer than guards.
// All inherited Git overrides (objects, refs, tracing, config, templates, etc.)
// are removed before production Git commands can run.
pub(super) struct PrivateGitEnvironment(Vec<EnvRestore>);
impl PrivateGitEnvironment {
    #[cfg(unix)]
    fn push(&mut self, guard: EnvRestore) {
        self.0.push(guard);
    }
}
impl Drop for PrivateGitEnvironment {
    fn drop(&mut self) {
        // Reverse nesting is essential when a removed Git key is then set.
        while let Some(guard) = self.0.pop() {
            drop(guard);
        }
    }
}

pub(super) fn private_git_environment(root: &Path) -> PrivateGitEnvironment {
    let guards: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        .map(|(key, _)| EnvRestore::remove_os(&key))
        .collect();
    let mut guards = PrivateGitEnvironment(guards);
    let config = root.join("fixture.gitconfig");
    std::fs::write(&config, "").unwrap();
    let templates = root.join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    guards.0.extend([
        EnvRestore::set("GIT_CONFIG_NOSYSTEM", "1"),
        EnvRestore::set_path("GIT_CONFIG_GLOBAL", &config),
        EnvRestore::set_path("GIT_TEMPLATE_DIR", &templates),
        EnvRestore::set("GIT_TERMINAL_PROMPT", "0"),
        EnvRestore::set("GIT_ALLOW_PROTOCOL", "file"),
    ]);
    guards
}

pub(super) fn git(root: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    assert!(cwd.starts_with(root));
    let path = if cfg!(windows) {
        std::env::var_os("PATH").unwrap_or_default()
    } else {
        std::ffi::OsString::from("/usr/bin:/bin")
    };
    std::process::Command::new("git")
        .env_clear()
        .env("PATH", path)
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("fixture.gitconfig"))
        .env("GIT_TEMPLATE_DIR", root.join("templates"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap()
}

pub(super) fn git_ok(root: &Path, cwd: &Path, args: &[&str]) -> String {
    let output = git(root, cwd, args);
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

pub(super) fn init_private_worktree(root: &Path, repo: &Path, wt: &Path, branch: &str) -> String {
    assert!(repo.starts_with(root) && wt.starts_with(root));
    std::fs::create_dir_all(repo).unwrap();
    git_ok(root, repo, &["init", "-b", "main"]);
    let git_dir = git_ok(root, repo, &["rev-parse", "--absolute-git-dir"]);
    assert_eq!(
        Path::new(&git_dir).canonicalize().unwrap(),
        repo.join(".git").canonicalize().unwrap()
    );
    std::fs::write(repo.join("base"), "base\n").unwrap();
    git_ok(root, repo, &["add", "base"]);
    git_ok(root, repo, &["commit", "-m", "base"]);
    git_ok(
        root,
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let common = git_ok(
        root,
        wt,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    assert_eq!(
        Path::new(&common).canonicalize().unwrap(),
        repo.join(".git").canonicalize().unwrap()
    );
    std::fs::write(wt.join("accepted"), "accepted\n").unwrap();
    git_ok(root, wt, &["add", "accepted"]);
    git_ok(root, wt, &["commit", "-m", "accepted PR head"]);
    git_ok(root, wt, &["rev-parse", "HEAD"])
}

#[cfg(unix)]
#[tokio::test]
async fn safe_merge_actual_cleaner_preserves_extra_commits() {
    run_private_case("extra").await;
}

#[cfg(unix)]
#[tokio::test]
async fn safe_merge_actual_cleaner_preserves_empty_commit() {
    run_private_case("empty").await;
}

#[cfg(unix)]
#[tokio::test]
async fn safe_merge_actual_cleaner_head_controls() {
    for case in [
        "detached",
        "branch",
        "missing_branch",
        "unknown_head",
        "unborn",
        "redirect",
        "accepted",
        "dirty",
        "holder",
        "ownership",
    ] {
        run_private_case(case).await;
    }
}

#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn run_private_case(case: &str) {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Each case owns a new source, branch, bare origin, registry, DB and HOME.
    // No subprocess ever receives an external remote URL.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut env = private_reclamation_environment(&root);
    let repo = root.join("repo");
    let wt = root.join("worktrees/fixture");
    let branch = "fixture/accepted";
    let head = init_private_worktree(&root, &repo, &wt, branch);
    let origin = root.join("origin.git");
    git_ok(&root, &root, &["init", "--bare", origin.to_str().unwrap()]);
    git_ok(
        &root,
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    // A synthetic squash is a distinct main commit, not ancestry evidence.
    git_ok(&root, &repo, &["merge", "--squash", branch]);
    git_ok(&root, &repo, &["commit", "-m", "squash PR"]);
    assert_eq!(
        git(
            &root,
            &repo,
            &["merge-base", "--is-ancestor", &head, "main"]
        )
        .status
        .code(),
        Some(1)
    );
    match case {
        "extra" => {
            std::fs::write(wt.join("later"), "later work\n").unwrap();
            git_ok(&root, &wt, &["add", "later"]);
            git_ok(&root, &wt, &["commit", "-m", "later"]);
        }
        "empty" | "redirect" => {
            git_ok(
                &root,
                &wt,
                &["commit", "--allow-empty", "-m", "later empty"],
            );
        }
        "detached" => {
            git_ok(&root, &wt, &["checkout", "--detach"]);
        }
        "dirty" => {
            std::fs::write(wt.join("accepted"), "dirty\n").unwrap();
        }
        _ => {}
    }
    git_ok(&root, &repo, &["push", "origin", branch]);
    let db = root.join("home/.tachi/global/memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    drop(memcore::MemoryStore::open(db.to_str().unwrap()).unwrap());
    tachi_clean::registry::register_worktree(tachi_clean::registry::RegisterOptions {
        path: wt.clone(),
        repo_root: repo.clone(),
        branch: branch.to_string(),
        dispatch_id: None,
        pr: Some("42".to_string()),
        output: tachi_clean::registry::RegisterOutputFormat::Json,
    })
    .unwrap();
    if case == "unborn" {
        git_ok(
            &root,
            &wt,
            &["symbolic-ref", "HEAD", "refs/heads/fixture/unborn"],
        );
    }
    let registry_path = root.join("home/.tachi/worktrees.json");
    let marker_path = wt.join(".tachi-worktree.json");
    if case == "ownership" {
        let mut marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        marker["inode"] = serde_json::json!(0);
        std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    }
    let registry = std::fs::read(&registry_path).unwrap();
    let marker = std::fs::read(&marker_path).unwrap();
    if case == "redirect" {
        // A private decoy has the accepted head/branch while the actual target
        // has an extra commit. Ambient Git selectors must not substitute it.
        let decoy = root.join("decoy");
        git_ok(
            &root,
            &root,
            &[
                "clone",
                "--no-hardlinks",
                repo.to_str().unwrap(),
                decoy.to_str().unwrap(),
            ],
        );
        git_ok(&root, &decoy, &["checkout", "-B", branch, &head]);
        env.push(EnvRestore::set_path("GIT_DIR", &decoy.join(".git")));
        env.push(EnvRestore::set_path("GIT_COMMON_DIR", &decoy.join(".git")));
        env.push(EnvRestore::set_path("GIT_WORK_TREE", &decoy));
    }
    let mut pr = ready_pr();
    pr.head_sha = match case {
        "unknown_head" => "0000000000000000000000000000000000000000".to_string(),
        _ => head.clone(),
    };
    pr.head_ref = match case {
        "missing_branch" => None,
        "branch" => Some("fixture/other".to_string()),
        _ => Some(branch.to_string()),
    };
    let flow = "flow_private_reclamation";
    write_verification(&root.join("runs"), flow, "passed", &pr.head_sha);
    seed_full_passed_set(&root.join("home/.tachi"), flow, &pr.head_sha, None);
    let server = verification_server(&root.join("home/.tachi"));
    seed_verification_claim(&server, flow, &pr.head_sha);
    let client = MockGhClient::new()
        .with_pr("o/r", pr)
        .with_checks("o/r", 42, vec![]);
    // The empty-commit case uses real registry discovery, not an explicit path.
    let path = if case == "empty" {
        None
    } else {
        Some(wt.to_str().unwrap())
    };
    let result = handle_github_safe_merge_with_holder_gate(
        &server,
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        Some(flow),
        &[],
        MergeGatePolicy::standard(),
        path,
        true,
        &|_| {
            if case == "holder" {
                Err("fixture holder is Held".to_string())
            } else {
                Ok(())
            }
        },
    )
    .await
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&result).unwrap();
    let registered =
        git_ok(&root, &repo, &["worktree", "list", "--porcelain"]).contains(wt.to_str().unwrap());
    let local = git(
        &root,
        &repo,
        &["show-ref", "--verify", "refs/heads/fixture/accepted"],
    )
    .status
    .success();
    let remote = git(
        &root,
        &origin,
        &["show-ref", "--verify", "refs/heads/fixture/accepted"],
    )
    .status
    .success();
    eprintln!("HEAD_FIXTURE_OBSERVED case={case} directory={} registration={registered} local={local} remote={remote} result={value}", wt.exists());
    assert_eq!(value["merge_executed"], true);
    if case == "accepted" {
        assert_eq!(value["reclamation"]["reclaimed"], true);
        assert!(!wt.exists() && !registered && !local && !remote);
    } else {
        assert!(
            wt.exists() && registered && local && remote,
            "{case}: all private resources must survive"
        );
        assert_eq!(std::fs::read(&registry_path).unwrap(), registry);
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker);
        assert_eq!(value["reclamation"]["reclaimed"], false);
        if case == "dirty" || case == "ownership" {
            let needle = if case == "dirty" {
                "dirty worktree"
            } else {
                "worktree marker and registry ownership records differ"
            };
            assert!(value["reclamation"]["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason.as_str().unwrap().contains(needle)));
        } else if case == "holder" {
            assert_eq!(value["reclamation"]["skipped"], "holder_evidence_refused");
        } else {
            assert_eq!(
                value["reclamation"]["skipped"],
                "worktree_head_evidence_refused"
            );
            let reason = match case {
                "detached" => "worktree_branch_unavailable",
                "unborn" => "worktree_head_unavailable",
                "missing_branch" => "accepted_branch_missing",
                "branch" => "worktree_branch_mismatch",
                _ => "worktree_head_mismatch",
            };
            assert_eq!(value["reclamation"]["error"], reason);
        }
    }
    drop(env);
}

#[cfg(unix)]
fn private_reclamation_environment(root: &Path) -> PrivateGitEnvironment {
    let mut env = private_git_environment(root);
    for (key, suffix) in [
        ("HOME", "home"),
        ("USERPROFILE", "home"),
        ("TACHI_HOME", "home/.tachi"),
        ("SIGIL_HOME", "home/.tachi"),
        ("TACHI_APP_HOME", "home/.tachi"),
        ("TACHI_RUN_ROOT", "runs"),
        ("TACHI_WORKTREES_ROOT", "worktrees"),
        ("XDG_CONFIG_HOME", "config"),
        ("CARGO_HOME", "cargo"),
        ("RUSTUP_HOME", "rustup"),
        ("TMPDIR", "tmp"),
    ] {
        let path = root.join(suffix);
        std::fs::create_dir_all(&path).unwrap();
        env.push(EnvRestore::set_path(key, &path));
    }
    env.push(EnvRestore::remove("TACHI_CLEAN_BIN"));
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let lsof = bin.join("lsof");
    std::fs::write(&lsof, "#!/bin/sh\n[ \"$1\" = '+D' ] && [ \"$2\" = \"$TACHI_WORKTREES_ROOT/fixture\" ] || exit 92\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&lsof, std::fs::Permissions::from_mode(0o700)).unwrap();
    env.push(EnvRestore::set(
        "PATH",
        &format!("{}:/usr/bin:/bin", bin.display()),
    ));
    env
}

#[cfg(unix)]
#[tokio::test]
async fn safe_merge_actual_cleaner_refuses_ambiguous_clones() {
    run_mapping_case("ambiguous").await;
}

#[cfg(unix)]
#[tokio::test]
async fn safe_merge_actual_cleaner_mapping_controls() {
    for case in [
        "explicit",
        "single",
        "empty",
        "absent",
        "malformed",
        "unreadable",
        "no_branch",
        "stale",
        "duplicate",
    ] {
        run_mapping_case(case).await;
    }
}

#[cfg(unix)]
#[allow(clippy::await_holding_lock)]
async fn run_mapping_case(case: &str) {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let env = private_reclamation_environment(&root);
    let repo_a = root.join("repo-a");
    let repo_b = root.join("repo-b");
    let wt_a = root.join("worktrees/fixture");
    let wt_b = root.join("worktrees/clone");
    let branch = "fixture/accepted";
    let head = init_private_worktree(&root, &repo_a, &wt_a, branch);
    // Copy objects, not a shared object store; both Git common directories are
    // fixture-owned. This local source is replaced by a private bare origin
    // before the production cleaner can execute any remote branch operation.
    git_ok(
        &root,
        &root,
        &[
            "clone",
            "--no-hardlinks",
            repo_a.to_str().unwrap(),
            repo_b.to_str().unwrap(),
        ],
    );
    assert_eq!(
        Path::new(&git_ok(
            &root,
            &repo_b,
            &["rev-parse", "--absolute-git-dir"]
        ))
        .canonicalize()
        .unwrap(),
        repo_b.join(".git").canonicalize().unwrap()
    );
    assert!(!repo_b.join(".git/objects/info/alternates").exists());
    git_ok(
        &root,
        &repo_b,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            wt_b.to_str().unwrap(),
            &head,
        ],
    );
    assert_eq!(
        Path::new(&git_ok(
            &root,
            &wt_b,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ))
        .canonicalize()
        .unwrap(),
        repo_b.join(".git").canonicalize().unwrap()
    );
    assert_ne!(repo_a.join(".git"), repo_b.join(".git"));
    assert_eq!(git_ok(&root, &wt_b, &["rev-parse", "HEAD"]), head);
    let origin_a = root.join("origin-a.git");
    let origin_b = root.join("origin-b.git");
    for (repo, origin, action) in [(&repo_a, &origin_a, "add"), (&repo_b, &origin_b, "set-url")] {
        git_ok(&root, &root, &["init", "--bare", origin.to_str().unwrap()]);
        git_ok(
            &root,
            repo,
            &["remote", action, "origin", origin.to_str().unwrap()],
        );
        assert_eq!(
            git_ok(&root, repo, &["remote", "get-url", "origin"]),
            origin.to_str().unwrap()
        );
        git_ok(&root, repo, &["push", "origin", branch]);
    }
    // Clear evidence is restricted to these two private paths. No actual
    // machine process census or process signalling occurs in this fixture.
    std::fs::write(root.join("bin/lsof"), "#!/bin/sh\n[ \"$1\" = '+D' ] || exit 92\ncase \"$2\" in\n\"$TACHI_WORKTREES_ROOT/fixture\"|\"$TACHI_WORKTREES_ROOT/clone\") exit 1;;\n*) exit 92;;\nesac\n").unwrap();
    let db = root.join("home/.tachi/global/memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    drop(memcore::MemoryStore::open(db.to_str().unwrap()).unwrap());
    if !matches!(case, "empty" | "absent") {
        for (repo, wt) in [(&repo_a, &wt_a), (&repo_b, &wt_b)] {
            if case == "single" && wt == &wt_b {
                continue;
            }
            tachi_clean::registry::register_worktree(tachi_clean::registry::RegisterOptions {
                path: wt.clone(),
                repo_root: repo.clone(),
                branch: branch.to_string(),
                dispatch_id: None,
                pr: Some("42".to_string()),
                output: tachi_clean::registry::RegisterOutputFormat::Json,
            })
            .unwrap();
        }
    }
    let registry_path = root.join("home/.tachi/worktrees.json");
    match case {
        "empty" => std::fs::write(&registry_path, "{\"version\":1,\"worktrees\":[]}").unwrap(),
        "malformed" => std::fs::write(&registry_path, "not JSON").unwrap(),
        "unreadable" => {
            std::fs::remove_file(&registry_path).unwrap();
            std::fs::create_dir(&registry_path).unwrap();
        }
        _ => {}
    }
    if matches!(case, "stale" | "duplicate") {
        let mut rows: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        if case == "stale" {
            rows["worktrees"][1]["path"] = serde_json::json!(root.join("worktrees/missing"));
        } else {
            rows["worktrees"][1] = rows["worktrees"][0].clone();
        }
        std::fs::write(&registry_path, serde_json::to_vec(&rows).unwrap()).unwrap();
    }
    let registry = std::fs::read(&registry_path).ok();
    let markers = [
        std::fs::read(wt_a.join(".tachi-worktree.json")).ok(),
        std::fs::read(wt_b.join(".tachi-worktree.json")).ok(),
    ];
    let flow = "flow_private_mapping";
    write_verification(&root.join("runs"), flow, "passed", &head);
    seed_full_passed_set(&root.join("home/.tachi"), flow, &head, None);
    let server = verification_server(&root.join("home/.tachi"));
    seed_verification_claim(&server, flow, &head);
    let mut pr = ready_pr();
    pr.head_sha = head;
    pr.head_ref = if case == "no_branch" {
        None
    } else {
        Some(branch.to_string())
    };
    let client = MockGhClient::new()
        .with_pr("o/r", pr)
        .with_checks("o/r", 42, vec![]);
    let result = handle_github_safe_merge_with_holder_gate(
        &server,
        &client,
        "o/r",
        42,
        MergeStrategy::Squash,
        false,
        Some(flow),
        &[],
        MergeGatePolicy::standard(),
        if case == "explicit" {
            Some(wt_b.to_str().unwrap())
        } else {
            None
        },
        true,
        &|_| Ok(()),
    )
    .await
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&result).unwrap();
    // Observe all actual resource surfaces BEFORE checking the new refusal.
    // A first-match mutant must reach deletion, not just a schema assertion.
    let mut observations = Vec::new();
    for (repo, wt, origin) in [(&repo_a, &wt_a, &origin_a), (&repo_b, &wt_b, &origin_b)] {
        observations.push([
            wt.is_dir(),
            git_ok(&root, repo, &["worktree", "list", "--porcelain"])
                .contains(wt.to_str().unwrap()),
            git(
                &root,
                repo,
                &["show-ref", "--verify", "refs/heads/fixture/accepted"],
            )
            .status
            .success(),
            git(
                &root,
                origin,
                &["show-ref", "--verify", "refs/heads/fixture/accepted"],
            )
            .status
            .success(),
        ]);
    }
    eprintln!(
        "MAPPING_FIXTURE_OBSERVED case={case} clone_a={:?} clone_b={:?} result={value}",
        observations[0], observations[1]
    );
    assert_eq!(value["merge_executed"], true);
    if matches!(case, "explicit" | "single") {
        let selected = if case == "explicit" { 1 } else { 0 };
        assert_eq!(observations[selected], [false; 4]);
        assert_eq!(observations[1 - selected], [true; 4]);
        assert_eq!(value["reclamation"]["reclaimed"], true);
    } else {
        assert_eq!(observations, vec![[true; 4], [true; 4]]);
        assert_eq!(std::fs::read(&registry_path).ok(), registry);
        assert_eq!(
            std::fs::read(wt_a.join(".tachi-worktree.json")).ok(),
            markers[0]
        );
        assert_eq!(
            std::fs::read(wt_b.join(".tachi-worktree.json")).ok(),
            markers[1]
        );
        let reason = match case {
            "ambiguous" | "stale" | "duplicate" => "worktree_mapping_ambiguous",
            "malformed" | "unreadable" => "worktree_mapping_unavailable",
            _ => "no_worktree_mapped",
        };
        assert_eq!(
            value["reclamation"],
            serde_json::json!({"attempted":false,"reclaimed":false,"skipped":reason})
        );
        if matches!(
            case,
            "ambiguous" | "stale" | "duplicate" | "malformed" | "unreadable"
        ) {
            let events =
                std::fs::read_to_string(root.join("runs").join(flow).join("events.jsonl")).unwrap();
            assert!(events.contains("github_safe_merge_reclaim_skipped"));
            assert!(events.contains(reason));
            assert!(!value["reclamation"]
                .to_string()
                .contains(wt_a.to_str().unwrap()));
            assert!(!value["reclamation"]
                .to_string()
                .contains(wt_b.to_str().unwrap()));
        }
    }
    drop(env);
}
