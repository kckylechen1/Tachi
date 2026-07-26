use super::*;

#[test]
fn credential_run_scoped_cleanup_rejects_parent_dir_targets() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-cleanup-parent-dir-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let parent = tempfile::tempdir().expect("temp parent dir");
    let run_dir = parent.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("create run dir");
    let outside_path = parent.path().join("outside-auth.json");
    let profile = crate::credential_profile::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "file_copy".to_string(),
            source: "auth_json".to_string(),
            target: "{run_dir}/../outside-auth.json".to_string(),
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

    crate::credential_profile::apply_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions {
            allow_existing: false,
            run_dir: Some(run_dir.clone()),
        },
    )
    .expect("apply parent-dir materialization");
    assert!(outside_path.exists());

    let cleanup = crate::credential_profile::cleanup_ephemeral_credential_materializations(
        &store, &run_dir, false,
    )
    .expect("run-scoped cleanup");

    assert!(
        cleanup.removed.is_empty(),
        "parent-dir target must not be removed as run-scoped cleanup: {cleanup:?}"
    );
    assert!(
        cleanup
            .skipped
            .iter()
            .any(|target| target.ends_with("../outside-auth.json")),
        "{cleanup:?}"
    );
    assert!(
        outside_path.exists(),
        "cleanup must not delete files outside the run dir"
    );
}
