pub(crate) const OBSERVE_TOOL_PATTERNS: &[&str] = &[
    "tachi_tools",
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
    "runtime_info",
    "tachi_status",
    "tachi_doctor_scan",
    "get_edges",
    "wiki_search",
    "wiki_browse",
    // Facade read tools
    "tachi_search",
    "tachi_web_search",
    "tachi_unstick",
    "tachi_browse",
    "tachi_board",
    "tachi_agent_eval",
    "tachi_wiki",
    "tachi_skill",
    "tachi_task",
    // Unified memory facade (search action is read-only)
    "tachi_memory",
    // Continuity event facade (query action is read-only)
    "tachi_event",
    "tachi_profile",
    // Zero-param session-start alias for tachi_memory(action='briefing')
    "tachi_briefing",
];

pub(crate) const REMEMBER_TOOL_PATTERNS: &[&str] = &[
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
    // Facade wiki write (action=write)
    "tachi_wiki",
    // Facade skill run (action=run)
    "tachi_skill",
    // Unified memory facade (save / extract_facts are write ops)
    "tachi_memory",
    // Continuity event facade (emit action is append-only write)
    "tachi_event",
    // Repo-shape adapter facade (imports into continuity events/projections)
    "tachi_domain_adapter",
];

pub(crate) const COORDINATE_TOOL_PATTERNS: &[&str] = &[
    "check_inbox",
    "handoff_check",
    "handoff_leave",
    "post_card",
    "update_card",
    // Facade coordination tools
    "tachi_handoff",
    "tachi_workflow",
    "tachi_orchestrator",
    "tachi_agents",
    "tachi_dispatch",
    "approve_merge",
    // GitHub tools (bundle membership for classification; visibility gated by vault token)
    "tachi_gh",
    // Facade task dispatch/merge/board
    "tachi_task",
    // Tachi Shell — coordination/orchestration facade
    "tachi_shell",
    // Tachi Arena - tracked worker mission ledger
    "tachi_arena",
    // Tachi Verify - background verification evidence ledger
    "tachi_verify",
];

pub(crate) const OPERATE_TOOL_PATTERNS: &[&str] = &[
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

/// Standard profile allow-list. Intersected with all bundles so the IDE/CLI
/// tool tray stays small and focused on daily facade entrypoints.
pub(crate) const STANDARD_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    // Active tool discovery for the current profile
    "tachi_tools",
    // Runtime identity / DB routing self-check for embedded clients
    "runtime_info",
    // Health check: daemon status, vector coverage, foundry queue
    "tachi_status",
    // Task facade (plan / dispatch / board / merge)
    "tachi_task",
    // Tracked subagent/advisor mission ledger for main-agent delegation.
    "tachi_arena",
    // Background verification evidence ledger for runners and safe_merge.
    "tachi_verify",
    // Unified memory facade (search / save / extract_facts)
    "tachi_memory",
    // Zero-param session-start briefing (calls tachi_memory(action='briefing') internally)
    "tachi_briefing",
    // Direct notepad/conclusion saver facade (high-frequency)
    "tachi_save",
    // Live web search. Keep in standard because some agents lack host search,
    // and future wiki/research ledger flows need one canonical search intake.
    "tachi_web_search",
    // Wiki facade (search / browse / write)
    "tachi_wiki",
    // Skill facade (discover + run)
    "tachi_skill",
    // Vault status (read-only, safe in standard)
    "vault_status",
    // GitHub facade (token checked at call time, not at list time)
    "tachi_gh",
];

/// Delegate profile allow-list (7 tools). For worker agents spawned by
/// tachi_dispatch. No dispatch (prevent recursion), no handoff, no hub_discover.
pub(crate) const DELEGATE_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    "tachi_tools",
    "runtime_info",
    // Unified memory facade (search + save)
    "tachi_memory",
    // Continuity events (query + append)
    "tachi_event",
    "tachi_web_search",
    "tachi_browse",
    // Self-rescue when stuck
    "tachi_unstick",
    // Declare task completion
    "tachi_complete",
    // Execute injected/recommended skills
    "run_skill",
];
