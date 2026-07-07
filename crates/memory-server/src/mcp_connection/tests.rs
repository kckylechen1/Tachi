use super::*;

/// reqwest is built with rustls-no-provider, so the crypto provider must be
/// installed before any `reqwest::Client` is built (even for plain HTTP).
/// Tests here construct clients directly, so install ring once up front.
fn ensure_test_tls_provider() {
    crate::ensure_tls_provider();
}

#[test]
fn expand_env_placeholders_supports_fallback_chain() {
    std::env::remove_var("BIGMODEL_API_KEY");
    std::env::set_var("REASONING_API_KEY", "glm-key");

    let expanded = expand_env_placeholders("Bearer ${BIGMODEL_API_KEY|REASONING_API_KEY}")
        .expect("fallback expansion should work");
    assert_eq!(expanded, "Bearer glm-key");
}

#[test]
fn resolve_header_map_expands_fallback_headers() {
    std::env::remove_var("BIGMODEL_API_KEY");
    std::env::set_var("REASONING_API_KEY", "glm-key");

    let headers = resolve_header_map(&json!({
        "headers": {
            "Authorization": "Bearer ${BIGMODEL_API_KEY|REASONING_API_KEY}"
        }
    }))
    .expect("header map should resolve");

    let auth = headers
        .get(&HeaderName::from_static("authorization"))
        .expect("authorization header should exist");
    assert_eq!(auth, "Bearer glm-key");
}

#[test]
fn resolve_env_fallback_chain_rejects_invalid_key_names() {
    let err = resolve_env_fallback_chain("BIGMODEL_API_KEY|BAD-KEY")
        .expect_err("invalid key names should be rejected");
    assert!(err.contains("Invalid environment variable name"));
}

#[test]
fn vault_placeholders_resolve_without_exposing_env() {
    std::env::remove_var("SECRET_TOKEN");
    let expanded =
        expand_placeholders_with_secret_resolver("Bearer ${vault:SECRET_TOKEN}", &|key| {
            Ok((key == "SECRET_TOKEN").then(|| "vault-secret".to_string()))
        })
        .expect("vault placeholder should resolve");

    assert_eq!(expanded, "Bearer vault-secret");
}

#[test]
fn missing_vault_placeholders_do_not_enumerate_secret_names() {
    std::env::remove_var("PRIMARY_REMOTE_SECRET");
    std::env::remove_var("SECONDARY_REMOTE_SECRET");

    let single =
        expand_placeholders_with_secret_resolver("Bearer ${vault:PRIMARY_REMOTE_SECRET}", &|_| {
            Ok(None)
        })
        .expect_err("missing single vault placeholder should fail");
    assert!(single.contains("Vault secret not available"));
    assert!(!single.contains("PRIMARY_REMOTE_SECRET"));

    let chain = expand_placeholders_with_secret_resolver(
        "Bearer ${vault:PRIMARY_REMOTE_SECRET|SECONDARY_REMOTE_SECRET}",
        &|_| Ok(None),
    )
    .expect_err("missing vault fallback chain should fail");
    assert!(chain.contains("No configured vault secret fallback is available"));
    assert!(!chain.contains("PRIMARY_REMOTE_SECRET"));
    assert!(!chain.contains("SECONDARY_REMOTE_SECRET"));
}

#[test]
fn broker_auth_resolves_bearer_from_vault_secret() {
    std::env::remove_var("EXA_API_KEY");
    let headers = resolve_header_map_with_secret_resolver(
        &json!({
            "auth": {
                "type": "bearer",
                "token": "EXA_API_KEY"
            }
        }),
        &|key| Ok((key == "EXA_API_KEY").then(|| "exa-secret".to_string())),
    )
    .expect("broker auth should resolve");

    let auth = headers
        .get(&HeaderName::from_static("authorization"))
        .expect("authorization header should exist");
    assert_eq!(auth, "Bearer exa-secret");
}

#[test]
fn broker_auth_supports_vault_secret_fallback_chains() {
    std::env::remove_var("PRIMARY_API_KEY");
    std::env::remove_var("SECONDARY_API_KEY");
    let headers = resolve_header_map_with_secret_resolver(
        &json!({
            "auth": {
                "type": "bearer",
                "token": "PRIMARY_API_KEY|SECONDARY_API_KEY"
            }
        }),
        &|key| Ok((key == "SECONDARY_API_KEY").then(|| "secondary-secret".to_string())),
    )
    .expect("broker auth should resolve fallback chains");

    let auth = headers
        .get(&HeaderName::from_static("authorization"))
        .expect("authorization header should exist");
    assert_eq!(auth, "Bearer secondary-secret");
}

#[test]
fn broker_auth_resolves_custom_templates() {
    let headers = resolve_header_map_with_secret_resolver(
        &json!({
            "auth": {
                "type": "custom",
                "headers": {
                    "X-Api-Key": "{{ CUSTOM_API_KEY }}"
                }
            }
        }),
        &|key| Ok((key == "CUSTOM_API_KEY").then(|| "custom-secret".to_string())),
    )
    .expect("custom auth should resolve");

    let value = headers
        .get(&HeaderName::from_static("x-api-key"))
        .expect("x-api-key header should exist");
    assert_eq!(value, "custom-secret");
}

#[test]
fn remote_mcp_url_extracts_mcp_remote_arg() {
    let def = json!({
        "transport": "stdio",
        "command": "/usr/local/bin/mcp-remote",
        "args": ["https://example.test/mcp?apiKey=secret"]
    });

    assert_eq!(
        remote_mcp_url(&def),
        Some("https://example.test/mcp?apiKey=secret")
    );
    assert!(is_remote_http_mcp(&def));
}

#[test]
fn remote_mcp_url_expands_vault_placeholder() {
    std::env::remove_var("TAVILY_API_KEY");
    let def = json!({
        "transport": "stdio",
        "command": "mcp-remote",
        "args": ["https://mcp.tavily.com/mcp/?tavilyApiKey=${vault:TAVILY_API_KEY}"]
    });

    let url = resolve_remote_mcp_url_with_secret_resolver(&def, &|key| {
        Ok((key == "TAVILY_API_KEY").then(|| "tvly-test-key".to_string()))
    })
    .expect("remote URL should resolve vault placeholders");

    assert_eq!(
        url,
        "https://mcp.tavily.com/mcp/?tavilyApiKey=tvly-test-key"
    );
}

#[test]
fn parse_sse_payload_errors_do_not_echo_body() {
    let err = parse_sse_payload("not json with SECRET_TOKEN=ghp_leaky")
        .expect_err("invalid remote body should fail");

    assert!(err.contains("No JSON payload found"));
    assert!(err.contains("bytes"));
    assert!(!err.contains("SECRET_TOKEN"));
    assert!(!err.contains("ghp_leaky"));
}

#[test]
fn remote_mcp_jsonrpc_response_requires_matching_id_without_echoing_body() {
    let body = r#"event: message
data: {"jsonrpc":"2.0","id":99,"result":{"secret":"github_pat_leaky"}}
"#;

    let err = parse_remote_mcp_jsonrpc_response(body, "tools/list", 2)
        .expect_err("mismatched JSON-RPC ids should fail");

    assert!(err.contains("does not match request id 2"), "{err}");
    assert!(!err.contains("github_pat_leaky"));
    assert!(!err.contains("secret"));
}

#[test]
fn remote_mcp_jsonrpc_response_requires_jsonrpc_version() {
    let body = r#"data: {"id":1,"result":{}}"#;

    let err = parse_remote_mcp_jsonrpc_response(body, "initialize", 1)
        .expect_err("missing jsonrpc version should fail");

    assert!(err.contains("not JSON-RPC 2.0"), "{err}");
}

#[test]
fn remote_mcp_error_summary_omits_error_data() {
    let error = json!({
        "code": -32000,
        "message": "provider refused request",
        "data": {
            "secret": "github_pat_leaky",
            "trace": "full remote trace"
        }
    });

    let summary = remote_mcp_error_summary(&error);
    assert!(summary.contains("code=-32000"));
    assert!(summary.contains("provider refused request"));
    assert!(!summary.contains("github_pat_leaky"));
    assert!(!summary.contains("full remote trace"));
    assert!(!summary.contains("data"));
}

#[tokio::test]
async fn remote_mcp_body_reader_rejects_large_content_length() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind remote body fixture");
    let port = listener.local_addr().expect("listener addr").port();
    let server_task = tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n",
                    REMOTE_MCP_MAX_RESPONSE_BYTES + 1
                );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });

    let response = {
        ensure_test_tls_provider();
        reqwest::Client::new()
    }
    .get(format!("http://127.0.0.1:{port}/mcp"))
    .send()
    .await
    .expect("response headers");
    let err = read_remote_mcp_body(response, "tools/list")
        .await
        .expect_err("oversized content-length should be rejected before body read");

    assert!(err.contains("tools/list body exceeds"));
    server_task.abort();
}

#[tokio::test]
async fn remote_mcp_body_reader_rejects_streaming_oversize_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind streaming body fixture");
    let port = listener.local_addr().expect("listener addr").port();
    let server_task = tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let headers =
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n";
            let _ = socket.write_all(headers).await;
            let body = vec![b'a'; REMOTE_MCP_MAX_RESPONSE_BYTES + 1];
            let _ = socket.write_all(&body).await;
            let _ = socket.shutdown().await;
        }
    });

    let response = {
        ensure_test_tls_provider();
        reqwest::Client::new()
    }
    .get(format!("http://127.0.0.1:{port}/mcp"))
    .send()
    .await
    .expect("response headers");
    let err = read_remote_mcp_body(response, "remote tool")
        .await
        .expect_err("streaming body should stop at the configured limit");

    assert!(err.contains("remote tool body exceeds"));
    server_task.abort();
}

#[tokio::test]
async fn initialized_notification_reports_http_failure() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind initialized notification fixture");
    let port = listener.local_addr().expect("listener addr").port();
    let server_task = tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            let _ = socket.write_all(response).await;
            let _ = socket.shutdown().await;
        }
    });

    let err = {
        ensure_test_tls_provider();
        send_remote_mcp_initialized_notification(
            &reqwest::Client::new(),
            &format!("http://127.0.0.1:{port}/mcp"),
            reqwest::header::HeaderMap::new(),
        )
        .await
    }
    .expect_err("non-success initialized notification status should fail");

    assert!(err.contains("initialized notification failed"));
    server_task.abort();
}

#[test]
fn validate_mcp_remote_url_allows_public_https() {
    assert!(validate_mcp_remote_url("https://open.bigmodel.cn/api/mcp/").is_ok());
    assert!(validate_mcp_remote_url("https://example.test/mcp").is_ok());
}

#[test]
fn validate_mcp_remote_url_rejects_internal_hosts() {
    for url in [
        "http://localhost/mcp",
        "http://127.0.0.1/mcp",
        "http://[::1]/mcp",
        "http://[::ffff:127.0.0.1]/mcp",
        "http://192.168.1.1/mcp",
        "http://10.0.0.1/mcp",
        "http://172.16.0.1/mcp",
        "http://169.254.1.1/mcp",
        "http://0.0.0.0/mcp",
        "http://[::]/mcp",
        "file:///etc/passwd",
        "https://foo.localhost/mcp",
    ] {
        assert!(
            validate_mcp_remote_url(url).is_err(),
            "{url} should be rejected as internal/unsafe"
        );
    }
}

#[test]
fn resolve_remote_mcp_url_rejects_loopback_after_expansion() {
    let def = json!({
        "transport": "streamable-http",
        "url": "http://127.0.0.1:8080/mcp"
    });
    assert!(
        resolve_remote_mcp_url_with_secret_resolver(&def, &|_| Ok(None)).is_err(),
        "loopback URL should be rejected during resolution"
    );
}

#[test]
fn resolve_remote_mcp_url_rejects_vault_expanded_loopback_host() {
    let def = json!({
        "transport": "streamable-http",
        "url": "http://${vault:MCP_HOST}:8080/mcp"
    });
    assert!(
        resolve_remote_mcp_url_with_secret_resolver(&def, &|key| {
            Ok((key == "MCP_HOST").then(|| "127.0.0.1".to_string()))
        })
        .is_err(),
        "vault-expanded loopback host should be rejected after placeholder expansion"
    );
}
