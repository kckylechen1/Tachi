use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_top_k() -> usize {
    6
}

fn default_scope() -> String {
    "project".to_string()
}

fn default_true() -> bool {
    true
}

fn default_recall_candidate_multiplier() -> usize {
    3
}

fn default_capture_min_chars() -> usize {
    24
}

fn default_recommend_limit() -> usize {
    5
}

fn default_compact_trigger() -> String {
    "token_pressure".to_string()
}

fn default_compact_target_tokens() -> usize {
    256
}

fn default_compact_max_output_tokens() -> usize {
    700
}

fn default_section_layer() -> String {
    "session".to_string()
}

fn default_section_kind() -> String {
    "context".to_string()
}

fn default_section_cache_boundary() -> String {
    "session".to_string()
}

fn default_compact_session_scope() -> String {
    "project".to_string()
}

fn default_compact_session_importance() -> f64 {
    0.6
}

fn default_true_wiki() -> bool {
    true
}

fn default_wiki_top_k() -> usize {
    3
}

fn default_wiki_project_name() -> String {
    "wiki".to_string()
}

// ─── Recall ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct RecallContextParams {
    /// User or agent query that should be used to recall prior context
    pub query: String,

    /// Number of final results to keep after filtering / reranking
    #[serde(default = "default_top_k")]
    pub top_k: usize,

    /// Candidate expansion multiplier before reranking (default: 3)
    #[serde(default = "default_recall_candidate_multiplier")]
    pub candidate_multiplier: usize,

    /// Optional path prefix filter
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional agent id used to auto-scope path_prefix when not explicitly provided
    #[serde(default)]
    pub agent_id: Option<String>,

    /// Topic names to exclude from the final context block
    #[serde(default)]
    pub exclude_topics: Vec<String>,

    /// Optional minimum score threshold after ranking
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    #[schemars(schema_with = "super::coerce::opt_number_from_string_or_number_schema")]
    pub min_score: Option<f64>,

    /// Optional agent role for sandbox filtering
    #[serde(default)]
    pub agent_role: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// When true, automatically search the wiki knowledge base and include
    /// relevant wiki entries in the prepend_context block (default: true)
    #[serde(default = "default_true_wiki")]
    pub include_wiki: bool,

    /// Maximum wiki entries to include when include_wiki is true (default: 3)
    #[serde(default = "default_wiki_top_k")]
    pub wiki_top_k: usize,

    /// Named project DB for wiki memories (default: "wiki")
    #[serde(default = "default_wiki_project_name")]
    pub wiki_project: String,
}

// ─── Capture ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct CaptureSessionParams {
    /// Conversation identifier
    pub conversation_id: String,

    /// Turn identifier
    pub turn_id: String,

    /// Canonical agent id for pathing / provenance
    pub agent_id: String,

    /// Messages in the session window
    pub messages: Vec<super::Message>,

    /// Optional base path for written memories
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Scope: "user" | "project" | "general"
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// #1114: see `SaveMemoryParams::project_explicit` — same signal, same
    /// wire key. `session_identity::enforce_session_project` stamps this on
    /// every bound-session tool call generically, so `capture_session`'s
    /// write-affinity scrutiny can distinguish a caller-supplied `project=`
    /// from a transport-injected session-binding default the same way the
    /// S1 gate already does for `save_memory`.
    #[serde(default, rename = "__tachi_project_explicit")]
    #[schemars(skip)]
    pub project_explicit: bool,

    /// Minimum combined character count before capture runs
    #[serde(default = "default_capture_min_chars")]
    pub min_chars: usize,

    /// Force capture even when the window is short
    #[serde(default)]
    pub force: bool,
}

// ─── Compact ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct CompactContextParams {
    /// Canonical agent id for pathing / provenance
    pub agent_id: String,

    /// Conversation identifier
    pub conversation_id: String,

    /// Idempotency key for the compacted window
    pub window_id: String,

    /// Runtime trigger reason
    #[serde(default = "default_compact_trigger")]
    pub trigger: String,

    /// Messages in the soon-to-be-evicted window
    pub messages: Vec<super::Message>,

    /// Optional prior compact summary for iterative rollups
    #[serde(default)]
    pub current_summary: Option<String>,

    /// Optional base path for future persistence / provenance
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Approximate target token budget for the compacted block
    #[serde(default = "default_compact_target_tokens")]
    pub target_tokens: usize,

    /// Max output tokens for the model call
    #[serde(default = "default_compact_max_output_tokens")]
    pub max_output_tokens: usize,

    /// Whether Tachi should later persist durable facts from this window
    #[serde(default)]
    pub persist: bool,
}

#[derive(Debug, Clone, JsonSchema)]
pub struct CompactArtifactItemParams {
    /// Stable id for the compacted artifact, if already assigned
    #[serde(default)]
    pub item_id: Option<String>,

    /// Optional window id or source ref
    #[serde(default)]
    pub window_id: Option<String>,

    /// Compact text block content
    pub compacted_text: String,

    /// Salient topics already attached to the artifact
    #[serde(default)]
    pub salient_topics: Vec<String>,

    /// Durable signals already extracted from the artifact
    #[serde(default)]
    pub durable_signals: Vec<String>,
}

impl<'de> Deserialize<'de> for CompactArtifactItemParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum CompactArtifactItemInput {
            Object {
                #[serde(default)]
                item_id: Option<String>,
                #[serde(default)]
                window_id: Option<String>,
                compacted_text: String,
                #[serde(default)]
                salient_topics: Vec<String>,
                #[serde(default)]
                durable_signals: Vec<String>,
            },
            Text(String),
        }

        match CompactArtifactItemInput::deserialize(deserializer)? {
            CompactArtifactItemInput::Object {
                item_id,
                window_id,
                compacted_text,
                salient_topics,
                durable_signals,
            } => Ok(CompactArtifactItemParams {
                item_id,
                window_id,
                compacted_text,
                salient_topics,
                durable_signals,
            }),
            CompactArtifactItemInput::Text(compacted_text) => Ok(CompactArtifactItemParams {
                item_id: None,
                window_id: None,
                compacted_text,
                salient_topics: Vec::new(),
                durable_signals: Vec::new(),
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CompactRollupParams {
    /// Canonical agent id for provenance
    pub agent_id: String,

    /// Conversation identifier
    pub conversation_id: String,

    /// Rollup id / idempotency key
    pub rollup_id: String,

    /// Existing compact artifacts that should be rolled up
    #[serde(default)]
    pub items: Vec<CompactArtifactItemParams>,

    /// Optional prior rollup summary to fold in
    #[serde(default)]
    pub current_summary: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Optional path prefix for future persistence / provenance
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Target token budget for the rolled-up block
    #[serde(default = "default_compact_target_tokens")]
    pub target_tokens: usize,

    /// Max output tokens for the model call
    #[serde(default = "default_compact_max_output_tokens")]
    pub max_output_tokens: usize,

    /// If true, include a rendered section artifact in the response
    #[serde(default = "default_true")]
    pub build_section: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CompactSessionMemoryParams {
    /// Canonical agent id for pathing / provenance
    pub agent_id: String,

    /// Conversation identifier
    pub conversation_id: String,

    /// Window id or rollup id for idempotency
    pub window_id: String,

    /// Compact text block to preserve as a durable session artifact
    #[serde(default)]
    pub compacted_text: String,

    /// Salient topics derived from compaction
    #[serde(default)]
    pub salient_topics: Vec<String>,

    /// Durable signals derived from compaction
    #[serde(default)]
    pub durable_signals: Vec<String>,

    /// Optional base path for persisted artifacts
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional named project DB
    #[serde(default)]
    pub project: Option<String>,

    /// Scope for persisted memories
    #[serde(default = "default_compact_session_scope")]
    pub scope: String,

    /// Default importance for persisted compact artifacts
    #[serde(default = "default_compact_session_importance")]
    pub importance: f64,

    /// If true, queue Foundry maintenance jobs after persistence
    #[serde(default = "default_true")]
    pub queue_maintenance: bool,
}

// ─── Section Build ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SectionBuildParams {
    /// Logical section layer: static | session | live | other
    #[serde(default = "default_section_layer")]
    pub layer: String,

    /// Section kind: memory_recall | compact_rollup | capability_bundle | profile | other
    #[serde(default = "default_section_kind")]
    pub kind: String,

    /// Optional human-readable section title
    #[serde(default)]
    pub title: Option<String>,

    /// Optional primary body content
    #[serde(default)]
    pub content: Option<String>,

    /// Optional bullet items to append below the body
    #[serde(default)]
    pub items: Vec<String>,

    /// Optional cache boundary marker for host runtimes
    #[serde(default = "default_section_cache_boundary")]
    pub cache_boundary: String,

    /// Optional source refs or ids associated with the section
    #[serde(default)]
    pub source_refs: Vec<String>,

    /// Optional maximum token budget for the rendered block
    #[serde(default)]
    pub target_tokens: Option<usize>,
}

// ─── Recommend / Bundle ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RecommendCapabilityParams {
    /// Natural language task or intent query
    pub query: String,

    /// Optional host/runtime name (e.g. openclaw, codex, trae)
    #[serde(default)]
    pub host: Option<String>,

    /// Optional capability type filter: skill | plugin | mcp
    #[serde(default)]
    pub cap_type: Option<String>,

    /// Max recommendations to return
    #[serde(default = "default_recommend_limit")]
    pub limit: usize,

    /// Include hidden capabilities in ranking
    #[serde(default)]
    pub include_hidden: bool,

    /// Include currently uncallable capabilities in ranking
    #[serde(default)]
    pub include_uncallable: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RecommendSkillParams {
    /// Natural language task or intent query
    pub query: String,

    /// Optional host/runtime name (e.g. openclaw, codex, trae)
    #[serde(default)]
    pub host: Option<String>,

    /// Max skill recommendations to return
    #[serde(default = "default_recommend_limit")]
    pub limit: usize,

    /// Include currently uncallable skills in ranking
    #[serde(default)]
    pub include_uncallable: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RecommendToolchainParams {
    /// Natural language task or intent query
    pub query: String,

    /// Optional host/runtime name (e.g. openclaw, codex, trae)
    #[serde(default)]
    pub host: Option<String>,

    /// Max skill recommendations to include
    #[serde(default = "default_recommend_limit")]
    pub skill_limit: usize,

    /// Max supporting capability recommendations to include
    #[serde(default = "default_recommend_limit")]
    pub capability_limit: usize,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PrepareCapabilityBundleParams {
    /// Natural language task or intent query
    pub query: String,

    /// Optional host/runtime name (e.g. openclaw, codex, trae)
    #[serde(default)]
    pub host: Option<String>,

    /// Max skill recommendations to consider
    #[serde(default = "default_recommend_limit")]
    pub skill_limit: usize,

    /// Max supporting capability recommendations to consider
    #[serde(default = "default_recommend_limit")]
    pub capability_limit: usize,

    /// If true, include a rendered section artifact in the response
    #[serde(default = "default_true")]
    pub include_section: bool,
}
