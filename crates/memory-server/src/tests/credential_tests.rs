use memory_core::vault::VaultEntry;

fn test_vault_entry(name: &str, allowed_agents: Option<Vec<String>>) -> VaultEntry {
    VaultEntry {
        name: name.to_string(),
        encrypted_value: "redacted-ciphertext".to_string(),
        nonce: "redacted-nonce".to_string(),
        secret_type: "api_key".to_string(),
        description: String::new(),
        allowed_agents,
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        accessed_at: String::new(),
        access_count: 0,
    }
}

#[test]
fn credential_profile_json_parse_supports_core_materializers() {
    let raw = r#"{
      "credential_profiles": {
        "codex_shared": {
          "provider": "openai_codex",
          "description": "Shared Codex auth",
          "entries": {
            "api_key": "OPENAI_API_KEY",
            "auth_json": "CODEX_AUTH_JSON"
          },
          "allowed_consumers": {
            "agents": ["codex_cli"],
            "profiles": ["codex_55_review"]
          },
          "materializers": [
            {"type": "env", "source": "api_key", "target": "OPENAI_API_KEY"},
            {"type": "file_copy", "source": "auth_json", "target": "~/.codex/auth.json", "chmod": "0600"},
            {"type": "config_overlay", "source": "api_key", "target": "OPENCODE_CONFIG_CONTENT", "template": {"provider": "openai"}}
          ]
        }
      }
    }"#;

    let doc: crate::credential_profile::CredentialProfileDocument =
        serde_json::from_str(raw).expect("profile document parses");
    let profile = doc
        .credential_profiles
        .get("codex_shared")
        .expect("profile exists");
    assert_eq!(profile.provider.as_deref(), Some("openai_codex"));
    assert_eq!(profile.materializers.len(), 3);
    assert_eq!(profile.materializers[1].chmod.as_deref(), Some("0600"));
}

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
    assert_eq!(report.steps[1].status, "ready");
    assert!(report.steps.iter().all(|step| step.redacted));
    assert!(report.steps.iter().all(|step| !step.would_write));

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
