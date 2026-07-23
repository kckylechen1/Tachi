use super::*;

#[test]
fn credential_doctor_reports_managed_target_hash_mismatch() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-doctor-managed-hash-mismatch-test-{}.sqlite",
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
    std::fs::write(&auth_path, r#"{"token":"edited-outside-tachi"}"#)
        .expect("modify managed target");
    #[cfg(unix)]
    std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
        .expect("keep safe permissions");

    let doctor = crate::credential_profile::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
    )
    .expect("doctor report");
    let codes = doctor
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"managed_target_hash_mismatch"), "{codes:?}");
    assert!(!codes.contains(&"existing_target"), "{codes:?}");
}

#[test]
fn credential_doctor_reports_managed_target_missing() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-doctor-managed-missing-test-{}.sqlite",
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
    std::fs::remove_file(&auth_path).expect("remove managed target");

    let doctor = crate::credential_profile::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
    )
    .expect("doctor report");
    let codes = doctor
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"managed_target_missing"), "{codes:?}");
    assert!(
        !codes.contains(&"managed_target_hash_mismatch"),
        "{codes:?}"
    );
    assert!(!codes.contains(&"existing_target"), "{codes:?}");
}

#[test]
fn credential_doctor_reports_unreadable_managed_metadata() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-doctor-managed-metadata-test-{}.sqlite",
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
    let row = store
        .list_state(crate::credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata")
        .into_iter()
        .next()
        .expect("managed metadata row");
    store
        .set_state(
            crate::credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE,
            &row.key,
            "{not-json",
        )
        .expect("corrupt managed metadata");

    let doctor = crate::credential_profile::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
    )
    .expect("doctor report");
    let codes = doctor
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"managed_metadata_unreadable"), "{codes:?}");
    assert!(codes.contains(&"existing_target"), "{codes:?}");
}
