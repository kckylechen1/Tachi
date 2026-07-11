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

mod env;
mod manager;
mod remote;

#[cfg(test)]
mod tests;

use self::env::{
    apply_env_allowlist, apply_sanitized_child_env, expand_placeholders_with_secret_resolver,
    normalize_path, parse_string_array, path_within_roots, resolve_env_map_with_secret_resolver,
    resolve_header_map_with_secret_resolver,
};
#[cfg(test)]
use self::env::{expand_env_placeholders, resolve_env_fallback_chain, resolve_header_map};
use self::remote::{
    build_remote_mcp_http_client, parse_remote_mcp_jsonrpc_response, read_remote_mcp_body,
    remote_mcp_allow_proxy, remote_mcp_error_summary, resolve_remote_mcp_url_with_secret_resolver,
    send_remote_mcp_initialized_notification, validate_remote_mcp_url_for_connect,
};
#[cfg(test)]
use self::remote::{
    parse_sse_payload, remote_mcp_url, validate_mcp_remote_url, ValidatedRemoteMcpUrl,
};

pub(crate) use self::remote::{is_bigmodel_remote_mcp, is_remote_http_mcp};
