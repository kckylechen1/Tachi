use super::*;
use serde_json::json;

fn native_tools() -> Vec<rmcp::model::Tool> {
    MemoryServer::native_tool_router().list_all()
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
fn projected_agent_eval_schema_matches_session_spine_action_gate() {
    let agent_eval_tool = || {
        native_tools()
            .into_iter()
            .find(|tool| tool.name.as_ref() == "tachi_agent_eval")
            .expect("native tachi_agent_eval tool")
    };

    let mut observe = vec![agent_eval_tool()];
    narrow_gated_action_schemas(&mut observe, Some(tachi_hub::ToolProfile::observe()));
    let observe_actions = observe[0].input_schema["properties"]["action"]["enum"]
        .as_array()
        .expect("observe agent-eval action enum");
    assert!(observe_actions.contains(&json!("get_session_state")));
    for denied in [
        "attach_session",
        "get_attachment",
        "ingest_session_event",
        "mark_session_connection",
        "reconnect_session",
        "advertise_session_capabilities",
        "request_intervention",
        "record_intervention_result",
    ] {
        assert!(
            !observe_actions.contains(&json!(denied)),
            "observe schema must hide denied action {denied}"
        );
    }

    let mut coordinate = vec![agent_eval_tool()];
    narrow_gated_action_schemas(&mut coordinate, Some(tachi_hub::ToolProfile::coordinate()));
    let coordinate_actions = coordinate[0].input_schema["properties"]["action"]["enum"]
        .as_array()
        .expect("coordinate agent-eval action enum");
    for allowed in [
        "attach_session",
        "ingest_session_event",
        "request_intervention",
        "record_intervention_result",
    ] {
        assert!(
            coordinate_actions.contains(&json!(allowed)),
            "coordinate schema must retain action {allowed}"
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
    // #1319-C2 guard, fixed: forbid the retired launch/dispatch AUTHORITY
    // (the dispatch_reason admission gate and the removed action=dispatch)
    // while still allowing the legitimate read field `dispatch_id` to be
    // documented for action='status'. A blanket `!contains("dispatch")`
    // would ban correct product guidance for a read-only query field.
    let standard_description = standard[0].description.as_deref().unwrap_or_default();
    assert!(
        standard_description.contains("dispatch_id"),
        "standard task description must document the dispatch_id status read: {standard_description}"
    );
    for retired in ["action=dispatch", "dispatch_reason", "spawned agent"] {
        assert!(
            !standard_description.contains(retired),
            "standard task description must not advertise retired launch authority \
             '{retired}': {standard_description}"
        );
    }
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
        // Fixed #1319-C2 guard: retired launch authority stays banned, but a
        // profile that can call status may see the dispatch_id read field.
        let description = tools[0].description.as_deref().unwrap_or_default();
        for retired in ["action=dispatch", "dispatch_reason", "spawned agent"] {
            assert!(
                !description.contains(retired),
                "non-admin task description must not advertise retired launch authority \
                 '{retired}': {description}"
            );
        }
        if actions.contains(&json!("status")) {
            assert!(
                description.contains("dispatch_id"),
                "a status-capable profile must see the dispatch_id read field: {description}"
            );
        }
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
    //     (tachi_component's own live action='plan', tachi_memory's "merge
    //     duplicates", dispatch's "auto-merge worktrees") — scanning
    //     those tools for the bare words would be a false-positive
    //     factory, not a real regression signal.
    fn leaks_retired_token(description: &str, token: &str) -> bool {
        description
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|word| word == token)
    }

    const CROSS_TOOL_UNAMBIGUOUS_TOKENS: &[&str] = &["cycle_plan", "refine_issues", "ux_matrix"];

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
        tachi_hub::ToolProfile::standard(),
        tachi_hub::ToolProfile::delegate(),
    ] {
        let projected = project_tool_definitions(native_tools(), Some(profile), None);
        let task_tool = projected
            .iter()
            .find(|tool| tool.name.as_ref() == "tachi_task")
            .expect("tachi_task survives projection for every non-admin profile");
        let description = task_tool.description.as_deref().unwrap_or_default();
        let enum_actions: std::collections::HashSet<&str> = task_tool.input_schema["properties"]
            ["action"]["enum"]
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
        let enum_actions: std::collections::HashSet<&str> = wiki_tool.input_schema["properties"]
            ["action"]["enum"]
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
fn delegate_projection_excludes_residual_wiki_facade() {
    let projected = project_tool_definitions(
        native_tools(),
        Some(tachi_hub::ToolProfile::delegate()),
        None,
    );
    assert!(
        projected
            .iter()
            .all(|tool| tool.name.as_ref() != "tachi_wiki"),
        "tachi_wiki must not appear in Worker discovery"
    );
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
    let error = rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text("boom")]);
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
    let meta = rmcp::model::RequestMetaObject::from(map);
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
    let meta = rmcp::model::RequestMetaObject::from(map);
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
        let identity =
            identity_from_initialize_meta(Some(&rmcp::model::RequestMetaObject::from(map)));
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
        let identity =
            identity_from_initialize_meta(Some(&rmcp::model::RequestMetaObject::from(map)));
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
    let meta = rmcp::model::RequestMetaObject::from(map);
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
    let meta = rmcp::model::RequestMetaObject::from(map);
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
    let meta = rmcp::model::RequestMetaObject::from(map);
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
    let meta = rmcp::model::RequestMetaObject::from(map);
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
    let meta = rmcp::model::RequestMetaObject::from(map);
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
        internal_proxy_token: None,
        owner_tool: None,
        owner_tool_error: None,
        client: None,
        agent_identity_id: None,
        agent_identity_error: None,
        project: Some("sigil".to_string()),
        project_error: None,
        workspace_root: Some("/home/agent/repos/sigil".to_string()),
        workspace_root_error: None,
        dispatch_depth: None,
        rate_limit_session: None,
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
        internal_proxy_token: None,
        owner_tool: None,
        owner_tool_error: None,
        client: None,
        agent_identity_id: None,
        agent_identity_error: None,
        project: None,
        project_error: None,
        workspace_root: Some("/home/agent/repos/sigil".to_string()),
        workspace_root_error: None,
        dispatch_depth: None,
        rate_limit_session: None,
    };
    assert_eq!(
        project_binding_source(&identity),
        ProjectBindingSource::WorkspaceRoot("/home/agent/repos/sigil")
    );
}

/// Audit C1: a modern request's derived bucket is stable for the same
/// resolved identity, distinct across identities, opaque, and absent when
/// the request carries no AgentIdentity (per-request fallback), even if a
/// client label or project is present.
#[test]
fn modern_identity_rate_limit_bucket_is_stable_per_identity() {
    let a = modern_identity_rate_limit_session_id(Some("agent.a"), Some("cli"), Some("sigil"))
        .expect("identity bucket");
    assert_eq!(
        Some(a.clone()),
        modern_identity_rate_limit_session_id(Some("agent.a"), Some("cli"), Some("sigil"))
    );
    assert!(a.starts_with("identity:"), "{a}");
    assert!(!a.contains("agent.a"), "bucket id must stay opaque: {a}");
    for other in [
        modern_identity_rate_limit_session_id(Some("agent.b"), Some("cli"), Some("sigil")),
        modern_identity_rate_limit_session_id(Some("agent.a"), Some("ide"), Some("sigil")),
        modern_identity_rate_limit_session_id(Some("agent.a"), Some("cli"), None),
    ] {
        assert_ne!(other.as_deref(), Some(a.as_str()));
    }
    for anonymous in [
        modern_identity_rate_limit_session_id(None, None, None),
        modern_identity_rate_limit_session_id(None, Some("cli"), None),
        modern_identity_rate_limit_session_id(None, Some("cli"), Some("sigil")),
        modern_identity_rate_limit_session_id(None, None, Some("sigil")),
    ] {
        assert_eq!(anonymous, None);
    }
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
/// alias manifest) among its six actions, so it was missing from the
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

// ── P1 result semantics: failure guidance at the server boundary ────────

fn guidance_args(action: Option<&str>) -> serde_json::Map<String, serde_json::Value> {
    let mut args = serde_json::Map::new();
    if let Some(action) = action {
        args.insert("action".to_string(), json!(action));
    }
    args
}

fn attach_to_err(
    tool_name: &str,
    action: Option<&str>,
    data: Option<serde_json::Value>,
) -> rmcp::ErrorData {
    let args = guidance_args(action);
    let mut result = Err(rmcp::ErrorData::internal_error(
        "handler exploded".to_string(),
        data,
    ));
    attach_failed_invocation_guidance(&mut result, tool_name, Some(&args));
    result.expect_err("attachment must not turn an error into success")
}

/// Err(ErrorData) channel: guidance is attached, and the original
/// code/message and structured data — including a canonical commit
/// receipt — survive verbatim.
#[test]
fn err_channel_attaches_guidance_and_preserves_original_structure() {
    let error = attach_to_err(
        "tachi_memory",
        Some("get"),
        Some(json!({
            "commit_receipt": {"id": "receipt-1", "revision": 7},
            "detail": "low-level failure",
        })),
    );
    assert_eq!(error.message, "handler exploded");
    let data = error.data.as_ref().expect("error data present");
    assert_eq!(
        data["commit_receipt"],
        json!({"id": "receipt-1", "revision": 7}),
        "canonical commit receipt must be untouched"
    );
    assert_eq!(data["detail"], json!("low-level failure"));
    assert_eq!(
        data["tachi_invocation_guidance"]["effect_policy"],
        json!("read_only")
    );
    assert_eq!(
        data["tachi_invocation_guidance"]["replay_authorization"]["authorized"],
        json!(true)
    );
}

/// A post-commit mutating failure must never advertise retry safety.
#[test]
fn err_channel_mutating_failure_never_authorizes_replay() {
    let error = attach_to_err("tachi_memory", Some("save"), None);
    let data = error.data.as_ref().expect("error data present");
    let guidance = &data["tachi_invocation_guidance"];
    assert_eq!(guidance["effect_policy"], json!("may_mutate"));
    assert_eq!(guidance["replay_authorization"]["authorized"], json!(false));
    assert_eq!(guidance["actual_effects"], json!("not_established"));
    let serialized = data.to_string();
    assert!(!serialized.contains("retry_safe"), "{serialized}");
    assert!(!serialized.contains("rollback"), "{serialized}");
}

/// A payload that pre-populates our guidance key must not be able to
/// supply local replay authority: our computed value wins, and the
/// producer's value is preserved but clearly subordinate.
#[test]
fn payload_collision_cannot_borrow_local_replay_authority() {
    let error = attach_to_err(
        "tachi_memory",
        Some("save"),
        Some(json!({
            "tachi_invocation_guidance": {
                "replay_authorization": {"authorized": true},
                "actual_effects": "none",
            }
        })),
    );
    let data = error.data.as_ref().expect("error data present");
    let guidance = &data["tachi_invocation_guidance"];
    assert_eq!(
        guidance["replay_authorization"]["authorized"],
        json!(false),
        "a payload-supplied claim must never become local authority"
    );
    assert_eq!(guidance["actual_effects"], json!("not_established"));
    assert_eq!(
        guidance["producer_supplied_guidance"]["replay_authorization"]["authorized"],
        json!(true),
        "the foreign value is preserved, not silently dropped"
    );
}

/// Non-object error data is preserved under `original_error_data` rather
/// than discarded.
#[test]
fn err_channel_non_object_data_is_preserved() {
    let error = attach_to_err("tachi_memory", Some("get"), Some(json!("plain string")));
    let data = error.data.as_ref().expect("error data present");
    assert_eq!(data["original_error_data"], json!("plain string"));
    assert_eq!(
        data["tachi_invocation_guidance"]["effect_policy"],
        json!("read_only")
    );
}

/// Proxy-qualified and unknown routes have no local authority, so their
/// errors carry no server-authored annotation: the payload is returned
/// byte-for-byte (here, `None` stays `None`).
#[test]
fn err_channel_unknown_and_proxy_routes_are_not_annotated() {
    for tool in [
        "remote__tachi_memory",
        "some_other_servers_tool",
        "newly_registered_mutation",
    ] {
        let error = attach_to_err(tool, Some("get"), None);
        assert_eq!(
            error.data, None,
            "{tool} must keep its error payload byte-for-byte; no local authority exists"
        );
    }
}

/// A proxy/unknown error that already carries structured data — including a
/// payload-supplied guidance key — is preserved verbatim. The server must
/// neither borrow authority nor even wrap the foreign value.
#[test]
fn err_channel_proxy_route_preserves_foreign_payload_verbatim() {
    let foreign = json!({
        "tachi_invocation_guidance": {
            "replay_authorization": {"authorized": true},
            "actual_effects": "none",
        },
        "detail": "remote detail",
    });
    let error = attach_to_err("remote__tachi_memory", Some("get"), Some(foreign.clone()));
    assert_eq!(
        error.data,
        Some(foreign),
        "a proxy error payload must pass through unchanged"
    );
}

/// Ok(is_error=true) channel: guidance is appended as its own content
/// block, the error flag is preserved, and the original content survives.
#[test]
fn is_error_result_channel_appends_guidance_block() {
    let mut result = Ok(rmcp::model::CallToolResult::error(vec![
        rmcp::model::ContentBlock::text("original failure"),
    ]));
    attach_failed_invocation_guidance(
        &mut result,
        "tachi_memory",
        Some(&guidance_args(Some("save"))),
    );
    let response = result.expect("still Ok");
    assert_eq!(response.is_error, Some(true));
    let texts: Vec<String> = response
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect();
    assert_eq!(texts.first().map(String::as_str), Some("original failure"));
    let guidance_block = texts
        .iter()
        .find(|text| text.contains("tachi_invocation_guidance"))
        .expect("guidance block appended");
    let parsed: serde_json::Value =
        serde_json::from_str(guidance_block).expect("guidance block is JSON");
    assert_eq!(
        parsed["tachi_invocation_guidance"]["replay_authorization"]["authorized"],
        json!(false)
    );
}

/// Ok(is_error=true) channel, local read-only route: guidance is still
/// attached (read-only half of the retained both-channel coverage).
#[test]
fn is_error_result_channel_attaches_guidance_for_read_only_local_route() {
    let mut result = Ok(rmcp::model::CallToolResult::error(vec![
        rmcp::model::ContentBlock::text("original failure"),
    ]));
    attach_failed_invocation_guidance(
        &mut result,
        "tachi_memory",
        Some(&guidance_args(Some("get"))),
    );
    let response = result.expect("still Ok");
    let texts: Vec<String> = response
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect();
    let guidance_block = texts
        .iter()
        .find(|text| text.contains("tachi_invocation_guidance"))
        .expect("read-only local route still gets guidance");
    let parsed: serde_json::Value =
        serde_json::from_str(guidance_block).expect("guidance block is JSON");
    assert_eq!(
        parsed["tachi_invocation_guidance"]["effect_policy"],
        json!("read_only")
    );
    assert_eq!(
        parsed["tachi_invocation_guidance"]["replay_authorization"]["authorized"],
        json!(true)
    );
}

/// Ok(is_error=true) channel with no local authority: no guidance block is
/// appended and the original content survives verbatim.
#[test]
fn is_error_result_channel_does_not_annotate_unknown_or_proxy_routes() {
    for tool in ["remote__tachi_memory", "some_other_servers_tool"] {
        let mut result = Ok(rmcp::model::CallToolResult::error(vec![
            rmcp::model::ContentBlock::text("original failure"),
        ]));
        attach_failed_invocation_guidance(&mut result, tool, Some(&guidance_args(Some("get"))));
        let response = result.expect("still Ok");
        assert_eq!(response.is_error, Some(true));
        let texts: Vec<String> = response
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect();
        assert_eq!(
            texts,
            vec!["original failure".to_string()],
            "{tool} must not gain an authored guidance block"
        );
    }
}
#[test]
fn successful_result_is_unchanged() {
    let mut result = Ok(rmcp::model::CallToolResult::success(vec![
        rmcp::model::ContentBlock::text("all good"),
    ]));
    let before = format!("{result:?}");
    attach_failed_invocation_guidance(
        &mut result,
        "tachi_memory",
        Some(&guidance_args(Some("get"))),
    );
    assert_eq!(format!("{result:?}"), before);
}
