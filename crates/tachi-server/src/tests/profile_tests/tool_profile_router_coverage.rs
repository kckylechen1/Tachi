use std::collections::{BTreeMap, BTreeSet};
use tachi_hub::{
    tool_matches_bundle, tool_name_matches_pattern, ToolBundle, COORDINATE_TOOL_PATTERNS,
    DELEGATE_MINIMAL_TOOL_PATTERNS, OBSERVE_TOOL_PATTERNS, OPERATE_TOOL_PATTERNS,
    REMEMBER_TOOL_PATTERNS, STANDARD_MINIMAL_TOOL_PATTERNS,
};

fn ensure_test_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("TACHI_DISABLE_PATH_VALIDATION", "1");
    });
}

pub(super) fn native_route_definitions() -> Vec<rmcp::model::Tool> {
    ensure_test_env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("profiles test runtime");
    let _guard = runtime.enter();
    let db_path = crate::utils::test_fixture_path(format!(
        "profiles-metadata-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let server = crate::MemoryServer::new(db_path, None).expect("test memory server");
    server.tool_router.list_all()
}

fn native_route_names() -> Vec<String> {
    let mut names: Vec<String> = native_route_definitions()
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
    let db_path = crate::utils::test_fixture_path(format!(
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
    "tachi_audit_log",
    "tachi_init_project_db",
    // tachi_research is now observe-bundled (#963/#530), no longer admin-only.
    // #757 Cut3-S1: folded sandbox verb (its six forwarding aliases above are
    // also admin-only) — absent from every profile bundle.
    "tachi_sandbox",
    "tachi_task_brief",
    // #1426: the route/recall tuning facade is deliberately in no bundle —
    // `tool_visible` grants it to admin/full profiles only, by omission from
    // the standard, delegate, and bundle pattern arrays.
    "tachi_tune",
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
    // #1099: "handoff_check"/"handoff_leave" retired — the routes no longer
    // exist. "tachi_handoff" (below) survives as a non-admin write route.
    "ingest_event",
    "sync_memories",
    "tachi_complete",
    "tachi_domain_adapter",
    "tachi_handoff",
    "tachi_memory",
    "tachi_orchestrator",
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
    // #1690 C3 slice A: the "second model brain" recommend family and its
    // standalone backcompat skill routes are retired — `run_skill` was a pure
    // forwarder to `handle_run_skill` (canonical: tachi_skill(action='run'))
    // and `prepare_capability_bundle` forwarded to the surviving bundle
    // handler (canonical: tachi_skill(action='bundle')). The recommend_* tools
    // have no canonical replacement; their scoring core survives only behind
    // the bundle handler until slice C removes it.
    "run_skill",
    "prepare_capability_bundle",
    "recommend_capability",
    "recommend_skill",
    "recommend_toolchain",
    // #1690 C3: skill_evolve (LLM telemetry-driven skill versioning) is retired
    // end-to-end — no canonical replacement survives the contraction.
    "skill_evolve",
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
fn tachi_skill_facade_advertises_canonical_run_and_bundle_actions() {
    // #1690 C3 slice A re-anchor: this test used to pin that the retired
    // `run_skill` / `prepare_capability_bundle` backcompat routes stayed
    // registered and pointed at tachi_skill. Those routes are now RETIRED
    // (their "stays retired" fate is guarded by `retired_native_aliases_stay_retired`
    // via `RETIRED_NATIVE_ALIASES`); what survives is the canonical facade's
    // advertisement of the actions the deleted routes used to forward to.
    // #1690 C3 slice C: the surviving static action set is {discover, run} —
    // the description must advertise exactly those and teach no retired action.
    let descriptions = native_route_descriptions();

    let tachi_skill = descriptions
        .get("tachi_skill")
        .expect("canonical tachi_skill facade should stay registered");
    assert!(
        tachi_skill.contains("action='run'"),
        "tachi_skill description should advertise canonical run action: {tachi_skill}"
    );
    assert!(
        tachi_skill.contains("action='discover'"),
        "tachi_skill description should advertise canonical discover action: {tachi_skill}"
    );
    for retired in ["bundle", "loadout", "from_pattern"] {
        assert!(
            !tachi_skill.contains(&format!("action='{retired}'")),
            "tachi_skill description must not teach retired action '{retired}': {tachi_skill}"
        );
    }
}

/// #1098: every name in the single typed action-effect authority's cache
/// membership lists (`crate::server_state::CACHEABLE_TOOLS` /
/// `CACHE_INVALIDATING_TOOLS`, sourced from `crate::action_effect`) must be a
/// live registered route — a stale entry left behind by a retired tool would
/// otherwise sit in the list forever with no dynamic check to catch it.
#[test]
fn f1098_cache_policy_entries_are_live_registered_routes() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();
    for name in crate::server_state::CACHEABLE_TOOLS
        .iter()
        .chain(crate::server_state::CACHE_INVALIDATING_TOOLS.iter())
    {
        assert!(
            route_names.contains(*name),
            "#1098 cache-policy entry '{name}' is not a live registered MCP route"
        );
    }
}

enum LiveActionInventory {
    Absent,
    Unbounded,
    Enumerated(Vec<String>),
}

fn action_inventory_from_live_schema(tool: &rmcp::model::Tool) -> LiveActionInventory {
    let schema = serde_json::to_value(&tool.input_schema)
        .expect("live MCP tool input schema must serialize for effect coverage");
    let Some(action_schema) = schema.pointer("/properties/action") else {
        return LiveActionInventory::Absent;
    };
    let Some(actions) = action_schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
    else {
        return LiveActionInventory::Unbounded;
    };
    LiveActionInventory::Enumerated(
        actions
            .iter()
            .map(|action| {
                action
                    .as_str()
                    .expect("action enums must contain strings")
                    .to_string()
            })
            .collect(),
    )
}

/// #1098 / #1170 ratchet: enumerate the live registered router and compare each
/// advertised action enum to the independent typed effect map. An action newly
/// added to a schema must be added to that map explicitly; an invented action
/// must have no metadata and therefore fail closed at the production gate.
#[test]
fn f1098_live_action_inventory_has_explicit_effect_metadata() {
    let mut missing_effects = Vec::new();
    let mut implicit_unknowns = Vec::new();

    for tool in native_route_definitions() {
        let tool_name = tool.name.to_string();
        let actions = match action_inventory_from_live_schema(&tool) {
            LiveActionInventory::Absent => continue,
            LiveActionInventory::Unbounded => {
                assert!(
                    crate::action_effect::facade_action_effect(
                        &tool_name,
                        Some("__action_without_schema_inventory"),
                    )
                    .is_none(),
                    "{tool_name} has no action enum but grants per-action effect metadata"
                );
                continue;
            }
            LiveActionInventory::Enumerated(actions) => actions,
        };

        for action in &actions {
            let metadata = crate::action_effect::facade_action_effect(&tool_name, Some(action));
            if metadata.is_none() {
                missing_effects.push(format!("{tool_name}(action='{action}')"));
            }
        }

        if crate::action_effect::facade_action_effect(
            &tool_name,
            Some("__new_action_without_effect_authority"),
        )
        .is_some()
        {
            implicit_unknowns.push(tool_name);
        }
    }

    assert!(
        missing_effects.is_empty(),
        "live actions missing explicit typed effect mappings: {missing_effects:?}"
    );
    assert!(
        implicit_unknowns.is_empty(),
        "action schemas granting implicit metadata to new actions: {implicit_unknowns:?}"
    );
}

/// #1098 acceptance: "dynamically enumerate every ... direct tool route;
/// every routable operation has effect/replay metadata or fails closed." This
/// dynamically walks the real, live router and proves the replay authority runs
/// clean end to end for every tool name the server actually exposes today.
///
/// codex review (PR #1213, checkpoint 3): the pre-fix-round version of this
/// test discarded the boolean result, proving only "did not panic". It now
/// also asserts, against the LIVE router (not just the string constants a
/// unit test in `action_effect` checks in isolation), that every currently
/// registered cache-invalidating standalone route this fix round fixed
/// (`remember`/`extract_facts`/`ingest_event`) really does classify unsafe
/// end to end through `shared_defs::dlq_replay_is_explicitly_safe` — catching a
/// future regression where the route stays registered but drops out of
/// `STANDALONE_UNSAFE_ROUTES`.
#[test]
fn f1098_every_live_native_route_classifies_without_panicking() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();
    for name in &route_names {
        let _ = crate::shared_defs::dlq_replay_is_explicitly_safe(name, None);
    }

    for fixed_route in ["remember", "extract_facts", "ingest_event"] {
        assert!(
            route_names.contains(fixed_route),
            "'{fixed_route}' must still be a live registered route for the \
             #1213 fail-open fix to mean anything"
        );
        assert!(
            !crate::shared_defs::dlq_replay_is_explicitly_safe(fixed_route, None),
            "'{fixed_route}' must classify unsafe-to-replay through the live \
             router (PR #1213 checkpoint 4 fix)"
        );
    }
}
