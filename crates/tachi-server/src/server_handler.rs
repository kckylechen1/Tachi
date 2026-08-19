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
            | "vault_status"
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
            | "tachi_gh"
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
                    "Required task memory, policy, or ledger action. Ordinary local delegation uses the host harness's native subagent. Recommendations are advisory and do not authorize an execution backend. GitHub PR lifecycle is tachi_gh only.",
                );
                hide_operator_dispatch_properties(tool);
                let action_summary = describe_allowed_task_actions(&allowed);
                tool.description = Some(std::borrow::Cow::Owned(format!(
                    "Task memory, policy, and ledger facade. Ordinary local delegation uses the host harness's native subagent.{action_summary} Sequencing and delegation decisions are the host model's job, not this facade. GitHub PR lifecycle is tachi_gh only.",
                )));
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
                    tool.description = Some(std::borrow::Cow::Owned(
                        "Stable, reusable knowledge base. action='search': look up lessons, patterns, and how-tos BEFORE debugging from scratch or reaching for web search — a prior lesson may already exist. action='browse': explore available categories. action='read': load a specific entry by path. WHEN: wiki for durable knowledge that helps future sessions (patterns, lessons, decisions, conventions). Use tachi_memory for session-specific facts (decisions, findings, commands for the current task). Pass project to target a named library.".to_string(),
                    ));
                }
            }
            _ => {}
        }
    }
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
        let project = match project_binding_source(&identity) {
            ProjectBindingSource::Named(project) => {
                let (canonical_project, _) = self
                    .resolve_server_named_project_binding(project)
                    .map_err(|err| {
                        rmcp::ErrorData::invalid_params(
                            format!("invalid HTTP direct-connect project binding: {err}"),
                            None,
                        )
                    })?;
                Some(canonical_project)
            }
            ProjectBindingSource::WorkspaceRoot(root) => Some(
                self.resolve_or_register_workspace_root(root)
                    .map_err(|err| {
                        rmcp::ErrorData::invalid_params(
                            format!(
                                "invalid HTTP direct-connect X-Tachi-Workspace-Root binding: {err}"
                            ),
                            None,
                        )
                    })?,
            ),
            ProjectBindingSource::None => None,
        };
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

fn identity_from_initialize_meta(meta: Option<&rmcp::model::Meta>) -> HttpSessionIdentity {
    let mut identity = HttpSessionIdentity::default();
    let Some(meta) = meta else {
        return identity;
    };
    identity.profile = meta_string(meta, crate::session_identity::META_PROFILE)
        .or_else(|| meta_string(meta, "tachi.profile"));
    identity.client = meta_string(meta, crate::session_identity::META_CLIENT)
        .or_else(|| meta_string(meta, "tachi.client"));
    match meta_string_result(meta, crate::session_identity::META_AGENT_IDENTITY) {
        Ok(Some(value)) => match assign_explicit_agent_identity(&mut identity, value) {
            Ok(()) => {}
            Err(err) => identity.agent_identity_error = Some(err),
        },
        Ok(None) => match meta_string_result(meta, "tachi.agentIdentity") {
            Ok(Some(value)) => match assign_explicit_agent_identity(&mut identity, value) {
                Ok(()) => {}
                Err(err) => identity.agent_identity_error = Some(err),
            },
            Ok(None) => {}
            Err(err) => identity.agent_identity_error = Some(err),
        },
        Err(err) => identity.agent_identity_error = Some(err),
    }
    match meta_string_result(meta, crate::session_identity::META_PROJECT) {
        Ok(Some(value)) => identity.project = Some(value),
        Ok(None) => match meta_string_result(meta, "tachi.project") {
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
    match meta_string_result(meta, crate::session_identity::META_WORKSPACE_ROOT) {
        Ok(Some(value)) => identity.workspace_root = Some(value),
        Ok(None) => match meta_string_result(meta, "tachi.workspaceRoot") {
            Ok(Some(value)) => identity.workspace_root = Some(value),
            Ok(None) => {}
            Err(err) => identity.workspace_root_error = Some(err),
        },
        Err(err) => identity.workspace_root_error = Some(err),
    }
    identity
}

fn meta_string(meta: &rmcp::model::Meta, key: &str) -> Option<String> {
    meta.0
        .get(key)
        .and_then(|value| value.as_str())
        .and_then(crate::session_identity::normalize_identity_value)
}

/// Presence-distinguishing twin of [`meta_string`] for `workspace_root`
/// (review finding [3], #1207): `Ok(None)` means the key is genuinely
/// absent; `Err` means it was present but unusable (wrong JSON type, or
/// blank after trimming) — the caller must not collapse that into `Ok(None)`
/// the way an absent key would be.
fn meta_string_result(meta: &rmcp::model::Meta, key: &str) -> Result<Option<String>, String> {
    match meta.0.get(key) {
        None => Ok(None),
        Some(value) => {
            let raw = value
                .as_str()
                .ok_or_else(|| format!("_meta.{key} must be a string"))?;
            match crate::session_identity::normalize_identity_value(raw) {
                Some(v) => Ok(Some(v)),
                None => Err(format!("_meta.{key} is blank")),
            }
        }
    }
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
            let mut tools = prepare_native_tool_definitions(all_native);

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
            tools = project_tool_definitions(
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
                crate::session_identity::enforce_server_session_project(
                    self,
                    name,
                    &mut params.arguments,
                    project,
                    "HTTP direct-connect",
                    // #1041 B1: the daemon's own call_tool is the sole
                    // authoritative hop — it, and only it, may inject a
                    // default `project` for an absent one.
                    crate::session_identity::EnforcementRole::Authoritative,
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
                // #1255: `clone_for_mcp_session` stamps a unique opaque id on
                // each MCP session clone; `check_session_rate_limit` keys burst
                // windows by that id so sessions sharing the process-global
                // RateLimiter do not inherit each other's counters.
                self.check_session_rate_limit(name, &args_hash)?
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

            let mut result = {
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
                } else {
                    let error_category = categorize_error(&err.to_string());
                    let reason = dlq_capture_refusal_reason(&tool_name_owned, is_native);
                    if let Err(signal_error) = record_dlq_capture_refused(
                        self,
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

    /// Full native tool surface (mirrors server_state/init.rs's tool_router
    /// sum). Shared by every test in this module that needs to walk the real
    /// `list_tools` projection instead of hand-building a single tool, so the
    /// router-sum listing has exactly one copy to keep in sync with
    /// `server_state/init.rs`.
    fn native_tools() -> Vec<rmcp::model::Tool> {
        (MemoryServer::a2a_tool_router()
            + MemoryServer::continuity_tool_router()
            + MemoryServer::component_tool_router()
            + MemoryServer::copilot_tool_router()
            + MemoryServer::dispatch_tool_router()
            + MemoryServer::handoff_tool_router()
            + MemoryServer::runtime_context_tool_router()
            + MemoryServer::hub_tool_router()
            + MemoryServer::pipeline_tool_router()
            + MemoryServer::memory_tool_router()
            + MemoryServer::vault_tool_router()
            + MemoryServer::workflow_tool_router()
            + MemoryServer::tune_tool_router()
            + MemoryServer::wiki_tool_router()
            + MemoryServer::sandbox_tool_router()
            + MemoryServer::peer_tool_router())
        .list_all()
    }

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

    /// Break caught: tools/list exposing a write action or its payload fields
    /// to a profile whose call-time gate permits only status.
    #[test]
    fn projected_a2a_schema_matches_profile_action_gate() {
        fn projected(profile: tachi_hub::ToolProfile) -> rmcp::model::Tool {
            project_tool_definitions(native_tools(), Some(profile), None)
                .into_iter()
                .find(|tool| tool.name.as_ref() == "tachi_a2a")
                .expect("profile-visible tachi_a2a")
        }

        let observe = projected(tachi_hub::ToolProfile::observe());
        let observe_properties = observe.input_schema["properties"]
            .as_object()
            .expect("observe a2a properties");
        assert_eq!(
            observe_properties["action"]["enum"],
            json!(["status"]),
            "projection must advertise exactly the call-time-allowed action"
        );
        for respond_only in [
            "recipient_agent_identity_id",
            "subject_ref",
            "text",
            "idempotency_key",
            "ttl_days",
        ] {
            assert!(
                !observe_properties.contains_key(respond_only),
                "observe schema leaked respond-only field {respond_only}"
            );
        }
        assert!(observe_properties.contains_key("limit"));
        assert!(tachi_hub::facade_action_allowed(
            "tachi_a2a",
            Some("status"),
            Some(tachi_hub::ToolProfile::observe())
        ));
        assert!(!tachi_hub::facade_action_allowed(
            "tachi_a2a",
            Some("respond"),
            Some(tachi_hub::ToolProfile::observe())
        ));

        let remember = projected(tachi_hub::ToolProfile::remember());
        let remember_properties = remember.input_schema["properties"]
            .as_object()
            .expect("remember a2a properties");
        assert_eq!(
            remember_properties["action"]["enum"],
            json!(["respond", "status"])
        );
        for respond_field in [
            "recipient_agent_identity_id",
            "subject_ref",
            "text",
            "idempotency_key",
            "ttl_days",
        ] {
            assert!(
                remember_properties.contains_key(respond_field),
                "remember schema lost respond field {respond_field}"
            );
        }
    }

    #[test]
    fn standard_schema_hides_dispatch_only_execution_knobs() {
        fn task_tool() -> rmcp::model::Tool {
            MemoryServer::workflow_tool_router()
                .list_all()
                .into_iter()
                .find(|tool| tool.name.as_ref() == "tachi_task")
                .expect("routed tachi_task tool")
        }

        let mut standard = vec![task_tool()];
        narrow_gated_action_schemas(&mut standard, Some(tachi_hub::ToolProfile::standard()));
        let standard_actions = standard[0].input_schema["properties"]["action"]["enum"]
            .as_array()
            .expect("standard action enum");
        assert!(!standard_actions.contains(&json!("dispatch")));
        assert!(!standard_actions.contains(&json!("recommend")));
        assert!(standard_actions.contains(&json!("complete")));
        let standard_properties = standard[0].input_schema["properties"]
            .as_object()
            .expect("standard properties");
        // #1319-C2: dispatch-only execution knobs are hidden at the struct
        // level (#[schemars(skip)]), so no profile — standard or admin —
        // exposes them anymore. The projection list below is the surviving
        // verification surface for the struct-level hide.
        for hidden in [
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
            "tool_profile",
            "mcp_access",
            "allowed_mcp_servers",
        ] {
            assert!(!standard_properties.contains_key(hidden), "{hidden}");
        }
        // #1319-C2: `cwd` survives as a brief field (relative
        // doc path resolution) — it must stay visible on the standard tool.
        assert!(standard_properties.contains_key("cwd"));
        assert!(standard_properties["cwd"]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("action=brief"));
        assert!(!standard[0]
            .description
            .as_deref()
            .unwrap_or_default()
            .contains("dispatch"));
        assert!(
            !standard[0].input_schema["properties"]["action"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("dispatch")
        );
        for property in standard_properties.values() {
            assert!(!property
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains("dispatch"));
        }
        let standard_schema =
            serde_json::to_string(&*standard[0].input_schema).expect("standard schema serializes");
        for hidden_text in [
            "dispatch_reason",
            "explicit_user_request",
            "action=dispatch",
            "spawned agent",
            "TachiDispatchReason",
        ] {
            assert!(!standard_schema.contains(hidden_text), "{hidden_text}");
        }

        for profile in [
            tachi_hub::ToolProfile::coordinate(),
            tachi_hub::ToolProfile::delegate(),
        ] {
            let mut tools = vec![task_tool()];
            narrow_gated_action_schemas(&mut tools, Some(profile));
            let actions = tools[0].input_schema["properties"]["action"]["enum"]
                .as_array()
                .expect("non-admin action enum");
            assert!(!actions.contains(&json!("dispatch")));
            assert!(!tools[0].input_schema["properties"]
                .as_object()
                .expect("non-admin properties")
                .contains_key("dispatch_reason"));
            assert!(!tools[0]
                .description
                .as_deref()
                .unwrap_or_default()
                .contains("dispatch"));
            assert!(
                !tools[0].input_schema["properties"]["action"]["description"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("dispatch")
            );
        }

        let mut admin = vec![task_tool()];
        narrow_gated_action_schemas(&mut admin, Some(tachi_hub::ToolProfile::admin()));
        let admin_actions = admin[0].input_schema["properties"]["action"]["enum"]
            .as_array()
            .expect("admin action enum");
        // #1319-C2: dispatch/cancel/wait were removed from tachi_task (external
        // staffing now flows through tachi_staff). The schema must NOT
        // advertise them, and the dispatch_reason field is gone.
        assert!(
            !admin_actions.contains(&json!("dispatch")),
            "tachi_task must not advertise dispatch after [1319-C2]"
        );
        assert!(
            !admin_actions.contains(&json!("cancel")),
            "tachi_task must not advertise cancel after [1319-C2]"
        );
        assert!(
            !admin_actions.contains(&json!("wait")),
            "tachi_task must not advertise wait after [1319-C2]"
        );
        assert!(
            !admin[0].input_schema["properties"]
                .as_object()
                .expect("admin properties")
                .contains_key("dispatch_reason"),
            "dispatch_reason field must be gone after [1319-C2]"
        );
        // The description no longer advertises dispatch as a tachi_task action.
        assert!(
            !admin[0]
                .description
                .as_deref()
                .unwrap_or_default()
                .contains("action='dispatch'"),
            "tachi_task description must not advertise action='dispatch' after [1319-C2]"
        );
    }

    #[test]
    fn projected_tools_list_never_teaches_retired_task_actions() {
        // The enum/schema/census discriminators (see
        // `f0_task_primary_does_not_advertise_gh_lifecycle` and the
        // facade_tests schema census) only ever checked the `action` enum.
        // They missed free-text tool descriptions, which is exactly how
        // Retired tachi_task actions must never be taught to the
        // model after the enum was pruned (F1: `narrow_gated_action_schemas`
        // rewrote the non-admin description but still said "Use
        // brief/status/plan ... recommend for advisory ... merge only
        // for..."). This walks the same projection `list_tools` actually
        // serves (`project_tool_definitions`) across profiles.
        //
        // Word-boundary, not substring, and split by scope:
        //   - tachi_task's OWN description is checked against the full
        //     retired list — that tool owns every one of these action names.
        //   - every OTHER tool's description is checked only against the
        //     three compound/unambiguous tokens (cycle_plan, refine_issues,
        //     ux_matrix). Plain "plan"/"recommend"/"merge" are ordinary
        //     English words that legitimately appear in unrelated tools
        //     (tachi_component's own live action='plan', tachi_gh's
        //     "routing plan"/"Safe-merge", tachi_memory's "merge
        //     duplicates", dispatch's "auto-merge worktrees") — scanning
        //     those tools for the bare words would be a false-positive
        //     factory, not a real regression signal.
        fn leaks_retired_token(description: &str, token: &str) -> bool {
            description
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|word| word == token)
        }

        const CROSS_TOOL_UNAMBIGUOUS_TOKENS: &[&str] =
            &["cycle_plan", "refine_issues", "ux_matrix"];

        for profile in [
            None,
            Some(tachi_hub::ToolProfile::standard()),
            Some(tachi_hub::ToolProfile::delegate()),
            Some(tachi_hub::ToolProfile::coordinate()),
            Some(tachi_hub::ToolProfile::admin()),
        ] {
            let projected = project_tool_definitions(native_tools(), profile, None);
            for tool in &projected {
                let description = tool.description.as_deref().unwrap_or_default();
                let scoped_tokens: &[&str] = if tool.name.as_ref() == "tachi_task" {
                    tachi_params::TACHI_TASK_RETIRED_ACTIONS
                } else {
                    CROSS_TOOL_UNAMBIGUOUS_TOKENS
                };
                for retired in scoped_tokens {
                    assert!(
                        !leaks_retired_token(description, retired),
                        "profile {:?} tool '{}' description still teaches retired task action {retired:?}: {description}",
                        profile.map(tachi_hub::ToolProfile::as_str),
                        tool.name
                    );
                }
            }
        }
    }

    #[test]
    fn projected_task_description_never_names_an_action_outside_its_own_enum() {
        // `narrow_gated_action_schemas` used to filter
        // the advertised `action` enum down to the profile's real allow-list
        // (240-246 of this file) and then paste a SEPARATE hand-written
        // sentence naming actions for the description — one sentence shared
        // by every non-admin profile, regardless of what that profile's
        // filtered enum actually contained. Delegate's enum was
        // complete/status/board/brief, but the shared sentence
        // still taught profile/card/adjudicate/claim/heartbeat/
        // handoff/release — a discoverable-but-not-callable trap. This test
        // makes that class of drift structurally impossible to reintroduce:
        // for every non-admin profile, every `TachiTaskAction` wire token
        // that appears (word-boundary, not substring — `plan` must not match
        // inside `planning`) in the projected `tachi_task` description must
        // also appear in that same projection's action enum.
        fn contains_word(haystack: &str, word: &str) -> bool {
            haystack
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|token| token == word)
        }

        for profile in [
            tachi_hub::ToolProfile::observe(),
            tachi_hub::ToolProfile::remember(),
            tachi_hub::ToolProfile::coordinate(),
            tachi_hub::ToolProfile::operate(),
            tachi_hub::ToolProfile::standard(),
            tachi_hub::ToolProfile::delegate(),
        ] {
            let projected = project_tool_definitions(native_tools(), Some(profile), None);
            let task_tool = projected
                .iter()
                .find(|tool| tool.name.as_ref() == "tachi_task")
                .expect("tachi_task survives projection for every non-admin profile");
            let description = task_tool.description.as_deref().unwrap_or_default();
            let enum_actions: std::collections::HashSet<&str> = task_tool.input_schema
                ["properties"]["action"]["enum"]
                .as_array()
                .expect("tachi_task action enum")
                .iter()
                .map(|v| v.as_str().expect("action enum entries are strings"))
                .collect();

            for wire in tachi_params::TachiTaskAction::primary_wire_strings() {
                if contains_word(description, wire) && !enum_actions.contains(wire) {
                    panic!(
                        "profile {:?} tachi_task description names action {wire:?}, \
                         which is NOT in this profile's own projected action enum \
                         ({enum_actions:?}): {description}",
                        profile.as_str(),
                    );
                }
            }
        }
    }

    #[test]
    fn projected_wiki_description_never_names_an_action_outside_its_own_enum() {
        fn contains_word(haystack: &str, word: &str) -> bool {
            haystack
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|token| token == word)
        }

        for profile in [
            tachi_hub::ToolProfile::observe(),
            tachi_hub::ToolProfile::remember(),
            tachi_hub::ToolProfile::coordinate(),
            tachi_hub::ToolProfile::operate(),
            tachi_hub::ToolProfile::standard(),
            tachi_hub::ToolProfile::delegate(),
        ] {
            let projected = project_tool_definitions(native_tools(), Some(profile), None);
            let Some(wiki_tool) = projected
                .iter()
                .find(|tool| tool.name.as_ref() == "tachi_wiki")
            else {
                continue;
            };
            let description = wiki_tool.description.as_deref().unwrap_or_default();
            let enum_actions: std::collections::HashSet<&str> = wiki_tool.input_schema
                ["properties"]["action"]["enum"]
                .as_array()
                .expect("tachi_wiki action enum")
                .iter()
                .map(|v| v.as_str().expect("action enum entries are strings"))
                .collect();

            for wire in tachi_params::TACHI_WIKI_ACTIONS {
                if contains_word(description, wire) && !enum_actions.contains(wire) {
                    panic!(
                        "profile {:?} tachi_wiki description names action {wire:?}, \
                         which is NOT in this profile's own projected action enum \
                         ({enum_actions:?}): {description}",
                        profile.as_str(),
                    );
                }
            }
        }
    }

    #[test]
    fn native_project_tool_schema_advertises_bound_alias_normalization() {
        let mut tool: rmcp::model::Tool = serde_json::from_value(json!({
            "name": "tachi_memory",
            "description": "memory facade",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {
                        "type": ["string", "null"],
                        "description": "Optional named project DB."
                    }
                }
            }
        }))
        .expect("test tool");

        annotate_bound_project_schema(&mut tool);

        let description = tool
            .input_schema
            .get("properties")
            .and_then(|properties| properties.get("project"))
            .and_then(|project| project.get("description"))
            .and_then(|description| description.as_str())
            .expect("project description");
        assert!(description.contains("Bound sessions should omit project"));
        assert!(description.contains("same canonical DB"));
        assert!(description.contains("normalized to the immutable bound identity"));
        assert!(description.contains("other-project writes and destructive actions are forbidden"));
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
        assert!(identity.project_error.is_none());
        assert!(identity.agent_identity_id.is_none());
        assert!(
            identity.agent_identity_error.is_none(),
            "absent identity key must stay error-free so env can fill (#1761)"
        );
    }

    #[test]
    fn initialize_meta_blank_or_illegal_agent_identity_is_error_not_absence() {
        for value in [json!("   "), json!("agent identity"), json!(12345)] {
            let mut map = serde_json::Map::new();
            map.insert(
                crate::session_identity::META_AGENT_IDENTITY.to_string(),
                value,
            );
            let identity = identity_from_initialize_meta(Some(&rmcp::model::Meta(map)));
            assert!(
                identity.agent_identity_id.is_none(),
                "a present unusable identity must not bind"
            );
            assert!(
                identity.agent_identity_error.is_some(),
                "present-but-blank/illegal identity must block env fallback (#1761)"
            );
        }
    }

    #[test]
    fn initialize_meta_malformed_project_is_recorded_as_an_error_not_absence() {
        for value in [json!("   "), json!(12345)] {
            let mut map = serde_json::Map::new();
            map.insert(crate::session_identity::META_PROJECT.to_string(), value);
            let identity = identity_from_initialize_meta(Some(&rmcp::model::Meta(map)));
            assert!(identity.project.is_none());
            assert!(
                identity.project_error.is_some(),
                "a present malformed project must not become an unbound session"
            );
        }
    }

    /// #1120 PR1: `_meta.tachiWorkspaceRoot` parses into `HttpSessionIdentity`
    /// the same way `META_PROJECT` already does.
    #[test]
    fn initialize_meta_binds_workspace_root() {
        let mut map = serde_json::Map::new();
        map.insert(
            crate::session_identity::META_WORKSPACE_ROOT.to_string(),
            json!("/home/agent/repos/sigil"),
        );
        let meta = rmcp::model::Meta(map);
        let identity = identity_from_initialize_meta(Some(&meta));
        assert_eq!(
            identity.workspace_root.as_deref(),
            Some("/home/agent/repos/sigil")
        );
        assert!(identity.project.is_none());
    }

    /// #1120 PR1: the dotted-alias fallback (`tachi.project` already has one)
    /// also covers `tachi.workspaceRoot`.
    #[test]
    fn initialize_meta_accepts_dotted_workspace_root_alias() {
        let mut map = serde_json::Map::new();
        map.insert("tachi.workspaceRoot".to_string(), json!("/repo/root"));
        let meta = rmcp::model::Meta(map);
        let identity = identity_from_initialize_meta(Some(&meta));
        assert_eq!(identity.workspace_root.as_deref(), Some("/repo/root"));
    }

    /// Review finding [3] (#1207): a blank `_meta.tachiWorkspaceRoot` was
    /// PRESENT but unusable — it must not be treated the same as the caller
    /// never having declared a root at all.
    #[test]
    fn initialize_meta_blank_workspace_root_is_recorded_as_an_error_not_absence() {
        let mut map = serde_json::Map::new();
        map.insert(
            crate::session_identity::META_WORKSPACE_ROOT.to_string(),
            json!("   "),
        );
        let meta = rmcp::model::Meta(map);
        let identity = identity_from_initialize_meta(Some(&meta));
        assert!(
            identity.workspace_root.is_none(),
            "a blank value must not bind a usable workspace root"
        );
        assert!(
            identity.workspace_root_error.is_some(),
            "a blank-but-present value must be recorded as an error, not silent absence"
        );
    }

    /// Review finding [3] (#1207) twin: a non-string `_meta.tachiWorkspaceRoot`
    /// (wrong JSON type) is malformed, not absent.
    #[test]
    fn initialize_meta_non_string_workspace_root_is_recorded_as_an_error() {
        let mut map = serde_json::Map::new();
        map.insert(
            crate::session_identity::META_WORKSPACE_ROOT.to_string(),
            json!(12345),
        );
        let meta = rmcp::model::Meta(map);
        let identity = identity_from_initialize_meta(Some(&meta));
        assert!(identity.workspace_root.is_none());
        assert!(
            identity.workspace_root_error.is_some(),
            "a non-string value must be recorded as an error, not silent absence"
        );
    }

    /// Review finding [3] (#1207): a malformed value under the canonical key
    /// must not be masked by falling through to try the dotted alias next —
    /// the caller's actual (bad) answer under the primary key is the signal,
    /// not "maybe they meant the alias".
    #[test]
    fn initialize_meta_malformed_canonical_key_is_not_masked_by_the_alias() {
        let mut map = serde_json::Map::new();
        map.insert(
            crate::session_identity::META_WORKSPACE_ROOT.to_string(),
            json!(""),
        );
        map.insert("tachi.workspaceRoot".to_string(), json!("/repo/root"));
        let meta = rmcp::model::Meta(map);
        let identity = identity_from_initialize_meta(Some(&meta));
        assert!(
            identity.workspace_root.is_none(),
            "the malformed canonical key must win over a well-formed alias"
        );
        assert!(identity.workspace_root_error.is_some());
    }

    /// Review finding [3] (#1207): a header that is present but not valid
    /// UTF-8 text is malformed, not absent — `header_string_result` must
    /// surface it as `Err`, distinct from a genuinely missing header.
    #[test]
    fn header_string_result_rejects_non_utf8_header_value() {
        let parts = axum::http::Request::builder()
            .header(
                crate::session_identity::HEADER_WORKSPACE_ROOT,
                axum::http::HeaderValue::from_bytes(&[0xff, 0xfe]).expect("opaque header bytes"),
            )
            .body(())
            .expect("build request")
            .into_parts()
            .0;
        let err = header_string_result(&parts, crate::session_identity::HEADER_WORKSPACE_ROOT)
            .expect_err("non-UTF-8 header bytes must be rejected, not treated as absent");
        assert!(err.contains("UTF-8"), "unexpected error message: {err}");
    }

    #[test]
    fn non_utf8_project_header_is_recorded_as_an_error_not_absence() {
        let parts = axum::http::Request::builder()
            .header(
                crate::session_identity::HEADER_PROJECT,
                axum::http::HeaderValue::from_bytes("量化".as_bytes())
                    .expect("HTTP permits opaque obs-text bytes"),
            )
            .body(())
            .expect("build request")
            .into_parts()
            .0;
        let err = header_string_result(&parts, crate::session_identity::HEADER_PROJECT)
            .expect_err("an undecodable project header must fail closed");
        assert!(err.contains("UTF-8"), "unexpected error message: {err}");
    }

    /// Review finding [3] (#1207) twin: a present-but-blank header value is
    /// malformed, not absent.
    #[test]
    fn header_string_result_rejects_a_blank_header_value() {
        let parts = axum::http::Request::builder()
            .header(crate::session_identity::HEADER_WORKSPACE_ROOT, "   ")
            .body(())
            .expect("build request")
            .into_parts()
            .0;
        let err = header_string_result(&parts, crate::session_identity::HEADER_WORKSPACE_ROOT)
            .expect_err("a blank header value must be rejected, not treated as absent");
        assert!(err.contains("blank"), "unexpected error message: {err}");
    }

    /// Review finding [3] (#1207) control case: a header that was never sent
    /// at all is genuinely absent — this must stay `Ok(None)`, not an error,
    /// or every session without the (optional) header would fail `initialize`.
    #[test]
    fn header_string_result_missing_header_is_ok_none() {
        let parts = axum::http::Request::builder()
            .body(())
            .expect("build request")
            .into_parts()
            .0;
        assert_eq!(
            header_string_result(&parts, crate::session_identity::HEADER_WORKSPACE_ROOT)
                .expect("a missing header is not an error"),
            None
        );
    }

    /// #1251: the `X-Tachi-Dispatch-Depth` header is read off the wire by the
    /// exact expression `http_session_identity` assigns into
    /// `HttpSessionIdentity::dispatch_depth` (`header_string(parts,
    /// HEADER_DISPATCH_DEPTH)`), which is what `apply_http_session_identity`
    /// then stamps into the session runtime. A present value round-trips
    /// (trimmed); an absent header yields `None` ≡ depth 0. This is the daemon
    /// end of the proxy→daemon rail; the full live proxy→daemon round-trip is
    /// covered by the manual verification path documented in the #1251 PR.
    #[test]
    fn dispatch_depth_header_is_read_off_the_wire() {
        let parts = axum::http::Request::builder()
            .header(crate::session_identity::HEADER_DISPATCH_DEPTH, "2")
            .body(())
            .expect("build request")
            .into_parts()
            .0;
        assert_eq!(
            header_string(&parts, crate::session_identity::HEADER_DISPATCH_DEPTH).as_deref(),
            Some("2"),
            "a present depth header must be read into HttpSessionIdentity::dispatch_depth"
        );

        let no_header = axum::http::Request::builder()
            .body(())
            .expect("build request")
            .into_parts()
            .0;
        assert_eq!(
            header_string(&no_header, crate::session_identity::HEADER_DISPATCH_DEPTH),
            None,
            "an absent depth header yields None, which resolves to leader depth 0"
        );
    }

    /// #1120 PR1 core regression: an explicit `X-Tachi-Project` (here, its
    /// `_meta` twin `META_PROJECT`) must win over a simultaneously-present
    /// `X-Tachi-Workspace-Root` — the workspace-root path only fills the gap
    /// for a caller with no already-registered project name, never overrides
    /// one the caller did supply.
    #[test]
    fn named_project_wins_over_workspace_root_when_both_present() {
        let identity = HttpSessionIdentity {
            profile: None,
            client: None,
            agent_identity_id: None,
            agent_identity_error: None,
            project: Some("sigil".to_string()),
            project_error: None,
            workspace_root: Some("/home/agent/repos/sigil".to_string()),
            workspace_root_error: None,
            dispatch_depth: None,
        };
        assert_eq!(
            project_binding_source(&identity),
            ProjectBindingSource::Named("sigil")
        );
    }

    #[test]
    fn workspace_root_used_only_when_project_absent() {
        let identity = HttpSessionIdentity {
            profile: None,
            client: None,
            agent_identity_id: None,
            agent_identity_error: None,
            project: None,
            project_error: None,
            workspace_root: Some("/home/agent/repos/sigil".to_string()),
            workspace_root_error: None,
            dispatch_depth: None,
        };
        assert_eq!(
            project_binding_source(&identity),
            ProjectBindingSource::WorkspaceRoot("/home/agent/repos/sigil")
        );
    }

    #[test]
    fn binding_source_is_none_when_neither_present() {
        let identity = HttpSessionIdentity::default();
        assert_eq!(
            project_binding_source(&identity),
            ProjectBindingSource::None
        );
    }

    #[test]
    fn tachi_memory_facade_is_not_destructive_after_maintenance_retirement() {
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
            Some(false),
            "tachi_memory no longer fronts delete or GC"
        );
    }

    fn annotated_destructive_hint(name: &str) -> Option<bool> {
        let mut tool: rmcp::model::Tool = serde_json::from_value(json!({
            "name": name,
            "description": format!("tool {name}"),
            "inputSchema": {
                "type": "object",
                "additionalProperties": true,
            }
        }))
        .expect("failed to build test tool");
        annotate_tool(&mut tool);
        tool.annotations.expect("annotations set").destructive_hint
    }

    /// #757 Cut3-S1 round-2 (review fixup): `tachi_sandbox` folds
    /// `sandbox_set_rule`/`sandbox_set_policy` (destructive actions per the
    /// alias manifest) among its five actions, so it was missing from the
    /// destructive match list entirely and fell to `destructive_hint=false`
    /// — a fail-open MCP client-facing hint. Assert the verb is annotated
    /// destructive.
    #[test]
    fn tachi_sandbox_facade_is_annotated_destructive() {
        assert_eq!(
            annotated_destructive_hint("tachi_sandbox"),
            Some(true),
            "tachi_sandbox must be destructive_hint=true (fronts set_rule/set_policy, both destructive)"
        );
    }

    /// The six legacy sandbox alias names are NOT in the tool-level
    /// destructive match list (and never were on main pre-fold — see the
    /// #757 fold history), so folding them into `tachi_sandbox` must not
    /// change their own annotated hint. This pins the alias-side "unchanged"
    /// half of the round-2 fix: only `tachi_sandbox` itself gained
    /// destructive_hint=true, the six aliases stay exactly as before.
    #[test]
    fn sandbox_aliases_keep_their_pre_fold_destructive_hint() {
        for legacy_name in [
            "sandbox_set_rule",
            "sandbox_check",
            "sandbox_set_policy",
            "sandbox_get_policy",
            "sandbox_list_policies",
            "sandbox_exec_audit",
        ] {
            assert_eq!(
                annotated_destructive_hint(legacy_name),
                Some(false),
                "legacy alias '{legacy_name}' must keep its pre-fold destructive_hint=false \
                 (tool-level annotation is unaware of the alias manifest's per-action \
                 destructive bit; this is documented as the S2+ direction, not fixed here)"
            );
        }
    }
}
