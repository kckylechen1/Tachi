pub const OBSERVE_TOOL_PATTERNS: &[&str] = &[
    "tachi_tools",
    "tachi_wiki_search",
    "recommend_capability",
    "recommend_skill",
    "recommend_toolchain",
    // #517 soft-deprecate: standalone prepare_capability_bundle removed from
    // default observe tray — use tachi_skill(action='bundle'). Tool remains
    // registered for explicit allow-lists / backcompat callers.
    // General Hub discovery remains visible for broad observe profiles; skill
    // workflow discovery should prefer tachi_skill(action='discover').
    "hub_discover",
    "list_memories",
    "memory_stats",
    "runtime_info",
    "tachi_status",
    "wiki_search",
    // Facade read tools
    "tachi_search",
    "tachi_web_search",
    "tachi_unstick",
    "tachi_browse",
    "tachi_agent_eval",
    "tachi_wiki",
    "tachi_skill",
    "tachi_task",
    // Unified memory facade (search action is read-only)
    "tachi_memory",
    // Continuity event facade (query action is read-only)
    "tachi_event",
    // Zero-param session-start alias for tachi_memory(action='briefing')
    "tachi_briefing",
    // Component governance read model (Issue #796)
    "tachi_component",
    // Research verb (read-side evidence pipeline; #530)
    "tachi_research",
    // Peer-publication broker read surface (#1016 S1): advisory, structurally
    // read-only, self-asserted-local. Coordinate/operate/observe profiles reach
    // it through this Observe bundle; standard/delegate need it on their
    // curated allow-lists below to see it through the intersection.
    "peer_query",
];

pub const REMEMBER_TOOL_PATTERNS: &[&str] = &[
    "tachi_wiki_write",
    "tachi_wiki_ingest",
    "extract_facts",
    // #517 soft-deprecate: standalone run_skill removed from remember tray —
    // use tachi_skill(action='run'). Tool remains registered for backcompat.
    "ingest_event",
    // Facade write tool
    "tachi_save",
    "tachi_complete",
    // Facade wiki write (action=write)
    "tachi_wiki",
    // Facade skill workflow (discover / run / bundle)
    "tachi_skill",
    // Unified memory facade (save / extract_facts are write ops)
    "tachi_memory",
    // Continuity event facade (emit action is append-only write)
    "tachi_event",
    // Repo-shape adapter facade (imports into continuity events/projections)
    "tachi_domain_adapter",
];

pub const COORDINATE_TOOL_PATTERNS: &[&str] = &[
    "handoff_check",
    "handoff_leave",
    // Facade coordination tools
    "tachi_handoff",
    "tachi_workflow",
    "tachi_orchestrator",
    "tachi_agents",
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

pub const OPERATE_TOOL_PATTERNS: &[&str] = &[
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
    "hub_call",
    "hub_disconnect",
    "wiki_lint",
    // #517 soft-deprecate: dual skill entrypoints stay registered under the
    // operate surface (not standard/delegate/remember trays). Prefer
    // tachi_skill(action='run'|'bundle').
    "run_skill",
    "prepare_capability_bundle",
    // Vault session management (password-protected)
    "vault_unlock",
    "vault_lock",
    "vault_status",
];

/// Standard profile allow-list. Intersected with all bundles so the IDE/CLI
/// tool tray stays small and focused on daily facade entrypoints.
pub const STANDARD_MINIMAL_TOOL_PATTERNS: &[&str] = &[
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
    // Vault session unlock/lock must work against the default daemon profile —
    // CLI `tachi vault unlock` depends on it (#979).
    "vault_unlock",
    "vault_lock",
    // Vault status (read-only, safe in standard)
    "vault_status",
    // GitHub facade (token checked at call time, not at list time)
    "tachi_gh",
    // Peer-publication broker (#1016 S1): advisory read-only peer awareness.
    "peer_query",
    // Agent eval facade (#1066): register/observe/adjudicate/get is the
    // first-class mirror eval intake for harness-native subagents — a daily
    // facade entrypoint for any host-native session, not an admin-only tool.
    "tachi_agent_eval",
];

/// Delegate profile allow-list. For worker agents spawned by tachi_dispatch.
///
/// F3 (#495/#913): `tachi_task` is now on the list; recursive `dispatch` is
/// denied by [`super::action_policy::facade_action_allowed`] (plan/complete/
/// status/board/wait/briefing/doc_index only). `tachi_skill` is limited to
/// discover/run/bundle by the same gate.
///
/// #517 soft-deprecate: standalone `run_skill` is no longer on the default
/// delegate tray — workers use `tachi_skill(action='run')`. The tool stays
/// registered for explicit allow-lists / older injection paths.
pub const DELEGATE_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    "tachi_tools",
    "runtime_info",
    // Unified memory facade (daily actions only under action policy)
    "tachi_memory",
    // Continuity events (query + append; no promote under action policy)
    "tachi_event",
    "tachi_web_search",
    "tachi_browse",
    // Self-rescue when stuck
    "tachi_unstick",
    // Task facade — action policy denies dispatch/recommend/merge/…
    "tachi_task",
    // Declare task completion (standalone backcompat; prefer tachi_task complete)
    "tachi_complete",
    // Canonical skill workflow facade (discover/run/bundle under action policy)
    "tachi_skill",
    // Peer-publication broker (#1016 S1): a worker lane reads a peer's advisory
    // presence to avoid colliding blind. Read-only; no write path exists.
    "peer_query",
];
