//! #1906: actual CLI reconciliation must preserve unmerged CLOSED-PR resources.
//! Every subprocess starts with an empty environment and private Git/config roots.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// tachi#1988: macOS runs a Gatekeeper/XProtect scan (syspolicyd ->
/// XprotectService, serialized machine-wide) on the first exec of every newly
/// written executable file, ~0.15-0.4 s idle and several seconds under load.
/// The verdict is cached per file, so re-exec (also through a symlink) costs
/// ~10 ms. Each distinct stub body is therefore written once per test here and
/// every case's `bin/<name>` links to it: scan cost scales with distinct
/// bodies, not with cases.
struct StubStore {
    dir: PathBuf,
    bodies: std::cell::RefCell<Vec<String>>,
}
impl StubStore {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("tachi-1988-stubs-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        // Link targets must be absolute: `temp_dir()` is `TMPDIR` verbatim, and
        // a relative one (`TMPDIR=.`) would resolve beneath each `bin/` and
        // dangle. Canonical, like the fixture roots.
        let dir = dir.canonicalize().unwrap();
        Self {
            dir,
            bodies: Default::default(),
        }
    }
    /// The store file holding exactly `body`, written on first request.
    fn install(&self, body: &str) -> PathBuf {
        let mut bodies = self.bodies.borrow_mut();
        let index = match bodies.iter().position(|known| known == body) {
            Some(index) => index,
            None => {
                let path = self.dir.join(format!("stub-{}", bodies.len()));
                fs::write(&path, body).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                bodies.push(body.to_string());
                bodies.len() - 1
            }
        };
        self.dir.join(format!("stub-{index}"))
    }
}
impl Drop for StubStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct Fixture<'s>(PathBuf, &'s StubStore);
impl Drop for Fixture<'_> {
    fn drop(&mut self) {
        // Only the directory created exclusively by this fixture is owned here.
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture<'_> {
    fn command(&self, program: &str, cwd: &Path) -> Command {
        assert!(cwd.starts_with(&self.0));
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .current_dir(cwd)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
            )
            .env("HOME", self.0.join("home"))
            .env("USERPROFILE", self.0.join("home"))
            .env("TACHI_HOME", self.0.join("home/.tachi"))
            .env("TACHI_WORKTREES_ROOT", self.0.join("worktrees"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("CARGO_HOME", self.0.join("cargo"))
            .env("RUSTUP_HOME", self.0.join("rustup"))
            .env("TMPDIR", self.0.join("tmp"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.0.join("gitconfig"))
            .env("GIT_TEMPLATE_DIR", self.0.join("templates"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("FIXTURE_ROOT", &self.0);
        cmd
    }
    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command("/usr/bin/git", cwd)
            .args(args)
            .output()
            .unwrap()
    }
    fn git_ok(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.git(cwd, args);
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
    fn cli(&self, args: &[&str]) -> serde_json::Value {
        let output = self
            .command(env!("CARGO_BIN_EXE_tachi-clean"), &self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "CLI {args:?}: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
    /// `bin/<name>` now runs `body`. A previous link is replaced, never
    /// written through, so the shared store file keeps its body.
    fn script(&self, name: &str, body: &str) {
        let path = self.0.join("bin").join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("replace stub {}: {error}", path.display()),
        }
        let target = self.1.install(body);
        std::os::unix::fs::symlink(&target, &path).unwrap();
        // A dangling or misdirected link would silently turn the stub into a
        // spawn failure; prove the link runs exactly the installed body.
        let resolved = fs::canonicalize(&path)
            .unwrap_or_else(|error| panic!("{name} stub link {} dangles: {error}", path.display()));
        assert_eq!(resolved, target, "{name} stub link");
    }
}

#[test]
fn closed_pr_cli_preserves_unmerged_worktree_and_refs() {
    let root = std::env::temp_dir().join(format!("tachi-1906-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let stubs = StubStore::new();
    let fixture = Fixture(root.canonicalize().unwrap(), &stubs);
    for dir in [
        "bin",
        "home/.tachi/global",
        "config",
        "cargo",
        "rustup",
        "tmp",
        "templates",
        "repo",
        "worktrees",
    ] {
        fs::create_dir_all(fixture.0.join(dir)).unwrap();
    }
    fs::write(fixture.0.join("gitconfig"), "").unwrap();
    // Explicit private DB; no runtime configuration or live database is consulted.
    drop(
        memcore::MemoryStore::open(
            fixture
                .0
                .join("home/.tachi/global/memory.db")
                .to_str()
                .unwrap(),
        )
        .unwrap(),
    );
    fixture.script("gh", "#!/bin/sh\n[ \"$*\" = 'pr view 7 --json state,headRefOid,headRefName' ] || exit 91\n/bin/cat \"$FIXTURE_ROOT/pr-state\"\n");
    fixture.script("lsof", "#!/bin/sh\n[ \"$1\" = '+D' ] && [ \"$2\" = \"$FIXTURE_ROOT/worktrees/closed\" ] || exit 92\nprintf 'clear\\n' >> \"$FIXTURE_ROOT/holder-probes\"\nexit 1\n");
    let repo = fixture.0.join("repo");
    let origin = fixture.0.join("origin.git");
    let wt = fixture.0.join("worktrees/closed");
    fixture.git_ok(&fixture.0, &["init", "--bare", origin.to_str().unwrap()]);
    fixture.git_ok(&repo, &["init", "-b", "main"]);
    assert_eq!(
        fixture.git_ok(&repo, &["rev-parse", "--absolute-git-dir"]),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(repo.join("base"), "base\n").unwrap();
    fixture.git_ok(&repo, &["add", "base"]);
    fixture.git_ok(&repo, &["commit", "-m", "base"]);
    fixture.git_ok(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    fixture.git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "fixture/closed",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    assert_eq!(
        fixture.git_ok(
            &wt,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(wt.join("unique"), "unmerged work\n").unwrap();
    fixture.git_ok(&wt, &["add", "unique"]);
    fixture.git_ok(&wt, &["commit", "-m", "unique unmerged work"]);
    let head = fixture.git_ok(&wt, &["rev-parse", "HEAD"]);
    assert_eq!(
        fixture
            .git(&repo, &["merge-base", "--is-ancestor", &head, "main"])
            .status
            .code(),
        Some(1)
    );
    fixture.git_ok(&repo, &["push", "origin", "fixture/closed"]);
    fixture.cli(&[
        "wt-register",
        wt.to_str().unwrap(),
        "--repo",
        repo.to_str().unwrap(),
        "--branch",
        "fixture/closed",
        "--pr",
        "7",
        "--json",
    ]);
    let registry_path = fixture.0.join("home/.tachi/worktrees.json");
    let marker_path = wt.join(".tachi-worktree.json");
    let registry = fs::read(&registry_path).unwrap();
    let marker = fs::read(&marker_path).unwrap();
    fs::write(fixture.0.join("pr-state"), "{\"state\":\"CLOSED\"}").unwrap();
    let dry = fixture.cli(&["wt-reconcile", "--dry-run", "--json"]);
    // Run apply before asserting either report: the old-code mutant must reach
    // actual private deletion, rather than fail only on a dry-run schema difference.
    let apply = fixture.cli(&["wt-reconcile", "--force", "--json"]);
    let registered = fixture
        .git_ok(&repo, &["worktree", "list", "--porcelain"])
        .contains(wt.to_str().unwrap());
    let local = fixture.git(
        &repo,
        &["show-ref", "--verify", "refs/heads/fixture/closed"],
    );
    let remote = fixture.git(
        &origin,
        &["show-ref", "--verify", "refs/heads/fixture/closed"],
    );
    eprintln!("CLOSED_FIXTURE_OBSERVED directory={} git_registration={} local_ref={} remote_ref={} registry={} marker={} dry={dry} apply={apply}", wt.exists(), registered, local.status.success(), remote.status.success(), fs::read(&registry_path).ok().as_ref() == Some(&registry), fs::read(&marker_path).ok().as_ref() == Some(&marker));
    assert!(wt.is_dir() && registered && local.status.success() && remote.status.success());
    assert_eq!(fs::read(&registry_path).unwrap(), registry);
    assert_eq!(fs::read(&marker_path).unwrap(), marker);
    assert_eq!(fixture.git_ok(&wt, &["rev-parse", "HEAD"]), head);
    for report in [&dry, &apply] {
        assert_eq!(
            report["refused"][0]["reasons"],
            serde_json::json!(["closed_pr_reclaimability_unproven"])
        );
        assert_eq!(report["reconciled"], serde_json::json!([]));
    }
    assert!(
        !fixture.0.join("holder-probes").exists(),
        "CLOSED must refuse before planning"
    );
    // MERGED still reaches the existing shared planner: dirty work remains refused.
    fs::write(wt.join("unique"), "dirty\n").unwrap();
    fs::write(fixture.0.join("pr-state"), serde_json::to_vec(&serde_json::json!({"state":"MERGED", "headRefOid":head, "headRefName":"fixture/closed"})).unwrap()).unwrap();
    let merged = fixture.cli(&["wt-reconcile", "--force", "--json"]);
    assert!(merged["refused"][0]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason.as_str().unwrap().contains("dirty worktree")));
    assert!(wt.is_dir());
    fs::write(wt.join("unique"), "unmerged work\n").unwrap();
    fixture.script("lsof", "#!/bin/sh\nexit 92\n");
    let unknown_holder = fixture.cli(&["wt-reconcile", "--force", "--json"]);
    assert!(unknown_holder["refused"][0]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason.as_str().unwrap().contains("inconclusive evidence")));
    assert!(wt.is_dir());
    assert_eq!(fs::read(&registry_path).unwrap(), registry);
    assert_eq!(fs::read(&marker_path).unwrap(), marker);
}

fn merged_cli_case(stubs: &StubStore, registered_pr: bool, case: &str) {
    let root = std::env::temp_dir().join(format!("tachi-merged-head-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let fixture = Fixture(root.canonicalize().unwrap(), stubs);
    for dir in [
        "bin",
        "home/.tachi/global",
        "config",
        "cargo",
        "rustup",
        "tmp",
        "templates",
        "repo",
        "worktrees",
    ] {
        fs::create_dir_all(fixture.0.join(dir)).unwrap();
    }
    fs::write(fixture.0.join("gitconfig"), "").unwrap();
    drop(
        memcore::MemoryStore::open(
            fixture
                .0
                .join("home/.tachi/global/memory.db")
                .to_str()
                .unwrap(),
        )
        .unwrap(),
    );
    let expected_query = if registered_pr {
        "pr view 7 --json state,headRefOid,headRefName"
    } else {
        "pr list --head fixture/merged --state all --json number,state,headRefOid,headRefName --limit 1"
    };
    fixture.script("gh", &format!("#!/bin/sh\n[ \"$*\" = '{expected_query}' ] || exit 91\nprintf '%s\\n' \"$*\" >> \"$FIXTURE_ROOT/gh-queries\"\n/bin/cat \"$FIXTURE_ROOT/pr-state\"\n"));
    fixture.script("lsof", "#!/bin/sh\n[ \"$1\" = '+D' ] && [ \"$2\" = \"$FIXTURE_ROOT/worktrees/merged\" ] || exit 92\nprintf 'clear\\n' >> \"$FIXTURE_ROOT/holder-probes\"\nexit 1\n");
    let repo = fixture.0.join("repo");
    let origin = fixture.0.join("origin.git");
    let wt = fixture.0.join("worktrees/merged");
    fixture.git_ok(&fixture.0, &["init", "--bare", origin.to_str().unwrap()]);
    fixture.git_ok(&repo, &["init", "-b", "main"]);
    assert_eq!(
        fixture.git_ok(&repo, &["rev-parse", "--absolute-git-dir"]),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(repo.join("base"), "base\n").unwrap();
    fixture.git_ok(&repo, &["add", "base"]);
    fixture.git_ok(&repo, &["commit", "-m", "base"]);
    fixture.git_ok(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    fixture.git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "fixture/merged",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    assert_eq!(
        fixture.git_ok(
            &wt,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(wt.join("accepted"), "accepted change\n").unwrap();
    fixture.git_ok(&wt, &["add", "accepted"]);
    fixture.git_ok(&wt, &["commit", "-m", "accepted PR head"]);
    let accepted = fixture.git_ok(&wt, &["rev-parse", "HEAD"]);
    fixture.git_ok(&repo, &["merge", "--squash", "fixture/merged"]);
    fixture.git_ok(&repo, &["commit", "-m", "independent squash merge"]);
    assert_eq!(
        fixture
            .git(&repo, &["merge-base", "--is-ancestor", &accepted, "main"])
            .status
            .code(),
        Some(1)
    );
    if case == "unique" {
        fs::write(wt.join("later"), "must survive\n").unwrap();
        fixture.git_ok(&wt, &["add", "later"]);
        fixture.git_ok(&wt, &["commit", "-m", "later unique commit"]);
    } else if case == "empty" {
        fixture.git_ok(
            &wt,
            &["commit", "--allow-empty", "-m", "later empty commit"],
        );
        assert_eq!(
            fixture.git_ok(&wt, &["rev-parse", "HEAD^{tree}"]),
            fixture.git_ok(&wt, &["rev-parse", &format!("{accepted}^{{tree}}")])
        );
        assert_ne!(fixture.git_ok(&wt, &["rev-parse", "HEAD"]), accepted);
    }
    fixture.git_ok(&repo, &["push", "origin", "fixture/merged"]);
    let mut register = vec![
        "wt-register",
        wt.to_str().unwrap(),
        "--repo",
        repo.to_str().unwrap(),
        "--branch",
        "fixture/merged",
        "--json",
    ];
    if registered_pr {
        register.extend(["--pr", "7"]);
    }
    fixture.cli(&register);
    if case == "detached" {
        fixture.git_ok(&wt, &["checkout", "--detach"]);
    }
    if case == "local-branch" {
        fixture.git_ok(&wt, &["branch", "-m", "fixture/renamed"]);
    }
    let local_ref = if case == "local-branch" {
        "refs/heads/fixture/renamed"
    } else {
        "refs/heads/fixture/merged"
    };
    let current = fixture.git_ok(&wt, &["rev-parse", "HEAD"]);
    if case == "unknown-head" {
        fixture.git_ok(&wt, &["symbolic-ref", "HEAD", "refs/heads/fixture/missing"]);
    }
    let registry_path = fixture.0.join("home/.tachi/worktrees.json");
    let marker_path = wt.join(".tachi-worktree.json");
    let registry = fs::read(&registry_path).unwrap();
    let marker = fs::read(&marker_path).unwrap();
    let mut payload = serde_json::json!({"number":7, "state":"MERGED", "headRefOid":accepted, "headRefName":"fixture/merged"});
    let expected_reason = match case {
        "unique" | "empty" => Some("worktree_head_mismatch"),
        "missing-head" => {
            payload.as_object_mut().unwrap().remove("headRefOid");
            Some("merged_pr_head_missing")
        }
        "empty-head" => {
            payload["headRefOid"] = serde_json::json!("");
            Some("merged_pr_head_missing")
        }
        "malformed-head" => {
            payload["headRefOid"] = serde_json::json!("not-a-commit");
            Some("merged_pr_head_invalid")
        }
        "zero-head" => {
            payload["headRefOid"] = serde_json::json!("0".repeat(40));
            Some("merged_pr_head_invalid")
        }
        "missing-branch" => {
            payload.as_object_mut().unwrap().remove("headRefName");
            Some("merged_pr_branch_missing")
        }
        "empty-branch" => {
            payload["headRefName"] = serde_json::json!("");
            Some("merged_pr_branch_missing")
        }
        "malformed-branch" => {
            payload["headRefName"] = serde_json::json!("bad branch");
            Some("merged_pr_branch_invalid")
        }
        "registry-branch" => {
            payload["headRefName"] = serde_json::json!("fixture/other");
            Some("registered_branch_mismatch")
        }
        "local-branch" => Some("worktree_branch_mismatch"),
        "detached" => Some("worktree_branch_unavailable"),
        "unknown-head" => Some("worktree_head_unavailable"),
        "exact" => None,
        _ => panic!("unknown private case"),
    };
    if !registered_pr {
        payload = serde_json::json!([payload]);
    }
    fs::write(
        fixture.0.join("pr-state"),
        serde_json::to_vec(&payload).unwrap(),
    )
    .unwrap();
    let dry = fixture.cli(&["wt-reconcile", "--dry-run", "--json"]);
    let apply = fixture.cli(&["wt-reconcile", "--force", "--json"]);
    let directory = wt.is_dir();
    let git_registration = fixture
        .git_ok(&repo, &["worktree", "list", "--porcelain"])
        .contains(wt.to_str().unwrap());
    let local = fixture
        .git(&repo, &["show-ref", "--verify", local_ref])
        .status
        .success();
    let remote = fixture
        .git(
            &origin,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let registry_bytes = fs::read(&registry_path).unwrap();
    let registry_preserved = registry_bytes == registry;
    let marker_preserved = fs::read(&marker_path).ok().as_ref() == Some(&marker);
    eprintln!("MERGED_HEAD_FIXTURE registered_pr={registered_pr} case={case} directory={directory} git_registration={git_registration} local_ref={local} remote_ref={remote} registry={registry_preserved} marker={marker_preserved} dry={dry} apply={apply}");
    assert_eq!(
        fs::read_to_string(fixture.0.join("gh-queries"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![expected_query, expected_query]
    );
    if let Some(reason) = expected_reason {
        assert!(
            directory
                && git_registration
                && local
                && remote
                && registry_preserved
                && marker_preserved
        );
        if case != "unknown-head" {
            assert_eq!(fixture.git_ok(&wt, &["rev-parse", "HEAD"]), current);
        } else {
            assert!(!fixture
                .git(&wt, &["rev-parse", "--verify", "HEAD^{commit}"])
                .status
                .success());
        }
        for report in [&dry, &apply] {
            assert_eq!(report["refused"][0]["reasons"], serde_json::json!([reason]));
            assert_eq!(report["reconciled"], serde_json::json!([]));
        }
        assert!(
            !fixture.0.join("holder-probes").exists(),
            "head refusal must precede planner"
        );
    } else {
        assert!(!directory && !git_registration && !local && !remote);
        assert!(!String::from_utf8(registry_bytes)
            .unwrap()
            .contains(wt.to_str().unwrap()));
        assert!(!marker_path.exists());
        assert_eq!(dry["reconciled"][0]["removed"], false);
        assert_eq!(apply["reconciled"][0]["removed"], true);
        assert_eq!(apply["refused"], serde_json::json!([]));
    }
}

#[test]
fn merged_pr_cli_preserves_later_unique_and_empty_commits() {
    let stubs = StubStore::new();
    for registered in [true, false] {
        for case in ["unique", "empty"] {
            merged_cli_case(&stubs, registered, case);
        }
    }
}

/// Head/branch evidence matrix, run once per PR lookup mode. tachi#1988: each
/// case builds real repositories, a DB and three CLI runs (~0.6-1.7 s on an
/// idle Mac), so the two lookup modes are separate tests to keep each well
/// inside nextest's 120 s cap under load.
const HEAD_AND_BRANCH_EVIDENCE_CASES: [&str; 12] = [
    "missing-head",
    "empty-head",
    "malformed-head",
    "zero-head",
    "missing-branch",
    "empty-branch",
    "malformed-branch",
    "registry-branch",
    "local-branch",
    "detached",
    "unknown-head",
    "exact",
];

#[test]
fn merged_pr_cli_requires_head_and_branch_evidence_and_retains_exact_head_cleanup_for_registered_pr(
) {
    let stubs = StubStore::new();
    for case in HEAD_AND_BRANCH_EVIDENCE_CASES {
        merged_cli_case(&stubs, true, case);
    }
}

#[test]
fn merged_pr_cli_requires_head_and_branch_evidence_and_retains_exact_head_cleanup_for_branch_lookup(
) {
    let stubs = StubStore::new();
    for case in HEAD_AND_BRANCH_EVIDENCE_CASES {
        merged_cli_case(&stubs, false, case);
    }
}

fn pr_lookup_error_cli_case(stubs: &StubStore, registered_pr: bool, case: &str) {
    let root = std::env::temp_dir().join(format!("tachi-pr-lookup-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let fixture = Fixture(root.canonicalize().unwrap(), stubs);
    for dir in [
        "bin",
        "home/.tachi/global",
        "config",
        "cargo",
        "rustup",
        "tmp",
        "templates",
        "repo",
        "worktrees",
    ] {
        fs::create_dir_all(fixture.0.join(dir)).unwrap();
    }
    fs::write(fixture.0.join("gitconfig"), "").unwrap();
    fixture.script("git", "#!/bin/sh\nexec /usr/bin/git \"$@\"\n");
    let private_cli = |args: &[&str]| -> serde_json::Value {
        let output = fixture
            .command(env!("CARGO_BIN_EXE_tachi-clean"), &fixture.0)
            .env("PATH", fixture.0.join("bin"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "private CLI {args:?}: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    };

    drop(
        memcore::MemoryStore::open(
            fixture
                .0
                .join("home/.tachi/global/memory.db")
                .to_str()
                .unwrap(),
        )
        .unwrap(),
    );

    fixture.script("lsof", "#!/bin/sh\n[ \"$1\" = '+D' ] && [ \"$2\" = \"$FIXTURE_ROOT/worktrees/merged\" ] || exit 92\nprintf 'clear\\n' >> \"$FIXTURE_ROOT/holder-probes\"\nexit 1\n");
    let repo = fixture.0.join("repo");
    let origin = fixture.0.join("origin.git");
    let wt = fixture.0.join("worktrees/merged");
    fixture.git_ok(&fixture.0, &["init", "--bare", origin.to_str().unwrap()]);
    fixture.git_ok(&repo, &["init", "-b", "main"]);
    assert_eq!(
        fixture.git_ok(&repo, &["rev-parse", "--absolute-git-dir"]),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(repo.join("base"), "base\n").unwrap();
    fixture.git_ok(&repo, &["add", "base"]);
    fixture.git_ok(&repo, &["commit", "-m", "base"]);
    fixture.git_ok(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    fixture.git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "fixture/merged",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    assert_eq!(
        fixture.git_ok(
            &wt,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(wt.join("accepted"), "accepted change\n").unwrap();
    fixture.git_ok(&wt, &["add", "accepted"]);
    fixture.git_ok(&wt, &["commit", "-m", "accepted PR head"]);
    let accepted = fixture.git_ok(&wt, &["rev-parse", "HEAD"]);
    fixture.git_ok(&repo, &["merge", "--squash", "fixture/merged"]);
    fixture.git_ok(&repo, &["commit", "-m", "independent squash merge"]);
    assert_eq!(
        fixture
            .git(&repo, &["merge-base", "--is-ancestor", &accepted, "main"])
            .status
            .code(),
        Some(1)
    );
    fixture.git_ok(&repo, &["push", "origin", "fixture/merged"]);
    let mut register = vec![
        "wt-register",
        wt.to_str().unwrap(),
        "--repo",
        repo.to_str().unwrap(),
        "--branch",
        "fixture/merged",
        "--json",
    ];
    if registered_pr {
        register.extend(["--pr", "7"]);
    }
    private_cli(&register);

    let view = "pr view 7 --json state,headRefOid,headRefName";
    let list = "pr list --head fixture/merged --state all --json number,state,headRefOid,headRefName --limit 1";
    let merged = serde_json::json!({"number":8,"state":"MERGED","headRefOid":accepted,"headRefName":"fixture/merged"});
    let payload = match case {
        "malformed" => "{private-invalid-json".to_string(),
        "nonobject" => "[0]".to_string(),
        "missing-state" => "{}".to_string(),
        "nonstring-state" => r#"{"state":7}"#.to_string(),
        "unknown-state" => r#"{"state":"UNRECOGNIZED"}"#.to_string(),
        "nonarray" => "{}".to_string(),
        "empty" => "[]".to_string(),
        "open" => r#"{"state":"OPEN"}"#.to_string(),
        "closed" => r#"{"state":"CLOSED"}"#.to_string(),
        "merged" | "command-failure" | "spawn-failure" => merged.to_string(),
        _ => panic!("unknown lookup case"),
    };
    let payload = if !registered_pr && !matches!(case, "malformed" | "nonarray" | "empty") {
        format!("[{payload}]")
    } else {
        payload
    };
    fs::write(fixture.0.join("lookup-payload"), payload).unwrap();
    fs::write(
        fixture.0.join("fallback-payload"),
        serde_json::json!([merged]).to_string(),
    )
    .unwrap();
    let failed_exit = if case == "command-failure" {
        "exit 17"
    } else {
        "/bin/cat \"$FIXTURE_ROOT/lookup-payload\""
    };
    let script = if registered_pr {
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FIXTURE_ROOT/gh-queries\"\ncase \"$*\" in\n'{view}') {failed_exit} ;;\n'{list}') /bin/cat \"$FIXTURE_ROOT/fallback-payload\" ;;\n*) exit 91 ;;\nesac\n")
    } else {
        format!("#!/bin/sh\n[ \"$*\" = '{list}' ] || exit 91\nprintf '%s\\n' \"$*\" >> \"$FIXTURE_ROOT/gh-queries\"\n{failed_exit}\n")
    };
    fixture.script("gh", &script);
    if case == "spawn-failure" {
        let interpreter = fixture.0.join("nonexistent-interpreter");
        assert!(interpreter.is_absolute());
        assert!(!interpreter.exists());
        fixture.script("gh", &format!("#!{}\n", interpreter.display()));
    }
    let registry_path = fixture.0.join("home/.tachi/worktrees.json");
    let marker_path = wt.join(".tachi-worktree.json");
    let registry = fs::read(&registry_path).unwrap();
    let marker = fs::read(&marker_path).unwrap();
    let dry = private_cli(&["wt-reconcile", "--dry-run", "--json"]);
    let apply = private_cli(&["wt-reconcile", "--force", "--json"]);
    let directory = wt.is_dir();
    let registration = fixture
        .git_ok(&repo, &["worktree", "list", "--porcelain"])
        .contains(wt.to_str().unwrap());
    let local = fixture
        .git(
            &repo,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let remote = fixture
        .git(
            &origin,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let registry_preserved = fs::read(&registry_path).unwrap() == registry;
    let marker_preserved = fs::read(&marker_path).ok().as_ref() == Some(&marker);
    eprintln!("PR_LOOKUP_FIXTURE registered={registered_pr} case={case} directory={directory} registration={registration} local={local} remote={remote} registry={registry_preserved} marker={marker_preserved} dry={dry} apply={apply}");
    if case == "merged" {
        assert!(
            !directory
                && !registration
                && !local
                && !remote
                && !registry_preserved
                && !marker_preserved
        );
    } else {
        assert!(
            directory && registration && local && remote && registry_preserved && marker_preserved,
            "lookup uncertainty must preserve all six surfaces"
        );
    }
    let expected_query = if registered_pr { view } else { list };
    if case == "spawn-failure" {
        assert!(!fixture.0.join("gh-queries").exists());
    } else {
        assert_eq!(
            fs::read_to_string(fixture.0.join("gh-queries"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec![expected_query, expected_query],
            "failed registered lookup must never select another PR"
        );
    }
    for report in [&dry, &apply] {
        if case == "spawn-failure" {
            let reason = report["skipped"][0]["reason"].as_str().unwrap();
            if registered_pr {
                assert_eq!(
                    reason,
                    "could not inspect PR status: pr view command unavailable"
                );
            } else {
                assert!(
                    reason.contains("os error 2"),
                    "expected missing-interpreter spawn error: {reason}"
                );
            }
        }
        match case {
            "merged" => assert_eq!(report["reconciled"].as_array().unwrap().len(), 1),
            "closed" => assert_eq!(
                report["refused"][0]["reasons"][0],
                "closed_pr_reclaimability_unproven"
            ),
            "open" => assert_eq!(report["skipped"][0]["reason"], "PR is still open"),
            "empty" => assert_eq!(report["skipped"][0]["reason"], "no PR found for branch"),
            _ => {
                assert!(report["reconciled"].as_array().unwrap().is_empty());
                assert!(report["skipped"][0]["reason"]
                    .as_str()
                    .unwrap()
                    .starts_with("could not inspect PR status:"));
                assert!(!report.to_string().contains("private-invalid-json"));
            }
        }
    }
}

#[test]
fn registered_pr_lookup_errors_never_fall_back_to_another_merged_pr() {
    let stubs = StubStore::new();
    for case in [
        "command-failure",
        "spawn-failure",
        "malformed",
        "nonobject",
        "missing-state",
        "nonstring-state",
        "unknown-state",
        "open",
        "closed",
        "merged",
    ] {
        pr_lookup_error_cli_case(&stubs, true, case);
    }
}

#[test]
fn branch_pr_lookup_distinguishes_invalid_evidence_from_absence() {
    let stubs = StubStore::new();
    for case in [
        "command-failure",
        "spawn-failure",
        "malformed",
        "nonarray",
        "nonobject",
        "missing-state",
        "nonstring-state",
        "unknown-state",
        "empty",
        "open",
        "closed",
        "merged",
    ] {
        pr_lookup_error_cli_case(&stubs, false, case);
    }
}

fn reconcile_warning_cli_case(case: &str) {
    let root = std::env::temp_dir().join(format!(
        "tachi-reconcile-warning-quote'-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&root).unwrap();
    let stubs = StubStore::new();
    let fixture = Fixture(root.canonicalize().unwrap(), &stubs);
    for dir in [
        "bin",
        "home/.tachi/global",
        "config",
        "cargo",
        "rustup",
        "tmp",
        "templates",
        "repo",
        "worktrees",
    ] {
        fs::create_dir_all(fixture.0.join(dir)).unwrap();
    }
    fs::write(fixture.0.join("gitconfig"), "").unwrap();
    fixture.script("git", "#!/bin/sh\nexec /usr/bin/git \"$@\"\n");
    let private_cli = |args: &[&str]| -> serde_json::Value {
        let output = fixture
            .command(env!("CARGO_BIN_EXE_tachi-clean"), &fixture.0)
            .env("PATH", fixture.0.join("bin"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "private CLI {args:?}: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    };

    drop(
        memcore::MemoryStore::open(
            fixture
                .0
                .join("home/.tachi/global/memory.db")
                .to_str()
                .unwrap(),
        )
        .unwrap(),
    );

    fixture.script("lsof", "#!/bin/sh\n[ \"$1\" = '+D' ] && [ \"$2\" = \"$FIXTURE_ROOT/worktrees/merged\" ] || exit 92\nprintf 'clear\\n' >> \"$FIXTURE_ROOT/holder-probes\"\nexit 1\n");
    let repo = fixture.0.join("repo");
    let origin = fixture.0.join("origin.git");
    let wt = fixture.0.join("worktrees/merged");
    fixture.git_ok(&fixture.0, &["init", "--bare", origin.to_str().unwrap()]);
    fixture.git_ok(&repo, &["init", "-b", "main"]);
    assert_eq!(
        fixture.git_ok(&repo, &["rev-parse", "--absolute-git-dir"]),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(repo.join("base"), "base\n").unwrap();
    fixture.git_ok(&repo, &["add", "base"]);
    fixture.git_ok(&repo, &["commit", "-m", "base"]);
    fixture.git_ok(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    fixture.git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "fixture/merged",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    assert_eq!(
        fixture.git_ok(
            &wt,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(wt.join("accepted"), "accepted change\n").unwrap();
    fixture.git_ok(&wt, &["add", "accepted"]);
    fixture.git_ok(&wt, &["commit", "-m", "accepted PR head"]);
    let accepted = fixture.git_ok(&wt, &["rev-parse", "HEAD"]);
    fixture.git_ok(&repo, &["merge", "--squash", "fixture/merged"]);
    fixture.git_ok(&repo, &["commit", "-m", "independent squash merge"]);
    assert_eq!(
        fixture
            .git(&repo, &["merge-base", "--is-ancestor", &accepted, "main"])
            .status
            .code(),
        Some(1)
    );
    fixture.git_ok(&repo, &["push", "origin", "fixture/merged"]);
    let mut register = vec![
        "wt-register",
        wt.to_str().unwrap(),
        "--repo",
        repo.to_str().unwrap(),
        "--branch",
        "fixture/merged",
        "--json",
    ];
    register.extend(["--pr", "7"]);
    private_cli(&register);

    fixture.script("gh", "#!/bin/sh\n[ \"$*\" = 'pr view 7 --json state,headRefOid,headRefName' ] || exit 91\n/bin/cat \"$FIXTURE_ROOT/pr-state\"\n");
    fs::write(fixture.0.join("pr-state"), serde_json::json!({"state":"MERGED", "headRefOid":accepted, "headRefName":"fixture/merged"}).to_string()).unwrap();
    let fault_match = match case {
        "branch" => {
            r#"[ "$#" -eq 5 ] && [ "$1" = '-C' ] && [ "$2" = "$FIXTURE_ROOT/repo" ] && [ "$3" = 'branch' ] && [ "$4" = '-D' ] && [ "$5" = 'fixture/merged' ]"#
        }
        "remove" => {
            r#"[ "$#" -eq 6 ] && [ "$1" = '-C' ] && [ "$2" = "$FIXTURE_ROOT/repo" ] && [ "$3" = 'worktree' ] && [ "$4" = 'remove' ] && [ "$5" = '--force' ] && [ "$6" = "$FIXTURE_ROOT/worktrees/merged" ]"#
        }
        "clean" => "false",
        _ => panic!("unknown warning case"),
    };
    fixture.script(
        "git",
        &format!(
            "#!/bin/sh\nif {fault_match}; then\n  printf 'injected\\n' >> \"$FIXTURE_ROOT/git-fault\"\n  printf 'private cleanup failure\\n' >&2\n  exit 17\nfi\nexec /usr/bin/git \"$@\"\n"
        ),
    );
    let registry_path = fixture.0.join("home/.tachi/worktrees.json");
    let marker_path = wt.join(".tachi-worktree.json");
    let registry = fs::read(&registry_path).unwrap();
    let marker = fs::read(&marker_path).unwrap();
    let report = private_cli(&["wt-reconcile", "--force", "--json"]);
    let directory = wt.is_dir();
    let registration = fixture
        .git_ok(&repo, &["worktree", "list", "--porcelain"])
        .contains(wt.to_str().unwrap());
    let local = fixture
        .git(
            &repo,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let remote = fixture
        .git(
            &origin,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let registry_preserved = fs::read(&registry_path).unwrap() == registry;
    let marker_preserved = fs::read(&marker_path).ok().as_ref() == Some(&marker);
    eprintln!("RECONCILE_WARNING_FIXTURE case={case} directory={directory} registration={registration} local={local} remote={remote} registry={registry_preserved} marker={marker_preserved} report={report}");
    let warnings = report["warnings"].as_array().unwrap();
    if case == "clean" {
        assert!(warnings.is_empty());
        assert!(!fixture.0.join("git-fault").exists());
    } else {
        assert_eq!(
            warnings.len(),
            1,
            "executor warning must survive exactly once: {report}"
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("git-fault")).unwrap(),
            "injected\n"
        );
        if case == "branch" {
            assert_eq!(
                warnings[0],
                "local branch deletion failed for fixture/merged: private cleanup failure"
            );
        } else {
            assert_eq!(warnings[0], "the scrap ledger was already recorded before this failed removal; the path and branch are now flagged as scrapped even though the tree is still present and untouched — reopening either exact one will be (over-cautiously) refused until a new branch/path is used");
        }
    }
    if case == "remove" {
        assert!(
            directory && registration && local && remote && registry_preserved && marker_preserved
        );
        assert!(report["reconciled"].as_array().unwrap().is_empty());
        assert_eq!(
            report["refused"][0]["reasons"][0],
            "git worktree remove failed: private cleanup failure"
        );
    } else {
        assert!(!directory && !registration && !remote && !registry_preserved && !marker_preserved);
        assert_eq!(local, case == "branch");
        assert_eq!(report["reconciled"][0]["removed"], true);
        assert!(report["refused"].as_array().unwrap().is_empty());
    }
    assert!(report["errors"].as_array().unwrap().is_empty());
}

#[test]
fn reconcile_preserves_executor_warning_after_local_branch_failure() {
    reconcile_warning_cli_case("branch");
}

#[test]
fn reconcile_preserves_executor_warning_after_worktree_remove_failure() {
    reconcile_warning_cli_case("remove");
}

#[test]
fn reconcile_clean_removal_has_no_executor_warnings() {
    reconcile_warning_cli_case("clean");
}

fn completion_error_cli_case(withhold_db: bool) {
    let root = std::env::temp_dir().join(format!(
        "tachi-completion-error-quote'-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&root).unwrap();
    let stubs = StubStore::new();
    let fixture = Fixture(root.canonicalize().unwrap(), &stubs);
    for dir in [
        "bin",
        "home/.tachi/global",
        "config",
        "cargo",
        "rustup",
        "tmp",
        "templates",
        "repo",
        "worktrees",
    ] {
        fs::create_dir_all(fixture.0.join(dir)).unwrap();
    }
    fs::write(fixture.0.join("gitconfig"), "").unwrap();
    fixture.script("git", "#!/bin/sh\nexec /usr/bin/git \"$@\"\n");
    let private_cli = |args: &[&str]| -> serde_json::Value {
        let output = fixture
            .command(env!("CARGO_BIN_EXE_tachi-clean"), &fixture.0)
            .env("PATH", fixture.0.join("bin"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "private CLI {args:?}: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    };

    drop(
        memcore::MemoryStore::open(
            fixture
                .0
                .join("home/.tachi/global")
                .join(memcore::MEMORY_DB_FILENAME)
                .to_str()
                .unwrap(),
        )
        .unwrap(),
    );

    fixture.script("lsof", "#!/bin/sh\n[ \"$1\" = '+D' ] && [ \"$2\" = \"$FIXTURE_ROOT/worktrees/merged\" ] || exit 92\nprintf 'clear\\n' >> \"$FIXTURE_ROOT/holder-probes\"\nexit 1\n");
    let repo = fixture.0.join("repo");
    let origin = fixture.0.join("origin.git");
    let wt = fixture.0.join("worktrees/merged");
    fixture.git_ok(&fixture.0, &["init", "--bare", origin.to_str().unwrap()]);
    fixture.git_ok(&repo, &["init", "-b", "main"]);
    assert_eq!(
        fixture.git_ok(&repo, &["rev-parse", "--absolute-git-dir"]),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(repo.join("base"), "base\n").unwrap();
    fixture.git_ok(&repo, &["add", "base"]);
    fixture.git_ok(&repo, &["commit", "-m", "base"]);
    fixture.git_ok(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    fixture.git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "fixture/merged",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    assert_eq!(
        fixture.git_ok(
            &wt,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(wt.join("accepted"), "accepted change\n").unwrap();
    fixture.git_ok(&wt, &["add", "accepted"]);
    fixture.git_ok(&wt, &["commit", "-m", "accepted PR head"]);
    let accepted = fixture.git_ok(&wt, &["rev-parse", "HEAD"]);
    fixture.git_ok(&repo, &["merge", "--squash", "fixture/merged"]);
    fixture.git_ok(&repo, &["commit", "-m", "independent squash merge"]);
    assert_eq!(
        fixture
            .git(&repo, &["merge-base", "--is-ancestor", &accepted, "main"])
            .status
            .code(),
        Some(1)
    );
    fixture.git_ok(&repo, &["push", "origin", "fixture/merged"]);
    let mut register = vec![
        "wt-register",
        wt.to_str().unwrap(),
        "--repo",
        repo.to_str().unwrap(),
        "--branch",
        "fixture/merged",
        "--json",
    ];
    register.extend(["--pr", "7"]);
    private_cli(&register);

    fixture.script("gh", "#!/bin/sh\n[ \"$*\" = 'pr view 7 --json state,headRefOid,headRefName' ] || exit 91\n/bin/cat \"$FIXTURE_ROOT/pr-state\"\n");
    fs::write(fixture.0.join("pr-state"), serde_json::json!({"state":"MERGED", "headRefOid":accepted, "headRefName":"fixture/merged"}).to_string()).unwrap();

    let db_path = fixture
        .0
        .join("home/.tachi/global")
        .join(memcore::MEMORY_DB_FILENAME);
    {
        let mut store =
            memcore::MemoryStore::open_existing_read_write(db_path.to_str().unwrap()).unwrap();
        memcore::insert_exec_env(
            store.connection(),
            &memcore::NewExecEnvLease {
                env_id: "completion-env".to_string(),
                kind: "worktree".to_string(),
                path: wt.to_str().unwrap().to_string(),
                repo_root: repo.to_str().unwrap().to_string(),
                branch: "fixture/merged".to_string(),
                base_sha: accepted.clone(),
                dispatch_id: None,
                env_class: memcore::EnvClass::EditOnly,
                created_at: "2026-09-13T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        memcore::insert_resource(
            store.connection_mut(),
            &memcore::NewExecEnvResource {
                resource_id: "completion-resource".to_string(),
                kind: memcore::ResourceKind::Worktree,
                path: wt.to_str().unwrap().to_string(),
                bytes: None,
                created_at: "2026-09-13T00:00:00Z".to_string(),
            },
        )
        .unwrap();
        memcore::bind_resource(
            store.connection_mut(),
            "completion-env",
            "completion-resource",
        )
        .unwrap();
    }
    let withheld = fixture.0.join("global-withheld");
    assert!(!withheld.exists());
    if withhold_db {
        fixture.script("git", r#"#!/bin/sh
if [ "$#" -eq 6 ] && [ "$1" = '-C' ] && [ "$2" = "$FIXTURE_ROOT/repo" ] && [ "$3" = 'worktree' ] && [ "$4" = 'remove' ] && [ "$5" = '--force' ] && [ "$6" = "$FIXTURE_ROOT/worktrees/merged" ]; then
  /usr/bin/git "$@" || exit 93
  printf 'git-removed\n' >> "$FIXTURE_ROOT/completion-fault"
  [ ! -e "$FIXTURE_ROOT/global-withheld" ] || exit 94
  /bin/mv "$FIXTURE_ROOT/home/.tachi/global" "$FIXTURE_ROOT/global-withheld" || exit 95
  printf 'db-withheld\n' >> "$FIXTURE_ROOT/completion-fault"
  exit 0
fi
exec /usr/bin/git "$@"
"#);
    }
    let registry_path = fixture.0.join("home/.tachi/worktrees.json");
    let marker_path = wt.join(".tachi-worktree.json");
    let registry = fs::read(&registry_path).unwrap();
    let marker = fs::read(&marker_path).unwrap();
    let output = fixture
        .command(env!("CARGO_BIN_EXE_tachi-clean"), &fixture.0)
        .env("PATH", fixture.0.join("bin"))
        .args(["wt-reconcile", "--force", "--json"])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let directory = wt.is_dir();
    let registration = fixture
        .git_ok(&repo, &["worktree", "list", "--porcelain"])
        .contains(wt.to_str().unwrap());
    let local = fixture
        .git(
            &repo,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let remote = fixture
        .git(
            &origin,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let registry_preserved = fs::read(&registry_path).unwrap() == registry;
    let marker_preserved = fs::read(&marker_path).ok().as_ref() == Some(&marker);
    let observed_db = if withhold_db {
        withheld.join(memcore::MEMORY_DB_FILENAME)
    } else {
        db_path
    };
    let observed = memcore::MemoryStore::open_read_only(observed_db.to_str().unwrap()).unwrap();
    let lease = memcore::get_exec_env(observed.connection(), "completion-env")
        .unwrap()
        .unwrap();
    let resource = memcore::get_resource(observed.connection(), "completion-resource")
        .unwrap()
        .unwrap();
    let bindings =
        memcore::active_binding_count(observed.connection(), "completion-resource").unwrap();
    eprintln!("COMPLETION_ERROR_FIXTURE withhold={withhold_db} exit={:?} directory={directory} registration={registration} local={local} remote={remote} registry={registry_preserved} marker={marker_preserved} lease={:?} resource={:?} bindings={bindings} report={report}", output.status.code(), lease.state, resource.state);
    assert_eq!(
        output.status.success(),
        !withhold_db,
        "physical removal must not hide durable completion failure: {report}"
    );
    assert!(
        !directory
            && !registration
            && !local
            && !remote
            && !registry_preserved
            && !marker_preserved
    );
    assert_eq!(report["reconciled"][0]["removed"], true);
    assert!(report["refused"].as_array().unwrap().is_empty());
    if withhold_db {
        assert_eq!(
            fs::read_to_string(fixture.0.join("completion-fault")).unwrap(),
            "git-removed\ndb-withheld\n"
        );
        assert_eq!(report["errors"].as_array().unwrap().len(), 1);
        assert!(report["errors"][0].as_str().unwrap().starts_with("worktree was removed but ExecEnv removal completion failed; lease remains fail-closed: open global DB for removal claim:"));
        assert_eq!(lease.state, memcore::ExecEnvState::Removing);
        assert_eq!(resource.state, memcore::ResourceState::Reclaiming);
        assert_eq!(bindings, 1);
    } else {
        assert!(!fixture.0.join("completion-fault").exists());
        assert!(report["errors"].as_array().unwrap().is_empty());
        assert_eq!(lease.state, memcore::ExecEnvState::Reclaimed);
        assert_eq!(resource.state, memcore::ResourceState::Reclaimed);
        assert_eq!(bindings, 0);
    }
}

#[test]
fn reconcile_reports_real_managed_completion_failure_after_physical_removal() {
    completion_error_cli_case(true);
}

#[test]
fn reconcile_managed_completion_success_reclaims_lease_and_resources() {
    completion_error_cli_case(false);
}

fn remote_delete_warning_cli_case(case: &str) {
    let root = std::env::temp_dir().join(format!(
        "tachi-remote-warning-quote'-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&root).unwrap();
    let stubs = StubStore::new();
    let fixture = Fixture(root.canonicalize().unwrap(), &stubs);
    for dir in [
        "bin",
        "home/.tachi/global",
        "config",
        "cargo",
        "rustup",
        "tmp",
        "templates",
        "repo",
        "worktrees",
    ] {
        fs::create_dir_all(fixture.0.join(dir)).unwrap();
    }
    fs::write(fixture.0.join("gitconfig"), "").unwrap();
    fixture.script("git", "#!/bin/sh\nexec /usr/bin/git \"$@\"\n");
    let private_cli = |args: &[&str]| -> serde_json::Value {
        let output = fixture
            .command(env!("CARGO_BIN_EXE_tachi-clean"), &fixture.0)
            .env("PATH", fixture.0.join("bin"))
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "private CLI {args:?}: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    };

    drop(
        memcore::MemoryStore::open(
            fixture
                .0
                .join("home/.tachi/global/memory.db")
                .to_str()
                .unwrap(),
        )
        .unwrap(),
    );

    fixture.script("lsof", "#!/bin/sh\n[ \"$1\" = '+D' ] && [ \"$2\" = \"$FIXTURE_ROOT/worktrees/merged\" ] || exit 92\nprintf 'clear\\n' >> \"$FIXTURE_ROOT/holder-probes\"\nexit 1\n");
    let repo = fixture.0.join("repo");
    let origin = fixture.0.join("origin.git");
    let wt = fixture.0.join("worktrees/merged");
    fixture.git_ok(&fixture.0, &["init", "--bare", origin.to_str().unwrap()]);
    fixture.git_ok(&repo, &["init", "-b", "main"]);
    assert_eq!(
        fixture.git_ok(&repo, &["rev-parse", "--absolute-git-dir"]),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(repo.join("base"), "base\n").unwrap();
    fixture.git_ok(&repo, &["add", "base"]);
    fixture.git_ok(&repo, &["commit", "-m", "base"]);
    fixture.git_ok(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    fixture.git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "fixture/merged",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    assert_eq!(
        fixture.git_ok(
            &wt,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        ),
        repo.join(".git").to_str().unwrap()
    );
    fs::write(wt.join("accepted"), "accepted change\n").unwrap();
    fixture.git_ok(&wt, &["add", "accepted"]);
    fixture.git_ok(&wt, &["commit", "-m", "accepted PR head"]);
    let accepted = fixture.git_ok(&wt, &["rev-parse", "HEAD"]);
    fixture.git_ok(&repo, &["merge", "--squash", "fixture/merged"]);
    fixture.git_ok(&repo, &["commit", "-m", "independent squash merge"]);
    assert_eq!(
        fixture
            .git(&repo, &["merge-base", "--is-ancestor", &accepted, "main"])
            .status
            .code(),
        Some(1)
    );
    fixture.git_ok(&repo, &["push", "origin", "fixture/merged"]);
    let mut register = vec![
        "wt-register",
        wt.to_str().unwrap(),
        "--repo",
        repo.to_str().unwrap(),
        "--branch",
        "fixture/merged",
        "--json",
    ];
    register.extend(["--pr", "7"]);
    private_cli(&register);

    fixture.script("gh", "#!/bin/sh\n[ \"$*\" = 'pr view 7 --json state,headRefOid,headRefName' ] || exit 91\n/bin/cat \"$FIXTURE_ROOT/pr-state\"\n");
    fs::write(fixture.0.join("pr-state"), serde_json::json!({"state":"MERGED", "headRefOid":accepted, "headRefName":"fixture/merged"}).to_string()).unwrap();

    if matches!(case, "absent" | "no-origin") {
        fixture.git_ok(&origin, &["update-ref", "-d", "refs/heads/fixture/merged"]);
    }
    if case == "no-origin" {
        fixture.git_ok(&repo, &["remote", "remove", "origin"]);
    }
    let fault = match case {
        "fault" => "printf 'private-remote-diagnostic-sentinel\\n' >&2; exit 17",
        "clean" | "absent" | "no-origin" => ":",
        _ => panic!("unknown remote warning case"),
    };
    fixture.script("git", &format!(r#"#!/bin/sh
if [ "$#" -eq 6 ] && [ "$1" = '-C' ] && [ "$2" = "$FIXTURE_ROOT/repo" ] && [ "$3" = 'push' ] && [ "$4" = 'origin' ] && [ "$5" = '--delete' ] && [ "$6" = 'fixture/merged' ]; then
  printf 'attempt\n' >> "$FIXTURE_ROOT/remote-attempts"
  {fault}
fi
exec /usr/bin/git "$@"
"#));
    let registry_path = fixture.0.join("home/.tachi/worktrees.json");
    let marker_path = wt.join(".tachi-worktree.json");
    let registry = fs::read(&registry_path).unwrap();
    let marker = fs::read(&marker_path).unwrap();
    let report = private_cli(&["wt-reconcile", "--force", "--json"]);
    let directory = wt.is_dir();
    let registration = fixture
        .git_ok(&repo, &["worktree", "list", "--porcelain"])
        .contains(wt.to_str().unwrap());
    let local = fixture
        .git(
            &repo,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let remote = fixture
        .git(
            &origin,
            &["show-ref", "--verify", "refs/heads/fixture/merged"],
        )
        .status
        .success();
    let registry_preserved = fs::read(&registry_path).unwrap() == registry;
    let marker_preserved = fs::read(&marker_path).ok().as_ref() == Some(&marker);
    eprintln!("REMOTE_WARNING_FIXTURE case={case} directory={directory} registration={registration} local={local} remote={remote} registry={registry_preserved} marker={marker_preserved} report={report}");
    let expected = if case == "clean" {
        serde_json::json!([])
    } else {
        serde_json::json!(["remote branch deletion command did not succeed for fixture/merged; remote state is unverified"])
    };
    assert_eq!(
        report["warnings"], expected,
        "remote failure must be visible but nonfatal"
    );
    assert!(!directory && !registration && !local && !registry_preserved && !marker_preserved);
    assert_eq!(remote, case == "fault");
    assert_eq!(
        fs::read_to_string(fixture.0.join("remote-attempts")).unwrap(),
        "attempt\n"
    );
    assert_eq!(report["reconciled"][0]["removed"], true);
    assert!(report["errors"].as_array().unwrap().is_empty());
    assert!(report["refused"].as_array().unwrap().is_empty());
    assert!(!report
        .to_string()
        .contains("private-remote-diagnostic-sentinel"));
}

#[test]
fn reconcile_remote_delete_failure_is_visible_without_failing_cleanup() {
    remote_delete_warning_cli_case("fault");
}

#[test]
fn reconcile_remote_delete_success_has_no_warning() {
    remote_delete_warning_cli_case("clean");
}

#[test]
fn reconcile_already_absent_remote_is_nonfatal_and_unverified() {
    remote_delete_warning_cli_case("absent");
}

#[test]
fn reconcile_missing_origin_is_nonfatal_and_unverified() {
    remote_delete_warning_cli_case("no-origin");
}
