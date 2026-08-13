use super::string_enum_schema;
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, str::FromStr};

fn default_facade_search_scope() -> String {
    "all".to_string()
}

fn default_facade_top_k() -> usize {
    6
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
        super::action_inventory::TACHI_MEMORY_ACTIONS,
        "Required Tachi memory facade action.",
        generator,
    )
}

/// Closed action vocabulary for the model-facing Memory facade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TachiMemoryAction {
    Search,
    Get,
    Save,
    Briefing,
    Checkpoint,
    Alerts,
    Ask,
    ExtractFacts,
    Consolidate,
}

impl TachiMemoryAction {
    pub const ALL: &'static [Self] = &[
        Self::Search,
        Self::Get,
        Self::Save,
        Self::Briefing,
        Self::Checkpoint,
        Self::Alerts,
        Self::Ask,
        Self::ExtractFacts,
        Self::Consolidate,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Get => "get",
            Self::Save => "save",
            Self::Briefing => "briefing",
            Self::Checkpoint => "checkpoint",
            Self::Alerts => "alerts",
            Self::Ask => "ask",
            Self::ExtractFacts => "extract_facts",
            Self::Consolidate => "consolidate",
        }
    }
}

impl fmt::Display for TachiMemoryAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for TachiMemoryAction {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "search" => Ok(Self::Search),
            "get" => Ok(Self::Get),
            "save" => Ok(Self::Save),
            "briefing" => Ok(Self::Briefing),
            "checkpoint" => Ok(Self::Checkpoint),
            "alerts" => Ok(Self::Alerts),
            "ask" => Ok(Self::Ask),
            "extract_facts" => Ok(Self::ExtractFacts),
            "consolidate" => Ok(Self::Consolidate),
            "progress" => Err("retired tachi_memory action 'progress'; use tachi_task(action='status')".to_string()),
            "readiness" => Err("retired tachi_memory action 'readiness'; use tachi_status".to_string()),
            "delete" => Err("retired tachi_memory action 'delete'; use tachi delete plan|apply".to_string()),
            "gc" => Err("retired tachi_memory action 'gc'; use tachi gc plan|apply".to_string()),
            "doctor_scan" => Err("retired tachi_memory action 'doctor_scan'; use tachi doctor".to_string()),
            "ingest" | "ingest_source" => Err(format!("retired tachi_memory action '{value}'; use admitted adapter/operator ingest API")),
            "pattern_feedback" => Err("retired tachi_memory action 'pattern_feedback'; use internal pattern-evidence API".to_string()),
            other => Err(format!("invalid tachi_memory action '{other}'; use search, get, save, briefing, checkpoint, alerts, ask, extract_facts, or consolidate")),
        }
    }
}

fn deserialize_memory_action<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    value
        .parse::<TachiMemoryAction>()
        .map(|action| action.as_str().to_string())
        .map_err(serde::de::Error::custom)
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

    /// Optional named project DB.
    #[serde(default)]
    #[schemars(
        description = "Wiki scope: set is named-only; omitted federates bound + shared. Other scopes: set targets only that library; omitted uses bound + global."
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

    /// What to save: "wiki" for wiki entry, "note" for a quick note, "memory" for full memory
    /// entry, or "facts"/"extract_facts" for LLM-atomized canonical facts.
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
    #[serde(deserialize_with = "deserialize_memory_action")]
    #[schemars(
        schema_with = "tachi_memory_action_schema",
        description = "Required. One of: search, get, save, briefing, checkpoint, alerts, ask, extract_facts, consolidate. Work status lives on tachi_task; health lives on tachi_status; maintenance lives on the operator CLI; ingestion and pattern evidence use admitted internal APIs."
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
        description = "[action=search|ask] Enable adaptive Voyage reranking when top hybrid scores are close."
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
        description = "[action=save|checkpoint|extract_facts] Full text to persist (or atomize source)."
    )]
    pub text: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=save|checkpoint] Title for the saved entry or checkpoint.")]
    pub title: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=save|checkpoint] Short summary stored alongside the saved entry or checkpoint."
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
        description = "[action=save] Kind hint: memory, note, wiki, facts, or extract_facts. If omitted, tachi_memory defaults to memory unless scope='note'; use tachi_save for title-based auto-detection."
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
        description = "[action=save] Arbitrary JSON metadata merged into the stored entry."
    )]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    #[schemars(
        description = "[action=save] Referenced source files, e.g. docs/SPEC.md, src/lib.rs."
    )]
    pub files: Vec<String>,
    /// tachi#1288 (Fix B): `TachiSaveParams.references` was already the wire
    /// contract for `tachi_save`/`tachi_wiki_write`, but `tachi_memory`
    /// (this struct) had no field to source it from, so
    /// `save_memory_checkpoint` hardcoded an empty list when it built its
    /// inner `TachiSaveParams`. Adding this mirrors `files` above so a
    /// `checkpoint` call can attach evidence the same way a `save`/wiki
    /// write can.
    #[serde(default)]
    #[schemars(
        description = "[action=save|checkpoint] External references: URLs, absolute paths, or GitHub shorthands (#N, repo#N, owner/repo#N)."
    )]
    pub references: Vec<String>,

    // --- shared ---
    #[serde(default)]
    #[schemars(
        description = "Named project library under ~/.tachi/projects/<name>/tachi-memory.db. When set, recall/save targets ONLY that library. Omit to use global + the daemon-bound workspace project DB (shown in every response)."
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

    // --- consolidate review/apply fields ---
    #[serde(default)]
    #[schemars(description = "[action=consolidate] Proposal id for review/apply workflows.")]
    pub proposal_id: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=consolidate] Review status: approved or rejected.")]
    pub review_status: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=consolidate] Optional review note.")]
    pub notes: Option<String>,
    #[serde(default)]
    #[schemars(description = "[action=consolidate] Required true to apply reviewed proposals.")]
    pub confirm: bool,
    #[serde(default)]
    #[schemars(description = "[action=consolidate] Optional proposal status filter.")]
    pub state_filter: Option<String>,

    // --- read-only briefing scope ---
    #[serde(default)]
    #[schemars(
        description = "[action=briefing] Optional GitHub issue to scope the read-only presence collision warnings."
    )]
    pub issue_ref: Option<String>,
}
