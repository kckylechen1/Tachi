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
use memcore::{AuthorityLevel, EffectScope, TachiEventRecord};
use rmcp::model::{
    InitializeRequestParams, InitializeResult, RequestMetaObject, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServerHandler;
use std::future::Future;
use std::time::{Duration, Instant};
use tachi_hub::should_expose_mcp_tools;

mod admission;
mod protocol;
pub(crate) use admission::parse_http_tool_profile;

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

pub(crate) fn tool_not_found_result(tool_name: &str) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(format!(
        "tool not found: '{tool_name}'. Call tools/list and use an exact visible tool name. Management operations use supported owner CLI commands."
    ))])
}

/// F3 (#495/#913): action denied by ToolProfile (distinct from tool-not-found).
fn tool_action_denied_result(
    tool_name: &str,
    action: &str,
    profile_label: &str,
) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(format!(
        "action '{action}' on tool '{tool_name}' is not allowed for ToolProfile '{profile_label}'. Use a permitted action from tools/list for this profile."
    ))])
}

fn dlq_capture_refusal_reason(tool_name: &str, is_native: bool) -> &'static str {
    if tool_name.starts_with("dlq_")
        || tool_name.starts_with("ghost_")
        || tool_name == "get_pipeline_status"
    {
        "excluded_control_or_status_route"
    } else if is_native {
        "native_route_not_generic_dlq_replayable"
    } else {
        "default_deny_no_explicit_safe_replay_authority"
    }
}

fn record_dlq_capture_refused(
    server: &MemoryServer,
    tool_name: &str,
    action: Option<&str>,
    reason: &str,
    is_native: bool,
    error_category: &str,
    project: Option<&str>,
) -> Result<(), String> {
    let event = TachiEventRecord {
        id: uuid::Uuid::new_v4().to_string(),
        source_repo: "tachi".to_string(),
        adapter: "server_handler.call_tool".to_string(),
        project: project.unwrap_or_default().to_string(),
        domain: "dlq".to_string(),
        session_id: server.rate_limit_session_id(),
        actor: "tachi-server".to_string(),
        event_type: "dlq_capture_refused".to_string(),
        authority: AuthorityLevel::ReviewSignalOnly,
        effects: vec![EffectScope::None],
        projection_hints: Vec::new(),
        payload: serde_json::json!({
            "tool_name": tool_name,
            "action": action,
            "reason": reason,
            "is_native": is_native,
            "error_category": error_category,
            "dlq_enqueued": false,
        }),
        provenance: serde_json::json!({
            "source": "server_handler.call_tool",
            "decision": "should_enqueue_dlq_refused",
        }),
        created_at: Utc::now().to_rfc3339(),
    };

    server.with_global_store(|store| {
        store
            .insert_tachi_event(&event)
            .map_err(|error| format!("insert dlq_capture_refused event: {error}"))
    })
}

fn attach_dlq_capture_refusal_fallback(
    error: &mut rmcp::ErrorData,
    tool_name: &str,
    action: Option<&str>,
    reason: &str,
    persistence_error: &str,
) {
    let original_error_data = error.data.take();
    error.data = Some(serde_json::json!({
        "original_error_data": original_error_data,
        "lifecycle_signal": {
            "event_type": "dlq_capture_refused",
            "tool_name": tool_name,
            "action": action,
            "reason": reason,
            "persistence_error": persistence_error,
        }
    }));
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

/// Preserve the handler's original error and add conservative recovery facts
/// **only for routes this server can classify locally**.
///
/// This covers both MCP error channels; neither channel proves rollback.
/// Attachment is gated on
/// [`crate::action_effect::local_effect_authority_established`]. An unknown or
/// proxy-qualified route has no local authority, so its `ErrorData` — including
/// a payload-supplied `tachi_invocation_guidance`, if any — is passed through
/// byte-for-byte and never becomes this server's authority; nothing is
/// annotated or wrapped for such a route.
///
/// For a *local* route (the only case that reaches the match below) the
/// guidance namespace is AUTHORED here, never trusted from the payload: a local
/// producer that pre-populates `tachi_invocation_guidance` cannot borrow this
/// server's replay authority, so its value is preserved under
/// `producer_supplied_guidance` while our computed guidance stays
/// authoritative. `code`, `message`, and every other `data` field (including a
/// canonical commit receipt) are left intact.
fn attach_failed_invocation_guidance(
    result: &mut Result<rmcp::model::CallToolResult, rmcp::ErrorData>,
    tool_name: &str,
    arguments: Option<&serde_json::Map<String, serde_json::Value>>,
) {
    if !crate::action_effect::local_effect_authority_established(tool_name, arguments) {
        return;
    }
    match result {
        Err(error) => {
            let guidance = crate::action_effect::failed_invocation_guidance(tool_name, arguments);
            let data = error.data.get_or_insert_with(|| serde_json::json!({}));
            if let Some(object) = data.as_object_mut() {
                // Never defer to a payload-supplied value under our own key.
                let mut guidance = guidance;
                if let Some(foreign) = object.remove("tachi_invocation_guidance") {
                    guidance["producer_supplied_guidance"] = foreign;
                }
                object.insert("tachi_invocation_guidance".to_string(), guidance);
            } else {
                let original = std::mem::replace(data, serde_json::Value::Null);
                *data = serde_json::json!({
                    "original_error_data": original,
                    "tachi_invocation_guidance": guidance,
                });
            }
        }
        Ok(response) if response.is_error.unwrap_or(false) => {
            let guidance = crate::action_effect::failed_invocation_guidance(tool_name, arguments);
            response.content.push(rmcp::model::ContentBlock::text(
                serde_json::json!({"tachi_invocation_guidance": guidance}).to_string(),
            ));
        }
        Ok(_) => {}
    }
}

fn annotate_tool(tool: &mut rmcp::model::Tool) {
    use rmcp::model::ToolAnnotations;

    let name = tool.name.as_ref();
    let read_only = matches!(
        name,
        "runtime_info"
            | "tachi_status"
            | "tachi_tools"
            | "tachi_component"
            | "tachi_web_search"
            // peer_query is structurally read-only (PeerPublicationRead,
            // query_only connection — #1016 S1); the hint must say so.
            | "peer_query"
    );
    let destructive = matches!(
        name,
        "archive_memory"
            | "tachi_task"
            | "tachi_staff"
            | "tachi_verify"
            // #757 Cut3-S1 round-2 (review fixup): `tachi_sandbox` folds
            // `sandbox_set_rule`/`sandbox_set_policy` (both destructive:true
            // in the alias manifest, see tools/alias_manifest.rs) alongside
            // three read-only actions. Same tool-level (not action-aware)
            // precedent as `tachi_memory`/`tachi_task` above: the MCP
            // destructive_hint is per-tool, not per-action, so the whole
            // verb is annotated destructive rather than fail open for its
            // mutating actions. S2+ direction (sol blueprint): derive
            // destructive_hint per-action from the manifest/action-policy
            // gate instead of this tool-level allowlist.
            | "tachi_sandbox"
    );
    let idempotent = matches!(name, "runtime_info" | "tachi_status" | "tachi_tools");
    let open_world = matches!(
        name,
        "tachi_web_search" | "tachi_research" | "hub_call" | "hub_discover"
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

/// Build the non-admin `tachi_task` description's action-naming clauses from
/// the `allowed` set already computed by the caller (the same list
/// `narrow_action_enum_property` uses for the schema enum), so the
/// description can never name an action the profile can't actually invoke.
/// [C1a review round 2]: the prior code carried one hand-written sentence for
/// every non-admin profile, which drifted from the real per-profile allow-list
/// the moment a narrower profile (e.g. delegate) trimmed `allowed` below what
/// the sentence claimed — the same "hand-written list vs. real gate" pattern
/// as the retired-action leak this file already fixed once in round 1. Only
/// mention a group when at least one of its actions survives the filter;
/// omitting an allowed action from the prose is fine (it is not a promise to
/// be exhaustive), naming a denied one is the bug this generates around.
fn describe_allowed_task_actions(allowed: &[&str]) -> String {
    let has = |action: &str| allowed.contains(&action);
    let mut clauses: Vec<String> = Vec::new();

    let context: Vec<&str> = ["brief", "status"].into_iter().filter(|a| has(a)).collect();
    if !context.is_empty() {
        clauses.push(format!("Use {} for context", context.join("/")));
    }

    let ledger_core: Vec<&str> = ["complete", "adjudicate", "board"]
        .into_iter()
        .filter(|a| has(a))
        .collect();
    let lifecycle: Vec<&str> = ["claim", "heartbeat", "handoff", "release"]
        .into_iter()
        .filter(|a| has(a))
        .collect();
    if !ledger_core.is_empty() || !lifecycle.is_empty() {
        let mut ledger_clause = String::new();
        if !ledger_core.is_empty() {
            ledger_clause.push_str(&ledger_core.join("/"));
        }
        if !lifecycle.is_empty() {
            if !ledger_clause.is_empty() {
                ledger_clause.push_str(" and ");
            }
            ledger_clause.push_str("lifecycle actions (");
            ledger_clause.push_str(&lifecycle.join("/"));
            ledger_clause.push(')');
        }
        ledger_clause.push_str(" for work-ledger state");
        clauses.push(ledger_clause);
    }

    if clauses.is_empty() {
        String::new()
    } else {
        format!(" {}.", clauses.join("; "))
    }
}

/// Intersect each gated facade's advertised action enum with the same policy
/// used at call time. This keeps projected schemas from teaching a caller an
/// action the server will deny. Admin retains the complete schema.
fn narrow_gated_action_schemas(
    tools: &mut [rmcp::model::Tool],
    profile: Option<tachi_hub::ToolProfile>,
) {
    let profile = profile.unwrap_or_else(tachi_hub::default_tool_profile);
    if profile.as_str() == "admin" {
        return;
    }
    for tool in tools.iter_mut() {
        match tool.name.as_ref() {
            "tachi_task" => {
                let allowed: Vec<&str> = tachi_params::TachiTaskAction::primary_wire_strings()
                    .iter()
                    .copied()
                    .filter(|action| {
                        tachi_hub::facade_action_allowed("tachi_task", Some(action), Some(profile))
                    })
                    .collect();
                narrow_action_enum_property(
                    tool,
                    &allowed,
                    "Required task memory, policy, or ledger action. Ordinary local delegation uses the host harness's native subagent. Recommendations are advisory and do not authorize an execution backend.",
                );
                hide_operator_dispatch_properties(tool);
                let action_summary = describe_allowed_task_actions(&allowed);
                tool.description = Some(std::borrow::Cow::Owned(format!(
                    "Task memory, policy, and ledger facade. Ordinary local delegation uses the host harness's native subagent.{action_summary}",
                )));
                if is_agent_facing_guide_profile(profile) {
                    append_action_param_guide(
                        tool,
                        tachi_params::render_action_param_guides(
                            tachi_params::TACHI_TASK_ACTION_NOTES,
                            &allowed,
                        ),
                    );
                }
            }
            "tachi_staff" => {
                let allowed: Vec<&str> = tachi_params::TACHI_STAFF_ACTIONS
                    .iter()
                    .copied()
                    .filter(|action| {
                        tachi_hub::facade_action_allowed("tachi_staff", Some(action), Some(profile))
                    })
                    .collect();
                narrow_action_enum_property(
                    tool,
                    &allowed,
                    // Trimmed to offset the `preflight` enum-value byte growth so
                    // the frozen profile input-schema budgets are not exceeded.
                    "Staffing action allowed by this profile.",
                );
                if is_agent_facing_guide_profile(profile) {
                    append_action_param_guide(
                        tool,
                        tachi_params::render_action_param_guides(
                            tachi_params::TACHI_STAFF_ACTION_NOTES,
                            &allowed,
                        ),
                    );
                }
            }
            "tachi_a2a" => {
                let allowed: Vec<&str> = tachi_params::TACHI_A2A_ACTIONS
                    .iter()
                    .copied()
                    .filter(|action| {
                        tachi_hub::facade_action_allowed("tachi_a2a", Some(action), Some(profile))
                    })
                    .collect();
                narrow_action_enum_property(
                    tool,
                    &allowed,
                    "Required same-host advisory-mailbox action allowed by the active profile.",
                );
                if !allowed.contains(&"respond") {
                    hide_a2a_respond_properties(tool);
                }
            }
            "tachi_memory" => {
                // The memory action enum is not profile-narrowed elsewhere, so
                // only append the compact action-parameter guide here (filtered
                // by the same allow-list used everywhere else). No enum or
                // property is removed — the guide is purely additive.
                let allowed: Vec<&str> = tachi_params::TACHI_MEMORY_ACTIONS
                    .iter()
                    .copied()
                    .filter(|action| {
                        tachi_hub::facade_action_allowed(
                            "tachi_memory",
                            Some(action),
                            Some(profile),
                        )
                    })
                    .collect();
                if is_agent_facing_guide_profile(profile) {
                    append_action_param_guide(
                        tool,
                        tachi_params::render_action_param_guides(
                            tachi_params::TACHI_MEMORY_ACTION_NOTES,
                            &allowed,
                        ),
                    );
                }
            }
            "tachi_agent_eval" => {
                if profile == tachi_hub::ToolProfile::standard() {
                    project_standard_eval_schema(tool);
                    continue;
                }
                seed_action_enum_property(tool, tachi_params::TACHI_AGENT_EVAL_ACTIONS);
                let allowed: Vec<&str> = tachi_params::TACHI_AGENT_EVAL_ACTIONS
                    .iter()
                    .copied()
                    .filter(|action| {
                        tachi_hub::facade_action_allowed(
                            "tachi_agent_eval",
                            Some(action),
                            Some(profile),
                        )
                    })
                    .collect();
                narrow_action_enum_property(
                    tool,
                    &allowed,
                    "Required agent-eval action allowed by the active profile.",
                );
            }
            "tachi_wiki" => {
                let allowed: Vec<&str> = tachi_params::TACHI_WIKI_ACTIONS
                    .iter()
                    .copied()
                    .filter(|action| {
                        tachi_hub::facade_action_allowed("tachi_wiki", Some(action), Some(profile))
                    })
                    .collect();
                narrow_action_enum_property(
                    tool,
                    &allowed,
                    "Required Tachi wiki facade action allowed by the active profile.",
                );
                if !allowed.contains(&"write") {
                    hide_wiki_write_properties(tool);
                    tool.description = Some(std::borrow::Cow::Owned(
                        "Stable, reusable knowledge base. action='search': look up lessons, patterns, and how-tos BEFORE debugging from scratch or reaching for web search — a prior lesson may already exist. action='browse': explore available categories. action='read': load a specific entry by path. WHEN: wiki for durable knowledge that helps future sessions (patterns, lessons, decisions, conventions). Use tachi_memory for session-specific facts (decisions, findings, commands for the current task). Pass project to target a named library.".to_string(),
                    ));
                }
            }
            _ => {}
        }
    }
}

/// Expose only the admitted native evaluation loop and its reachable payloads.
fn project_standard_eval_schema(tool: &mut rmcp::model::Tool) {
    const ACTIONS: &[&str] = &[
        "register",
        "observe",
        "adjudicate",
        "get",
        "candidate_projection",
    ];
    const FIELDS: &[&str] = &[
        "action",
        "register",
        "observe",
        "adjudicate",
        "get",
        "candidate_projection",
        "limit",
    ];
    let mut schema = (*tool.input_schema).clone();
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        schema.clear();
        schema.insert("not".to_string(), serde_json::json!({}));
        tool.input_schema = std::sync::Arc::new(schema);
        return;
    };
    properties.retain(|name, _| FIELDS.contains(&name.as_str()));
    if !properties.contains_key("action") {
        schema.clear();
        schema.insert("not".to_string(), serde_json::json!({}));
        tool.input_schema = std::sync::Arc::new(schema);
        return;
    }
    properties.insert("action".to_string(), serde_json::json!({
        "type": "string",
        "enum": ACTIONS,
        "description": "Native evaluation memory: register, observe, adjudicate, get, or read candidate_projection."
    }));
    if let Some(required) = schema
        .get_mut("required")
        .and_then(serde_json::Value::as_array_mut)
    {
        required.retain(|name| name.as_str().is_some_and(|name| FIELDS.contains(&name)));
    }

    fn refs(value: &serde_json::Value, found: &mut std::collections::BTreeSet<String>) {
        match value {
            serde_json::Value::Object(object) => {
                if let Some(name) = object
                    .get("$ref")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|reference| reference.strip_prefix("#/$defs/"))
                {
                    found.insert(name.to_string());
                }
                for child in object.values() {
                    refs(child, found);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    refs(child, found);
                }
            }
            _ => {}
        }
    }
    let mut reachable = std::collections::BTreeSet::new();
    refs(&schema["properties"], &mut reachable);
    if let Some(definitions) = schema.get("$defs").and_then(serde_json::Value::as_object) {
        let mut pending = reachable.iter().cloned().collect::<Vec<_>>();
        while let Some(name) = pending.pop() {
            if let Some(definition) = definitions.get(&name) {
                let mut nested = std::collections::BTreeSet::new();
                refs(definition, &mut nested);
                for reference in nested {
                    if reachable.insert(reference.clone()) {
                        pending.push(reference);
                    }
                }
            }
        }
    }
    if let Some(definitions) = schema
        .get_mut("$defs")
        .and_then(serde_json::Value::as_object_mut)
    {
        definitions.retain(|name, _| reachable.contains(name));
    }
    tool.input_schema = std::sync::Arc::new(schema);
}

fn hide_wiki_write_properties(tool: &mut rmcp::model::Tool) {
    let mut schema = (*tool.input_schema).clone();
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        schema.clear();
        schema.insert("not".to_string(), serde_json::json!({}));
        tool.input_schema = std::sync::Arc::new(schema);
        return;
    };
    for property in [
        "title",
        "text",
        "topic",
        "summary",
        "keywords",
        "entities",
        "importance",
        "scope",
        "metadata",
        "force",
        "references",
        "include_patterns",
        "pattern_query",
        "pattern_top_k",
    ] {
        properties.remove(property);
    }
    tool.input_schema = std::sync::Arc::new(schema);
}

fn hide_a2a_respond_properties(tool: &mut rmcp::model::Tool) {
    let mut schema = (*tool.input_schema).clone();
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        schema.clear();
        schema.insert("not".to_string(), serde_json::json!({}));
        tool.input_schema = std::sync::Arc::new(schema);
        return;
    };
    for property in [
        "recipient_agent_identity_id",
        "subject_ref",
        "text",
        "idempotency_key",
        "ttl_days",
    ] {
        properties.remove(property);
    }
    tool.input_schema = std::sync::Arc::new(schema);
}

/// Apply the production profile, action-schema, and annotation projection used
/// by `tools/list`. Keeping this as one pure transform lets architecture census
/// tests measure the model-facing definitions without duplicating runtime
/// policy.
pub(crate) fn project_tool_definitions(
    tools: Vec<rmcp::model::Tool>,
    profile: Option<tachi_hub::ToolProfile>,
    env_patterns: Option<&[String]>,
) -> Vec<rmcp::model::Tool> {
    let mut tools = tachi_hub::filter_tool_defs(tools, profile, env_patterns);
    narrow_gated_action_schemas(&mut tools, profile);
    for tool in &mut tools {
        annotate_tool(tool);
    }
    tools
}

/// Apply native-only schema preparation before proxy and skill definitions are
/// joined to the list. This is a separate stage because bound-project schema
/// annotations must not be added to third-party tools.
pub(crate) fn prepare_native_tool_definitions(
    mut tools: Vec<rmcp::model::Tool>,
) -> Vec<rmcp::model::Tool> {
    for tool in &mut tools {
        annotate_bound_project_schema(tool);
    }
    tools
}

/// Operator-owned worker launch is not an ordinary agent intent. Remove its
/// exclusive parameters from non-admin schemas and suppress stale launch
/// wording on shared fields that remain useful to recommend/wait/complete.
fn hide_operator_dispatch_properties(tool: &mut rmcp::model::Tool) {
    const OPERATOR_LAUNCH_PROPERTIES: &[&str] = &[
        "dispatch_reason",
        "env_id",
        "unmanaged_cwd",
        "skills",
        "context_query",
        "model",
        "permission_profile",
        "allowed_tools",
        "completion_predicate",
        "max_turns",
        "sandbox",
        "inject_tachi_mcp",
        "inject_hub_mcps",
        "command",
        "harness_transport",
        "harness_server_url",
        "credential_profiles",
        "mcp_access",
        "allowed_mcp_servers",
        "inject_card",
    ];
    const OPERATOR_LAUNCH_DEFINITIONS: &[&str] = &[
        "CompletionPredicate",
        "DispatchMcpAccessParams",
        "TachiDispatchReason",
    ];

    let mut schema = (*tool.input_schema).clone();
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        schema.clear();
        schema.insert("not".to_string(), serde_json::json!({}));
        tool.input_schema = std::sync::Arc::new(schema);
        return;
    };
    for property in OPERATOR_LAUNCH_PROPERTIES {
        properties.remove(*property);
    }
    for property in properties.values_mut() {
        let Some(property) = property.as_object_mut() else {
            continue;
        };
        let advertises_launch = property
            .get("description")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|description| description.to_ascii_lowercase().contains("dispatch"));
        if advertises_launch {
            property.remove("description");
        }
    }
    if let Some(cwd) = properties
        .get_mut("cwd")
        .and_then(serde_json::Value::as_object_mut)
    {
        cwd.insert(
            "description".to_string(),
            serde_json::Value::String(
                "[action=brief] Workspace root used to resolve relative canonical document paths."
                    .to_string(),
            ),
        );
    }
    for definitions_key in ["$defs", "definitions"] {
        let Some(definitions) = schema
            .get_mut(definitions_key)
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };
        for definition in OPERATOR_LAUNCH_DEFINITIONS {
            definitions.remove(*definition);
        }
    }
    tool.input_schema = std::sync::Arc::new(schema);
}

/// The compact action-parameter guide is an agent-facing product-facade polish
/// for ordinary Agent and retained coordinate/delegate restrictions. Internal
/// owner management keeps its existing schema without the agent-facing guide.
fn is_agent_facing_guide_profile(profile: tachi_hub::ToolProfile) -> bool {
    profile == tachi_hub::ToolProfile::standard()
        || profile == tachi_hub::ToolProfile::coordinate()
        || profile == tachi_hub::ToolProfile::delegate()
}

/// P3 (agent-facing action input cost): append the compact action-parameter
/// guide (per-action required/optional/defaults + a minimal example) to the
/// facade's TOOL description. `guide` is already filtered to the
/// visible/allow-listed actions, so a denied action is never taught.
///
/// Deliberately NOT the `action` property description: the `input_schema`
/// surfaces are frozen byte budgets, and tools/list shows the tool description
/// to the model anyway. Growth is offset by consolidating the facades' verbose
/// descriptions (see `tachi_staff`); no property, enum value, or
/// default is changed or removed.
fn append_action_param_guide(tool: &mut rmcp::model::Tool, guide: Option<String>) {
    let Some(guide) = guide else {
        return;
    };
    let base = tool.description.as_deref().unwrap_or("").trim_end();
    let description = if base.is_empty() {
        format!("By action: {guide}")
    } else {
        format!("{base} By action: {guide}")
    };
    tool.description = Some(std::borrow::Cow::Owned(description));
}

fn seed_action_enum_property(tool: &mut rmcp::model::Tool, actions: &[&str]) {
    let mut schema = (*tool.input_schema).clone();
    if let Some(action) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|properties| properties.get_mut("action"))
        .and_then(serde_json::Value::as_object_mut)
    {
        action.insert("type".to_string(), serde_json::json!("string"));
        action.insert("enum".to_string(), serde_json::json!(actions));
    }
    tool.input_schema = std::sync::Arc::new(schema);
}

/// Intersect the existing `properties.action.enum` with `allowed`. An absent
/// enum fails closed to an empty set rather than advertising an action that
/// runtime policy rejects.
fn narrow_action_enum_property(tool: &mut rmcp::model::Tool, allowed: &[&str], description: &str) {
    let mut schema = (*tool.input_schema).clone();
    let referenced_values = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .and_then(|properties| properties.get("action"))
        .and_then(serde_json::Value::as_object)
        .and_then(|action| action.get("$ref"))
        .and_then(serde_json::Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        .and_then(|name| schema.get("$defs")?.get(name))
        .and_then(|definition| definition.get("enum"))
        .cloned();
    let Some(action_prop) = schema
        .get_mut("properties")
        .and_then(|p| p.as_object_mut())
        .and_then(|props| props.get_mut("action"))
        .and_then(|a| a.as_object_mut())
    else {
        schema.clear();
        schema.insert("not".to_string(), serde_json::json!({}));
        tool.input_schema = std::sync::Arc::new(schema);
        return;
    };
    // schemars may emit a small enum directly or behind a local `$defs`
    // reference. Inline the latter before filtering so the projected wire is
    // self-contained and the same helper covers every gated facade.
    if !action_prop.contains_key("enum") {
        action_prop.remove("$ref");
        if let Some(values) = referenced_values {
            action_prop.insert("enum".to_string(), values);
            action_prop.insert("type".to_string(), serde_json::json!("string"));
        }
    }
    let values = action_prop
        .entry("enum")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if let Some(values) = values.as_array_mut() {
        values.retain(|value| {
            value
                .as_str()
                .is_some_and(|action| allowed.contains(&action))
        });
    } else {
        *values = serde_json::Value::Array(Vec::new());
    }
    action_prop.insert(
        "description".to_string(),
        serde_json::Value::String(description.to_string()),
    );
    tool.input_schema = std::sync::Arc::new(schema);
}

const BOUND_PROJECT_SCHEMA_GUIDANCE: &str = "Bound sessions should omit project. An explicit alias for the same canonical DB is normalized to the immutable bound identity; other-project writes and destructive actions are forbidden. Established read-only cross-project actions remain action-gated.";

/// Add the session-binding contract to every native tool schema that exposes a
/// `project` property. This is applied at the MCP boundary so folded and legacy
/// tools cannot drift into contradictory per-struct wording.
fn annotate_bound_project_schema(tool: &mut rmcp::model::Tool) {
    if !tool
        .input_schema
        .get("properties")
        .and_then(|properties| properties.get("project"))
        .is_some_and(serde_json::Value::is_object)
    {
        return;
    }
    let mut schema = (*tool.input_schema).clone();
    let Some(project) = schema
        .get_mut("properties")
        .and_then(|properties| properties.as_object_mut())
        .and_then(|properties| properties.get_mut("project"))
        .and_then(|project| project.as_object_mut())
    else {
        return;
    };
    let description = project
        .get("description")
        .and_then(|value| value.as_str())
        .filter(|description| !description.is_empty())
        .map(|description| format!("{description} {BOUND_PROJECT_SCHEMA_GUIDANCE}"))
        .unwrap_or_else(|| BOUND_PROJECT_SCHEMA_GUIDANCE.to_string());
    project.insert(
        "description".to_string(),
        serde_json::Value::String(description),
    );
    tool.input_schema = std::sync::Arc::new(schema);
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct HttpSessionIdentity {
    profile: Option<String>,
    client: Option<String>,
    internal_proxy_token: Option<String>,
    owner_tool: Option<String>,
    owner_tool_error: Option<String>,
    agent_identity_id: Option<String>,
    /// Present-but-blank/illegal `tachiAgentIdentity` / `X-Tachi-Agent-Identity`
    /// must not collapse to "absent" and fall through to process env (#1761).
    agent_identity_error: Option<String>,
    project: Option<String>,
    /// A caller that sent a malformed project identity must not silently
    /// become an unbound session. In particular, HTTP permits opaque obs-text
    /// bytes that `HeaderValue::to_str` cannot decode as UTF-8; collapsing
    /// that case to `None` would bypass named-project validation entirely.
    project_error: Option<String>,
    /// #1120 PR1: `X-Tachi-Workspace-Root` / `_meta.tachiWorkspaceRoot`. Only
    /// consulted when `project` is absent — an explicit named-project binding
    /// always wins, matching how a caller-supplied `project=` argument always
    /// wins over a transport default elsewhere in this module.
    workspace_root: Option<String>,
    /// Review finding [3] (#1207): set when `X-Tachi-Workspace-Root` /
    /// `_meta.tachiWorkspaceRoot` was PRESENT but unusable — blank/whitespace,
    /// or (header only) not valid UTF-8 text — as opposed to simply absent.
    /// `workspace_root` collapses "absent" and "malformed" to the same `None`;
    /// that is fine for a caller that never declared a root, but a caller that
    /// DID send one and got it silently ignored must not fall through to an
    /// unbound session — this carries the reason so
    /// `apply_http_session_identity` can fail closed instead.
    workspace_root_error: Option<String>,
    /// #1251: the raw `X-Tachi-Dispatch-Depth` header value for the recursive-
    /// dispatch gate. Stored raw (like the other identity fields); a present
    /// value is honored, a malformed one saturates to the limit at the gate
    /// (`session_identity::resolve_dispatch_depth`) — deliberately NOT failed
    /// at `initialize` the way `workspace_root_error` is, because a malformed
    /// depth must fail CLOSED (refuse the eventual dispatch), not fail the
    /// whole session's `initialize` (which every non-dispatch tool call would
    /// also ride through). Header-only: the proxy injects it via
    /// `custom_headers`, never `_meta`.
    dispatch_depth: Option<String>,
    /// Audit C1: validated `X-Tachi-Rate-Limit-Session` key. Header-only (no
    /// `_meta` twin); a malformed value is dropped to `None` rather than
    /// failing the request, because it selects a rate-limit bucket only and
    /// never identity, profile, or authority.
    rate_limit_session: Option<String>,
}

/// #1120 PR1: which session-identity field supplies the bound project, when
/// more than one is present. An explicit `X-Tachi-Project` always wins over
/// `X-Tachi-Workspace-Root` — the workspace-root path only fills the gap for
/// a caller that has no already-registered project name to declare, mirroring
/// how a caller-supplied `project=` tool argument always wins over a
/// transport default elsewhere in this module. Pure/no I/O so the precedence
/// itself is unit-testable without constructing a full `RequestContext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectBindingSource<'a> {
    Named(&'a str),
    WorkspaceRoot(&'a str),
    None,
}

fn project_binding_source(identity: &HttpSessionIdentity) -> ProjectBindingSource<'_> {
    match identity.project.as_deref() {
        Some(project) => ProjectBindingSource::Named(project),
        None => match identity.workspace_root.as_deref() {
            Some(root) => ProjectBindingSource::WorkspaceRoot(root),
            None => ProjectBindingSource::None,
        },
    }
}

impl MemoryServer {
    fn apply_http_session_identity(
        &self,
        request: &InitializeRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        let identity = http_session_identity(request, context);
        self.apply_resolved_request_identity(identity, context, false)
    }

    fn clone_for_modern_request(
        &self,
        context: &RequestContext<RoleServer>,
        tool_call: bool,
    ) -> Result<Self, rmcp::ErrorData> {
        validate_modern_identity_headers(context)?;
        let request_server = self.clone_for_mcp_session();
        let identity = request_identity(Some(&context.meta), context);
        // Audit C1: a modern request has no session to hold a rate-limit
        // bucket, and `clone_for_mcp_session` just minted a per-request one.
        // Key the bucket by the resolved AgentIdentity (plus client and
        // project) whenever one is resolved — even if the request also
        // carries a valid `X-Tachi-Rate-Limit-Session` header. Every modern
        // request is a fresh clone, so honoring a caller-rotated header would
        // let one identified peer shed its own burst/RPM window by sending a
        // fresh legal key per call (PR2009). The header stays the fallback
        // for identityless requests (the stdio proxy's per-connection key);
        // without either anchor the per-request key remains: a client label
        // or project alone is shared by every instance of that client and
        // would let one peer trip another's loop block.
        let identity_bucket = (identity.agent_identity_id.clone(), identity.client.clone());
        request_server.apply_resolved_request_identity(identity, context, tool_call)?;
        // Applied only after `apply_resolved_request_identity` succeeded —
        // every identity check passed — so the derived bucket can never
        // short-circuit identity admission. It overrides the header-installed
        // `client:` key from `apply_resolved_request_identity`.
        if let Some(key) = modern_identity_rate_limit_session_id(
            identity_bucket.0.as_deref(),
            identity_bucket.1.as_deref(),
            request_server.session_project().as_deref(),
        ) {
            request_server.set_rate_limit_session_id(key);
        }
        Ok(request_server)
    }

    fn apply_resolved_request_identity(
        &self,
        identity: HttpSessionIdentity,
        context: &RequestContext<RoleServer>,
        tool_call: bool,
    ) -> Result<(), rmcp::ErrorData> {
        if let Some(err) = identity.project_error.as_deref() {
            return Err(rmcp::ErrorData::invalid_params(
                format!("malformed X-Tachi-Project identity: {err}"),
                None,
            ));
        }
        // Review finding [3] (#1207): a caller-supplied but malformed/blank
        // `X-Tachi-Workspace-Root` (or its `_meta` twin) must fail the whole
        // `initialize` call, not silently disappear into an unbound session
        // — an unbound session skips the git-root/tenant checks below
        // entirely, so a garbled identity header must never be treated the
        // same as "no header sent".
        if let Some(err) = identity.workspace_root_error.as_deref() {
            return Err(rmcp::ErrorData::invalid_params(
                format!("malformed X-Tachi-Workspace-Root identity: {err}"),
                None,
            ));
        }
        if let Some(err) = identity.agent_identity_error.as_deref() {
            return Err(rmcp::ErrorData::invalid_params(
                format!("malformed agent identity assertion: {err}"),
                None,
            ));
        }
        let profile = identity
            .profile
            .as_deref()
            .map(parse_http_tool_profile)
            .transpose()?;
        // A direct HTTP/proxy session that does not assert a profile must not
        // inherit an internal owner policy. Local stdio keeps the
        // explicitly configured process profile; only the per-connection HTTP
        // boundary defaults missing identity to the ordinary Agent surface.
        let profile = profile.or_else(|| {
            context
                .extensions
                .get::<axum::http::request::Parts>()
                .map(|_| tachi_hub::default_tool_profile())
        });
        if let Some(error) = identity.owner_tool_error {
            return Err(rmcp::ErrorData::invalid_params(error, None));
        }
        self.admit_owner_tool(
            identity.owner_tool.as_deref(),
            identity.internal_proxy_token.as_deref(),
            profile,
        )?;
        let mut roots = Vec::new();
        let (requested, binding) = match project_binding_source(&identity) {
            ProjectBindingSource::Named(project) => (Some(project), "project"),
            ProjectBindingSource::WorkspaceRoot(root) => {
                roots.push(root.to_string());
                (None, "X-Tachi-Workspace-Root")
            }
            ProjectBindingSource::None => (None, "workspace"),
        };
        // ZCode context is tool-call-only. Combine it before registration so
        // conflicting sources cannot create either candidate's project DB.
        if tool_call {
            if let Some(root) = crate::session_identity::zcode_workspace_path(&context.meta)
                .map_err(|error| rmcp::ErrorData::invalid_params(error, None))?
            {
                roots.push(root);
            }
        }
        let project = self
            .resolve_workspace_binding(None, requested, &roots)
            .map_err(|error| {
                rmcp::ErrorData::invalid_params(
                    format!("invalid HTTP direct-connect {binding} binding: {error}"),
                    None,
                )
            })?;
        self.set_session_identity(identity.client.clone(), project, profile);
        // The daemon HTTP listener is loopback-only and advertises
        // `loopback-trust-v1`. A valid identity explicitly carried by that
        // connection is therefore a local self-assertion, matching the
        // frozen AgentIdentity v1 contract. Absence stays rejected: direct
        // HTTP never inherits the daemon process env, so this cannot turn a
        // missing assertion into an identity. If the listener ever grows a
        // non-loopback bind, this trust decision must move to authenticated
        // transport evidence before that bind is enabled.
        let local_agent_assertion = context
            .extensions
            .get::<axum::http::request::Parts>()
            .is_none()
            || identity.agent_identity_id.is_some();
        crate::claims_ops::admit_agent_connection(
            self,
            identity.agent_identity_id,
            local_agent_assertion,
        )
        .map_err(|message| rmcp::ErrorData::invalid_params(message, None))?;
        // #1251: persist the wire depth into this session's runtime so the
        // recursion gate in `handle_tachi_dispatch` reads the CALLER's depth
        // (via the session), not the daemon's process env.
        self.set_session_dispatch_depth(identity.dispatch_depth);
        // Audit C1: applied last, after every identity check above has
        // passed, so the bucket header can never short-circuit or stand in
        // for identity admission. It only picks the rate-limit bucket. For a
        // modern request with a resolved AgentIdentity this `client:` key is
        // immediately overridden by the identity-derived bucket in
        // `clone_for_modern_request` (PR2009: a rotated header must not let
        // an identified peer shed its burst window); it stays authoritative
        // for identityless requests and legacy sessions.
        if let Some(key) = identity.rate_limit_session {
            self.set_rate_limit_session_id(format!("client:{key}"));
        }
        Ok(())
    }
}

/// Audit C1: stable rate-limit bucket for a modern (2026-07-28) request,
/// derived from its resolved agent identity, client label, and canonical
/// bound project. Takes precedence over a valid `X-Tachi-Rate-Limit-Session`
/// header whenever an AgentIdentity is resolved (PR2009): every modern
/// request is a fresh clone, so a header-selected bucket would let one
/// identified peer rotate fresh legal keys to shed its burst window. Returns
/// `None` when the request asserts no AgentIdentity (a client label or
/// project alone is shared across instances), leaving the header-installed
/// `client:` key or the per-request key in place. The key is a digest so the
/// bucket id stays opaque (it also appears in lifecycle events) and cannot
/// collide with the `client:` namespace.
fn modern_identity_rate_limit_session_id(
    agent_identity: Option<&str>,
    client: Option<&str>,
    project: Option<&str>,
) -> Option<String> {
    use sha2::{Digest, Sha256};

    // The client label and project are shared across instances, so they only
    // refine a bucket that an AgentIdentity already anchors.
    agent_identity?;
    let material = serde_json::json!([agent_identity, client, project]).to_string();
    Some(format!(
        "identity:{:x}",
        Sha256::digest(material.as_bytes())
    ))
}

fn http_session_identity(
    request: &InitializeRequestParams,
    context: &RequestContext<RoleServer>,
) -> HttpSessionIdentity {
    // RMCP moves wire `_meta` into RequestContext before dispatch. Typed params
    // remain a fallback for direct in-process handler calls.
    let meta = if context.meta.is_empty() {
        request.meta.as_ref()
    } else {
        Some(&context.meta)
    };
    request_identity(meta, context)
}

fn request_identity(
    meta: Option<&RequestMetaObject>,
    context: &RequestContext<RoleServer>,
) -> HttpSessionIdentity {
    let mut identity = identity_from_initialize_meta(meta);
    if let Some(parts) = context.extensions.get::<axum::http::request::Parts>() {
        identity.profile =
            header_string(parts, crate::session_identity::HEADER_PROFILE).or(identity.profile);
        identity.client =
            header_string(parts, crate::session_identity::HEADER_CLIENT).or(identity.client);
        identity.internal_proxy_token =
            header_string(parts, crate::session_identity::HEADER_INTERNAL_PROXY_TOKEN);
        match header_string_result(parts, crate::session_identity::HEADER_OWNER_TOOL) {
            Ok(value) => identity.owner_tool = value,
            Err(error) => identity.owner_tool_error = Some(error),
        }
        match header_string_result(parts, crate::session_identity::HEADER_AGENT_IDENTITY) {
            Ok(Some(value)) => {
                if let Err(err) = assign_explicit_agent_identity(&mut identity, value) {
                    identity.agent_identity_error = Some(err);
                }
            }
            Ok(None) => {}
            Err(err) => {
                identity.agent_identity_id = None;
                identity.agent_identity_error = Some(err);
            }
        }
        match header_string_result(parts, crate::session_identity::HEADER_PROJECT) {
            Ok(Some(value)) => {
                identity.project = Some(value);
                identity.project_error = None;
            }
            Ok(None) => {}
            Err(err) => {
                identity.project = None;
                identity.project_error = Some(err);
            }
        }
        // #1251: read the per-call recursion-depth marker off the wire. This is
        // the ONLY correct place to learn the caller's depth in the daemon-proxy
        // topology — `handle_tachi_dispatch` runs in the daemon carrying the
        // daemon's own env (always depth 0), so the depth must arrive per-call
        // over this header rail. Header-only (no `_meta` twin): the proxy
        // injects it via `custom_headers` in `call_daemon_tool_raw`.
        identity.dispatch_depth =
            header_string(parts, crate::session_identity::HEADER_DISPATCH_DEPTH);
        // Audit C1: header-only rate-limit bucket key. Malformed values are
        // ignored (not an error): the key never carries authority, so the
        // daemon just keeps its own per-session or identity-derived bucket.
        identity.rate_limit_session =
            header_string(parts, crate::session_identity::HEADER_RATE_LIMIT_SESSION)
                .filter(|key| crate::session_identity::valid_rate_limit_session_key(key));
        // Review finding [3] (#1207): a header wins over `_meta` per this
        // function's usual precedence, but ONLY when it is actually present
        // and well-formed. A PRESENT-but-malformed header must win the error
        // too (surfacing the header's own problem, not silently keeping a
        // `_meta`-derived value/error) rather than being treated as absent.
        match header_string_result(parts, crate::session_identity::HEADER_WORKSPACE_ROOT) {
            Ok(Some(value)) => {
                identity.workspace_root = Some(value);
                identity.workspace_root_error = None;
            }
            Ok(None) => {}
            Err(err) => identity.workspace_root_error = Some(err),
        }
        // HTTP has request Parts. Env fallback is stdio/local only
        // (#1761): a daemon process env must not confer identity on a
        // direct-connect session that omitted both `_meta` and header.
        return identity;
    }
    if identity.agent_identity_id.is_none() && identity.agent_identity_error.is_none() {
        identity.agent_identity_id = crate::session_identity::agent_identity_from_env_value(
            std::env::var(crate::session_identity::ENV_AGENT_IDENTITY)
                .ok()
                .as_deref(),
        );
    }
    identity
}

fn validate_modern_identity_headers(
    context: &RequestContext<RoleServer>,
) -> Result<(), rmcp::ErrorData> {
    let Some(parts) = context.extensions.get::<axum::http::request::Parts>() else {
        return Ok(());
    };
    for (header, canonical_meta, alias_meta) in [
        (
            crate::session_identity::HEADER_PROFILE,
            crate::session_identity::META_PROFILE,
            "tachi.profile",
        ),
        (
            crate::session_identity::HEADER_CLIENT,
            crate::session_identity::META_CLIENT,
            "tachi.client",
        ),
        (
            crate::session_identity::HEADER_AGENT_IDENTITY,
            crate::session_identity::META_AGENT_IDENTITY,
            "tachi.agentIdentity",
        ),
        (
            crate::session_identity::HEADER_PROJECT,
            crate::session_identity::META_PROJECT,
            "tachi.project",
        ),
        (
            crate::session_identity::HEADER_WORKSPACE_ROOT,
            crate::session_identity::META_WORKSPACE_ROOT,
            "tachi.workspaceRoot",
        ),
    ] {
        let header_value = header_string_result(parts, header).map_err(|error| {
            rmcp::ErrorData::invalid_params(format!("malformed {header} identity: {error}"), None)
        })?;
        let meta_value = crate::session_identity::aliased_meta_identity_string(
            &context.meta,
            canonical_meta,
            alias_meta,
        )
        .map_err(|error| rmcp::ErrorData::invalid_params(error, None))?;
        if let Some((header_value, meta_value)) = header_value
            .as_ref()
            .zip(meta_value.as_ref())
            .filter(|(header_value, meta_value)| header_value != meta_value)
        {
            return Err(rmcp::ErrorData::header_mismatch(
                format!(
                    "{header} header identity ({header_value}) does not match request _meta identity ({meta_value})"
                ),
                None,
            ));
        }
    }
    Ok(())
}

fn reject_modern_direct_stdio(
    mode: crate::mcp_peer::McpPeerMode,
    context: &RequestContext<RoleServer>,
) -> Result<(), rmcp::ErrorData> {
    // The direct debugging route has neither the stdio proxy's process-bound
    // admission gate nor HTTP transport headers. It remains deliberately
    // legacy rather than accepting request assertions as process authority.
    if mode == crate::mcp_peer::McpPeerMode::Modern20260728
        && context
            .extensions
            .get::<axum::http::request::Parts>()
            .is_none()
    {
        return Err(rmcp::ErrorData::unsupported_protocol_version(
            rmcp::model::ProtocolVersion::V_2026_07_28,
            rmcp::model::ProtocolVersion::known_up_to(&rmcp::model::ProtocolVersion::V_2025_11_25),
        ));
    }
    Ok(())
}

/// Extract session identity fields from MCP initialize `_meta` (#732).
/// Headers still win when both are present (applied after this helper).
fn assign_explicit_agent_identity(
    identity: &mut HttpSessionIdentity,
    value: String,
) -> Result<(), String> {
    if crate::session_identity::valid_agent_identity_assertion(&value) {
        identity.agent_identity_id = Some(value);
        identity.agent_identity_error = None;
        Ok(())
    } else {
        identity.agent_identity_id = None;
        Err("agent identity assertion is invalid".to_string())
    }
}

fn identity_from_initialize_meta(
    meta: Option<&rmcp::model::RequestMetaObject>,
) -> HttpSessionIdentity {
    let mut identity = HttpSessionIdentity::default();
    let Some(meta) = meta else {
        return identity;
    };
    identity.profile = meta_string(meta, crate::session_identity::META_PROFILE)
        .or_else(|| meta_string(meta, "tachi.profile"));
    identity.client = meta_string(meta, crate::session_identity::META_CLIENT)
        .or_else(|| meta_string(meta, "tachi.client"));
    match crate::session_identity::meta_identity_string(
        meta,
        crate::session_identity::META_AGENT_IDENTITY,
    ) {
        Ok(Some(value)) => match assign_explicit_agent_identity(&mut identity, value) {
            Ok(()) => {}
            Err(err) => identity.agent_identity_error = Some(err),
        },
        Ok(None) => {
            match crate::session_identity::meta_identity_string(meta, "tachi.agentIdentity") {
                Ok(Some(value)) => match assign_explicit_agent_identity(&mut identity, value) {
                    Ok(()) => {}
                    Err(err) => identity.agent_identity_error = Some(err),
                },
                Ok(None) => {}
                Err(err) => identity.agent_identity_error = Some(err),
            }
        }
        Err(err) => identity.agent_identity_error = Some(err),
    }
    match crate::session_identity::meta_identity_string(meta, crate::session_identity::META_PROJECT)
    {
        Ok(Some(value)) => identity.project = Some(value),
        Ok(None) => match crate::session_identity::meta_identity_string(meta, "tachi.project") {
            Ok(Some(value)) => identity.project = Some(value),
            Ok(None) => {}
            Err(err) => identity.project_error = Some(err),
        },
        Err(err) => identity.project_error = Some(err),
    }
    // Review finding [3] (#1207): unlike the fields above, a PRESENT-but-
    // malformed `_meta.tachiWorkspaceRoot` (non-string type, or blank) must
    // be recorded as an error, not silently treated the same as "the caller
    // never declared a workspace root". Try the canonical key first; only
    // fall through to the dotted alias when the canonical key is genuinely
    // ABSENT (a malformed canonical key is itself the caller's answer and
    // must not be masked by trying the alias next).
    match crate::session_identity::meta_identity_string(
        meta,
        crate::session_identity::META_WORKSPACE_ROOT,
    ) {
        Ok(Some(value)) => identity.workspace_root = Some(value),
        Ok(None) => {
            match crate::session_identity::meta_identity_string(meta, "tachi.workspaceRoot") {
                Ok(Some(value)) => identity.workspace_root = Some(value),
                Ok(None) => {}
                Err(err) => identity.workspace_root_error = Some(err),
            }
        }
        Err(err) => identity.workspace_root_error = Some(err),
    }
    identity
}

fn meta_string(meta: &rmcp::model::RequestMetaObject, key: &str) -> Option<String> {
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

/// Presence-distinguishing twin of [`header_string`] for `workspace_root`
/// (review finding [3], #1207): `Ok(None)` means the header is genuinely
/// absent; `Err` means it was present but unusable (not valid UTF-8 text, or
/// blank after trimming).
fn header_string_result(
    parts: &axum::http::request::Parts,
    name: &str,
) -> Result<Option<String>, String> {
    match parts.headers.get(name) {
        None => Ok(None),
        Some(value) => {
            let raw = value
                .to_str()
                .map_err(|_| format!("{name} header is not valid UTF-8 text"))?;
            match crate::session_identity::normalize_identity_value(raw) {
                Some(v) => Ok(Some(v)),
                None => Err(format!("{name} header is blank")),
            }
        }
    }
}

#[cfg(test)]
#[path = "server_handler/tests.rs"]
mod tests;
