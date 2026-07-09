use super::*;

#[test]
fn credential_manual_cleanup_removes_managed_file_without_secret_leak() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-manual-cleanup-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let auth_path = out_dir.path().join("auth.json");
    let profile = codex_auth_file_profile(&auth_path);
    apply_codex_auth_file_materialization(&store, &profile);
    assert!(auth_path.exists());

    let dry_run = crate::credential_profile::cleanup_managed_credential_materializations(
        &store,
        &crate::credential_profile::CredentialCleanupOptions {
            run_dir: None,
            profile: Some("codex_shared".to_string()),
            consumer: Some("codex_cli".to_string()),
            dry_run: true,
            mark_only: false,
        },
    )
    .expect("dry-run manual cleanup");
    assert_eq!(
        dry_run.would_remove,
        vec![auth_path.to_string_lossy().to_string()]
    );
    assert!(auth_path.exists(), "dry-run must not remove target");

    let applied = crate::credential_profile::cleanup_managed_credential_materializations(
        &store,
        &crate::credential_profile::CredentialCleanupOptions {
            run_dir: None,
            profile: Some("codex_shared".to_string()),
            consumer: Some("codex_cli".to_string()),
            dry_run: false,
            mark_only: false,
        },
    )
    .expect("apply manual cleanup");
    assert_eq!(
        applied.removed,
        vec![auth_path.to_string_lossy().to_string()]
    );
    assert!(!auth_path.exists());
    let raw = serde_json::to_string(&applied).expect("serialize cleanup report");
    assert!(!raw.contains("secret-token"));
    assert!(!raw.contains("CODEX_AUTH_JSON\":"));

    let rows = store
        .list_state(crate::credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    let metadata: serde_json::Value =
        serde_json::from_str(&rows[0].value_json).expect("metadata JSON");
    assert_eq!(metadata["cleanup_status"], serde_json::json!("cleaned"));
}
