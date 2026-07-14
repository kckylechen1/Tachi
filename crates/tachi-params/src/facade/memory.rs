use super::string_enum_schema;
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

use crate::memory::Message;

fn default_facade_search_scope() -> String {
    "all".to_string()
}

fn default_facade_top_k() -> usize {
    6
}

// ─── Ingest action defaults (mirror crates/tachi-params/src/memory/ingest.rs) ─
//
// `tachi_memory(action='ingest'|'ingest_source')` fronts the same pipeline
// handlers as the standalone tools, so the wire defaults must match byte-for-byte.
// Scope/importance defaults are applied in the handler arm (the shared `scope`
// and `importance` fields already carry their own serde defaults).

fn default_facade_ingest_type() -> String {
    "source".to_string()
}

fn default_facade_chunk_size_chars() -> usize {
    1200
}

fn default_facade_chunk_overlap_chars() -> usize {
    120
}

fn default_facade_true() -> bool {
    true
}

pub const MAX_FACADE_TOP_K: usize = 100;

pub fn clamp_facade_top_k(top_k: usize) -> usize {
    top_k.clamp(1, MAX_FACADE_TOP_K)
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
            "pattern_feedback",
            "progress",
            "readiness",
            // #757 fold: memory-admin + pipeline tools re-fronted as actions.
            "delete",
            "gc",
            "doctor_scan",
            "ingest",
            "ingest_source",
            // #1001: manual presence-claim backstop (auto hooks cover
            // briefing/intake/dispatch; these two are for harness-native work
            // that never routes through those call sites).
            "claim",
            "release",
            // #964: read-once agent-to-agent ephemeral notes.
            "sticky_leave",
            "sticky_check",
        ],
        "Required Tachi memory facade action.",
        generator,
    )
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiSearchParams {
    /// Search query text. tachi_search is the human-readable/markdown alias of
    /// tachi_memory(action=search) (which defaults to JSON); same retrieval, formatted output.
    pub query: String,

    /// Scope: "wiki" searches wiki entries, "memory" searches general memory, "patterns" searches projected pattern memory, "all" searches both memory/wiki (default), "sft" searches training/distillation corpus.
    #[serde(default = "default_facade_search_scope")]
    #[schemars(schema_with = "super::memory_scope_schema")]
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

    /// Optional agent role for sandbox filtering. When set, memory/wiki rows
    /// denied by global sandbox rules are omitted from results.
    #[serde(default)]
    pub agent_role: Option<String>,

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
    #[schemars(schema_with = "super::save_kind_schema")]
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
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "crate::coerce::opt_number_from_string_or_number_schema")]
    pub importance: Option<f64>,

    /// Category: "fact" | "decision" | "experience" | "preference" | "entity" | "other"
    #[serde(default)]
    #[schemars(schema_with = "super::memory_category_schema")]
    pub category: Option<String>,

    /// Tags for recall and FTS (modules, crates, topics)
    #[serde(default, alias = "indexed_tags")]
    #[schemars(description = "Tags for recall/FTS, e.g. rust, mcp, refactor.")]
    pub keywords: Vec<String>,

    /// People, repos, services, tools (used for auto-link)
    #[serde(default)]
    #[schemars(description = "Named entities, e.g. sigil, tachi-server, postgres.")]
    pub entities: Vec<String>,

    /// Scope: "user" | "project" | "general"
    #[serde(default)]
    pub scope: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// #1041 F2: see `crate::memory::SaveMemoryParams::project_explicit` —
    /// same signal, same wire key, threaded through `handle_tachi_save`'s
    /// internal `SaveMemoryParams`/`RememberParams` construction.
    #[serde(default, rename = "__tachi_project_explicit")]
    #[schemars(skip)]
    pub project_explicit: bool,

    /// Optional codebase area tag (does not select DB)
    #[serde(default)]
    pub domain: Option<String>,

    /// Retention policy: "ephemeral" | "durable" | "permanent" | "pinned"
    #[serde(default)]
    #[schemars(schema_with = "super::retention_policy_schema")]
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

    /// Response shape: default receipt, or "full" for the pre-change verbose payload (includes echo).
    #[serde(default, alias = "output_format")]
    pub format: Option<String>,
}

// ─── Facade: unified memory / agent session UX ───────────────────────────────

fn default_memory_top_k() -> usize {
    6
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct TachiMemoryParams {
    #[schemars(
        schema_with = "tachi_memory_action_schema",
        description = "Required. One of: search (hybrid vector+FTS+symbolic recall), get (fetch one memory by id), save (persist memory entry; prefer tachi_save for decisions), extract_facts (LLM atomize raw text into entries), briefing (session-start context), checkpoint (mid-task handoff summary), alerts (compact warnings when stuck), ask (Q&A over evidence; set synthesize=true for LLM answer), consolidate (merge related memories), recall_simulate (replay labeled query→expected-id cases and report recall@k/MRR without mutating access counters), recall_proposals (generate/list evidence-backed RecallConfig proposals), review_recall_proposal (approve/reject one recall proposal), apply_recall_proposals (persist approved TACHI_RECALL_* config.env values), pattern_feedback (record explicit hit/miss/stale/seen feedback for projected pattern memory), progress (long-running flow status), readiness (health + tool visibility), claim (register/heartbeat a manual presence claim on an issue_ref/flow_id; advisory only, never a lock), release (release a claim by claim_id or dispatch_id), delete (permanently remove a memory entry by id; folded from delete_memory), gc (run garbage collection on growing tables; folded from memory_gc), doctor_scan (read-only scan of memory.db roots; folded from tachi_doctor_scan), ingest (unified event/source ingest; folded from ingest), ingest_source (batch source ingest with chunking/enrichment; folded from ingest_source), sticky_leave (leave a read-once ephemeral note for the leader or a named seat), sticky_check (claim/list unread stickies addressed to the caller; include_read=true shows the read/expired archive)."
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
        schema_with = "super::tachi_memory_scope_schema",
        description = "[action=search|ask] Recall scope: \"all\" (default), \"memory\", \"wiki\", \"patterns\", or \"sft\". [action=save] \"note\" routes to the note writer; \"user\", \"project\", \"general\", and \"global\" select storage/routing scope."
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
        schema_with = "super::memory_category_schema",
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
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask] Optional agent role for sandbox filtering. Denied memory/wiki rows are omitted."
    )]
    pub agent_role: Option<String>,

    // --- save fields ---
    #[serde(default)]
    #[schemars(
        description = "[action=save|checkpoint|extract_facts|sticky_leave] Full text to persist (or atomize source; sticky note body)."
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
    #[schemars(description = "[action=save] Named entities, e.g. sigil, tachi-server, postgres.")]
    pub entities: Vec<String>,
    #[serde(
        default,
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(
        schema_with = "crate::coerce::opt_number_from_string_or_number_schema",
        description = "[action=save] Importance score 0.0–1.0 (default: 0.5)."
    )]
    pub importance: Option<f64>,
    #[serde(default)]
    #[schemars(
        schema_with = "super::retention_policy_schema",
        description = "[action=save] Retention policy name."
    )]
    pub retention_policy: Option<String>,
    #[serde(default)]
    #[schemars(
        schema_with = "super::save_kind_schema",
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
    #[schemars(
        description = "[action=progress] Event name, e.g. step_done, failed. [action=pattern_feedback] Outcome: hit, miss, stale, or seen."
    )]
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
    /// #1041 F2: see `crate::memory::SaveMemoryParams::project_explicit` —
    /// same signal, same wire key, threaded through `handle_tachi_memory`'s
    /// `action=save` internal `TachiSaveParams` construction.
    #[serde(default, rename = "__tachi_project_explicit")]
    #[schemars(skip)]
    pub project_explicit: bool,
    #[serde(default)]
    #[schemars(
        description = "Optional area tag (e.g. rust, ci, mcp). Filter on search; stored on save. Does not select the DB — use project for that."
    )]
    pub domain: Option<String>,

    // --- briefing shape ---
    // #527: agent-facing default is compact; full briefing is opt-in via compact=false.
    #[serde(default = "default_facade_true")]
    #[schemars(
        description = "[action=briefing] When true (default), emit a tight summary: top 6 memories, top 3 wiki, top 3 kanban, top 2 checkpoints, no doctrine/limits metadata. Set false for the full briefing board."
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

    // --- ingest / ingest_source fields (#757 fold from standalone pipeline tools) ---
    //
    // All additive + #[serde(default)] so existing callers are byte-compatible.
    // Defaults mirror crates/tachi-params/src/memory/ingest.rs exactly so the
    // folded actions produce handler-identical results.
    #[serde(default)]
    #[schemars(
        description = "[action=ingest|ingest_source] Raw source content or structured event payload. For ingest_source a JSON string is unwrapped to text."
    )]
    pub content: Option<serde_json::Value>,
    #[serde(default = "default_facade_ingest_type")]
    #[schemars(description = "[action=ingest] Ingest mode: \"event\" or \"source\" (default).")]
    pub ingest_type: String,
    #[serde(default)]
    #[schemars(
        description = "[action=ingest|ingest_source] Optional source URL or canonical reference."
    )]
    pub source_url: Option<String>,
    #[serde(default = "default_facade_true")]
    #[schemars(
        description = "[action=ingest|ingest_source] Whether to chunk long content before storage."
    )]
    pub auto_chunk: bool,
    #[serde(default = "default_facade_true")]
    #[schemars(
        description = "[action=ingest|ingest_source] Whether to generate summaries for stored chunks."
    )]
    pub auto_summarize: bool,
    #[serde(default = "default_facade_true")]
    #[schemars(
        description = "[action=ingest|ingest_source] Whether to build graph edges against similar memories."
    )]
    pub auto_link: bool,
    #[serde(default = "default_facade_chunk_size_chars")]
    #[schemars(description = "[action=ingest|ingest_source] Chunk size in characters.")]
    pub chunk_size_chars: usize,
    #[serde(default = "default_facade_chunk_overlap_chars")]
    #[schemars(
        description = "[action=ingest|ingest_source] Overlap between adjacent chunks in characters."
    )]
    pub chunk_overlap_chars: usize,
    #[serde(default)]
    #[schemars(description = "[action=ingest] Conversation identifier for event ingestion.")]
    pub conversation_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=ingest] Turn identifier for event ingestion.")]
    pub turn_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=ingest] Event type label for event ingestion.")]
    pub event_type: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=ingest] Messages in the conversation turn.")]
    pub messages: Vec<Message>,

    // --- presence claim fields (#1001) ---
    #[serde(default)]
    #[schemars(
        description = "[action=claim] GitHub issue this session is working (e.g. org/repo#123). At least one of issue_ref/flow_id is required."
    )]
    pub issue_ref: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=claim] Branch/worktree name this claim is associated with (advisory)."
    )]
    pub branch: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=claim] File paths this session declares it is touching; used for advisory file-scope-overlap collision warnings against other live claims."
    )]
    pub declared_file_scope: Vec<String>,
    #[serde(default)]
    #[schemars(description = "[action=release] Claim id to release.")]
    pub claim_id: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=release] Release the active claim for this dispatch id instead of a claim_id."
    )]
    pub dispatch_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=release] Optional human-readable release reason.")]
    pub release_reason: Option<String>,

    // --- sticky fields (#964) ---
    #[serde(default)]
    #[schemars(
        description = "[action=sticky_leave] Seat/agent name this sticky is addressed to. Omit for a leader-only broadcast (worker seats never see unaddressed stickies)."
    )]
    pub to: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=sticky_leave] Days until an unread sticky auto-archives (1-30, default 7; clamped to this range)."
    )]
    pub ttl_days: Option<u32>,
    #[serde(default)]
    #[schemars(
        description = "[action=sticky_check] When true, show the read/expired archive instead of claiming unread stickies (read-only, does not consume anything)."
    )]
    pub include_read: bool,
    #[serde(default)]
    #[schemars(
        description = "[action=sticky_leave|sticky_check|briefing] Caller's seat/agent name for addressing. Omit to be treated as the leader/main session; for sticky_leave, omit to fall back to server-resolved identity."
    )]
    pub agent_id: Option<String>,
}
