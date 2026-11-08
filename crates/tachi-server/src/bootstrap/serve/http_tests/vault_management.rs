use super::*;

fn daemon(global: Option<&Path>, project: Option<&Path>) -> crate::cli_client::DaemonInfo {
    crate::cli_client::DaemonInfo {
        url: "http://127.0.0.1:6919/mcp".into(),
        global_db: global.map(|path| path.display().to_string()),
        project_db: project.map(|path| path.display().to_string()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        pid: Some(std::process::id() as i64),
        internal_proxy_token: None,
    }
}

#[cfg(unix)]
#[test]
fn vault_cli_operator_actions_use_owner_channel_through_real_daemon() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().expect("tempdir");
    let tachi_home = temp.path().join("home");
    let global = tachi_home.join("global/memory.db");
    let password_file = temp.path().join("vault-password");
    let password = "correct horse battery staple";
    std::fs::create_dir_all(global.parent().expect("global parent")).expect("global parent");
    crate::utils::write_owner_only_file_atomic(&password_file, password.as_bytes())
        .expect("write owner-only password fixture");
    let key = crate::bootstrap::vault_cli::vault_init_with_password(&global, password.to_string())
        .expect("initialize vault fixture");
    crate::bootstrap::vault_cli::vault_upsert_secret_with_key(
        &global,
        &key,
        "FIXTURE_API_KEY",
        memcore::SECRET_TYPE_API_KEY,
        "synthetic operator CLI fixture",
        "synthetic-value-never-a-real-credential".into(),
    )
    .expect("seed synthetic API key");
    for index in 1..=2 {
        crate::bootstrap::vault_cli::vault_upsert_secret_with_key(
            &global,
            &key,
            &format!("FIXTURE_POOL_API_KEY_{index}"),
            memcore::SECRET_TYPE_API_KEY,
            "synthetic pool fixture",
            format!("synthetic-pool-value-{index}"),
        )
        .expect("seed existing pool members");
    }
    let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
    let _sigil_home = EnvRestore::remove("SIGIL_HOME");
    let _app_home = EnvRestore::remove("TACHI_APP_HOME");

    let rt = test_runtime();
    let (ct, daemon_task) = rt.block_on(async {
        let server = crate::MemoryServer::new(global.clone(), None).expect("daemon server");
        let (daemon, ct, daemon_task) = spawn_test_http_daemon(server, &global).await;
        let port = daemon
            .url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.split('/').next())
            .and_then(|value| value.parse::<u16>().ok())
            .expect("test daemon URL port");
        let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&tachi_home, &global);
        crate::utils::write_json_file_owner_only(
            &pid_path,
            &serde_json::json!({
                "pid": std::process::id(),
                "port": port,
                "url": daemon.url,
                "global_db": global.display().to_string(),
                "version": env!("CARGO_PKG_VERSION"),
                "internal_proxy_token": daemon.internal_proxy_token,
            }),
        )
        .expect("write daemon discovery receipt");

        for action in [
            tachi_bootstrap::cli::VaultAction::Status,
            tachi_bootstrap::cli::VaultAction::Lock,
            tachi_bootstrap::cli::VaultAction::Unlock {
                stdin_password: false,
                keychain: false,
                password_file: Some(password_file),
                insecure_password_file: false,
            },
        ] {
            crate::bootstrap::vault_cli::run_vault_command(&global, &tachi_home, action)
                .await
                .expect("explicit Vault CLI action must use the authenticated owner channel");
        }
        crate::bootstrap::vault_cli::run_vault_command(
            &global,
            &tachi_home,
            tachi_bootstrap::cli::VaultAction::SetupRotation {
                prefix: "FIXTURE_POOL_API_KEY".into(),
                total_keys: 2,
                strategy: "least_recently_used".into(),
            },
        )
        .await
        .expect("configure an existing pool through owner channel");
        let store = crate::bootstrap::open_cli_store_read_only(&global).unwrap();
        let pool = store
            .vault_get_rotation("FIXTURE_POOL_API_KEY")
            .unwrap()
            .unwrap();
        assert_eq!(pool.rotation_strategy, "least_recently_used");
        assert_eq!(pool.total_keys, 2);
        let connection = rusqlite::Connection::open(&global).expect("fixture DB");
        let access_count = || {
            connection
                .query_row(
                    "SELECT access_count FROM vault_entries WHERE name='FIXTURE_API_KEY'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("lease access count")
        };
        // Unlock may warm provider caches; measure the lease independently of it.
        let before_lease = access_count();
        let lease_args =
            serde_json::Map::from_iter([("name".into(), serde_json::json!("FIXTURE_API_KEY"))]);
        for profile in [None, Some(tachi_hub::ToolProfile::admin())] {
            let denied = crate::cli_client::call_daemon_tool_with_access(
                &daemon,
                "vault_lease_api_key",
                lease_args.clone(),
                None,
                if profile.is_some() {
                    crate::cli_client::DaemonToolAccess::Owner
                } else {
                    crate::cli_client::DaemonToolAccess::Agent(None)
                },
            )
            .await
            .expect_err("no MCP profile may expose raw Vault leasing");
            assert!(denied.to_string().contains("tool not found"), "{denied}");
        }
        let (_, _, denied) = http_mcp_initialize(
            &daemon.url,
            http_headers(&[(crate::session_identity::HEADER_PROFILE, "admin")]),
            None,
        )
        .await;
        assert_eq!(
            denied["error"]["code"], -32602,
            "Admin requires daemon capability: {denied}"
        );
        crate::bootstrap::vault_cli::run_vault_command(
            &global,
            &tachi_home,
            tachi_bootstrap::cli::VaultAction::List {
                stdin_password: false,
                keychain: false,
                password_file: None,
                insecure_password_file: false,
            },
        )
        .await
        .expect("metadata list remains available through the owner channel");
        assert_eq!(
            access_count(),
            before_lease,
            "denied requests and metadata-only List must not read the synthetic key"
        );
        crate::bootstrap::vault_cli::run_vault_command(
            &global,
            &tachi_home,
            tachi_bootstrap::cli::VaultAction::Lease {
                name: "FIXTURE_API_KEY".into(),
                env_name: None,
                stdin_password: false,
                keychain: false,
                password_file: None,
                insecure_password_file: false,
                json: true,
            },
        )
        .await
        .expect("explicit operator lease must reach authorized daemon");
        assert_eq!(
            access_count(),
            before_lease + 1,
            "exactly the explicitly authorized lease must read the synthetic key once"
        );
        (ct, daemon_task)
    });
    ct.cancel();
    rt.block_on(daemon_task).expect("daemon task");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_cli_list_uses_authorized_daemon_health_without_metadata_fallback() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().expect("temp");
    let home = temp.path().join("home");
    let global = home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    let _home = EnvRestore::set_path("TACHI_HOME", &home);
    let _sigil = EnvRestore::remove("SIGIL_HOME");
    let _app = EnvRestore::remove("TACHI_APP_HOME");
    let _alias = EnvRestore::set("EXTRACT_API_KEY", "vault:DEEPSEEK_API_KEY");
    let server = crate::MemoryServer::new(global.clone(), None).expect("server");
    crate::vault_test_api::seed_vault(&server, "synthetic-health-board-password").unwrap();
    crate::vault_test_api::seed_vault_secret(
        &server,
        crate::vault_test_api::VaultSecretFixture {
            name: "DEEPSEEK_API_KEY".into(),
            value: "synthetic-health-board-secret".into(),
            secret_type: "api_key".into(),
            description: "health fixture".into(),
            allowed_agents: None,
        },
    )
    .unwrap();
    crate::vault_test_api::seed_vault_secret(
        &server,
        crate::vault_test_api::VaultSecretFixture {
            name: "OWNER_HEALTH_API_KEY".into(),
            value: "synthetic-owner-health-secret".into(),
            secret_type: "api_key".into(),
            description: "owner health fixture".into(),
            allowed_agents: None,
        },
    )
    .unwrap();
    crate::provider_config::materialize_for_server(&server).expect("materialize");
    let generation = server.llm.provider_health_board_snapshot().2.unwrap();
    let health = memcore::vault::health::record_key_outcome_for_generation(
        None,
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_API_KEY",
        memcore::vault::health::TypedOutcome::Exhausted,
        memcore::vault::health::EvidenceKind::Probed,
        None,
        chrono::Utc::now(),
        Some(generation),
    )
    .health;
    server
        .with_global_store(|store| {
            store
                .vault_upsert_key_health(&health)
                .map_err(|error| error.to_string())
        })
        .unwrap();

    let observations = ToolResponseObservations::default();
    let (daemon, cancel, task) = spawn_test_http_daemon_with_observers(
        server.clone(),
        &global,
        None,
        Some(observations.clone()),
    )
    .await;
    let port = reqwest::Url::parse(&daemon.url).unwrap().port().unwrap();
    let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&home, &global);
    crate::utils::write_json_file_owner_only(&pid_path, &serde_json::json!({
        "pid": std::process::id(), "port": port, "url": daemon.url, "global_db": global,
        "version": env!("CARGO_PKG_VERSION"), "internal_proxy_token": daemon.internal_proxy_token,
    })).unwrap();
    crate::bootstrap::vault_cli::run_vault_command(
        &global,
        &home,
        tachi_bootstrap::cli::VaultAction::RecordKeyResult {
            logical_name: "OWNER_HEALTH_API_KEY".into(),
            key_id: "OWNER_HEALTH_API_KEY".into(),
            status_code: Some(402),
            outcome: Some("exhausted".into()),
            retry_after_secs: None,
            reason: Some("synthetic quota fixture".into()),
            json: true,
        },
    )
    .await
    .expect("owner result recording must update the running daemon");
    assert_eq!(
        server.llm.provider_health_memory_snapshot()["OWNER_HEALTH_API_KEY"]
            ["OWNER_HEALTH_API_KEY"]
            .status,
        "exhausted"
    );
    for profile in [None, Some(tachi_hub::ToolProfile::admin())] {
        let denied = crate::cli_client::call_daemon_tool_with_access(
            &daemon,
            "vault_list",
            serde_json::Map::new(),
            None,
            if profile.is_some() {
                crate::cli_client::DaemonToolAccess::Owner
            } else {
                crate::cli_client::DaemonToolAccess::Agent(None)
            },
        )
        .await
        .expect_err("raw Vault listing is retired from all MCP profiles");
        assert!(denied.to_string().contains("tool not found"));
    }
    observations.lock().unwrap().clear();
    let result = crate::bootstrap::vault_cli::run_vault_command(
        &global,
        &home,
        tachi_bootstrap::cli::VaultAction::List {
            stdin_password: false,
            keychain: false,
            password_file: None,
            insecure_password_file: false,
        },
    )
    .await;
    // A matching daemon that rejects dispatch must stay an error. Falling
    // back here would turn its denied observation into a successful local list.
    crate::utils::write_json_file_owner_only(&pid_path, &serde_json::json!({
        "pid": std::process::id(), "port": port, "url": daemon.url, "global_db": global,
        "version": env!("CARGO_PKG_VERSION"), "internal_proxy_token": uuid::Uuid::new_v4().simple().to_string(),
    })).unwrap();
    let denied = crate::bootstrap::vault_cli::run_vault_command(
        &global,
        &home,
        tachi_bootstrap::cli::VaultAction::List {
            stdin_password: false,
            keychain: false,
            password_file: None,
            insecure_password_file: false,
        },
    )
    .await;
    cancel.cancel();
    task.await.expect("daemon terminal");
    result.expect("actual operator List");
    assert!(
        denied.is_err(),
        "matching daemon denial must not become metadata fallback success"
    );
    let rows = observations.lock().unwrap();
    assert_eq!(rows.len(), 2, "successful List plus denied owner request");
    assert_eq!(rows[0].0, "owner:list");
    assert!(rows[1].2.contains("authentication required"));
    assert_eq!(
        rows[0].1.as_deref(),
        None,
        "List must reach the owner health handler without an MCP profile, response={}",
        rows[0].2
    );
    assert!(
        rows[0].2.contains("402"),
        "real daemon response must carry observed health"
    );
    assert!(!rows[0].2.contains("synthetic-health-board-secret"));
    assert!(!rows[0].2.contains("synthetic-owner-health-secret"));
}

#[tokio::test]
async fn vault_owner_channel_rejects_missing_capability_and_request_overrides() {
    let fixture = tempfile::tempdir().unwrap();
    let global = fixture.path().join("memory.db");
    let server = crate::MemoryServer::new(global.clone(), None).unwrap();
    {
        let mut state = server.vault_write();
        state.key = Some(crate::CachedVaultKey::copy_from(&[41; 32]));
        state.unlock_time = Some(std::time::Instant::now());
    }
    let (daemon, cancel, task) = spawn_test_http_daemon(server.clone(), &global).await;
    let url = daemon.url.replace("/mcp", "/internal/vault");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for token in [None, Some("wrong-owner-capability")] {
        let mut request = client
            .post(&url)
            .json(&serde_json::json!({"action":"lock"}));
        if let Some(token) = token {
            request = request.header(crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN, token);
        }
        assert_eq!(
            request.send().await.unwrap().status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        assert!(
            server.vault_read().key.is_some(),
            "unauthorized lock cannot change shared state"
        );
    }
    for body in [
        serde_json::json!({"action":"get", "name":"secret"}),
        serde_json::json!({"action":"status", "agent_id":"owner"}),
        serde_json::json!({"action":"lock", "profile":"admin"}),
        serde_json::json!({"action":"list", "project":"other-project"}),
        serde_json::json!({"action":"lease", "name":"secret", "consumer":"owner"}),
        serde_json::json!({"action":"unlock_fifo", "password_fifo_path":"/unused", "password":"synthetic-only"}),
        serde_json::json!({"action":"unlock_fifo", "password":"synthetic-only"}),
        serde_json::json!({"action":"status", "tool_name":"vault_get"}),
    ] {
        let response = client
            .post(&url)
            .header(
                crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN,
                daemon.internal_proxy_token.as_deref().unwrap(),
            )
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "rejected body: {body}"
        );
        let text = response.text().await.unwrap();
        assert!(
            !text.contains("synthetic-only"),
            "parse errors cannot echo input secrets"
        );
        assert!(
            server.vault_read().key.is_some(),
            "invalid requests cannot change shared state"
        );
    }
    crate::bootstrap::vault_cli::daemon::call_daemon_vault(
        &daemon,
        &crate::bootstrap::vault_cli::daemon::VaultAdminRequest::Lock {},
    )
    .await
    .expect("owner lock");
    assert!(
        server.vault_read().key.is_none(),
        "owner endpoint uses the existing daemon state"
    );
    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn vault_owner_client_refuses_redirects_and_unsupported_daemons_without_replay() {
    use crate::bootstrap::vault_cli::daemon::{call_daemon_vault, VaultAdminRequest};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    for status in [
        axum::http::StatusCode::NOT_FOUND,
        axum::http::StatusCode::TEMPORARY_REDIRECT,
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&calls);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = axum::Router::new().fallback(move || {
            let count = Arc::clone(&count);
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                (
                    status,
                    [(axum::http::header::LOCATION, "/redirected")],
                    "unsupported owner endpoint",
                )
            }
        });
        let cancel = CancellationToken::new();
        let shutdown = cancel.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let mut info = daemon(None, None);
        info.url = format!("http://{address}/mcp");
        info.internal_proxy_token = Some("synthetic-owner-capability".into());
        let error = call_daemon_vault(&info, &VaultAdminRequest::Status {})
            .await
            .expect_err("must fail closed")
            .to_string();
        if status == axum::http::StatusCode::NOT_FOUND {
            assert!(error.contains("restart"), "{error}");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no redirect, retry, or raw MCP fallback"
        );
        cancel.cancel();
        task.await.unwrap();
    }
    let mut info = daemon(None, None);
    info.internal_proxy_token = Some("synthetic-owner-capability".into());
    for url in [
        "https://127.0.0.1:6919/mcp",
        "http://example.test:6919/mcp",
        "http://127.0.0.1:6919/mcp?override=1",
        "http://user@127.0.0.1:6919/mcp",
        "http://127.0.0.1:6919/other",
    ] {
        info.url = url.into();
        let error = call_daemon_vault(&info, &VaultAdminRequest::Status {})
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("verified loopback"), "{error}");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn vault_cli_health_ack_requires_initialization_and_current_durable_write() {
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = tempfile::tempdir().expect("fixture");
    let home = fixture.path().join("home");
    let global = home.join("global/memory.db");
    std::fs::create_dir_all(global.parent().unwrap()).unwrap();
    let _home = EnvRestore::set_path("TACHI_HOME", &home);
    let _sigil = EnvRestore::remove("SIGIL_HOME");
    let _app = EnvRestore::remove("TACHI_APP_HOME");
    let server = crate::MemoryServer::new(global.clone(), None).expect("server");
    let observations = ToolResponseObservations::default();
    let (daemon, cancel, task) = spawn_test_http_daemon_with_observers(
        server.clone(),
        &global,
        None,
        Some(observations.clone()),
    )
    .await;
    let port = reqwest::Url::parse(&daemon.url).unwrap().port().unwrap();
    crate::utils::write_json_file_owner_only(
        &crate::daemon_lock::scoped_daemon_pid_path(&home, &global),
        &serde_json::json!({
            "pid": std::process::id(), "port": port, "url": daemon.url,
            "global_db": global, "version": env!("CARGO_PKG_VERSION"),
            "internal_proxy_token": daemon.internal_proxy_token,
        }),
    )
    .unwrap();
    const KEY: &str = "OWNER_ACK_API_KEY";
    let action = || tachi_bootstrap::cli::VaultAction::RecordKeyResult {
        logical_name: KEY.into(),
        key_id: KEY.into(),
        status_code: Some(402),
        outcome: Some("exhausted".into()),
        retry_after_secs: None,
        reason: Some("synthetic quota report".into()),
        json: true,
    };
    let uninitialized = crate::bootstrap::vault_cli::run_vault_command(&global, &home, action())
        .await
        .expect_err("owner CLI must retain its initialization precondition");
    assert!(uninitialized.to_string().contains("not initialized"));
    assert!(!server
        .llm
        .provider_health_memory_snapshot()
        .contains_key(KEY));
    crate::vault_test_api::seed_vault(&server, "synthetic-health-ack-password").unwrap();

    // A separate real SQLite writer makes the current persist exceed its own
    // deadline. It does not alter the store authorizer or install fixture DDL.
    let writer_gate = rusqlite::Connection::open(&global).expect("writer gate");
    writer_gate.execute_batch("BEGIN IMMEDIATE").unwrap();
    let failed = crate::bootstrap::vault_cli::run_vault_command(&global, &home, action())
        .await
        .expect_err("a failed owner write must not become a successful local fallback");
    assert!(
        failed.to_string().contains("persist vault key health"),
        "{failed}"
    );
    server
        .llm
        .await_provider_health_persistence()
        .await
        .expect_err("join the failed observation writer before releasing the fixture lock");
    assert!(server
        .with_global_store_read(|store| {
            store
                .vault_get_key_health(KEY, KEY)
                .map_err(|error| error.to_string())
        })
        .unwrap()
        .is_none());
    writer_gate.execute_batch("ROLLBACK").unwrap();

    crate::bootstrap::vault_cli::run_vault_command(&global, &home, action())
        .await
        .expect("this write must succeed after recovery despite the historical tracker error");
    let durable = server
        .with_global_store_read(|store| {
            store
                .vault_get_key_health(KEY, KEY)
                .map_err(|error| error.to_string())
        })
        .unwrap()
        .expect("successful owner acknowledgement requires a durable row");
    assert_eq!(durable.status, "exhausted");
    assert_eq!(
        server.llm.provider_health_memory_snapshot()[KEY][KEY].status,
        "exhausted"
    );
    cancel.cancel();
    task.await.expect("daemon terminal");

    let responses = observations.lock().unwrap();
    assert_eq!(responses.len(), 3, "one owner request per actual CLI call");
    for (_, _, body) in &responses[..2] {
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(
            body.get("recorded").is_none(),
            "failed writes cannot claim success: {body}"
        );
        assert!(body["error"].is_string());
    }
    let recovered: serde_json::Value = serde_json::from_str(&responses[2].2).unwrap();
    assert_eq!(recovered["recorded"], true);
}
