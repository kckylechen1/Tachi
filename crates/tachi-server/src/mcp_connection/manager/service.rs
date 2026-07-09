use super::super::*;

impl MemoryServer {
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
}
