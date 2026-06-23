const ADMIN_ONLY_NATIVE_ROUTE_NAMES: &[&str] = &[
    "add_edge",
    "chain_skills",
    "cyberbrain_search",
    "cyberbrain_write",
    "delete_domain",
    "delete_memory",
    "distill_trajectory",
    "dlq_list",
    "dlq_retry",
    "get_domain",
    "get_state",
    "hub_export_skills",
    "hub_feedback",
    "hub_get",
    "hub_quick_add",
    "hub_register",
    "hub_review",
    "hub_set_active_version",
    "hub_set_enabled",
    "hub_stats",
    "ingest",
    "ingest_source",
    "list_domains",
    "memory_gc",
    "pack_get",
    "pack_list",
    "pack_project",
    "pack_register",
    "pack_remove",
    "projection_list",
    "register_domain",
    "sandbox_check",
    "sandbox_exec_audit",
    "sandbox_get_policy",
    "sandbox_list_policies",
    "sandbox_set_policy",
    "sandbox_set_rule",
    "section9_audit_log",
    "section9_review",
    "set_state",
    "shell_exec_audit",
    "shell_get_policy",
    "shell_list_policies",
    "shell_set_policy",
    "skill_evolve",
    "tachi_audit_log",
    "tachi_init_project_db",
    "tachi_wiki_organize",
    "vault_get",
    "vault_init",
    "vault_list",
    "vault_remove",
    "vault_set",
    "vault_set_api_key_pool",
    "vault_lease_api_key",
    "vault_record_key_result",
    "vault_setup_rotation",
    "vc_bind",
    "vc_list",
    "vc_register",
    "vc_resolve",
];

const NON_ADMIN_WRITE_ROUTE_NAMES: &[&str] = &[
    "archive_memory",
    "capture_session",
    "compact_rollup",
    "compact_session_memory",
    "extract_facts",
    "handoff_check",
    "handoff_leave",
    "ingest_event",
    "post_card",
    "project_agent_profile",
    "queue_agent_evolution",
    "remember",
    "review_agent_evolution_proposal",
    "save_memory",
    "sync_memories",
    "synthesize_agent_evolution",
    "tachi_complete",
    "tachi_handoff",
    "tachi_memory",
    "tachi_orchestrator",
    "tachi_arena",
    "tachi_verify",
    "tachi_save",
    "tachi_wiki_write",
    "update_card",
];

use super::*;
use rmcp::model::Tool;
use std::collections::BTreeSet;

fn ensure_test_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("VOYAGE_API_KEY", "test-voyage-key");
        std::env::set_var("SILICONFLOW_API_KEY", "test-siliconflow-key");
        std::env::set_var("SILICONFLOW_MODEL", "test-model");
        std::env::set_var("SUMMARY_MODEL", "test-summary-model");
        std::env::set_var("TACHI_DISABLE_PATH_VALIDATION", "1");
    });
}

fn native_route_names() -> Vec<String> {
    ensure_test_env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("profiles test runtime");
    let _guard = runtime.enter();
    let db_path = std::env::temp_dir().join(format!(
        "profiles-metadata-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = crate::MemoryServer::new(db_path, None).expect("test memory server");
    let mut names: Vec<String> = server
        .tool_router
        .list_all()
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    names.sort();
    names
}

fn bundle_count(tool_name: &str) -> usize {
    [
        ToolBundle::Observe,
        ToolBundle::Remember,
        ToolBundle::Coordinate,
        ToolBundle::Operate,
    ]
    .into_iter()
    .filter(|bundle| tool_matches_bundle(tool_name, *bundle))
    .count()
}

fn test_tool(name: &str) -> Tool {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "description": format!("tool {name}"),
        "inputSchema": {
            "type": "object",
            "additionalProperties": true,
        }
    }))
    .expect("test tool")
}

#[test]
fn profile_parsing_maps_host_aliases() {
    // IDE + CLI → standard
    assert_eq!(parse_tool_profile("codex"), Some(ToolProfile::standard()));
    assert_eq!(parse_tool_profile("cursor"), Some(ToolProfile::standard()));
    assert_eq!(
        parse_tool_profile("windsurf"),
        Some(ToolProfile::standard())
    );
    assert_eq!(
        parse_tool_profile("antigravity"),
        Some(ToolProfile::standard())
    );
    assert_eq!(
        parse_tool_profile("claude-code"),
        Some(ToolProfile::standard())
    );
    assert_eq!(parse_tool_profile("ide"), Some(ToolProfile::standard()));
    // Worker agents → delegate
    assert_eq!(
        parse_tool_profile("delegate"),
        Some(ToolProfile::delegate())
    );
    assert_eq!(parse_tool_profile("worker"), Some(ToolProfile::delegate()));
    assert_eq!(
        parse_tool_profile("subagent"),
        Some(ToolProfile::delegate())
    );
    // Framework agents → operate
    assert_eq!(parse_tool_profile("openclaw"), Some(ToolProfile::operate()));
    assert_eq!(parse_tool_profile("hermes"), Some(ToolProfile::operate()));
    assert_eq!(
        parse_tool_profile("companion"),
        Some(
            ToolProfile::remember()
                .merge(ToolProfile::coordinate())
                .merge(ToolProfile::operate())
        )
    );
    assert_eq!(
        parse_tool_profile("workflow"),
        Some(ToolProfile::coordinate().merge(ToolProfile::operate()))
    );
    assert_eq!(parse_tool_profile("admin"), Some(ToolProfile::admin()));
}

#[test]
fn profile_parsing_supports_additive_surface_tokens() {
    assert_eq!(
        parse_tool_profile("observe,coordinate"),
        Some(ToolProfile::observe().merge(ToolProfile::coordinate()))
    );
    assert_eq!(
        parse_tool_profile("remember+operate"),
        Some(ToolProfile::remember().merge(ToolProfile::operate()))
    );
}

#[test]
fn pattern_matching_supports_wildcards() {
    assert!(tool_name_matches_pattern("hub_call", "hub_*"));
    assert!(!tool_name_matches_pattern("save_memory", "hub_*"));
}

#[test]
fn profile_filter_and_env_whitelist_form_intersection() {
    let filtered = filter_tool_defs(
        vec![
            test_tool("search_memory"),
            test_tool("save_memory"),
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
    assert_eq!(
        names,
        vec!["search_memory".to_string(), "recall_context".to_string()]
    );
}

#[test]
fn coordinate_surface_includes_memory_and_workflow_tools() {
    let filtered = filter_tool_defs(
        vec![
            test_tool("search_memory"),
            test_tool("save_memory"),
            test_tool("ingest_event"),
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
            "search_memory".to_string(),
            "save_memory".to_string(),
            "ingest_event".to_string(),
            "post_card".to_string()
        ]
    );
}

#[test]
fn explicit_remember_surface_excludes_admin_tools() {
    let filtered = filter_tool_defs(
        vec![
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
        vec![
            "search_memory".to_string(),
            "save_memory".to_string(),
            "ingest_event".to_string()
        ]
    );
}

#[test]
fn every_standard_and_delegate_allow_list_entry_exists_in_tool_router() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();
    for name in STANDARD_MINIMAL_TOOL_PATTERNS
        .iter()
        .chain(DELEGATE_MINIMAL_TOOL_PATTERNS.iter())
    {
        assert!(
            route_names.contains(*name),
            "minimal allow-list entry '{name}' is missing from the tool router"
        );
    }
}

#[test]
fn every_bundle_wildcard_matches_a_real_tool() {
    let route_names = native_route_names();
    let wildcard_patterns: BTreeSet<&str> = OBSERVE_TOOL_PATTERNS
        .iter()
        .chain(REMEMBER_TOOL_PATTERNS.iter())
        .chain(COORDINATE_TOOL_PATTERNS.iter())
        .chain(OPERATE_TOOL_PATTERNS.iter())
        .chain(STANDARD_MINIMAL_TOOL_PATTERNS.iter())
        .chain(DELEGATE_MINIMAL_TOOL_PATTERNS.iter())
        .copied()
        .filter(|pattern| pattern.contains('*'))
        .collect();

    for pattern in wildcard_patterns {
        assert!(
            route_names
                .iter()
                .any(|tool_name| tool_name_matches_pattern(tool_name, pattern)),
            "wildcard pattern '{pattern}' matches no routed tool"
        );
    }
}

#[test]
fn every_real_tool_is_either_bundled_or_explicitly_admin_only() {
    let route_names = native_route_names();
    let actual_admin_only: BTreeSet<String> = route_names
        .iter()
        .filter(|tool_name| bundle_count(tool_name) == 0)
        .cloned()
        .collect();
    let expected_admin_only: BTreeSet<String> = ADMIN_ONLY_NATIVE_ROUTE_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    assert_eq!(
            actual_admin_only, expected_admin_only,
            "new routed tools must either join a non-admin bundle or be explicitly classified as admin-only"
        );
}

#[test]
fn every_non_admin_write_tool_is_bundled_and_invalidates_cache() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();

    for tool_name in NON_ADMIN_WRITE_ROUTE_NAMES {
        assert!(
            route_names.contains(*tool_name),
            "expected non-admin write tool '{tool_name}' to exist in the router"
        );
        assert!(
            bundle_count(tool_name) > 0,
            "non-admin write tool '{tool_name}' must belong to a bundle"
        );
        assert!(
            crate::server_state::CACHE_INVALIDATING_TOOLS.contains(tool_name),
            "non-admin write tool '{tool_name}' must invalidate the read cache"
        );
    }

    assert!(!tool_matches_bundle("tachi_complete", ToolBundle::Observe));
    assert!(tool_matches_bundle("tachi_complete", ToolBundle::Remember));
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
            test_tool("tachi_task"),
            test_tool("tachi_arena"),
            test_tool("tachi_memory"),
            test_tool("tachi_briefing"),
            test_tool("tachi_save"),
            test_tool("tachi_web_search"),
            test_tool("tachi_wiki"),
            test_tool("tachi_skill"),
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
            test_tool("tachi_plan"),
            test_tool("recall_context"),
            test_tool("tachi_browse"),
            test_tool("hub_discover"),
            test_tool("run_skill"),
            test_tool("tachi_dispatch"),
            test_tool("tachi_complete"),
            test_tool("approve_merge"),
            test_tool("tachi_board"),
            test_tool("vault_unlock"),
            test_tool("vault_lock"),
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
            "tachi_task".to_string(),
            "tachi_arena".to_string(),
            "tachi_memory".to_string(),
            "tachi_briefing".to_string(),
            "tachi_save".to_string(),
            "tachi_web_search".to_string(),
            "tachi_wiki".to_string(),
            "tachi_skill".to_string(),
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
            test_tool("tachi_browse"),
            test_tool("tachi_unstick"),
            test_tool("tachi_complete"),
            test_tool("run_skill"),
            // Old tools that should be excluded from delegate
            test_tool("tachi_search"),
            test_tool("tachi_save"),
            // Should be excluded:
            test_tool("tachi_plan"),
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
            "tachi_browse".to_string(),
            "tachi_unstick".to_string(),
            "tachi_complete".to_string(),
            "run_skill".to_string(),
        ]
    );
}

#[test]
fn standard_and_delegate_labels() {
    assert_eq!(ToolProfile::standard().as_str(), "standard");
    assert_eq!(ToolProfile::delegate().as_str(), "delegate");
    // admin still wins over minimal flags if explicitly merged.
    assert_eq!(
        ToolProfile::standard().merge(ToolProfile::admin()).as_str(),
        "admin"
    );
    // standard wins over delegate if both set.
    assert_eq!(
        ToolProfile::delegate()
            .merge(ToolProfile::standard())
            .as_str(),
        "standard"
    );
}
