use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use memcore::{EnvClass, ExecEnvSelector, NewExecEnvLease, ReclaimOutcome};

use super::{admit_agent_connection, handle_task_claim};
use crate::exec_env_ops::{provision_managed_env, ProvisionEnvOptions};
use crate::gh_ops::worktree_holder_gate;
use crate::tests::make_server;

struct EnvRestore {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvRestore {
    fn set(values: &[(&'static str, &Path)]) -> Self {
        let saved = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self { saved }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
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
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create test root");
    path
}

fn claim_params(issue_ref: &str, worktree_path: &Path) -> crate::tool_params::TachiTaskParams {
    serde_json::from_value(serde_json::json!({
        "action": "claim",
        "issue_ref": issue_ref,
        "branch": "goal/1253-canonical-path",
        "claim_role": "executor",
        "claim_mode": "writable",
        "worktree_path": worktree_path,
        "claim_scope": ["crates/tachi-server/src/**"],
        "expected_head": "2969d6aa",
        "lease_expires_at": "2030-01-01T00:00:00Z"
    }))
    .expect("claim params")
}

#[cfg(unix)]
#[test]
fn symlinked_root_persists_one_path_for_holder_and_collision_checks() {
    let root = unique_temp_dir("identity-workclaim-canonical-path");
    let projects = root.join("Projects");
    let desktop = root.join("desktop");
    let real_tree = projects.join("Sigil");
    let alias_tree = desktop.join("sigil");
    std::fs::create_dir_all(&real_tree).expect("create real tree");
    std::fs::create_dir_all(&desktop).expect("create alias parent");
    std::os::unix::fs::symlink(&real_tree, &alias_tree).expect("create tree alias");

    let server = make_server();
    admit_agent_connection(&server, Some("agent.canonical".to_string()), true)
        .expect("admit local identity");
    let first = handle_task_claim(&server, &claim_params("org/repo#1253", &alias_tree))
        .expect("claim through symlink spelling");
    let claim_id = first["claim_id"].as_str().expect("claim id").to_string();

    server
        .with_global_store(|store| {
            let claim = memcore::get_claim(store.connection(), &claim_id)
                .map_err(|error| error.to_string())?
                .expect("persisted claim");
            let canonical_tree = std::fs::canonicalize(&real_tree)
                .expect("canonical tree")
                .display()
                .to_string();
            assert_eq!(
                claim.worktree_path.as_deref(),
                Some(canonical_tree.as_str()),
                "the symlinked spelling must not survive the WorkClaim write"
            );
            memcore::insert_exec_env(
                store.connection(),
                &NewExecEnvLease {
                    env_id: "env-canonical".to_string(),
                    kind: "worktree".to_string(),
                    path: claim.worktree_path.expect("canonical claim path"),
                    repo_root: projects.display().to_string(),
                    branch: "goal/1253-canonical-path".to_string(),
                    base_sha: "2969d6aa".to_string(),
                    dispatch_id: None,
                    env_class: EnvClass::EditOnly,
                    created_at: String::new(),
                },
            )
            .map_err(|error| error.to_string())?;
            memcore::bind_work_claim_exec_env(
                store.connection_mut(),
                &claim_id,
                "env-canonical",
                0,
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        })
        .expect("seed held ExecEnv lease through the symlinked root");

    let held = worktree_holder_gate(&server, alias_tree.to_str().expect("utf-8 alias"))
        .expect_err("the symlink spelling must resolve to held evidence");
    assert!(held.contains("Held"), "unexpected holder refusal: {held}");

    let collision = handle_task_claim(&server, &claim_params("org/repo#1253", &real_tree))
        .expect_err("canonical-equivalent writable trees must collide");
    assert!(
        collision.contains("writable worktree path overlaps"),
        "unexpected collision refusal: {collision}"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn provision_writer_persists_canonical_path_for_post_delete_reclaim() {
    let _global = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = unique_temp_dir("identity-workclaim-provision-canonical-path");
    let home = root.join("home");
    let repo = root.join("source");
    let projects = root.join("Projects");
    let desktop = root.join("desktop");
    let alias_root = desktop.join("managed");
    let alias_tree = alias_root.join("sigil");
    std::fs::create_dir_all(&home).expect("create isolated home");
    std::fs::create_dir_all(&repo).expect("create source repo");
    std::fs::create_dir_all(&projects).expect("create real managed root");
    std::fs::create_dir_all(&desktop).expect("create alias parent");
    std::os::unix::fs::symlink(&projects, &alias_root).expect("create managed-root alias");
    let _env = EnvRestore::set(&[("HOME", &home), ("TACHI_WORKTREES_ROOT", &alias_root)]);

    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.name", "Canonical Path Test"]);
    git(
        &repo,
        &["config", "user.email", "canonical-path@example.invalid"],
    );
    std::fs::write(repo.join("README.md"), "canonical path test\n").expect("write fixture");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "fixture"]);

    let mut store = memcore::MemoryStore::open_in_memory().expect("in-memory store");
    let provisioned = provision_managed_env(
        store.connection_mut(),
        &ProvisionEnvOptions {
            repo_root: repo.clone(),
            path: Some(alias_tree.clone()),
            branch: Some("goal/1253-provision-canonical".to_string()),
            base: Some("HEAD".to_string()),
            task: None,
            role: None,
            dispatch_id: None,
            name: None,
            env_class: EnvClass::EditOnly,
            private_target_approval: None,
            private_target_dir: None,
            dry_run: false,
        },
    )
    .expect("provision through symlinked managed root");
    let env_id = provisioned.env_id.expect("persisted ExecEnv lease");
    let canonical_tree = std::fs::canonicalize(projects.join("sigil"))
        .expect("canonical provisioned worktree")
        .display()
        .to_string();
    assert_eq!(
        provisioned.report.path, canonical_tree,
        "the production provision writer must replace the pre-creation alias spelling"
    );
    let lease = memcore::find_active_exec_env_by_path(store.connection(), &canonical_tree)
        .expect("query canonical lease path")
        .expect("active lease at canonical path");
    assert_eq!(lease.env_id, env_id);
    assert_eq!(lease.path, canonical_tree);

    git(
        &repo,
        &["worktree", "remove", "--force", canonical_tree.as_str()],
    );
    assert!(
        !Path::new(&canonical_tree).exists(),
        "fixture must prove the cached canonical spelling survives deletion"
    );
    assert_eq!(
        memcore::reclaim_exec_env(
            store.connection_mut(),
            &ExecEnvSelector::Path(canonical_tree),
            Some("canonical path regression test"),
        )
        .expect("reclaim by cached canonical path"),
        ReclaimOutcome::Reclaimed { env_id }
    );

    drop(_env);
    let _ = std::fs::remove_dir_all(root);
}
