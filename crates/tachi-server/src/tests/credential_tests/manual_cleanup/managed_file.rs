use super::*;

#[test]
fn credential_manual_cleanup_removes_managed_file_without_secret_leak() {
    let db_path = crate::utils::test_fixture_path(format!(
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

    // #1342 follow-up: a NOT-yet-cleaned row must never carry a TTL, or the
    // reap could delete this row's own bookkeeping while the credential file
    // it describes is still live on disk.
    let rows_before_cleanup = store
        .list_state(crate::credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata before cleanup");
    let metadata_before: serde_json::Value =
        serde_json::from_str(&rows_before_cleanup[0].value_json).expect("metadata JSON");
    assert!(
        metadata_before.get("expires_at").is_none(),
        "a not-yet-cleaned row must carry no expires_at at all: {metadata_before}"
    );

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
    // #1342 follow-up: a cleaned row must carry a 30-day `hard_state` TTL —
    // `memcore::reap_expired_state` owns the actual cleanup off this field.
    let expires_at = metadata["expires_at"]
        .as_str()
        .expect("a cleaned credential materialization row must carry expires_at");
    let parsed = chrono::DateTime::parse_from_rfc3339(expires_at)
        .expect("expires_at must be valid RFC3339")
        .with_timezone(&chrono::Utc);
    assert!(
        parsed > chrono::Utc::now() + chrono::Duration::days(29),
        "expires_at ({parsed}) must be roughly 30 days out"
    );

    let _ = std::fs::remove_file(&db_path);
}
