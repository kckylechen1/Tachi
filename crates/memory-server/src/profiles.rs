use rmcp::model::Tool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolBundle {
    Observe,
    Remember,
    Coordinate,
    Operate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolProfile {
    observe: bool,
    remember: bool,
    coordinate: bool,
    operate: bool,
    admin: bool,
    /// Standard profile: curated 10-tool allow-list for IDE + CLI agents.
    /// When set, only patterns in STANDARD_MINIMAL_TOOL_PATTERNS pass.
    standard_minimal: bool,
    /// Delegate profile: curated 6-tool allow-list for worker agents.
    /// When set, only patterns in DELEGATE_MINIMAL_TOOL_PATTERNS pass.
    /// Standard takes precedence over delegate if both are set.
    delegate_minimal: bool,
}

impl ToolProfile {
    const fn observe() -> Self {
        Self {
            observe: true,
            remember: false,
            coordinate: false,
            operate: false,
            admin: false,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    const fn remember() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: false,
            operate: false,
            admin: false,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    const fn coordinate() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: true,
            operate: false,
            admin: false,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    const fn operate() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: false,
            operate: true,
            admin: false,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    const fn admin() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: true,
            operate: true,
            admin: true,
            standard_minimal: false,
            delegate_minimal: false,
        }
    }

    /// Standard profile for IDE + CLI agents (Windsurf, Cursor, Antigravity,
    /// Trae, Codex standalone, Claude Code standalone). Enables all bundles but
    /// intersects with a curated 12-tool allow-list to keep the tool tray small.
    const fn standard() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: true,
            operate: true,
            admin: false,
            standard_minimal: true,
            delegate_minimal: false,
        }
    }

    /// Delegate profile for worker agents spawned by tachi_dispatch.
    /// Read + remember bundles only, intersected with a curated 7-tool allow-list.
    /// No dispatch (prevent recursion), no handoff (parent manages), no hub_discover.
    const fn delegate() -> Self {
        Self {
            observe: true,
            remember: true,
            coordinate: false,
            operate: false,
            admin: false,
            standard_minimal: false,
            delegate_minimal: true,
        }
    }

    fn merge(self, other: Self) -> Self {
        Self {
            observe: self.observe || other.observe,
            remember: self.remember || other.remember,
            coordinate: self.coordinate || other.coordinate,
            operate: self.operate || other.operate,
            admin: self.admin || other.admin,
            // Minimal allow-lists are sticky. Standard > delegate if both set.
            standard_minimal: self.standard_minimal || other.standard_minimal,
            delegate_minimal: self.delegate_minimal || other.delegate_minimal,
        }
    }

    fn allows(self, bundle: ToolBundle) -> bool {
        self.admin
            || match bundle {
                ToolBundle::Observe => self.observe,
                ToolBundle::Remember => self.remember,
                ToolBundle::Coordinate => self.coordinate,
                ToolBundle::Operate => self.operate,
            }
    }

    pub(super) fn as_str(self) -> String {
        if self.admin {
            return "admin".to_string();
        }
        if self.standard_minimal {
            return "standard".to_string();
        }
        if self.delegate_minimal {
            return "delegate".to_string();
        }

        let mut names = Vec::new();
        if self.observe {
            names.push("observe");
        }
        if self.remember {
            names.push("remember");
        }
        if self.coordinate {
            names.push("coordinate");
        }
        if self.operate {
            names.push("operate");
        }
        if names.is_empty() {
            "observe".to_string()
        } else {
            names.join(",")
        }
    }
}

pub(super) const fn default_tool_profile() -> ToolProfile {
    ToolProfile::standard()
}

const OBSERVE_TOOL_PATTERNS: &[&str] = &[
    "tachi_task_brief",
    "tachi_progress_check",
    "tachi_wiki_search",
    "recommend_capability",
    "recommend_skill",
    "recommend_toolchain",
    "prepare_capability_bundle",
    "hub_discover",
    "search_memory",
    "get_memory",
    "memory_graph",
    "list_memories",
    "memory_stats",
    "get_edges",
    "wiki_search",
    "wiki_browse",
    // Facade read tools
    "tachi_search",
    "tachi_web_search",
    "tachi_plan",
    "tachi_unstick",
    "tachi_browse",
    "tachi_board",
];

const REMEMBER_TOOL_PATTERNS: &[&str] = &[
    "save_memory",
    "remember",
    "tachi_wiki_write",
    "tachi_wiki_ingest",
    "extract_facts",
    "run_skill",
    "ingest_event",
    // Facade write tool
    "tachi_save",
    "tachi_complete",
];

const COORDINATE_TOOL_PATTERNS: &[&str] = &[
    "check_inbox",
    "handoff_check",
    "handoff_leave",
    "post_card",
    "update_card",
    // Facade coordination tools
    "tachi_handoff",
    "tachi_dispatch",
    "approve_merge",
    // GitHub tools (bundle membership for classification; visibility gated by vault token)
    "tachi_gh_issue_read",
    "tachi_gh_issue_list",
    "tachi_gh_issue_create",
    "tachi_gh_pr_read",
    "tachi_gh_pr_list",
    "tachi_gh_repo_view",
];

/// GitHub MCP proxy tools — only exposed when Vault has GH_TOKEN.
pub(super) const GH_TOOL_PATTERNS: &[&str] = &[
    "tachi_gh_issue_read",
    "tachi_gh_issue_list",
    "tachi_gh_issue_create",
    "tachi_gh_pr_read",
    "tachi_gh_pr_list",
    "tachi_gh_repo_view",
];

const OPERATE_TOOL_PATTERNS: &[&str] = &[
    "section_build",
    "compact_context",
    "compact_rollup",
    "compact_session_memory",
    "recall_context",
    "capture_session",
    "archive_memory",
    "find_similar_memory",
    "get_pipeline_status",
    "sync_memories",
    "agent_register",
    "agent_whoami",
    "synthesize_agent_evolution",
    "project_agent_profile",
    "queue_agent_evolution",
    "review_agent_evolution_proposal",
    "list_agent_evolution_proposals",
    "hub_call",
    "hub_disconnect",
    "wiki_lint",
    // Vault session management (password-protected)
    "vault_unlock",
    "vault_lock",
    "vault_status",
];

/// Standard profile allow-list. Intersected with all bundles
/// so the IDE/CLI tool tray stays small and focused.
const STANDARD_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    // Planning + context
    "tachi_plan",
    "recall_context",
    // Unified search (wiki + memory)
    "tachi_search",
    // Live web search
    "tachi_web_search",
    // Wiki browse
    "tachi_browse",
    // Unified save (wiki + memory + note)
    "tachi_save",
    // Unified handoff (leave + check)
    "tachi_handoff",
    // Skill discovery + execution
    "hub_discover",
    "run_skill",
    // Agent dispatch + task completion + merge
    "tachi_dispatch",
    "tachi_complete",
    "approve_merge",
    "tachi_board",
    // GitHub tools: conditionally added by has_gh_token, not in fixed list.
    // Vault management (password-protected, safe in standard)
    "vault_unlock",
    "vault_lock",
    "vault_status",
];

/// Delegate profile allow-list (7 tools). For worker agents spawned by
/// tachi_dispatch. No dispatch (prevent recursion), no handoff, no hub_discover.
const DELEGATE_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    // Search + browse
    "tachi_search",
    "tachi_web_search",
    "tachi_browse",
    // Save (if authorized)
    "tachi_save",
    // Self-rescue when stuck
    "tachi_unstick",
    // Declare task completion
    "tachi_complete",
    // Execute injected/recommended skills
    "run_skill",
];

#[cfg(test)]
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
    "tachi_doctor_scan",
    "tachi_init_project_db",
    "vault_get",
    "vault_init",
    "vault_list",
    "vault_remove",
    "vault_set",
    "vault_setup_rotation",
    "vc_bind",
    "vc_list",
    "vc_register",
    "vc_resolve",
];

#[cfg(test)]
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
    "tachi_save",
    "tachi_wiki_write",
    "update_card",
];

pub(super) fn parse_tool_profile(raw: &str) -> Option<ToolProfile> {
    let mut resolved: Option<ToolProfile> = None;

    for token in raw
        .split([',', '+'])
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let token_profile = match token.to_ascii_lowercase().as_str() {
            "observe" | "read" | "reader" => ToolProfile::observe(),
            "remember" | "write" | "writer" | "agent" => ToolProfile::remember(),
            "standard" | "ide" | "cursor" | "trae" | "windsurf" | "antigravity" | "claude"
            | "claude-code" | "codex" => ToolProfile::standard(),
            "delegate" | "worker" | "subagent" => ToolProfile::delegate(),
            "coordinate" => ToolProfile::coordinate(),
            "companion" | "copilot" | "coach" => ToolProfile::remember()
                .merge(ToolProfile::coordinate())
                .merge(ToolProfile::operate()),
            "workflow" => ToolProfile::coordinate().merge(ToolProfile::operate()),
            "operate" | "runtime" | "openclaw" | "hermes" | "adapter" | "ops" => {
                ToolProfile::operate()
            }
            "admin" | "full" => ToolProfile::admin(),
            _ => return None,
        };
        resolved = Some(match resolved {
            Some(profile) => profile.merge(token_profile),
            None => token_profile,
        });
    }

    resolved
}

pub(super) fn parse_tool_patterns_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) fn filter_tool_defs(
    tools: Vec<Tool>,
    profile: Option<ToolProfile>,
    env_patterns: Option<&[String]>,
    has_gh_token: bool,
) -> Vec<Tool> {
    tools
        .into_iter()
        .filter(|tool| {
            let name = tool.name.as_ref();
            if matches_any_pattern(name, GH_TOOL_PATTERNS.iter().copied()) {
                return has_gh_token && tool_visible_gh(name, profile);
            }
            tool_visible(name, profile, env_patterns)
        })
        .collect()
}

/// GH tools visibility: require standard/coordinate/admin profile.
fn tool_visible_gh(tool_name: &str, profile: Option<ToolProfile>) -> bool {
    let profile = profile.unwrap_or_else(default_tool_profile);
    if profile.admin {
        return true;
    }
    if profile.standard_minimal {
        return true;
    }
    profile.allows(ToolBundle::Coordinate)
}

#[cfg(test)]
fn tool_matches_bundle(tool_name: &str, bundle: ToolBundle) -> bool {
    matches_any_pattern(
        tool_name,
        match bundle {
            ToolBundle::Observe => OBSERVE_TOOL_PATTERNS.iter().copied(),
            ToolBundle::Remember => REMEMBER_TOOL_PATTERNS.iter().copied(),
            ToolBundle::Coordinate => COORDINATE_TOOL_PATTERNS.iter().copied(),
            ToolBundle::Operate => OPERATE_TOOL_PATTERNS.iter().copied(),
        },
    )
}

pub(super) fn tool_visible(
    tool_name: &str,
    profile: Option<ToolProfile>,
    env_patterns: Option<&[String]>,
) -> bool {
    if let Some(patterns) = env_patterns {
        if !matches_any_pattern(tool_name, patterns.iter().map(String::as_str)) {
            return false;
        }
    }

    let profile = profile.unwrap_or_else(default_tool_profile);
    if profile.admin {
        return true;
    }

    // Curated minimal allow-lists: standard (12 tools) > delegate (7 tools).
    if profile.standard_minimal {
        if !matches_any_pattern(tool_name, STANDARD_MINIMAL_TOOL_PATTERNS.iter().copied()) {
            return false;
        }
    } else if profile.delegate_minimal {
        if !matches_any_pattern(tool_name, DELEGATE_MINIMAL_TOOL_PATTERNS.iter().copied()) {
            return false;
        }
    }

    profile.allows(ToolBundle::Observe)
        && matches_any_pattern(tool_name, OBSERVE_TOOL_PATTERNS.iter().copied())
        || profile.allows(ToolBundle::Remember)
            && matches_any_pattern(tool_name, REMEMBER_TOOL_PATTERNS.iter().copied())
        || profile.allows(ToolBundle::Coordinate)
            && matches_any_pattern(tool_name, COORDINATE_TOOL_PATTERNS.iter().copied())
        || profile.allows(ToolBundle::Operate)
            && matches_any_pattern(tool_name, OPERATE_TOOL_PATTERNS.iter().copied())
}

fn matches_any_pattern<'a>(tool_name: &str, mut patterns: impl Iterator<Item = &'a str>) -> bool {
    patterns.any(|pattern| tool_name_matches_pattern(tool_name, pattern))
}

pub(super) fn tool_name_matches_pattern(tool_name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return tool_name == pattern;
    }

    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let segments: Vec<&str> = pattern
        .split('*')
        .filter(|segment| !segment.is_empty())
        .collect();

    if segments.is_empty() {
        return true;
    }

    let mut cursor = 0usize;
    let mut first = true;

    for segment in segments {
        let Some(found_at) = tool_name[cursor..].find(segment) else {
            return false;
        };
        let absolute = cursor + found_at;
        if first && anchored_start && absolute != 0 {
            return false;
        }
        cursor = absolute + segment.len();
        first = false;
    }

    if anchored_end {
        cursor == tool_name.len()
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            false,
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
            false,
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
            false,
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
            .chain(GH_TOOL_PATTERNS.iter())
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
                crate::CACHE_INVALIDATING_TOOLS.contains(tool_name),
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
                test_tool("tachi_plan"),
                test_tool("search_memory"),
                test_tool("save_memory"),
                test_tool("hub_register"),
            ],
            None,
            None,
            false,
        );
        let names: Vec<String> = filtered
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        assert_eq!(names, vec!["tachi_plan".to_string()]);
    }

    #[test]
    fn standard_profile_restricts_to_allow_list() {
        // Without GH token: GH tools excluded
        let filtered = filter_tool_defs(
            vec![
                test_tool("tachi_plan"),
                test_tool("tachi_search"),
                test_tool("tachi_web_search"),
                test_tool("tachi_browse"),
                test_tool("tachi_save"),
                test_tool("tachi_handoff"),
                test_tool("hub_discover"),
                test_tool("run_skill"),
                test_tool("recall_context"),
                test_tool("tachi_complete"),
                test_tool("tachi_dispatch"),
                test_tool("approve_merge"),
                test_tool("tachi_board"),
                test_tool("tachi_gh_issue_read"),
                test_tool("tachi_gh_issue_list"),
                test_tool("tachi_gh_pr_read"),
                test_tool("tachi_gh_pr_list"),
                test_tool("tachi_gh_repo_view"),
                test_tool("tachi_gh_issue_create"),
                test_tool("search_memory"),
                test_tool("save_memory"),
                test_tool("tachi_unstick"),
                test_tool("get_memory"),
                test_tool("post_card"),
            ],
            Some(ToolProfile::standard()),
            None,
            false, // no GH token
        );
        let names: Vec<String> = filtered
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "tachi_plan".to_string(),
                "tachi_search".to_string(),
                "tachi_web_search".to_string(),
                "tachi_browse".to_string(),
                "tachi_save".to_string(),
                "tachi_handoff".to_string(),
                "hub_discover".to_string(),
                "run_skill".to_string(),
                "recall_context".to_string(),
                "tachi_complete".to_string(),
                "tachi_dispatch".to_string(),
                "approve_merge".to_string(),
                "tachi_board".to_string(),
            ]
        );
    }

    #[test]
    fn standard_profile_includes_gh_tools_when_token_present() {
        // With GH token: GH read tools included, create excluded (not in standard)
        let filtered = filter_tool_defs(
            vec![
                test_tool("tachi_plan"),
                test_tool("tachi_gh_issue_read"),
                test_tool("tachi_gh_issue_list"),
                test_tool("tachi_gh_pr_read"),
                test_tool("tachi_gh_pr_list"),
                test_tool("tachi_gh_repo_view"),
                test_tool("tachi_gh_issue_create"),
            ],
            Some(ToolProfile::standard()),
            None,
            true, // has GH token
        );
        let names: Vec<String> = filtered
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "tachi_plan".to_string(),
                "tachi_gh_issue_read".to_string(),
                "tachi_gh_issue_list".to_string(),
                "tachi_gh_pr_read".to_string(),
                "tachi_gh_pr_list".to_string(),
                "tachi_gh_repo_view".to_string(),
                "tachi_gh_issue_create".to_string(),
            ]
        );
    }

    #[test]
    fn delegate_profile_restricts_to_allow_list() {
        let filtered = filter_tool_defs(
            vec![
                // Delegate tools (should pass)
                test_tool("tachi_search"),
                test_tool("tachi_web_search"),
                test_tool("tachi_browse"),
                test_tool("tachi_save"),
                test_tool("tachi_unstick"),
                test_tool("tachi_complete"),
                test_tool("run_skill"),
                // Should be excluded:
                test_tool("tachi_plan"),
                test_tool("tachi_handoff"),
                test_tool("tachi_dispatch"),
                test_tool("hub_discover"),
                test_tool("recall_context"),
                test_tool("search_memory"),
                // GH tools should be excluded even with token:
                test_tool("tachi_gh_issue_list"),
            ],
            Some(ToolProfile::delegate()),
            None,
            true, // even with GH token, delegate shouldn't see GH tools
        );
        let names: Vec<String> = filtered
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "tachi_search".to_string(),
                "tachi_web_search".to_string(),
                "tachi_browse".to_string(),
                "tachi_save".to_string(),
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
}
