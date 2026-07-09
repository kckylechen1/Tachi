use super::bindings::{build_project_env_plan, parse_project_vault_env_bindings_detailed};
use super::materialize::sync_project_env;
use super::shell::shell_export_line;
use super::types::UnlockedVaultStore;

#[test]
fn parses_project_vault_env_bindings_with_diagnostics() {
    let (bindings, ignored) = parse_project_vault_env_bindings_detailed(
        "\
# comment
export OPENAI_API_KEY=vault:OPENAI_API_KEY
BAD-NAME=vault:bad
LITERAL=value
BROKEN
",
    );
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].env_name, "OPENAI_API_KEY");
    assert_eq!(bindings[0].secret_name, "OPENAI_API_KEY");
    assert_eq!(ignored.len(), 3);
}

#[test]
fn shell_export_escapes_single_quotes() {
    assert_eq!(shell_export_line("A", "x'y"), "export A='x'\\''y'");
}

fn temp_store() -> memcore::MemoryStore {
    let db_path = std::env::temp_dir().join(format!(
        "tachi-env-cmd-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    memcore::MemoryStore::open(db_path.to_str().expect("utf8 temp db"))
        .expect("open temp memory store")
}

fn put_secret(store: &memcore::MemoryStore, key: &[u8; 32], name: &str, value: &str) {
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(key, value.as_bytes()).expect("encrypt test secret");
    store
        .vault_upsert_entry(&memcore::vault::VaultEntry {
            name: name.to_string(),
            encrypted_value,
            nonce,
            secret_type: "api_key".to_string(),
            description: "test secret".to_string(),
            allowed_agents: None,
            created_at: "2026-06-09T00:00:00Z".to_string(),
            updated_at: "2026-06-09T00:00:00Z".to_string(),
            accessed_at: String::new(),
            access_count: 0,
        })
        .expect("upsert test secret");
}

#[test]
fn project_env_plan_marks_secret_pool_and_missing() {
    let store = temp_store();
    let key = crate::vault_crypto::DerivedVaultKey::derive("test-password", b"1234567890123456")
        .expect("key");
    put_secret(&store, key.bytes(), "direct.secret", "direct-value");
    store
        .vault_set_rotation(&memcore::vault::VaultKeyRotation {
            prefix: "POOL_API_KEY".to_string(),
            current_index: 1,
            total_keys: 2,
            rotation_strategy: "round_robin".to_string(),
            created_at: "2026-06-09T00:00:00Z".to_string(),
            updated_at: "2026-06-09T00:00:00Z".to_string(),
        })
        .expect("set rotation");

    let temp = tempfile::tempdir().expect("temp project");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi");
    std::fs::write(
        project.join(".tachi/vault.env"),
        "\
PROJECT_DIRECT=vault:direct.secret
PROJECT_POOL=vault:POOL_API_KEY
PROJECT_MISSING=vault:missing.secret
BAD-NAME=vault:direct.secret
",
    )
    .expect("write bindings");

    let plan = build_project_env_plan(&store, &project, None).expect("plan");
    assert_eq!(plan.binding_count, 3);
    assert_eq!(plan.missing_count, 1);
    assert_eq!(plan.ignored_lines.len(), 1);
    assert_eq!(plan.bindings[0].source, "secret");
    assert_eq!(plan.bindings[1].source, "pool");
    assert_eq!(plan.bindings[2].status, "missing");
}

#[test]
fn project_env_sync_preview_does_not_write() {
    let store = temp_store();
    let key = crate::vault_crypto::DerivedVaultKey::derive("test-password", b"1234567890123456")
        .expect("key");
    put_secret(&store, key.bytes(), "direct.secret", "direct-value");
    let unlocked = UnlockedVaultStore { store, key };

    let temp = tempfile::tempdir().expect("temp project");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi");
    std::fs::write(
        project.join(".tachi/vault.env"),
        "PROJECT_DIRECT=vault:direct.secret\n",
    )
    .expect("write bindings");

    let report = sync_project_env(&unlocked, &project, None, true, false, None, false)
        .expect("preview sync");
    assert!(!report.written);
    assert!(report.dry_run);
    assert!(!project.join(".tachi/env.generated").exists());
}

#[test]
fn project_env_sync_writes_generated_exports() {
    let store = temp_store();
    let key = crate::vault_crypto::DerivedVaultKey::derive("test-password", b"1234567890123456")
        .expect("key");
    put_secret(&store, key.bytes(), "direct.secret", "direct-value");
    put_secret(&store, key.bytes(), "POOL_API_KEY_1", "pool-value-1");
    store
        .vault_set_rotation(&memcore::vault::VaultKeyRotation {
            prefix: "POOL_API_KEY".to_string(),
            current_index: 1,
            total_keys: 1,
            rotation_strategy: "round_robin".to_string(),
            created_at: "2026-06-09T00:00:00Z".to_string(),
            updated_at: "2026-06-09T00:00:00Z".to_string(),
        })
        .expect("set rotation");
    let unlocked = UnlockedVaultStore { store, key };

    let temp = tempfile::tempdir().expect("temp project");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi");
    std::fs::write(
        project.join(".tachi/vault.env"),
        "\
PROJECT_DIRECT=vault:direct.secret
PROJECT_POOL=vault:POOL_API_KEY
",
    )
    .expect("write bindings");

    let report = sync_project_env(&unlocked, &project, None, false, false, None, false)
        .expect("sync project env");
    assert!(report.written);
    let generated = project.join(".tachi/env.generated");
    let content = std::fs::read_to_string(&generated).expect("read generated env");
    assert!(content.contains("DO NOT COMMIT plaintext secrets"));
    assert!(content.contains("export PROJECT_DIRECT='direct-value'"));
    assert!(content.contains("export PROJECT_POOL='pool-value-1'"));
    let leftovers: Vec<_> = std::fs::read_dir(generated.parent().unwrap())
        .expect("read generated dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("env.generated.tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "generated env writes should not leave temp files: {leftovers:?}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&generated)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn project_env_sync_refuses_overwrite_without_force() {
    let store = temp_store();
    let key = crate::vault_crypto::DerivedVaultKey::derive("test-password", b"1234567890123456")
        .expect("key");
    put_secret(&store, key.bytes(), "direct.secret", "new-value");
    let unlocked = UnlockedVaultStore { store, key };

    let temp = tempfile::tempdir().expect("temp project");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi");
    std::fs::write(
        project.join(".tachi/vault.env"),
        "PROJECT_DIRECT=vault:direct.secret\n",
    )
    .expect("write bindings");
    let generated = project.join(".tachi/env.generated");
    std::fs::write(&generated, "existing\n").expect("write existing generated env");

    let err = sync_project_env(&unlocked, &project, None, false, false, None, false)
        .expect_err("sync without force must reject existing output");
    assert!(
        err.to_string().contains("pass --force to overwrite"),
        "unexpected error: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&generated).expect("read generated"),
        "existing\n"
    );

    let report = sync_project_env(&unlocked, &project, None, false, true, None, false)
        .expect("force sync should overwrite");
    assert!(report.written);
    let content = std::fs::read_to_string(&generated).expect("read forced output");
    assert!(content.contains("DO NOT COMMIT plaintext secrets"));
    assert!(content.contains("export PROJECT_DIRECT='new-value'"));
}
