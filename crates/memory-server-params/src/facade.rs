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

mod task;
pub use task::TachiTaskParams;

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
