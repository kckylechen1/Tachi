use std::collections::{BTreeMap, BTreeSet};
use tachi_hub::{
    tool_matches_bundle, tool_name_matches_pattern, ToolBundle, COORDINATE_TOOL_PATTERNS,
    DELEGATE_MINIMAL_TOOL_PATTERNS, OBSERVE_TOOL_PATTERNS, OPERATE_TOOL_PATTERNS,
    REMEMBER_TOOL_PATTERNS, STANDARD_MINIMAL_TOOL_PATTERNS,
};

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

fn native_route_descriptions() -> BTreeMap<String, String> {
    ensure_test_env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("profiles test runtime");
    let _guard = runtime.enter();
    let db_path = std::env::temp_dir().join(format!(
        "profiles-description-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = crate::MemoryServer::new(db_path, None).expect("test memory server");
    server
        .tool_router
        .list_all()
        .into_iter()
        .map(|tool| {
            (
                tool.name.into_owned(),
                tool.description
                    .map(|description| description.into_owned())
                    .unwrap_or_default(),
            )
        })
        .collect()
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

const ADMIN_ONLY_NATIVE_ROUTE_NAMES: &[&str] = &[
    "chain_skills",
    "check_inbox",
    "distill_trajectory",
    "dlq_list",
    "dlq_retry",
    "get_memory",
    "hub_export_skills",
    "hub_feedback",
    "hub_get",
    "hub_quick_add",
    "hub_register",
    "hub_review",
    "hub_set_active_version",
    "hub_set_enabled",
    "hub_stats",
    "post_card",
    "remember",
    "sandbox_check",
    "sandbox_exec_audit",
    "sandbox_get_policy",
    "sandbox_list_policies",
    "sandbox_set_policy",
    "sandbox_set_rule",
    "save_memory",
    "search_memory",
    "skill_evolve",
    "tachi_audit_log",
    "tachi_init_project_db",
    // tachi_research is now observe-bundled (#963/#530), no longer admin-only.
    "tachi_task_brief",
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
    "update_card",
];

/// #757: graph/state primitives are no longer MCP-registered. #913 deleted
/// the in-crate handler/facade layer outright (zero remaining callers), so
/// these names must never resurface as routed tools, bundle members, or
/// cache-policy entries.
const INTERNALIZED_GRAPH_STATE_MCP_NAMES: &[&str] = &[
    "add_edge",
    "set_state",
    "get_state",
    "get_edges",
    "memory_graph",
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
    "project_agent_profile",
    "queue_agent_evolution",
    "review_agent_evolution_proposal",
    "sync_memories",
    "synthesize_agent_evolution",
    "tachi_complete",
    "tachi_domain_adapter",
    "tachi_handoff",
    "tachi_memory",
    "tachi_orchestrator",
    "tachi_arena",
    "tachi_verify",
    "tachi_save",
    "tachi_wiki_write",
];

const RETIRED_NATIVE_ALIASES: &[&str] = &[
    "cyberbrain_write",
    "cyberbrain_search",
    "section9_review",
    "section9_audit_log",
    "shell_set_policy",
    "shell_get_policy",
    "shell_list_policies",
    "shell_exec_audit",
    "tachi_plan",
    "tachi_progress_check",
    "wiki_browse",
];

// Batch C (#757) removed `tachi_board` and `tachi_dispatch` — their canonical
// replacements are tachi_task(action='board'/'dispatch'). Only `get_memory`
// remains a folded admin-only compat tool here.
const FOLDED_NATIVE_COMPAT_TOOLS: &[&str] = &["get_memory"];

const PROFILE_RETIRED_DIRECT_TOOLS: &[&str] = &[
    "check_inbox",
    "post_card",
    "remember",
    "save_memory",
    "search_memory",
    "tachi_task_brief",
    "update_card",
];

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
fn retired_native_aliases_stay_retired() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();
    let cacheable: BTreeSet<&str> = crate::server_state::CACHEABLE_TOOLS
        .iter()
        .copied()
        .collect();
    let invalidating: BTreeSet<&str> = crate::server_state::CACHE_INVALIDATING_TOOLS
        .iter()
        .copied()
        .collect();

    for alias in RETIRED_NATIVE_ALIASES {
        assert!(
            !route_names.contains(*alias),
            "retired native alias '{alias}' must not be reintroduced to the tool router"
        );
        assert!(
            bundle_count(alias) == 0,
            "retired native alias '{alias}' must not be included in tool profile bundles"
        );
        assert!(
            !cacheable.contains(alias) && !invalidating.contains(alias),
            "retired native alias '{alias}' must not remain in cache policy lists"
        );
    }
}

/// Discrimination (#757): graph/state primitives must not reappear on the MCP
/// router. The in-crate handler/facade layer was deleted in #913 (dead code,
/// zero callers); only the memcore store layer (with its own live callers —
/// auto_link, contradiction, etc.) remains.
#[test]
fn f757_graph_state_primitives_are_not_mcp_registered() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();
    let cacheable: BTreeSet<&str> = crate::server_state::CACHEABLE_TOOLS
        .iter()
        .copied()
        .collect();
    let invalidating: BTreeSet<&str> = crate::server_state::CACHE_INVALIDATING_TOOLS
        .iter()
        .copied()
        .collect();

    for name in INTERNALIZED_GRAPH_STATE_MCP_NAMES {
        assert!(
            !route_names.contains(*name),
            "internalized graph/state tool '{name}' must not be registered on the MCP router"
        );
        assert_eq!(
            bundle_count(name),
            0,
            "internalized graph/state tool '{name}' must not appear in profile bundles"
        );
        assert!(
            !cacheable.contains(name) && !invalidating.contains(name),
            "internalized graph/state tool '{name}' must not remain in cache policy lists"
        );
    }
}

#[test]
fn folded_native_compat_tools_stay_admin_only() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();

    for tool_name in FOLDED_NATIVE_COMPAT_TOOLS {
        assert!(
            route_names.contains(*tool_name),
            "folded compatibility tool '{tool_name}' should remain routable for admin/backcompat"
        );
        assert!(
            bundle_count(tool_name) == 0,
            "folded compatibility tool '{tool_name}' must not re-enter non-admin profile bundles"
        );
        assert!(
            !STANDARD_MINIMAL_TOOL_PATTERNS.contains(tool_name)
                && !DELEGATE_MINIMAL_TOOL_PATTERNS.contains(tool_name),
            "folded compatibility tool '{tool_name}' must not be exposed through minimal profiles"
        );
    }
}

#[test]
fn profile_retired_direct_tools_stay_admin_only() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();

    for tool_name in PROFILE_RETIRED_DIRECT_TOOLS {
        assert!(
            route_names.contains(*tool_name),
            "profile-retired direct tool '{tool_name}' should remain routable for admin/backcompat"
        );
        assert!(
            bundle_count(tool_name) == 0,
            "profile-retired direct tool '{tool_name}' must not re-enter non-admin profile bundles"
        );
        assert!(
            !STANDARD_MINIMAL_TOOL_PATTERNS.contains(tool_name)
                && !DELEGATE_MINIMAL_TOOL_PATTERNS.contains(tool_name),
            "profile-retired direct tool '{tool_name}' must not be exposed through minimal profiles"
        );
    }
}

#[test]
fn standalone_skill_entrypoints_stay_routable_but_point_to_tachi_skill() {
    let descriptions = native_route_descriptions();

    let tachi_skill = descriptions
        .get("tachi_skill")
        .expect("canonical tachi_skill facade should stay registered");
    assert!(
        tachi_skill.contains("action='run'"),
        "tachi_skill description should advertise canonical run action: {tachi_skill}"
    );
    assert!(
        tachi_skill.contains("action='bundle'"),
        "tachi_skill description should advertise canonical bundle action: {tachi_skill}"
    );

    let run_skill = descriptions
        .get("run_skill")
        .expect("run_skill compatibility route should stay registered");
    assert!(
        run_skill.contains("compatibility route"),
        "run_skill description should mark it as a compatibility route: {run_skill}"
    );
    assert!(
        run_skill.contains("tachi_skill(action='run')"),
        "run_skill description should name the canonical tachi_skill action: {run_skill}"
    );

    let prepare_bundle = descriptions
        .get("prepare_capability_bundle")
        .expect("prepare_capability_bundle compatibility route should stay registered");
    assert!(
        prepare_bundle.contains("compatibility route"),
        "prepare_capability_bundle description should mark it as a compatibility route: {prepare_bundle}"
    );
    assert!(
        prepare_bundle.contains("tachi_skill(action='bundle')"),
        "prepare_capability_bundle description should name the canonical tachi_skill action: {prepare_bundle}"
    );
}
