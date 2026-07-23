use super::*;

#[test]
fn credential_manual_cleanup_mark_only_keeps_config_patch_file() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-manual-mark-only-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_path = out_dir.path().join("opencode.json");
    std::fs::write(&config_path, r#"{"provider":{"openai":{}}}"#).expect("write existing config");
    let profile = crate::credential_profile::CredentialProfile {
        provider: Some("opencode".to_string()),
        description: None,
        entries: [("api_key".to_string(), "OPENAI_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
            kind: "config_patch".to_string(),
            source: "api_key".to_string(),
            target: config_path.to_string_lossy().to_string(),
            chmod: Some("0600".to_string()),
            template: Some(serde_json::json!({
                "provider": {"openai": {"apiKey": "{{secret}}"}}
            })),
        }],
    };
    let secret_values = [("OPENAI_API_KEY".to_string(), "sk-mark-secret".to_string())]
        .into_iter()
        .collect();
    crate::credential_profile::apply_credential_materialization(
        "opencode_shared",
        &profile,
        "opencode",
        &store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions {
            allow_existing: true,
            run_dir: None,
        },
    )
    .expect("apply config_patch");

    let skipped = crate::credential_profile::cleanup_managed_credential_materializations(
        &store,
        &crate::credential_profile::CredentialCleanupOptions {
            run_dir: None,
            profile: Some("opencode_shared".to_string()),
            consumer: Some("opencode".to_string()),
            dry_run: false,
            mark_only: false,
        },
    )
    .expect("cleanup without mark-only");
    assert!(skipped.removed.is_empty());
    assert!(skipped
        .skipped
        .iter()
        .any(|entry| entry.contains("config_patch requires --mark-only")));
    assert!(config_path.exists());

    let marked = crate::credential_profile::cleanup_managed_credential_materializations(
        &store,
        &crate::credential_profile::CredentialCleanupOptions {
            run_dir: None,
            profile: Some("opencode_shared".to_string()),
            consumer: Some("opencode".to_string()),
            dry_run: false,
            mark_only: true,
        },
    )
    .expect("mark config_patch cleaned");
    assert_eq!(
        marked.marked,
        vec![config_path.to_string_lossy().to_string()]
    );
    assert!(
        config_path.exists(),
        "mark-only must not remove config file"
    );
    let raw = serde_json::to_string(&marked).expect("serialize mark-only report");
    assert!(!raw.contains("sk-mark-secret"));

    let rows = store
        .list_state(crate::credential_profile::CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .expect("list managed credential metadata");
    let metadata: serde_json::Value =
        serde_json::from_str(&rows[0].value_json).expect("metadata JSON");
    assert_eq!(metadata["cleanup_status"], serde_json::json!("cleaned"));
}
