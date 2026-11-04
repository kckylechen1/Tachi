use super::bindings::build_project_env_plan;
use super::materialize::{resolve_project_env_values, run_with_project_env};
use super::shell::shell_export_line;
use super::types::UnlockedVaultStore;
use crate::vault_ops::parse_project_vault_env_bindings_detailed;

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
    let db_path = crate::utils::test_fixture_path(format!(
        "tachi-env-cmd-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    memcore::MemoryStore::open(db_path.to_str().expect("utf8 temp db"))
        .expect("open temp memory store")
}

fn put_secret(store: &memcore::MemoryStore, key: &[u8; 32], name: &str, value: &str) {
    put_secret_typed(store, key, name, value, "api_key");
}

fn put_secret_typed(
    store: &memcore::MemoryStore,
    key: &[u8; 32],
    name: &str,
    value: &str,
    secret_type: &str,
) {
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(key, value.as_bytes()).expect("encrypt test secret");
    store
        .vault_upsert_entry(&memcore::vault::VaultEntry {
            name: name.to_string(),
            encrypted_value,
            nonce,
            secret_type: secret_type.to_string(),
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
fn local_lease_refuses_explicit_config_even_when_name_is_not_lane_config() {
    let store = temp_store();
    let key = crate::vault_crypto::derive_cheap("test-password", b"1234567890123456").expect("key");
    put_secret_typed(
        &store,
        key.bytes(),
        "CUSTOM_ENDPOINT",
        "https://example.test/v1",
        "config",
    );
    let err =
        super::super::vault_cli::lease_api_key_from_store(&store, key.bytes(), "CUSTOM_ENDPOINT")
            .expect_err("explicit config must not lease as an API key");
    let msg = err.to_string();
    assert!(
        msg.contains("config") && msg.contains("not a credential"),
        "{msg}"
    );
}

#[test]
fn member_lease_preflight_validates_the_canonical_rotation_member_set() {
    let store = temp_store();
    let key = crate::vault_crypto::derive_cheap("test-password", b"1234567890123456").expect("key");
    put_secret(&store, key.bytes(), "CUSTOM_POOL_API_KEY_1", "key-one");
    put_secret(&store, key.bytes(), "CUSTOM_POOL_API_KEY_2", "key-two");
    put_secret_typed(
        &store,
        key.bytes(),
        "CUSTOM_POOL_API_KEY_3",
        "not-a-key",
        "config",
    );
    store
        .vault_set_rotation(&memcore::vault::VaultKeyRotation {
            prefix: "CUSTOM_POOL_API_KEY".to_string(),
            current_index: 1,
            total_keys: 2,
            rotation_strategy: "round_robin".to_string(),
            created_at: "2026-06-09T00:00:00Z".to_string(),
            updated_at: "2026-06-09T00:00:00Z".to_string(),
        })
        .expect("seed poisoned legacy rotation");

    let error =
        super::super::vault_cli::validate_api_key_lease_target(&store, "CUSTOM_POOL_API_KEY_2")
            .expect_err("member lease must validate the canonical prefix")
            .to_string();
    assert!(
        error.contains("CUSTOM_POOL_API_KEY_3") && error.contains("config"),
        "{error}"
    );
}

#[test]
fn project_env_plan_marks_secret_pool_and_missing() {
    let store = temp_store();
    let key = crate::vault_crypto::derive_cheap("test-password", b"1234567890123456").expect("key");
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

    let plan = build_project_env_plan(&store, &project).expect("plan");
    assert_eq!(plan.binding_count, 3);
    assert_eq!(plan.missing_count, 1);
    assert_eq!(plan.ignored_lines.len(), 1);
    assert_eq!(plan.bindings[0].source, "secret");
    assert_eq!(plan.bindings[1].source, "pool");
    assert_eq!(plan.bindings[2].status, "missing");
}

#[test]
fn project_env_materialization_rejects_restricted_secret_and_does_not_touch_it() {
    let store = temp_store();
    let key = crate::vault_crypto::derive_cheap("test-password", b"1234567890123456").expect("key");
    let (encrypted_value, nonce) =
        crate::vault_crypto::encrypt(key.bytes(), b"restricted-value").expect("encrypt");
    store
        .vault_upsert_entry(&memcore::vault::VaultEntry {
            name: "restricted.secret".to_string(),
            encrypted_value,
            nonce,
            secret_type: "api_key".to_string(),
            description: String::new(),
            allowed_agents: Some(vec!["agent-a".to_string()]),
            created_at: "2026-06-09T00:00:00Z".to_string(),
            updated_at: "2026-06-09T00:00:00Z".to_string(),
            accessed_at: String::new(),
            access_count: 0,
        })
        .expect("seed restricted secret");
    let mut unlocked = UnlockedVaultStore { store, key };
    let temp = tempfile::tempdir().expect("temp project");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi");
    std::fs::write(
        project.join(".tachi/vault.env"),
        "PROJECT_SECRET=vault:restricted.secret\n",
    )
    .expect("write bindings");

    let error = resolve_project_env_values(&mut unlocked, &project)
        .expect_err("identity-less project export must reject restricted secret")
        .to_string();
    assert!(error.contains("agent_id is required"), "{error}");
    let retained = unlocked
        .store
        .vault_get_entry("restricted.secret")
        .expect("read secret")
        .expect("secret remains");
    assert_eq!(retained.access_count, 0);
}

#[test]
fn project_env_materialization_resolves_lane_slot_pointer() {
    let store = temp_store();
    let key = crate::vault_crypto::derive_cheap("test-password", b"1234567890123456").expect("key");
    put_secret(&store, key.bytes(), "DEEPSEEK_API_KEY", "deepseek-secret");
    put_secret(
        &store,
        key.bytes(),
        "EXTRACT_API_KEY",
        "vault:DEEPSEEK_API_KEY",
    );
    let mut unlocked = UnlockedVaultStore { store, key };
    let temp = tempfile::tempdir().expect("temp project");
    let project = temp.path().join("project");
    std::fs::create_dir_all(project.join(".tachi")).expect("create .tachi");
    std::fs::write(
        project.join(".tachi/vault.env"),
        "MY_KEY=vault:EXTRACT_API_KEY\n",
    )
    .expect("write bindings");

    let exports =
        resolve_project_env_values(&mut unlocked, &project).expect("resolve slot binding");
    assert_eq!(
        exports.as_slice(),
        &[("MY_KEY".to_string(), "deepseek-secret".to_string())]
    );
    assert!(!project.join(".tachi/env.generated").exists());
}

#[test]
#[cfg(unix)]
fn explicit_environment_runner_preserves_child_exit_code() {
    let cwd = tempfile::tempdir().expect("cwd");
    let error = run_with_project_env(
        cwd.path(),
        &["/bin/sh".into(), "-c".into(), "exit 37".into()],
        &[],
    )
    .expect_err("nonzero child exit");
    assert_eq!(
        error
            .downcast_ref::<crate::bootstrap::VaultCommandExit>()
            .expect("typed child exit")
            .code(),
        37
    );
}
