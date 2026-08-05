use memcore::vault::VaultEntry;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn test_vault_entry(name: &str, allowed_agents: Option<Vec<String>>) -> VaultEntry {
    VaultEntry {
        name: name.to_string(),
        encrypted_value: "redacted-ciphertext".to_string(),
        nonce: "redacted-nonce".to_string(),
        secret_type: "api_key".to_string(),
        description: String::new(),
        allowed_agents,
        created_at: "2026-06-08T00:00:00Z".to_string(),
        updated_at: "2026-06-08T00:00:00Z".to_string(),
        accessed_at: String::new(),
        access_count: 0,
    }
}

fn codex_auth_file_profile(auth_path: &std::path::Path) -> crate::CredentialProfile {
    crate::CredentialProfile {
        provider: None,
        description: None,
        entries: [("auth_json".to_string(), "CODEX_AUTH_JSON".to_string())]
            .into_iter()
            .collect(),
        allowed_consumers: crate::AllowedConsumers::default(),
        materializers: vec![crate::CredentialMaterializer {
            kind: "file_copy".to_string(),
            source: "auth_json".to_string(),
            target: auth_path.to_string_lossy().to_string(),
            chmod: None,
            template: None,
        }],
    }
}

fn apply_codex_auth_file_materialization(
    store: &memcore::MemoryStore,
    profile: &crate::CredentialProfile,
) {
    let secret_values = [(
        "CODEX_AUTH_JSON".to_string(),
        r#"{"token":"secret-token"}"#.to_string(),
    )]
    .into_iter()
    .collect();

    crate::apply_credential_materialization(
        "codex_shared",
        profile,
        "codex_cli",
        store,
        &secret_values,
        &crate::CredentialApplyOptions::default(),
    )
    .expect("apply materialization");
}

mod config_materialization;
mod doctor;
mod dry_run;
mod file_materialization;
mod manual_cleanup;
mod profile_discovery;
mod run_scoped_cleanup;
