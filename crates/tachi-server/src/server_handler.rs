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

/// F3 (#495/#913): action denied by ToolProfile (distinct from tool-not-found).
fn tool_action_denied_result(
    tool_name: &str,
    action: &str,
    profile_label: &str,
) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::error(vec![rmcp::model::Content::text(format!(
        "action '{action}' on tool '{tool_name}' is not allowed for ToolProfile '{profile_label}'. Use a permitted action for this profile, or call tachi_tools() to inspect the active surface."
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
            // peer_query is structurally read-only (PeerPublicationRead,
            // query_only connection — #1016 S1); the hint must say so.
            | "peer_query"
    );
    let destructive = matches!(
        name,
        "archive_memory"
            // #757-fold fix (gpt-5.6-terra review): `delete_memory`/
            // `memory_gc` were destructive on main; folding them into
            // `tachi_memory(action='delete'|'gc')` dropped the tool off this
            // list entirely (falling to the `false` default below), which
            // fails open on the MCP destructive_hint. `tachi_task` is
            // already annotated destructive wholesale despite having
            // read-only actions (status/plan/board/...) — same tool-level
            // (not action-aware) precedent applies here.
            | "tachi_memory"
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
        "tachi_web_search" | "tachi_research" | "tachi_gh" | "hub_call" | "hub_discover"
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

/// #919 CONCERN: the `tachi_task` MCP schema advertises every primary action
/// (including `dispatch`/`recommend`/`merge`) regardless of profile, even
/// though the F3 action-policy gate (`facade_action_allowed`) denies those to
/// a delegate worker at call time. Advertising a capability the gate then
/// denies is an unnecessary info-leak/confusion surface, so for a delegate
/// session intersect the advertised `action` enum with what the SAME gate
/// (single source of truth — no separate hardcoded list to drift) actually
/// allows. Read-only: only narrows the schema, never widens it beyond what
/// `TACHI_TASK_PRIMARY_ACTIONS` already declares.
fn narrow_gated_action_schemas(
    tools: &mut [rmcp::model::Tool],
    profile: Option<tachi_hub::ToolProfile>,
) {
    let profile = profile.unwrap_or_else(tachi_hub::default_tool_profile);
    if profile.as_str() != "delegate" {
        return;
    }
    for tool in tools.iter_mut() {
        if tool.name.as_ref() != "tachi_task" {
            continue;
        }
        let allowed: Vec<&str> = tachi_params::TACHI_TASK_PRIMARY_ACTIONS
            .iter()
            .copied()
            .filter(|action| {
                tachi_hub::facade_action_allowed("tachi_task", Some(action), Some(profile))
            })
            .collect();
        narrow_action_enum_property(tool, &allowed);
    }
}

/// Rewrite the `properties.action.enum` array of a tool's input schema to
/// `allowed`, if that property/shape is present. No-op for tools whose
/// schema doesn't have the expected `{properties: {action: {enum: [...]}}}`
/// shape (defensive — a schema change elsewhere should never panic list_tools).
fn narrow_action_enum_property(tool: &mut rmcp::model::Tool, allowed: &[&str]) {
    let mut schema = (*tool.input_schema).clone();
    let Some(action_prop) = schema
        .get_mut("properties")
        .and_then(|p| p.as_object_mut())
        .and_then(|props| props.get_mut("action"))
        .and_then(|a| a.as_object_mut())
    else {
        return;
    };
    action_prop.insert(
        "enum".to_string(),
        serde_json::Value::Array(
            allowed
                .iter()
                .map(|a| serde_json::Value::String((*a).to_string()))
                .collect(),
        ),
    );
    tool.input_schema = std::sync::Arc::new(schema);
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
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
    let mut identity = identity_from_initialize_meta(request.meta.as_ref());
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

/// Extract session identity fields from MCP initialize `_meta` (#732).
/// Headers still win when both are present (applied after this helper).
fn identity_from_initialize_meta(meta: Option<&rmcp::model::Meta>) -> HttpSessionIdentity {
    let mut identity = HttpSessionIdentity::default();
    let Some(meta) = meta else {
        return identity;
    };
    identity.profile = meta_string(meta, crate::session_identity::META_PROFILE)
        .or_else(|| meta_string(meta, "tachi.profile"));
    identity.client = meta_string(meta, crate::session_identity::META_CLIENT)
        .or_else(|| meta_string(meta, "tachi.client"));
    identity.project = meta_string(meta, crate::session_identity::META_PROJECT)
        .or_else(|| meta_string(meta, "tachi.project"));
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
            narrow_gated_action_schemas(&mut tools, self.active_tool_profile());
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

            let active_profile = self.active_tool_profile();
            let visible = tachi_hub::tool_visible(name, active_profile, env_patterns.as_deref());

            if !visible {
                return Ok(tool_not_found_result(name));
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
                return Ok(tool_action_denied_result(
                    name,
                    action_label,
                    &profile_label,
                ));
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

            // #757-fold fix (gpt-5.6-terra review, CONCERN): `tachi_doctor_scan`
            // was in CACHEABLE_TOOLS pre-fold (read-only). Folding it into
            // `tachi_memory(action='doctor_scan')` moved it under the
            // tool-name-level `tachi_memory` entry in CACHE_INVALIDATING_TOOLS
            // (needed because every OTHER tachi_memory action is a genuine
            // read/write mix), which would invalidate the whole tool cache —
            // including unrelated cached reads from other tools — on every
            // doctor_scan call. The cache key already hashes the full
            // arguments (including `action`), so this facade's one read-only
            // action can be carved out precisely without touching the
            // mutating actions' invalidation.
            let is_memory_doctor_scan_read = name == "tachi_memory"
                && action_arg
                    .as_deref()
                    .map(|action| action.eq_ignore_ascii_case("doctor_scan"))
                    .unwrap_or(false);

            // ─── Phantom Tools: cache invalidation on write ops ──────────
            if CACHE_INVALIDATING_TOOLS.contains(&name) && !is_memory_doctor_scan_read {
                self.tool_cache_lock().clear();
            }

            // ─── Phantom Tools: check cache for read-only tools ──────────
            let is_cacheable = CACHEABLE_TOOLS.contains(&name) || is_memory_doctor_scan_read;
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
    fn peer_query_is_annotated_read_only() {
        // #1016 S1: peer_query answers through a structurally read-only
        // connection; the MCP read_only_hint must advertise that (terminal
        // review caught the allowlist omission).
        let mut tool = rmcp::model::Tool::new(
            std::borrow::Cow::Borrowed("peer_query"),
            std::borrow::Cow::Borrowed("peer publication read"),
            std::sync::Arc::new(serde_json::Map::new()),
        );
        annotate_tool(&mut tool);
        let ann = tool.annotations.expect("annotations set");
        assert_eq!(ann.read_only_hint, Some(true));
        assert_eq!(ann.destructive_hint, Some(false));
    }

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

    #[test]
    fn initialize_meta_binds_profile_client_and_project() {
        let mut map = serde_json::Map::new();
        map.insert(
            crate::session_identity::META_PROFILE.to_string(),
            json!("delegate"),
        );
        map.insert(
            crate::session_identity::META_CLIENT.to_string(),
            json!("meta-client"),
        );
        map.insert(
            crate::session_identity::META_PROJECT.to_string(),
            json!("Sigil-meta"),
        );
        let meta = rmcp::model::Meta(map);
        let identity = identity_from_initialize_meta(Some(&meta));
        assert_eq!(identity.profile.as_deref(), Some("delegate"));
        assert_eq!(identity.client.as_deref(), Some("meta-client"));
        assert_eq!(identity.project.as_deref(), Some("Sigil-meta"));
    }

    #[test]
    fn initialize_meta_accepts_dotted_aliases() {
        let mut map = serde_json::Map::new();
        map.insert("tachi.profile".to_string(), json!("observe"));
        map.insert("tachi.project".to_string(), json!("wiki"));
        let meta = rmcp::model::Meta(map);
        let identity = identity_from_initialize_meta(Some(&meta));
        assert_eq!(identity.profile.as_deref(), Some("observe"));
        assert_eq!(identity.project.as_deref(), Some("wiki"));
    }

    #[test]
    fn initialize_meta_none_is_empty_identity() {
        let identity = identity_from_initialize_meta(None);
        assert!(identity.profile.is_none());
        assert!(identity.client.is_none());
        assert!(identity.project.is_none());
    }

    /// #757-fold fix (gpt-5.6-terra review): `delete_memory`/`memory_gc` were
    /// destructive-annotated on main; the fold into `tachi_memory(action=
    /// 'delete'|'gc')` dropped `tachi_memory` off the destructive list
    /// entirely, so it fell through to `destructive_hint=false`. Assert the
    /// unified facade is annotated destructive again.
    #[test]
    fn tachi_memory_facade_is_annotated_destructive() {
        let mut tool: rmcp::model::Tool = serde_json::from_value(json!({
            "name": "tachi_memory",
            "description": "tool tachi_memory",
            "inputSchema": {
                "type": "object",
                "additionalProperties": true,
            }
        }))
        .expect("failed to build test tool");
        annotate_tool(&mut tool);
        assert_eq!(
            tool.annotations.expect("annotations set").destructive_hint,
            Some(true),
            "tachi_memory must be destructive_hint=true (fronts delete/gc, both destructive)"
        );
    }
}
