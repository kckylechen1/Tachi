use super::super::*;

impl MemoryServer {
    pub(crate) async fn proxy_call_bigmodel_mcp(
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
        let allow_proxy = remote_mcp_allow_proxy(def);
        let client = build_remote_mcp_http_client(&validated, 90, allow_proxy, capability_id)
            .map_err(|e| {
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

    pub(crate) async fn proxy_list_remote_http_mcp_tools(
        &self,
        capability_id: &str,
        def: &serde_json::Value,
    ) -> Result<Vec<rmcp::model::Tool>, String> {
        let url = self.resolve_remote_mcp_url_for_capability(capability_id, def)?;
        let validated = validate_remote_mcp_url_for_connect(&url).await?;
        let allow_proxy = remote_mcp_allow_proxy(def);
        let client = build_remote_mcp_http_client(&validated, 90, allow_proxy, capability_id)?;
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
}
