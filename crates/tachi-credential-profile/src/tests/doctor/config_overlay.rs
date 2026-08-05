use super::*;

#[test]
fn credential_doctor_reports_plaintext_config_overlay_secret_drift() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-doctor-plaintext-config-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
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

    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: config_path.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(serde_json::json!({"provider": {"openai": {"apiKey": "{{secret}}"}}})),
        }],
    };

    let report = crate::doctor_credential_profile("opencode_shared", &profile, "opencode", &store)
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
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-doctor-config-ref-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
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

    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: config_path.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(
                serde_json::json!({"provider": {"openai": {"apiKey": "${OPENAI_API_KEY}"}}}),
            ),
        }],
    };

    let report = crate::doctor_credential_profile("opencode_shared", &profile, "opencode", &store)
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
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-doctor-compound-config-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
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

    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: config_path.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(
                serde_json::json!({"provider": {"custom": {"api_key_v2": "{{secret}}"}}}),
            ),
        }],
    };

    let report =
        crate::doctor_credential_profile("custom_shared", &profile, "custom_agent", &store)
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
