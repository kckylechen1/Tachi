use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn credential_materialize_apply_writes_config_overlay_file_0600() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-materialize-config-file-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
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
