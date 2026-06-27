use super::*;

#[test]
fn credential_materialize_cleanup_removes_run_scoped_credentials() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-cleanup-run-scoped-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let run_dir = tempfile::tempdir().expect("temp run dir");
    let profile = crate::credential_profile::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![
            crate::credential_profile::CredentialMaterializer {
                kind: "file_copy".to_string(),
                source: "auth_json".to_string(),
                target: "{credentials_dir}/auth.json".to_string(),
                chmod: None,
                template: None,
            },
            crate::credential_profile::CredentialMaterializer {
                kind: "config_overlay".to_string(),
                source: "auth_json".to_string(),
                target: "{run_dir}/config.json".to_string(),
                chmod: None,
                template: Some(serde_json::json!({"auth": "{{secret}}"})),
            },
        ],
    };
    let secret_values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"secret-token"}"#.to_string(),
    )]
    .into_iter()
    .collect();

    let result = crate::credential_profile::apply_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions {
            allow_existing: false,
            run_dir: Some(run_dir.path().to_path_buf()),
        },
    )
    .expect("apply run-scoped materialization");
    let auth_path = run_dir.path().join("credentials").join("auth.json");
    let config_path = run_dir.path().join("config.json");
    assert_eq!(
        result.report.steps[0].target,
        auth_path.to_string_lossy().to_string()
    );
    assert_eq!(
        result.report.steps[1].target,
        config_path.to_string_lossy().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(&auth_path).expect("read auth file"),
        r#"{"token":"secret-token"}"#
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).expect("read config file"),
        r#"{"auth":"{\"token\":\"secret-token\"}"}"#
    );

    let dry_run = crate::credential_profile::cleanup_ephemeral_credential_materializations(
        &store,
        run_dir.path(),
        true,
    )
    .expect("dry-run cleanup");
    assert!(dry_run
        .would_remove
        .contains(&auth_path.to_string_lossy().to_string()));
    assert!(dry_run
        .would_remove
        .contains(&config_path.to_string_lossy().to_string()));
    assert!(auth_path.exists(), "dry-run must not remove the file");
    assert!(config_path.exists(), "dry-run must not remove the file");

    let applied = crate::credential_profile::cleanup_ephemeral_credential_materializations(
        &store,
        run_dir.path(),
        false,
    )
    .expect("apply cleanup");
    assert!(applied
        .removed
        .contains(&auth_path.to_string_lossy().to_string()));
    assert!(applied
        .removed
        .contains(&config_path.to_string_lossy().to_string()));
    assert!(
        !auth_path.exists(),
        "cleanup should remove the credential file"
    );
    assert!(
        !config_path.exists(),
        "cleanup should remove run-scoped config files"
    );

    let rows = store
        .list_state(crate::credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let metadata: serde_json::Value =
            serde_json::from_str(&row.value_json).expect("metadata JSON");
        assert_eq!(metadata["cleanup_status"], serde_json::json!("cleaned"));
        assert_eq!(
            metadata["cleanup_run_dir"],
            serde_json::json!(run_dir.path().to_string_lossy().to_string())
        );
        assert!(!row.value_json.contains("secret-token"));
    }

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
            .any(|issue| issue.code == "managed_target_missing"),
        "{:#?}",
        doctor.issues
    );
}
