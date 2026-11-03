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

fn legacy_auth_file_profile(auth_path: &std::path::Path) -> crate::CredentialProfile {
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

fn seed_legacy_materialization(
    store: &memcore::MemoryStore,
    profile_name: &str,
    consumer: &str,
    profile: &crate::CredentialProfile,
    content: &str,
) {
    let materializer = &profile.materializers[0];
    let target = std::path::Path::new(&materializer.target);
    std::fs::create_dir_all(target.parent().expect("legacy fixture parent"))
        .expect("create legacy fixture parent");
    std::fs::write(target, content).expect("seed historical credential file");
    #[cfg(unix)]
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o600))
        .expect("restrict historical fixture");
    crate::managed::record_managed_materialization(
        store,
        profile_name,
        consumer,
        &crate::CredentialMaterializeStepReport {
            materializer_type: materializer.kind.clone(),
            source: materializer.source.clone(),
            resolved_secret: crate::plan::resolve_source(profile, &materializer.source),
            target: materializer.target.clone(),
            output: format!("{}:{}", materializer.kind, materializer.target),
            status: "written".to_string(),
            redacted: true,
            would_write: false,
            applied: true,
            chmod: materializer.chmod.clone(),
        },
        content,
    )
    .expect("seed historical managed metadata");
}

fn seed_legacy_auth_file(store: &memcore::MemoryStore, profile: &crate::CredentialProfile) {
    seed_legacy_materialization(
        store,
        "codex_shared",
        "codex_cli",
        profile,
        r#"{"token":"secret-token"}"#,
    );
}

mod config_materialization;
mod doctor;
mod dry_run;
mod file_materialization;
mod manual_cleanup;
mod profile_discovery;
mod run_scoped_cleanup;
