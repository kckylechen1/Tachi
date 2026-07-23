use super::*;

#[test]
fn credential_materialize_apply_prepares_config_overlay_env_content() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-materialize-config-env-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
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
