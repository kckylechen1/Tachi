//! #1906: actual CLI reconciliation must preserve unmerged CLOSED-PR resources.
//! Every subprocess starts with an empty environment and private Git/config roots.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        // Only the directory created exclusively by this fixture is owned here.
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
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
    fn script(&self, name: &str, body: &str) {
        let path = self.0.join("bin").join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn closed_pr_cli_preserves_unmerged_worktree_and_refs() {
    let root = std::env::temp_dir().join(format!("tachi-1906-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let fixture = Fixture(root.canonicalize().unwrap());
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
    fixture.script("gh", "#!/bin/sh\n[ \"$*\" = 'pr view 7 --json state' ] || exit 91\n/bin/cat \"$FIXTURE_ROOT/pr-state\"\n");
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
    fs::write(fixture.0.join("pr-state"), "{\"state\":\"MERGED\"}").unwrap();
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
