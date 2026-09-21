use super::*;

#[test]
fn profile_filter_and_env_whitelist_form_intersection() {
    let filtered = filter_tool_defs(
        vec![
            test_tool("search_memory"),
            test_tool("recall_context"),
            test_tool("post_card"),
        ],
        Some(ToolProfile::operate()),
        Some(&["search_memory".to_string(), "recall_*".to_string()]),
    );
    let names: Vec<String> = filtered
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert_eq!(names, vec!["recall_context".to_string()]);
}

#[test]
fn legacy_coordinate_selector_is_confined_to_product_facades() {
    let filtered = filter_tool_defs(
        vec![
            test_tool("tachi_memory"),
            test_tool("search_memory"),
            test_tool("save_memory"),
            test_tool("ingest_event"),
            test_tool("tachi_verify"),
            test_tool("post_card"),
            test_tool("hub_register"),
        ],
        Some(ToolProfile::coordinate()),
        None,
    );
    let names: Vec<String> = filtered
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert_eq!(names, vec!["tachi_memory".to_string()]);
}

#[test]
fn explicit_remember_surface_excludes_admin_tools() {
    let filtered = filter_tool_defs(
        vec![
            test_tool("tachi_memory"),
            test_tool("search_memory"),
            test_tool("save_memory"),
            test_tool("ingest_event"),
            test_tool("hub_register"),
        ],
        Some(ToolProfile::remember()),
        None,
    );
    let names: Vec<String> = filtered
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert_eq!(names, vec!["tachi_memory".to_string()]);
}

#[test]
fn every_legacy_ordinary_selector_discovers_exactly_the_five_facades() {
    let expected = vec![
        "tachi_memory".to_string(),
        "tachi_task".to_string(),
        "tachi_staff".to_string(),
        "tachi_gh".to_string(),
        "tachi_a2a".to_string(),
    ];
    for raw in [
        "observe",
        "read",
        "reader",
        "remember",
        "write",
        "writer",
        "agent",
        "coordinate",
        "observe+remember",
        "remember+observe",
        "reader+writer",
        "agent+read",
        "coordinate+observe",
        "observe+coordinate",
    ] {
        let profile = parse_tool_profile(raw)
            .unwrap_or_else(|| panic!("legacy ordinary selector {raw} should parse"));
        let names = filter_tool_defs(
            vec![
                test_tool("tachi_memory"),
                test_tool("tachi_task"),
                test_tool("tachi_staff"),
                test_tool("tachi_gh"),
                test_tool("tachi_a2a"),
                test_tool("runtime_info"),
                test_tool("tachi_status"),
                test_tool("tachi_verify"),
                test_tool("tachi_skill"),
                test_tool("hub_register"),
            ],
            Some(profile),
            None,
        )
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
        assert_eq!(names, expected, "legacy selector {raw}");
    }
}

#[test]
fn omitted_profile_defaults_to_standard_surface() {
    let filtered = filter_tool_defs(
        vec![
            test_tool("tachi_memory"),
            test_tool("tachi_task"),
            test_tool("tachi_staff"),
            test_tool("tachi_gh"),
            test_tool("tachi_a2a"),
            test_tool("tachi_tools"),
            test_tool("tachi_status"),
            test_tool("tachi_verify"),
            test_tool("tachi_web_search"),
            test_tool("tachi_wiki"),
            test_tool("tachi_skill"),
            test_tool("vault_status"),
        ],
        None,
        None,
    );
    let names: Vec<String> = filtered
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "tachi_memory".to_string(),
            "tachi_task".to_string(),
            "tachi_staff".to_string(),
            "tachi_gh".to_string(),
            "tachi_a2a".to_string(),
        ]
    );
}

#[test]
fn standard_profile_restricts_to_allow_list() {
    // tachi_gh is always visible (token checked at call time, not list time)
    let filtered = filter_tool_defs(
        vec![
            test_tool("tachi_tools"),
            test_tool("runtime_info"),
            test_tool("tachi_status"),
            test_tool("tachi_a2a"),
            test_tool("tachi_task"),
            test_tool("tachi_staff"),
            test_tool("tachi_verify"),
            test_tool("tachi_agent_eval"),
            test_tool("tachi_memory"),
            test_tool("tachi_event"),
            test_tool("tachi_domain_adapter"),
            test_tool("tachi_orchestrator"),
            test_tool("tachi_briefing"),
            test_tool("tachi_save"),
            test_tool("tachi_web_search"),
            test_tool("tachi_wiki"),
            test_tool("tachi_skill"),
            test_tool("vault_unlock"),
            test_tool("vault_lock"),
            test_tool("vault_status"),
            test_tool("tachi_gh"),
            test_tool("search_memory"),
            test_tool("save_memory"),
            test_tool("tachi_unstick"),
            test_tool("get_memory"),
            test_tool("post_card"),
            // Old tools that should be excluded from standard
            test_tool("tachi_search"),
            test_tool("tachi_handoff"),
            test_tool("recall_context"),
            test_tool("tachi_browse"),
            test_tool("hub_discover"),
            test_tool("run_skill"),
            test_tool("tachi_dispatch"),
            test_tool("tachi_complete"),
            test_tool("approve_merge"),
            test_tool("tachi_board"),
            test_tool("vault_get"),
            test_tool("vault_lease_api_key"),
            test_tool("vault_set_api_key_pool"),
            test_tool("vault_record_key_result"),
        ],
        Some(ToolProfile::standard()),
        None,
    );
    let names: Vec<String> = filtered
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "tachi_a2a".to_string(),
            "tachi_task".to_string(),
            "tachi_staff".to_string(),
            "tachi_memory".to_string(),
            "tachi_gh".to_string(),
        ]
    );
}

#[test]
fn delegate_profile_restricts_to_allow_list() {
    let filtered = filter_tool_defs(
        vec![
            // Delegate tools (should pass)
            test_tool("tachi_memory"),
            test_tool("tachi_web_search"),
            test_tool("tachi_wiki"),
            test_tool("tachi_unstick"),
            // F3: task facade is on the list; dispatch gated by action policy
            test_tool("tachi_task"),
            test_tool("tachi_staff"),
            test_tool("tachi_gh"),
            test_tool("tachi_a2a"),
            test_tool("tachi_skill"),
            // #517: standalone run_skill no longer on default delegate tray
            test_tool("run_skill"),
            test_tool("tachi_complete"),
            // Old tools that should be excluded from delegate
            test_tool("tachi_search"),
            test_tool("tachi_save"),
            test_tool("tachi_browse"),
            // Should be excluded:
            test_tool("tachi_handoff"),
            test_tool("tachi_dispatch"),
            test_tool("hub_discover"),
            test_tool("recall_context"),
            test_tool("search_memory"),
            // Raw Vault tools must stay admin-only for delegates.
            test_tool("vault_get"),
            test_tool("vault_lease_api_key"),
            test_tool("vault_set_api_key_pool"),
            test_tool("vault_record_key_result"),
        ],
        Some(ToolProfile::delegate()),
        None,
    );
    let names: Vec<String> = filtered
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "tachi_memory".to_string(),
            "tachi_task".to_string(),
            "tachi_staff".to_string(),
            "tachi_gh".to_string(),
            "tachi_a2a".to_string(),
        ]
    );
    assert!(
        !names.iter().any(|n| n == "run_skill"),
        "run_skill must not appear on default delegate tray (#517 soft-deprecate)"
    );
}

#[test]
fn explicit_ops_and_admin_profiles_retain_diagnostic_routes() {
    let tools = vec![
        test_tool("tachi_memory"),
        test_tool("runtime_info"),
        test_tool("tachi_status"),
        test_tool("vault_status"),
        test_tool("hub_register"),
    ];

    let ops = filter_tool_defs(
        tools.clone(),
        Some(parse_tool_profile("ops").expect("explicit Ops profile")),
        None,
    );
    let ops_names: Vec<String> = ops.into_iter().map(|tool| tool.name.into_owned()).collect();
    assert_eq!(
        ops_names,
        vec![
            "tachi_memory".to_string(),
            "runtime_info".to_string(),
            "tachi_status".to_string(),
            "vault_status".to_string(),
        ]
    );

    let admin = filter_tool_defs(
        tools,
        Some(parse_tool_profile("admin").expect("explicit admin profile")),
        None,
    );
    let admin_names: Vec<String> = admin
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert!(admin_names.iter().any(|name| name == "hub_register"));
}
