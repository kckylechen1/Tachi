use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

// ─── Facade: unified search ──────────────────────────────────────────────────

fn default_facade_search_scope() -> String {
    "all".to_string()
}

fn default_facade_top_k() -> usize {
    6
}

fn default_profile_max_chars() -> usize {
    4_000
}

fn default_profile_dry_run() -> bool {
    true
}

pub const MAX_FACADE_TOP_K: usize = 100;

pub fn clamp_facade_top_k(top_k: usize) -> usize {
    top_k.clamp(1, MAX_FACADE_TOP_K)
}

fn string_enum_schema(
    values: &[&str],
    description: &str,
    _: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    rmcp::schemars::json_schema!({
        "type": "string",
        "enum": values,
        "description": description
    })
}

fn tachi_memory_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "search",
            "get",
            "save",
            "extract_facts",
            "briefing",
            "checkpoint",
            "alerts",
            "ask",
            "consolidate",
            "recall_simulate",
            "recall_proposals",
            "review_recall_proposal",
            "apply_recall_proposals",
            "progress",
            "readiness",
        ],
        "Required Tachi memory facade action.",
        generator,
    )
}

fn tachi_event_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "emit",
            "query",
            "metrics",
            "project",
            "context",
            "label_eval",
        ],
        "Required Tachi event ledger action.",
        generator,
    )
}

fn tachi_profile_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["import", "render", "context"],
        "Required Tachi profile action. import builds a canonical AgentProfilePack from supplied docs; render returns dry-run target files; context returns a bounded runtime alignment block.",
        generator,
    )
}

fn tachi_wiki_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["search", "browse", "read", "write"],
        "Required Tachi wiki facade action.",
        generator,
    )
}

fn tachi_skill_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &["discover", "run", "bundle", "loadout", "from_pattern"],
        "Required Tachi skill facade action.",
        generator,
    )
}

fn tachi_task_action_schema(
    generator: &mut rmcp::schemars::SchemaGenerator,
) -> rmcp::schemars::Schema {
    string_enum_schema(
        &[
            "plan",
            "briefing",
            "doc_index",
            "recommend",
            "dispatch",
            "complete",
            "profiles",
            "profile",
            "card",
            "route_simulate",
            "proposals",
            "review_proposal",
            "apply_proposals",
            "status",
            "cancel",
            "board",
            "wait",
            "merge",
            "intake",
            "link_pr",
            "cycle_status",
            "pr_status",
            "pr_handoff",
            "release_note",
            "ux_matrix",
            "build_references",
            "close_loop",
        ],
        "Required Tachi task facade action. action='briefing' returns a feature-scoped handoff board; action='doc_index' returns the same project-first layered source index for agent context assembly; action='status' reads one dispatch ledger and may query backend-local status; action='cancel' requests cooperative cancellation for a dispatch backend that supports it; action='wait' polls a dispatch until terminal state; action='complete' records evaluated completion evidence; action='route_simulate' replays recent /eval rows across current, cost_sensitive, and quality_first routing policies without mutating policy; action='proposals' lists/generates route-policy and loadout-evolution proposals from replay/eval evidence; action='review_proposal' approves/rejects a proposal; action='apply_proposals' persists an approved route-policy rule without silently mutating recommendation scoring; approved loadout-evolution proposals wait for MBIT/profile-card projection; action='intake' binds a GitHub issue to a Tachi flow; action='link_pr' attaches a PR to a flow; action='cycle_status' returns a read-only project lifecycle status from linked issue/PR, docs/specs, flow artifacts, verification, and closure state; action='pr_status' previews GitHub PR safe-merge status without merging; action='pr_handoff' writes a PR body/branch handoff with verification and known gaps; action='release_note' synthesizes a release/changelog note from flow GitHub state, docs, and verification evidence; action='ux_matrix' writes/returns a feature UX workflow checklist for the issue→briefing→dispatch→PR→release lifecycle; action='close_loop' writes issue/doc/wiki closure; action='merge' is local dispatched worktree git merge only; use tachi_gh(action='safe_merge') to execute GitHub PR merges.",
        generator,
    )
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiSearchParams {
    /// Search query text. tachi_search is the human-readable/markdown alias of
    /// tachi_memory(action=search) (which defaults to JSON); same retrieval, formatted output.
    pub query: String,

    /// Scope: "wiki" searches wiki entries, "memory" searches general memory, "all" searches both (default), "sft" searches training/distillation corpus.
    #[serde(default = "default_facade_search_scope")]
    pub scope: String,

    /// Number of results to return (default: 6)
    #[serde(default = "default_facade_top_k")]
    pub top_k: usize,

    /// Optional path prefix filter
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    #[schemars(
        description = "Named project library under ~/.tachi/projects/<name>/memory.db. When set, search/save targets ONLY that library (not the daemon-bound workspace DB). Omit to use global + daemon-bound project DB."
    )]
    pub project: Option<String>,

    /// Optional domain filter
    #[serde(default)]
    #[schemars(
        description = "Optional area tag filter (e.g. rust, mcp). Does not select the DB — use project for library targeting."
    )]
    pub domain: Option<String>,

    #[serde(default)]
    pub file_context: Option<String>,

    #[serde(default)]
    pub error_context: Option<String>,

    /// Optional caller-supplied context tokens used to bias memory/wiki recall
    /// without overwriting exact ID-like queries. Kept as `context_symbols` for
    /// compatibility with HyperMemory adapters, but values are domain-neutral.
    #[serde(default)]
    pub context_symbols: Vec<String>,

    /// Wiki category filter (only used when scope includes wiki)
    #[serde(default)]
    pub category: Option<String>,

    /// Whether to include archived entries
    #[serde(default)]
    pub include_archived: bool,

    /// Include training/distillation corpus entries such as `/sft/...`.
    /// Defaults to false for agent recall; use `scope="sft"` or this flag to opt in.
    #[serde(default)]
    pub include_training: bool,

    /// Enable adaptive Voyage reranking for close top results in memory search.
    #[serde(default)]
    pub enable_rerank: bool,

    /// Point-in-time validity filter (ISO 8601). Returns only memories valid at this time.
    #[serde(default)]
    pub as_of: Option<String>,
}

// ─── Facade: web search ──────────────────────────────────────────────────────

fn default_web_search_top_k() -> usize {
    8
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiWebSearchParams {
    /// Web search query text
    pub query: String,

    /// Number of results to request when the backend supports it
    #[serde(default = "default_web_search_top_k")]
    pub top_k: usize,

    /// Backend selector: "auto" (default), "exa", "tavily", "bigmodel", or a concrete capability id
    #[serde(default)]
    pub backend: Option<String>,

    /// Explicit tool name override for advanced/debug usage
    #[serde(default)]
    pub tool_name: Option<String>,

    /// Optional domains to include, if the selected backend supports it
    #[serde(default)]
    pub include_domains: Vec<String>,

    /// Optional domains to exclude, if the selected backend supports it
    #[serde(default)]
    pub exclude_domains: Vec<String>,
}

// ─── Facade: unified save ────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiSaveParams {
    /// Full text content
    pub text: String,

    /// Existing memory ID to update. When set, updates the entry instead of creating a new one.
    #[serde(default)]
    pub id: Option<String>,

    /// What to save: "wiki" for wiki entry, "note" for a quick note, "memory" for full memory entry.
    /// If omitted, auto-detected: title present → wiki; short/casual text → note; otherwise → memory.
    #[serde(default)]
    pub kind: Option<String>,

    /// Title (required for wiki entries, ignored for notes)
    #[serde(default)]
    pub title: Option<String>,

    /// Short summary
    #[serde(default)]
    pub summary: Option<String>,

    /// Hierarchical path
    #[serde(default)]
    pub path: Option<String>,

    /// 0.0–1.0 importance score
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub importance: Option<f64>,

    /// Category: "fact" | "decision" | "experience" | "preference" | "entity" | "other"
    #[serde(default)]
    pub category: Option<String>,

    /// Tags for recall and FTS (modules, crates, topics)
    #[serde(default, alias = "indexed_tags")]
    #[schemars(description = "Tags for recall/FTS, e.g. rust, mcp, refactor.")]
    pub keywords: Vec<String>,

    /// People, repos, services, tools (used for auto-link)
    #[serde(default)]
    #[schemars(description = "Named entities, e.g. sigil, memory-server, postgres.")]
    pub entities: Vec<String>,

    /// Scope: "user" | "project" | "general"
    #[serde(default)]
    pub scope: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Optional codebase area tag (does not select DB)
    #[serde(default)]
    pub domain: Option<String>,

    /// Retention policy: "ephemeral" | "durable" | "permanent" | "pinned"
    #[serde(default)]
    pub retention_policy: Option<String>,

    /// Bypass noise filter
    #[serde(default)]
    pub force: bool,

    /// External references: URLs, absolute paths, or GitHub shorthands (#N, repo#N, owner/repo#N).
    #[serde(default)]
    pub references: Vec<String>,

    /// Topic / subject area
    #[serde(default)]
    pub topic: Option<String>,

    /// Source identifier (used when kind="facts" or kind="extract_facts")
    #[serde(default)]
    pub source: Option<String>,
    /// When this memory became true/effective. Defaults to timestamp.
    #[serde(default)]
    pub valid_from: Option<String>,

    /// When this memory stopped being true/effective. None = still valid.
    #[serde(default)]
    pub valid_until: Option<String>,

    /// Arbitrary metadata payload merged before provenance injection.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,

    /// Also append a typed continuity ledger event for this memory save.
    #[serde(default)]
    pub emit_continuity: bool,

    /// Source files this memory references (stored as `metadata.files`). Surfaced
    /// inline on search results so agents can jump to the referenced file without
    /// a follow-up `get_memory`. Merged with paths auto-parsed from `spec:` pointers.
    #[serde(default)]
    #[schemars(description = "Referenced source files, e.g. docs/SPEC.md, src/lib.rs.")]
    pub files: Vec<String>,
}

// ─── Facade: unified memory / agent session UX ───────────────────────────────

fn default_memory_top_k() -> usize {
    6
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiMemoryParams {
    #[schemars(
        schema_with = "tachi_memory_action_schema",
        description = "Required. One of: search (hybrid vector+FTS+symbolic recall), get (fetch one memory by id), save (persist memory entry; prefer tachi_save for decisions), extract_facts (LLM atomize raw text into entries), briefing (session-start context), checkpoint (mid-task handoff summary), alerts (compact warnings when stuck), ask (Q&A over evidence; set synthesize=true for LLM answer), consolidate (merge related memories), recall_simulate (replay labeled query→expected-id cases and report recall@k/MRR without mutating access counters), recall_proposals (generate/list evidence-backed RecallConfig proposals), review_recall_proposal (approve/reject one recall proposal), apply_recall_proposals (persist approved TACHI_RECALL_* config.env values), progress (long-running flow status), readiness (health + tool visibility)."
    )]
    pub action: String,
    #[serde(default, alias = "output_format")]
    #[schemars(
        description = "Response shape: default JSON for agent automation; pass \"markdown\" for human-readable text."
    )]
    pub format: Option<String>,

    // --- search fields ---
    #[serde(default)]
    #[schemars(description = "[action=search|ask] Query text (required for these actions).")]
    pub query: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask] Recall scope: \"all\" (default), \"memory\", \"wiki\", or \"sft\"."
    )]
    pub scope: Option<String>,
    #[serde(default = "default_memory_top_k")]
    #[schemars(
        description = "[action=search|ask] Maximum results to return (default: 6, max: 100)."
    )]
    pub top_k: usize,
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask] Optional path prefix filter, e.g. /scratch/sigil/."
    )]
    pub path_prefix: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=search|ask] Optional open-file path hint to bias recall.")]
    pub file_context: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=search|ask] Optional error message hint to bias recall.")]
    pub error_context: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask|save|checkpoint] Wiki category filter (search/ask), or the category for the saved/checkpointed entry."
    )]
    pub category: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask] Include archived wiki/memory entries in results."
    )]
    pub include_archived: bool,
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask] Include training/distillation corpus entries such as /sft/ in normal recall. Defaults false; scope='sft' opts in."
    )]
    pub include_training: bool,
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask|recall_simulate] Enable adaptive Voyage reranking when top hybrid scores are close; recall_simulate replays the same rerank gate without cache/access mutation."
    )]
    pub enable_rerank: bool,
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask] Point-in-time validity filter (ISO 8601). Returns only memories valid at this time."
    )]
    pub as_of: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=ask|consolidate] Call the configured LLM to synthesize from evidence."
    )]
    pub synthesize: bool,
    #[serde(default)]
    #[schemars(description = "[action=ask|consolidate] Optional model override for synthesis.")]
    pub model: Option<String>,

    // --- save fields ---
    #[serde(default)]
    #[schemars(
        description = "[action=save|checkpoint|extract_facts] Full text to persist (or atomize source)."
    )]
    pub text: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=save|checkpoint] Title for the saved entry or checkpoint (also used as a title override by briefing/progress)."
    )]
    pub title: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=save|checkpoint|progress] Short summary stored alongside the saved entry, checkpoint, or progress state."
    )]
    pub summary: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=save] Topic label for the entry.")]
    pub topic: Option<String>,
    #[serde(default, alias = "indexed_tags")]
    #[schemars(description = "[action=save] Tags for recall/FTS, e.g. rust, mcp, refactor.")]
    pub keywords: Vec<String>,
    #[serde(default)]
    #[schemars(description = "[action=save] Named entities, e.g. sigil, memory-server, postgres.")]
    pub entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(description = "[action=save] Importance score 0.0–1.0 (default: 0.5).")]
    pub importance: Option<f64>,
    #[serde(default)]
    #[schemars(description = "[action=save] Retention policy name.")]
    pub retention_policy: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=save] Kind hint: memory, note, or wiki. If omitted, tachi_memory defaults to memory unless scope='note'; use tachi_save for title-based auto-detection."
    )]
    pub kind: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=save] Hierarchical path. Working notes: /scratch/<project>/..., review notes: /code-review/<project>/..., wiki: /wiki/... e.g. /scratch/sigil/schema-bug-fix"
    )]
    pub path: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=get|save] Memory id to fetch (get) or to update in place instead of creating a new row (save)."
    )]
    pub id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=save] Bypass dedup guards when true.")]
    pub force: bool,
    #[serde(default)]
    #[schemars(
        description = "[action=save] Provenance source label, e.g. cursor, codex, openclaw."
    )]
    pub source: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=save] Validity start (ISO 8601) for time-bounded facts.")]
    pub valid_from: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=save] Validity end (ISO 8601) for time-bounded facts.")]
    pub valid_until: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=save] Arbitrary JSON metadata merged into the stored entry. [action=recall_simulate|recall_proposals] Supply cases/eval_cases: [{query, expected_ids|expected_id, top_k?, project?, path_prefix?}] and optional variants: [{name, recall_config:{or_fallback_fts_score_factor?, default_fts?, ...}}]."
    )]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(
        description = "[action=save] Also append a typed continuity ledger event for this memory save."
    )]
    pub emit_continuity: bool,
    #[serde(default)]
    #[schemars(
        description = "[action=save] Referenced source files, e.g. docs/SPEC.md, src/lib.rs."
    )]
    pub files: Vec<String>,

    // --- progress / long-running command fields ---
    #[serde(default)]
    #[schemars(description = "[action=progress] Flow id (create or resume a tracked command).")]
    pub flow_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=progress] Event name, e.g. step_done, failed.")]
    pub event: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=progress] State payload or status line.")]
    pub state: Option<String>,

    // --- shared ---
    #[serde(default)]
    #[schemars(
        description = "Named project library under ~/.tachi/projects/<name>/memory.db. When set, recall/save targets ONLY that library. Omit to use global + the daemon-bound workspace project DB (shown in every response)."
    )]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional area tag (e.g. rust, ci, mcp). Filter on search; stored on save. Does not select the DB — use project for that."
    )]
    pub domain: Option<String>,

    // --- briefing shape ---
    #[serde(default)]
    #[schemars(
        description = "[action=briefing] When true, emit a tight 6-row summary: top 6 memories, top 3 wiki, top 3 kanban, top 2 checkpoints, no health snapshot. Default false (full briefing)."
    )]
    pub compact: bool,

    // --- recall config proposals ---
    #[serde(default)]
    #[schemars(
        description = "[action=review_recall_proposal|apply_recall_proposals] Recall proposal id."
    )]
    pub proposal_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=review_recall_proposal] Review status: approved or rejected."
    )]
    pub review_status: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=review_recall_proposal] Optional review note.")]
    pub notes: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=apply_recall_proposals] Required true to write approved TACHI_RECALL_* values to config.env."
    )]
    pub confirm: bool,
    #[serde(default)]
    #[schemars(description = "[action=recall_proposals] Optional proposal status filter.")]
    pub state_filter: Option<String>,
}

// ─── Facade: append-only continuity events ───────────────────────────────────

fn default_tachi_event_limit() -> usize {
    20
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiEventParams {
    #[schemars(
        schema_with = "tachi_event_action_schema",
        description = "Required. emit appends a domain-neutral continuity event; query lists recent events; metrics returns read-only continuity metrics; project materializes candidate events into stable memory projections; context returns projected continuity memory for prompt/read-model use; label_eval compares session.outcome labels to session.outcome.review gold labels."
    )]
    pub action: String,
    #[serde(default, alias = "output_format")]
    #[schemars(description = "Response shape. Defaults to JSON.")]
    pub format: Option<String>,

    #[serde(default)]
    #[schemars(description = "[action=emit] Optional event id. Defaults to a generated UUID.")]
    pub id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Source repo or product surface that produced the event, e.g. sigil, quant, romanbath."
    )]
    pub source_repo: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Adapter/client that submitted the event, e.g. memory-server, codex, openclaw."
    )]
    pub adapter: Option<String>,
    #[serde(default)]
    #[schemars(description = "Named project DB selector and event project label.")]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional domain label, e.g. trading, bonding, coding, project.")]
    pub domain: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional session/conversation/run id.")]
    pub session_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional actor label, e.g. user, agent, strategy, runtime.")]
    pub actor: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=emit|required, action=query|filter] Event type, e.g. pattern.observed, affect.sample, evidence.gate."
    )]
    pub event_type: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Authority level: collect_only, review_signal_only, interaction_routing_only, tone_and_reminder_only, advisory, raw_fact, derived_evidence, blocker, execution_gate."
    )]
    pub authority: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Effect scopes after projection: recall, prompt, routing, tone, scoring, execution, memory_write, project_cycle, domain_state."
    )]
    pub effects: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Projection hints and filters: pattern, outcome, affect, bonding, world_book, project_cycle, domain_profile, evidence_gate."
    )]
    pub projection_hints: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=emit] Domain payload. Stored as JSON; not interpreted by the kernel."
    )]
    pub payload: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(
        description = "[action=emit] Provenance/evidence JSON, e.g. files, source ids, sample_n, timestamps."
    )]
    pub provenance: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(description = "[action=emit] Event time. Defaults to now.")]
    pub created_at: Option<String>,
    #[serde(default = "default_tachi_event_limit")]
    #[schemars(
        description = "[action=query|metrics|project|context] Maximum events or projection rows to inspect/return, default 20, max 500."
    )]
    pub limit: usize,
    #[serde(default)]
    #[schemars(
        description = "[action=context] Optional projected memory path prefix. If omitted, inferred from projection_hints."
    )]
    pub path_prefix: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=project] Preview projection writes without upserting memories."
    )]
    pub dry_run: bool,
}

/// Domain-specific adapter facade for repo-derived memory shapes.
///
/// This surface keeps fork/domain conventions out of the generic save/search
/// structs while still letting Tachi own the canonical event/projection behavior.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiDomainAdapterParams {
    #[schemars(description = "Action: lorebook_import.")]
    pub action: String,

    #[serde(default)]
    #[schemars(description = "Named project DB for imported events/projections.")]
    pub project: Option<String>,

    #[serde(default)]
    #[schemars(description = "Domain label for imported events/projections.")]
    pub domain: Option<String>,

    #[serde(default)]
    #[schemars(description = "Actor/agent label for event imports.")]
    pub actor: Option<String>,

    #[serde(default)]
    pub session_id: Option<String>,

    #[serde(default)]
    #[schemars(description = "Character/card name for lorebook imports.")]
    pub character: Option<String>,

    #[serde(default)]
    #[schemars(description = "Lorebook entries, using RomanBath/SillyTavern field names.")]
    pub entries: Vec<serde_json::Value>,

    #[serde(default)]
    #[schemars(description = "Project lorebook events after import. Defaults false.")]
    pub project_events: bool,

    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiProfileDocumentParams {
    #[schemars(
        description = "Document kind: agents|claude|gemini|cursor|soul|identity|user|tools|memory_policy|tool_policy|other."
    )]
    pub kind: String,

    #[serde(default)]
    pub path: Option<String>,

    pub content: String,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiProfileDocumentPathParams {
    #[schemars(
        description = "Document kind: agents|claude|gemini|cursor|soul|identity|user|tools|memory_policy|tool_policy|other."
    )]
    pub kind: String,

    pub path: String,
}

/// Canonical user-agent alignment/profile facade.
///
/// This first slice is read-only: it imports and renders profile projections,
/// but never writes AGENTS.md / CLAUDE.md / GEMINI.md / OpenClaw files.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiProfileParams {
    #[schemars(schema_with = "tachi_profile_action_schema")]
    pub action: String,

    #[serde(default)]
    pub agent_id: Option<String>,

    #[serde(default)]
    pub display_name: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Single render target, e.g. codex_agents, claude_md, gemini_md, cursor_mdc, openclaw_agents, openclaw_soul, openclaw_identity, openclaw_user, openclaw_tools."
    )]
    pub target: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Multiple render targets. If empty, render uses target or codex_agents."
    )]
    pub targets: Vec<String>,

    #[serde(default)]
    pub documents: Vec<TachiProfileDocumentParams>,

    #[serde(default)]
    pub document_paths: Vec<TachiProfileDocumentPathParams>,

    #[serde(default)]
    #[schemars(
        description = "Existing AgentProfilePack JSON. When omitted, Tachi imports from documents/document_paths."
    )]
    pub pack: Option<serde_json::Value>,

    #[serde(default)]
    pub project: Option<String>,

    #[serde(default)]
    pub role: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Session kind: main|subagent|cron|group|external. subagent/group/external suppress private user context."
    )]
    pub session_kind: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Include user_model/private user profile in context render. Defaults false."
    )]
    pub include_private_user: bool,

    #[serde(default = "default_true")]
    #[schemars(
        description = "Include projected continuity read-model context (patterns, lorebook, affect, metrics) in action=context."
    )]
    pub include_continuity: bool,

    #[serde(default = "default_profile_max_chars")]
    pub max_chars: usize,

    #[serde(default = "default_profile_dry_run")]
    pub dry_run: bool,
}

// ─── Facade: unified handoff ─────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiHandoffParams {
    /// Action: "leave" to leave a handoff memo, "check" to check for pending memos, "promote_issue" to create a GitHub issue from a memo
    pub action: String,

    /// Summary of what was accomplished (required when action="leave")
    #[serde(default)]
    pub summary: Option<String>,

    /// Next steps for the receiving agent (used when action="leave")
    #[serde(default)]
    pub next_steps: Vec<String>,

    /// Target agent ID (used when action="leave")
    #[serde(default)]
    pub target_agent: Option<String>,

    /// Optional context (used when action="leave")
    #[serde(default)]
    pub context: Option<serde_json::Value>,

    /// Agent ID to check for (used when action="check")
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Whether to acknowledge retrieved memos (used when action="check", default: true)
    #[serde(default = "default_true")]
    pub acknowledge: bool,

    /// Handoff memo ID to promote (required for action="promote_issue", with or without "handoff:" prefix)
    #[serde(default)]
    pub memo_id: Option<String>,

    /// GitHub repo in "owner/repo" format (required for action="promote_issue")
    #[serde(default)]
    pub repo: Option<String>,

    /// Issue title override (used by action="promote_issue"; defaults to memo summary truncated to 120 chars)
    #[serde(default)]
    pub title: Option<String>,

    /// Issue labels (used by action="promote_issue"; defaults to ["handoff"])
    #[serde(default)]
    pub labels: Vec<String>,

    /// Shell flow ID for artifact linkage (optional for action="promote_issue"; writes status.json + events.jsonl)
    #[serde(default)]
    pub flow_id: Option<String>,

    /// Force re-promote even if memo already has a GitHub issue link (used by action="promote_issue")
    #[serde(default)]
    pub force: bool,
}

mod dispatch;
pub use dispatch::{
    DispatchMcpAccessParams, TachiApproveMergeParams, TachiCompleteParams, TachiDispatchParams,
    TachiSubagentEvalParams,
};

// ─── Facade: wiki (search / browse / write) ──────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiWikiParams {
    /// Action: "search", "browse", "read", or "write"
    #[schemars(schema_with = "tachi_wiki_action_schema")]
    pub action: String,
    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub top_k: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default, alias = "indexed_tags")]
    #[schemars(description = "Wiki tags for recall/FTS.")]
    pub keywords: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Related repos, tools, or people.")]
    pub entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub importance: Option<f64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Named project library under ~/.tachi/projects/<name>/memory.db. When set, wiki recall targets ONLY that library."
    )]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional area tag on the wiki entry.")]
    pub domain: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional JSON object merged into wiki metadata before provenance.")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub force: bool,

    /// External references (URLs, absolute paths, GitHub shorthands). Validated on write.
    #[serde(default)]
    pub references: Vec<String>,

    /// Recall projected continuity patterns and persist references in wiki metadata.
    #[serde(default)]
    pub include_patterns: bool,

    /// Optional pattern search/filter text. Defaults to title + summary when include_patterns=true.
    #[serde(default)]
    pub pattern_query: Option<String>,

    /// Maximum pattern references to attach.
    #[serde(default)]
    pub pattern_top_k: Option<usize>,
}

// ─── Facade: workflow closure (Issue → Doc → Memory) ─────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiWorkflowParams {
    /// close_loop | build_references
    pub action: String,
    #[serde(default)]
    pub issue_ref: Option<String>,
    /// Optional PR reference ("owner/repo#123" or URL); when set, close_loop also
    /// posts the write-back comment to the PR.
    #[serde(default)]
    pub pr_ref: Option<String>,
    #[serde(default)]
    pub doc_paths: Vec<String>,
    /// Spec/contract file paths this change touched. Used for the close_loop
    /// spec-drift advisory (specs are the source of truth and must stay current).
    #[serde(default)]
    pub spec_paths: Vec<String>,
    #[serde(default)]
    pub related_issues: Vec<String>,
    /// Whether close_loop posts the write-back comment to the issue/PR.
    /// Defaults to true (best-effort; never fails the closure if GitHub is down).
    #[serde(default)]
    pub post_comment: Option<bool>,
    /// Optional Tachi flow id. When set and wiki_title/wiki_text are omitted,
    /// close_loop drafts them from the flow's result.md (lowers the activation
    /// energy to actually close the loop).
    #[serde(default)]
    pub flow_id: Option<String>,
    #[serde(default)]
    pub wiki_title: Option<String>,
    #[serde(default)]
    pub wiki_text: Option<String>,
    #[serde(default)]
    pub wiki_path: Option<String>,
    #[serde(default)]
    pub wiki_topic: Option<String>,
    #[serde(default)]
    pub wiki_summary: Option<String>,
    #[serde(default)]
    pub wiki_category: Option<String>,
    #[serde(default)]
    pub wiki_keywords: Vec<String>,
    #[serde(default)]
    pub wiki_entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub wiki_importance: Option<f64>,
    #[serde(default)]
    pub wiki_scope: Option<String>,
    #[serde(default)]
    pub wiki_domain: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub force: bool,
}

// ─── Facade: skill (discover / run / bundle / loadout) ───────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiSkillParams {
    /// Action: "discover", "run", "bundle", "loadout", or "from_pattern"
    #[schemars(schema_with = "tachi_skill_action_schema")]
    pub action: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub cap_type: Option<String>,
    #[serde(default)]
    pub enabled_only: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub skill_id: Option<String>,
    #[serde(default)]
    pub args: Option<serde_json::Value>,
    /// DispatchProfile name for action="loadout", e.g. "claude_plan".
    #[serde(default)]
    pub profile: Option<String>,
    /// Optional host/runtime name for bundle preparation, e.g. "codex".
    #[serde(default)]
    pub host: Option<String>,
    /// Max skill recommendations in a capability bundle.
    #[serde(default)]
    pub skill_limit: Option<usize>,
    /// Max supporting capabilities in a capability bundle.
    #[serde(default)]
    pub capability_limit: Option<usize>,
    /// Max projected packs in a capability bundle.
    #[serde(default)]
    pub pack_limit: Option<usize>,
    /// Include a ready-to-inject markdown section in bundle responses.
    #[serde(default)]
    pub include_section: Option<bool>,
}

// ─── Facade: task (plan / recommend / dispatch / board / merge / lifecycle) ──

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiTaskParams {
    /// Action: "plan", "briefing", "doc_index", "recommend", "dispatch", "complete", "profiles", "profile", "card", "route_simulate", "proposals", "review_proposal", "apply_proposals", "status", "cancel", "board", "wait", "merge", "intake", "link_pr", "cycle_status", "pr_status", "pr_handoff", "release_note", "ux_matrix", "build_references", or "close_loop".
    /// action="merge" is local dispatched worktree git merge only; use
    /// tachi_gh(action='safe_merge') to execute GitHub PR merges.
    /// action="intake" reads/binds a GitHub issue to a Tachi flow and seeds flow artifacts.
    /// action="link_pr" attaches a GitHub PR to an existing flow.
    /// action="cycle_status" returns a read-only lifecycle status from linked issue/PR, docs/specs, flow artifacts, verification, and closure state.
    /// action="pr_status" previews GitHub PR safe-merge status and may persist flow status.
    /// action="pr_handoff" writes a PR body/branch handoff from flow, issue, verification, and gaps.
    /// action="release_note" synthesizes a release/changelog note from a flow or PR and writes
    /// release_note.md when flow_id is supplied.
    /// action="ux_matrix" returns a feature workflow UX checklist and writes ux_matrix.json
    /// when flow_id is supplied.
    /// action="build_references" previews the issue/doc/related reference array.
    /// action="close_loop" writes durable wiki closure through the task lifecycle.
    /// action="doc_index" exposes the layered GitHub/docs/wiki/guide/feedback/eval/runtime source index.
    /// Use "recommend" before assigning external workers so Tachi can choose a dispatch profile
    /// from the task, risk, and live eval evidence.
    /// Use "route_simulate" to replay recent /eval rows across policy variants before
    /// proposing routing changes.
    #[schemars(schema_with = "tachi_task_action_schema")]
    pub action: String,
    /// Response shape: default JSON for agent automation; pass "markdown" for human-readable text.
    #[serde(default)]
    pub format: Option<String>,
    // plan fields
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|dispatch|route_simulate|complete|intake|pr_handoff|ux_matrix] Task description / prompt text."
    )]
    pub task: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=plan|recommend|dispatch] Requesting agent id.")]
    pub agent_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|briefing] Area tag (e.g. rust, mcp) to scope context."
    )]
    pub domain: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|briefing] Optional recall path prefix filter."
    )]
    pub path_prefix: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=plan|recommend|briefing] Maximum context fragments to recall."
    )]
    pub top_k: Option<usize>,
    // feature briefing fields
    #[serde(default)]
    #[schemars(
        description = "Canonical docs to prioritize in action='briefing', e.g. docs/engineering/architecture/subagent-eval-system.md."
    )]
    pub doc_paths: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Related GitHub issues/PRs for action='close_loop' or action='build_references'."
    )]
    pub related_issues: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Canonical spec docs to prioritize in action='briefing'. Kept separate from memory/wiki fragments."
    )]
    pub spec_paths: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "When true, action='briefing' may include broader global memory fragments. Default false keeps briefing feature/project scoped."
    )]
    pub include_global: bool,
    #[serde(default)]
    pub compact: Option<bool>,
    // dispatch / complete fields
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch|recommend|complete] Agent backend, e.g. claude, codex, grok, kimi."
    )]
    pub agent: Option<String>,
    /// [action=complete] Outcome: success | failure | partial | aborted.
    #[serde(default)]
    pub outcome: Option<String>,
    /// [action=complete] Optional task id.
    #[serde(default)]
    pub task_id: Option<String>,
    /// [action=complete] Standard task type, e.g. fix_request or plan_request.
    #[serde(default)]
    pub task_type: Option<String>,
    /// [action=complete] Execution duration in milliseconds.
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub duration_ms: Option<u64>,
    /// [action=complete] Skills actually used. Distinct from dispatch prompt skills.
    #[serde(default)]
    pub skills_used: Vec<String>,
    /// [action=complete] Cost in tokens.
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    pub cost_tokens: Option<u64>,
    /// [action=complete] Cost in USD.
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub cost_usd: Option<f64>,
    /// [action=complete] Quality score 0.0-1.0.
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub quality_score: Option<f64>,
    /// [action=complete] Completion notes or summary.
    #[serde(default)]
    pub notes: Option<String>,
    /// [action=complete] Execution trajectory. Store compact step objects, not raw transcripts.
    #[serde(default)]
    pub trajectory: Option<serde_json::Value>,
    /// [action=complete] Unified diff or patch evidence.
    #[serde(default)]
    pub diff: Option<String>,
    /// [action=complete] Structured subagent eval records.
    #[serde(default)]
    pub subagents: Vec<TachiSubagentEvalParams>,
    /// [action=complete] Feedback/prompt-quality rule ids that were applied to this task.
    #[serde(default)]
    pub feedback_rules_applied: Vec<String>,
    /// [action=complete] Evidence references for verification.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// [action=complete] Verification commands run.
    #[serde(default)]
    pub tests_run: Vec<String>,
    /// [action=complete] Whether a diff was present; inferred from diff if absent.
    #[serde(default)]
    pub diff_present: Option<bool>,
    /// [action=complete] Target DB scope: global or project.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Working directory for the spawned agent.")]
    pub cwd: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Skill ids to inject into the agent prompt.")]
    pub skills: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Extra recall query; top hits are injected into the prompt."
    )]
    pub context_query: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Model override for the spawned agent.")]
    pub model: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(description = "[action=dispatch] Timeout in seconds for the spawned agent.")]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Permission profile, e.g. full, allowlist, default."
    )]
    pub permission_profile: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Tool allowlist when permission_profile=allowlist."
    )]
    pub allowed_tools: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u32_from_string_or_number"
    )]
    #[schemars(
        description = "[action=dispatch] Maximum conversation turns for the spawned agent."
    )]
    pub max_turns: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Codex sandbox: workspace-write | danger-full-access | read-only."
    )]
    pub sandbox: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Inject Tachi MCP when the backend supports it.")]
    pub inject_tachi_mcp: Option<bool>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Inject Hub MCPs when the backend supports it.")]
    pub inject_hub_mcps: Option<bool>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] Explicit command argv override for custom backends."
    )]
    pub command: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Harness transport override, e.g. opencode_serve to dispatch through an existing local OpenCode server via opencode run --attach."
    )]
    pub harness_transport: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Harness server URL for attach transports, e.g. http://127.0.0.1:4321 for OpenCode serve."
    )]
    pub harness_server_url: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Named project DB for context/dispatch. Shared across actions; omit for the daemon-bound workspace DB."
    )]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch|recommend] Workflow stage hint, e.g. plan, build, review."
    )]
    pub stage: Option<String>,
    #[serde(default, alias = "dispatch_profile")]
    #[schemars(
        description = "Dispatch profile id, e.g. claude_plan, glm_51_impl, opencode_builder, codex_55_review, codex_53_fast, kimi_arch, deepseek_explore, or kimi_ux. Distinct from the server ToolProfile."
    )]
    pub profile: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Credential profile ids to materialize before spawning the worker, e.g. codex_shared. Values resolve from .tachi/credentials/*.json."
    )]
    pub credential_profiles: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "GitHub repository in owner/repo format for action='intake', action='link_pr', or action='pr_status'. Optional when issue_ref/pr_ref is owner/repo#123 or a GitHub URL."
    )]
    pub repo: Option<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u64_from_string_or_number"
    )]
    #[schemars(
        description = "GitHub issue/PR number for action='intake', action='link_pr', or action='pr_status' when repo is supplied."
    )]
    pub number: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "[action=intake|link_pr|pr_status|pr_handoff|dispatch|complete] GitHub issue ref, e.g. owner/repo#123 or URL."
    )]
    pub issue_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=link_pr|pr_status|pr_handoff|release_note|dispatch|complete] GitHub PR ref, e.g. owner/repo#123 or URL."
    )]
    pub pr_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Tachi flow id for feature-scoped artifacts, also linking a dispatch/complete back to its flow (briefing/intake/link_pr/pr_handoff/release_note/ux_matrix/close_loop/status/wait/dispatch/complete)."
    )]
    pub flow_id: Option<String>,
    /// [action=complete] Dispatch id linked to this completion.
    #[serde(default)]
    pub dispatch_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Risk override for recommendation/routing: low | medium | high | critical."
    )]
    pub risk: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Expected child-agent tool surface, e.g. readonly, delegate, reviewer."
    )]
    pub tool_profile: Option<String>,
    #[serde(default, alias = "include_capability_bundle")]
    #[schemars(
        description = "[action=dispatch|recommend] Include a capability bundle in the dispatch prompt when supported."
    )]
    pub auto_capability_bundle: Option<bool>,
    #[serde(default)]
    #[schemars(
        description = "[action=dispatch] MCP/GitHub access contract for the spawned agent."
    )]
    pub mcp_access: Option<DispatchMcpAccessParams>,
    #[serde(default)]
    #[schemars(description = "[action=dispatch] Hub MCP server ids the dispatch may inject.")]
    pub allowed_mcp_servers: Vec<String>,
    // board fields
    #[serde(default)]
    #[schemars(description = "[action=board] Filter dispatch ledger rows by state.")]
    pub state_filter: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=board|proposals] Maximum ledger/proposal rows to return.")]
    pub limit: Option<usize>,
    #[serde(default)]
    #[schemars(
        description = "Proposal id for action='review_proposal' or action='apply_proposals'. Route-policy proposals persist rules; loadout-evolution proposals project reviewed profile/card overlays."
    )]
    pub proposal_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "Review status for action='review_proposal': approved or rejected.")]
    pub review_status: Option<String>,
    // merge fields. These apply only to local dispatch worktree merge via
    // approve_merge; GitHub PR gates/merges go through tachi_gh safe_merge.
    #[serde(default)]
    #[schemars(
        description = "Local dispatched worktree path to merge. Do not pass a GitHub PR ref here; use tachi_gh(action='safe_merge') for PR gates/merges."
    )]
    pub worktree: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=merge|pr_handoff] Branch to merge from the local dispatched worktree (merge), or the branch name to record in the handoff (pr_handoff)."
    )]
    pub branch: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Local dispatch worktree merge strategy for action='merge'. Ignored by action='pr_status', which always runs GitHub safe_merge in preview mode."
    )]
    pub strategy: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "GitHub PR gate policy for action='pr_status': permissive | standard | strict. Defaults to standard."
    )]
    pub merge_policy: Option<String>,
    #[serde(default = "default_true")]
    #[schemars(
        description = "[action=merge] Remove the local worktree after a successful merge. Defaults true."
    )]
    pub delete_worktree: bool,
    #[serde(default)]
    #[schemars(
        description = "[action=merge|dispatch] Confirm the local worktree merge (merge), or bypass the leader confirmation gate when dispatching an issue flow (dispatch)."
    )]
    pub confirm: bool,
    // close_loop fields
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure entry title.")]
    pub wiki_title: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure entry body text.")]
    pub wiki_text: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure path, e.g. /wiki/....")]
    pub wiki_path: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure topic label.")]
    pub wiki_topic: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure short summary.")]
    pub wiki_summary: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure category.")]
    pub wiki_category: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure keywords/tags.")]
    pub wiki_keywords: Vec<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure named entities.")]
    pub wiki_entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(description = "[action=close_loop] Wiki closure importance 0.0-1.0.")]
    pub wiki_importance: Option<f64>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure scope: global or project.")]
    pub wiki_scope: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=close_loop] Wiki closure area/domain tag.")]
    pub wiki_domain: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Bypass wiki noise filtering for action='close_loop'. Does not force dispatch or merge behavior."
    )]
    pub force: bool,
}

mod orchestration;
pub use orchestration::{
    TachiAgentEvalParams, TachiAgentsParams, TachiArenaParams, TachiBoardParams,
    TachiOrchestratorParams, TachiShellDispatchSliceParams, TachiShellParams, TachiVerifyParams,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_top_k_is_capped() {
        assert_eq!(clamp_facade_top_k(0), 1);
        assert_eq!(clamp_facade_top_k(6), 6);
        assert_eq!(clamp_facade_top_k(10_000), MAX_FACADE_TOP_K);
    }
}
