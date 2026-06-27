use super::*;

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
