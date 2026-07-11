use super::*;

pub(crate) fn is_bigmodel_remote_mcp(def: &serde_json::Value) -> bool {
    def.get("url")
        .and_then(|value| value.as_str())
        .map(|url| url.contains("open.bigmodel.cn/api/mcp/"))
        .unwrap_or(false)
}

/// Per-server opt-in to allow this remote MCP server's HTTP client to honor a
/// configured/system proxy (HTTP_PROXY/HTTPS_PROXY/etc). Defaults to `false`
/// (fail-safe): any absent or malformed `allow_proxy` field resolves to the
/// pinned/no-proxy default — never to the proxy-permitted path. See
/// `kckylechen1/tachi#947`.
pub(super) fn remote_mcp_allow_proxy(def: &serde_json::Value) -> bool {
    def.get("allow_proxy")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(super) fn remote_mcp_url(def: &serde_json::Value) -> Option<&str> {
    if let Some(url) = def.get("url").and_then(|value| value.as_str()) {
        return Some(url);
    }

    let command = def.get("command").and_then(|value| value.as_str())?;
    let command_name = std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    if command_name != "mcp-remote" {
        return None;
    }

    def.get("args")
        .and_then(|value| value.as_array())
        .and_then(|args| {
            args.iter()
                .filter_map(|arg| arg.as_str())
                .find(|arg| arg.starts_with("https://") || arg.starts_with("http://"))
        })
}

pub(super) struct ValidatedRemoteMcpUrl {
    pub(super) url: String,
    pub(super) resolved_addrs: Option<Vec<SocketAddr>>,
}

fn mcp_remote_ip_is_blocked(ip: IpAddr) -> bool {
    is_private_or_local_ip(ip)
}

fn reject_blocked_mcp_remote_ip(ip: IpAddr) -> Result<(), String> {
    if mcp_remote_ip_is_blocked(ip) {
        Err(format!(
            "MCP URL resolves to a private or local address: {ip}"
        ))
    } else {
        Ok(())
    }
}

/// Validate that a remote MCP URL points to a publicly reachable host.
/// Blocks non-HTTP(S) schemes, loopback, link-local, and private addresses
/// to prevent SSRF against internal services.
pub(super) fn validate_mcp_remote_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid MCP URL '{url}': {e}"))?;

    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "MCP URL scheme '{scheme}' is not allowed; only http/https are permitted"
        ));
    }

    let host = parsed
        .host()
        .ok_or_else(|| format!("MCP URL '{url}' is missing a host"))?;

    // Hostname-level blocklist for common local aliases; IPv6 literals keep brackets.
    let host_str = host.to_string().to_ascii_lowercase();
    if host_str == "localhost"
        || host_str == "127.0.0.1"
        || host_str == "[::1]"
        || host_str.ends_with(".localhost")
        || host_str == "0.0.0.0"
        || host_str == "[::]"
    {
        return Err(format!(
            "MCP URL host '{host}' resolves to a loopback address and is not allowed"
        ));
    }

    match host {
        url::Host::Ipv4(v4) => {
            reject_blocked_mcp_remote_ip(IpAddr::V4(v4)).map_err(|_| {
                format!(
                    "MCP URL host '{host}' is a loopback/private/link-local address and is not allowed"
                )
            })?;
        }
        url::Host::Ipv6(v6) => {
            reject_blocked_mcp_remote_ip(IpAddr::V6(v6)).map_err(|_| {
                format!("MCP URL host '{host}' is a loopback/link-local address and is not allowed")
            })?;
        }
        _ => {}
    }

    Ok(())
}

pub(super) async fn validate_remote_mcp_url_for_connect(
    url: &str,
) -> Result<ValidatedRemoteMcpUrl, String> {
    validate_mcp_remote_url(url)?;
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid MCP URL '{url}': {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("MCP URL '{url}' is missing a host"))?;

    let ip_literal = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    if ip_literal.parse::<IpAddr>().is_ok() {
        return Ok(ValidatedRemoteMcpUrl {
            url: url.to_string(),
            resolved_addrs: None,
        });
    }

    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| format!("MCP URL '{url}' has no usable port"))?;
    let mut resolved_addrs = Vec::new();
    let mut resolved_any = false;
    for addr in lookup_host((host, port))
        .await
        .map_err(|e| format!("resolve MCP URL host: {e}"))?
    {
        resolved_any = true;
        reject_blocked_mcp_remote_ip(addr.ip())?;
        resolved_addrs.push(addr);
    }
    if !resolved_any {
        return Err(format!("MCP URL host '{host}' resolved to no addresses"));
    }

    Ok(ValidatedRemoteMcpUrl {
        url: url.to_string(),
        resolved_addrs: Some(resolved_addrs),
    })
}

pub(super) fn build_remote_mcp_http_client(
    validated: &ValidatedRemoteMcpUrl,
    timeout_secs: u64,
    allow_proxy: bool,
    server_label: &str,
) -> Result<reqwest::Client, String> {
    // reqwest is built with rustls-no-provider; install ring before building
    // any HTTPS-capable client so the first request doesn't panic.
    crate::ensure_tls_provider();

    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(timeout_secs));

    if allow_proxy {
        // Owner-ratified escape hatch (kckylechen1/tachi#947): a per-server
        // `allow_proxy: true` opt-in permits a configured/system proxy
        // (HTTP_PROXY/HTTPS_PROXY/etc) to handle this server's connection.
        // The `resolve_to_addrs` IP pin below is meaningless behind a proxy
        // (the proxy — not us — does DNS + connect), so it is intentionally
        // skipped in this branch. This is a conscious, per-server bypass of
        // SSRF IP pinning, never a blanket default.
        tracing::warn!(
            server = %server_label,
            "remote MCP server '{server_label}' has allow_proxy=true: SSRF IP pinning is \
             bypassed for this server because its HTTP client is permitted to use a \
             configured/system proxy"
        );
    } else {
        // SECURITY (fail-safe default, kckylechen1/tachi#947): tachi-server's
        // reqwest is built with the `system-proxy` feature, which by default
        // honors $HTTP_PROXY/$HTTPS_PROXY/system proxy settings. Without
        // `.no_proxy()`, our pre-connect SSRF validation (resolving the host
        // and rejecting private/local IPs, then pinning the connection to
        // those exact resolved addresses via `resolve_to_addrs`) is dead
        // weight: a proxy that rebinds the hostname to 127.x/private
        // (attacker-controlled or misconfigured proxy) bypasses the guard
        // entirely. `.no_proxy()` clears any configured proxy AND disables
        // the automatic system-proxy lookup, so this client always connects
        // directly to the addresses we already validated.
        builder = builder.no_proxy();
        if let Some(addrs) = &validated.resolved_addrs {
            if let Some(host) = url::Url::parse(&validated.url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_string))
            {
                builder = builder.resolve_to_addrs(host.as_str(), addrs);
            }
        }
    }

    builder
        .build()
        .map_err(|e| format!("build http client: {e}"))
}

pub(super) fn resolve_remote_mcp_url_with_secret_resolver<F>(
    def: &serde_json::Value,
    secret_resolver: &F,
) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let url = remote_mcp_url(def).ok_or_else(|| "missing url for remote MCP".to_string())?;
    let expanded = expand_placeholders_with_secret_resolver(url, secret_resolver)?;
    validate_mcp_remote_url(&expanded)?;
    Ok(expanded)
}

/// Returns true for remote HTTP-based MCP servers (streamable-http, sse, http transport).
/// These servers use raw HTTP JSON-RPC instead of rmcp's transport layer to avoid
/// argument serialization issues in rmcp's streamable-http client.
pub(crate) fn is_remote_http_mcp(def: &serde_json::Value) -> bool {
    let transport = def
        .get("transport")
        .and_then(|v| v.as_str())
        .unwrap_or("stdio");
    let has_remote_url = remote_mcp_url(def).is_some();
    if matches!(transport, "streamable-http" | "sse" | "http") && has_remote_url {
        return true;
    }

    // mcp-remote is a stdio wrapper around a remote HTTP MCP server. Treating
    // it as remote HTTP lets Tachi use its own spec-compliant stateless
    // Streamable HTTP path instead of paying an extra process hop.
    transport == "stdio"
        && def
            .get("command")
            .and_then(|value| value.as_str())
            .map(|command| {
                std::path::Path::new(command)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(command)
                    == "mcp-remote"
            })
            .unwrap_or(false)
        && has_remote_url
}

pub(super) fn parse_sse_payload(body: &str) -> Result<serde_json::Value, String> {
    let mut data_lines = Vec::new();
    for line in body.lines() {
        if let Some(payload) = line.strip_prefix("data:") {
            let trimmed = payload.trim();
            if !trimmed.is_empty() {
                data_lines.push(trimmed.to_string());
            }
        }
    }

    for payload in data_lines.iter().rev() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
            return Ok(json);
        }
    }

    serde_json::from_str(body).map_err(|_| {
        format!(
            "No JSON payload found in response body ({} bytes)",
            body.len()
        )
    })
}

pub(super) fn parse_remote_mcp_jsonrpc_response(
    body: &str,
    context: &str,
    expected_id: i64,
) -> Result<serde_json::Value, String> {
    let value = parse_sse_payload(body)?;
    if value.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(format!("{context} response is not JSON-RPC 2.0"));
    }
    let id = value
        .get("id")
        .ok_or_else(|| format!("{context} response is missing JSON-RPC id"))?;
    if id != &json!(expected_id) {
        return Err(format!(
            "{context} response JSON-RPC id does not match request id {expected_id}"
        ));
    }
    Ok(value)
}

pub(super) async fn read_remote_mcp_body(
    mut response: reqwest::Response,
    context: &str,
) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|len| len > REMOTE_MCP_MAX_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "{context} body exceeds {} bytes",
            REMOTE_MCP_MAX_RESPONSE_BYTES
        ));
    }

    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("{context} body: {e}"))?
    {
        if body.len().saturating_add(chunk.len()) > REMOTE_MCP_MAX_RESPONSE_BYTES {
            return Err(format!(
                "{context} body exceeds {} bytes",
                REMOTE_MCP_MAX_RESPONSE_BYTES
            ));
        }
        body.extend_from_slice(&chunk);
    }

    String::from_utf8(body).map_err(|e| format!("{context} body is not UTF-8: {e}"))
}

fn truncate_for_remote_mcp_error(message: &str) -> String {
    let mut out = message
        .chars()
        .take(REMOTE_MCP_ERROR_MESSAGE_MAX_CHARS)
        .collect::<String>();
    if message.chars().count() > REMOTE_MCP_ERROR_MESSAGE_MAX_CHARS {
        out.push_str("...");
    }
    out
}

pub(super) fn remote_mcp_error_summary(error: &serde_json::Value) -> String {
    if let Some(obj) = error.as_object() {
        let code = obj
            .get("code")
            .and_then(|value| value.as_i64())
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let message = obj
            .get("message")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(truncate_for_remote_mcp_error)
            .unwrap_or_else(|| "remote server returned an error".to_string());
        format!("code={code}, message={message}")
    } else {
        "remote server returned a non-object error".to_string()
    }
}

pub(super) async fn send_remote_mcp_initialized_notification(
    client: &reqwest::Client,
    url: &str,
    headers: reqwest::header::HeaderMap,
) -> Result<(), String> {
    let response = client
        .post(url)
        .headers(headers)
        .json(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .send()
        .await
        .map_err(|e| format!("initialized notification failed: {e}"))?;
    response
        .error_for_status()
        .map(|_| ())
        .map_err(|e| format!("initialized notification failed: {e}"))
}
