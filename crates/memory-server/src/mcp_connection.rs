use crate::network_safety::is_private_or_local_ip;
use crate::server_state::MemoryServer;
use crate::utils::lock_or_recover;
use crate::vault_ops::read_unlocked_vault_secret;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::json;
use serde_json::Map as JsonMap;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::lookup_host;

const MCP_PRESERVED_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_RUNTIME_DIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
];
const REMOTE_MCP_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const REMOTE_MCP_ERROR_MESSAGE_MAX_CHARS: usize = 240;

fn apply_sanitized_child_env(cmd: &mut tokio::process::Command, env_map: &HashMap<String, String>) {
    cmd.env_clear();
    for var in MCP_PRESERVED_ENV_VARS {
        if let Ok(val) = std::env::var(var) {
            cmd.env(var, val);
        }
    }
    for (k, v) in env_map {
        cmd.env(k, v);
    }
}

fn parse_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

fn apply_env_allowlist(
    env_map: HashMap<String, String>,
    allowlist: &[String],
) -> HashMap<String, String> {
    if allowlist.is_empty() {
        return env_map;
    }
    let allowed: std::collections::HashSet<&str> = allowlist.iter().map(String::as_str).collect();
    env_map
        .into_iter()
        .filter(|(k, _)| allowed.contains(k.as_str()))
        .collect()
}

fn normalize_path(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

fn canonicalize_for_policy(path: &std::path::Path) -> std::path::PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

fn path_within_roots(path: &std::path::Path, roots: &[String]) -> bool {
    if roots.is_empty() {
        return true;
    }
    let candidate = canonicalize_for_policy(path);
    roots.iter().any(|root| {
        let root_path = canonicalize_for_policy(&normalize_path(root));
        candidate.starts_with(&root_path)
    })
}

fn resolve_env_map_with_secret_resolver<F>(
    def: &serde_json::Value,
    secret_resolver: &F,
) -> Result<HashMap<String, String>, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut result = HashMap::new();
    if let Some(obj) = def.get("env").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let Some(val) = v.as_str() {
                let resolved = expand_placeholders_with_secret_resolver(val, secret_resolver)?;
                result.insert(k.clone(), resolved);
            } else {
                return Err(format!(
                    "Invalid env value type for key '{}': expected string",
                    k
                ));
            }
        }
    } else if def.get("env").is_some() {
        return Err("Invalid env field: expected object".to_string());
    }
    Ok(result)
}

#[cfg(test)]
fn expand_env_placeholders(value: &str) -> Result<String, String> {
    expand_placeholders_with_secret_resolver(value, &|_| Ok(None))
}

fn expand_placeholders_with_secret_resolver<F>(
    value: &str,
    secret_resolver: &F,
) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut output = String::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = value[cursor..].find("${") {
        let start = cursor + rel_start;
        output.push_str(&value[cursor..start]);
        let rest = &value[start + 2..];
        let end_rel = rest
            .find('}')
            .ok_or_else(|| format!("Unclosed environment placeholder in '{value}'"))?;
        let end = start + 2 + end_rel;
        let spec = &value[start + 2..end];
        let resolved = if let Some(secret_spec) = spec.strip_prefix("vault:") {
            resolve_secret_fallback_chain(secret_spec, secret_resolver)?
        } else {
            resolve_env_fallback_chain(spec)?
        };
        output.push_str(&resolved);
        cursor = end + 1;
    }

    output.push_str(&value[cursor..]);
    Ok(output)
}

fn resolve_secret_fallback_chain<F>(spec: &str, secret_resolver: &F) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let candidates: Vec<&str> = spec
        .split('|')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .collect();
    if candidates.is_empty() {
        return Err("Empty vault placeholder".to_string());
    }

    let mut last_secret_error = None;
    for key in &candidates {
        validate_secret_key_name(key)?;
        match secret_resolver(key) {
            Ok(Some(value)) if !value.trim().is_empty() => return Ok(value.trim().to_string()),
            Ok(_) => {}
            Err(err) => last_secret_error = Some(err),
        }
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
    }

    let base = if candidates.len() == 1 {
        "Vault secret not available".to_string()
    } else {
        "No configured vault secret fallback is available".to_string()
    };
    if let Some(err) = last_secret_error {
        Err(format!("{base}: {err}"))
    } else {
        Err(base)
    }
}

fn resolve_env_fallback_chain(spec: &str) -> Result<String, String> {
    let candidates: Vec<&str> = spec
        .split('|')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .collect();
    if candidates.is_empty() {
        return Err("Empty environment placeholder".to_string());
    }

    for key in &candidates {
        validate_env_key_name(key)?;
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
    }

    if candidates.len() == 1 {
        Err(format!(
            "Environment variable '{}' not set (required by MCP server)",
            candidates[0]
        ))
    } else {
        Err(format!(
            "None of the environment variables [{}] are set (required by MCP server)",
            candidates.join(", ")
        ))
    }
}

fn validate_env_key_name(key: &str) -> Result<(), String> {
    validate_secret_key_name(key).map_err(|_| {
        format!(
            "Invalid environment variable name '{}': only letters, digits, and underscores are allowed",
            key
        )
    })?;
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err("Environment variable name cannot be empty".to_string());
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!(
            "Invalid environment variable name '{}': must start with a letter or underscore",
            key
        ));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(format!(
            "Invalid environment variable name '{}': only letters, digits, and underscores are allowed",
            key
        ));
    }
    Ok(())
}

fn validate_secret_key_name(key: &str) -> Result<(), String> {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err("Secret name cannot be empty".to_string());
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(format!(
            "Invalid secret name '{}': must start with a letter or underscore",
            key
        ));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(format!(
            "Invalid secret name '{}': only letters, digits, and underscores are allowed",
            key
        ));
    }
    Ok(())
}

#[cfg(test)]
fn resolve_header_map(def: &serde_json::Value) -> Result<HashMap<HeaderName, HeaderValue>, String> {
    resolve_header_map_with_secret_resolver(def, &|_| Ok(None))
}

fn resolve_header_map_with_secret_resolver<F>(
    def: &serde_json::Value,
    secret_resolver: &F,
) -> Result<HashMap<HeaderName, HeaderValue>, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut headers = HashMap::new();
    if let Some(obj) = def.get("headers").and_then(|value| value.as_object()) {
        for (name, value) in obj {
            let raw_value = value.as_str().ok_or_else(|| {
                format!("Invalid header value type for '{name}': expected string")
            })?;
            let resolved = expand_placeholders_with_secret_resolver(raw_value, secret_resolver)?;
            insert_header(&mut headers, name, &resolved)?;
        }
    } else if def.get("headers").is_some() {
        return Err("Invalid headers field: expected object".to_string());
    }

    if let Some(auth) = def.get("auth") {
        for (name, value) in resolve_broker_auth_headers(auth, secret_resolver)? {
            insert_header(&mut headers, &name, &value)?;
        }
    }

    Ok(headers)
}

fn insert_header(
    headers: &mut HashMap<HeaderName, HeaderValue>,
    name: &str,
    value: &str,
) -> Result<(), String> {
    let header_name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|e| format!("Invalid header name '{name}': {e}"))?;
    let header_value = HeaderValue::from_str(value)
        .map_err(|e| format!("Invalid header value for '{name}': {e}"))?;
    headers.insert(header_name, header_value);
    Ok(())
}

fn resolve_secret_reference<F>(key_spec: &str, secret_resolver: &F) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    resolve_secret_fallback_chain(key_spec, secret_resolver)
}

fn resolve_broker_auth_headers<F>(
    auth: &serde_json::Value,
    secret_resolver: &F,
) -> Result<Vec<(String, String)>, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let obj = auth
        .as_object()
        .ok_or_else(|| "Invalid auth field: expected object".to_string())?;
    let auth_type = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "auth.type is required".to_string())?;

    match auth_type {
        "bearer" => {
            let key = obj
                .get("token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "auth.token is required for bearer auth".to_string())?;
            let token = resolve_secret_reference(key, secret_resolver)?;
            Ok(vec![(
                "Authorization".to_string(),
                format!("Bearer {token}"),
            )])
        }
        "basic" => {
            let user_key = obj
                .get("username")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "auth.username is required for basic auth".to_string())?;
            let username = resolve_secret_reference(user_key, secret_resolver)?;
            let password = obj
                .get("password")
                .and_then(|v| v.as_str())
                .map(|key| resolve_secret_reference(key, secret_resolver))
                .transpose()?
                .unwrap_or_default();
            let encoded = B64.encode(format!("{username}:{password}"));
            Ok(vec![(
                "Authorization".to_string(),
                format!("Basic {encoded}"),
            )])
        }
        "api-key" => {
            let key = obj
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "auth.key is required for api-key auth".to_string())?;
            let header = obj
                .get("header")
                .and_then(|v| v.as_str())
                .unwrap_or("Authorization");
            let prefix = obj.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
            let value = resolve_secret_reference(key, secret_resolver)?;
            let header_value = if prefix.is_empty() {
                value
            } else {
                format!("{prefix} {value}")
            };
            Ok(vec![(header.to_string(), header_value)])
        }
        "custom" => {
            let headers = obj
                .get("headers")
                .and_then(|v| v.as_object())
                .ok_or_else(|| "auth.headers is required for custom auth".to_string())?;
            let mut out = Vec::new();
            for (name, value) in headers {
                let template = value.as_str().ok_or_else(|| {
                    format!("Invalid custom auth header '{name}': expected string")
                })?;
                out.push((
                    name.clone(),
                    expand_custom_auth_template(template, secret_resolver)?,
                ));
            }
            Ok(out)
        }
        "passthrough" => Ok(Vec::new()),
        other => Err(format!(
            "auth.type '{}' is not supported (expected bearer, basic, api-key, custom, passthrough)",
            other
        )),
    }
}

fn expand_custom_auth_template<F>(template: &str, secret_resolver: &F) -> Result<String, String>
where
    F: Fn(&str) -> Result<Option<String>, String>,
{
    let mut output = String::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = template[cursor..].find("{{") {
        let start = cursor + rel_start;
        output.push_str(&template[cursor..start]);
        let rest = &template[start + 2..];
        let end_rel = rest
            .find("}}")
            .ok_or_else(|| format!("Unclosed credential placeholder in '{template}'"))?;
        let end = start + 2 + end_rel;
        let key = template[start + 2..end].trim();
        output.push_str(&resolve_secret_reference(key, secret_resolver)?);
        cursor = end + 2;
    }

    output.push_str(&template[cursor..]);
    Ok(output)
}

pub(crate) fn is_bigmodel_remote_mcp(def: &serde_json::Value) -> bool {
    def.get("url")
        .and_then(|value| value.as_str())
        .map(|url| url.contains("open.bigmodel.cn/api/mcp/"))
        .unwrap_or(false)
}

fn remote_mcp_url(def: &serde_json::Value) -> Option<&str> {
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

struct ValidatedRemoteMcpUrl {
    url: String,
    resolved_addrs: Option<Vec<SocketAddr>>,
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
fn validate_mcp_remote_url(url: &str) -> Result<(), String> {
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

async fn validate_remote_mcp_url_for_connect(url: &str) -> Result<ValidatedRemoteMcpUrl, String> {
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

fn build_remote_mcp_http_client(
    validated: &ValidatedRemoteMcpUrl,
    timeout_secs: u64,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(timeout_secs));
    if let Some(addrs) = &validated.resolved_addrs {
        if let Some(host) = url::Url::parse(&validated.url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string))
        {
            builder = builder.resolve_to_addrs(host.as_str(), addrs);
        }
    }
    builder
        .build()
        .map_err(|e| format!("build http client: {e}"))
}

fn resolve_remote_mcp_url_with_secret_resolver<F>(
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

fn parse_sse_payload(body: &str) -> Result<serde_json::Value, String> {
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

fn parse_remote_mcp_jsonrpc_response(
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

async fn read_remote_mcp_body(
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

fn remote_mcp_error_summary(error: &serde_json::Value) -> String {
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

async fn send_remote_mcp_initialized_notification(
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

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

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

        let single = expand_placeholders_with_secret_resolver(
            "Bearer ${vault:PRIMARY_REMOTE_SECRET}",
            &|_| Ok(None),
        )
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

        let response = reqwest::Client::new()
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

        let response = reqwest::Client::new()
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

        let err = send_remote_mcp_initialized_notification(
            &reqwest::Client::new(),
            &format!("http://127.0.0.1:{port}/mcp"),
            reqwest::header::HeaderMap::new(),
        )
        .await
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
}

impl MemoryServer {
    fn resolve_vault_secret_for_capability(
        &self,
        capability_id: &str,
        key: &str,
    ) -> Result<Option<String>, String> {
        match read_unlocked_vault_secret(self, key, Some(capability_id), true) {
            Ok(value) => Ok(Some(value)),
            // Backward compatibility: existing Hub definitions can still run
            // from env vars when the vault is unavailable or intentionally
            // locked. Authorization failures are not swallowed below.
            Err(err)
                if err.starts_with("Secret not found: ")
                    || err.starts_with("Vault is locked")
                    || err.starts_with("Vault auto-locked")
                    || err.starts_with("Vault not initialized") =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    fn resolve_env_map_for_capability(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<HashMap<String, String>, String> {
        resolve_env_map_with_secret_resolver(def, &|key| {
            self.resolve_vault_secret_for_capability(capability_id, key)
        })
    }

    fn resolve_header_map_for_capability(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<HashMap<HeaderName, HeaderValue>, String> {
        resolve_header_map_with_secret_resolver(def, &|key| {
            self.resolve_vault_secret_for_capability(capability_id, key)
        })
    }

    fn resolve_auth_header_for_capability(
        &self,
        capability_id: &str,
        value: &str,
    ) -> Result<String, String> {
        expand_placeholders_with_secret_resolver(value, &|key| {
            self.resolve_vault_secret_for_capability(capability_id, key)
        })
    }

    fn resolve_remote_mcp_url_for_capability(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<String, String> {
        resolve_remote_mcp_url_with_secret_resolver(def, &|key| {
            self.resolve_vault_secret_for_capability(capability_id, key)
        })
    }

    pub(super) async fn proxy_call_bigmodel_mcp(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
        tool_name: &str,
        arguments: Option<JsonMap<String, serde_json::Value>>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let url = self
            .resolve_remote_mcp_url_for_capability(capability_id, def)
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("resolve remote MCP URL: {e}"), None)
            })?;
        let validated = validate_remote_mcp_url_for_connect(&url)
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("validate remote MCP URL: {e}"), None)
            })?;
        let client = build_remote_mcp_http_client(&validated, 90).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("build http client: {e}"), None)
        })?;
        let url = validated.url;

        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in self
            .resolve_header_map_for_capability(capability_id, def)
            .map_err(|e| rmcp::ErrorData::internal_error(format!("resolve headers: {e}"), None))?
        {
            headers.insert(name, value);
        }
        if let Some(token) = def
            .get("auth_header")
            .and_then(|value| value.as_str())
            .map(|value| self.resolve_auth_header_for_capability(capability_id, value))
            .transpose()
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("resolve auth header: {e}"), None)
            })?
        {
            let bearer = format!("Bearer {token}");
            let header_value = HeaderValue::from_str(&bearer).map_err(|e| {
                rmcp::ErrorData::internal_error(format!("invalid authorization header: {e}"), None)
            })?;
            headers.insert(reqwest::header::AUTHORIZATION, header_value);
        }
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );

        let initialize_payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "tachi-hub", "version": env!("CARGO_PKG_VERSION")},
            }
        });
        let init_response = client
            .post(&url)
            .headers(headers.clone())
            .json(&initialize_payload)
            .send()
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("initialize request failed: {e}"), None)
            })?;
        let init_headers = init_response.headers().clone();
        let init_body = read_remote_mcp_body(init_response, "initialize")
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let init_json =
            parse_remote_mcp_jsonrpc_response(&init_body, "initialize", 1).map_err(|e| {
                rmcp::ErrorData::internal_error(format!("parse initialize response: {e}"), None)
            })?;
        if init_json.get("error").is_some() {
            let error = init_json.get("error").expect("checked above");
            return Err(rmcp::ErrorData::internal_error(
                format!(
                    "remote MCP initialize failed: {}",
                    remote_mcp_error_summary(error)
                ),
                None,
            ));
        }

        let session_id = init_headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok());

        let mut session_headers = headers.clone();
        if let Some(sid) = session_id {
            let session_header = HeaderValue::from_str(sid).map_err(|e| {
                rmcp::ErrorData::internal_error(format!("invalid session header: {e}"), None)
            })?;
            session_headers.insert(HeaderName::from_static("mcp-session-id"), session_header);
        }

        send_remote_mcp_initialized_notification(&client, &url, session_headers.clone())
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

        let call_payload = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments.unwrap_or_default(),
            }
        });
        let call_response = client
            .post(&url)
            .headers(session_headers)
            .json(&call_payload)
            .send()
            .await
            .map_err(|e| {
                rmcp::ErrorData::internal_error(format!("remote tool call failed: {e}"), None)
            })?;
        let call_body = read_remote_mcp_body(call_response, "remote tool")
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        let call_json =
            parse_remote_mcp_jsonrpc_response(&call_body, "remote tool", 2).map_err(|e| {
                rmcp::ErrorData::internal_error(format!("parse tool response: {e}"), None)
            })?;

        if let Some(error) = call_json.get("error") {
            return Err(rmcp::ErrorData::internal_error(
                format!("remote MCP tool error: {}", remote_mcp_error_summary(error)),
                None,
            ));
        }

        let result_json = call_json.get("result").cloned().ok_or_else(|| {
            rmcp::ErrorData::internal_error("remote MCP missing result field".to_string(), None)
        })?;
        serde_json::from_value(result_json).map_err(|e| {
            rmcp::ErrorData::internal_error(format!("decode remote tool result failed: {e}"), None)
        })
    }

    pub(super) async fn proxy_list_remote_http_mcp_tools(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<Vec<rmcp::model::Tool>, String> {
        let url = self.resolve_remote_mcp_url_for_capability(capability_id, def)?;
        let validated = validate_remote_mcp_url_for_connect(&url).await?;
        let client = build_remote_mcp_http_client(&validated, 90)?;
        let url = validated.url;

        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in self.resolve_header_map_for_capability(capability_id, def)? {
            headers.insert(name, value);
        }
        if let Some(token) = def
            .get("auth_header")
            .and_then(|value| value.as_str())
            .map(|value| self.resolve_auth_header_for_capability(capability_id, value))
            .transpose()?
        {
            let bearer = format!("Bearer {token}");
            let header_value = HeaderValue::from_str(&bearer)
                .map_err(|e| format!("invalid authorization header: {e}"))?;
            headers.insert(reqwest::header::AUTHORIZATION, header_value);
        }
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );

        let initialize_payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "tachi-hub", "version": env!("CARGO_PKG_VERSION")},
            }
        });
        let init_response = client
            .post(&url)
            .headers(headers.clone())
            .json(&initialize_payload)
            .send()
            .await
            .map_err(|e| format!("initialize request failed: {e}"))?;
        let init_headers = init_response.headers().clone();
        let init_body = read_remote_mcp_body(init_response, "initialize").await?;
        let init_json = parse_remote_mcp_jsonrpc_response(&init_body, "initialize", 1)
            .map_err(|e| format!("parse initialize response: {e}"))?;
        if let Some(error) = init_json.get("error") {
            return Err(format!(
                "remote MCP initialize failed: {}",
                remote_mcp_error_summary(error)
            ));
        }

        let mut session_headers = headers.clone();
        if let Some(sid) = init_headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            let session_header =
                HeaderValue::from_str(sid).map_err(|e| format!("invalid session header: {e}"))?;
            session_headers.insert(HeaderName::from_static("mcp-session-id"), session_header);
        }

        send_remote_mcp_initialized_notification(&client, &url, session_headers.clone()).await?;

        let list_response = client
            .post(&url)
            .headers(session_headers)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            }))
            .send()
            .await
            .map_err(|e| format!("tools/list request failed: {e}"))?;
        let list_body = read_remote_mcp_body(list_response, "tools/list").await?;
        let list_json = parse_remote_mcp_jsonrpc_response(&list_body, "tools/list", 2)
            .map_err(|e| format!("parse tools/list response: {e}"))?;
        if let Some(error) = list_json.get("error") {
            return Err(format!(
                "remote MCP tools/list failed: {}",
                remote_mcp_error_summary(error)
            ));
        }
        let result_json = list_json
            .get("result")
            .cloned()
            .ok_or_else(|| "remote MCP tools/list missing result field".to_string())?;
        let result: rmcp::model::ListToolsResult = serde_json::from_value(result_json)
            .map_err(|e| format!("decode tools/list result failed: {e}"))?;
        Ok(result.tools)
    }

    pub(super) fn clear_proxy_tools(&self, server_name: &str) {
        lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools").remove(server_name);
    }

    pub(super) fn cache_proxy_tools(&self, server_name: &str, tools: Vec<rmcp::model::Tool>) {
        lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools")
            .insert(server_name.to_string(), tools);
    }

    pub(super) async fn connect_mcp_service(
        &self,
        capability_id: &str,
        requested_capability_id: Option<&str>,
        def: &serde_json::Value,
        timeout: Duration,
    ) -> Result<rmcp::service::RunningService<rmcp::service::RoleClient, ()>, String> {
        let connect_started = Instant::now();
        let (policy, policy_source) =
            self.get_effective_sandbox_policy(requested_capability_id, capability_id);
        let policy_enabled = policy
            .as_ref()
            .and_then(|v| v.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !policy_enabled {
            self.record_sandbox_exec_audit(
                capability_id,
                "preflight",
                "denied",
                Some("sandbox policy disabled capability"),
                0,
                None,
                Some("policy_disabled"),
                &json!({
                    "has_policy": policy.is_some(),
                    "requested_capability_id": requested_capability_id,
                    "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                }),
            );
            return Err(format!(
                "Sandbox policy disabled capability '{}'",
                capability_id
            ));
        }

        let policy_runtime = policy
            .as_ref()
            .and_then(|v| v.get("runtime_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("process");
        if policy_runtime != "process" && policy_runtime != "wasm" {
            self.record_sandbox_exec_audit(
                capability_id,
                "preflight",
                "denied",
                Some("invalid sandbox runtime_type"),
                0,
                None,
                Some("invalid_runtime_type"),
                &json!({
                    "runtime_type": policy_runtime,
                    "requested_capability_id": requested_capability_id,
                    "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                }),
            );
            return Err(format!(
                "Invalid sandbox runtime_type '{}' for '{}'",
                policy_runtime, capability_id
            ));
        }

        let policy_startup_ms = policy
            .as_ref()
            .and_then(|v| v.get("max_startup_ms"))
            .and_then(|v| v.as_u64())
            .unwrap_or(timeout.as_millis() as u64)
            .max(1);
        let effective_timeout = Duration::from_millis(
            std::cmp::min(timeout.as_millis() as u64, policy_startup_ms).max(1),
        );

        let transport_type = match def.get("transport") {
            Some(v) => match v.as_str() {
                Some(raw) => raw,
                None => {
                    eprintln!(
                        "[mcp] Invalid 'transport' field type; expected string, defaulting to 'stdio'"
                    );
                    "stdio"
                }
            },
            None => "stdio",
        };
        match transport_type {
            "stdio" => {
                if policy_runtime == "wasm" {
                    self.record_sandbox_exec_audit(
                        capability_id,
                        "preflight",
                        "denied",
                        Some("runtime_type=wasm incompatible with stdio transport"),
                        0,
                        None,
                        Some("runtime_transport_mismatch"),
                        &json!({
                            "runtime_type": policy_runtime,
                            "transport": transport_type,
                            "requested_capability_id": requested_capability_id,
                            "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                        }),
                    );
                    return Err(format!(
                        "Capability '{}' requires runtime_type=wasm but stdio transport was requested",
                        capability_id
                    ));
                }

                let command = def["command"]
                    .as_str()
                    .ok_or_else(|| "missing command".to_string())?;
                let args: Vec<String> = match def.get("args") {
                    Some(v) if v.is_null() => Vec::new(),
                    Some(v) => {
                        let array = v
                            .as_array()
                            .ok_or_else(|| "invalid args: expected string array".to_string())?;
                        let mut parsed = Vec::with_capacity(array.len());
                        for (idx, item) in array.iter().enumerate() {
                            let value = item.as_str().ok_or_else(|| {
                                format!("invalid args[{idx}]: expected string value")
                            })?;
                            parsed.push(value.to_string());
                        }
                        parsed
                    }
                    None => Vec::new(),
                };
                let env_map = self.resolve_env_map_for_capability(capability_id, def).map_err(|e| {
                    self.record_sandbox_exec_audit(
                        capability_id,
                        "preflight",
                        "denied",
                        Some("invalid env configuration"),
                        0,
                        None,
                        Some("invalid_env"),
                        &json!({
                            "transport": transport_type,
                            "requested_capability_id": requested_capability_id,
                            "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                            "error": e,
                        }),
                    );
                    e
                })?;
                let env_allowlist =
                    parse_string_array(policy.as_ref().and_then(|v| v.get("env_allowlist")));
                let env_map = apply_env_allowlist(env_map, &env_allowlist);

                let cwd_roots =
                    parse_string_array(policy.as_ref().and_then(|v| v.get("cwd_roots")));
                let fs_read_roots =
                    parse_string_array(policy.as_ref().and_then(|v| v.get("fs_read_roots")));
                let fs_write_roots =
                    parse_string_array(policy.as_ref().and_then(|v| v.get("fs_write_roots")));
                if !fs_read_roots.is_empty() || !fs_write_roots.is_empty() {
                    self.record_sandbox_exec_audit(
                        capability_id,
                        "preflight",
                        "denied",
                        Some("process runtime cannot enforce fs root restrictions"),
                        0,
                        None,
                        Some("fs_roots_unsupported"),
                        &json!({
                            "transport": transport_type,
                            "requested_capability_id": requested_capability_id,
                            "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                            "fs_read_roots": fs_read_roots,
                            "fs_write_roots": fs_write_roots,
                        }),
                    );
                    return Err(format!(
                        "Sandbox policy for '{}' declares fs_read_roots/fs_write_roots, but stdio process transport cannot enforce them yet",
                        capability_id
                    ));
                }
                let cwd = def.get("cwd").and_then(|v| v.as_str());
                if !cwd_roots.is_empty() {
                    let cwd_str = cwd.ok_or_else(|| {
                        let reason = format!(
                            "Sandbox policy for '{}' requires cwd within allowed roots, but definition has no cwd",
                            capability_id
                        );
                        self.record_sandbox_exec_audit(
                            capability_id,
                            "preflight",
                            "denied",
                            Some("cwd required by policy but missing in definition"),
                            0,
                            None,
                            Some("cwd_missing"),
                            &json!({
                                "transport": transport_type,
                                "requested_capability_id": requested_capability_id,
                                "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                                "cwd_roots": cwd_roots,
                            }),
                        );
                        reason
                    })?;
                    let cwd_path = normalize_path(cwd_str);
                    if !path_within_roots(&cwd_path, &cwd_roots) {
                        self.record_sandbox_exec_audit(
                            capability_id,
                            "preflight",
                            "denied",
                            Some("cwd outside allowed roots"),
                            0,
                            None,
                            Some("cwd_denied"),
                            &json!({
                                "transport": transport_type,
                                "cwd": cwd_path.display().to_string(),
                                "requested_capability_id": requested_capability_id,
                                "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                                "cwd_roots": cwd_roots,
                            }),
                        );
                        return Err(format!(
                            "Sandbox policy denied cwd '{}' for '{}'",
                            cwd_path.display(),
                            capability_id
                        ));
                    }
                }

                let mut cmd = tokio::process::Command::new(command);
                cmd.args(&args);
                // Do NOT use kill_on_drop(true) here. TokioChildProcess owns the
                // Child handle and kills via that handle on drop/graceful shutdown,
                // which avoids the PID-reuse race inherent in storing a bare PID.
                apply_sanitized_child_env(&mut cmd, &env_map);
                if let Some(cwd_str) = cwd {
                    cmd.current_dir(normalize_path(cwd_str));
                }

                let transport = rmcp::transport::TokioChildProcess::new(cmd).map_err(|e| {
                    let reason = format!("spawn failed: {e}");
                    self.record_sandbox_exec_audit(
                        capability_id,
                        "startup",
                        "failed",
                        Some("child process spawn failed"),
                        connect_started.elapsed().as_millis() as u64,
                        None,
                        Some("spawn_failed"),
                        &json!({
                            "transport": transport_type,
                            "requested_capability_id": requested_capability_id,
                            "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                            "error": reason,
                        }),
                    );
                    reason
                })?;

                match tokio::time::timeout(
                    effective_timeout,
                    rmcp::ServiceExt::serve((), transport),
                )
                .await
                {
                    Ok(Ok(client)) => {
                        self.record_sandbox_exec_audit(
                            capability_id,
                            "startup",
                            "allowed",
                            None,
                            connect_started.elapsed().as_millis() as u64,
                            None,
                            None,
                            &json!({
                                "transport": transport_type,
                                "requested_capability_id": requested_capability_id,
                                "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                                "policy_timeout_ms": effective_timeout.as_millis() as u64,
                            }),
                        );
                        Ok(client)
                    }
                    Ok(Err(e)) => {
                        let reason = format!("MCP handshake failed: {e}");
                        self.record_sandbox_exec_audit(
                            capability_id,
                            "startup",
                            "failed",
                            Some("handshake failed"),
                            connect_started.elapsed().as_millis() as u64,
                            None,
                            Some("handshake_failed"),
                            &json!({
                                "transport": transport_type,
                                "requested_capability_id": requested_capability_id,
                                "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                                "error": reason,
                            }),
                        );
                        Err(reason)
                    }
                    Err(_) => {
                        let reason = format!(
                            "MCP handshake timed out after {}ms",
                            effective_timeout.as_millis()
                        );
                        self.record_sandbox_exec_audit(
                            capability_id,
                            "startup",
                            "timeout",
                            Some("handshake timeout"),
                            connect_started.elapsed().as_millis() as u64,
                            None,
                            Some("startup_timeout"),
                            &json!({
                                "transport": transport_type,
                                "requested_capability_id": requested_capability_id,
                                "policy_source": if policy_source.is_empty() { None::<String> } else { Some(policy_source.clone()) },
                                "effective_timeout_ms": effective_timeout.as_millis() as u64,
                            }),
                        );
                        Err(reason)
                    }
                }
            }
            "sse" | "http" | "streamable-http" => {
                let url = self.resolve_remote_mcp_url_for_capability(capability_id, def)?;
                validate_remote_mcp_url_for_connect(&url).await?;
                let mut transport_config =
                    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(
                        url.as_str(),
                    );
                if let Some(token) = def
                    .get("auth_header")
                    .and_then(|value| value.as_str())
                    .map(|value| self.resolve_auth_header_for_capability(capability_id, value))
                    .transpose()?
                {
                    transport_config = transport_config.auth_header(token);
                }
                let headers = self.resolve_header_map_for_capability(capability_id, def)?;
                if !headers.is_empty() {
                    transport_config = transport_config.custom_headers(headers);
                }
                let transport = StreamableHttpClientTransport::from_config(transport_config);
                match tokio::time::timeout(
                    effective_timeout,
                    rmcp::ServiceExt::serve((), transport),
                )
                .await
                {
                    Ok(Ok(client)) => {
                        self.record_sandbox_exec_audit(
                            capability_id,
                            "startup",
                            "allowed",
                            None,
                            connect_started.elapsed().as_millis() as u64,
                            None,
                            None,
                            &json!({
                                "transport": transport_type,
                                "url": url,
                                "policy_timeout_ms": effective_timeout.as_millis() as u64,
                            }),
                        );
                        Ok(client)
                    }
                    Ok(Err(e)) => {
                        let reason = format!("SSE handshake failed: {e}");
                        self.record_sandbox_exec_audit(
                            capability_id,
                            "startup",
                            "failed",
                            Some("remote transport handshake failed"),
                            connect_started.elapsed().as_millis() as u64,
                            None,
                            Some("handshake_failed"),
                            &json!({
                                "transport": transport_type,
                                "url": url,
                                "error": reason,
                            }),
                        );
                        Err(reason)
                    }
                    Err(_) => {
                        let reason = format!(
                            "SSE handshake timed out after {}ms",
                            effective_timeout.as_millis()
                        );
                        self.record_sandbox_exec_audit(
                            capability_id,
                            "startup",
                            "timeout",
                            Some("remote transport handshake timeout"),
                            connect_started.elapsed().as_millis() as u64,
                            None,
                            Some("startup_timeout"),
                            &json!({
                                "transport": transport_type,
                                "url": url,
                                "effective_timeout_ms": effective_timeout.as_millis() as u64,
                            }),
                        );
                        Err(reason)
                    }
                }
            }
            other => {
                self.record_sandbox_exec_audit(
                    capability_id,
                    "preflight",
                    "denied",
                    Some("unsupported transport"),
                    0,
                    None,
                    Some("unsupported_transport"),
                    &json!({
                        "transport": other,
                    }),
                );
                Err(format!("unsupported transport: {other}"))
            }
        }
    }

    pub(super) async fn discover_mcp_tools(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<Vec<rmcp::model::Tool>, String> {
        if is_remote_http_mcp(def) {
            return self
                .proxy_list_remote_http_mcp_tools(capability_id, def)
                .await;
        }

        let discovery_timeout = self.tool_discovery.mcp_discovery_timeout;
        let client = self
            .connect_mcp_service(capability_id, None, def, discovery_timeout)
            .await?;
        let list_result =
            tokio::time::timeout(discovery_timeout, client.peer().list_all_tools()).await;
        let cancel_result = client.cancel().await;

        match list_result {
            Ok(Ok(tools)) => {
                let _ = cancel_result;
                Ok(tools)
            }
            Ok(Err(e)) => Err(format!("list_tools failed: {e}")),
            Err(_) => Err(format!(
                "list_tools timed out after {}ms",
                discovery_timeout.as_millis()
            )),
        }
    }
}
