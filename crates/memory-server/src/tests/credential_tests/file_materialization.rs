use super::*;

#[test]
fn credential_materialize_apply_writes_file_0600_and_returns_env_map() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-materialize-apply-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let auth_path = out_dir.path().join("auth.json");
    let profile = crate::credential_profile::CredentialProfile {
        provider: Some("openai_codex".to_string()),
        description: None,
        entries: [
            ("api_key".to_string(), "OPENAI_API_KEY".to_string()),
            ("auth_json".to_string(), "CODEX_AUTH_JSON".to_string()),
        ]
        .into_iter()
        .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![
            crate::credential_profile::CredentialMaterializer {
                kind: "env".to_string(),
                source: "api_key".to_string(),
                target: "OPENAI_API_KEY".to_string(),
                chmod: None,
                template: None,
            },
            crate::credential_profile::CredentialMaterializer {
                kind: "file_copy".to_string(),
                source: "auth_json".to_string(),
                target: auth_path.to_string_lossy().to_string(),
                chmod: None,
                template: None,
            },
        ],
    };
    let secret_values = [
        ("OPENAI_API_KEY".to_string(), "sk-test-secret".to_string()),
        (
            "CODEX_AUTH_JSON".to_string(),
            r#"{"token":"secret-token"}"#.to_string(),
        ),
    ]
    .into_iter()
    .collect();

    let result = crate::credential_profile::apply_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions::default(),
    )
    .expect("apply materialization");
    assert_eq!(
        result.env.get("OPENAI_API_KEY").map(String::as_str),
        Some("sk-test-secret")
    );
    assert_eq!(
        std::fs::read_to_string(&auth_path).expect("read materialized file"),
        r#"{"token":"secret-token"}"#
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&auth_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let report_raw = serde_json::to_string(&result.report).expect("serialize report");
    assert!(!report_raw.contains("sk-test-secret"));
    assert!(!report_raw.contains("secret-token"));
    assert_eq!(result.report.steps[0].status, "prepared_env");
    assert_eq!(result.report.steps[1].status, "written");
    assert!(result.report.steps.iter().all(|step| step.applied));

    let audit_detail: String = store
        .connection()
        .query_row(
            "SELECT detail FROM vault_audit WHERE operation = 'credential_materialize'",
            [],
            |row| row.get(0),
        )
        .expect("credential materialize audit row");
    assert!(audit_detail.contains("consumer=codex_cli"));
    assert!(audit_detail.contains("env:OPENAI_API_KEY"));
    assert!(!audit_detail.contains("sk-test-secret"));
    assert!(!audit_detail.contains("secret-token"));
}

#[test]
fn credential_materialize_apply_records_managed_hash_metadata() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-materialize-managed-hash-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
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

#[test]
fn credential_materialize_apply_refuses_existing_file_by_default() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-materialize-existing-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let auth_path = out_dir.path().join("auth.json");
    std::fs::write(&auth_path, "existing").expect("write existing target");
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
            target: auth_path.to_string_lossy().to_string(),
            chmod: None,
            template: None,
        }],
    };
    let secret_values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"new-secret"}"#.to_string(),
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
    .expect_err("existing file should require allow_existing");
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&auth_path).expect("read existing target"),
        "existing"
    );
}

#[test]
fn credential_materialize_cleans_temp_file_on_chmod_failure() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-temp-cleanup-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
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
