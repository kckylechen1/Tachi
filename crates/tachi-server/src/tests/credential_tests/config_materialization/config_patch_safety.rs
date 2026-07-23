use super::*;

#[test]
fn credential_config_patch_refuses_unsafe_targets() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-config-patch-risk-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("CLAUDE_API_KEY", None))
        .expect("insert api key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let unsafe_target = out_dir.path().join(".claude/settings.json");
    let profile = crate::credential_profile::CredentialProfile {
        provider: Some("claude".to_string()),
        description: None,
        entries: [("api_key".to_string(), "CLAUDE_API_KEY".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::credential_profile::AllowedConsumers::default(),
        materializers: vec![crate::credential_profile::CredentialMaterializer {
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

    let err = crate::credential_profile::apply_credential_materialization(
        "claude_shared",
        &profile,
        "claude_code",
        &store,
        &secret_values,
        &crate::credential_profile::CredentialApplyOptions {
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
