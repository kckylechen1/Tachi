use super::*;

#[test]
fn credential_config_patch_refuses_unsafe_targets() {
    let db_path = crate::test_fixtures::test_fixture_path(format!(
        "credential-config-patch-risk-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CLAUDE_API_KEY", None))
        .expect("insert api key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let unsafe_target = out_dir.path().join(".claude/settings.json");
    let profile = crate::CredentialProfile {
        provider: Some("claude".to_string()),
        description: None,
        entries: [("api_key".to_string(), "CLAUDE_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_patch".to_string(),
            source: "api_key".to_string(),
            target: unsafe_target.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(serde_json::json!({"apiKey": "{{secret}}" })),
        }],
    };
    let secret_values = [("CLAUDE_API_KEY".to_string(), "sk-claude-secret".to_string())]
        .into_iter()
        .collect();

    let err = crate::apply_credential_materialization(
        "claude_shared",
        &profile,
        "claude_code",
        &store,
        &secret_values,
        &crate::CredentialApplyOptions {
            allow_existing: true,
            run_dir: None,
        },
    )
    .expect_err("unsafe config_patch target should be refused");
    assert!(
        err.contains("refusing high-risk credential target"),
        "{err}"
    );
    assert!(!err.contains("sk-claude-secret"));
    assert!(!unsafe_target.exists());
}

/// #1393-L3: OpenCode provider configs must retain an env reference on disk.
/// A Vault secret may be injected into the child environment, but a config
/// patch must never serialize that secret as `apiKey` plaintext.
#[test]
fn opencode_config_patch_refuses_plaintext_api_key_without_writing() {
    let db_path = std::env::temp_dir().join(format!(
        "credential-opencode-plaintext-refusal-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let target = out_dir.path().join("opencode.json");
    let original = r#"{"provider":{"openai":{"baseURL":"https://api.example.test"}}}"#;
    std::fs::write(&target, original).expect("seed config");
    let profile = crate::CredentialProfile {
        provider: Some("opencode".to_string()),
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_patch".to_string(),
            source: "api_key".to_string(),
            target: target.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(serde_json::json!({
                "provider": {"openai": {"apiKey": "{{secret}}"}}
            })),
        }],
    };
    let secret = "opencode-plaintext-sentinel-do-not-write";
    let secret_values = [("OPENAI_API_KEY".to_string(), secret.to_string())]
        .into_iter()
        .collect();

    let err = crate::apply_credential_materialization(
        "opencode_shared",
        &profile,
        "opencode",
        &store,
        &secret_values,
        &crate::CredentialApplyOptions {
            allow_existing: true,
            run_dir: None,
        },
    )
    .expect_err("OpenCode plaintext apiKey patch must be refused");

    assert!(err.contains("plaintext apiKey"), "{err}");
    assert!(!err.contains(secret), "refusal leaked secret: {err}");
    assert_eq!(
        std::fs::read_to_string(&target).expect("read config"),
        original
    );
}

#[test]
fn opencode_profile_name_guard_works_without_provider_metadata() {
    let store = memcore::MemoryStore::open_in_memory().expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");
    let out_dir = tempfile::tempdir().expect("temp output dir");
    let target = out_dir.path().join("custom-config.json");
    let original = r#"{"keep":true}"#;
    std::fs::write(&target, original).expect("seed config");
    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_patch".to_string(),
            source: "api_key".to_string(),
            target: target.to_string_lossy().to_string(),
            chmod: None,
            template: Some(serde_json::json!({"apiKey": "{{secret}}"})),
        }],
    };
    let secret_values = [(
        "OPENAI_API_KEY".to_string(),
        "profile-name-secret".to_string(),
    )]
    .into_iter()
    .collect();

    let err = crate::apply_credential_materialization(
        "custom_opencode_profile",
        &profile,
        "opencode",
        &store,
        &secret_values,
        &crate::CredentialApplyOptions {
            allow_existing: true,
            run_dir: None,
        },
    )
    .expect_err("OpenCode profile name must activate the persistent-secret guard");
    assert!(err.contains("plaintext apiKey"), "{err}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
}

#[test]
fn normalized_opencode_target_guard_catches_aliased_custom_profile() {
    let store = memcore::MemoryStore::open_in_memory().expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");
    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_dir = out_dir.path().join(".config/opencode");
    let alias_dir = out_dir.path().join(".config/alias");
    std::fs::create_dir_all(&config_dir).expect("create OpenCode fixture dir");
    std::fs::create_dir_all(&alias_dir).expect("create alias fixture dir");
    let target = config_dir.join("opencode.json");
    let aliased_target = alias_dir.join("../opencode/opencode.json");
    let original = r#"{"keep":true}"#;
    std::fs::write(&target, original).expect("seed config");
    let profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_patch".to_string(),
            source: "api_key".to_string(),
            target: aliased_target.to_string_lossy().to_string(),
            chmod: None,
            template: Some(serde_json::json!({"apiKey": "{{secret}}"})),
        }],
    };
    let secret_values = [(
        "OPENAI_API_KEY".to_string(),
        "target-guard-secret".to_string(),
    )]
    .into_iter()
    .collect();

    let err = crate::apply_credential_materialization(
        "custom_alias",
        &profile,
        "custom_consumer",
        &store,
        &secret_values,
        &crate::CredentialApplyOptions {
            allow_existing: true,
            run_dir: None,
        },
    )
    .expect_err("normalized OpenCode config target must activate the guard");
    assert!(err.contains("plaintext apiKey"), "{err}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
}

#[test]
fn opencode_file_copy_fails_closed_without_writing_raw_secret() {
    let store = memcore::MemoryStore::open_in_memory().expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENCODE_CONFIG_JSON", None))
        .expect("insert config metadata");
    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_dir = out_dir.path().join(".config/opencode");
    std::fs::create_dir_all(&config_dir).expect("create OpenCode fixture dir");
    let target = config_dir.join("opencode.json");
    let profile = crate::CredentialProfile {
        provider: Some("opencode".to_string()),
        description: None,
        entries: [("config".to_string(), "OPENCODE_CONFIG_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "file_copy".to_string(),
            source: "config".to_string(),
            target: target.to_string_lossy().to_string(),
            chmod: None,
            template: None,
        }],
    };
    let raw_secret = r#"{"provider":{"openai":{"apiKey":"raw-secret"}}}"#;
    let secret_values = [("OPENCODE_CONFIG_JSON".to_string(), raw_secret.to_string())]
        .into_iter()
        .collect();

    let err = crate::apply_credential_materialization(
        "opencode_config_copy",
        &profile,
        "opencode",
        &store,
        &secret_values,
        &crate::CredentialApplyOptions::default(),
    )
    .expect_err("OpenCode file_copy cannot prove env-ref-only output");
    assert!(err.contains("file_copy"), "{err}");
    assert!(!err.contains("raw-secret"), "refusal leaked secret: {err}");
    assert!(!target.exists(), "refused file_copy must not create config");
}

fn opencode_unrelated_patch_profile(target: &std::path::Path) -> crate::CredentialProfile {
    crate::CredentialProfile {
        provider: Some("opencode".to_string()),
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_patch".to_string(),
            source: "api_key".to_string(),
            target: target.to_string_lossy().to_string(),
            chmod: None,
            template: Some(serde_json::json!({"unrelated": {"enabled": true}})),
        }],
    }
}

fn assert_no_tachi_write_artifacts(dir: &std::path::Path) {
    let artifacts = std::fs::read_dir(dir)
        .expect("list fixture directory")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".tachi-bak-") || name.contains(".tachi-tmp-"))
        .collect::<Vec<_>>();
    assert!(
        artifacts.is_empty(),
        "rejected final JSON must not create backup/temp artifacts: {artifacts:?}"
    );
}

fn apply_opencode_unrelated_patch(
    store: &memcore::MemoryStore,
    target: &std::path::Path,
) -> Result<(), String> {
    let profile = opencode_unrelated_patch_profile(target);
    let secret_values = [(
        "OPENAI_API_KEY".to_string(),
        "unused-fixture-secret".to_string(),
    )]
    .into_iter()
    .collect();
    crate::apply_credential_materialization(
        "opencode_final_json_guard",
        &profile,
        "opencode",
        store,
        &secret_values,
        &crate::CredentialApplyOptions {
            allow_existing: true,
            run_dir: None,
        },
    )
    .map(|_| ())
}

#[test]
fn opencode_patch_rejects_existing_nested_literal_apikey_before_backup_or_write() {
    let store = memcore::MemoryStore::open_in_memory().expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");
    let out_dir = tempfile::tempdir().expect("temp output dir");
    let target = out_dir.path().join("opencode.json");
    let original =
        br#"{"provider":{"openai":{"options":{"apiKey":"existing-nested-literal"}}},"keep":true}"#;
    std::fs::write(&target, original).expect("seed config");

    let err = apply_opencode_unrelated_patch(&store, &target)
        .expect_err("final nested literal apiKey must be rejected");
    assert!(err.contains("final") && err.contains("apiKey"), "{err}");
    assert!(
        !err.contains("existing-nested-literal"),
        "error leaked literal: {err}"
    );
    assert_eq!(std::fs::read(&target).expect("read target"), original);
    assert_no_tachi_write_artifacts(out_dir.path());
}

#[test]
fn opencode_patch_rejects_existing_top_level_literal_apikey_before_backup_or_write() {
    let store = memcore::MemoryStore::open_in_memory().expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");
    let out_dir = tempfile::tempdir().expect("temp output dir");
    let target = out_dir.path().join("opencode.json");
    let original = br#"{"apiKey":"existing-top-level-literal","keep":true}"#;
    std::fs::write(&target, original).expect("seed config");

    let err = apply_opencode_unrelated_patch(&store, &target)
        .expect_err("final top-level literal apiKey must be rejected");
    assert!(err.contains("final") && err.contains("apiKey"), "{err}");
    assert!(
        !err.contains("existing-top-level-literal"),
        "error leaked literal: {err}"
    );
    assert_eq!(std::fs::read(&target).expect("read target"), original);
    assert_no_tachi_write_artifacts(out_dir.path());
}

#[test]
fn opencode_patch_allows_existing_env_ref_when_final_json_remains_indirect() {
    let store = memcore::MemoryStore::open_in_memory().expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");
    let out_dir = tempfile::tempdir().expect("temp output dir");
    let target = out_dir.path().join("opencode.json");
    std::fs::write(
        &target,
        br#"{"provider":{"openai":{"apiKey":"{env:OPENAI_API_KEY}"}},"keep":true}"#,
    )
    .expect("seed config");

    apply_opencode_unrelated_patch(&store, &target)
        .expect("existing env-ref apiKey must remain allowed");
    let final_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&target).expect("read target"))
            .expect("parse final config");
    assert_eq!(
        final_json["provider"]["openai"]["apiKey"],
        serde_json::json!("{env:OPENAI_API_KEY}")
    );
    assert_eq!(final_json["unrelated"]["enabled"], serde_json::json!(true));
    assert_no_tachi_write_artifacts(out_dir.path());
}
