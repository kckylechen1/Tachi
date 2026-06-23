use super::*;

#[test]
fn credential_doctor_reports_managed_target_hash_mismatch() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-doctor-managed-hash-mismatch-test-{}.sqlite",
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
    let db_path = std::env::temp_dir().join(format!(
        "credential-doctor-managed-missing-test-{}.sqlite",
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
    let db_path = std::env::temp_dir().join(format!(
        "credential-doctor-managed-metadata-test-{}.sqlite",
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

#[test]
fn credential_doctor_reports_missing_secret_existing_target_and_permissions() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-doctor-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CODEX_AUTH_JSON", None))
        .expect("insert auth json metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let auth_path = out_dir.path().join("auth.json");
    std::fs::write(&auth_path, "existing").expect("write existing target");
    #[cfg(unix)]
    std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o644))
        .expect("set broad permissions");

    let profile = crate::credential_profile::CredentialProfile {
        provider: None,
        description: None,
        entries: [
            ("auth_json".to_string(), "CODEX_AUTH_JSON".to_string()),
            ("missing".to_string(), "MISSING_SECRET".to_string()),
        ]
        .into_iter()
        .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![
            crate::credential_profile::CredentialMaterializer {
                kind: "file_copy".to_string(),
                source: "auth_json".to_string(),
                target: auth_path.to_string_lossy().to_string(),
                chmod: None,
                template: None,
            },
            crate::credential_profile::CredentialMaterializer {
                kind: "env".to_string(),
                source: "missing".to_string(),
                target: "MISSING_SECRET".to_string(),
                chmod: None,
                template: None,
            },
        ],
    };

    let report = crate::credential_profile::doctor_credential_profile(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
    )
    .expect("doctor report");
    let codes = report
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"missing_secret"), "{codes:?}");
    assert!(codes.contains(&"existing_target"), "{codes:?}");
    #[cfg(unix)]
    assert!(codes.contains(&"target_permissions_too_broad"), "{codes:?}");
    assert!(report.summary.issue_count >= 2);
}

#[test]
fn credential_doctor_reports_high_risk_targets() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-doctor-risk-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CLAUDE_AUTH_JSON", None))
        .expect("insert claude auth metadata");

    let home = tempfile::tempdir().expect("temp home");
    let target = home.path().join(".claude/settings.json");
    let profile = crate::credential_profile::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CLAUDE_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "file_copy".to_string(),
            source: "auth_json".to_string(),
            target: target.to_string_lossy().to_string(),
            chmod: None,
            template: None,
        }],
    };

    let report = crate::credential_profile::doctor_credential_profile(
        "claude_shared",
        &profile,
        "claude_code",
        &store,
    )
    .expect("doctor report");
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "high_risk_target"),
        "{:#?}",
        report.issues
    );
}

#[test]
fn credential_doctor_reports_plaintext_config_overlay_secret_drift() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-doctor-plaintext-config-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_path = out_dir.path().join("opencode.json");
    std::fs::write(
        &config_path,
        r#"{"provider":{"openai":{"apiKey":"sk-plaintext-secret"}}}"#,
    )
    .expect("write plaintext config target");
    #[cfg(unix)]
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644))
        .expect("set broad permissions");

    let profile = crate::credential_profile::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: config_path.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(serde_json::json!({"provider": {"openai": {"apiKey": "{{secret}}"}}})),
        }],
    };

    let report = crate::credential_profile::doctor_credential_profile(
        "opencode_shared",
        &profile,
        "opencode",
        &store,
    )
    .expect("doctor report");
    let codes = report
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"existing_target"), "{codes:?}");
    assert!(codes.contains(&"plaintext_config_secret"), "{codes:?}");
    #[cfg(unix)]
    assert!(codes.contains(&"target_permissions_too_broad"), "{codes:?}");
    assert_eq!(
        codes
            .iter()
            .filter(|code| **code == "plaintext_config_secret")
            .count(),
        1,
        "{codes:?}"
    );
}

#[test]
fn credential_doctor_allows_config_overlay_env_and_vault_references() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-doctor-config-ref-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_path = out_dir.path().join("opencode.json");
    std::fs::write(
        &config_path,
        r#"{"provider":{"openai":{"apiKey":"${OPENAI_API_KEY}"},"zai":{"apiKey":"vault:ZAI_API_KEY"},"moonshot":{"api_key":"$MOONSHOT_API_KEY"},"template":{"apiKey":"{{secret}}"}}}"#,
    )
    .expect("write referenced config target");
    #[cfg(unix)]
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))
        .expect("set safe permissions");

    let profile = crate::credential_profile::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: config_path.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(
                serde_json::json!({"provider": {"openai": {"apiKey": "${OPENAI_API_KEY}"}}}),
            ),
        }],
    };

    let report = crate::credential_profile::doctor_credential_profile(
        "opencode_shared",
        &profile,
        "opencode",
        &store,
    )
    .expect("doctor report");
    assert!(
        !report
            .issues
            .iter()
            .any(|issue| issue.code == "plaintext_config_secret"),
        "{:#?}",
        report.issues
    );
}

#[test]
fn credential_doctor_detects_compound_secretish_config_keys() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-doctor-compound-config-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_path = out_dir.path().join("agent.json");
    std::fs::write(
        &config_path,
        r#"{"provider":{"custom":{"my_secret_key":"sk-plaintext-secret","api_key_v2":"sk-second-secret"}}}"#,
    )
    .expect("write compound plaintext config target");

    let profile = crate::credential_profile::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: config_path.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(
                serde_json::json!({"provider": {"custom": {"api_key_v2": "{{secret}}"}}}),
            ),
        }],
    };

    let report = crate::credential_profile::doctor_credential_profile(
        "custom_shared",
        &profile,
        "custom_agent",
        &store,
    )
    .expect("doctor report");
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.code == "plaintext_config_secret"),
        "{:#?}",
        report.issues
    );
}
