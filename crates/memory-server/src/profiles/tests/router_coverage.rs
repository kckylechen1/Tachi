use super::*;

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
    "tachi_domain_adapter",
    "tachi_handoff",
    "tachi_memory",
    "tachi_orchestrator",
    "tachi_arena",
    "tachi_verify",
    "tachi_save",
    "tachi_wiki_write",
    "update_card",
];

use std::collections::BTreeSet;

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
