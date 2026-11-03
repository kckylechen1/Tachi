use super::*;
use crate::vault_ops::connections::{current_node, Connection};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use std::cell::Cell;

fn api_connection(env_name: &str, secret_name: &str) -> Connection {
    Connection {
        name: "service".to_string(),
        purpose: "Synthetic service fixture".to_string(),
        enabled: true,
        allowed_agents: Vec::new(),
        allowed_projects: Vec::new(),
        executor: None,
        target: ConnectionTarget::Api {
            env: BTreeMap::from([(env_name.to_string(), format!("vault:{secret_name}"))]),
            endpoint: Some("https://example.com/v1".to_string()),
            model: None,
        },
    }
}

fn run_action(cwd: &Path, password_file: &Path) -> ConnectionAction {
    ConnectionAction::Run {
        name: "service".to_string(),
        expect_revision: None,
        cwd: Some(cwd.to_path_buf()),
        stdin_password: false,
        keychain: false,
        password_file: Some(password_file.to_path_buf()),
        insecure_password_file: false,
        command: vec!["true".to_string()],
    }
}

struct VaultFixture {
    dir: tempfile::TempDir,
    db: PathBuf,
    password_file: PathBuf,
    key: crate::vault_crypto::DerivedVaultKey,
}

impl VaultFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp fixture");
        let db = dir.path().join("memory.db");
        let password_file = dir.path().join("password");
        let password = "synthetic-test-password";
        let salt = b"1234567890123456";
        // The CLI unlock path checks the production stored-KDF allowlist.
        // Keep the fixture key and stored profile on that same supported path.
        let key = crate::vault_crypto::DerivedVaultKey::derive(password, salt)
            .expect("derive fixture key");
        let store = open_cli_store(&db).expect("open fixture DB");
        store
            .vault_set_config(&memcore::vault::VaultConfig {
                salt: B64.encode(salt),
                verifier: crate::vault_crypto::create_verifier(key.bytes()).expect("verifier"),
                kdf_algorithm: "argon2id".to_string(),
                kdf_params: crate::vault_crypto::active_kdf_params_json().to_string(),
                cipher: memcore::vault::VaultCipher::Aes256Gcm,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
            })
            .expect("configure fixture Vault");
        std::fs::write(&password_file, password).expect("write synthetic password");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600))
                .expect("owner-only password fixture");
        }
        Self {
            dir,
            db,
            password_file,
            key,
        }
    }

    fn put_secret(&self, name: &str, allowed_agents: Option<Vec<String>>) {
        self.put_secret_value(name, "synthetic-api-value", allowed_agents);
    }

    fn put_secret_value(&self, name: &str, value: &str, allowed_agents: Option<Vec<String>>) {
        let (encrypted_value, nonce) =
            crate::vault_crypto::encrypt(self.key.bytes(), value.as_bytes())
                .expect("encrypt fixture value");
        open_cli_store(&self.db)
            .expect("open fixture DB")
            .vault_upsert_entry(&memcore::vault::VaultEntry {
                name: name.to_string(),
                encrypted_value,
                nonce,
                secret_type: "api_key".to_string(),
                description: "Synthetic fixture".to_string(),
                allowed_agents,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                accessed_at: String::new(),
                access_count: 0,
            })
            .expect("insert fixture secret");
    }
}

#[test]
#[cfg(unix)]
fn connection_api_run_injects_declared_refs_into_child_cwd_without_patching_configs() {
    let fixture = VaultFixture::new();
    fixture.put_secret("EXA_KEY", None);
    fixture.put_secret("UNRELATED", None);
    let store = open_cli_store(&fixture.db).expect("open fixture DB");
    let mut unrelated = store.vault_get_entry("UNRELATED").unwrap().unwrap();
    unrelated.encrypted_value = "invalid-ciphertext".to_string();
    store
        .vault_upsert_entry(&unrelated)
        .expect("corrupt unrelated secret");
    drop(store);

    let cwd = fixture.dir.path().join("project");
    std::fs::create_dir_all(cwd.join(".tachi")).expect("create project fixture");
    let config = "{\"provider\":{\"keep\":true}}\n";
    let project_bindings = "IGNORED=vault:UNRELATED\n";
    std::fs::write(cwd.join("opencode.json"), config).unwrap();
    std::fs::write(cwd.join(".tachi/vault.env"), project_bindings).unwrap();
    put_connection(
        &fixture.db,
        api_connection("TACHI_TEST_CONNECTION_API_KEY", "EXA_KEY"),
        false,
    )
    .expect("register API connection");
    let _inherited =
        crate::test_support::EnvRestore::set("TACHI_TEST_CONNECTION_API_KEY", "inherited-value");
    let mut action = run_action(&cwd, &fixture.password_file);
    if let ConnectionAction::Run { command, .. } = &mut action {
        *command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s' \"$TACHI_TEST_CONNECTION_API_KEY\" > child-result".to_string(),
        ];
    }
    run_connection_action_with_runner(&fixture.db, action, |actual_cwd, command, exports| {
        assert_eq!(actual_cwd, cwd.canonicalize().unwrap());
        assert_eq!(
            exports,
            &[(
                "TACHI_TEST_CONNECTION_API_KEY".to_string(),
                "synthetic-api-value".to_string()
            )]
        );
        crate::bootstrap::env_cmd::run_with_project_env(actual_cwd, command, exports)
    })
    .expect("execute child with requested Vault binding");

    assert_eq!(
        std::fs::read_to_string(cwd.join("child-result")).unwrap(),
        "synthetic-api-value"
    );
    assert_eq!(
        std::env::var("TACHI_TEST_CONNECTION_API_KEY").unwrap(),
        "inherited-value"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("opencode.json")).unwrap(),
        config
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join(".tachi/vault.env")).unwrap(),
        project_bindings
    );
    let store = open_cli_store(&fixture.db).unwrap();
    assert_eq!(
        store
            .vault_get_entry("EXA_KEY")
            .unwrap()
            .unwrap()
            .access_count,
        1
    );
    assert_eq!(
        store
            .vault_get_entry("UNRELATED")
            .unwrap()
            .unwrap()
            .access_count,
        0
    );
}

#[test]
fn connection_api_missing_key_does_not_fall_back_to_parent_environment() {
    let fixture = VaultFixture::new();
    put_connection(
        &fixture.db,
        api_connection("TACHI_TEST_CONNECTION_FALLBACK", "MISSING"),
        false,
    )
    .unwrap();
    let _inherited = crate::test_support::EnvRestore::set(
        "TACHI_TEST_CONNECTION_FALLBACK",
        "parent-value-must-not-mask-missing-vault-key",
    );
    let called = Cell::new(false);
    let error = run_connection_action_with_runner(
        &fixture.db,
        run_action(fixture.dir.path(), &fixture.password_file),
        |_, _, _| {
            called.set(true);
            Ok(())
        },
    )
    .expect_err("missing Vault key must stop execution")
    .to_string();
    assert_eq!(error, "Secret not found: MISSING");
    assert!(!called.get());
}

#[test]
fn connection_api_run_observes_secret_updates_without_changing_registration() {
    let fixture = VaultFixture::new();
    fixture.put_secret("EXA_KEY", None);
    let catalog = put_connection(&fixture.db, api_connection("API_KEY", "EXA_KEY"), false)
        .expect("register stable Vault reference");
    let catalog_bytes = std::fs::read(catalog_path(&fixture.db)).unwrap();
    let mut action = run_action(fixture.dir.path(), &fixture.password_file);
    if let ConnectionAction::Run {
        expect_revision, ..
    } = &mut action
    {
        *expect_revision = Some(catalog.revision);
    }
    run_connection_action_with_runner(&fixture.db, action.clone(), |_, _, exports| {
        assert_eq!(
            exports,
            &[("API_KEY".to_string(), "synthetic-api-value".to_string())]
        );
        Ok(())
    })
    .expect("first run reads the initial secret");

    fixture.put_secret_value("EXA_KEY", "updated-synthetic-api-value", None);
    run_connection_action_with_runner(&fixture.db, action, |_, _, exports| {
        assert_eq!(
            exports,
            &[(
                "API_KEY".to_string(),
                "updated-synthetic-api-value".to_string()
            )]
        );
        Ok(())
    })
    .expect("next run reads the updated secret through the same reference");
    assert_eq!(
        load_catalog(&fixture.db).unwrap().revision,
        catalog.revision
    );
    assert_eq!(
        std::fs::read(catalog_path(&fixture.db)).unwrap(),
        catalog_bytes
    );
}

#[test]
fn connection_api_empty_or_whitespace_key_is_rejected_before_runner() {
    let fixture = VaultFixture::new();
    put_connection(&fixture.db, api_connection("API_KEY", "EMPTY_KEY"), false).unwrap();
    for value in ["", " \t\r\n"] {
        fixture.put_secret_value("EMPTY_KEY", value, None);
        let called = Cell::new(false);
        let error = run_connection_action_with_runner(
            &fixture.db,
            run_action(fixture.dir.path(), &fixture.password_file),
            |_, _, _| {
                called.set(true);
                Ok(())
            },
        )
        .expect_err("an empty Vault value must not reach the child")
        .to_string();
        assert_eq!(error, "Vault secret EMPTY_KEY is empty");
        assert!(!called.get());
    }
}

#[tokio::test]
async fn connection_worktree_context_and_prepare_agree_while_runner_keeps_worktree_cwd() {
    use crate::vault_ops::connections::{context_for_cli, ConnectionQuery};
    use rmcp::handler::server::wrapper::Parameters;

    let (server, project_db) = crate::tests::make_server_with_project_fixture("project");
    let primary = project_db.parent().unwrap().parent().unwrap();
    let worktree_metadata = primary.join(".git/worktrees/linked");
    std::fs::create_dir_all(&worktree_metadata).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let linked = temp.path().join("linked");
    std::fs::create_dir(&linked).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", worktree_metadata.display()),
    )
    .unwrap();
    std::fs::write(worktree_metadata.join("commondir"), "../..").unwrap();
    let primary_context = context_for_cli(primary);
    let linked_context = context_for_cli(&linked);
    assert!(primary_context.project.is_some());
    assert_eq!(linked_context.project, primary_context.project);
    let catalog = put_connection(
        &server.global_db_path_buf(),
        Connection {
            name: "service".into(),
            purpose: "Synthetic linked-worktree connection".into(),
            enabled: true,
            allowed_agents: Vec::new(),
            allowed_projects: vec![primary_context.project.clone().unwrap()],
            executor: None,
            target: ConnectionTarget::Ssh {
                alias: "test-host".into(),
                remote_cwd: None,
            },
        },
        false,
    )
    .unwrap();
    server.set_session_identity(
        None,
        Some("project".into()),
        Some(tachi_hub::ToolProfile::standard()),
    );
    let params: ConnectionQuery =
        serde_json::from_value(serde_json::json!({"action":"prepare","name":"service"})).unwrap();
    let prepared: serde_json::Value =
        serde_json::from_str(&server.vault_connections(Parameters(params)).await.unwrap()).unwrap();
    assert_eq!(
        prepared["current_project"],
        primary_context.project.unwrap()
    );
    assert!(prepared.get("launch").is_some());
    let mut action = run_action(&linked, &temp.path().join("absent-password"));
    if let ConnectionAction::Run {
        expect_revision, ..
    } = &mut action
    {
        *expect_revision = Some(catalog.revision);
    }
    let called = Cell::new(false);
    run_connection_action_with_runner(
        &server.global_db_path_buf(),
        action,
        |cwd, command, exports| {
            called.set(true);
            assert_eq!(cwd, linked.canonicalize().unwrap());
            assert_eq!(command[0], "ssh");
            assert!(exports.is_empty());
            Ok(())
        },
    )
    .unwrap();
    assert!(called.get());
}

#[test]
fn connection_api_direct_cli_cannot_read_agent_restricted_key() {
    let fixture = VaultFixture::new();
    fixture.put_secret("RESTRICTED", Some(vec!["agent-a".to_string()]));
    put_connection(&fixture.db, api_connection("API_KEY", "RESTRICTED"), false).unwrap();
    let called = Cell::new(false);
    let error = run_connection_action_with_runner(
        &fixture.db,
        run_action(fixture.dir.path(), &fixture.password_file),
        |_, _, _| {
            called.set(true);
            Ok(())
        },
    )
    .expect_err("CLI has no bound Agent identity")
    .to_string();
    assert!(error.contains("agent_id is required"), "{error}");
    assert!(!called.get());
    let store = open_cli_store(&fixture.db).unwrap();
    assert_eq!(
        store
            .vault_get_entry("RESTRICTED")
            .unwrap()
            .unwrap()
            .access_count,
        0
    );
}

#[test]
fn connection_run_preflight_refuses_stale_disabled_other_node_and_agent_scope_before_unlock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("uninitialized.db");
    for (case, expected) in [
        ("stale", "Connection catalog changed"),
        ("disabled", "Connection is disabled"),
        ("node", "different execution node"),
        ("agent", "unavailable to this consumer or project"),
    ] {
        let mut connection = api_connection("API_KEY", "MISSING");
        match case {
            "disabled" => connection.enabled = false,
            "node" => {
                connection.executor = Some(
                    if current_node() == "test-node-a" {
                        "test-node-b"
                    } else {
                        "test-node-a"
                    }
                    .to_string(),
                )
            }
            "agent" => connection.allowed_agents = vec!["agent-a".to_string()],
            _ => {}
        }
        let catalog = put_connection(&db, connection, true).unwrap();
        let mut action = run_action(dir.path(), &dir.path().join("missing-password-file"));
        if case == "stale" {
            if let ConnectionAction::Run {
                expect_revision, ..
            } = &mut action
            {
                *expect_revision = Some(catalog.revision - 1);
            }
        }
        let error = run_connection_action_with_runner(&db, action, |_, _, _| {
            panic!("preflight failure must not reach execution")
        })
        .expect_err("preflight must fail")
        .to_string();
        assert!(error.contains(expected), "{case}: {error}");
        assert!(!db.exists(), "preflight must not initialize a Vault DB");
    }
}

#[test]
fn connection_ssh_builds_quoted_argv_without_unlocking_or_inspecting_host_config() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("uninitialized.db");
    let mut connection = api_connection("API_KEY", "UNUSED");
    connection.target = ConnectionTarget::Ssh {
        alias: "dgx".to_string(),
        remote_cwd: Some("/srv/project with spaces".to_string()),
    };
    put_connection(&db, connection, false).unwrap();
    let mut action = run_action(dir.path(), &dir.path().join("missing-password-file"));
    if let ConnectionAction::Run { command, .. } = &mut action {
        *command = vec!["printf".to_string(), "%s".to_string(), "x y".to_string()];
    }
    let called = Cell::new(false);
    run_connection_action_with_runner(&db, action, |cwd, command, exports| {
        called.set(true);
        assert_eq!(cwd, dir.path().canonicalize().unwrap());
        assert!(exports.is_empty());
        assert_eq!(
            command,
            &[
                "ssh",
                "--",
                "dgx",
                "cd '/srv/project with spaces' && exec 'printf' '%s' 'x y'",
            ]
        );
        Ok(())
    })
    .expect("SSH must not require a Vault database or password");
    assert!(called.get());
    assert!(!db.exists());
}
