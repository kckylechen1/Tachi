use super::*;

#[test]
fn credential_materialize_apply_records_managed_hash_metadata() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-materialize-managed-hash-test-{}.sqlite",
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

    let rows = store
        .list_state(crate::credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].key.starts_with("managed:"));
    let metadata: serde_json::Value =
        serde_json::from_str(&rows[0].value_json).expect("metadata JSON");
    assert_eq!(metadata["profile"], serde_json::json!("codex_shared"));
    assert_eq!(metadata["consumer"], serde_json::json!("codex_cli"));
    assert_eq!(
        metadata["resolved_secret"],
        serde_json::json!("CODEX_AUTH_JSON")
    );
    assert_eq!(
        metadata["target"],
        serde_json::json!(auth_path.to_string_lossy().to_string())
    );
    assert!(metadata["content_hash"]
        .as_str()
        .unwrap()
        .starts_with("stable-fnv1a:"));
    assert!(!rows[0].value_json.contains("secret-token"));

    let doctor = crate::credential_profile::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
    )
    .expect("doctor report");
    assert!(
        !doctor
            .issues
            .iter()
            .any(|issue| issue.code == "existing_target"),
        "{:#?}",
        doctor.issues
    );
    assert!(
        !doctor
            .issues
            .iter()
            .any(|issue| issue.code == "managed_target_hash_mismatch"),
        "{:#?}",
        doctor.issues
    );
}
