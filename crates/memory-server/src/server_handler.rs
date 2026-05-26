use super::*;

fn current_exposed_tool_patterns() -> Option<Vec<String>> {
    std::env::var("TACHI_EXPOSED_TOOLS")
        .ok()
        .map(|raw| crate::profiles::parse_tool_patterns_csv(&raw))
        .filter(|patterns| !patterns.is_empty())
}

fn tool_not_found_error() -> rmcp::ErrorData {
    rmcp::ErrorData::invalid_request("tool not found".to_string(), None)
}

impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Tachi — memory + Hub copilot for AI agents. Before non-trivial work, call tachi_task(action='plan') to recall wiki lessons, prior memories, and useful skills. When stuck after repeated attempts, search wiki with tachi_wiki(action='search') and inspect tachi_status. Store durable debugging lessons with tachi_wiki(action='write').")
    }

    fn list_tools(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        async move {
            let all_native = self.tool_router.list_all();
            let mut tools: Vec<rmcp::model::Tool> = all_native;

            // Add proxy tools from registered MCP servers
            let proxy_snapshot = match self.proxy_tools.lock() {
                Ok(proxy) => proxy.clone(),
                Err(poisoned) => {
                    eprintln!(
                        "[list_tools] WARNING: proxy_tools mutex poisoned; recovering with inner state"
                    );
                    poisoned.into_inner().clone()
                }
            };
            for (server_name, server_tools) in proxy_snapshot {
                let cap_id = format!("mcp:{server_name}");
                let cap = match self.get_capability(&cap_id) {
                    Ok(cap) if cap.enabled => cap,
                    _ => continue,
                };
                if !should_expose_mcp_tools(&cap) {
                    continue;
                }

                let cap_def = match serde_json::from_str::<serde_json::Value>(&cap.definition) {
                    Ok(def) => def,
                    Err(e) => {
                        eprintln!(
                            "[list_tools] WARNING: invalid capability definition JSON for '{}': {e}; skipping direct proxy exposure",
                            cap_id
                        );
                        continue;
                    }
                };
                let exposure_mode =
                    resolve_mcp_tool_exposure(&cap_def, self.mcp_tool_exposure_mode);
                if exposure_mode == McpToolExposureMode::Gateway {
                    continue;
                }

                let filtered_tools =
                    filter_mcp_tools_by_permissions(&cap_def, server_tools.clone());

                for tool in filtered_tools {
                    let mut proxied = tool.clone();
                    proxied.name =
                        std::borrow::Cow::Owned(format!("{}__{}", server_name, tool.name));
                    tools.push(proxied);
                }
            }
            // Add skill tools
            if self.mcp_tool_exposure_mode != McpToolExposureMode::Gateway {
                match self.skill_tool_defs.lock() {
                    Ok(skill_defs) => tools.extend(skill_defs.values().cloned()),
                    Err(poisoned) => {
                        eprintln!(
                            "[list_tools] WARNING: skill_tool_defs mutex poisoned; recovering with inner state"
                        );
                        tools.extend(poisoned.into_inner().values().cloned());
                    }
                }
            }

            let env_patterns = current_exposed_tool_patterns();
            tools = crate::profiles::filter_tool_defs(
                tools,
                self.active_tool_profile(),
                env_patterns.as_deref(),
            );

            Ok(rmcp::model::ListToolsResult {
                tools,
                ..Default::default()
            })
        }
    }

    fn call_tool(
        &self,
        params: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::CallToolResult, rmcp::ErrorData>> + Send + '_
    {
        async move {
            let name = params.name.as_ref();
            let env_patterns = current_exposed_tool_patterns();

            let visible = crate::profiles::tool_visible(
                name,
                self.active_tool_profile(),
                env_patterns.as_deref(),
            );

            if !visible {
                return Err(tool_not_found_error());
            }

            // ─── Rate Limiter: throttle and loop detection ───────────────
            let stuck_warning: Option<String> = {
                let args_hash = params
                    .arguments
                    .as_ref()
                    .map(|a| stable_hash(&serde_json::to_string(a).unwrap_or_default()))
                    .unwrap_or_default();
                // Each MemoryServer clone corresponds to one MCP session
                // (StreamableHttpService creates one clone per session), so
                // "default" as session_id is correct for per-session limiting.
                self.check_rate_limit(name, &args_hash, "default")?
            };

            // ─── Phantom Tools: cache invalidation on write ops ──────────
            if CACHE_INVALIDATING_TOOLS.contains(&name) {
                if let Ok(mut cache) = self.tool_cache.lock() {
                    cache.clear();
                }
            }

            // ─── Phantom Tools: check cache for read-only tools ──────────
            let is_cacheable = CACHEABLE_TOOLS.contains(&name);
            let cache_key = if is_cacheable {
                let args_str = params
                    .arguments
                    .as_ref()
                    .map(|a| {
                        serde_json::to_string(a).unwrap_or_else(|e| {
                            eprintln!(
                                "[call_tool] WARNING: failed to serialize arguments for cache key (tool='{}'): {e}",
                                name
                            );
                            String::new()
                        })
                    })
                    .unwrap_or_default();
                let key = stable_hash(&format!("{}{}", name, args_str));

                // Check cache
                if let Ok(cache) = self.tool_cache.lock() {
                    if let Some(cached) = cache.get(&key) {
                        if cached.created_at.elapsed() < TOOL_CACHE_TTL {
                            self.cache_hits
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let mut hit = cached.result.clone();
                            if let Some(warn) = stuck_warning.clone() {
                                hit.content.push(rmcp::model::Content::text(warn));
                            }
                            return Ok(hit);
                        }
                    }
                }
                self.cache_misses
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(key)
            } else {
                None
            };

            // ─── Dispatch to handler ─────────────────────────────────────
            // Save tool name and arguments for DLQ capture on failure
            let tool_name_owned = name.to_string();
            let tool_args_for_dlq = params.arguments.clone();

            let result = {
                // 1. Native tools first (highest priority)
                if self.tool_router.has_route(name) {
                    let context =
                        rmcp::handler::server::tool::ToolCallContext::new(self, params, context);
                    self.tool_router.call(context).await
                }
                // 2. Skill tools (tachi_skill_*)
                else if self
                    .skill_tools
                    .lock()
                    .unwrap_or_else(|e| {
                        eprintln!(
                            "[call_tool] WARNING: skill_tools mutex poisoned; recovering with inner state"
                        );
                        e.into_inner()
                    })
                    .contains_key(name)
                {
                    if self.mcp_tool_exposure_mode == McpToolExposureMode::Gateway {
                        Err(rmcp::ErrorData::invalid_params(
                            "Direct skill tools are disabled for gateway mode; use run_skill".to_string(),
                            None,
                        ))
                    } else {
                        self.call_skill_tool(name, params.arguments).await
                    }
                }
                // 3. Proxy tools (server__tool pattern)
                else if let Some((server_name, tool_name)) = name.split_once("__") {
                    let exposure_mode = self.proxy_tool_exposure_mode_for_server(server_name)?;
                    if exposure_mode == McpToolExposureMode::Gateway {
                        Err(rmcp::ErrorData::invalid_params(
                            format!(
                                "Direct proxy tools are disabled for '{}'; use hub_call(server_id='mcp:{}', tool_name='{}')",
                                server_name, server_name, tool_name
                            ),
                            None,
                        ))
                    } else {
                        self.proxy_call_internal(server_name, tool_name, params.arguments)
                            .await
                    }
                } else {
                    Err(tool_not_found_error())
                }
            };

            // ─── Dead Letter Queue: capture failures ─────────────────────
            // Skip DLQ for DLQ tools themselves and ghost_* tools
            let is_dlq_exempt = tool_name_owned.starts_with("dlq_")
                || tool_name_owned.starts_with("ghost_")
                || tool_name_owned == "get_pipeline_status";

            if let Err(ref err) = result {
                if !is_dlq_exempt {
                    let error_str = format!("{}", err);
                    let category = categorize_error(&error_str);
                    let should_auto_retry = category == "timeout" || category == "internal";

                    let dl = DeadLetter {
                        id: uuid::Uuid::new_v4().to_string(),
                        tool_name: tool_name_owned.clone(),
                        arguments: tool_args_for_dlq.clone(),
                        error: error_str.clone(),
                        error_category: category.clone(),
                        timestamp: Utc::now().to_rfc3339(),
                        retry_count: 0,
                        max_retries: if should_auto_retry { 1 } else { 3 },
                        status: "pending".to_string(),
                    };

                    let dl_id = dl.id.clone();
                    {
                        let mut dlq = self.dead_letters.lock().unwrap_or_else(|e| e.into_inner());
                        dlq.push_back(dl);
                        // Enforce ring buffer max
                        while dlq.len() > DLQ_MAX_ENTRIES {
                            dlq.pop_front();
                        }
                    }

                    // Auto-retry once for timeout/internal errors
                    if should_auto_retry {
                        // Brief delay before retry
                        tokio::time::sleep(Duration::from_millis(100)).await;

                        // Retry via shared dispatch helper
                        let retry_result = self
                            .retry_dispatch(&tool_name_owned, tool_args_for_dlq)
                            .await;

                        {
                            let mut dlq =
                                self.dead_letters.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(dl) = dlq.iter_mut().find(|dl| dl.id == dl_id) {
                                dl.retry_count = 1;
                                if retry_result.is_ok() {
                                    dl.status = "resolved".to_string();
                                } else {
                                    dl.status = "abandoned".to_string();
                                    if let Err(ref e) = retry_result {
                                        dl.error = format!("{e}");
                                    }
                                }
                            }
                        }

                        if retry_result.is_ok() {
                            // Cache the retry result if applicable
                            if let (Some(key), Ok(ref res)) = (&cache_key, &retry_result) {
                                if let Ok(mut cache) = self.tool_cache.lock() {
                                    cache.insert(
                                        key.clone(),
                                        CachedResult {
                                            result: res.clone(),
                                            created_at: Instant::now(),
                                        },
                                    );
                                }
                            }
                            return match (retry_result, stuck_warning.clone()) {
                                (Ok(mut tool_result), Some(warn)) => {
                                    tool_result.content.push(rmcp::model::Content::text(warn));
                                    Ok(tool_result)
                                }
                                (other, _) => other,
                            };
                        }
                    }
                }
            }

            // ─── Phantom Tools: store result in cache ────────────────────
            if let (Some(key), Ok(ref res)) = (&cache_key, &result) {
                if let Ok(mut cache) = self.tool_cache.lock() {
                    // Evict expired entries when cache exceeds cap
                    if cache.len() >= TOOL_CACHE_MAX_ENTRIES {
                        cache.retain(|_, v| v.created_at.elapsed() < TOOL_CACHE_TTL);
                        // If still over cap after TTL eviction, remove oldest entries
                        if cache.len() >= TOOL_CACHE_MAX_ENTRIES {
                            let mut oldest_key = None;
                            let mut oldest_age = Duration::ZERO;
                            for (k, v) in cache.iter() {
                                let age = v.created_at.elapsed();
                                if age > oldest_age {
                                    oldest_age = age;
                                    oldest_key = Some(k.clone());
                                }
                            }
                            if let Some(k) = oldest_key {
                                cache.remove(&k);
                            }
                        }
                    }
                    cache.insert(
                        key.clone(),
                        CachedResult {
                            result: res.clone(),
                            created_at: Instant::now(),
                        },
                    );
                }
            }

            // ─── Stuck detection: append soft warning block ──────────────
            // Done AFTER caching so the cached entry stays warning-free; the
            // warning is intentionally a property of *this* call, not of the
            // tool's output. Cache hits and retries get fresh warnings on
            // their own dispatch through call_tool.
            let result = match (result, stuck_warning) {
                (Ok(mut tool_result), Some(warn)) => {
                    tool_result.content.push(rmcp::model::Content::text(warn));
                    Ok(tool_result)
                }
                (other, _) => other,
            };

            result
        }
    }
}
