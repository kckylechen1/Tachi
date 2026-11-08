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

#[test]
fn env_set_stores_secret_updates_bindings_and_guards_conflicts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("tachi-vault.sqlite");
    let pw_file = temp.path().join("pw.txt");
    std::fs::write(&pw_file, "test-master-password\n").expect("write pw");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pw_file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    let key = super::super::vault_cli::vault_init_with_password(
        &db_path,
        "test-master-password".to_string(),
    )
    .expect("init vault");

    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).expect("mkdir project");
    let creds_path = project.join(".env.credentials");
    std::fs::write(&creds_path, "# header\nEXISTING=1\n").expect("write initial creds");

    let mut input = std::io::Cursor::new(b"poly-secret-123\n");
    super::edit::run_env_set_with_reader(
        super::edit::EnvSetArgs {
            global_db_path: &db_path,
            cwd: &project,
            key: "POLYGON_API_KEY",
            secret: None,
            file: Some(std::path::Path::new(".env.credentials")),
            secret_type: None,
            description: Some("polygon key"),
            value_stdin: true,
            replace: false,
            stdin_password: false,
            keychain: false,
            password_file: Some(&pw_file),
            insecure_password_file: false,
        },
        &mut input,
    )
    .expect("env set");

    let creds_after = std::fs::read_to_string(&creds_path).expect("read creds");
    assert!(creds_after.contains("POLYGON_API_KEY=vault:POLYGON_API_KEY"));
    let vault_env =
        std::fs::read_to_string(project.join(".tachi/vault.env")).expect("read vault.env");
    assert!(vault_env.contains("POLYGON_API_KEY=vault:POLYGON_API_KEY"));

    // Different value without --replace must fail
    let mut conflict_input = std::io::Cursor::new(b"poly-secret-different\n");
    let conflict_err = super::edit::run_env_set_with_reader(
        super::edit::EnvSetArgs {
            global_db_path: &db_path,
            cwd: &project,
            key: "POLYGON_API_KEY",
            secret: None,
            file: None,
            secret_type: None,
            description: None,
            value_stdin: true,
            replace: false,
            stdin_password: false,
            keychain: false,
            password_file: Some(&pw_file),
            insecure_password_file: false,
        },
        &mut conflict_input,
    )
    .expect_err("conflicting value without --replace must fail")
    .to_string();
    assert!(conflict_err.contains("--replace"), "{conflict_err}");

    // With --replace, updates cleanly and resolves via resolve_project_env_values
    let mut replace_input = std::io::Cursor::new(b"poly-secret-updated\n");
    super::edit::run_env_set_with_reader(
        super::edit::EnvSetArgs {
            global_db_path: &db_path,
            cwd: &project,
            key: "POLYGON_API_KEY",
            secret: None,
            file: None,
            secret_type: None,
            description: None,
            value_stdin: true,
            replace: true,
            stdin_password: false,
            keychain: false,
            password_file: Some(&pw_file),
            insecure_password_file: false,
        },
        &mut replace_input,
    )
    .expect("env set --replace");

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).unwrap();
    let mut unlocked = UnlockedVaultStore { store, key };
    let exports = resolve_project_env_values(&mut unlocked, &project).expect("resolve exports");
    assert_eq!(
        exports.as_slice(),
        &[(
            "POLYGON_API_KEY".to_string(),
            "poly-secret-updated".to_string()
        )]
    );
}

#[test]
fn env_seal_ingests_plaintext_rewrites_in_place_and_exports_original_values() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("tachi-vault.sqlite");
    let pw_file = temp.path().join("pw.txt");
    std::fs::write(&pw_file, "test-master-password\n").expect("write pw");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pw_file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    let key = super::super::vault_cli::vault_init_with_password(
        &db_path,
        "test-master-password".to_string(),
    )
    .expect("init vault");

    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).expect("mkdir project");
    let creds_path = project.join(".env.credentials");
    std::fs::write(
        &creds_path,
        "# Market credentials\nTUSHARE_TOKEN=ts-plain-secret-999 # inline comment\nexport IFIND_REFRESH_TOKEN='ifind-quoted-secret'\n",
    )
    .expect("write plaintext .env.credentials");

    // Mixed .env without --key must be refused
    let mixed_env = project.join(".env");
    std::fs::write(&mixed_env, "PORT=18888\nAPI_KEY=plain\n").expect("write .env");
    let mixed_err = super::edit::run_env_seal(super::edit::EnvSealArgs {
        global_db_path: &db_path,
        cwd: &project,
        file: Some(std::path::Path::new(".env")),
        keys: &[],
        dry_run: false,
        replace: false,
        stdin_password: false,
        keychain: false,
        password_file: Some(&pw_file),
        insecure_password_file: false,
    })
    .expect_err("mixed .env without --key must fail")
    .to_string();
    assert!(mixed_err.contains("--key"), "{mixed_err}");

    // Seal .env.credentials
    super::edit::run_env_seal(super::edit::EnvSealArgs {
        global_db_path: &db_path,
        cwd: &project,
        file: None,
        keys: &[],
        dry_run: false,
        replace: false,
        stdin_password: false,
        keychain: false,
        password_file: Some(&pw_file),
        insecure_password_file: false,
    })
    .expect("seal .env.credentials");

    let sealed_content = std::fs::read_to_string(&creds_path).expect("read sealed file");
    assert!(!sealed_content.contains("ts-plain-secret-999"));
    assert!(!sealed_content.contains("ifind-quoted-secret"));
    assert!(sealed_content.contains("# Market credentials\n"));
    assert!(sealed_content.contains("TUSHARE_TOKEN=vault:TUSHARE_TOKEN # inline comment\n"));
    assert!(sealed_content.contains("export IFIND_REFRESH_TOKEN=vault:IFIND_REFRESH_TOKEN\n"));

    let store = memcore::MemoryStore::open(db_path.to_str().unwrap()).unwrap();
    let mut unlocked = UnlockedVaultStore { store, key };
    let exports = resolve_project_env_values(&mut unlocked, &project).expect("resolve exports");
    assert_eq!(
        exports.as_slice(),
        &[
            (
                "IFIND_REFRESH_TOKEN".to_string(),
                "ifind-quoted-secret".to_string()
            ),
            (
                "TUSHARE_TOKEN".to_string(),
                "ts-plain-secret-999".to_string()
            ),
        ]
    );
}

#[test]
#[cfg(unix)]
fn env_set_and_seal_deduplicate_repeated_keys_and_reject_outside_tachi_symlink() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("tachi-vault.sqlite");
    let pw_file = temp.path().join("pw.txt");
    std::fs::write(&pw_file, "test-master-password\n").expect("write pw");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pw_file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    let _key = super::super::vault_cli::vault_init_with_password(
        &db_path,
        "test-master-password".to_string(),
    )
    .expect("init vault");

    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).expect("mkdir project");

    // 1. .tachi symlink pointing outside project must be refused
    std::os::unix::fs::symlink(&outside, project.join(".tachi")).expect("symlink .tachi");
    let mut input = std::io::Cursor::new(b"secret-1\n");
    let err = super::edit::run_env_set_with_reader(
        super::edit::EnvSetArgs {
            global_db_path: &db_path,
            cwd: &project,
            key: "MY_API_KEY",
            secret: None,
            file: None,
            secret_type: None,
            description: None,
            value_stdin: true,
            replace: true,
            stdin_password: false,
            keychain: false,
            password_file: Some(&pw_file),
            insecure_password_file: false,
        },
        &mut input,
    )
    .expect_err("outside .tachi symlink must fail")
    .to_string();
    assert!(err.contains("outside project directory"), "{err}");
    assert!(!outside.join("vault.env").exists());
    std::fs::remove_file(project.join(".tachi")).expect("remove symlink");

    // 2. Duplicate keys in target dotenv file are collapsed to a single pointer
    let creds = project.join(".env.credentials");
    std::fs::write(
        &creds,
        "MY_API_KEY=vault:OLD_ONE\nOTHER=1\nMY_API_KEY=vault:OLD_TWO\n",
    )
    .expect("write duplicate keys");
    let mut input2 = std::io::Cursor::new(b"secret-1\n");
    super::edit::run_env_set_with_reader(
        super::edit::EnvSetArgs {
            global_db_path: &db_path,
            cwd: &project,
            key: "MY_API_KEY",
            secret: Some("NEW_SECRET"),
            file: Some(std::path::Path::new(".env.credentials")),
            secret_type: None,
            description: None,
            value_stdin: true,
            replace: true,
            stdin_password: false,
            keychain: false,
            password_file: Some(&pw_file),
            insecure_password_file: false,
        },
        &mut input2,
    )
    .expect("env set collapses duplicates");
    let creds_after = std::fs::read_to_string(&creds).expect("read creds");
    assert_eq!(
        creds_after.matches("MY_API_KEY=").count(),
        1,
        "{creds_after}"
    );
    assert!(creds_after.contains("MY_API_KEY=vault:NEW_SECRET\n"));
}

fn vault_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("tachi-vault.sqlite");
    let pw_file = temp.path().join("pw.txt");
    std::fs::write(&pw_file, "test-master-password\n").expect("write pw");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pw_file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    super::super::vault_cli::vault_init_with_password(&db_path, "test-master-password".to_string())
        .expect("init vault");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).expect("mkdir project");
    (temp, db_path, pw_file, project)
}

#[test]
fn env_set_replaces_a_whole_multi_line_value_and_keeps_file_format() {
    let (_temp, db_path, pw_file, project) = vault_fixture();
    let creds_path = project.join(".env.credentials");
    // CRLF, no trailing newline, a multi-line quoted secret and an inline comment.
    std::fs::write(
        &creds_path,
        "PRIVATE_KEY=\"first-secret-line\r\nremaining-secret\" # rotated monthly\r\nOTHER=1",
    )
    .expect("write creds");

    let mut input = std::io::Cursor::new(b"new-secret-value\n");
    super::edit::run_env_set_with_reader(
        super::edit::EnvSetArgs {
            global_db_path: &db_path,
            cwd: &project,
            key: "PRIVATE_KEY",
            secret: None,
            file: Some(std::path::Path::new(".env.credentials")),
            secret_type: None,
            description: None,
            value_stdin: true,
            replace: false,
            stdin_password: false,
            keychain: false,
            password_file: Some(&pw_file),
            insecure_password_file: false,
        },
        &mut input,
    )
    .expect("env set");

    let after = std::fs::read_to_string(&creds_path).expect("read creds");
    assert_eq!(
        after,
        "PRIVATE_KEY=vault:PRIVATE_KEY # rotated monthly\r\nOTHER=1"
    );
}

#[test]
fn env_seal_stores_a_multi_line_value_whole() {
    let (_temp, db_path, pw_file, project) = vault_fixture();
    let creds_path = project.join(".env.credentials");
    std::fs::write(
        &creds_path,
        "AUTH_BLOB='line one\nkey=value inside\nline three'\nOTHER_TOKEN=plain\n",
    )
    .expect("write creds");

    super::edit::run_env_seal(super::edit::EnvSealArgs {
        global_db_path: &db_path,
        cwd: &project,
        file: None,
        keys: &[],
        dry_run: false,
        replace: false,
        stdin_password: false,
        keychain: false,
        password_file: Some(&pw_file),
        insecure_password_file: false,
    })
    .expect("seal");

    let sealed = std::fs::read_to_string(&creds_path).expect("read sealed");
    assert_eq!(
        sealed,
        "AUTH_BLOB=vault:AUTH_BLOB\nOTHER_TOKEN=vault:OTHER_TOKEN\n"
    );
}
