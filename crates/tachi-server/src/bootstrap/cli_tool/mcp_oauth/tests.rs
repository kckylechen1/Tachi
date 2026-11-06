use super::*;
use clap::Parser;
use std::time::Instant;
use tachi_bootstrap::cli::{Cli, Commands, McpAction};

#[test]
fn login_cli_reuses_existing_password_sources_and_public_client_options() {
    let cli = Cli::try_parse_from([
        "tachi",
        "mcp",
        "login",
        "example",
        "--client-id",
        "public",
        "--scope",
        "read",
        "--scope",
        "write",
        "--callback-port",
        "8123",
        "--keychain",
    ])
    .unwrap();
    let Some(Commands::Mcp {
        action: McpAction::Login(args),
    }) = cli.command
    else {
        panic!("expected OAuth login")
    };
    assert!(args.keychain);
    assert_eq!(args.scopes, ["read", "write"]);
    assert_eq!(args.callback_port, 8123);
    assert_eq!(args.client_id.as_deref(), Some("public"));
    assert!(Cli::try_parse_from([
        "tachi",
        "mcp",
        "login",
        "example",
        "--keychain",
        "--stdin-password"
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "tachi",
        "mcp",
        "login",
        "example",
        "--client-id",
        "public",
        "--client-metadata-url",
        "https://example.test/client.json"
    ])
    .is_err());
}

#[tokio::test]
async fn callback_with_wrong_state_does_not_consume_the_pending_login() {
    let (sender, mut receiver) = oneshot::channel();
    let state = Arc::new(CallbackState {
        expected_state: "correct-state".into(),
        sender: Mutex::new(Some(sender)),
    });
    let (status, body) = callback(
        axum::extract::State(state.clone()),
        axum::extract::Query(HashMap::from([
            ("state".into(), "wrong-state".into()),
            ("code".into(), "private-code".into()),
        ])),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert!(!body.contains("private-code"));
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let (status, body) = callback(
        axum::extract::State(state),
        axum::extract::Query(HashMap::from([
            ("state".into(), "correct-state".into()),
            ("code".into(), "private-code".into()),
            ("iss".into(), "https://issuer.example.test".into()),
        ])),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(!body.contains("private-code"));
    let callback = receiver.await.unwrap().unwrap();
    assert_eq!(callback.code, "private-code");
    assert_eq!(
        callback.issuer.as_deref(),
        Some("https://issuer.example.test")
    );
}

#[test]
fn login_metadata_updates_ignore_discovery_but_detect_connection_changes() {
    let original = json!({"url":"https://first.example.test/mcp", "transport":"http", "auth":{"type":"oauth", "vault_key":"MCP_TEST_OAUTH"}});
    let mut updated = original.clone();
    updated["discovered_at"] = json!("later");
    assert!(same_connection(&original, &updated));
    updated["url"] = json!("https://second.example.test/mcp");
    assert!(!same_connection(&original, &updated));
}

#[tokio::test]
async fn managed_regression_stale_oauth_login_cannot_overwrite_new_connection_credentials() {
    let home = tempfile::tempdir().unwrap();
    let server = crate::MemoryServer::new_with_home_for_test(
        home.path().join("memory.db"),
        None,
        home.path().to_path_buf(),
    )
    .unwrap();
    {
        let mut state = server.vault_write();
        state.key = Some(memory_server_runtime::CachedVaultKey::copy_from(&[79; 32]));
        state.unlock_time = Some(Instant::now());
    }
    let config: McpOAuthConfig =
        serde_json::from_value(json!({"type":"oauth", "vault_key":"MCP_LOGIN_TEST_OAUTH"}))
            .unwrap();
    let first_url = "https://first.example.test/mcp";
    let second_url = "https://second.example.test/mcp";
    let mut original = crate::tests::make_mcp_capability("mcp:login-test", 1);
    original.definition = json!({"transport":"http", "url":first_url, "auth":config}).to_string();
    let mut current = original.clone();
    current.definition = json!({"transport":"http", "url":second_url, "auth":config}).to_string();
    server
        .with_global_store(|store| {
            store
                .hub_register(&current)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let credentials = |token: &str| {
        serde_json::from_value(json!({
        "client_id":"public-client", "granted_scopes":[], "token_received_at":1,
        "token_response":{"access_token":token, "token_type":"Bearer", "refresh_token":format!("refresh-{token}")}
    })).unwrap()
    };
    let old = OAuthCredentialStore::new(server.clone(), config.vault_key.clone(), first_url, None)
        .unwrap();
    let new = OAuthCredentialStore::new(server.clone(), config.vault_key.clone(), second_url, None)
        .unwrap();
    new.save(credentials("new-grant")).await.unwrap();
    assert!(
        publish_login(&old, &original, &config, credentials("old-grant"))
            .await
            .is_err()
    );
    let kept = new
        .load()
        .await
        .unwrap()
        .expect("stale login must leave new resource credentials intact");
    assert_eq!(
        serde_json::to_value(kept).unwrap()["token_response"]["access_token"],
        "new-grant"
    );
    let saved = server
        .with_global_store_read(|store| {
            store
                .hub_get(&current.id)
                .map_err(|error| error.to_string())
        })
        .unwrap()
        .unwrap();
    assert_eq!(saved.definition, current.definition);
}
