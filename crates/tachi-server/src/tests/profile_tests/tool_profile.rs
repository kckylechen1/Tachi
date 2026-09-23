use super::*;

#[tokio::test]
async fn f757_direct_add_edge_call_is_not_registered() {
    // #757: add_edge is internalized — missing from the router for every
    // profile (not merely profile-hidden). Discrimination: call still returns
    // tool-not-found rather than executing graph mutation.
    let server = make_server();
    // Admin profile would have exposed admin-only tools if they were registered.
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile should parse"),
    ));

    let result = call_tool_via_server(server, "add_edge", None)
        .await
        .expect("unregistered tool should return a tool-level error, not a transport error");

    assert_eq!(result.is_error, Some(true));
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(
        message.contains("tool not found"),
        "expected tool not found for internalized add_edge, got: {message}"
    );
}

#[tokio::test]
async fn unknown_tool_returns_tool_error_without_crashing_service() {
    let server = make_server();

    let result = call_tool_via_server(server, "tachi_tachi_tournament", None)
        .await
        .expect("unknown tool should not become a JSON-RPC transport error");

    assert_eq!(result.is_error, Some(true));
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(message.contains("tool not found"));
    assert!(message.contains("tachi_tachi_tournament"));
}

/// #1690 C3 slice A discriminator: the retired "second model brain" tool
/// family must be rejected by the MCP router as unknown tools — a typed
/// tool-level error, never silently routed or aliased to a surviving handler.
/// RED pre-fix (the routes exist, so a well-formed call routes and succeeds
/// with `is_error=false`), GREEN post-fix (no route → `tool not found`).
#[tokio::test]
async fn f1690_retired_recommend_family_is_rejected_by_router() {
    for tool_name in [
        "recommend_capability",
        "recommend_skill",
        "recommend_toolchain",
        "prepare_capability_bundle",
        // #1690 C3: skill_evolve joins the retired set — LLM telemetry-driven
        // skill versioning is gone end-to-end; distill_trajectory follows with
        // the trajectory→skill snapshot/registration pipeline.
        "skill_evolve",
        "distill_trajectory",
    ] {
        let server = make_server();
        // Admin profile sees every registered tool, so a routed (pre-fix) call
        // is not blocked by profile visibility — only router membership can
        // reject it.
        server.set_tool_profile(Some(
            tachi_hub::parse_tool_profile("admin").expect("admin profile should parse"),
        ));

        let mut args = serde_json::Map::new();
        args.insert("query".to_string(), serde_json::json!("incident"));

        let result = call_tool_via_server(server, tool_name, Some(args))
            .await
            .expect("retired tool should return a tool-level error, not a transport error");

        assert_eq!(
            result.is_error,
            Some(true),
            "'{tool_name}' must be rejected as an unknown tool post-#1690, got: {result:?}"
        );
        let message = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.as_str())
            .unwrap_or("");
        assert!(
            message.contains("tool not found"),
            "expected a typed tool-not-found error for '{tool_name}', got: {message}"
        );
        assert!(
            message.contains(tool_name),
            "rejection message should name the retired tool, got: {message}"
        );
    }
}

#[tokio::test]
async fn standard_profile_exposes_agent_intents_not_execution_internals() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("standard").expect("standard profile should parse"),
    ));

    let tools = server
        .tachi_tools()
        .await
        .expect("tool discovery should work");

    assert!(tools.contains("`tachi_memory`"));
    assert!(tools.contains("`tachi_task`"));
    assert!(tools.contains("`tachi_staff`"));
    assert!(tools.contains("`tachi_gh`"));
    assert!(tools.contains("`tachi_a2a`"));
    assert!(!tools.contains("`tachi_event`"));
    assert!(tools.contains("`tachi_agent_eval`"));
    assert!(!tools.contains("`tachi_verify`"));
    assert!(!tools.contains("`tachi_tools`"));
    assert!(!tools.contains("`tachi_status`"));
    assert!(!tools.contains("`tachi_web_search`"));
    assert!(!tools.contains("`tachi_wiki`"));
    assert!(!tools.contains("`tachi_skill`"));
    assert!(!tools.contains("`vault_status`"));
    assert!(!tools.contains("`tachi_orchestrator`"));

    let standard = tachi_hub::ToolProfile::standard();
    for action in [
        "register",
        "observe",
        "adjudicate",
        "get",
        "candidate_projection",
    ] {
        assert!(tachi_hub::facade_action_allowed(
            "tachi_agent_eval",
            Some(action),
            Some(standard)
        ));
    }
    for action in [
        "aggregate",
        "aggregate_live",
        "telemetry",
        "perf",
        "route_projection",
        "attach_session",
        "get_attachment",
        "",
        "future_action",
    ] {
        assert!(!tachi_hub::facade_action_allowed(
            "tachi_agent_eval",
            Some(action),
            Some(standard)
        ));
    }
}

#[tokio::test]
async fn standard_eval_tool_call_denies_operator_and_attachment_actions() {
    for action in [
        "aggregate",
        "route_projection",
        "attach_session",
        "get_attachment",
        "future_action",
    ] {
        let server = make_server();
        server.set_tool_profile(Some(tachi_hub::ToolProfile::standard()));
        let arguments = serde_json::json!({"action": action})
            .as_object()
            .expect("arguments object")
            .clone();
        let result = call_tool_via_server(server, "tachi_agent_eval", Some(arguments))
            .await
            .expect("denial should be a tool response");
        assert_eq!(result.is_error, Some(true), "{action} must be denied");
        let text = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|content| content.text.as_str())
            .unwrap_or("");
        assert!(text.contains("not allowed"), "{action} returned: {text}");
    }
}

#[tokio::test]
async fn coordinate_profile_is_confined_to_product_facades() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("coordinate").expect("coordinate profile should parse"),
    ));

    let tools = server
        .tachi_tools()
        .await
        .expect("tool discovery should work");

    assert!(tools.contains("`tachi_staff`"));
    assert!(!tools.contains("`tachi_agent_eval`"));
    assert!(!tools.contains("`tachi_verify`"));
    assert!(!tools.contains("`tachi_orchestrator`"));
}

/// #1935: Worker discovery matches Lead, while action policy still prevents a
/// worker from recursively starting or cancelling staff.
#[tokio::test]
async fn delegate_profile_exposes_staff_status_but_denies_recursive_staffing() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("delegate").expect("delegate profile should parse"),
    ));

    let tools = server
        .tachi_tools()
        .await
        .expect("tool discovery should work");

    assert!(tools.contains("`tachi_staff`"));

    for action in ["start", "cancel"] {
        let mut args = serde_json::Map::new();
        args.insert("action".to_string(), serde_json::json!(action));
        let result = call_tool_on_server(server.clone(), "tachi_staff", Some(args))
            .await
            .expect("denied staffing action should return a tool result");
        assert_eq!(result.is_error, Some(true));
        let message = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.as_str())
            .unwrap_or("");
        assert!(message.contains("not allowed"), "{message}");
    }
}

#[tokio::test]
async fn additive_worker_projection_matches_its_call_time_policy() {
    fn action_names(
        tools: &[rmcp::model::Tool],
        tool_name: &str,
    ) -> std::collections::BTreeSet<String> {
        tools
            .iter()
            .find(|tool| tool.name.as_ref() == tool_name)
            .unwrap_or_else(|| panic!("missing projected tool {tool_name}"))
            .input_schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap_or_else(|| panic!("missing projected action enum for {tool_name}"))
            .iter()
            .map(|action| {
                action
                    .as_str()
                    .expect("projected action must be a string")
                    .to_string()
            })
            .collect()
    }

    fn projected(profile: tachi_hub::ToolProfile) -> Vec<rmcp::model::Tool> {
        let tools = crate::server_handler::prepare_native_tool_definitions(
            super::tool_profile_router_coverage::native_route_definitions(),
        );
        crate::server_handler::project_tool_definitions(tools, Some(profile), None)
    }

    let worker = tachi_hub::parse_tool_profile("delegate+operate")
        .expect("additive Worker selector should parse");
    let worker_tools = tokio::task::spawn_blocking(move || projected(worker))
        .await
        .expect("project additive Worker tools");
    let worker_names = worker_tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        worker_names,
        std::collections::BTreeSet::from([
            "tachi_a2a",
            "tachi_gh",
            "tachi_memory",
            "tachi_staff",
            "tachi_task",
        ])
    );
    assert_eq!(
        action_names(&worker_tools, "tachi_staff"),
        std::collections::BTreeSet::from(["status".to_string()])
    );
    assert_eq!(
        action_names(&worker_tools, "tachi_gh"),
        std::collections::BTreeSet::from([
            "issue_freshness_scan".to_string(),
            "issue_list".to_string(),
            "issue_read".to_string(),
            "pr_comments".to_string(),
            "pr_list".to_string(),
            "pr_read".to_string(),
            "pr_review_digest".to_string(),
            "pr_status".to_string(),
            "repo_view".to_string(),
        ])
    );

    let server = make_server();
    server.set_tool_profile(Some(worker));
    for (tool, action) in [("tachi_staff", "start"), ("tachi_gh", "safe_merge")] {
        let result = call_tool_on_server(
            server.clone(),
            tool,
            Some(serde_json::Map::from_iter([(
                "action".to_string(),
                serde_json::json!(action),
            )])),
        )
        .await
        .expect("denied additive Worker action should return a tool result");
        let message = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.as_str())
            .unwrap_or("");
        assert_eq!(result.is_error, Some(true), "{tool}:{action}: {message}");
        assert!(
            message.contains("not allowed"),
            "{tool}:{action}: {message}"
        );
    }

    let lead_worker = tachi_hub::parse_tool_profile("standard+delegate")
        .expect("additive Lead selector should parse");
    let (lead_worker_tools, standard_tools) = tokio::task::spawn_blocking(move || {
        (
            projected(lead_worker),
            projected(tachi_hub::ToolProfile::standard()),
        )
    })
    .await
    .expect("project additive and ordinary Lead tools");
    for tool in ["tachi_staff", "tachi_gh"] {
        assert_eq!(
            action_names(&lead_worker_tools, tool),
            action_names(&standard_tools, tool),
            "standard allow-list precedence must govern {tool} schemas"
        );
    }
    assert!(action_names(&lead_worker_tools, "tachi_staff").contains("start"));
    assert!(action_names(&lead_worker_tools, "tachi_gh").contains("safe_merge"));
    let ordinary_tool_names = standard_tools
        .iter()
        .map(|tool| tool.name.as_ref().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let ordinary_action_names = ordinary_tool_names
        .iter()
        .flat_map(|tool_name| action_names(&standard_tools, tool_name))
        .collect::<std::collections::BTreeSet<_>>();
    let registered_route_names = tokio::task::spawn_blocking(|| {
        crate::server_handler::prepare_native_tool_definitions(
            super::tool_profile_router_coverage::native_route_definitions(),
        )
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<std::collections::BTreeSet<_>>()
    })
    .await
    .expect("collect registered route names");
    let residual_route_names = registered_route_names
        .into_iter()
        .filter(|name| !ordinary_tool_names.contains(name) && !ordinary_action_names.contains(name))
        .collect::<std::collections::BTreeSet<_>>();
    for (profile, tools) in [
        ("Lead", standard_tools.as_slice()),
        ("Worker", worker_tools.as_slice()),
    ] {
        for tool in tools {
            let description = tool.description.as_deref().unwrap_or_default();
            let input_schema = serde_json::to_string(&tool.input_schema)
                .expect("ordinary projected input schema must serialize");
            for residual in &residual_route_names {
                assert!(
                    !description.contains(residual) && !input_schema.contains(residual),
                    "ordinary {profile} {} definition advertises residual route {residual}; description={description}; input_schema={input_schema}",
                    tool.name,
                );
            }
        }
    }

    let (prepared, admin_tools) = tokio::task::spawn_blocking(|| {
        let prepared = crate::server_handler::prepare_native_tool_definitions(
            super::tool_profile_router_coverage::native_route_definitions(),
        );
        let admin_tools = crate::server_handler::project_tool_definitions(
            prepared.clone(),
            Some(tachi_hub::ToolProfile::admin()),
            None,
        );
        (prepared, admin_tools)
    })
    .await
    .expect("project admin tools");
    for tool_name in ["tachi_staff", "tachi_gh"] {
        let before = prepared
            .iter()
            .find(|tool| tool.name.as_ref() == tool_name)
            .expect("prepared admin tool");
        let after = admin_tools
            .iter()
            .find(|tool| tool.name.as_ref() == tool_name)
            .expect("projected admin tool");
        assert_eq!(
            before.input_schema, after.input_schema,
            "admin {tool_name} schema must remain unchanged"
        );
    }
}

/// #1319-C2: `action='dispatch'` was removed from `tachi_task` wholesale
/// (the worker launch lifecycle left Task). It must now be rejected for a
/// delegate worker at the param-parse layer — no profile, not even admin,
/// can dispatch via `tachi_task`. This supersedes the old #919 operator-only
/// gate (which denied dispatch to non-admin profiles); the action is gone
/// entirely rather than gated. End-to-end via `call_tool` so a future
/// regression that re-adds the variant without re-routing fails loudly.
#[tokio::test]
async fn delegate_tachi_task_dispatch_is_rejected_end_to_end() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("delegate").expect("delegate profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("dispatch"));
    args.insert("agent".to_string(), serde_json::json!("claude"));
    args.insert(
        "task".to_string(),
        serde_json::json!("recursive dispatch attempt"),
    );

    let result = call_tool_via_server(server, "tachi_task", Some(args))
        .await
        .expect("rejected action should return a tool result, not a transport error");

    assert_eq!(
        result.is_error,
        Some(true),
        "delegate must not be able to dispatch via tachi_task"
    );
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(
        message.contains("dispatch"),
        "rejection message should name the removed action, got: {message}"
    );
    // Not a "tool not found" — the tool IS visible to delegate; only the
    // dispatch *action* is rejected.
    assert!(!message.contains("tool not found"));
}

/// #1319-C2: dispatch is gone for every profile, including admin. The old
/// operator-only distinction no longer exists for dispatch — it is rejected
/// at the param-parse layer regardless of profile.
#[tokio::test]
async fn standard_tachi_task_dispatch_is_rejected_end_to_end() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("standard").expect("standard profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("dispatch"));
    args.insert("agent".to_string(), serde_json::json!("opencode"));
    args.insert(
        "dispatch_reason".to_string(),
        serde_json::json!("explicit_user_request"),
    );
    args.insert(
        "task".to_string(),
        serde_json::json!("attempt Tachi-owned launch from a daily agent profile"),
    );

    let result = call_tool_via_server(server, "tachi_task", Some(args))
        .await
        .expect("rejected action should return a tool result");

    assert_eq!(result.is_error, Some(true));
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(message.contains("dispatch"), "{message}");
}

/// Positive control for the RED test above: the same delegate profile CAN
/// call a permitted `tachi_task` action end-to-end (the gate isn't just
/// denying everything).
#[tokio::test]
async fn delegate_tachi_task_board_is_allowed_end_to_end() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("delegate").expect("delegate profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("board"));

    let result = call_tool_via_server(server, "tachi_task", Some(args))
        .await
        .expect("permitted action should return a tool result");

    assert_ne!(
        result.is_error,
        Some(true),
        "delegate should be able to call tachi_task(action='board')"
    );
}

#[tokio::test]
async fn delegate_tachi_task_handoff_is_denied_end_to_end() {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("delegate").expect("delegate profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("handoff"));
    args.insert("claim_id".to_string(), serde_json::json!("claim-1"));
    args.insert("transition_version".to_string(), serde_json::json!(0));

    let result = call_tool_via_server(server, "tachi_task", Some(args))
        .await
        .expect("denied action should return a tool result, not a transport error");

    assert_eq!(
        result.is_error,
        Some(true),
        "delegate must not be able to hand off via tachi_task"
    );
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(
        message.contains("not allowed") || message.contains("not available"),
        "expected a permission-denied message, got: {message}"
    );
    assert!(
        message.contains("handoff"),
        "denial message should name the denied action, got: {message}"
    );
    assert!(!message.contains("tool not found"));
}

#[tokio::test]
async fn session_spine_mutation_profile_gate_runs_at_real_tools_call_boundary() {
    let call = |profile: &'static str| async move {
        let server = make_server();
        server.set_tool_profile(Some(
            tachi_hub::parse_tool_profile(profile).expect("profile should parse"),
        ));
        let mut args = serde_json::Map::new();
        args.insert(
            "action".to_string(),
            serde_json::json!("request_intervention"),
        );
        call_tool_via_server(server, "tachi_agent_eval", Some(args))
            .await
            .expect("routed tool call must return a tool result")
    };

    let denied = call("observe").await;
    let denied_message = denied
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert_eq!(denied.is_error, Some(true));
    assert!(
        denied_message.contains("tool not found"),
        "{denied_message}"
    );

    let admitted = call("admin").await;
    let admitted_message = admitted
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert_eq!(
        admitted.is_error,
        Some(true),
        "missing action payload should fail after authorization"
    );
    assert!(
        !admitted_message.contains("not allowed")
            && !admitted_message.contains("not available")
            && !admitted_message.contains("tool not found"),
        "admin call must reach parameter validation: {admitted_message}"
    );
    assert_eq!(
        admitted_message, "current host admission is unavailable",
        "admin call must reach the production host-admission gate"
    );
}

#[tokio::test]
async fn tachi_tune_routes_through_composed_router_for_admin() {
    // #1426 dead-facade regression discriminator: tachi_tune was once
    // registered but never summed into the composed tool router, so a real
    // routed CallToolRequest fell through to tool-not-found while direct
    // handler tests stayed green. This is the only tune happy path that
    // exercises the server_handler router; keep it routed, not direct.
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("admin").expect("admin profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("route_proposals"));

    let result = call_tool_via_server(server, "tachi_tune", Some(args))
        .await
        .expect("routed tachi_tune should return a tool result, not a transport error");

    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(
        !message.contains("tool not found"),
        "tachi_tune fell out of the composed router again (#1426): {message}"
    );
    assert_ne!(
        result.is_error,
        Some(true),
        "routed route_proposals on a fresh store should succeed, got: {message}"
    );
}

#[tokio::test]
async fn tachi_tune_is_invisible_to_standard_profile() {
    // Admin-by-omission: tachi_tune joins no bundle, so every non-admin
    // profile must refuse the routed call outright (tool_visible gate),
    // indistinguishable from an unregistered tool.
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile("standard").expect("standard profile should parse"),
    ));

    let mut args = serde_json::Map::new();
    args.insert("action".to_string(), serde_json::json!("route_proposals"));

    let result = call_tool_via_server(server, "tachi_tune", Some(args))
        .await
        .expect("invisible tool should return a tool-level error, not a transport error");

    assert_eq!(result.is_error, Some(true));
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    assert!(
        message.contains("tool not found"),
        "expected visibility refusal for standard-profile tachi_tune, got: {message}"
    );
}
