use memory_core::vault::VaultEntry;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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

fn codex_auth_file_profile(
    auth_path: &std::path::Path,
) -> crate::credential_profile::CredentialProfile {
    crate::credential_profile::CredentialProfile {
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
    }
}

fn apply_codex_auth_file_materialization(
    store: &memory_core::MemoryStore,
    profile: &crate::credential_profile::CredentialProfile,
) {
    let secret_values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"secret-token"}"#.to_string(),
    )]
    .into_iter()
    .collect();

    crate::credential_profile::apply_credential_materialization(
        "codex_shared",
        profile,
        "codex_cli",
        store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions::default(),
    )
    .expect("apply materialization");
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
fn credential_profile_discovery_skips_unrelated_malformed_json() {
    let dir = tempfile::tempdir().expect("temp credential dir");
    std::fs::write(dir.path().join("broken.json"), "{not valid json")
        .expect("write malformed profile");
    std::fs::write(
        dir.path().join("valid.json"),
        r#"{
          "credential_profiles": {
            "codex_shared": {
              "entries": {"api_key": "OPENAI_API_KEY"},
              "materializers": [
                {"type": "env", "source": "api_key", "target": "OPENAI_API_KEY"}
              ]
            }
          }
        }"#,
    )
    .expect("write valid profile");

    let (path, profile) =
        crate::credential_profile::find_credential_profile(dir.path(), "codex_shared")
            .expect("valid profile should be found despite malformed sibling");
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("valid.json")
    );
    assert_eq!(profile.materializers[0].kind, "env");
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
fn credential_materialize_apply_prepares_config_overlay_env_content() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-materialize-config-env-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");

    let profile = crate::credential_profile::CredentialProfile {
        provider: Some("opencode".to_string()),
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: "OPENCODE_CONFIG_CONTENT".to_string(),
            chmod: None,
            template: Some(serde_json::json!({
                "provider": "openai",
                "apiKey": "{{secret}}",
                "nested": {
                    "token": "{{value}}"
                }
            })),
        }],
    };
    let secret_values = [(
        "OPENAI_API_KEY".to_string(),
        "sk-overlay-secret".to_string(),
    )]
    .into_iter()
    .collect();

    let result = crate::credential_profile::apply_credential_materialization(
        "opencode_shared",
        &profile,
        "opencode",
        &store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions::default(),
    )
    .expect("apply config overlay env materialization");

    let content = result
        .env
        .get("OPENCODE_CONFIG_CONTENT")
        .expect("config content env");
    let parsed: serde_json::Value = serde_json::from_str(content).expect("config content JSON");
    assert_eq!(parsed["provider"], serde_json::json!("openai"));
    assert_eq!(parsed["apiKey"], serde_json::json!("sk-overlay-secret"));
    assert_eq!(
        parsed["nested"]["token"],
        serde_json::json!("sk-overlay-secret")
    );
    assert_eq!(result.report.steps[0].status, "prepared_config_env");
    assert!(result.report.steps[0].applied);
    assert!(!result.report.steps[0].would_write);

    let report_raw = serde_json::to_string(&result.report).expect("serialize report");
    assert!(!report_raw.contains("sk-overlay-secret"));
    assert!(report_raw.contains("config_overlay:OPENCODE_CONFIG_CONTENT"));
}

#[test]
fn credential_materialize_apply_writes_config_overlay_file_0600() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-materialize-config-file-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memory_core::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("ROUTER_API_KEY", None))
        .expect("insert router key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_path = out_dir.path().join("router-config.json");
    let profile = crate::credential_profile::CredentialProfile {
        provider: Some("router".to_string()),
        description: None,
        entries: [("api_key".to_string(), "ROUTER_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "api_key".to_string(),
            target: config_path.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(serde_json::json!({
                "providers": {
                    "router": {
                        "api_key": "{{secret}}"
                    }
                }
            })),
        }],
    };
    let secret_values = [("ROUTER_API_KEY".to_string(), "router-secret".to_string())]
        .into_iter()
        .collect();

    let result = crate::credential_profile::apply_credential_materialization(
        "router_shared",
        &profile,
        "opencode",
        &store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions::default(),
    )
    .expect("apply config overlay file materialization");

    let parsed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&config_path).expect("read config overlay file"),
    )
    .expect("config overlay JSON");
    assert_eq!(
        parsed["providers"]["router"]["api_key"],
        serde_json::json!("router-secret")
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(result.env.is_empty());
    assert_eq!(result.report.steps[0].status, "written");
    assert!(result.report.steps[0].applied);

    let report_raw = serde_json::to_string(&result.report).expect("serialize report");
    assert!(!report_raw.contains("router-secret"));
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
