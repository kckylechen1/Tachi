use super::remote::{build_remote_mcp_http_client, validate_remote_mcp_url_for_connect};
use crate::vault_ops::OAuthCredentialStore;
use crate::MemoryServer;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, CredentialStore, OAuthHttpClient, OAuthHttpClientFuture,
    OAuthHttpRedirectPolicy, OAuthHttpRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpOAuthConfig {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) vault_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_metadata_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) scopes: Vec<String>,
}

impl McpOAuthConfig {
    pub(crate) fn from_definition(def: &Value) -> Result<Option<Self>, String> {
        let Some(auth) = def
            .get("auth")
            .filter(|auth| auth.get("type").and_then(Value::as_str) == Some("oauth"))
        else {
            return Ok(None);
        };
        let config: Self = serde_json::from_value(auth.clone()).map_err(|_| {
            "Invalid MCP OAuth configuration; credentials must use a Vault reference"
        })?;
        crate::vault_crypto::validate_secret_name(&config.vault_key)?;
        if crate::vault_ops::is_lane_slot_secret_name(&config.vault_key)
            || memcore::is_lane_config_url_name(&config.vault_key)
        {
            return Err("MCP OAuth credentials require a dedicated Vault name".into());
        }
        if def.get("auth_header").is_some()
            || def
                .get("headers")
                .and_then(Value::as_object)
                .is_some_and(|headers| {
                    headers
                        .keys()
                        .any(|name| name.eq_ignore_ascii_case("authorization"))
                })
        {
            return Err("MCP OAuth cannot be combined with an Authorization header".into());
        }
        if let Some(url) = &config.client_metadata_url {
            let url = url::Url::parse(url).map_err(|_| "Invalid OAuth client metadata URL")?;
            if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
                return Err("OAuth client metadata URL must be HTTPS without credentials".into());
            }
        }
        Ok(Some(config))
    }
}

/// OAuth errors can contain token endpoint bodies and callback material. Expose categories only.
pub(crate) fn oauth_error(error: &AuthError) -> String {
    match error {
        AuthError::AuthorizationRequired
        | AuthError::TokenExpired
        | AuthError::TokenRefreshRejected(_) => {
            "MCP OAuth authorization required; run `tachi mcp login NAME`".into()
        }
        AuthError::CredentialStoreError(_) => {
            "Cannot access MCP OAuth credentials in Vault; check Vault unlock and access".into()
        }
        AuthError::TokenRefreshFailed(_) => {
            "MCP OAuth refresh failed; retry the request or run `tachi mcp login NAME`".into()
        }
        AuthError::InsufficientScope { .. } | AuthError::InvalidScope(_) => {
            "MCP OAuth requires additional scopes; run `tachi mcp login NAME --scope SCOPE`".into()
        }
        AuthError::RegistrationFailed(_) => {
            "MCP OAuth client registration failed; use a public --client-id if required".into()
        }
        AuthError::PkceUnsupported => "MCP OAuth server does not support PKCE S256".into(),
        AuthError::NoAuthorizationSupport | AuthError::MetadataError(_) => {
            "Cannot discover MCP OAuth authorization metadata".into()
        }
        _ => "MCP OAuth authorization failed; retry login and check the provider configuration"
            .into(),
    }
}

pub(crate) async fn authorization_manager(
    url: &str,
    allow_proxy: bool,
) -> Result<AuthorizationManager, String> {
    AuthorizationManager::new_with_oauth_http_client(
        url,
        Arc::new(McpOAuthHttpClient { allow_proxy }),
    )
    .await
    .map_err(|error| oauth_error(&error))
}

pub(super) async fn restore_authorization(
    server: &MemoryServer,
    capability_id: &str,
    config: &McpOAuthConfig,
    url: &str,
    allow_proxy: bool,
) -> Result<AuthorizationManager, String> {
    let mut manager = authorization_manager(url, allow_proxy).await?;
    let credentials = OAuthCredentialStore::new(
        server.clone(),
        config.vault_key.clone(),
        url,
        Some(capability_id.to_string()),
    )?;
    // SDK restore can clear credentials after an issuer change. Fence it against login/refresh.
    let _guard = credentials
        .acquire_refresh_guard()
        .await
        .map_err(|error| oauth_error(&error))?;
    manager.set_credential_store(credentials);
    if !manager
        .initialize_from_store()
        .await
        .map_err(|error| oauth_error(&error))?
    {
        return Err(format!(
            "MCP OAuth authorization required; run `tachi mcp login {}`",
            capability_id.trim_start_matches("mcp:")
        ));
    }
    Ok(manager)
}

/// Apply the existing remote MCP network policy to every SDK discovery/token request.
struct McpOAuthHttpClient {
    allow_proxy: bool,
}

impl OAuthHttpClient for McpOAuthHttpClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        Box::pin(async move {
            let follow = matches!(request.redirect_policy, OAuthHttpRedirectPolicy::Follow);
            let mut request = reqwest::Request::try_from(request.request)
                .map_err(|_| std::io::Error::other("Invalid OAuth HTTP request"))?;
            for redirects in 0..=5 {
                let validated = validate_remote_mcp_url_for_connect(request.url().as_str())
                    .await
                    .map_err(|_| {
                        std::io::Error::other(
                            "OAuth endpoint rejected by remote MCP network policy",
                        )
                    })?;
                let client =
                    build_remote_mcp_http_client(&validated, 30, self.allow_proxy, "oauth")
                        .map_err(|_| std::io::Error::other("Cannot create OAuth HTTP client"))?;
                let sent = request
                    .try_clone()
                    .ok_or_else(|| std::io::Error::other("Cannot replay OAuth metadata request"))?;
                let mut response = client
                    .execute(sent)
                    .await
                    .map_err(|_| std::io::Error::other("OAuth HTTP request failed"))?;
                if follow
                    && response.status().is_redirection()
                    && matches!(*request.method(), http::Method::GET | http::Method::HEAD)
                {
                    let location = response
                        .headers()
                        .get(http::header::LOCATION)
                        .and_then(|value| value.to_str().ok())
                        .ok_or_else(|| std::io::Error::other("Invalid OAuth metadata redirect"))?;
                    let next = request
                        .url()
                        .join(location)
                        .map_err(|_| std::io::Error::other("Invalid OAuth metadata redirect"))?;
                    if redirects == 5 || next.origin() != request.url().origin() {
                        return Err(
                            std::io::Error::other("OAuth metadata redirect rejected").into()
                        );
                    }
                    *request.url_mut() = next;
                    continue;
                }
                let success = response.status().is_success();
                let mut output = http::Response::builder()
                    .status(response.status())
                    .version(response.version());
                for (name, value) in response.headers() {
                    output = output.header(name, value);
                }
                let limit = super::REMOTE_MCP_MAX_RESPONSE_BYTES;
                if response
                    .content_length()
                    .is_some_and(|length| length > limit as u64)
                {
                    return Err(std::io::Error::other("OAuth response is too large").into());
                }
                let mut body = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| std::io::Error::other("Cannot read OAuth response"))?
                {
                    if chunk.len() > limit - body.len() {
                        return Err(std::io::Error::other("OAuth response is too large").into());
                    }
                    body.extend_from_slice(&chunk);
                }
                if !success {
                    body = sanitized_oauth_error_body(&body);
                }
                return output.body(body).map_err(|error| Box::new(error) as _);
            }
            Err(std::io::Error::other("OAuth metadata redirect limit exceeded").into())
        })
    }
}

fn sanitized_oauth_error_body(body: &[u8]) -> Vec<u8> {
    let parsed = serde_json::from_slice::<Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str);
    let code = code
        .filter(|code| {
            matches!(
                *code,
                "invalid_request"
                    | "invalid_client"
                    | "invalid_grant"
                    | "unauthorized_client"
                    | "unsupported_grant_type"
                    | "invalid_scope"
                    | "access_denied"
                    | "temporarily_unavailable"
            )
        })
        .unwrap_or("server_error");
    serde_json::json!({"error": code}).to_string().into_bytes()
}

#[cfg(test)]
mod tests;
