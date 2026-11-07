use super::*;
use crate::MemoryServer;
use rmcp::transport::auth::{
    AuthorizationManager, AuthorizationMetadata, OAuthHttpClient, OAuthHttpClientFuture,
    OAuthHttpRequest,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const RESOURCE: &str = "https://mcp.example.test/mcp";
const NAME: &str = "MCP_TEST_OAUTH";
const KEY: [u8; 32] = [79; 32];

fn fixture() -> (tempfile::TempDir, MemoryServer) {
    let dir = tempfile::tempdir().unwrap();
    let server = MemoryServer::new_with_home_for_test(
        dir.path().join("memory.db"),
        None,
        dir.path().to_path_buf(),
    )
    .unwrap();
    unlock(&server);
    (dir, server)
}

fn unlock(server: &MemoryServer) {
    let mut state = server.vault_write();
    state.key = Some(memory_server_runtime::CachedVaultKey::copy_from(&KEY));
    state.unlock_time = Some(Instant::now());
    state.auto_lock_after_secs = 0;
}

fn credentials() -> StoredCredentials {
    serde_json::from_value(json!({
        "client_id": "public-client", "granted_scopes": ["read"],
        "token_received_at": 1, "issuer": "https://issuer.example.test",
        "token_response": {
            "access_token": "access-secret-fixture", "refresh_token": "refresh-secret-fixture",
            "token_type": "Bearer", "expires_in": 3600
        }
    }))
    .unwrap()
}

fn store(server: &MemoryServer) -> OAuthCredentialStore<MemoryServer> {
    OAuthCredentialStore::new(server.clone(), NAME.into(), RESOURCE, None).unwrap()
}

#[tokio::test]
async fn oauth_credentials_are_encrypted_and_reloaded_after_restart() {
    let (dir, server) = fixture();
    let vault = store(&server);
    assert!(vault.load().await.unwrap().is_none());
    vault.save(credentials()).await.unwrap();
    let entry = server
        .with_global_store_read(|store| {
            store
                .vault_get_entry(NAME)
                .map_err(|error| error.to_string())
        })
        .unwrap()
        .unwrap();
    assert_eq!(entry.secret_type, "oauth_token");
    let raw = serde_json::to_string(&entry).unwrap();
    assert!(!raw.contains("access-secret-fixture"));
    assert!(!raw.contains("refresh-secret-fixture"));
    let encrypted: Value = serde_json::from_slice(
        &crypto::decrypt(&KEY, &entry.encrypted_value, &entry.nonce).unwrap(),
    )
    .unwrap();
    assert_eq!(encrypted["resource"], RESOURCE);
    assert_eq!(
        encrypted["credentials"]["token_response"]["refresh_token"],
        "refresh-secret-fixture"
    );
    drop(vault);
    drop(server);
    let restarted = MemoryServer::new_with_home_for_test(
        dir.path().join("memory.db"),
        None,
        dir.path().to_path_buf(),
    )
    .unwrap();
    unlock(&restarted);
    let reloaded = store(&restarted).load().await.unwrap().unwrap();
    assert_eq!(
        serde_json::to_value(reloaded).unwrap(),
        serde_json::to_value(credentials()).unwrap()
    );
}

#[tokio::test]
async fn oauth_resource_binding_rejects_reuse_and_clear_for_another_endpoint() {
    let (_dir, server) = fixture();
    let vault = store(&server);
    vault.save(credentials()).await.unwrap();
    let other = OAuthCredentialStore::new(
        server.clone(),
        NAME.into(),
        "https://other.example.test/mcp",
        None,
    )
    .unwrap();
    assert!(other.load().await.unwrap().is_none());
    other.clear().await.unwrap();
    assert!(vault.load().await.unwrap().is_some());
}

#[tokio::test]
async fn oauth_store_preserves_acl_and_refuses_locked_or_wrong_type_entries() {
    let (_dir, server) = fixture();
    let vault = store(&server);
    vault.save(credentials()).await.unwrap();
    server
        .with_global_store(|store| {
            let mut entry = store.vault_get_entry(NAME).unwrap().unwrap();
            entry.allowed_agents = Some(vec!["mcp:allowed".into()]);
            store
                .vault_upsert_entry(&entry)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert!(matches!(
        vault.load().await,
        Err(AuthError::CredentialStoreError(_))
    ));
    assert!(vault.save(credentials()).await.is_err());
    assert!(vault.clear().await.is_err());
    let allowed = OAuthCredentialStore::new(
        server.clone(),
        NAME.into(),
        RESOURCE,
        Some("mcp:allowed".into()),
    )
    .unwrap();
    allowed.save(credentials()).await.unwrap();
    let entry = server
        .with_global_store_read(|store| {
            store
                .vault_get_entry(NAME)
                .map_err(|error| error.to_string())
        })
        .unwrap()
        .unwrap();
    assert_eq!(entry.allowed_agents, Some(vec!["mcp:allowed".into()]));
    server
        .with_global_store(|store| {
            let mut entry = entry;
            entry.secret_type = "api_key".into();
            store
                .vault_upsert_entry(&entry)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert!(allowed.load().await.is_err());
    assert!(allowed.save(credentials()).await.is_err());
    assert!(allowed.clear().await.is_err());
    {
        let mut state = server.vault_write();
        state.key = None;
        state.unlock_time = None;
    }
    assert!(matches!(
        allowed.load().await,
        Err(AuthError::CredentialStoreError(_))
    ));
}

#[tokio::test]
async fn oauth_refresh_lock_is_shared_by_independent_store_instances() {
    let (_dir, server) = fixture();
    let vault = store(&server);
    let guard = vault.acquire_refresh_guard().await.unwrap().unwrap();
    let mut path = server
        .global_db_path_buf()
        .canonicalize()
        .unwrap()
        .into_os_string();
    path.push(format!(".oauth-{NAME}.lock"));
    let second = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    assert!(matches!(
        second.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(guard);
    second.try_lock().unwrap();
}

#[derive(Default)]
struct RefreshHttp {
    requests: Mutex<Vec<String>>,
}

impl OAuthHttpClient for RefreshHttp {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push(String::from_utf8(request.request.body().clone()).unwrap());
            Ok(http::Response::builder().status(200).header("content-type", "application/json")
                .body(json!({"access_token":"refreshed-access", "refresh_token":"rotated-refresh", "token_type":"Bearer", "expires_in":3600}).to_string().into_bytes()).unwrap())
        })
    }
}

#[tokio::test]
async fn sdk_restores_and_persists_rotating_refresh_tokens_in_vault() {
    let (_dir, server) = fixture();
    let vault = store(&server);
    vault.save(credentials()).await.unwrap();
    let http = Arc::new(RefreshHttp::default());
    let mut manager = AuthorizationManager::new_with_oauth_http_client(RESOURCE, http.clone())
        .await
        .unwrap();
    let mut metadata = AuthorizationMetadata::default();
    metadata.authorization_endpoint = "https://issuer.example.test/authorize".into();
    metadata.token_endpoint = "https://issuer.example.test/token".into();
    metadata.issuer = Some("https://issuer.example.test".into());
    manager.set_metadata(metadata);
    manager.set_credential_store(vault.clone());
    assert!(manager.initialize_from_store().await.unwrap());
    assert_eq!(
        manager.get_access_token().await.unwrap(),
        "refreshed-access"
    );
    let saved = serde_json::to_value(vault.load().await.unwrap().unwrap()).unwrap();
    assert_eq!(saved["token_response"]["refresh_token"], "rotated-refresh");
    assert_eq!(saved["granted_scopes"], json!(["read"]));
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("refresh_token=refresh-secret-fixture"));
    assert!(requests[0].contains("grant_type=refresh_token"));
}

#[tokio::test]
async fn oauth_login_publication_is_atomic_with_credential_write_and_preserves_governance() {
    let (_dir, server) = fixture();
    let vault = store(&server);
    vault.save(credentials()).await.unwrap();
    let mut cap = crate::tests::make_mcp_capability("mcp:atomic-login", 1);
    cap.enabled = false;
    cap.review_status = "pending".into();
    cap.uses = 7;
    server
        .with_global_store(|store| store.hub_register(&cap).map_err(|error| error.to_string()))
        .unwrap();
    let original = server
        .with_global_store_read(|store| store.hub_get(&cap.id).map_err(|error| error.to_string()))
        .unwrap()
        .unwrap();
    server
        .with_global_store(|store| {
            let mut entry = store.vault_get_entry(NAME).unwrap().unwrap();
            entry.secret_type = "api_key".into();
            store
                .vault_upsert_entry(&entry)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let publish = |transaction: &memcore::store::vault::VaultTransaction<'_>| {
        let current = transaction
            .hub_get(&cap.id)
            .map_err(|error| error.to_string())?
            .unwrap();
        assert!(transaction
            .hub_replace_definition(
                &current,
                r#"{"auth":{"type":"oauth","vault_key":"MCP_TEST_OAUTH"}}"#
            )
            .map_err(|error| error.to_string())?);
        Ok(())
    };
    assert!(vault.save_with(credentials(), publish).is_err());
    let after_failure = server
        .with_global_store_read(|store| store.hub_get(&cap.id).map_err(|error| error.to_string()))
        .unwrap()
        .unwrap();
    assert_eq!(
        after_failure.definition, original.definition,
        "credential failure must roll back the already-written definition"
    );
    server
        .with_global_store(|store| {
            let mut entry = store.vault_get_entry(NAME).unwrap().unwrap();
            entry.secret_type = "oauth_token".into();
            store
                .vault_upsert_entry(&entry)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    vault.save_with(credentials(), publish).unwrap();
    let saved = server
        .with_global_store_read(|store| store.hub_get(&cap.id).map_err(|error| error.to_string()))
        .unwrap()
        .unwrap();
    assert!(saved.definition.contains("MCP_TEST_OAUTH"));
    assert!(!saved.enabled);
    assert_eq!(saved.review_status, "pending");
    assert_eq!(saved.uses, 7);
    assert_eq!(saved.created_at, original.created_at);
    assert!(vault.load().await.unwrap().is_some());
}
