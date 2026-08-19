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
fn coordinate_surface_includes_memory_and_workflow_tools() {
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
    assert_eq!(
        names,
        vec![
            "tachi_memory".to_string(),
            "ingest_event".to_string(),
            "tachi_verify".to_string()
        ]
    );
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
    assert_eq!(
        names,
        vec!["tachi_memory".to_string(), "ingest_event".to_string()]
    );
}

#[test]
fn omitted_profile_defaults_to_standard_surface() {
    let filtered = filter_tool_defs(
        vec![
            test_tool("tachi_task"),
            test_tool("search_memory"),
            test_tool("save_memory"),
            test_tool("hub_register"),
        ],
        None,
        None,
    );
    let names: Vec<String> = filtered
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert_eq!(names, vec!["tachi_task".to_string()]);
}

#[test]
fn standard_profile_restricts_to_allow_list() {
    // tachi_gh is always visible (token checked at call time, not list time)
    let filtered = filter_tool_defs(
        vec![
            test_tool("tachi_tools"),
            test_tool("runtime_info"),
            test_tool("tachi_status"),
            test_tool("tachi_task"),
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
            "tachi_tools".to_string(),
            "runtime_info".to_string(),
            "tachi_status".to_string(),
            "tachi_task".to_string(),
            "tachi_verify".to_string(),
            "tachi_memory".to_string(),
            "tachi_web_search".to_string(),
            "tachi_wiki".to_string(),
            "tachi_skill".to_string(),
            "vault_unlock".to_string(),
            "vault_lock".to_string(),
            "vault_status".to_string(),
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
            // GH tools should be excluded even with token:
            test_tool("tachi_gh"),
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
            "tachi_web_search".to_string(),
            "tachi_wiki".to_string(),
            "tachi_unstick".to_string(),
            "tachi_task".to_string(),
            "tachi_skill".to_string(),
        ]
    );
    assert!(
        !names.iter().any(|n| n == "run_skill"),
        "run_skill must not appear on default delegate tray (#517 soft-deprecate)"
    );
}
