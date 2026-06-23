use super::*;

#[test]
fn credential_materialize_dry_run_reports_redacted_outputs() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-materialize-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");
    store
        .vault_upsert_entry(&test_vault_entry(
            "CODEX_AUTH_JSON",
            Some(vec!["codex_cli".to_string()]),
        ))
        .expect("insert auth json metadata");

    let profile = crate::credential_profile::CredentialProfile {
        provider: Some("openai_codex".to_string()),
        description: None,
        entries: [
            ("api_key".to_string(), "OPENAI_API_KEY".to_string()),
            ("auth_json".to_string(), "CODEX_AUTH_JSON".to_string()),
        ]
        .into_iter()
        .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers {
            agents: vec!["codex_cli".to_string()],
            profiles: vec![],
        },
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
                target: "~/.codex/auth.json".to_string(),
                chmod: Some("0600".to_string()),
                template: None,
            },
        ],
    };

    let report = crate::credential_profile::plan_credential_materialization(
        "codex_shared",
        &profile,
        "codex_cli",
        &store,
    )
    .expect("dry-run report");
    assert!(report.allowed);
    assert!(report.missing_secrets.is_empty());
    assert!(report.denied_secrets.is_empty());
    assert_eq!(report.steps[0].status, "ready");
    assert_eq!(report.steps[0].output, "env:OPENAI_API_KEY");
    assert!(!report.steps[0].would_write);
    assert_eq!(report.steps[1].status, "ready");
    assert!(report.steps[1].would_write);
    assert!(report.steps.iter().all(|step| step.redacted));

    let raw = serde_json::to_string(&report).expect("serialize report");
    assert!(!raw.contains("redacted-ciphertext"));
    assert!(!raw.contains("redacted-nonce"));
}

#[test]
fn credential_materialize_dry_run_enforces_consumer_and_entry_allowlists() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-materialize-deny-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry(
            "CODEX_AUTH_JSON",
            Some(vec!["codex_cli".to_string()]),
        ))
        .expect("insert restricted secret metadata");

    let profile = crate::credential_profile::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers {
            agents: vec!["codex_cli".to_string()],
            profiles: vec![],
        },
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "file_copy".to_string(),
            source: "auth_json".to_string(),
            target: "~/.codex/auth.json".to_string(),
            chmod: Some("0600".to_string()),
            template: None,
        }],
    };

    let report = crate::credential_profile::plan_credential_materialization(
        "codex_shared",
        &profile,
        "opencode",
        &store,
    )
    .expect("dry-run report");
    assert!(!report.allowed);
    assert_eq!(report.steps[0].status, "denied_secret");
    assert_eq!(report.denied_secrets, vec!["CODEX_AUTH_JSON"]);
}
