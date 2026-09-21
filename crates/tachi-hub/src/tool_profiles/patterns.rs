pub const OBSERVE_TOOL_PATTERNS: &[&str] = &[
    "tachi_a2a",
    "tachi_tools",
    "tachi_wiki_search",
    // General Hub discovery remains visible for broad observe profiles; skill
    // workflow discovery should prefer tachi_skill(action='discover').
    "hub_discover",
    "list_memories",
    "memory_stats",
    "runtime_info",
    "tachi_status",
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
    // Research verb (read-side evidence pipeline; #530)
    "tachi_research",
    // Peer-publication broker read surface (#1016 S1): advisory, structurally
    // read-only, self-asserted-local. Coordinate/operate/observe profiles reach
    // it through this Observe bundle; standard/delegate need it on their
    // curated allow-lists below to see it through the intersection.
    "peer_query",
];

pub const REMEMBER_TOOL_PATTERNS: &[&str] = &[
    "tachi_a2a",
    "tachi_wiki_write",
    "tachi_wiki_ingest",
    "extract_facts",
    // #517 soft-deprecate: standalone run_skill removed from remember tray —
    // use tachi_skill(action='run'). Tool remains registered for backcompat.
    "ingest_event",
    // Facade wiki write (action=write)
    "tachi_wiki",
    // Facade skill workflow (discover / run / bundle)
    "tachi_skill",
    // Unified memory facade (save / extract_facts are write ops)
    "tachi_memory",
    // Repo-shape adapter facade (imports into continuity events/projections)
    "tachi_domain_adapter",
];

pub const COORDINATE_TOOL_PATTERNS: &[&str] = &[
    // #1099: `handoff_check`/`handoff_leave` direct routes retired (leave/
    // check superseded by A2A/orchestrator, see #1016). `tachi_handoff`
    // survives, narrowed to its one action without a replacement
    // (`promote_issue`).
    // Facade coordination tools
    "tachi_handoff",
    // #1679 durable delivery seam: requester-side claim/ack/block/resume
    // receipts over the delivery spine (coordinate/admin only).
    "tachi_delivery",
    "tachi_agents",
    // GitHub tools (bundle membership for classification; visibility gated by vault token)
    "tachi_gh",
    // Facade task dispatch/merge/board
    "tachi_task",
    // Tachi Staff — external staffing facade (coordination/orchestration)
    "tachi_staff",
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
    "get_pipeline_status",
    "sync_memories",
    "hub_call",
    "hub_disconnect",
    "wiki_lint",
    // Vault session management (password-protected)
    "vault_unlock",
    "vault_lock",
    "vault_status",
];

/// Ordinary Lead profile allow-list. The five product facades are the complete
/// default model-facing surface; diagnostics remain on explicit Ops/admin
/// profiles without claiming that their registered routes were deleted.
pub const STANDARD_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    "tachi_memory",
    "tachi_task",
    "tachi_staff",
    "tachi_gh",
    "tachi_a2a",
];

/// Worker profile allow-list. Discovery matches the Lead product surface;
/// action policy still denies recursive staffing and GitHub mutation.
pub const DELEGATE_MINIMAL_TOOL_PATTERNS: &[&str] = &[
    "tachi_memory",
    "tachi_task",
    "tachi_staff",
    "tachi_gh",
    "tachi_a2a",
];
