use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tachi_hub::{
    tool_matches_bundle, tool_name_matches_pattern, ToolBundle, COORDINATE_TOOL_PATTERNS,
    DELEGATE_MINIMAL_TOOL_PATTERNS, OBSERVE_TOOL_PATTERNS, OPERATE_TOOL_PATTERNS,
    REMEMBER_TOOL_PATTERNS, STANDARD_MINIMAL_TOOL_PATTERNS,
};
use tachi_params::TACHI_SKILL_ACTIONS;

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
    "dlq_list",
    "dlq_retry",
    "hub_export_skills",
    "hub_feedback",
    "hub_get",
    "hub_quick_add",
    "hub_register",
    "hub_review",
    "hub_set_active_version",
    "hub_set_enabled",
    "hub_stats",
    "sandbox_check",
    "sandbox_exec_audit",
    "sandbox_get_policy",
    "sandbox_list_policies",
    "sandbox_set_policy",
    "sandbox_set_rule",
    "tachi_audit_log",
    "tachi_component",
    "tachi_event",
    "tachi_init_project_db",
    // tachi_research is now observe-bundled (#963/#530), no longer admin-only.
    // #757 Cut3-S1: folded sandbox verb (its six forwarding aliases above are
    // also admin-only) — absent from every profile bundle.
    "tachi_sandbox",
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
    "tachi_domain_adapter",
    "tachi_handoff",
    "tachi_memory",
    "tachi_verify",
    "tachi_wiki_write",
];

const RETIRED_NATIVE_ALIASES: &[&str] = &[
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
    "tachi_orchestrator",
    "tachi_plan",
    "tachi_progress_check",
    "tachi_save",
    "tachi_task_brief",
    "update_card",
    "wiki_browse",
    "wiki_search",
];

const FOLDED_NATIVE_COMPAT_TOOLS: &[&str] = &[];

const PROFILE_RETIRED_DIRECT_TOOLS: &[&str] = &[];

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
fn skill_facade_advertises_canonical_actions() {
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

/// Retired-surface documentation lint (issue #1830).
/// Derives retired tokens from RETIRED_NATIVE_ALIASES (this file) +
/// complement of TACHI_SKILL_ACTIONS against pinned superset (tied to census
/// fixture with comment: see crates/tachi-params/src/facade/action_inventory.rs:289
/// assert_eq!(TACHI_SKILL_ACTIONS, &["discover", "run"])).
/// Scans ACTIVE documentation surface line-by-line: `docs/**` (EXCLUDING
/// `docs/archive/**`), `README.md`, `README.zh-CN.md`, `prompts/**`.
/// Line-based. Exempt only if line carries a retirement marker or under nearest
/// preceding section header (#...) carrying one.
/// Markers: retired, RETIRED, 退役, historical, 历史.
/// Match EXACT identifier tokens (bounded non-alnum, e.g. `action="loadout"`,
/// `from_pattern`, `recommend_capability`, `run_skill`) to avoid false hits on
/// unrelated prose. For tachi_skill retired, hit only on tachi_skill ctx or
/// action="..." forms (other words like "bundle"/"loadout" are common prose).
/// Must pass on current tree. Negative/prohibition teaching lines (e.g. backcompat
/// listings naming old tokens) are counted separately; pure marker is the gate.
/// Conservative: do not weaken positive detection.
#[test]
fn retired_surface_documentation_has_markers() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).join("../..");

    let mut files: Vec<std::path::PathBuf> = vec![];
    // READMEs
    for name in ["README.md", "README.zh-CN.md"] {
        let p = repo_root.join(name);
        if p.exists() {
            files.push(p);
        }
    }
    // prompts/** (recursive per B2)
    let prompts_dir = repo_root.join("prompts");
    if prompts_dir.is_dir() {
        collect_prompts_files(&prompts_dir, &mut files);
    }
    // docs/** (exclude archive)
    let docs_dir = repo_root.join("docs");
    if docs_dir.is_dir() {
        collect_docs_files(&docs_dir, &mut files);
    }

    let marker_words = ["retired", "RETIRED", "退役", "历史", "historical"];

    let mut bad_hits: Vec<String> = vec![];

    // derive retired tokens from code (machine-known, no hand copy of full list)
    let native_tokens: &[&str] = RETIRED_NATIVE_ALIASES;
    // reuse the router retired check rather than duplicate (B4)
    retired_native_aliases_stay_retired();
    // B4 self-maintaining: historical superset (all known skill actions ever) MINUS live = retired set.
    // cross-assertion: superset names must be live or retired (so adding live requires updating superset;
    // retiring a skill without having had it in superset would make retirement invisible to docs lint).
    const HISTORICAL_SKILL_SUPERSET: &[&str] =
        &["discover", "run", "bundle", "loadout", "from_pattern"];
    let live_skill: Vec<&str> = TACHI_SKILL_ACTIONS.to_vec();
    for &name in &live_skill {
        assert!(
            HISTORICAL_SKILL_SUPERSET.contains(&name),
            "live skill action '{}' must be listed in HISTORICAL_SKILL_SUPERSET so future retirement is visible to lint",
            name
        );
    }
    let skill_tokens: Vec<&str> = HISTORICAL_SKILL_SUPERSET
        .iter()
        .filter(|a| !live_skill.contains(a))
        .copied()
        .collect();
    for &name in HISTORICAL_SKILL_SUPERSET {
        let is_live = live_skill.contains(&name);
        let is_retired = skill_tokens.contains(&name);
        assert!(
            is_live || is_retired,
            "superset name '{}' must be EITHER in TACHI_SKILL_ACTIONS (live) OR in lint retired set (invariant: superset = live ∪ retired always)",
            name
        );
    }

    for file in &files {
        let content = match std::fs::read_to_string(file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let lines: Vec<&str> = content.lines().collect();
        let rel = file
            .strip_prefix(&repo_root)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string();

        let mut in_code_fence = false;
        let mut current_section_header = "";
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                in_code_fence = !in_code_fence;
            }
            if !in_code_fence && trimmed.starts_with('#') {
                current_section_header = line;
            }
            let section_header = current_section_header;

            let line_has_marker = marker_words.iter().any(|m| line.contains(m));
            let header_has_marker = marker_words.iter().any(|m| section_header.contains(m));
            let exempted_by_marker = line_has_marker || header_has_marker;

            // native retired: exact bounded ident token + instruction context (B1)
            // heuristics: imperative verbs (use|call|invoke|run|via|通过|调用|使用), code-fence/json/inline-code presence,
            // or table row presenting token as available. Zero FP bar on clean tree.
            for tok in native_tokens {
                if is_exact_identifier_hit(line, tok)
                    && is_teaching_context(line, tok)
                    && !exempted_by_marker
                {
                    bad_hits.push(format!(
                        "{}:{}: unmarked retired token '{}'",
                        rel,
                        i + 1,
                        tok
                    ));
                }
            }
            // tachi_skill retired tokens: only in tachi_skill or action= form (exact)
            for tok in &skill_tokens {
                let has_tachi_skill_ctx = line.contains("tachi_skill");
                let has_action_form = line.contains(&format!("action=\"{}\"", tok))
                    || line.contains(&format!("action='{}'", tok));
                if (has_tachi_skill_ctx || has_action_form)
                    && is_exact_identifier_hit(line, tok)
                    && !exempted_by_marker
                {
                    bad_hits.push(format!(
                        "{}:{}: unmarked retired tachi_skill token '{}'",
                        rel,
                        i + 1,
                        tok
                    ));
                }
            }
        }
    }

    assert!(
        bad_hits.is_empty(),
        "retired tokens found in active documentation surface without retirement marker (on line or section header); fix docs or matcher:\n{}",
        bad_hits.join("\n")
    );
}

fn is_exact_identifier_hit(line: &str, tok: &str) -> bool {
    if !line.contains(tok) {
        return false;
    }
    let mut search_start = 0usize;
    while let Some(idx) = line[search_start..].find(tok) {
        let abs_idx = search_start + idx;
        let before = if abs_idx == 0 {
            '\0'
        } else {
            line.as_bytes()[abs_idx - 1] as char
        };
        let after_pos = abs_idx + tok.len();
        let after = if after_pos >= line.len() {
            '\0'
        } else {
            line.as_bytes()[after_pos] as char
        };
        if !before.is_alphanumeric() && before != '_' && !after.is_alphanumeric() && after != '_' {
            return true;
        }
        search_start = abs_idx + 1;
    }
    false
}

fn is_teaching_context(line: &str, tok: &str) -> bool {
    let l = line.to_lowercase();
    let t = tok.to_lowercase();
    // B1: imperative verb immediately before token (narrowed to zero-fp on README lists/tables; via dropped)
    if l.contains(&format!("use {}", t))
        || l.contains(&format!("use `{}", t))
        || l.contains(&format!("call {}", t))
        || l.contains(&format!("call `{}", t))
        || l.contains(&format!("invoke {}", t))
        || l.contains(&format!("invoke `{}", t))
        || l.contains(&format!("run {}", t))
        || l.contains(&format!("run `{}", t))
        || l.contains(&format!("通过{}", t))
        || l.contains(&format!("调用{}", t))
        || l.contains(&format!("使用{}", t))
        || l.contains(&format!("执行{}", t))
    {
        return true;
    }
    // usage forms: tok( or action=
    if line.contains(&format!("{}(", tok))
        || line.contains(&format!("action=\"{}\"", tok))
        || line.contains(&format!("action='{}'", tok))
    {
        return true;
    }
    false
}

fn collect_docs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "archive" {
                continue;
            }
            if p.is_dir() {
                collect_docs_files(&p, out);
            } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if ext == "md" || ext == "mdx" {
                    out.push(p);
                }
            }
        }
    }
}

fn collect_prompts_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_prompts_files(&p, out);
            } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if ext == "md" || ext == "mdx" || ext == "txt" {
                    out.push(p);
                }
            }
        }
    }
}
