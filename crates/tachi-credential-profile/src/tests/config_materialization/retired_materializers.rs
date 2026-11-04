use super::*;

#[test]
fn retired_file_materializers_fail_before_any_write_and_remain_diagnosable() {
    let dir = tempfile::tempdir().expect("fixture");
    let run_dir = dir.path().join("run");
    std::fs::create_dir(&run_dir).expect("run dir");
    let store = memcore::MemoryStore::open(dir.path().join("vault.sqlite").to_str().unwrap())
        .expect("store");
    store
        .vault_upsert_entry(&test_vault_entry("KEY", None))
        .expect("metadata");
    let persistent = dir.path().join("user-config.json");
    let original = r#"{"keep":"unchanged"}"#;
    std::fs::write(&persistent, original).expect("original config");
    let values = [("KEY".to_string(), "synthetic-secret-value".to_string())]
        .into_iter()
        .collect();
    for kind in ["file_copy", "config_patch"] {
        let profile = crate::CredentialProfile {
            provider: None,
            description: None,
            entries: Default::default(),
            allowed_consumers: Default::default(),
            materializers: vec![
                crate::CredentialMaterializer {
                    kind: "config_overlay".to_string(),
                    source: "KEY".to_string(),
                    target: "{credentials_dir}/must-not-write.json".to_string(),
                    chmod: None,
                    template: Some(serde_json::json!({"token": "{{secret}}"})),
                },
                crate::CredentialMaterializer {
                    kind: kind.to_string(),
                    source: "KEY".to_string(),
                    target: persistent.to_string_lossy().to_string(),
                    chmod: None,
                    template: Some(serde_json::json!({"apiKey": "{{secret}}"})),
                },
            ],
        };
        let plan = crate::plan_credential_materialization_with_run_dir(
            "legacy",
            &profile,
            "worker",
            &store,
            Some(&run_dir),
        )
        .expect("diagnosable plan");
        assert_eq!(plan.steps[0].status, "ready");
        assert_eq!(plan.steps[1].status, "retired");
        assert!(!plan.steps[1].would_write);
        let err = crate::apply_credential_materialization(
            "legacy",
            &profile,
            "worker",
            &store,
            &values,
            &crate::CredentialApplyOptions {
                run_dir: Some(run_dir.clone()),
                allow_existing: true,
            },
        )
        .expect_err("retired writer must fail before earlier steps execute");
        assert!(err.contains(kind) && err.contains("retired"), "{err}");
        assert!(err.contains("config_content_env"), "{err}");
        assert!(!err.contains("synthetic-secret-value"), "{err}");
        assert!(!run_dir.join("credentials").exists());
        assert_eq!(std::fs::read_to_string(&persistent).unwrap(), original);
        assert!(store
            .list_state(crate::CREDENTIAL_MATERIALIZATION_NAMESPACE)
            .unwrap()
            .is_empty());
        // Retired file adapters remain diagnosable without a run directory.
        let mut historical_profile = profile;
        historical_profile.materializers.remove(0);
        let doctor =
            crate::doctor_credential_profile("legacy", &historical_profile, "worker", &store, None)
                .expect("historical profile remains diagnosable");
        assert!(doctor
            .issues
            .iter()
            .any(|issue| issue.code == "retired_materializer"));
        assert!(!doctor
            .issues
            .iter()
            .any(|issue| issue.code == "invalid_materializer_target"));
    }
}

#[test]
fn config_content_env_is_the_only_config_environment_adapter() {
    let dir = tempfile::tempdir().expect("fixture");
    let store = memcore::MemoryStore::open(dir.path().join("vault.sqlite").to_str().unwrap())
        .expect("store");
    store
        .vault_upsert_entry(&test_vault_entry("KEY", None))
        .expect("metadata");
    let values = [("KEY".to_string(), "synthetic-secret-value".to_string())]
        .into_iter()
        .collect();
    let mut profile = crate::CredentialProfile {
        provider: None,
        description: None,
        entries: Default::default(),
        allowed_consumers: Default::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "config_overlay".to_string(),
            source: "KEY".to_string(),
            target: "WORKER_CONFIG".to_string(),
            chmod: None,
            template: Some(serde_json::json!({"token": "{{secret}}"})),
        }],
    };
    let error = crate::apply_credential_materialization(
        "worker",
        &profile,
        "worker",
        &store,
        &values,
        &Default::default(),
    )
    .expect_err("ambiguous overlay env form is retired");
    assert!(error.contains("use config_content_env"), "{error}");
    profile.materializers[0].kind = "config_content_env".to_string();
    profile.materializers[0].target = dir
        .path()
        .join("must-not-write.json")
        .to_string_lossy()
        .to_string();
    let error = crate::apply_credential_materialization(
        "worker",
        &profile,
        "worker",
        &store,
        &values,
        &Default::default(),
    )
    .expect_err("content-env must never become a file writer");
    assert!(error.contains("shell env name"), "{error}");
    assert!(!dir.path().join("must-not-write.json").exists());
}
