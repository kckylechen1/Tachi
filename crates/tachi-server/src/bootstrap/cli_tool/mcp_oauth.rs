use crate::mcp_connection::oauth::{authorization_manager, oauth_error, McpOAuthConfig};
use crate::vault_ops::OAuthCredentialStore;
use rmcp::transport::auth::{
    AuthorizationRequest, CredentialStore, InMemoryCredentialStore, OAuthState,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tachi_bootstrap::cli::McpLoginArgs;
use tokio::sync::oneshot;

pub(super) async fn run_login(
    args: McpLoginArgs,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let name = super::normalize_mcp_name(args.name.strip_prefix("mcp:").unwrap_or(&args.name))?;
    let capability_id = format!("mcp:{name}");
    let db = memory_server_hub_cli::resolve_hub_db(None, app_home);
    let server = crate::cli_client::build_in_process_server(&db, None)?;
    let capability = server
        .with_global_store_read(|store| {
            store
                .hub_get(&capability_id)
                .map_err(|_| "Cannot read registered MCP".to_string())
        })?
        .ok_or("MCP is not registered; run `tachi mcp add NAME URL` first")?;
    let mut def: Value = serde_json::from_str(&capability.definition)
        .map_err(|_| "Invalid registered MCP definition")?;
    if !crate::mcp_connection::is_remote_http_mcp(&def) {
        return Err("OAuth login requires a registered remote HTTP MCP".into());
    }
    let existing = McpOAuthConfig::from_definition(&def)?;
    if def.get("auth").is_some_and(|auth| !auth.is_null()) && existing.is_none() {
        return Err("MCP already uses another authentication method; update its definition before OAuth login".into());
    }
    let config = login_config(&name, &args, existing);
    def["auth"] = serde_json::to_value(&config)?;
    McpOAuthConfig::from_definition(&def)?;
    let allow_proxy = def
        .get("allow_proxy")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let vault_config = crate::bootstrap::vault_cli::read_vault_config_for_key(&db)?;
    let key = crate::bootstrap::vault_cli::read_verified_vault_key(
        &vault_config,
        args.stdin_password,
        args.keychain,
        args.password_file.as_deref(),
        args.insecure_password_file,
    )?;
    super::cache_mcp_vault_key(&server, &key);
    drop(key);
    let url = server.resolve_remote_mcp_url_for_capability(&capability_id, &def)?;
    let vault = OAuthCredentialStore::new(server.clone(), config.vault_key.clone(), &url, None)?;
    // Check the reference and ACL before asking the owner to authorize in a browser.
    vault.load().await.map_err(|error| oauth_error(&error))?;
    let listener =
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, args.callback_port)).await?;
    let redirect_uri = format!("http://{}/callback", listener.local_addr()?);
    let staged = InMemoryCredentialStore::new();
    let mut manager = authorization_manager(&url, allow_proxy).await?;
    manager.set_credential_store(staged.clone());
    let mut oauth = OAuthState::Unauthorized(manager);
    let mut request = AuthorizationRequest::new(redirect_uri)
        .with_client_name("Tachi")
        .with_scopes(config.scopes.clone());
    if let Some(client_id) = &config.client_id {
        request = request.with_preregistered_client(client_id);
    }
    if let Some(metadata_url) = &config.client_metadata_url {
        request = request.with_client_metadata_url(metadata_url);
    }
    oauth
        .start_authorization(request)
        .await
        .map_err(|error| oauth_error(&error))?;
    let authorization_url = oauth
        .get_authorization_url()
        .await
        .map_err(|error| oauth_error(&error))?;
    let expected_state = url::Url::parse(&authorization_url)?
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .ok_or("OAuth authorization URL has no state")?;
    let (sender, receiver) = oneshot::channel();
    let callback_state = Arc::new(CallbackState {
        expected_state,
        sender: Mutex::new(Some(sender)),
    });
    let app = axum::Router::new()
        .route("/callback", axum::routing::get(callback))
        .with_state(callback_state);
    let (shutdown, stopped) = oneshot::channel();
    let mut callback_server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await;
    });
    eprintln!("Open this URL to authorize the MCP connection:\n{authorization_url}\nWaiting up to 3 minutes for the browser callback.");
    let received = tokio::time::timeout(Duration::from_secs(180), receiver).await;
    let _ = shutdown.send(());
    if tokio::time::timeout(Duration::from_secs(2), &mut callback_server)
        .await
        .is_err()
    {
        callback_server.abort();
        let _ = callback_server.await;
    }
    let authorized = received
        .map_err(|_| "MCP OAuth login timed out; existing credentials are unchanged")?
        .map_err(|_| "MCP OAuth callback was interrupted")?
        .ok_or("MCP OAuth authorization was declined; existing credentials are unchanged")?;
    oauth
        .handle_callback_with_issuer(
            &authorized.code,
            &authorized.state,
            authorized.issuer.as_deref(),
        )
        .await
        .map_err(|error| oauth_error(&error))?;
    let credentials = staged
        .load()
        .await
        .map_err(|error| oauth_error(&error))?
        .ok_or("OAuth provider did not return credentials")?;
    let updated = publish_login(&vault, &capability, &config, credentials).await?;
    let refreshed = crate::hub_ops::refresh_mcp_capability_state(
        &server,
        crate::DbScope::Global,
        &updated,
        true,
    )
    .await;
    let (discovery_status, tools_count, warning) = match refreshed {
        Ok(capability) => {
            let definition: Value = serde_json::from_str(&capability.definition)?;
            let status = definition
                .get("discovery_status")
                .and_then(Value::as_str)
                .unwrap_or("not_started")
                .to_string();
            let warning = match status.as_str() {
                "failed" => Some("OAuth login succeeded; MCP discovery failed. Check the endpoint and retry login."),
                "not_started" => Some("OAuth login succeeded; discovery waits until the MCP is enabled and approved."),
                _ => None,
            };
            (
                status,
                definition.get("tools_count").cloned().unwrap_or(json!(0)),
                warning,
            )
        }
        Err(_) => (
            "failed".into(),
            json!(0),
            Some("OAuth login succeeded; discovery could not be saved. Retry login."),
        ),
    };
    super::super::super::print_pretty_json(&json!({
        "id": capability_id, "authorized": true, "vault_key": config.vault_key,
        "discovery_status": discovery_status, "tools_count": tools_count, "warning": warning,
    }))
}

async fn publish_login(
    vault: &OAuthCredentialStore<crate::MemoryServer>,
    capability: &memcore::HubCapability,
    config: &McpOAuthConfig,
    credentials: rmcp::transport::auth::StoredCredentials,
) -> Result<memcore::HubCapability, Box<dyn std::error::Error>> {
    let _guard = vault
        .acquire_refresh_guard()
        .await
        .map_err(|error| oauth_error(&error))?;
    let original: Value = serde_json::from_str(&capability.definition)?;
    vault
        .save_with(credentials, |transaction| {
            let mut current = transaction
                .hub_get(&capability.id)
                .map_err(|_| "Cannot read registered MCP")?
                .filter(|current| current.cap_type.eq_ignore_ascii_case("mcp"))
                .ok_or("MCP registration was removed or replaced")?;
            let mut definition: Value = serde_json::from_str(&current.definition)
                .map_err(|_| "Invalid registered MCP definition")?;
            if !same_connection(&definition, &original) {
                return Err("MCP connection changed during login".into());
            }
            definition["auth"] =
                serde_json::to_value(config).map_err(|_| "Invalid MCP auth configuration")?;
            crate::mcp_proxy::clear_mcp_discovery_metadata(&mut definition);
            let definition =
                serde_json::to_string(&definition).map_err(|_| "Cannot encode MCP definition")?;
            if !transaction
                .hub_replace_definition(&current, &definition)
                .map_err(|_| "Cannot update MCP configuration")?
            {
                return Err("MCP connection changed during login".into());
            }
            current.definition = definition;
            Ok(current)
        })
        .map_err(|_| {
            "Cannot commit MCP OAuth login; check Vault access and retry if the connection changed"
                .into()
        })
}

fn login_config(
    name: &str,
    args: &McpLoginArgs,
    existing: Option<McpOAuthConfig>,
) -> McpOAuthConfig {
    let mut config = existing.unwrap_or_else(|| McpOAuthConfig {
        kind: "oauth".into(),
        vault_key: format!("MCP_{}_OAUTH", name.to_ascii_uppercase()),
        client_id: None,
        client_metadata_url: None,
        scopes: Vec::new(),
    });
    if args.client_id.is_some() {
        config.client_id = args.client_id.clone();
        config.client_metadata_url = None;
    }
    if args.client_metadata_url.is_some() {
        config.client_metadata_url = args.client_metadata_url.clone();
        config.client_id = None;
    }
    if !args.scopes.is_empty() {
        config.scopes.clone_from(&args.scopes);
    }
    config
}

fn same_connection(left: &Value, right: &Value) -> bool {
    [
        "url",
        "transport",
        "command",
        "args",
        "headers",
        "auth_header",
        "auth",
        "allow_proxy",
    ]
    .iter()
    .all(|field| left.get(field) == right.get(field))
}

struct AuthorizedCallback {
    code: String,
    state: String,
    issuer: Option<String>,
}

struct CallbackState {
    expected_state: String,
    sender: Mutex<Option<oneshot::Sender<Option<AuthorizedCallback>>>>,
}

async fn callback(
    axum::extract::State(state): axum::extract::State<Arc<CallbackState>>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> (axum::http::StatusCode, &'static str) {
    use axum::http::StatusCode;
    if params.get("state") != Some(&state.expected_state) {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid OAuth callback state. Return to the authorization page.",
        );
    }
    let result = if params.contains_key("error") {
        None
    } else {
        let Some(code) = params.get("code").filter(|code| !code.is_empty()) else {
            return (StatusCode::BAD_REQUEST, "Missing OAuth authorization code.");
        };
        Some(AuthorizedCallback {
            code: code.clone(),
            state: state.expected_state.clone(),
            issuer: params.get("iss").cloned(),
        })
    };
    let Ok(mut sender) = state.sender.lock() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "OAuth callback unavailable.",
        );
    };
    let Some(sender) = sender.take() else {
        return (StatusCode::CONFLICT, "OAuth callback already received.");
    };
    let _ = sender.send(result);
    (
        StatusCode::OK,
        "OAuth callback received. Return to Tachi to see the login result.",
    )
}

#[cfg(test)]
#[path = "mcp_oauth/tests.rs"]
mod tests;
