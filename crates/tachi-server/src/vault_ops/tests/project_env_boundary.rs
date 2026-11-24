use crate::server_state::MemoryServer;
use std::path::Path;

const SECRET: &str = "project-boundary-secret-bytes";
const SECRET_NAME: &str = "PROJECT_BOUNDARY_API_KEY";

async fn server_with_secret(label: &str) -> MemoryServer {
    let db_path = crate::utils::test_fixture_path(format!(
        "memory-server-vault-{label}-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = MemoryServer::new(db_path, None).expect("create test server");
    crate::vault_test_api::seed_vault(&server, label).expect("seed unlocked vault");
    crate::vault_test_api::owner_set_vault_secret(&server, SECRET_NAME, SECRET, None, "", false)
        .await
        .expect("account");
    server
}

fn write_bindings(project: &Path, body: &str) {
    std::fs::create_dir_all(project.join(".tachi")).expect("mkdir .tachi");
    std::fs::write(project.join(".tachi").join("vault.env"), body).expect("write bindings");
}

/// Names a dispatch child would receive, or the refusal that stops it.
fn injected_names(server: &MemoryServer, cwd: &Path) -> Result<Vec<String>, String> {
    crate::vault_ops::load_unlocked_env_secrets_for_child_env(server, Some(cwd)).map(|secrets| {
        let mut names: Vec<String> = secrets.into_iter().map(|(name, _)| name).collect();
        names.sort();
        names.dedup();
        names
    })
}

// A checkout nested inside another project is its own trust domain: it must
// not inherit the bindings of the project that happens to contain it.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_bindings_do_not_cross_into_a_nested_checkout() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = server_with_secret("project-env-nested-checkout").await;
    let outer = tempfile::tempdir().expect("outer project");
    write_bindings(
        outer.path(),
        &format!("{SECRET_NAME}=vault:{SECRET_NAME}\n"),
    );

    let plain = outer.path().join("packages").join("plain").join("src");
    std::fs::create_dir_all(&plain).expect("plain subdirectory");
    assert_eq!(
        injected_names(&server, &plain).expect("same project"),
        vec![SECRET_NAME.to_string()],
        "a subdirectory of the project keeps inheriting its bindings"
    );

    let nested = outer.path().join("vendor").join("nested");
    std::fs::create_dir_all(nested.join(".git")).expect("nested checkout");
    write_bindings(&nested, "NESTED_KEY=vault:PROJECT_BOUNDARY_API_KEY\n");
    assert_eq!(
        injected_names(&server, &nested.join("src")).expect("nested checkout"),
        vec!["NESTED_KEY".to_string()],
        "the nested checkout sees its own bindings and not the outer project's"
    );

    // A linked worktree keeps `.git` as a file.
    let linked = outer.path().join("vendor").join("linked");
    std::fs::create_dir_all(&linked).expect("linked worktree dir");
    std::fs::write(linked.join(".git"), "gitdir: /elsewhere/worktrees/linked\n")
        .expect("linked worktree marker");
    assert!(
        injected_names(&server, &linked)
            .expect("linked worktree")
            .is_empty(),
        "a nested checkout without bindings must not reach the outer project's"
    );
}

// A binding file reached through a symlink is not project content; refusing
// it keeps the file from becoming a read of whatever the link names.
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_bindings_refuse_a_symlink_leaving_the_project() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = server_with_secret("project-env-symlink").await;
    let project = tempfile::tempdir().expect("project");
    let elsewhere = tempfile::tempdir().expect("elsewhere");
    let outside = elsewhere.path().join("real.env");
    std::fs::write(&outside, "OUTSIDE_KEY=vault:PROJECT_BOUNDARY_API_KEY\n").expect("outside file");
    std::fs::create_dir_all(project.path().join(".tachi")).expect("mkdir .tachi");
    std::os::unix::fs::symlink(&outside, project.path().join(".tachi").join("vault.env"))
        .expect("plant symlinked bindings");

    let error = injected_names(&server, project.path())
        .expect_err("a binding file that leaves the project must be refused");
    assert!(error.contains("refusing"), "{error}");
    let resolver_error = crate::vault_ops::load_project_env_bindings(project.path())
        .expect_err("the resolver behind tachi env run shares the refusal");
    assert!(
        resolver_error.to_string().contains("refusing"),
        "{resolver_error}"
    );
}

// The project file names the variables a child receives; it must not be able
// to name the ones that decide what the child loads or executes.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn project_bindings_cannot_name_the_child_execution_environment() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server = server_with_secret("project-env-execution-names").await;
    let project = tempfile::tempdir().expect("project");
    let mut bindings = String::from("MY_KEY=vault:PROJECT_BOUNDARY_API_KEY\n");
    for name in [
        "PATH",
        "BASH_ENV",
        "NODE_OPTIONS",
        "GIT_SSH_COMMAND",
        "RUSTC_WRAPPER",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
    ] {
        bindings.push_str(&format!("{name}=vault:PROJECT_BOUNDARY_API_KEY\n"));
    }
    write_bindings(project.path(), &bindings);

    assert_eq!(
        injected_names(&server, project.path()).expect("child env"),
        vec!["MY_KEY".to_string()],
        "only the ordinary variable may reach the child"
    );
}
