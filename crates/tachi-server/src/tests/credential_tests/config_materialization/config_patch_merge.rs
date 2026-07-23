use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn credential_config_patch_merges_json_without_retaining_old_secret_backup() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-config-patch-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
    store
        .vault_upsert_entry(&test_vault_entry("OPENAI_API_KEY", None))
        .expect("insert api key metadata");

    let out_dir = tempfile::tempdir().expect("temp output dir");
    let config_path = out_dir.path().join("opencode.json");
    let existing = r#"{"provider":{"openai":{"baseURL":"https://api.example.test"}},"keep":true}"#;
    std::fs::write(&config_path, existing).expect("write existing config");
    #[cfg(unix)]
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644))
        .expect("set initial broad permissions");

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
                "provider": {
                    "openai": {
                        "apiKey": "{{secret}}"
                    }
                }
            })),
        }],
    };
    let secret_values = [("OPENAI_API_KEY".to_string(), "sk-patch-secret".to_string())]
        .into_iter()
        .collect();

    let result = crate::credential_profile::apply_credential_materialization(
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
    .expect("apply config_patch materialization");

    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_path).expect("read config"))
            .expect("patched config JSON");
    assert_eq!(
        parsed["provider"]["openai"]["baseURL"],
        serde_json::json!("https://api.example.test")
    );
    assert_eq!(
        parsed["provider"]["openai"]["apiKey"],
        serde_json::json!("sk-patch-secret")
    );
    assert_eq!(parsed["keep"], serde_json::json!(true));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let backups = std::fs::read_dir(out_dir.path())
        .expect("list output dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("opencode.json.tachi-bak-"))
        })
        .collect::<Vec<_>>();
    assert!(
        backups.is_empty(),
        "successful materialization should not retain old credential backups: {backups:?}"
    );
    let temp_leftovers = std::fs::read_dir(out_dir.path())
        .expect("list output dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".tachi-tmp-"))
        })
        .collect::<Vec<_>>();
    assert!(
        temp_leftovers.is_empty(),
        "atomic write should not leave temp files: {temp_leftovers:?}"
    );

    assert_eq!(result.report.steps[0].status, "written");
    assert!(result.report.steps[0].applied);
    let raw = serde_json::to_string(&result.report).expect("serialize report");
    assert!(!raw.contains("sk-patch-secret"));
    assert!(raw.contains("config_patch"));
}
