use super::*;

#[cfg(unix)]
fn install_permission_denied_symlink_hook() -> crate::path_utils::PlanCSymlinkHookGuard {
    crate::path_utils::install_plan_c_symlink_hook_for_test(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "injected Plan C symlink permission denial",
        ))
    })
}

#[tokio::test]
async fn tachi_init_project_db_creates_expected_path() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home.temp_home.join("Project DB Repo");
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    let response = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("tachi_init_project_db should succeed");
    let json: serde_json::Value =
        serde_json::from_str(&response).expect("tachi_init_project_db response should be JSON");

    let db_path =
        crate::path_utils::resolve_project_db_path(&root, std::path::Path::new(".tachi/memory.db"))
            .expect("resolve project db path");
    assert_eq!(json["created"], json!(true));
    assert_eq!(json["db_path"], json!(db_path.display().to_string()));
    assert!(db_path.exists(), "project db should be created on disk");
    let project = json["project"]
        .as_str()
        .expect("canonical project identity");
    let alias = crate::path_utils::plan_c_global_db_path(project);
    assert!(
        alias.starts_with(temp_home.temp_home.join(".tachi/projects")),
        "Plan C alias must stay inside the temporary TACHI_HOME: {}",
        alias.display()
    );
    assert!(alias.is_symlink(), "Plan C alias should be created");

    let response_second = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("second tachi_init_project_db should succeed");
    let json_second: serde_json::Value = serde_json::from_str(&response_second)
        .expect("second tachi_init_project_db response should be JSON");
    assert_eq!(json_second["created"], json!(false));
}

#[cfg(unix)]
#[tokio::test]
async fn alias_precommit_explicit_permission_failure_leaves_no_project_state() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home.temp_home.join("Explicit Permission Repo");
    std::fs::create_dir_all(root.join(".git")).expect("fake git root");
    let db_path = root.join(".tachi/memory.db");
    let manifest = temp_home.temp_home.join(".tachi/manifest.json");
    let _hook = install_permission_denied_symlink_hook();

    let error = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect_err("symlink permission failure must refuse explicit init");

    assert!(error.contains("permission denial"), "{error}");
    assert!(!db_path.exists(), "failed precommit must remove its DB");
    assert!(!root.join(".tachi").exists(), "DB parent must not remain");
    assert!(!manifest.exists(), "manifest must not remain");
    assert_eq!(server.project_db_path_buf(), None);
}

#[cfg(unix)]
#[tokio::test]
async fn alias_precommit_explicit_eexist_wrong_target_is_rollback_safe_and_retry_refuses() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home.temp_home.join("Explicit Wrong Target Repo");
    std::fs::create_dir_all(root.join(".git")).expect("fake git root");
    let db_path = root.join(".tachi/memory.db");
    let manifest = temp_home.temp_home.join(".tachi/manifest.json");
    let user_target = temp_home.temp_home.join("user-owned-explicit.db");
    std::fs::write(&user_target, b"user-owned").expect("user target");
    let hook_target = user_target.clone();
    let hook = crate::path_utils::install_plan_c_symlink_hook_for_test(move |alias| {
        std::os::unix::fs::symlink(&hook_target, alias)
    });

    let first = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect_err("racing wrong-target alias must refuse explicit init");
    assert!(first.contains("Plan C alias integrity failure"), "{first}");
    assert!(!db_path.exists(), "failed precommit must remove its DB");
    assert!(!root.join(".tachi").exists(), "DB parent must not remain");
    assert!(!manifest.exists(), "manifest must not remain");
    assert_eq!(server.project_db_path_buf(), None);
    assert_eq!(std::fs::read(&user_target).unwrap(), b"user-owned");
    drop(hook);

    let retry = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect_err("persistent wrong-target alias must refuse retry");
    assert!(retry.contains("Plan C alias integrity failure"), "{retry}");
    assert!(!db_path.exists());
    assert!(!manifest.exists());
    assert_eq!(server.project_db_path_buf(), None);
    assert_eq!(std::fs::read(&user_target).unwrap(), b"user-owned");
}

#[cfg(unix)]
#[tokio::test]
async fn alias_precommit_explicit_accepts_matching_concurrent_symlink() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home.temp_home.join("Explicit Matching Race Repo");
    std::fs::create_dir_all(root.join(".git")).expect("fake git root");
    let db_path = root.join(".tachi/memory.db");
    let hook_db = db_path.clone();
    let _hook = crate::path_utils::install_plan_c_symlink_hook_for_test(move |alias| {
        std::os::unix::fs::symlink(&hook_db, alias)
    });

    server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("matching concurrent alias must be accepted");

    assert!(db_path.exists());
    assert_eq!(
        std::fs::canonicalize(server.project_db_path_buf().expect("active DB")).unwrap(),
        std::fs::canonicalize(db_path).unwrap()
    );
    assert!(temp_home.temp_home.join(".tachi/manifest.json").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn alias_precommit_explicit_already_resolved_still_requires_alias_confirmation() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home
        .temp_home
        .join("Explicit Resolved Alias Gate Repo");
    std::fs::create_dir_all(root.join(".git")).expect("fake git root");
    let params = || {
        Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        })
    };
    let response = server
        .tachi_init_project_db(params())
        .await
        .expect("initial explicit init");
    let body: serde_json::Value = serde_json::from_str(&response).expect("response JSON");
    let project = body["project"].as_str().expect("project identity");
    let db_path = root.join(".tachi/memory.db");
    let alias = crate::path_utils::plan_c_global_db_path(project);
    let manifest = temp_home.temp_home.join(".tachi/manifest.json");
    let db_before = std::fs::read(&db_path).expect("DB preimage");
    let manifest_before = std::fs::read(&manifest).expect("manifest preimage");
    let active_before = server.project_db_path_buf();
    std::fs::remove_file(&alias).expect("remove alias to exercise recreation gate");
    let _hook = install_permission_denied_symlink_hook();

    let error = server
        .tachi_init_project_db(params())
        .await
        .expect_err("resolved project must not bypass alias confirmation");

    assert!(error.contains("permission denial"), "{error}");
    assert_eq!(std::fs::read(&db_path).unwrap(), db_before);
    assert_eq!(std::fs::read(&manifest).unwrap(), manifest_before);
    assert_eq!(server.project_db_path_buf(), active_before);
    assert!(!alias.exists());
}

#[tokio::test]
async fn custom_db_relpath_reopens_from_manifest_without_alias() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home.temp_home.join("Custom Path Repo");
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    let response = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: "data/project.db".to_string(),
        }))
        .await
        .expect("custom contained DB path should initialize");
    let json: serde_json::Value = serde_json::from_str(&response).expect("response JSON");
    let project = json["project"]
        .as_str()
        .expect("canonical project identity");
    let db_path = root.join("data/project.db");
    assert!(db_path.exists());

    let alias = crate::path_utils::plan_c_global_db_path(project);
    let _ = std::fs::remove_file(alias);
    let reopened = crate::MemoryServer::resolve_named_project_db_path(project)
        .expect("manifest scope must reopen a custom DB path without an alias");
    assert_eq!(
        std::fs::canonicalize(reopened).unwrap(),
        std::fs::canonicalize(db_path).unwrap()
    );
}

#[tokio::test]
async fn tachi_init_project_db_activates_project_store_and_read_pool() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home.temp_home.join("Project Runtime Repo");
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    assert_eq!(server.project_db_path_buf(), None);

    let response = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect("tachi_init_project_db should activate project state");
    let json: serde_json::Value =
        serde_json::from_str(&response).expect("tachi_init_project_db response should be JSON");
    assert_eq!(json["active"], json!(true));
    let project = json["project"]
        .as_str()
        .expect("canonical project identity");
    let alias = crate::path_utils::plan_c_global_db_path(project);
    assert!(
        alias.starts_with(temp_home.temp_home.join(".tachi/projects")),
        "Plan C alias must stay inside the temporary TACHI_HOME: {}",
        alias.display()
    );
    assert!(alias.is_symlink(), "Plan C alias should be created");

    let db_path =
        crate::path_utils::resolve_project_db_path(&root, std::path::Path::new(".tachi/memory.db"))
            .expect("resolve project db path");
    assert_eq!(server.project_db_path_buf(), Some(db_path));

    server
        .with_project_store(|store| {
            store
                .upsert(&make_entry("activated-project-read-visible"))
                .map_err(|e| format!("project upsert failed: {e}"))
        })
        .expect("project writer should be active");

    let found = server
        .with_project_store_read(|store| {
            store
                .get("activated-project-read-visible")
                .map_err(|e| format!("project read get failed: {e}"))
        })
        .expect("project read pool should be active");
    assert_eq!(
        found.expect("project entry exists").id,
        "activated-project-read-visible"
    );
}

#[tokio::test]
async fn tachi_init_project_db_rejects_plan_c_split_brain_before_open() {
    let (server, temp_home) = make_server_with_temp_home();
    let root = temp_home
        .temp_home
        .join("Split Brain Repo")
        .canonicalize()
        .unwrap_or_else(|_| temp_home.temp_home.join("Split Brain Repo"));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    let alias_db = crate::path_utils::plan_c_global_db_path("Split_Brain_Repo");
    std::fs::create_dir_all(alias_db.parent().expect("alias parent")).expect("create alias parent");
    memcore::MemoryStore::open(alias_db.to_str().expect("alias db")).expect("open alias db");

    let error = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect_err("ambiguous legacy alias ownership must fail before DB open");
    assert!(
        error.contains("refusing ownership guess"),
        "unexpected split-brain refusal: {error}"
    );
    assert!(
        !alias_db.is_symlink(),
        "init must not silently replace a regular alias DB"
    );
    assert!(
        !root.join(".tachi/memory.db").exists(),
        "init must fail before opening or creating the repo-local DB"
    );
    assert!(
        !root.join(".tachi").exists(),
        "identity preflight failure must not create the DB parent directory"
    );
}

/// #1120 PR2 core regression: an omitted `project_root` must be a loud,
/// actionable error, never a silent fallback to the SERVER process's own
/// cwd (`find_git_root()`). This is genuinely behavioral-red on pre-fix
/// code, not just compile-red: `cargo test` runs with the Tachi source
/// checkout itself as cwd (a real git repo), so pre-fix this call would
/// silently succeed by resolving `project_root` to the CALLER's unrelated
/// test-process cwd — the exact caller-cwd-blindness bug #1120 exists to
/// close, reproduced live by omitting the field in this very test binary.
#[tokio::test]
async fn tachi_init_project_db_requires_project_root_explicitly() {
    let server = make_server();

    let err = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: None,
            db_relpath: ".tachi/memory.db".to_string(),
        }))
        .await
        .expect_err("omitted project_root must be rejected, not silently resolved from cwd");

    assert!(
        err.contains("project_root is required"),
        "unexpected error: {err}"
    );
    assert!(
        err.contains("X-Tachi-Workspace-Root"),
        "error should point at the #1120 PR1 auto-register alternative: {err}"
    );
}

#[tokio::test]
async fn tachi_init_project_db_rejects_path_traversal() {
    let server = make_server();
    let root =
        crate::utils::test_fixture_path(format!("tachi-project-escape-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join(".git")).expect("create fake git root");

    let err = server
        .tachi_init_project_db(Parameters(InitProjectDbParams {
            project_root: Some(root.display().to_string()),
            db_relpath: "../outside.db".to_string(),
        }))
        .await
        .expect_err("path traversal db_relpath should be rejected");

    assert!(err.contains("db_relpath"), "unexpected error: {err}");

    let _ = std::fs::remove_dir_all(root);
}
