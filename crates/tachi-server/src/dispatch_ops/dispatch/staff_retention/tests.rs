use super::*;
use crate::staffing_ops::tests::{
    isolate_staff_repository_environment, test_server, StaffRepositoryFixture,
};
use crate::test_support::EnvRestore;

fn provision(server: &crate::MemoryServer, repo: &Path, dispatch_id: &str) -> PathBuf {
    let provisioned = server
        .with_global_store(|store| {
            crate::exec_env_ops::provision_managed_env(
                store,
                &crate::exec_env_ops::ProvisionEnvOptions {
                    repo_root: repo.to_path_buf(),
                    path: None,
                    branch: Some(format!("staff/{dispatch_id}")),
                    base: Some("HEAD".to_string()),
                    task: None,
                    role: None,
                    dispatch_id: Some(dispatch_id.to_string()),
                    name: Some(format!("{dispatch_id}-postflight")),
                    env_class: memcore::EnvClass::EditOnly,
                    private_target_approval: None,
                    private_target_dir: None,
                    dry_run: false,
                },
            )
        })
        .expect("provision Staff worktree");
    assert!(
        provisioned.report.errors.is_empty(),
        "{:?}",
        provisioned.report.errors
    );
    PathBuf::from(provisioned.report.path)
}

fn set_state(dispatch_id: &str, state: &str) {
    let run = dispatch_runs_root().join(dispatch_id);
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(
        run.join("status.json"),
        json!({"dispatch_id": dispatch_id, "state": state}).to_string(),
    )
    .unwrap();
}

fn commit(tree: &Path, file: &str) {
    std::fs::write(tree.join(file), "result\n").unwrap();
    git(tree, &["add", file]).unwrap();
    git(tree, &["commit", "-m", file]).unwrap();
}

fn ids(entries: &[Value]) -> Vec<&str> {
    let mut ids: Vec<&str> = entries
        .iter()
        .map(|entry| entry["dispatch_id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

fn active_leases(server: &crate::MemoryServer) -> Vec<memcore::ExecEnvLease> {
    server
        .with_global_store_read(|store| {
            memcore::list_exec_envs(store.connection(), Some(memcore::ExecEnvState::Active))
                .map_err(|error| error.to_string())
        })
        .unwrap()
}

/// Terminal results that are safe on a remote are released; running, local-only
/// and dirty results are kept. Preview never removes anything.
#[test]
fn retained_staff_worktrees_release_only_results_safe_on_a_remote() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _git = isolate_staff_repository_environment();
    let _env = [
        EnvRestore::set_path("HOME", &home),
        EnvRestore::set_path("TACHI_HOME", &root.path().join("tachi")),
        EnvRestore::set_path("TACHI_WORKTREES_ROOT", &root.path().join("worktrees")),
    ];
    let repository = StaffRepositoryFixture::new(&home);
    let server = test_server();

    let pushed = provision(&server, &repository.repo, "d-pushed");
    commit(&pushed, "pushed.txt");
    git(&pushed, &["push", "origin", "HEAD"]).unwrap();
    set_state("d-pushed", "TASK_STATE_COMPLETED");

    let untouched = provision(&server, &repository.repo, "d-untouched");
    set_state("d-untouched", "TASK_STATE_FAILED");

    let local = provision(&server, &repository.repo, "d-local");
    commit(&local, "local.txt");
    set_state("d-local", "TASK_STATE_COMPLETED");

    let running = provision(&server, &repository.repo, "d-running");
    set_state("d-running", "TASK_STATE_WORKING");

    let dirty = provision(&server, &repository.repo, "d-dirty");
    std::fs::write(dirty.join("draft.txt"), "uncommitted\n").unwrap();
    set_state("d-dirty", "TASK_STATE_CANCELED");

    let preview = release_retained_staff_worktrees(active_leases(&server), false, |_| {
        panic!("preview must not reclaim")
    });
    assert_eq!(
        ids(&preview.released),
        ["d-dirty", "d-pushed", "d-untouched"]
    );
    assert!(pushed.is_dir() && untouched.is_dir() && dirty.is_dir());

    let report = release_retained_staff_worktrees_for_server(&server).unwrap();
    assert_eq!(
        ids(&report.released),
        ["d-pushed", "d-untouched"],
        "{report:?}"
    );
    assert_eq!(
        ids(&report.kept),
        ["d-dirty", "d-local", "d-running"],
        "{report:?}"
    );
    let reason = |id: &str| {
        report
            .kept
            .iter()
            .find(|entry| entry["dispatch_id"] == id)
            .unwrap()["reason"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(reason("d-local").contains("only locally"));
    assert!(reason("d-running").contains("not terminal"));
    assert!(reason("d-dirty").contains("refused"));

    assert!(!pushed.exists() && !untouched.exists());
    assert!(local.is_dir() && running.is_dir() && dirty.is_dir());
    let branches = git(&repository.repo, &["branch", "--list", "staff/*"]).unwrap();
    assert!(!branches.contains("staff/d-pushed"), "{branches}");
    assert!(!branches.contains("staff/d-untouched"), "{branches}");
    assert!(branches.contains("staff/d-local"), "{branches}");
    let active: Vec<String> = active_leases(&server)
        .into_iter()
        .filter_map(|lease| lease.dispatch_id)
        .collect();
    assert!(!active
        .iter()
        .any(|id| id == "d-pushed" || id == "d-untouched"));
    assert_eq!(active.len(), 3, "{active:?}");
}
