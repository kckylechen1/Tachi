//! Streamable HTTP MCP transport calls used by CLI and stdio proxy surfaces.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ListToolsResult, RawContent};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::ServiceExt;
use serde_json::Value;

use super::detect::DaemonInfo;
use super::tool_map::remap_daemon_tool;

const DAEMON_CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonCallError {
    BeforeDispatch(String),
    AfterDispatch(String),
}

impl DaemonCallError {
    pub(super) fn message(&self) -> &str {
        match self {
            Self::BeforeDispatch(message) | Self::AfterDispatch(message) => message,
        }
    }

    pub(crate) fn allows_in_process_fallback(&self) -> bool {
        matches!(self, Self::BeforeDispatch(_))
    }
}

impl std::fmt::Display for DaemonCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for DaemonCallError {}

/// Call a tool over MCP-streamable-HTTP against a known daemon URL.
/// Returns the first text content block from the tool result.
pub(crate) async fn call_daemon_tool(
    info: &DaemonInfo,
    tool_name: &str,
    arguments: serde_json::Map<String, Value>,
) -> Result<String, DaemonCallError> {
    let (daemon_tool, daemon_args) = remap_daemon_tool(tool_name, arguments);
    let mut params = CallToolRequestParams::new(daemon_tool.clone());
    if !daemon_args.is_empty() {
        params = params.with_arguments(daemon_args);
    }
    let result = call_daemon_tool_raw(info, params).await?;
    if result.is_error.unwrap_or(false) {
        let err_text =
            first_text_block(&result.content).unwrap_or_else(|| "<no error text>".to_string());
        return Err(DaemonCallError::AfterDispatch(format!(
            "daemon tool '{daemon_tool}' returned error: {err_text}"
        )));
    }

    Ok(first_text_block(&result.content).unwrap_or_else(|| "{}".to_string()))
}

/// Call a daemon tool over Streamable HTTP without remapping the name or
/// collapsing the result to text. stdio proxy mode uses this to stay a pure
/// transport adapter while the daemon remains the semantic owner.
pub(crate) async fn call_daemon_tool_raw(
    info: &DaemonInfo,
    params: CallToolRequestParams,
) -> Result<rmcp::model::CallToolResult, DaemonCallError> {
    let tool_name = params.name.as_ref().to_string();
    let transport_config = StreamableHttpClientTransportConfig::with_uri(info.url.clone());
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client = ServiceExt::serve((), transport).await.map_err(|e| {
        DaemonCallError::BeforeDispatch(format!("daemon handshake failed at {}: {e}", info.url))
    })?;

    let peer = client.peer().clone();
    let result = tokio::time::timeout(DAEMON_CALL_TIMEOUT, peer.call_tool(params))
        .await
        .map_err(|_| {
            DaemonCallError::AfterDispatch(format!(
                "daemon call '{tool_name}' timed out after {:?}",
                DAEMON_CALL_TIMEOUT
            ))
        })?
        .map_err(|e| {
            DaemonCallError::AfterDispatch(format!("daemon call '{tool_name}' failed: {e}"))
        })?;

    // Dropping the client is enough to close the short-lived CLI HTTP session.
    // Calling `cancel()` here has caused daemon-side lifecycle confusion with
    // rmcp streamable HTTP: a successful forwarded write could be followed by
    // the daemon exiting and leaving a stale pid file.
    drop(client);

    Ok(result)
}

/// List daemon tools over Streamable HTTP. stdio proxy mode uses daemon-side
/// discovery so it does not need to construct a local MemoryServer or open DBs.
pub(crate) async fn list_daemon_tools(
    info: &DaemonInfo,
    params: Option<rmcp::model::PaginatedRequestParams>,
) -> Result<ListToolsResult, DaemonCallError> {
    let transport_config = StreamableHttpClientTransportConfig::with_uri(info.url.clone());
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client = ServiceExt::serve((), transport).await.map_err(|e| {
        DaemonCallError::BeforeDispatch(format!("daemon handshake failed at {}: {e}", info.url))
    })?;

    let peer = client.peer().clone();
    let result = tokio::time::timeout(DAEMON_CALL_TIMEOUT, peer.list_tools(params))
        .await
        .map_err(|_| {
            DaemonCallError::AfterDispatch(format!(
                "daemon tools/list timed out after {:?}",
                DAEMON_CALL_TIMEOUT
            ))
        })?
        .map_err(|e| DaemonCallError::AfterDispatch(format!("daemon tools/list failed: {e}")))?;

    drop(client);
    Ok(result)
}

fn first_text_block(blocks: &[rmcp::model::Annotated<RawContent>]) -> Option<String> {
    blocks.iter().find_map(|c| match &c.raw {
        RawContent::Text(t) => Some(t.text.clone()),
        _ => None,
    })
}
