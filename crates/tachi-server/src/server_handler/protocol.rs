use super::*;

impl ServerHandler for MemoryServer {
    // rmcp 3.9 routes SEP-2640 `skills/list` and `skills/get` as typed
    // requests (gated on the declared skills extension). A `skills/*` request
    // reaches this fallback only when its params failed typed decoding, so it
    // is invalid params, not an unknown method; Tachi serves no custom method.
    async fn on_custom_request(
        &self,
        request: rmcp::model::CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CustomResult, rmcp::ErrorData> {
        if matches!(request.method.as_str(), "skills/list" | "skills/get") {
            admit_modern_skill_request(&context)?;
            return Err(rmcp::ErrorData::invalid_params(
                format!("malformed {} params", request.method),
                None,
            ));
        }
        Err(rmcp::ErrorData::new(
            rmcp::model::ErrorCode::METHOD_NOT_FOUND,
            "method not found",
            None,
        ))
    }

    async fn list_skills(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListSkillsResult, rmcp::ErrorData> {
        admit_modern_skill_request(&context)?;
        // A single immutable entry fits one page; no cursors are issued.
        if request.is_some_and(|params| params.cursor.is_some()) {
            return Err(rmcp::ErrorData::invalid_params(
                "invalid skill cursor",
                None,
            ));
        }
        Ok(
            rmcp::model::ListSkillsResult::with_all_items(vec![tachi_skill()?])
                .with_ttl_ms(SKILL_TTL_MS)
                .with_cache_scope(rmcp::model::CacheScope::Public),
        )
    }

    async fn get_skill(
        &self,
        request: rmcp::model::GetSkillRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetSkillResult, rmcp::ErrorData> {
        admit_modern_skill_request(&context)?;
        if request.uri != crate::server_instructions::SKILL_URI {
            return Err(rmcp::ErrorData::invalid_params("skill unavailable", None));
        }
        Ok(rmcp::model::GetSkillResult::new(tachi_skill()?)
            .with_ttl_ms(SKILL_TTL_MS)
            .with_cache_scope(rmcp::model::CacheScope::Public))
    }

    fn supported_protocol_versions(
        &self,
    ) -> std::borrow::Cow<'static, [rmcp::model::ProtocolVersion]> {
        std::borrow::Cow::Borrowed(crate::mcp_peer::supported_protocol_versions())
    }

    async fn discover(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::DiscoverResult, rmcp::ErrorData> {
        let mode = crate::mcp_peer::McpPeerMode::from_context(&context)?.require_modern()?;
        reject_modern_direct_stdio(mode, &context)?;
        validate_modern_identity_headers(&context)?;
        Ok(rmcp::model::DiscoverResult::from_server_info(
            crate::mcp_peer::supported_protocol_versions().to_vec(),
            self.get_info(),
        ))
    }

    // Preserve the SDK's empty inherited results in both implemented modes.
    async fn complete(
        &self,
        _request: rmcp::model::CompleteRequestParams,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::CompleteResult, rmcp::ErrorData> {
        let mode = crate::mcp_peer::McpPeerMode::from_context(&context)?;
        reject_modern_direct_stdio(mode, &context)?;
        if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
            validate_modern_identity_headers(&context)?;
        }
        Ok(Default::default())
    }

    async fn list_prompts(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ListPromptsResult, rmcp::ErrorData> {
        let mode = crate::mcp_peer::McpPeerMode::from_context(&context)?;
        reject_modern_direct_stdio(mode, &context)?;
        let mut result = rmcp::model::ListPromptsResult::default();
        if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
            validate_modern_identity_headers(&context)?;
            result.ttl_ms = Some(0);
            result.cache_scope = Some(rmcp::model::CacheScope::Private);
        }
        Ok(result)
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        let mode = crate::mcp_peer::McpPeerMode::from_context(&context)?;
        reject_modern_direct_stdio(mode, &context)?;
        let mut result = rmcp::model::ListResourcesResult::default();
        if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
            validate_modern_identity_headers(&context)?;
            result.ttl_ms = Some(0);
            result.cache_scope = Some(rmcp::model::CacheScope::Private);
        }
        Ok(result)
    }

    async fn list_resource_templates(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, rmcp::ErrorData> {
        let mode = crate::mcp_peer::McpPeerMode::from_context(&context)?;
        reject_modern_direct_stdio(mode, &context)?;
        let mut result = rmcp::model::ListResourceTemplatesResult::default();
        if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
            validate_modern_identity_headers(&context)?;
            result.ttl_ms = Some(0);
            result.cache_scope = Some(rmcp::model::CacheScope::Private);
        }
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, rmcp::ErrorData> {
        let mode = crate::mcp_peer::McpPeerMode::from_context(&context)?;
        reject_modern_direct_stdio(mode, &context)?;
        if request.uri == crate::server_instructions::SKILL_URI {
            if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
                validate_modern_identity_headers(&context)?;
            }
            // Bundled public documentation has no project-store dependency.
            return Ok(crate::server_instructions::skill_read_response(
                mode == crate::mcp_peer::McpPeerMode::Modern20260728,
            ));
        }
        let request_server = match mode {
            crate::mcp_peer::McpPeerMode::Legacy => None,
            crate::mcp_peer::McpPeerMode::Modern20260728 => {
                Some(self.clone_for_modern_request(&context, false)?)
            }
        };
        let server = request_server.as_ref().unwrap_or(self);
        if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
            server.agent_runtime_write().presence_client_label = context
                .meta
                .client_info()
                .map(|info| crate::claims_ops::sanitize_presence_field(&info.name, 160));
        }
        server.touch_activity();

        let unavailable = || rmcp::ErrorData::resource_not_found("resource unavailable", None);
        let Some(reference) = crate::memory_resources::parse_resource_uri(&request.uri) else {
            return Err(unavailable());
        };
        let Some(project_name) = server
            .session_project()
            .filter(|project| !project.trim().is_empty())
        else {
            return Err(unavailable());
        };
        let profile = server.active_tool_profile();
        if !tachi_hub::tool_visible(
            "tachi_memory",
            profile,
            current_exposed_tool_patterns().as_deref(),
        ) || !tachi_hub::facade_action_allowed("tachi_memory", Some("get"), profile)
        {
            return Err(unavailable());
        }
        let body = crate::memory_resources::read_resource_text(server, &project_name, &reference)
            .map_err(|_| unavailable())?;
        Ok(crate::memory_resources::read_response(
            request.uri,
            body,
            mode == crate::mcp_peer::McpPeerMode::Modern20260728,
        )
        .into())
    }

    fn get_info(&self) -> ServerConfig {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources();
        #[cfg(any(unix, windows))]
        let capabilities = capabilities.enable_tasks();
        let mut capabilities = capabilities.build();
        capabilities
            .extensions
            .get_or_insert_with(Default::default)
            .insert(
                crate::server_instructions::SKILL_EXTENSION.to_string(),
                Default::default(),
            );
        ServerConfig::new(capabilities)
            .with_instructions(crate::server_instructions::skill_server_instructions())
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, rmcp::ErrorData>> + Send + '_ {
        async move {
            crate::mcp_peer::reject_modern_initialize(&request)?;
            let mut info = self.negotiate_initialize(&request)?;
            if let Some(extensions) = info.capabilities.extensions.as_mut() {
                extensions.remove(rmcp::model::TASKS_EXTENSION_ID);
                extensions.remove(crate::server_instructions::SKILL_EXTENSION);
                if extensions.is_empty() {
                    info.capabilities.extensions = None;
                }
            }
            self.apply_http_session_identity(&request, &context)?;
            self.agent_runtime_write().presence_client_label = Some(
                crate::claims_ops::sanitize_presence_field(&request.client_info.name, 160),
            );
            context.peer.set_peer_info(request);
            self.touch_activity();
            Ok(info)
        }
    }

    fn list_tools(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        async move {
            let mode = crate::mcp_peer::McpPeerMode::from_context(&context)?;
            reject_modern_direct_stdio(mode, &context)?;
            let request_server = match mode {
                crate::mcp_peer::McpPeerMode::Legacy => None,
                crate::mcp_peer::McpPeerMode::Modern20260728 => {
                    Some(self.clone_for_modern_request(&context, false)?)
                }
            };
            let server = request_server.as_ref().unwrap_or(self);
            let env_patterns = current_exposed_tool_patterns();
            let mut tools = prepare_native_tool_definitions(server.tool_router.list_all());
            tools.extend(
                lock_or_recover(&server.tool_discovery.skill_tool_defs, "skill_tool_defs")
                    .values()
                    .cloned(),
            );
            let tools = project_tool_definitions(
                tools,
                server.active_tool_profile(),
                env_patterns.as_deref(),
            );

            let mut result = rmcp::model::ListToolsResult {
                tools,
                ..Default::default()
            };
            if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
                result.ttl_ms = Some(0);
                result.cache_scope = Some(rmcp::model::CacheScope::Private);
            }
            Ok(result)
        }
    }

    #[cfg(any(unix, windows))]
    async fn get_task(
        &self,
        request: rmcp::model::GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetTaskResult, rmcp::ErrorData> {
        let server = task_request_server(self, &context, &request.task_id, "status")?;
        let result = crate::mcp_tasks::get(&server, &request.task_id)?;
        if result.task.status() == rmcp::model::TaskStatus::InputRequired
            && !context
                .client_capabilities()
                .and_then(|caps| caps.elicitation)
                .is_some_and(|cap| cap.form.is_some())
        {
            let mut required = rmcp::model::ClientCapabilities::default();
            required.elicitation = Some(
                rmcp::model::ElicitationCapability::new()
                    .with_form(rmcp::model::FormElicitationCapability::default()),
            );
            return Err(rmcp::ErrorData::missing_required_client_capability(
                required,
            ));
        }
        Ok(result)
    }

    #[cfg(any(unix, windows))]
    async fn update_task(
        &self,
        request: rmcp::model::UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        let server = task_request_server(self, &context, &request.task_id, "update")?;
        crate::mcp_tasks::update(&server, &request.task_id, &request.input_responses)
    }

    #[cfg(any(unix, windows))]
    async fn cancel_task(
        &self,
        request: rmcp::model::CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        let server = task_request_server(self, &context, &request.task_id, "cancel")?;
        crate::mcp_tasks::cancel(&server, &request.task_id).await
    }

    fn call_tool(
        &self,
        mut params: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::CallToolResponse, rmcp::ErrorData>> + Send + '_
    {
        async move {
            let mode = crate::mcp_peer::McpPeerMode::from_context(&context)?;
            reject_modern_direct_stdio(mode, &context)?;
            let request_server = match mode {
                crate::mcp_peer::McpPeerMode::Legacy => None,
                crate::mcp_peer::McpPeerMode::Modern20260728 => {
                    Some(self.clone_for_modern_request(&context, true)?)
                }
            };
            let server = request_server.as_ref().unwrap_or(self);
            let workspace_server = if context
                .extensions
                .get::<axum::http::request::Parts>()
                .is_some()
            {
                server.clone_for_workspace_call(&context.meta)?
            } else {
                None
            };
            let server = workspace_server.as_ref().unwrap_or(server);
            let owner_server = server.owner_tool_server(
                params.name.as_ref(),
                request_identity(Some(&context.meta), &context),
            )?;
            let server = owner_server.as_ref().unwrap_or(server);
            // Idle reaper: every tool call (including ones a stdio child
            // forwards to this daemon) counts as activity, so an idle daemon is
            // genuinely unused and safe to self-terminate.
            if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
                server.agent_runtime_write().presence_client_label = context
                    .meta
                    .client_info()
                    .map(|info| crate::claims_ops::sanitize_presence_field(&info.name, 160));
            }
            server.touch_activity();
            let name_owned = params.name.as_ref().to_string();
            let name = name_owned.as_str();
            let env_patterns = current_exposed_tool_patterns();

            let active_profile = server.active_tool_profile();
            let visible = tachi_hub::tool_visible(name, active_profile, env_patterns.as_deref());

            if !visible {
                return Ok(tool_not_found_result(name).into());
            }

            // F3 (#495/#913): action-level ToolProfile gate for facade tools.
            // Runs after tool_visible so delegate can list tachi_task while still
            // denying recursive dispatch.
            let action_arg: Option<String> = params
                .arguments
                .as_ref()
                .and_then(|args| args.get("action"))
                .and_then(|value| value.as_str())
                .map(|s| s.to_string());
            if !tachi_hub::facade_action_allowed(name, action_arg.as_deref(), active_profile) {
                let profile_label = active_profile
                    .map(|p| p.as_str())
                    .unwrap_or_else(|| "standard".to_string());
                let action_label = action_arg.as_deref().unwrap_or("");
                return Ok(tool_action_denied_result(name, action_label, &profile_label).into());
            }

            let roots_server = if server.tool_router.has_route(name)
                && context
                    .extensions
                    .get::<axum::http::request::Parts>()
                    .is_some()
            {
                match server.clone_for_roots_call(&params, &context, mode).await? {
                    std::ops::ControlFlow::Continue(server) => server,
                    std::ops::ControlFlow::Break(input) => return Ok(input.into()),
                }
            } else {
                None
            };
            let server = roots_server.as_ref().unwrap_or(server);
            let bound_project = server.session_project();
            if let Some(project) = bound_project.as_deref() {
                crate::session_identity::enforce_server_session_project(
                    server,
                    name,
                    &mut params.arguments,
                    project,
                    "HTTP direct-connect",
                    // #1041 B1: the daemon's own call_tool is the sole
                    // authoritative hop — it, and only it, may inject a
                    // default `project` for an absent one.
                    crate::session_identity::EnforcementRole::Authoritative,
                )?;
            } else {
                crate::session_identity::stamp_unbound_project_explicit_marker(
                    name,
                    &mut params.arguments,
                );
            }
            // C1 fix (fail-closed): an unbound HTTP direct-connect session has no
            // declared tenant, so an explicit `project=` on a mutating tool is a
            // potential cross-tenant write and must be rejected. Bound sessions
            // (including the stdio proxy, which forwards X-Tachi-Project) pass the
            // bound_project check above and are not affected.
            crate::session_identity::reject_unbound_cross_project_write(
                name,
                &params.arguments,
                bound_project.as_deref(),
                "HTTP direct-connect",
            )?;

            // Serialize normalized arguments once, then drop the JSON before dispatch.
            let serialized_args = params.arguments.as_ref().map(serde_json::to_string);
            // ─── Rate Limiter: throttle and loop detection ───────────────
            let stuck_warning: Option<String> = {
                let args_hash = serialized_args
                    .as_ref()
                    .map(|args| stable_hash(args.as_deref().unwrap_or_default()))
                    .unwrap_or_default();
                // #1255: `clone_for_mcp_session` stamps a unique opaque id on
                // each MCP session clone; `check_session_rate_limit` keys burst
                // windows by that id so sessions sharing the process-global
                // RateLimiter do not inherit each other's counters. Audit C1:
                // that id is re-keyed to a stable bucket (bucket header or
                // modern identity) in `apply_resolved_request_identity` /
                // `clone_for_modern_request`, so short-lived proxy sessions
                // and modern requests still accumulate one window per client.
                server.check_session_rate_limit_for_call(
                    name,
                    &args_hash,
                    params.arguments.as_ref(),
                )?
            };

            #[cfg(any(unix, windows))]
            if mode == crate::mcp_peer::McpPeerMode::Modern20260728
                && context
                    .client_capabilities()
                    .is_some_and(|caps| caps.supports_tasks())
                && name == "tachi_staff"
                && action_arg
                    .as_deref()
                    .is_some_and(|action| action.trim().eq_ignore_ascii_case("result"))
                && params
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("format"))
                    .and_then(serde_json::Value::as_str)
                    == Some("json")
            {
                return crate::mcp_tasks::begin_result(server, &params);
            }

            // ─── Phantom Tools: cache invalidation on write ops ──────────
            if CACHE_INVALIDATING_TOOLS.contains(&name) {
                server.tool_cache_lock().clear();
            }

            // ─── Phantom Tools: check cache for read-only tools ──────────
            let is_cacheable = CACHEABLE_TOOLS.contains(&name);
            let cache_key = if is_cacheable {
                let args_str = match serialized_args.as_ref() {
                    Some(Ok(args)) => args.as_str(),
                    Some(Err(e)) => {
                        eprintln!(
                            "[call_tool] WARNING: failed to serialize arguments for cache key (tool='{}'): {e}",
                            name
                        );
                        ""
                    }
                    None => "",
                };
                let key = stable_hash(&format!("{}{}", name, args_str));

                // Check cache
                let cached_hit = {
                    let cache = server.tool_cache_lock();
                    if let Some(cached) = cache.get(&key) {
                        if cached.created_at.elapsed() < TOOL_CACHE_TTL {
                            Some(cached.result.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };
                if let Some(mut hit) = cached_hit {
                    server
                        .cache_hits
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if let Some(warn) = stuck_warning.clone() {
                        hit.content.push(rmcp::model::ContentBlock::text(warn));
                    }
                    return Ok(hit.into());
                }
                server
                    .cache_misses
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(key)
            } else {
                None
            };

            drop(serialized_args);
            // ─── Dispatch to handler ─────────────────────────────────────
            // Save tool name and arguments for DLQ capture on failure
            let tool_name_owned = name.to_string();
            let tool_args_for_dlq = params.arguments.clone();

            let mut result = {
                // 1. Native tools first (highest priority)
                if server.tool_router.has_route(name) {
                    let context =
                        rmcp::handler::server::tool::ToolCallContext::new(server, params, context);
                    match server.tool_router.call(context).await {
                        Ok(rmcp::model::CallToolResponse::Complete(result)) => Ok(result),
                        Ok(_) => Err(rmcp::ErrorData::internal_error(
                            "Tachi tools require a complete synchronous result",
                            None,
                        )),
                        Err(error) => Err(error),
                    }
                }
                // 2. Skill tools (tachi_skill_*)
                else if lock_or_recover(&server.tool_discovery.skill_tools, "skill_tools")
                    .contains_key(name)
                {
                    server.call_skill_tool(name, params.arguments).await
                } else {
                    Ok(tool_not_found_result(name))
                }
            };

            // ─── Dead Letter Queue: capture failures ─────────────────────
            if let Err(ref err) = result {
                let is_native = server.tool_router.has_route(&tool_name_owned);
                if should_enqueue_dlq(&tool_name_owned, tool_args_for_dlq.as_ref(), is_native) {
                    let error_str = format!("{}", err);
                    let category = categorize_error(&error_str);

                    let dl = DeadLetter {
                        id: uuid::Uuid::new_v4().to_string(),
                        tool_name: tool_name_owned.clone(),
                        arguments: tool_args_for_dlq.clone(),
                        error: error_str.clone(),
                        error_category: category,
                        timestamp: Utc::now().to_rfc3339(),
                        retry_count: 0,
                        max_retries: 3,
                        status: "pending".to_string(),
                    };

                    {
                        let mut dlq = server.dead_letters_lock();
                        push_dead_letter_with_limits(&mut dlq, dl, Utc::now());
                    }
                } else {
                    let error_category = categorize_error(&err.to_string());
                    let reason = dlq_capture_refusal_reason(&tool_name_owned, is_native);
                    if let Err(signal_error) = record_dlq_capture_refused(
                        server,
                        &tool_name_owned,
                        action_arg.as_deref(),
                        reason,
                        is_native,
                        &error_category,
                        bound_project.as_deref(),
                    ) {
                        if let Err(error) = &mut result {
                            attach_dlq_capture_refusal_fallback(
                                error,
                                &tool_name_owned,
                                action_arg.as_deref(),
                                reason,
                                &signal_error,
                            );
                        }
                    }
                }
            }

            attach_failed_invocation_guidance(
                &mut result,
                &tool_name_owned,
                tool_args_for_dlq.as_ref(),
            );

            // ─── Phantom Tools: store result in cache ────────────────────
            if let (Some(key), Ok(ref res)) = (&cache_key, &result) {
                if tool_result_can_be_cached(res) {
                    let mut cache = server.tool_cache_lock();
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
                    tool_result
                        .content
                        .push(rmcp::model::ContentBlock::text(warn));
                    Ok(tool_result)
                }
                (other, _) => other,
            };

            result.map(|mut result| {
                // Native constructors already carry this discriminator, but a
                // directly exposed legacy MCP upstream does not. Hydrate at
                // the final modern HTTP boundary after every dispatch path.
                if mode == crate::mcp_peer::McpPeerMode::Modern20260728 {
                    result
                        .result_type
                        .get_or_insert(rmcp::model::ResultType::COMPLETE);
                }
                result.into()
            })
        }
    }
}

#[cfg(any(unix, windows))]
fn task_request_server(
    server: &MemoryServer,
    context: &RequestContext<RoleServer>,
    task_id: &str,
    action: &str,
) -> Result<MemoryServer, rmcp::ErrorData> {
    let mode = crate::mcp_peer::McpPeerMode::from_context(context)?;
    if mode != crate::mcp_peer::McpPeerMode::Modern20260728 {
        return Err(rmcp::ErrorData::invalid_request(
            "Tasks requires MCP 2026-07-28 request metadata",
            None,
        ));
    }
    reject_modern_direct_stdio(mode, context)?;
    if !context
        .client_capabilities()
        .is_some_and(|caps| caps.supports_tasks())
    {
        return Err(rmcp::ErrorData::missing_required_client_capability(
            rmcp::model::ClientCapabilities::builder()
                .enable_tasks()
                .build(),
        ));
    }
    let server = server.clone_for_modern_request(context, false)?;
    server.touch_activity();
    // Reuse the canonical read-only polling classification only for a valid
    // tasks/get handle. Updates, cancellation and malformed handles remain
    // subject to identical-call detection; all methods retain the RPM budget.
    let (action, dispatch_id) = match (action, crate::mcp_tasks::dispatch_from_task(task_id)) {
        ("status", Ok(dispatch_id)) => ("status", dispatch_id),
        ("status", Err(_)) => ("tasks/get", task_id),
        _ => (action, task_id),
    };
    let args = serde_json::json!({"action":action,"dispatch_id":dispatch_id});
    server.check_session_rate_limit_for_call(
        "tachi_staff",
        &stable_hash(&args.to_string()),
        args.as_object(),
    )?;
    Ok(server)
}

const SKILL_TTL_MS: u64 = 300_000;

fn admit_modern_skill_request(context: &RequestContext<RoleServer>) -> Result<(), rmcp::ErrorData> {
    let mode = crate::mcp_peer::McpPeerMode::from_context(context)?;
    if mode != crate::mcp_peer::McpPeerMode::Modern20260728 {
        return Err(rmcp::ErrorData::invalid_request(
            "Skills requires MCP 2026-07-28 per-request metadata",
            None,
        ));
    }
    reject_modern_direct_stdio(mode, context)?;
    validate_modern_identity_headers(context)
}

fn tachi_skill() -> Result<rmcp::model::Skill, rmcp::ErrorData> {
    serde_json::from_value(crate::server_instructions::skill_manifest().clone()).map_err(|error| {
        rmcp::ErrorData::internal_error(format!("bundled skill manifest: {error}"), None)
    })
}
