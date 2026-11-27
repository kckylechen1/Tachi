// The apply result is handed to dispatch code that logs with `{:?}`; its
// `env` is the decrypted credential set the child is about to receive.
#[test]
fn credential_apply_result_debug_names_env_values_without_printing_them() {
    let result = crate::CredentialApplyResult {
        report: crate::CredentialMaterializeReport {
            profile: "codex_shared".to_string(),
            consumer: "codex_cli".to_string(),
            dry_run: false,
            provider: None,
            allowed: true,
            missing_secrets: Vec::new(),
            denied_secrets: Vec::new(),
            warnings: Vec::new(),
            steps: Vec::new(),
        },
        env: [
            ("ZAI_API_KEY".to_string(), "zz-secret".to_string()),
            ("OPENAI_API_KEY".to_string(), "sk-secret".to_string()),
        ]
        .into_iter()
        .collect(),
    };
    let rendered = format!("{result:?}");
    assert!(!rendered.contains("sk-secret"), "{rendered}");
    assert!(!rendered.contains("zz-secret"), "{rendered}");
    assert!(
        rendered.contains(r#"env: ["OPENAI_API_KEY", "ZAI_API_KEY"]"#),
        "{rendered}"
    );
    assert!(rendered.contains("codex_shared"), "{rendered}");
}
