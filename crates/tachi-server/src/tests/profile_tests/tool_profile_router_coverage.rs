use std::collections::{BTreeMap, BTreeSet};
use tachi_hub::{DELEGATE_MINIMAL_TOOL_PATTERNS, STANDARD_MINIMAL_TOOL_PATTERNS};

pub(super) fn native_route_definitions() -> Vec<rmcp::model::Tool> {
    crate::MemoryServer::native_tool_router().list_all()
}

#[tokio::test]
async fn installed_native_router_matches_metadata_definitions() {
    let server = super::make_server();
    let mut installed = server.tool_router.list_all();
    let mut metadata = native_route_definitions();
    installed.sort_by(|left, right| left.name.cmp(&right.name));
    metadata.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(installed, metadata);
}

#[test]
fn native_schema_preparation_preserves_tools_without_object_project() {
    for input_schema in [
        serde_json::json!({}),
        serde_json::json!({"properties": null}),
        serde_json::json!({"properties": {}}),
        serde_json::json!({"properties": {"project": null}}),
        serde_json::json!({"properties": {"project": true}}),
        serde_json::json!({"properties": {"project": 0}}),
        serde_json::json!({"properties": {"project": "project"}}),
        serde_json::json!({"properties": {"project": []}}),
    ] {
        let tool: rmcp::model::Tool = serde_json::from_value(serde_json::json!({
            "name": "test_native",
            "description": "native definition",
            "inputSchema": input_schema,
        }))
        .expect("test tool");
        assert_eq!(
            crate::server_handler::prepare_native_tool_definitions(vec![tool.clone()]),
            vec![tool],
        );
    }
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
    native_route_definitions()
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

const WRITE_ROUTE_NAMES: &[&str] = &[
    "archive_memory",
    "capture_session",
    "compact_rollup",
    "compact_session_memory",
    "extract_facts",
    "ingest_event",
    "sync_memories",
    "tachi_domain_adapter",
    "tachi_memory",
    "tachi_verify",
    "tachi_wiki_write",
];

const RETIRED_NATIVE_ALIASES: &[&str] = &[
    // Hub upstream MCP proxy, virtual capabilities, and web search are retired.
    "vc_register",
    "vc_bind",
    "vc_list",
    "vc_resolve",
    "hub_disconnect",
    "tachi_web_search",
    "chain_skills",
    "tachi_unstick",
    "tachi_wiki_organize",
    // Owner CLI manages credentials; MCP exposes read-only connection metadata.
    "vault_init",
    "vault_unlock",
    "vault_lock",
    "vault_status",
    "vault_set",
    "vault_get",
    "vault_list",
    "vault_remove",
    "vault_set_api_key_pool",
    "vault_setup_rotation",
    "vault_lease_api_key",
    "vault_record_key_result",
    "check_inbox",
    "cyberbrain_write",
    "cyberbrain_search",
    "distill_trajectory",
    "find_similar_memory",
    "get_memory",
    "post_card",
    "prepare_capability_bundle",
    "recommend_capability",
    "recommend_skill",
    "recommend_toolchain",
    "remember",
    "run_skill",
    "sandbox_set_rule",
    "sandbox_check",
    "sandbox_set_policy",
    "sandbox_get_policy",
    "sandbox_list_policies",
    "sandbox_exec_audit",
    "save_memory",
    "search_memory",
    "section9_review",
    "section9_audit_log",
    "shell_set_policy",
    "shell_get_policy",
    "shell_list_policies",
    "shell_exec_audit",
    "skill_evolve",
    "tachi_briefing",
    "tachi_complete",
    // Removed 2026-10-22 together with `handoff_ops` (last action promote_issue).
    "tachi_handoff",
    // Removed 2026-10-22 together with `gh_ops`; GitHub is a read-only source.
    "tachi_gh",
    "tachi_orchestrator",
    // Removed together with the route/recall proposal handlers; labeled recall
    // replay survives only as the local `tachi eval recall` CLI.
    "tachi_tune",
    "tachi_plan",
    "tachi_progress_check",
    "tachi_research",
    "wiki_browse",
    "tachi_save",
    "tachi_task_brief",
    "update_card",
    "wiki_search",
    "tachi_dispatch",
    "tachi_board",
    "approve_merge",
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
fn write_routes_remain_registered_and_invalidate_cache() {
    let route_names: BTreeSet<String> = native_route_names().into_iter().collect();

    for tool_name in WRITE_ROUTE_NAMES {
        assert!(
            route_names.contains(*tool_name),
            "expected write tool '{tool_name}' to exist in the router"
        );
        assert!(
            crate::server_state::CACHE_INVALIDATING_TOOLS.contains(tool_name),
            "write tool '{tool_name}' must invalidate the read cache"
        );
    }
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
            !STANDARD_MINIMAL_TOOL_PATTERNS.contains(alias)
                && !DELEGATE_MINIMAL_TOOL_PATTERNS.contains(alias),
            "retired native alias '{alias}' must not be included in Agent or Worker discovery"
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
        assert!(
            !STANDARD_MINIMAL_TOOL_PATTERNS.contains(name)
                && !DELEGATE_MINIMAL_TOOL_PATTERNS.contains(name),
            "internalized graph/state tool '{name}' must not appear in Agent or Worker discovery"
        );
        assert!(
            !cacheable.contains(name) && !invalidating.contains(name),
            "internalized graph/state tool '{name}' must not remain in cache policy lists"
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
        tachi_skill.contains("action='discover'"),
        "tachi_skill description should advertise canonical discover action: {tachi_skill}"
    );
    assert!(
        tachi_skill.contains("action='run'"),
        "tachi_skill description should advertise canonical run action: {tachi_skill}"
    );
    for retired in ["bundle", "loadout", "from_pattern"] {
        assert!(
            !tachi_skill.contains(&format!("action='{retired}'")),
            "tachi_skill description must not teach retired action '{retired}': {tachi_skill}"
        );
    }
}

#[test]
fn memory_facade_runtime_and_schema_do_not_teach_retired_save_alias() {
    let tool = native_route_definitions()
        .into_iter()
        .find(|tool| tool.name == "tachi_memory")
        .expect("canonical tachi_memory facade should stay registered");
    let description = tool.description.as_deref().unwrap_or_default();
    assert!(
        !description.contains("tachi_save"),
        "live tachi_memory tool description must not recommend retired tachi_save: {description}"
    );

    let schema = serde_json::to_value(&tool.input_schema)
        .expect("live tachi_memory input schema must serialize");
    let schema_text = serde_json::to_string(&schema)
        .expect("live tachi_memory input schema must render for contract inspection");
    assert!(
        !schema_text.contains("tachi_save"),
        "live tachi_memory input schema must not teach retired tachi_save: {schema_text}"
    );
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

    for fixed_route in ["extract_facts", "ingest_event"] {
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

const FACADE_RETIRED_ACTIONS: &[&str] = &[
    "dispatch",
    "wait",
    "cancel",
    "plan",
    "cycle_plan",
    "recommend",
    "refine_issues",
    "merge",
    "ux_matrix",
    "briefing",
    "doc_index",
    "cycle_status",
    "profiles",
    "profile",
    "card",
    "build_references",
    "close_loop",
    "route_simulate",
    "proposals",
    "review_proposal",
    "apply_proposals",
    "link_pr",
    "pr_status",
    "pr_handoff",
    "release_note",
];

#[test]
fn retired_task_actions_stay_rejected() {
    for action in FACADE_RETIRED_ACTIONS {
        assert!(
            serde_json::from_value::<tachi_params::TachiTaskAction>(serde_json::json!(action))
                .is_err(),
            "retired task action '{action}' must stay rejected by the typed wire parser"
        );
    }
}
