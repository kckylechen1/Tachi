use super::*;

fn ssh(name: &str) -> Connection {
    Connection {
        name: name.into(),
        purpose: "Synthetic test host".into(),
        enabled: true,
        allowed_agents: Vec::new(),
        allowed_projects: Vec::new(),
        executor: None,
        target: ConnectionTarget::Ssh {
            alias: "test-host".into(),
            remote_cwd: None,
        },
    }
}

#[test]
fn catalog_update_is_explicit_revisioned_and_database_scoped() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("one.db");
    let other = directory.path().join("two.db");
    assert_eq!(load_catalog(&db).unwrap().revision, 0);
    assert!(!catalog_path(&db).exists());
    assert_eq!(
        put_connection(&db, ssh("staging"), false).unwrap().revision,
        1
    );
    assert!(put_connection(&db, ssh("staging"), false).is_err());
    assert_eq!(load_catalog(&db).unwrap().revision, 1);
    assert!(load_catalog(&other).unwrap().connections.is_empty());
    assert_eq!(
        put_connection(&db, ssh("staging"), true).unwrap().revision,
        2
    );
    assert_eq!(remove_connection(&db, "staging").unwrap().revision, 3);
}

#[test]
fn concurrent_catalog_writers_do_not_lose_connections() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("global.db");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let writers: Vec<_> = (0..8)
        .map(|index| {
            let db = db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                put_connection(&db, ssh(&format!("host-{index}")), false).unwrap();
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let catalog = load_catalog(&db).unwrap();
    assert_eq!(catalog.revision, 8);
    assert_eq!(catalog.connections.len(), 8);
}

#[test]
fn definitions_reject_literals_unknown_secret_fields_and_option_hosts() {
    for definition in [
        r#"{"name":"test","purpose":"test","target":{"kind":"api","env":{"KEY":"literal-secret"}}}"#,
        r#"{"name":"test","purpose":"test","password":"secret-canary","target":{"kind":"ssh","alias":"host"}}"#,
        r#"{"name":"test","purpose":"test","target":{"kind":"ssh","alias":"-oProxyCommand=evil"}}"#,
        r#"{"name":"test","purpose":"test","target":{"kind":"api","env":{"KEY":"vault:KEY"},"endpoint":"https://host/?key=secret-canary"}}"#,
    ] {
        let error = parse_connection(definition.as_bytes()).unwrap_err();
        assert!(!error.contains("secret-canary"));
        assert!(!error.contains("literal-secret"));
    }
}

#[test]
fn scope_and_execution_require_the_actual_context() {
    let mut connection = ssh("scoped");
    connection.allowed_agents = vec!["worker".into()];
    connection.allowed_projects = vec!["project".into()];
    connection.executor = Some("node-a".into());
    let mut context = ConnectionContext {
        agent_id: Some("worker".into()),
        project: Some("project".into()),
        node: "node-a".into(),
    };
    assert!(connection.check_execution(&context).is_ok());
    context.agent_id = None;
    assert!(!connection.visible_to(&context));
    context.agent_id = Some("worker".into());
    context.project = None;
    assert!(!connection.visible_to(&context));
    context.project = Some("project".into());
    context.node = "node-b".into();
    assert!(connection.check_execution(&context).is_err());
}

#[test]
fn ssh_remote_arguments_preserve_shell_metacharacters() {
    let command = vec![
        "printf".into(),
        "%s".into(),
        "a'b $HOME; $(touch marker)\nnext".into(),
    ];
    let prepared = ssh_remote_command(&command, None).unwrap();
    #[cfg(unix)]
    {
        let output = std::process::Command::new("sh")
            .args(["-c", &prepared])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), command[2]);
    }
    assert!(ssh_remote_command(&["-c".into(), "payload".into()], None).is_err());
    assert!(ssh_remote_command(&["printf".into(), "\0".into()], None).is_err());
}

#[cfg(unix)]
#[test]
fn catalog_rejects_symlinks_and_untrusted_write_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("global.db");
    put_connection(&db, ssh("host"), false).unwrap();
    let path = catalog_path(&db);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(load_catalog(&db).is_err());
    std::fs::remove_file(&path).unwrap();
    symlink(directory.path().join("absent"), &path).unwrap();
    assert!(load_catalog(&db).is_err());
    assert!(put_connection(&db, ssh("host"), false).is_err());
}

fn api(name: &str, secret: &str) -> Connection {
    let mut connection = ssh(name);
    connection.target = ConnectionTarget::Api {
        env: BTreeMap::from([("SERVICE_KEY".into(), format!("vault:{secret}"))]),
        endpoint: None,
        model: None,
    };
    connection
}

fn query(value: serde_json::Value) -> ConnectionQuery {
    serde_json::from_value(value).unwrap()
}

fn context(agent: Option<&str>) -> ConnectionContext {
    ConnectionContext {
        agent_id: agent.map(str::to_string),
        project: None,
        node: "test-node".into(),
    }
}

fn seed_metadata(server: &crate::MemoryServer, name: &str, agents: Option<Vec<String>>) {
    server
        .with_global_store(|store| {
            store
                .vault_upsert_entry(&memcore::vault::VaultEntry {
                    name: name.into(),
                    encrypted_value: "ciphertext-canary-not-decryptable".into(),
                    nonce: "nonce-canary".into(),
                    secret_type: "api_key".into(),
                    description: "private-entry-description-canary".into(),
                    allowed_agents: agents,
                    created_at: String::new(),
                    updated_at: String::new(),
                    accessed_at: String::new(),
                    access_count: 7,
                })
                .map_err(|error| error.to_string())
        })
        .unwrap();
}

#[test]
fn discovery_intersects_record_and_secret_acl_without_decrypting_or_touching() {
    let server = crate::tests::make_server();
    let db = server.global_db_path_buf();
    seed_metadata(&server, "PUBLIC", None);
    seed_metadata(&server, "PRIVATE", Some(vec!["worker-a".into()]));
    seed_metadata(
        &server,
        "OTHER_AGENT",
        Some(vec!["different-worker".into()]),
    );
    for connection in [
        api("public", "PUBLIC"),
        api("private", "PRIVATE"),
        api("other-agent", "OTHER_AGENT"),
        api("missing", "MISSING"),
    ] {
        put_connection(&db, connection, false).unwrap();
    }
    let mut scoped = ssh("other-project");
    let root = tempfile::tempdir().unwrap();
    scoped.allowed_projects =
        vec![crate::path_utils::plan_c_dir_name_from_root(root.path()).unwrap()];
    put_connection(&db, scoped, false).unwrap();
    let response = handle_connections(
        &*server,
        query(serde_json::json!({"action":"list"})),
        context(None),
    )
    .unwrap();
    for private in [
        "ciphertext-canary",
        "nonce-canary",
        "private-entry-description",
        "PRIVATE",
        "OTHER_AGENT",
        "other-project",
    ] {
        assert!(!response.contains(private), "response leaked {private}");
    }
    let json: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(json["connections"].as_array().unwrap().len(), 2);
    assert_eq!(json["execution"], "not_started");
    for name in ["private", "other-agent", "absent"] {
        let error = handle_connections(
            &*server,
            query(serde_json::json!({"action":"describe","name":name})),
            context(None),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "Connection is unavailable to this consumer or project"
        );
    }
    let prepared: serde_json::Value = serde_json::from_str(
        &handle_connections(
            &*server,
            query(serde_json::json!({"action":"prepare","name":"private"})),
            context(Some("worker-a")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        prepared["connection"]["blockers"],
        serde_json::json!(["requires_bound_consumer"])
    );
    assert!(prepared.get("launch").is_none());
    server
        .with_global_store_read(|store| {
            assert_eq!(
                store
                    .vault_get_entry("PUBLIC")
                    .unwrap()
                    .unwrap()
                    .access_count,
                7
            );
            assert_eq!(
                store
                    .vault_get_entry("PRIVATE")
                    .unwrap()
                    .unwrap()
                    .access_count,
                7
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn prepare_pins_revision_and_reports_missing_disabled_or_other_executor() {
    let server = crate::tests::make_server();
    let db = server.global_db_path_buf();
    put_connection(&db, ssh("host"), false).unwrap();
    put_connection(&db, api("missing", "MISSING"), false).unwrap();
    let prepared: serde_json::Value = serde_json::from_str(
        &handle_connections(
            &*server,
            query(serde_json::json!({"action":"prepare","name":"host"})),
            context(None),
        )
        .unwrap(),
    )
    .unwrap();
    let prefix = prepared["launch"]["argv_prefix"].as_array().unwrap();
    assert_eq!(prefix[prefix.len() - 2], "--expect-revision");
    assert_eq!(prefix[prefix.len() - 1], "2");
    assert_eq!(prepared["connection"]["connection_check"], "not_performed");
    let missing: serde_json::Value = serde_json::from_str(
        &handle_connections(
            &*server,
            query(serde_json::json!({"action":"prepare","name":"missing"})),
            context(None),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        missing["connection"]["blockers"],
        serde_json::json!(["credential_missing"])
    );
    assert!(missing.get("launch").is_none());
    let mut connection = ssh("host");
    connection.enabled = false;
    connection.executor = Some("another-node".into());
    put_connection(&db, connection, true).unwrap();
    let blocked: serde_json::Value = serde_json::from_str(
        &handle_connections(
            &*server,
            query(serde_json::json!({"action":"prepare","name":"host"})),
            context(None),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        blocked["connection"]["blockers"],
        serde_json::json!(["disabled", "different_executor"])
    );
    assert!(blocked.get("launch").is_none());
}

#[test]
fn connection_query_rejects_caller_identity_and_execution_actions() {
    for value in [
        serde_json::json!({"action":"list","agent_id":"admin"}),
        serde_json::json!({"action":"list","project":"other"}),
        serde_json::json!({"action":"run","name":"host"}),
        serde_json::json!({"action":"prepare","name":"host","command":["id"]}),
    ] {
        assert!(serde_json::from_value::<ConnectionQuery>(value).is_err());
    }
}

#[test]
fn pool_discovery_checks_all_member_acl_and_never_advances_rotation() {
    let server = crate::tests::make_server();
    seed_metadata(&server, "POOL_1", None);
    seed_metadata(&server, "POOL_2", Some(vec!["worker-a".into()]));
    server
        .with_global_store(|store| {
            store
                .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                    prefix: "POOL".into(),
                    current_index: 1,
                    total_keys: 2,
                    rotation_strategy: "round_robin".into(),
                    created_at: String::new(),
                    updated_at: String::new(),
                })
                .map_err(|error| error.to_string())
        })
        .unwrap();
    put_connection(&server.global_db_path_buf(), api("pool", "POOL"), false).unwrap();
    let anonymous: serde_json::Value = serde_json::from_str(
        &handle_connections(
            &*server,
            query(serde_json::json!({"action":"list"})),
            context(None),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(anonymous["connections"], serde_json::json!([]));
    let bound: serde_json::Value = serde_json::from_str(
        &handle_connections(
            &*server,
            query(serde_json::json!({"action":"prepare","name":"pool"})),
            context(Some("worker-a")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(bound["connection"]["credential_status"], "configured");
    assert!(bound.get("launch").is_none());
    server
        .with_global_store(|store| {
            let mut rotation = store.vault_get_rotation("POOL").unwrap().unwrap();
            assert_eq!(rotation.current_index, 1);
            rotation.total_keys = 3;
            store
                .vault_set_rotation(&rotation)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let invalid: serde_json::Value = serde_json::from_str(
        &handle_connections(
            &*server,
            query(serde_json::json!({"action":"prepare","name":"pool"})),
            context(Some("worker-a")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        invalid["connection"]["credential_status"],
        "invalid_rotation"
    );
    assert!(invalid.get("launch").is_none());
}

#[test]
fn cli_project_scope_uses_canonical_cwd_instead_of_declared_agent_identity() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("nested")).unwrap();
    let context = context_for_cli(&root.join("nested"));
    assert_eq!(
        context.project,
        crate::path_utils::plan_c_dir_name_from_root(&root)
    );
    assert_eq!(context.agent_id, None);
    assert_eq!(context_for_cli(directory.path()).project, None);
    let other = directory.path().join("other").join("repo");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    assert_ne!(context.project, context_for_cli(&other).project);
}

#[test]
fn indirect_lane_slots_are_rejected_by_registration_and_catalog_loading() {
    let directory = tempfile::tempdir().unwrap();
    let db = directory.path().join("global.db");
    let connection = api("slot", "EXTRACT_API_KEY");
    let definition = serde_json::to_vec(&connection).unwrap();
    assert!(parse_connection(&definition)
        .unwrap_err()
        .contains("lane slots"));
    assert!(put_connection(&db, connection.clone(), false)
        .unwrap_err()
        .contains("lane slots"));
    assert!(!catalog_path(&db).exists());
    let catalog = ConnectionCatalog {
        version: CATALOG_VERSION,
        revision: 1,
        connections: vec![connection],
    };
    crate::utils::write_owner_only_file_atomic(
        &catalog_path(&db),
        &serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();
    assert!(load_catalog(&db).unwrap_err().contains("lane slots"));
}

#[test]
fn exact_pool_member_reports_invalid_parent_rotation_without_decryption() {
    let server = crate::tests::make_server();
    seed_metadata(&server, "POOL_1", None);
    server
        .with_global_store(|store| {
            store
                .vault_set_rotation(&memcore::vault::VaultKeyRotation {
                    prefix: "POOL".into(),
                    current_index: 1,
                    total_keys: 2,
                    rotation_strategy: "round_robin".into(),
                    created_at: String::new(),
                    updated_at: String::new(),
                })
                .map_err(|error| error.to_string())
        })
        .unwrap();
    put_connection(&server.global_db_path_buf(), api("member", "POOL_1"), false).unwrap();
    let prepare = || -> serde_json::Value {
        serde_json::from_str(
            &handle_connections(
                &*server,
                query(serde_json::json!({"action":"prepare","name":"member"})),
                context(None),
            )
            .unwrap(),
        )
        .unwrap()
    };
    let invalid = prepare();
    assert_eq!(
        invalid["connection"]["credential_status"],
        "invalid_rotation"
    );
    assert!(invalid.get("launch").is_none());
    server
        .with_global_store(|store| {
            let mut rotation = store.vault_get_rotation("POOL").unwrap().unwrap();
            rotation.total_keys = 1;
            store
                .vault_set_rotation(&rotation)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let valid = prepare();
    assert_eq!(valid["connection"]["credential_status"], "configured");
    assert!(valid.get("launch").is_some());
}
