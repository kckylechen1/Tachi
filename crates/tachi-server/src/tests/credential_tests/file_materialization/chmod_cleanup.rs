use super::*;

#[test]
fn credential_materialize_cleans_temp_file_on_chmod_failure() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-temp-cleanup-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let target = out_dir.path().join("auth.json");
    let profile = crate::credential_profile::CredentialProfile {
        provider: Some("codex".to_string()),
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "file_copy".to_string(),
            source: "auth_json".to_string(),
            target: target.to_string_lossy().to_string(),
            chmod: Some("invalid".to_string()),
            template: None,
        }],
    };
    let secret_values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"secret-token"}"#.to_string(),
    )]
    .into_iter()
    .collect();

    let err = crate::credential_profile::apply_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions::default(),
    )
    .expect_err("invalid chmod should fail materialization");
    assert!(err.contains("invalid chmod"), "{err}");
    assert!(
        !target.exists(),
        "failed materialization must not publish target"
    );

    let temp_leftovers = std::fs::read_dir(out_dir.path())
        .expect("list output dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".tachi-tmp-"))
        })
        .collect::<Vec<_>>();
    assert!(
        temp_leftovers.is_empty(),
        "failed materialization should remove temp files: {temp_leftovers:?}"
    );
}
