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
