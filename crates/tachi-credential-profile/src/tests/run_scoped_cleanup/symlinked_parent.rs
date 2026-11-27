use super::*;

// The worker that ran in the run directory can rewrite anything under it, so a
// path-based removal would follow its symlink and delete the host file.
#[test]
fn credential_run_scoped_cleanup_refuses_to_traverse_a_symlinked_parent() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-symlinked-parent-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let host = tempfile::tempdir().expect("host dir outside the run dir");
    let run_dir = tempfile::tempdir().expect("temp run dir");
    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "auth_json".to_string(),
            target: "{credentials_dir}/auth.json".to_string(),
            chmod: None,
            template: None,
        }],
    };
    let secret_values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"secret-token"}"#.to_string(),
    )]
    .into_iter()
    .collect();
    crate::apply_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        &secret_values,
        &crate::CredentialApplyOptions {
            allow_existing: false,
            run_dir: Some(run_dir.path().to_path_buf()),
        },
    )
    .expect("apply run-scoped materialization");

    // The worker swaps its credentials directory for a link to a host
    // directory holding a file of the very same name and bytes.
    let credentials_dir = std::fs::canonicalize(run_dir.path())
        .expect("canonical run dir")
        .join("credentials");
    let host_credentials = host.path().join("credentials");
    let host_auth = host_credentials.join("auth.json");
    std::fs::create_dir_all(&host_credentials).expect("create host credentials dir");
    std::fs::write(&host_auth, r#"{"token":"secret-token"}"#).expect("write host file");
    std::fs::remove_dir_all(&credentials_dir).expect("drop materialized credentials dir");
    std::os::unix::fs::symlink(&host_credentials, &credentials_dir)
        .expect("worker plants a symlinked parent");
    assert!(
        credentials_dir.join("auth.json").exists(),
        "the planted link resolves to the host file"
    );

    let dry_run =
        crate::cleanup_ephemeral_credential_materializations(&store, run_dir.path(), true)
            .expect("dry-run cleanup");
    assert!(dry_run.would_remove.is_empty(), "{dry_run:?}");
    assert!(
        dry_run.errors.iter().any(|error| error.contains("symlink")),
        "{dry_run:?}"
    );

    let applied =
        crate::cleanup_ephemeral_credential_materializations(&store, run_dir.path(), false)
            .expect("apply cleanup");

    assert_eq!(
        std::fs::read_to_string(&host_auth).ok().as_deref(),
        Some(r#"{"token":"secret-token"}"#),
        "cleanup deleted a host file through the worker's symlink: {applied:?}"
    );
    assert!(applied.removed.is_empty(), "{applied:?}");
    assert!(applied.missing.is_empty(), "{applied:?}");
    assert!(
        applied.errors.iter().any(|error| error.contains("symlink")),
        "{applied:?}"
    );
    assert!(
        std::fs::symlink_metadata(&credentials_dir)
            .expect("planted link")
            .file_type()
            .is_symlink(),
        "cleanup must leave the worker's link alone"
    );
    let rows = store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&rows[0].value_json).expect("metadata JSON")
            ["cleanup_status"],
        serde_json::json!("cleaned"),
        "a refused target must stay open for the next cleanup"
    );
}

/// The run directory is reached through a link that lives where a worker may
/// write. Where the link points at cleanup time says nothing about where it
/// pointed when the credentials were recorded, so the cleanup refuses the
/// whole run directory: nothing is removed and the records stay open.
#[test]
fn credential_run_scoped_cleanup_refuses_a_run_dir_behind_a_link() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-alias-run-dir-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let real_run_dir = root.path().join("real-run");
    let alias_run_dir = root.path().join("alias-run");
    std::fs::create_dir_all(real_run_dir.join("credentials")).expect("create real credentials");
    std::os::unix::fs::symlink(&real_run_dir, &alias_run_dir).expect("create run_dir alias");

    let lexical_target = alias_run_dir.join("credentials").join("auth.json");
    let profile = legacy_auth_file_profile(&lexical_target);
    seed_legacy_auth_file(&store, &profile);

    let error = crate::cleanup_ephemeral_credential_materializations(&store, &alias_run_dir, false)
        .expect_err("a link to the run directory cannot be vouched for");
    assert!(error.contains("without following a link"), "{error}");
    assert!(
        real_run_dir.join("credentials").join("auth.json").exists(),
        "the credential file was removed through the link"
    );
    let rows = store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&rows[0].value_json).expect("metadata JSON")
            ["cleanup_status"],
        serde_json::json!("cleaned"),
        "a refused run directory must leave its records open"
    );
}

/// A worker repoints the run directory's link before the cleanup runs, at
/// another directory that holds a file of the same name.
#[test]
fn credential_run_scoped_cleanup_refuses_a_repointed_run_dir_link() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-repointed-link-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = std::fs::canonicalize(tempfile::tempdir().expect("temp root").path())
        .expect("canonical root");
    let real_run_dir = root.join("job");
    let alias_run_dir = root.join("current");
    std::fs::create_dir_all(real_run_dir.join("credentials")).expect("create real credentials");
    std::os::unix::fs::symlink(&real_run_dir, &alias_run_dir).expect("create run_dir alias");

    let lexical_target = alias_run_dir.join("credentials").join("auth.json");
    let profile = legacy_auth_file_profile(&lexical_target);
    seed_legacy_auth_file(&store, &profile);

    let other_run_dir = root.join("other");
    let sentinel = other_run_dir.join("credentials").join("auth.json");
    std::fs::create_dir_all(sentinel.parent().unwrap()).expect("create other credentials");
    std::fs::write(&sentinel, "another run's credential").expect("seed the other run");
    std::fs::remove_file(&alias_run_dir).expect("drop the link");
    std::os::unix::fs::symlink(&other_run_dir, &alias_run_dir).expect("worker repoints the link");

    let error = crate::cleanup_ephemeral_credential_materializations(&store, &alias_run_dir, false)
        .expect_err("a repointed link cannot be vouched for");
    assert!(error.contains("without following a link"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&sentinel).ok().as_deref(),
        Some("another run's credential"),
        "cleanup followed the repointed link and deleted another run's file"
    );
}

/// A link nested in the run directory itself is the same case: the worker
/// can rewrite it, so the cleanup refuses the run directory.
#[test]
fn credential_run_scoped_cleanup_refuses_a_link_nested_inside_the_run_dir() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-nested-alias-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = tempfile::tempdir().expect("temp root");
    let real_run_dir = root.path().join("job");
    let alias_run_dir = real_run_dir.join("current");
    std::fs::create_dir_all(real_run_dir.join("credentials")).expect("create real credentials");
    std::os::unix::fs::symlink(&real_run_dir, &alias_run_dir)
        .expect("create alias nested inside real_run_dir");

    let legacy_target = alias_run_dir.join("credentials").join("auth.json");
    let profile = legacy_auth_file_profile(&legacy_target);
    seed_legacy_auth_file(&store, &profile);

    let error = crate::cleanup_ephemeral_credential_materializations(&store, &alias_run_dir, false)
        .expect_err("a nested link cannot be vouched for");
    assert!(error.contains("without following a link"), "{error}");
    assert!(
        real_run_dir.join("credentials").join("auth.json").exists(),
        "the credential file was removed through the nested link"
    );
    let rows = store
        .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&rows[0].value_json).expect("metadata JSON")
            ["cleanup_status"],
        serde_json::json!("cleaned"),
        "a refused run directory must leave its records open"
    );
}

#[test]
fn credential_run_scoped_cleanup_does_not_redirect_when_run_dir_alias_points_elsewhere() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-cleanup-retargeted-alias-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let root = std::fs::canonicalize(tempfile::tempdir().expect("temp root").path())
        .expect("canonical root");
    let dir_a = root.join("run-a");
    let dir_b = root.join("run-b");
    std::fs::create_dir_all(dir_a.join("credentials")).expect("dir_a credentials");
    std::fs::create_dir_all(dir_b.join("credentials")).expect("dir_b credentials");
    std::fs::write(dir_b.join("credentials/auth.json"), "keep-dir-b-file").expect("seed dir_b");

    let alias_run_dir = root.join("run-alias");
    std::os::unix::fs::symlink(&dir_b, &alias_run_dir).expect("alias -> dir_b");

    let profile = legacy_auth_file_profile(&dir_a.join("credentials/auth.json"));
    seed_legacy_auth_file(&store, &profile);

    let error = crate::cleanup_ephemeral_credential_materializations(&store, &alias_run_dir, false)
        .expect_err("a run directory reached through a link is refused outright");
    assert!(error.contains("without following a link"), "{error}");
    assert_eq!(
        std::fs::read_to_string(dir_b.join("credentials/auth.json"))
            .ok()
            .as_deref(),
        Some("keep-dir-b-file"),
        "cleanup must never delete a file in the swapped alias target"
    );
    assert!(
        dir_a.join("credentials/auth.json").exists(),
        "a refused run directory leaves every file alone"
    );
    // A target outside that run directory is read by its own path.
    match crate::cleanup::inspect_target_file(&dir_a.join("credentials/auth.json"), None, 1024) {
        crate::cleanup::TargetInspection::RegularFile { bytes, .. } => {
            assert_ne!(bytes, b"keep-dir-b-file", "inspection read dir_b");
        }
        _ => panic!("expected regular file at dir_a"),
    }
}
