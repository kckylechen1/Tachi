use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn credential_materialize_apply_writes_file_0600_and_returns_env_map() {
    let db_path = crate::utils::test_fixture_path(format!(
        "credential-materialize-apply-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).expect("open store");
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
