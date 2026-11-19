use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tachi_hub::{DELEGATE_MINIMAL_TOOL_PATTERNS, STANDARD_MINIMAL_TOOL_PATTERNS};
use tachi_params::{
    TACHI_MEMORY_ACTIONS, TACHI_MEMORY_RETIRED_C2B_ACTIONS, TACHI_SKILL_ACTIONS,
    TACHI_TASK_RETIRED_ACTIONS,
};

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
    // #1690 C3: standalone skill aliases and recommendation routes are retired
    // end-to-end. Their duplicate entries preserve the branch census while the
    // main-side inventory above remains authoritative.
    "run_skill",
    "prepare_capability_bundle",
    "recommend_capability",
    "recommend_skill",
    "recommend_toolchain",
    // #1690 C3: skill_evolve (LLM telemetry-driven skill versioning) and
    // distill_trajectory (trajectory → skill snapshot/registration) are retired
    // end-to-end — no canonical replacement survives the contraction.
    "skill_evolve",
    "distill_trajectory",
    "tachi_save",
    "tachi_task_brief",
    "update_card",
    "wiki_browse",
    "wiki_search",
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

/// Retired-surface documentation lint (issue #1830, hardened by #1454 F3).
/// Derives retired tokens from RETIRED_NATIVE_ALIASES (this file) +
/// hand-pinned HISTORICAL_SKILL_RETIRED (the retired set from census history) +
/// DELETED_NATIVE_ROUTES + INTERNALIZED_GRAPH_STATE_MCP_NAMES + retired facade
/// actions.
/// HISTORICAL_SKILL_SUPERSET is independent hand-maintained grows-only list of
/// all skill actions ever (live + retired); see B4 repair.
/// Scans the ACTIVE documentation surface line-by-line: `docs/**` (EXCLUDING
/// `docs/**/archive/**` historical directories), the three root READMEs,
/// and `prompts/**`. Markdown/text plus tracked YAML, JSON, and config-shaped
/// files are included; unreadable/non-UTF-8 files and symlinks fail loudly.
/// Explicitly classified `*.fixture.json` files and JSON under
/// `docs/engineering/receipts/**` or `docs/engineering/artifacts/**` are machine
/// data rather than teaching surfaces, but they are still enumerated and must be
/// readable; arbitrary JSON, Markdown/prose, and all YAML/config remain governed.
/// Exemptions are strictly structural (archive path exclusion and MachineData
/// classification): authorable in-file headings, prose markers, or retirement
/// records never exempt retired-surface mentions in active governed documents.
/// Match EXACT identifier tokens (bounded non-alnum, e.g. `action="loadout"`,
/// `from_pattern`, `recommend_capability`, `run_skill`) to avoid false hits on
/// unrelated prose.
/// Teaching shapes implemented (B1): (i) inline-code/backtick `tok` presence;
/// (ii) code-fence CONTENT presence;
/// (iii) table row with exact tok in FIRST cell;
/// (iv) verb adjacency (EN: use/call/invoke/run/via + CN: 使用/调用/通过/采用/用 ) within same line;
/// (v) `action = "x"` / `actions: x` / JSON equivalents with flexible spacing,
/// and raw `tachi_task dispatch` references.
/// For tachi_skill retired: hit on tachi_skill ctx, action=... (flex), or fence content.
/// Native retired: unambiguous identifiers (with `_` or `tachi` prefix) fail on
/// any exact hit; ambiguous English-word aliases (e.g. `remember`) require
/// teaching context.
/// Conservative: do not weaken positive detection. Zero-FP required on clean tree.
const HISTORICAL_SKILL_SUPERSET: &[&str] =
    &["discover", "run", "bundle", "loadout", "from_pattern"];
const HISTORICAL_SKILL_RETIRED: &[&str] = &["bundle", "loadout", "from_pattern"];
// W2: retired facade actions (Task/Memory) are machine-known in
// tachi-params' authoritative inventories. Pinned legacy-rejection set
// {dispatch, wait, cancel} comes from the enum's typed rejection path
// (crates/tachi-params/src/facade/action_enums.rs:168,239 +
// facade/task.rs:443,457,601) — if those arms change, update this pin.
// Full typed-rejection set mirrored from TachiTaskAction::is_explicit_retired_wire
// (action_enums.rs:164-192). Self-validating in the lint test below: every name
// is asserted rejected by the live wire parser, so this list can never drift stale.
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
// Fully deleted native routes (absent from router AND from RETIRED_NATIVE_ALIASES):
// verified absent from the live router in the lint test below, so a reintroduction
// fails loudly.
const DELETED_NATIVE_ROUTES: &[&str] = &["tachi_dispatch", "tachi_board", "approve_merge"];

#[test]
fn retired_surface_documentation_has_markers() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).join("../..");

    let mut files: Vec<std::path::PathBuf> = vec![];
    // prompts/** (recursive per B2; no archive exclusion was present here).
    let prompts_dir = repo_root.join("prompts");
    collect_governed_files(&prompts_dir, false, &mut files)
        .unwrap_or_else(|error| panic!("{error}"));
    // docs/** (preserve the existing archive exclusion).
    let docs_dir = repo_root.join("docs");
    collect_governed_files(&docs_dir, true, &mut files).unwrap_or_else(|error| panic!("{error}"));
    // The root README set is part of the existing governed surface and is
    // required, so a missing/unreadable path cannot silently shrink coverage.
    for name in ["README.md", "README.zh-CN.md", "README.classical.md"] {
        let path = repo_root.join(name);
        register_governed_file(&path, &mut files).unwrap_or_else(|error| panic!("{error}"));
    }
    files.sort_unstable();
    files.dedup();

    // Self-validation: every deleted route is absent from the live router, and every
    // pinned facade action is rejected by the live typed wire parser.
    {
        let route_names: std::collections::BTreeSet<String> =
            native_route_names().into_iter().collect();
        for r in DELETED_NATIVE_ROUTES {
            assert!(
                !route_names.contains(*r),
                "deleted native route '{r}' must stay absent from the router (else move it off the lint list)"
            );
        }
        for a in FACADE_RETIRED_ACTIONS {
            assert!(
                serde_json::from_value::<tachi_params::TachiTaskAction>(serde_json::json!(a))
                    .is_err(),
                "retired task action '{a}' must stay rejected by the typed wire parser"
            );
        }
    }
    // reuse the router retired check rather than duplicate (B4)
    retired_native_aliases_stay_retired();
    // B4 repair (accepted): HISTORICAL_SKILL_SUPERSET is independent hand-maintained
    // grows-only list (append on new live skill; never delete).
    // retired set is hand-pinned HISTORICAL_SKILL_RETIRED (computed from census history,
    // NOT derived as superset-minus-live to avoid tautology).
    // Assertions:
    // (i) superset == live ∪ retired exactly
    // (ii) retired ∩ live = ∅
    // (iii) known {bundle, loadout, from_pattern} ⊆ retired
    let live_skill: Vec<&str> = TACHI_SKILL_ACTIONS.to_vec();
    for &name in &live_skill {
        assert!(
            HISTORICAL_SKILL_SUPERSET.contains(&name),
            "live skill action '{}' must be listed in HISTORICAL_SKILL_SUPERSET so future retirement is visible to lint",
            name
        );
    }
    // (i) superset == live ∪ retired exactly (retired from census source)
    let mut union: Vec<&str> = live_skill.clone();
    union.extend(HISTORICAL_SKILL_RETIRED.iter().copied());
    union.sort_unstable();
    union.dedup();
    let mut sup: Vec<&str> = HISTORICAL_SKILL_SUPERSET.to_vec();
    sup.sort_unstable();
    sup.dedup();
    assert_eq!(
        sup, union,
        "HISTORICAL_SKILL_SUPERSET must == live ∪ retired exactly (no tautology; retired is hand list from census history, not superset-minus-live)"
    );
    // (ii) retired ∩ live = ∅
    for &r in HISTORICAL_SKILL_RETIRED {
        assert!(
            !live_skill.contains(&r),
            "retired skill '{}' must not be live per census",
            r
        );
    }
    // (iii) known retired set ⊆ retired
    for &k in &["bundle", "loadout", "from_pattern"] {
        assert!(
            HISTORICAL_SKILL_RETIRED.contains(&k),
            "known retired set item '{}' must be in HISTORICAL_SKILL_RETIRED",
            k
        );
    }

    let mut bad_hits: Vec<String> = vec![];
    for file in &files {
        let content = read_governed_file(file).unwrap_or_else(|error| panic!("{error}"));
        // Machine-data exemption is intentionally path/shape based and narrow:
        // readability and path safety are still enforced above, while frozen
        // fixture/receipt/artifact JSON bytes are not interpreted as model-facing
        // instructions. Ordinary JSON, Markdown/prose, and every YAML/config file
        // remain in the active scan below.
        if classify_governed_file(&repo_root, file) == GovernedFileKind::MachineData {
            continue;
        }
        let rel = file
            .strip_prefix(&repo_root)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();
        bad_hits.extend(scan_active_document_content(&rel, &content));
    }

    assert!(
        bad_hits.is_empty(),
        "retired tokens found in active documentation surface (exemptions are restricted to archive/** paths and MachineData JSON files); fix docs or archive historical records:\n{}",
        bad_hits.join("\n")
    );
}

/// Scans an active governed document's contents for retired native aliases,
/// deleted routes, internalized graph/state names, retired skill actions, and
/// retired facade actions. No in-file heading or prose marker can exempt hits.
fn scan_active_document_content(rel: &str, content: &str) -> Vec<String> {
    let native_tokens: Vec<&str> = RETIRED_NATIVE_ALIASES
        .iter()
        .chain(DELETED_NATIVE_ROUTES)
        .chain(INTERNALIZED_GRAPH_STATE_MCP_NAMES)
        .copied()
        .collect();
    let facade_retired_tokens: BTreeSet<&str> = TACHI_TASK_RETIRED_ACTIONS
        .iter()
        .chain(TACHI_MEMORY_RETIRED_C2B_ACTIONS.iter())
        .chain(FACADE_RETIRED_ACTIONS.iter())
        .copied()
        .collect();
    let skill_tokens = HISTORICAL_SKILL_RETIRED;
    let live_memory = TACHI_MEMORY_ACTIONS;
    let live_task: Vec<&str> = tachi_params::TachiTaskAction::primary_wire_strings();

    let mut bad_hits = Vec::new();
    let mut fence_opener: Option<(char, usize)> = None;
    let mut fence_current_tool: Option<String> = None;

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        // B3 repair: track opener char + length; only same-char fence of len >= opener closes.
        if let Some((op_ch, op_len)) = fence_opener {
            if let Some((ch, len)) = parse_fence_opener(trimmed) {
                if ch == op_ch && len >= op_len {
                    fence_opener = None;
                }
            }
        } else if let Some((ch, len)) = parse_fence_opener(trimmed) {
            if len >= 3 {
                fence_opener = Some((ch, len));
            }
        }
        let in_code_fence = fence_opener.is_some();

        // Multi-line JSON envelopes: inside a fence, track the current
        // `"tool": "<name>"` so a later `"action": "<tok>"` line attributes
        // to that facade (closes the multiline-envelope evasion).
        if in_code_fence {
            let boundary = line.trim();
            if boundary == "}" || boundary == "{" || boundary == "}," {
                fence_current_tool = None;
            }
            if let Some(name) = keyed_identifier_value(line, "tool") {
                fence_current_tool = Some(name.to_string());
            }
        } else {
            fence_current_tool = None;
        }

        // native retired: unambiguous identifiers fail on any exact hit;
        // English-word aliases (e.g. "remember") keep the teaching-context gate.
        for &tok in &native_tokens {
            let unambiguous = tok.contains('_') || tok.starts_with("tachi");
            let is_hit = is_exact_identifier_hit(line, tok)
                && (unambiguous || is_teaching_context(line, tok));
            if is_hit {
                bad_hits.push(format!(
                    "{}:{}: retired token '{}' in active documentation",
                    rel,
                    i + 1,
                    tok
                ));
            }
        }

        // tachi_skill retired tokens: tachi_skill ctx, flex action=, or fence content in
        // code-shaped context (same prose-in-fence guard as native).
        for &tok in skill_tokens {
            let has_tachi_skill_ctx = contains_exact_identifier(line, "tachi_skill");
            let has_action_form = contains_action_assignment(line, tok);
            let is_teaching =
                has_tachi_skill_ctx || has_action_form || (in_code_fence && code_shaped(line, tok));
            if is_teaching && is_exact_identifier_hit(line, tok) {
                bad_hits.push(format!(
                    "{}:{}: retired tachi_skill token '{}' in active documentation",
                    rel,
                    i + 1,
                    tok
                ));
            }
        }

        // W2: retired facade actions (tachi_task/tachi_memory action names like
        // "cancel"/"merge"/"card") — these are common English words, so they ONLY
        // count when pinned to a facade context on the same line where the token is
        // NOT live on any facade named on that line.
        for tok in facade_retired_tokens.iter().copied() {
            let owner_facade = if TACHI_MEMORY_RETIRED_C2B_ACTIONS.contains(&tok) {
                "tachi_memory"
            } else {
                "tachi_task"
            };
            let live_on_named_facade = (contains_exact_identifier(line, "tachi_memory")
                && live_memory.contains(&tok))
                || (contains_exact_identifier(line, "tachi_task") && live_task.contains(&tok))
                || (contains_exact_identifier(line, "tachi_skill")
                    && TACHI_SKILL_ACTIONS.contains(&tok));
            let live_anywhere = live_memory.contains(&tok)
                || live_task.contains(&tok)
                || TACHI_SKILL_ACTIONS.contains(&tok);
            let facade_named = contains_exact_identifier(line, owner_facade)
                || fence_current_tool.as_deref() == Some(owner_facade);
            let quoted = contains_quoted_identifier(line, tok, '"')
                || contains_quoted_identifier(line, tok, '\'')
                || contains_quoted_identifier(line, tok, '`');
            let has_action_form = contains_action_assignment(line, tok);
            let has_facade_pair = contains_identifier_pair(line, owner_facade, tok);
            let is_teaching = !live_on_named_facade
                && ((facade_named && (has_action_form || quoted || has_facade_pair))
                    || (has_action_form && !live_anywhere)
                    || (in_code_fence && has_action_form && !live_anywhere));
            if is_teaching && is_exact_identifier_hit(line, tok) {
                bad_hits.push(format!(
                    "{}:{}: retired facade action '{}' in active documentation",
                    rel,
                    i + 1,
                    tok
                ));
            }
        }
    }

    bad_hits
}

fn is_exact_identifier_hit(line: &str, tok: &str) -> bool {
    exact_identifier_starts(line, tok).next().is_some()
}

fn exact_identifier_starts<'a>(line: &'a str, tok: &'a str) -> impl Iterator<Item = usize> + 'a {
    line.match_indices(tok).filter_map(move |(start, _)| {
        let before = line[..start].chars().next_back();
        let end = start + tok.len();
        let after = line[end..].chars().next();
        if before.is_none_or(|character| !is_ascii_identifier_char(character))
            && after.is_none_or(|character| !is_ascii_identifier_char(character))
        {
            Some(start)
        } else {
            None
        }
    })
}

fn contains_exact_identifier(line: &str, tok: &str) -> bool {
    is_exact_identifier_hit(line, tok)
}

fn is_ascii_identifier_char(character: char) -> bool {
    character == '_' || character.is_ascii_alphanumeric()
}

fn exact_identifier_at_start(value: &str, tok: &str) -> bool {
    let Some(rest) = value.strip_prefix(tok) else {
        return false;
    };
    rest.chars()
        .next()
        .is_none_or(|character| !is_ascii_identifier_char(character))
}

fn contains_quoted_identifier(line: &str, tok: &str, delimiter: char) -> bool {
    exact_identifier_starts(line, tok).any(|start| {
        line[..start].ends_with(delimiter) && line[start + tok.len()..].starts_with(delimiter)
    })
}

fn contains_call_syntax(line: &str, tok: &str) -> bool {
    exact_identifier_starts(line, tok)
        .any(|start| line[start + tok.len()..].trim_start().starts_with('('))
}

fn keyed_value_tail<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    for start in exact_identifier_starts(line, key) {
        let mut rest = &line[start + key.len()..];
        rest = rest.trim_start();
        if let Some(delimiter) = rest.chars().next() {
            if delimiter == '"' || delimiter == '\'' {
                rest = &rest[delimiter.len_utf8()..];
                rest = rest.trim_start();
            }
        }
        let separator = rest.chars().next()?;
        if separator != '=' && separator != ':' {
            continue;
        }
        return Some(rest[separator.len_utf8()..].trim_start());
    }
    None
}

fn contains_keyed_identifier(line: &str, key: &str, tok: &str) -> bool {
    let Some(mut value) = keyed_value_tail(line, key) else {
        return false;
    };
    let is_collection = value
        .chars()
        .next()
        .is_some_and(|character| matches!(character, '[' | '(' | '{'));
    if is_collection {
        return is_exact_identifier_hit(value, tok);
    }
    for delimiter in ['"', '\'', '`'] {
        if value.starts_with(delimiter) {
            value = &value[delimiter.len_utf8()..];
            break;
        }
    }
    exact_identifier_at_start(value, tok)
}

fn contains_action_assignment(line: &str, tok: &str) -> bool {
    contains_keyed_identifier(line, "action", tok)
        || contains_keyed_identifier(line, "actions", tok)
}

fn keyed_identifier_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut value = keyed_value_tail(line, key)?;
    for delimiter in ['"', '\'', '`'] {
        if value.starts_with(delimiter) {
            value = &value[delimiter.len_utf8()..];
            break;
        }
    }
    let end = value
        .char_indices()
        .find(|(_, character)| !is_ascii_identifier_char(*character))
        .map_or(value.len(), |(index, _)| index);
    (!value[..end].is_empty()).then_some(&value[..end])
}

fn contains_identifier_pair(line: &str, first: &str, second: &str) -> bool {
    let first_starts: Vec<usize> = exact_identifier_starts(line, first).collect();
    let second_starts: Vec<usize> = exact_identifier_starts(line, second).collect();
    first_starts.iter().any(|first_start| {
        second_starts.iter().any(|second_start| {
            if first_start == second_start {
                return false;
            }
            let (start, end) = if first_start < second_start {
                (first_start + first.len(), *second_start)
            } else {
                (second_start + second.len(), *first_start)
            };
            let between = &line[start..end];
            between.chars().count() <= 8
                && between
                    .chars()
                    .all(|character| !is_ascii_identifier_char(character))
        })
    })
}

/// Parse fence opener from trimmed line: returns (char, len) if starts with 3+ ``` or ~~~ .
fn parse_fence_opener(trimmed: &str) -> Option<(char, usize)> {
    if trimmed.is_empty() {
        return None;
    }
    let first = trimmed.chars().next().unwrap();
    if first != '`' && first != '~' {
        return None;
    }
    let mut count = 0usize;
    for c in trimmed.chars() {
        if c == first {
            count += 1;
        } else {
            break;
        }
    }
    if count >= 3 {
        Some((first, count))
    } else {
        None
    }
}

/// Inside a code fence, a token counts as teaching only in a code-shaped
/// context: `action="tok"` (any quote/spacing), `tok(`, backticked `tok`, or a
/// JSON-ish `"tool": "tok"` / `"action": "tok"` pair. Bare prose quoted inside
/// an example fence (e.g. a "remember more" sentence) is not teaching.
fn code_shaped(line: &str, tok: &str) -> bool {
    contains_action_assignment(line, tok)
        || contains_call_syntax(line, tok)
        || contains_quoted_identifier(line, tok, '`')
        || contains_quoted_identifier(line, tok, '"')
}

fn is_table_first_cell_hit(line: &str, tok: &str) -> bool {
    let t = line.trim();
    if !t.starts_with('|') {
        return false;
    }
    // first cell is the first non-empty after leading |
    let cells: Vec<&str> = t
        .split('|')
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();
    if cells.is_empty() {
        return false;
    }
    let first_cell = cells[0];
    is_exact_identifier_hit(first_cell, tok)
}

fn is_teaching_context(line: &str, tok: &str) -> bool {
    // B1 (iv) verb adjacency: EN {use,call,invoke,run,via} + CN {使用,调用,通过,采用,用} within same line
    // (attached or spaced; ` after verb for inline). The helpers use exact
    // ASCII word boundaries and Unicode-safe slices, so `misuse run_skill` and
    // `请调用tachi_complete完成任务` are handled distinctly.
    let en_verbs = ["use", "call", "invoke", "run", "via"];
    let cn_verbs = ["使用", "调用", "通过", "采用", "用"];
    for v in en_verbs {
        if contains_word_then_identifier(line, v, tok) {
            return true;
        }
    }
    for v in cn_verbs {
        if contains_prefix_then_identifier(line, v, tok) {
            return true;
        }
    }
    // (i) inline-code/backtick presence of exact retired token = teaching
    if contains_quoted_identifier(line, tok, '`') {
        return true;
    }
    // tok( form
    if contains_call_syntax(line, tok) {
        return true;
    }
    // (v) flex action= (also covers native)
    if contains_action_assignment(line, tok) {
        return true;
    }
    // (iii) table row containing exact retired token in the FIRST cell = teaching
    if is_table_first_cell_hit(line, tok) {
        return true;
    }
    false
}

fn contains_word_then_identifier(line: &str, word: &str, tok: &str) -> bool {
    let lowered = line.to_ascii_lowercase();
    let word = word.to_ascii_lowercase();
    let tok = tok.to_ascii_lowercase();
    let found = exact_identifier_starts(&lowered, &word).any(|start| {
        let mut rest = &lowered[start + word.len()..];
        rest = rest.trim_start();
        if rest.starts_with('`') {
            rest = &rest['`'.len_utf8()..];
        }
        exact_identifier_at_start(rest, &tok)
    });
    found
}

fn contains_prefix_then_identifier(line: &str, prefix: &str, tok: &str) -> bool {
    line.match_indices(prefix).any(|(start, _)| {
        let before = line[..start].chars().next_back();
        if before.is_some_and(is_ascii_identifier_char) {
            return false;
        }
        let mut rest = &line[start + prefix.len()..];
        rest = rest.trim_start();
        if rest.starts_with('`') {
            rest = &rest['`'.len_utf8()..];
        }
        exact_identifier_at_start(rest, tok)
    })
}

const GOVERNED_TEXT_EXTENSIONS: &[&str] = &[
    "md",
    "mdx",
    "txt",
    "yaml",
    "yml",
    "json",
    "jsonc",
    "toml",
    "ini",
    "cfg",
    "conf",
    "config",
    "properties",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GovernedFileKind {
    Prose,
    ActiveStructured,
    MachineData,
}

fn classify_governed_file(repo_root: &Path, path: &Path) -> GovernedFileKind {
    let relative = path.strip_prefix(repo_root).unwrap_or(path);
    let filename = relative.file_name().and_then(|name| name.to_str());
    let machine_data_root = relative.starts_with(Path::new("docs/engineering/receipts"))
        || relative.starts_with(Path::new("docs/engineering/artifacts"));
    let is_json = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
    if filename.is_some_and(|name| name.ends_with(".fixture.json"))
        || (machine_data_root && is_json)
    {
        GovernedFileKind::MachineData
    } else if is_governed_text_path(path)
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                !matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "md" | "mdx" | "txt"
                )
            })
    {
        GovernedFileKind::ActiveStructured
    } else {
        GovernedFileKind::Prose
    }
}

fn is_governed_text_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            GOVERNED_TEXT_EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn register_governed_file(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "governed active file '{}' cannot be inspected: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "governed active file '{}' is a symlink; symlinks are not followed",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "governed active path '{}' is not a regular file",
            path.display()
        ));
    }
    if !is_governed_text_path(path) {
        return Err(format!(
            "governed active file '{}' has an unsupported text extension",
            path.display()
        ));
    }
    out.push(path.to_path_buf());
    Ok(())
}

fn collect_governed_files(
    dir: &Path,
    skip_archive: bool,
    out: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(dir).map_err(|error| {
        format!(
            "governed active directory '{}' cannot be inspected: {error}",
            dir.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "governed active directory '{}' is a symlink; symlinks are not followed",
            dir.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "governed active path '{}' is not a directory",
            dir.display()
        ));
    }

    let entries = std::fs::read_dir(dir).map_err(|error| {
        format!(
            "governed active directory '{}' cannot be read: {error}",
            dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "governed active directory '{}' has an unreadable entry: {error}",
                dir.display()
            )
        })?;
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        // This preserves the pre-existing docs/archive historical exclusion;
        // prompts had no such exclusion, so callers pass false there.
        if skip_archive && name == "archive" {
            continue;
        }

        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "governed active path '{}' cannot be inspected: {error}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "governed active path '{}' is a symlink; symlinks are not followed",
                path.display()
            ));
        }
        if metadata.is_dir() {
            collect_governed_files(&path, skip_archive, out)?;
        } else if metadata.is_file() && is_governed_text_path(&path) {
            out.push(path);
        } else if !metadata.is_file() {
            return Err(format!(
                "governed active path '{}' is not a regular file or directory",
                path.display()
            ));
        }
    }
    Ok(())
}

fn read_governed_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| {
        format!(
            "governed active file '{}' is unreadable or not UTF-8: {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod matcher_unit_tests {
    use super::*;

    #[test]
    fn fence_parser_b3() {
        // B3: simple fences, nested valid CommonMark, mixed delimiters, unterminated.
        // (full state machine exercised by RED scratch + clean docs lint)
        assert_eq!(parse_fence_opener("```"), Some(('`', 3)));
        assert_eq!(parse_fence_opener("````"), Some(('`', 4)));
        assert_eq!(parse_fence_opener("~~~"), Some(('~', 3)));
        assert_eq!(parse_fence_opener("```rust"), Some(('`', 3)));
        assert_eq!(parse_fence_opener("``"), None);
        assert_eq!(parse_fence_opener("` ``"), None);
        // unterminated opener still parses
        assert_eq!(parse_fence_opener("```"), Some(('`', 3)));
        // mixed 4 vs 3: parse detects each independently (state prevents false close)
        assert_eq!(parse_fence_opener("````"), Some(('`', 4)));
        assert_eq!(parse_fence_opener("```"), Some(('`', 3)));
    }

    #[test]
    fn matcher_adversarial_table_round1_round2() {
        // oracle checkpoint 6: table of adversarial inputs (round-1 + round-2 evasion shapes) -> expected verdict
        // each must produce correct teaching/exact for B1 shapes; zero FP on non-teaching
        let cases: &[(&str, &str, bool, &str)] = &[
            // B1 shapes that must fire (RED will exercise in scratch)
            (
                r#"Call run_skill"#,
                "run_skill",
                true,
                "verb EN Call (round1)",
            ),
            (
                r#"Invoke recommend_capability"#,
                "recommend_capability",
                true,
                "verb EN Invoke (round1)",
            ),
            (
                r#"action = "loadout""#,
                "loadout",
                true,
                "spaced action= B1(v) (round2)",
            ),
            (
                r#"请采用 tachi_complete 完成任务"#,
                "tachi_complete",
                true,
                "CN 采用 verb B1(iv) (round2)",
            ),
            (
                r#"请用 tachi_complete 完成任务"#,
                "tachi_complete",
                true,
                "CN 用 verb B1(iv) (round2)",
            ),
            (
                r#"请调用tachi_complete完成任务"#,
                "tachi_complete",
                true,
                "Unicode-adjacent CN verb must remain a teaching hit",
            ),
            (
                r#"| tachi_complete | completion tool |"#,
                "tachi_complete",
                true,
                "table first cell B1(iii) (round2)",
            ),
            (
                r#"see the `run_skill` API"#,
                "run_skill",
                true,
                "inline-code/backtick B1(i)",
            ),
            (
                r#"`recommend_capability` (retired)"#,
                "recommend_capability",
                true,
                "inline code presence",
            ),
            (
                r#"tachi_skill(action = 'bundle')"#,
                "bundle",
                true,
                "flex action for skill retired",
            ),
            // code fence content: presence inside fence counts (tested via || in_code_fence in main loop)
            // negative to keep zero-FP
            (
                r#"the loadout of skills"#,
                "loadout",
                false,
                "prose 'loadout' without verb/action/table/fence/inline",
            ),
            (r#"bundle of facts"#, "bundle", false, "prose bundle"),
            (
                r#"| bar | tachi_complete |"#,
                "tachi_complete",
                false,
                "token not in FIRST cell",
            ),
            (
                r#"use the discover action"#,
                "discover",
                false,
                "live skill not retired",
            ),
            (
                r#"action = "from_pattern" in old code"#,
                "from_pattern",
                true,
                "flex action",
            ),
            // W3: "legacy" sentence must detect retired token
            (
                r#"Legacy clients should call run_skill for every workflow."#,
                "run_skill",
                true,
                "W3: legacy-clients sentence must still detect",
            ),
            // W2: retired facade action in action= form
            (
                r#"tachi_task(action="cancel", dispatch_id=...)"#,
                "cancel",
                true,
                "W2: retired facade action in action= form",
            ),
            (
                r#"actions: dispatch / status"#,
                "dispatch",
                true,
                "W2: YAML-style plural action key",
            ),
            (
                r#"interaction="dispatch""#,
                "dispatch",
                false,
                "boundary: interaction= is not action=",
            ),
            // W2: bare English word without action/facade context must NOT fire
            (
                r#"remember to merge the PR"#,
                "merge",
                false,
                "W2: bare English 'merge' is not a retired-action hit",
            ),
        ];
        for (line, tok, expect, note) in cases {
            let exact = is_exact_identifier_hit(line, tok);
            let teach = is_teaching_context(line, tok);
            let detected = exact && teach;
            assert_eq!(
                detected, *expect,
                "matcher unit {}: tok={} line={}",
                note, tok, line
            );
        }
    }

    #[test]
    fn matcher_boundaries_cover_raw_facade_and_unicode_forms() {
        assert!(contains_action_assignment("actions: dispatch", "dispatch"));
        assert!(contains_action_assignment(
            r#""action": "dispatch""#,
            "dispatch"
        ));
        assert!(!contains_action_assignment(
            r#"interaction="dispatch""#,
            "dispatch"
        ));
        assert!(contains_identifier_pair(
            "tachi_task dispatch",
            "tachi_task",
            "dispatch"
        ));
        assert!(!contains_identifier_pair(
            "tachi_task_dispatch",
            "tachi_task",
            "dispatch"
        ));
        assert!(is_teaching_context(
            "请调用tachi_complete完成任务",
            "tachi_complete"
        ));
        assert!(!is_exact_identifier_hit(
            "tachi_complete_v2",
            "tachi_complete"
        ));
    }

    #[test]
    fn f3_active_document_cannot_self_exempt_retired_tool_guidance() {
        let cases: &[(&str, &[&str])] = &[
            (
                "## Historical\nUse `run_skill` to complete the task.",
                &["run_skill"],
            ),
            (
                "Use `run_skill` (deprecated) to complete the task.",
                &["run_skill"],
            ),
            // Non-adjacent guidance under a Historical heading (previously escaped
            // actionable_retired_guidance's immediate-verb matcher).
            (
                "## Historical\nUse the run_skill API for legacy flows.",
                &["run_skill"],
            ),
            (
                "## Historical compatibility\nThe old client called tachi_task(action=dispatch).",
                &["dispatch"],
            ),
            (
                "## Historical APIs\ntachi_task(action='cancel')",
                &["cancel"],
            ),
            (
                "## Historical APIs\ntachi_skill(action='bundle')",
                &["bundle"],
            ),
            ("## Deprecated interface\ntachi_save()", &["tachi_save"]),
            ("## Historical APIs\ntachi_task dispatch", &["dispatch"]),
            ("## 历史\n请调用tachi_complete完成任务", &["tachi_complete"]),
            // Descriptive active-doc prose with inline retirement markers must still fail.
            (
                "Legacy routes `tachi_save` and `dispatch` were retired.",
                &["tachi_save"],
            ),
            (
                "Use `tachi_save` (retired only for historical compatibility).",
                &["tachi_save"],
            ),
            (
                "Deprecated clients should call tachi_save and tachi_task(action=dispatch).",
                &["tachi_save", "dispatch"],
            ),
            (
                "| Subagent dispatch | `tachi_dispatch()` + `tachi_task(action=complete)` (`tachi_complete` RETIRED) |",
                &["tachi_dispatch", "tachi_complete"],
            ),
            ("## Historical\n```md\n`run_skill()`\n```", &["run_skill"]),
            ("Use `remember` (retired) here.", &["remember"]),
        ];

        for (doc, expected_tokens) in cases {
            let hits = scan_active_document_content("docs/active.md", doc);
            for token in *expected_tokens {
                assert!(
                    hits.iter().any(|hit| hit.contains(&format!("'{token}'"))),
                    "active document must fail on retired token '{token}' despite headings/markers; doc={doc:?}, hits={hits:?}"
                );
            }
        }

        let clean_prose = "Remember to verify every structural invariant before finishing.";
        assert!(
            scan_active_document_content("docs/active.md", clean_prose).is_empty(),
            "non-teaching English prose must not false-positive"
        );
    }

    #[test]
    fn archive_directory_exclusion_is_strictly_structural_under_docs() {
        let root = std::env::temp_dir().join(format!(
            "tachi-profile-lint-archive-{}",
            uuid::Uuid::new_v4()
        ));
        let docs_archive = root.join("docs/archive");
        let docs_nested_archive = root.join("docs/engineering/archive");
        let docs_active = root.join("docs/engineering");
        let prompts_archive = root.join("prompts/archive");
        std::fs::create_dir_all(&docs_archive).expect("create docs/archive");
        std::fs::create_dir_all(&docs_nested_archive).expect("create nested docs archive");
        std::fs::create_dir_all(&docs_active).expect("create active docs dir");
        std::fs::create_dir_all(&prompts_archive).expect("create prompts/archive");

        let top_archived = docs_archive.join("old.md");
        let nested_archived = docs_nested_archive.join("nested.md");
        let active_doc = docs_active.join("active.md");
        let prompt_doc = prompts_archive.join("guide.md");
        for path in [&top_archived, &nested_archived, &active_doc, &prompt_doc] {
            std::fs::write(path, "# test\n").expect("write test doc");
        }

        let mut docs_files = Vec::new();
        collect_governed_files(&root.join("docs"), true, &mut docs_files)
            .expect("collect docs files");
        let mut prompts_files = Vec::new();
        collect_governed_files(&root.join("prompts"), false, &mut prompts_files)
            .expect("collect prompts files");
        std::fs::remove_dir_all(&root).expect("cleanup archive test directory");

        assert!(
            !docs_files.contains(&top_archived) && !docs_files.contains(&nested_archived),
            "docs/**/archive/** must be excluded structurally: {docs_files:?}"
        );
        assert!(
            docs_files.contains(&active_doc),
            "non-archived docs file must remain governed: {docs_files:?}"
        );
        assert!(
            prompts_files.contains(&prompt_doc),
            "prompts/** must not exclude archive subdirs: {prompts_files:?}"
        );
    }

    #[test]
    fn governed_file_classification_is_narrow_and_pins_active_raw_forms() {
        let root = Path::new("/repo");
        assert_eq!(
            classify_governed_file(
                root,
                &root.join("docs/engineering/architecture/example.fixture.json")
            ),
            GovernedFileKind::MachineData
        );
        assert_eq!(
            classify_governed_file(root, &root.join("docs/engineering/receipts/example.json")),
            GovernedFileKind::MachineData
        );
        assert_eq!(
            classify_governed_file(root, &root.join("docs/engineering/artifacts/example.json")),
            GovernedFileKind::MachineData
        );
        assert_eq!(
            classify_governed_file(root, &root.join("docs/engineering/receipts/example.md")),
            GovernedFileKind::Prose
        );

        // The exemption is not an extension-wide JSON/config bypass: active
        // YAML and ordinary JSON stay in the strict teaching surface.
        assert_eq!(
            classify_governed_file(root, &root.join("docs/current-state.agent.yaml")),
            GovernedFileKind::ActiveStructured
        );
        assert_eq!(
            classify_governed_file(
                root,
                &root.join("docs/engineering/architecture/ordinary.json")
            ),
            GovernedFileKind::ActiveStructured
        );
        assert_eq!(
            classify_governed_file(root, &root.join("prompts/guide.md")),
            GovernedFileKind::Prose
        );
        assert!(contains_action_assignment("actions: dispatch", "dispatch"));
        assert!(contains_action_assignment(
            r#"{"action": "dispatch"}"#,
            "dispatch"
        ));
        assert!(contains_identifier_pair(
            "tachi_task dispatch",
            "tachi_task",
            "dispatch"
        ));
        assert!(!contains_action_assignment(
            "interaction=dispatch",
            "dispatch"
        ));
    }

    #[test]
    fn governed_file_reading_fails_loudly_for_non_utf8() {
        let root = std::env::temp_dir().join(format!(
            "tachi-profile-lint-non-utf8-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).expect("create non-UTF-8 test directory");
        let path = root.join("invalid.md");
        std::fs::write(&path, [0xff, 0xfe]).expect("write non-UTF-8 test file");
        let result = read_governed_file(&path);
        std::fs::remove_dir_all(&root).expect("remove non-UTF-8 test directory");

        let error = result.expect_err("non-UTF-8 governed file must fail loudly");
        assert!(error.contains("unreadable or not UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn governed_file_collection_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "tachi-profile-lint-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&root).expect("create symlink test directory");
        let target = root.join("target.md");
        let link = root.join("link.md");
        std::fs::write(&target, "retired").expect("write symlink target");
        symlink(&target, &link).expect("create symlink test fixture");

        let mut files = Vec::new();
        let result = collect_governed_files(&root, false, &mut files);
        std::fs::remove_dir_all(&root).expect("remove symlink test directory");

        let error = result.expect_err("symlinked governed file must fail loudly");
        assert!(error.contains("symlink; symlinks are not followed"));
    }
}
