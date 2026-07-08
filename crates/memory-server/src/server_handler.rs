use crate::mcp_proxy::{
    filter_mcp_tools_by_permissions, resolve_mcp_tool_exposure, McpToolExposureMode,
};
use crate::server_state::{
    CachedResult, MemoryServer, CACHEABLE_TOOLS, CACHE_INVALIDATING_TOOLS, TOOL_CACHE_MAX_ENTRIES,
    TOOL_CACHE_TTL,
};
use crate::shared_defs::{
    categorize_error, push_dead_letter_with_limits, should_enqueue_dlq, DeadLetter,
};
use crate::utils::{lock_or_recover, stable_hash};
use chrono::Utc;
use rmcp::model::{InitializeRequestParams, InitializeResult, ServerCapabilities, ServerInfo};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServerHandler;
use std::future::Future;
use std::time::{Duration, Instant};
use tachi_hub::should_expose_mcp_tools;

pub(crate) fn current_exposed_tool_patterns() -> Option<Vec<String>> {
    static CACHED: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| {
            std::env::var("TACHI_EXPOSED_TOOLS")
                .ok()
                .map(|raw| tachi_hub::parse_tool_patterns_csv(&raw))
                .filter(|patterns| !patterns.is_empty())
        })
        .clone()
}

fn tool_not_found_result(tool_name: &str) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::error(vec![rmcp::model::Content::text(format!(
        "tool not found: '{tool_name}'. Call tachi_tools() or tools/list and use an exact visible tool name for the current TACHI_PROFILE."
    ))])
}

pub(crate) fn split_proxy_tool_name<'a>(
    name: &str,
    server_names: impl Iterator<Item = &'a str>,
) -> Option<(String, String)> {
    let mut best_match: Option<(String, String)> = None;
    for server_name in server_names {
        let prefix = format!("{server_name}__");
        let Some(remote_tool) = name.strip_prefix(&prefix) else {
            continue;
        };
        if remote_tool.is_empty() {
            continue;
        }
        if best_match
            .as_ref()
            .is_none_or(|(matched, _)| server_name.len() > matched.len())
        {
            best_match = Some((server_name.to_string(), remote_tool.to_string()));
        }
    }

    best_match.or_else(|| {
        name.split_once("__")
            .filter(|(server_name, remote_tool)| !server_name.is_empty() && !remote_tool.is_empty())
            .map(|(server_name, remote_tool)| (server_name.to_string(), remote_tool.to_string()))
    })
}

fn tool_result_can_be_cached(result: &rmcp::model::CallToolResult) -> bool {
    !result.is_error.unwrap_or(false)
}

fn annotate_tool(tool: &mut rmcp::model::Tool) {
    use rmcp::model::ToolAnnotations;

    let name = tool.name.as_ref();
    let read_only = matches!(
        name,
        "runtime_info"
            | "tachi_status"
            | "tachi_tools"
            | "tachi_briefing"
            | "tachi_component"
            | "tachi_web_search"
            | "vault_status"
    );
    let destructive = matches!(
        name,
        "delete_memory"
            | "archive_memory"
            | "memory_gc"
            | "tachi_task"
            | "tachi_shell"
            | "tachi_orchestrator"
            | "tachi_arena"
            | "tachi_verify"
            | "tachi_workflow"
            | "tachi_gh"
    );
    let idempotent = matches!(
        name,
        "runtime_info" | "tachi_status" | "tachi_tools" | "vault_status"
    );
    let open_world = matches!(
        name,
        "tachi_web_search" | "tachi_gh" | "hub_call" | "hub_discover"
    );

    let existing = tool.annotations.take().unwrap_or_default();
    let mut annotations = ToolAnnotations::default();
    annotations.title = existing.title;
    annotations.read_only_hint = existing.read_only_hint.or(Some(read_only));
    annotations.destructive_hint = existing.destructive_hint.or(Some(destructive));
    annotations.idempotent_hint = existing.idempotent_hint.or(Some(idempotent));
    annotations.open_world_hint = existing.open_world_hint.or(Some(open_world));
    tool.annotations = Some(annotations);
}

#[derive(Debug, Default)]
struct HttpSessionIdentity {
    profile: Option<String>,
    client: Option<String>,
    project: Option<String>,
}

impl MemoryServer {
    fn apply_http_session_identity(
        &self,
        request: &InitializeRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        let identity = http_session_identity(request, context);
        let profile = identity
            .profile
            .as_deref()
            .map(parse_http_tool_profile)
            .transpose()?;
        if let Some(project) = identity.project.as_deref() {
            Self::resolve_named_project_db_path(project).map_err(|err| {
                rmcp::ErrorData::invalid_params(
                    format!("invalid HTTP direct-connect project binding: {err}"),
                    None,
                )
            })?;
        }
        self.set_session_identity(identity.client, identity.project, profile);
        Ok(())
    }
}

fn http_session_identity(
    request: &InitializeRequestParams,
    context: &RequestContext<RoleServer>,
) -> HttpSessionIdentity {
    let mut identity = HttpSessionIdentity::default();
    if let Some(meta) = request.meta.as_ref() {
        identity.profile = meta_string(meta, crate::session_identity::META_PROFILE)
            .or_else(|| meta_string(meta, "tachi.profile"));
        identity.client = meta_string(meta, crate::session_identity::META_CLIENT)
            .or_else(|| meta_string(meta, "tachi.client"));
        identity.project = meta_string(meta, crate::session_identity::META_PROJECT)
            .or_else(|| meta_string(meta, "tachi.project"));
    }
    if let Some(parts) = context.extensions.get::<axum::http::request::Parts>() {
        identity.profile =
            header_string(parts, crate::session_identity::HEADER_PROFILE).or(identity.profile);
        identity.client =
            header_string(parts, crate::session_identity::HEADER_CLIENT).or(identity.client);
        identity.project =
            header_string(parts, crate::session_identity::HEADER_PROJECT).or(identity.project);
    }
    identity
}

fn meta_string(meta: &rmcp::model::Meta, key: &str) -> Option<String> {
    meta.0
        .get(key)
        .and_then(|value| value.as_str())
        .and_then(crate::session_identity::normalize_identity_value)
}

fn header_string(parts: &axum::http::request::Parts, name: &str) -> Option<String> {
    parts
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(crate::session_identity::normalize_identity_value)
}

fn parse_http_tool_profile(raw: &str) -> Result<tachi_hub::ToolProfile, rmcp::ErrorData> {
    let profile = tachi_hub::parse_tool_profile(raw).ok_or_else(|| {
        rmcp::ErrorData::invalid_params(
            format!(
                "unknown HTTP direct-connect Tachi profile '{raw}'; expected standard, delegate, observe, remember, coordinate, operate, or a host alias"
            ),
            None,
        )
    })?;
    if profile.as_str() == "admin" {
        return Err(rmcp::ErrorData::invalid_params(
            "HTTP direct-connect profile 'admin' requires explicit authorization; #495 must wire profile claims to an authorization policy before admin can be accepted over HTTP",
            None,
        ));
    }
    Ok(profile)
}

impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(crate::server_instructions::mcp_server_instructions())
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, rmcp::ErrorData>> + Send + '_ {
        async move {
            self.apply_http_session_identity(&request, &context)?;
            context.peer.set_peer_info(request);
            Ok(self.get_info())
        }
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
            let proxy_snapshot =
                lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools").clone();
            let mcp_tool_exposure_mode = self.tool_discovery.mcp_tool_exposure_mode;
            let skill_tool_defs_snapshot =
                lock_or_recover(&self.tool_discovery.skill_tool_defs, "skill_tool_defs").clone();

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
                let exposure_mode = resolve_mcp_tool_exposure(&cap_def, mcp_tool_exposure_mode);
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
            if mcp_tool_exposure_mode != McpToolExposureMode::Gateway {
                tools.extend(skill_tool_defs_snapshot.values().cloned());
            }

            let env_patterns = current_exposed_tool_patterns();
            tools = tachi_hub::filter_tool_defs(
                tools,
                self.active_tool_profile(),
                env_patterns.as_deref(),
            );
            for tool in &mut tools {
                annotate_tool(tool);
            }

            Ok(rmcp::model::ListToolsResult {
                tools,
                ..Default::default()
            })
        }
    }

    fn call_tool(
        &self,
        mut params: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::CallToolResult, rmcp::ErrorData>> + Send + '_
    {
        async move {
            // Idle reaper: every tool call (including ones a stdio child
            // forwards to this daemon) counts as activity, so an idle daemon is
            // genuinely unused and safe to self-terminate.
            self.touch_activity();
            let name_owned = params.name.as_ref().to_string();
            let name = name_owned.as_str();
            let env_patterns = current_exposed_tool_patterns();

            let visible =
                tachi_hub::tool_visible(name, self.active_tool_profile(), env_patterns.as_deref());

            if !visible {
                return Ok(tool_not_found_result(name));
            }

            let bound_project = self.session_project();
            if let Some(project) = bound_project.as_deref() {
                crate::session_identity::enforce_session_project(
                    name,
                    &mut params.arguments,
                    project,
                    "HTTP direct-connect",
                )?;
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
                self.tool_cache_lock().clear();
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
                let cached_hit = {
                    let cache = self.tool_cache_lock();
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
                    self.cache_hits
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if let Some(warn) = stuck_warning.clone() {
                        hit.content.push(rmcp::model::Content::text(warn));
                    }
                    return Ok(hit);
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
                else if lock_or_recover(&self.tool_discovery.skill_tools, "skill_tools")
                    .contains_key(name)
                {
                    let exposure = self.tool_discovery.mcp_tool_exposure_mode;
                    if exposure == McpToolExposureMode::Gateway {
                        Err(rmcp::ErrorData::invalid_params(
                            "Direct skill tools are disabled for gateway mode; use tachi_skill(action='run')"
                                .to_string(),
                            None,
                        ))
                    } else {
                        self.call_skill_tool(name, params.arguments).await
                    }
                }
                // 3. Proxy tools (server__tool pattern)
                else if let Some((server_name, tool_name)) = {
                    let proxy_tools =
                        lock_or_recover(&self.tool_discovery.proxy_tools, "proxy_tools");
                    split_proxy_tool_name(name, proxy_tools.keys().map(String::as_str))
                } {
                    let exposure_mode = self.proxy_tool_exposure_mode_for_server(&server_name)?;
                    if exposure_mode == McpToolExposureMode::Gateway {
                        Err(rmcp::ErrorData::invalid_params(
                            format!(
                                "Direct proxy tools are disabled for '{}'; use hub_call(server_id='mcp:{}', tool_name='{}')",
                                server_name, server_name, tool_name
                            ),
                            None,
                        ))
                    } else {
                        self.proxy_call_internal(&server_name, &tool_name, params.arguments)
                            .await
                    }
                } else {
                    Ok(tool_not_found_result(name))
                }
            };

            // ─── Dead Letter Queue: capture failures ─────────────────────
            if let Err(ref err) = result {
                let is_native = self.tool_router.has_route(&tool_name_owned);
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
                        let mut dlq = self.dead_letters_lock();
                        push_dead_letter_with_limits(&mut dlq, dl, Utc::now());
                    }
                }
            }

            // ─── Phantom Tools: store result in cache ────────────────────
            if let (Some(key), Ok(ref res)) = (&cache_key, &result) {
                if tool_result_can_be_cached(res) {
                    let mut cache = self.tool_cache_lock();
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_error_results_are_not_cacheable() {
        let error = rmcp::model::CallToolResult::error(vec![rmcp::model::Content::text("boom")]);
        assert!(!tool_result_can_be_cached(&error));

        let ok: rmcp::model::CallToolResult = serde_json::from_value(json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": false
        }))
        .expect("tool result");
        assert!(tool_result_can_be_cached(&ok));
    }
}
