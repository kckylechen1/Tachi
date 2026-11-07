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

pub(super) fn default_facade_true() -> bool {
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
            // #1426: recall tuning moved to `tachi_tune`; redirect here so the
            // serde path (the only real caller) surfaces the owner guidance.
            "recall_simulate" => Err("Invalid tachi_memory action 'recall_simulate'. Recall tuning left Memory in #1426; use tachi_tune(action='recall_simulate').".to_string()),
            "recall_proposals" => Err("Invalid tachi_memory action 'recall_proposals'. Recall tuning left Memory in #1426; use tachi_tune(action='recall_proposals').".to_string()),
            "review_recall_proposal" => Err("Invalid tachi_memory action 'review_recall_proposal'. Recall tuning left Memory in #1426; use tachi_tune(action='recall_review').".to_string()),
            "apply_recall_proposals" => Err("Invalid tachi_memory action 'apply_recall_proposals'. Recall tuning left Memory in #1426; use tachi_tune(action='recall_apply').".to_string()),
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
    #[serde(
        default = "default_facade_search_scope",
        rename = "content_scope",
        alias = "scope"
    )]
    #[schemars(schema_with = "super::memory_scope_schema")]
    pub scope: String,

    /// Number of results to return (default: 6)
    #[serde(default = "default_facade_top_k")]
    pub top_k: usize,

    /// Optional path prefix filter
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional conversation directory boundary, independent of path_prefix.
    #[serde(default)]
    pub conversation_scope: Option<String>,
    /// Prefer the dominant conversation among ranked hits (default true).
    #[serde(default = "default_evidence_pack_enabled")]
    pub auto_conversation_scope: bool,
    /// Select complementary evidence before applying the response budget.
    #[serde(default = "default_evidence_pack_enabled")]
    pub multi_evidence_pack: bool,

    /// Optional named project DB.
    #[serde(default)]
    #[schemars(
        description = "Explicit project searches only that library. Omit to search the current workspace first, then global/shared Wiki and configured fallback projects only when no visible result is found."
    )]
    pub project: Option<String>,

    /// Set by the session boundary; distinguishes an explicit target from an injected default.
    #[serde(default = "default_facade_true", rename = "__tachi_project_explicit")]
    #[schemars(skip)]
    pub project_explicit: bool,

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

    /// Include superseded (historical) memories (tachi#2126). Default false.
    #[serde(default)]
    pub include_superseded: bool,
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

    /// Storage destination: "project" (default) or "global". Legacy scope remains accepted.
    #[serde(default, rename = "storage_scope", alias = "scope")]
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

    /// Prior memory ids this save supersedes/invalidates (tachi#2126).
    #[serde(default)]
    pub supersedes: Vec<String>,

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
        description = "Required memory action. Work status: tachi_task. Warnings: alerts. Maintenance: operator CLI. Ingestion and pattern evidence: admitted internal APIs."
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
        description = "Legacy scope. Prefer content_scope for search/ask and storage_scope for save/checkpoint/extract_facts. [action=search|ask] all, memory, wiki, patterns, sft. [action=save] note, user, project, general, global remain accepted for compatibility."
    )]
    pub scope: Option<String>,
    /// Content to retrieve; independent of the library selected by project/session binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "super::memory_scope_schema")]
    pub content_scope: Option<String>,
    /// Storage destination for save/checkpoint/extract_facts: project or global; scope is the legacy alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_scope: Option<String>,
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

    /// Optional conversation directory boundary, independent of path_prefix.
    #[serde(default)]
    pub conversation_scope: Option<String>,
    /// Prefer the dominant conversation among ranked hits (default true).
    #[serde(default = "default_evidence_pack_enabled")]
    pub auto_conversation_scope: bool,
    /// Select complementary evidence before applying the response budget.
    #[serde(default = "default_evidence_pack_enabled")]
    pub multi_evidence_pack: bool,

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
        description = "[action=search|ask|get|briefing] Include archived memory/wiki items."
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
        description = "[action=search|ask|get] Point-in-time validity filter (ISO 8601). Search/ask return only memories valid at this time; get returns a miss with reason=outside_validity_window when the row is outside its window (tachi#2125)."
    )]
    pub as_of: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "[action=search|ask] Include superseded (historical) memories (tachi#2126). Default false; get(id) still returns history."
    )]
    pub include_superseded: bool,
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
        description = "[action=save] Kind hint: memory, note, wiki, facts, or extract_facts. If omitted, tachi_memory defaults to memory unless scope='note'; title and kind remain handled by tachi_memory."
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
        description = "[action=save] Prior memory ids this save supersedes/invalidates (tachi#2126)."
    )]
    pub supersedes: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "[save] Stored metadata. [briefing] knowledge_context: {context_epoch, unit?: body|summary, retained?: previous manifest}. Reset epoch after compaction; acknowledge only retained content."
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
        description = "Explicit project targets only that library. Omit for workspace-first recall, then global/shared Wiki and configured fallback projects only on no visible matches. Writes use the bound workspace unless storage_scope is global."
    )]
    pub project: Option<String>,
    /// #1041 F2: see `crate::memory::SaveMemoryParams::project_explicit` —
    /// same wire key, threaded through search and save. The session boundary
    /// overwrites caller-supplied values before routing.
    #[serde(default = "default_facade_true", rename = "__tachi_project_explicit")]
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
        description = "[briefing] Compact by default: 6 memories, 3 wiki, 3 kanban, 2 checkpoints. False restores the full board. knowledge_context has its own content budget."
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

fn default_evidence_pack_enabled() -> bool {
    true
}

impl TachiMemoryParams {
    /// Preserve legacy scope while keeping content selection separate from writes.
    pub fn normalize_scopes(&mut self) -> Result<(), String> {
        for scope in [&mut self.content_scope, &mut self.storage_scope]
            .into_iter()
            .flatten()
        {
            scope.make_ascii_lowercase();
        }
        let action = self.action.to_ascii_lowercase();
        let selected = match action.as_str() {
            "search" | "ask" => {
                if self.storage_scope.is_some() {
                    return Err("storage_scope is only valid for save/checkpoint/extract_facts; use content_scope for search/ask".to_string());
                }
                if let Some(scope) = &self.content_scope {
                    if !matches!(
                        scope.as_str(),
                        "all" | "memory" | "wiki" | "patterns" | "sft"
                    ) {
                        return Err(format!("Invalid content_scope '{scope}': use all, memory, wiki, patterns, or sft"));
                    }
                }
                self.content_scope.as_ref()
            }
            "save" | "checkpoint" | "extract_facts" => {
                if self.content_scope.is_some() {
                    return Err("content_scope is only valid for search/ask; use storage_scope for save/checkpoint/extract_facts".to_string());
                }
                if let Some(scope) = &self.storage_scope {
                    if !matches!(scope.as_str(), "project" | "global") {
                        return Err(format!("Invalid storage_scope '{scope}': use project or global; choose note with kind='note'"));
                    }
                }
                self.storage_scope.as_ref()
            }
            _ => {
                if self.content_scope.is_some() || self.storage_scope.is_some() {
                    return Err("content_scope applies to search/ask; storage_scope applies to save/checkpoint/extract_facts".to_string());
                }
                None
            }
        };
        if let Some(selected) = selected {
            if self
                .scope
                .as_ref()
                .is_some_and(|legacy| !legacy.eq_ignore_ascii_case(selected))
            {
                return Err(
                    "scope conflicts with content_scope/storage_scope; supply one scope value"
                        .to_string(),
                );
            }
            self.scope = Some(selected.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod scope_compatibility_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn search_and_save_accept_canonical_and_legacy_scope_keys() {
        for key in ["content_scope", "scope"] {
            let params: TachiSearchParams =
                serde_json::from_value(json!({"query": "needle", (key): "memory"})).unwrap();
            assert_eq!(params.scope, "memory");
            let wire = serde_json::to_value(params).unwrap();
            assert_eq!(wire["content_scope"], "memory");
            assert!(wire.get("scope").is_none());
        }
        for key in ["storage_scope", "scope"] {
            let params: TachiSaveParams =
                serde_json::from_value(json!({"text": "saved observation", (key): "global"}))
                    .unwrap();
            assert_eq!(params.scope.as_deref(), Some("global"));
        }
        assert!(serde_json::from_value::<TachiSearchParams>(json!({
            "query": "needle", "content_scope": "memory", "scope": "wiki"
        }))
        .is_err());
    }

    #[test]
    fn unified_memory_keeps_content_and_storage_scopes_independent() {
        let mut search: TachiMemoryParams = serde_json::from_value(json!({
            "action": "search", "query": "needle", "content_scope": "wiki"
        }))
        .unwrap();
        search.normalize_scopes().unwrap();
        assert_eq!(search.scope.as_deref(), Some("wiki"));
        let mut save: TachiMemoryParams = serde_json::from_value(json!({
            "action": "save", "text": "observation", "storage_scope": "global"
        }))
        .unwrap();
        save.normalize_scopes().unwrap();
        assert_eq!(save.scope.as_deref(), Some("global"));
        for wire in [
            json!({"action": "search", "storage_scope": "global"}),
            json!({"action": "save", "content_scope": "memory"}),
            json!({"action": "extract_facts", "content_scope": "memory"}),
            json!({"action": "search", "content_scope": "wiki", "scope": "memory"}),
            json!({"action": "save", "storage_scope": "all"}),
        ] {
            let mut params: TachiMemoryParams = serde_json::from_value(wire).unwrap();
            assert!(params.normalize_scopes().is_err());
        }
    }

    #[test]
    fn unified_memory_forwarding_omits_absent_canonical_scopes() {
        for action in [
            "search",
            "ask",
            "get",
            "save",
            "checkpoint",
            "extract_facts",
        ] {
            for input in [
                json!({"action": action}),
                json!({"action": action, "content_scope": null, "storage_scope": null}),
            ] {
                let mut params: TachiMemoryParams = serde_json::from_value(input).unwrap();
                params.normalize_scopes().unwrap();
                let wire = serde_json::to_value(params).unwrap();
                assert!(wire.get("content_scope").is_none(), "{wire}");
                assert!(wire.get("storage_scope").is_none(), "{wire}");
            }
        }
    }

    #[test]
    fn unified_memory_forwarding_normalizes_canonical_and_legacy_scope_case() {
        for (action, canonical_key, canonical, legacy, expected) in [
            ("search", "content_scope", "WiKi", "WIKI", "wiki"),
            ("ask", "content_scope", "MEMORY", "memory", "memory"),
            ("save", "storage_scope", "GLOBAL", "Global", "global"),
            (
                "checkpoint",
                "storage_scope",
                "Project",
                "PROJECT",
                "project",
            ),
            (
                "extract_facts",
                "storage_scope",
                "GLOBAL",
                "global",
                "global",
            ),
        ] {
            for legacy_scope in [None, Some(legacy)] {
                let mut input = json!({"action": action, (canonical_key): canonical});
                if let Some(legacy_scope) = legacy_scope {
                    input["scope"] = json!(legacy_scope);
                }
                let mut params: TachiMemoryParams = serde_json::from_value(input).unwrap();
                params.normalize_scopes().unwrap();
                let wire = serde_json::to_value(params).unwrap();
                assert_eq!(wire[canonical_key], expected);
                assert_eq!(wire["scope"], expected);
                let other_key = if canonical_key == "content_scope" {
                    "storage_scope"
                } else {
                    "content_scope"
                };
                assert!(wire.get(other_key).is_none(), "{wire}");

                let mut forwarded: TachiMemoryParams =
                    serde_json::from_value(wire.clone()).unwrap();
                forwarded.normalize_scopes().unwrap();
                assert_eq!(serde_json::to_value(forwarded).unwrap(), wire);
            }
        }
        for input in [
            json!({"action": "search", "content_scope": "WIKI", "scope": "MEMORY"}),
            json!({"action": "save", "storage_scope": "GLOBAL", "scope": "PROJECT"}),
        ] {
            let mut params: TachiMemoryParams = serde_json::from_value(input).unwrap();
            assert!(params.normalize_scopes().unwrap_err().contains("conflicts"));
        }
    }
}
