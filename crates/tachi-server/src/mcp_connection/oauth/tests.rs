use super::*;
use serde_json::json;

#[test]
fn oauth_config_accepts_vault_reference_and_rejects_inline_or_conflicting_credentials() {
    let definition = json!({"url":"https://mcp.example.test/mcp", "auth":{"type":"oauth", "vault_key":"MCP_EXAMPLE_OAUTH"}});
    assert_eq!(
        McpOAuthConfig::from_definition(&definition)
            .unwrap()
            .unwrap()
            .vault_key,
        "MCP_EXAMPLE_OAUTH"
    );
    for invalid in [
        json!({"auth":{"type":"oauth", "vault_key":"MCP_EXAMPLE_OAUTH", "token":"private-sentinel"}}),
        json!({"auth":{"type":"oauth", "vault_key":"MCP_EXAMPLE_OAUTH"}, "auth_header":"private-sentinel"}),
        json!({"auth":{"type":"oauth", "vault_key":"MCP_EXAMPLE_OAUTH"}, "headers":{"authorization":"private-sentinel"}}),
    ] {
        let error = McpOAuthConfig::from_definition(&invalid).unwrap_err();
        assert!(!error.contains("private-sentinel"));
    }
}

#[test]
fn oauth_headers_are_owned_by_the_sdk_not_static_vault_expansion() {
    let definition = json!({"auth":{"type":"oauth", "vault_key":"MCP_EXAMPLE_OAUTH"}});
    let headers =
        crate::mcp_connection::env::resolve_header_map_with_secret_resolver(&definition, &|_| {
            panic!("the header resolver must not read the serialized OAuth token envelope")
        })
        .unwrap();
    assert!(headers.is_empty());
}

#[test]
fn oauth_failure_categories_do_not_expose_provider_bodies_or_tokens() {
    for error in [
        AuthError::TokenExchangeFailed("private-sentinel".into()),
        AuthError::TokenRefreshRejected("private-sentinel".into()),
        AuthError::CredentialStoreError("private-sentinel".into()),
        AuthError::RegistrationFailed("private-sentinel".into()),
    ] {
        assert!(!oauth_error(&error).contains("private-sentinel"));
    }
    let body = sanitized_oauth_error_body(br#"{"error":"invalid_grant","error_description":"private-sentinel","access_token":"private-sentinel"}"#);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"error":"invalid_grant"})
    );
    let unknown = sanitized_oauth_error_body(br#"{"error":"private-sentinel"}"#);
    assert!(!String::from_utf8(unknown)
        .unwrap()
        .contains("private-sentinel"));
}
