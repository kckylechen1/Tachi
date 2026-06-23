use super::*;

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

    pub(crate) async fn proxy_list_remote_http_mcp_tools(
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

    pub(crate) fn clear_proxy_tools(&self, server_name: &str) {
        lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools").remove(server_name);
    }

    pub(crate) fn cache_proxy_tools(&self, server_name: &str, tools: Vec<rmcp::model::Tool>) {
        lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools")
            .insert(server_name.to_string(), tools);
    }

    pub(crate) async fn connect_mcp_service(
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

    pub(crate) async fn discover_mcp_tools(
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
