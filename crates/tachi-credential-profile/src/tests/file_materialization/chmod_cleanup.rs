use super::*;

#[test]
fn credential_materialize_rejects_invalid_or_broad_chmod_before_writing() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-temp-cleanup-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let target = out_dir.path().join("auth.json");
    let mut profile = crate::CredentialProfile {
        provider: Some("codex".to_string()),
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
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

    for (mode, message) in [("invalid", "invalid chmod"), ("0644", "0400 or 0600")] {
        profile.materializers[0].chmod = Some(mode.to_string());
        let err = crate::apply_credential_materialization(
            "codex_shared",
            &profile,
            "codex_cli",
            &store,
            &secret_values,
            &crate::CredentialApplyOptions {
                allow_existing: false,
                run_dir: Some(out_dir.path().to_path_buf()),
            },
        )
        .expect_err("unsafe chmod must fail before creating a credential file");
        assert!(err.contains(message), "{err}");
        assert!(
            !target.exists(),
            "failed materialization must not publish target"
        );
    }

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
        "refused materialization must not create temp files: {temp_leftovers:?}"
    );
}
