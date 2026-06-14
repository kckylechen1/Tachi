use super::*;

fn default_path() -> String {
    "/".to_string()
}

fn default_importance() -> f64 {
    0.7
}

fn default_category() -> String {
    "fact".to_string()
}

fn default_scope() -> String {
    "project".to_string()
}

fn default_auto_link() -> bool {
    true
}

fn default_top_k() -> usize {
    6
}

fn default_candidates() -> usize {
    20
}

pub(crate) const MAX_SEARCH_TOP_K: usize = 100;
pub(crate) const MAX_SEARCH_CANDIDATES_PER_CHANNEL: usize = 500;

fn default_mmr_threshold() -> Option<f64> {
    Some(0.85)
}

fn default_graph_hops() -> u32 {
    1
}

fn default_find_similar_top_k() -> usize {
    5
}

fn default_limit() -> usize {
    100
}

fn default_extraction_source() -> String {
    "extraction".to_string()
}

fn default_edge_weight() -> f64 {
    1.0
}

fn default_edge_direction() -> String {
    "both".to_string()
}

fn default_weight_semantic() -> f64 {
    0.40
}

fn default_weight_fts() -> f64 {
    0.30
}

fn default_weight_symbolic() -> f64 {
    0.20
}

fn default_weight_decay() -> f64 {
    0.10
}

fn default_ingest_type() -> String {
    "source".to_string()
}

fn default_auto_chunk() -> bool {
    true
}

fn default_true() -> bool {
    true
}

fn default_chunk_size_chars() -> usize {
    1200
}

fn default_chunk_overlap_chars() -> usize {
    120
}

fn default_sync_limit() -> usize {
    100
}

fn default_wiki_path_prefix() -> String {
    "/wiki".to_string()
}

fn default_wiki_path_prefix_opt() -> Option<String> {
    Some(default_wiki_path_prefix())
}

fn default_wiki_stale_days() -> u32 {
    90
}

fn default_include_skill_quality() -> bool {
    false
}

fn default_wiki_write_importance() -> f64 {
    0.85
}

fn default_wiki_write_category() -> String {
    "experience".to_string()
}

fn default_wiki_write_scope() -> String {
    "global".to_string()
}

fn default_wiki_retention_policy() -> String {
    "permanent".to_string()
}

fn default_missing_edge_threshold() -> f64 {
    0.85
}

fn default_contradiction_threshold() -> f64 {
    0.85
}

// ─── Save / Update ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub(crate) struct SaveMemoryParams {
    /// Full text content of the memory
    pub text: String,

    /// Short summary (≤100 chars)
    #[serde(default)]
    pub summary: String,

    /// Hierarchical path, e.g. "/openclaw/agent-main"
    #[serde(default = "default_path")]
    pub path: String,

    /// 0.0–1.0 importance score
    #[serde(default = "default_importance")]
    pub importance: f64,

    /// Category: "fact" | "decision" | "experience" | "preference" | "entity" | "other"
    #[serde(default = "default_category")]
    pub category: String,

    /// Topic / subject area
    #[serde(default)]
    pub topic: String,

    /// Tags for recall/FTS (wire alias: `indexed_tags`)
    #[serde(default, alias = "indexed_tags")]
    pub keywords: Vec<String>,

    /// Legacy OpenClaw wire field. Accepted for old clients; new writes fold it into `entities`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persons: Vec<String>,

    /// Entity names mentioned
    #[serde(default)]
    pub entities: Vec<String>,

    /// Legacy OpenClaw wire field. Accepted for old clients; new writes preserve it in metadata.
    #[serde(default)]
    pub location: String,

    /// Scope: "user" | "project" | "general"
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Optional embedding vector (if provided, skip embedding generation)
    #[serde(default)]
    pub vector: Option<Vec<f32>>,

    /// Optional entry ID (for updates)
    #[serde(default)]
    pub id: Option<String>,

    /// Bypass noise filter and force-save text
    #[serde(default)]
    pub force: bool,

    /// Auto-link: create graph edges to memories sharing the same entities (default: true)
    #[serde(default = "default_auto_link")]
    pub auto_link: bool,

    /// Optional project name to target a specific project DB (e.g. "hapi", "sigil").
    /// If omitted, uses the default project DB configured at startup.
    #[serde(default)]
    pub project: Option<String>,

    /// Retention policy: "ephemeral" | "durable" | "permanent" | "pinned".
    /// NULL/omitted = durable (default).
    #[serde(default)]
    pub retention_policy: Option<String>,

    /// Domain this memory belongs to (e.g. "code-review", "sigil").
    /// Legacy wire alias: `domain_key`.
    #[serde(default, alias = "domain_key")]
    pub domain: Option<String>,

    /// Optional timestamp override.
    #[serde(default)]
    pub timestamp: Option<String>,

    /// When this memory became true/effective. Defaults to timestamp.
    #[serde(default)]
    pub valid_from: Option<String>,

    /// When this memory stopped being true/effective. None = still valid.
    #[serde(default)]
    pub valid_until: Option<String>,

    /// Arbitrary metadata payload merged before provenance injection.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

// ─── Remember shortcut ──────────────────────────────────────────────────────

/// Low-friction write surface for IDE / human use. Infers `path`, `category`,
/// and `importance` so callers only need to supply text. Internally delegates
/// to `handle_save_memory`, so the capture gate, noise filter, provenance, and
/// enrichment pipeline all run identically to a `save_memory` call.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub(crate) struct RememberParams {
    /// Full text content to remember.
    pub text: String,

    /// Optional short summary (≤100 chars). If omitted, the enrichment pipeline
    /// will generate one asynchronously.
    #[serde(default)]
    pub summary: String,

    /// Optional tags (forwarded as `keywords` to save_memory).
    #[serde(default)]
    pub tags: Vec<String>,

    /// Optional explicit topic / subject area.
    #[serde(default)]
    pub topic: String,

    /// Importance score 0.0–1.0. Defaults to 0.6 (slightly below save_memory's
    /// 0.7) since `remember` is intended for casual notes.
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub importance: Option<f64>,

    /// Scope: "user" | "project" | "general". Defaults to "project".
    #[serde(default)]
    pub scope: Option<String>,

    /// Optional named project DB target.
    #[serde(default)]
    pub project: Option<String>,

    /// Optional path override. If omitted, defaults to `/notes/{YYYY-MM-DD}`.
    #[serde(default)]
    pub path: Option<String>,

    /// Optional category override. Defaults to "fact".
    #[serde(default)]
    pub category: Option<String>,

    /// Optional domain (e.g. "finance", "code-review").
    #[serde(default)]
    pub domain: Option<String>,

    /// Optional retention policy.
    #[serde(default)]
    pub retention_policy: Option<String>,

    /// When this memory became true/effective. Defaults to timestamp.
    #[serde(default)]
    pub valid_from: Option<String>,

    /// When this memory stopped being true/effective. None = still valid.
    #[serde(default)]
    pub valid_until: Option<String>,

    /// Bypass noise filter (forwarded to save_memory). Defaults to false.
    #[serde(default)]
    pub force: bool,
}

// ─── Search ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct HybridWeightsParam {
    /// Semantic (vector) weight (default: 0.4)
    #[serde(default = "default_weight_semantic")]
    pub semantic: f64,
    /// Full-text search weight (default: 0.3)
    #[serde(default = "default_weight_fts")]
    pub fts: f64,
    /// Symbolic weight (default: 0.2)
    #[serde(default = "default_weight_symbolic")]
    pub symbolic: f64,
    /// Decay weight (default: 0.1)
    #[serde(default = "default_weight_decay")]
    pub decay: f64,
    /// Use Reciprocal Rank Fusion instead of linear weighted blending.
    #[serde(default)]
    pub use_rrf: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SearchMemoryParams {
    /// Search query text
    pub query: String,

    /// Optional query embedding vector; when provided, enables vector channel
    #[serde(default)]
    pub query_vec: Option<Vec<f32>>,

    /// Number of results to return (default: 6)
    #[serde(default = "default_top_k")]
    pub top_k: usize,

    /// Optional path prefix filter
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Include training/distillation corpus entries such as `/sft/...`.
    /// Defaults to false so normal agent recall stays focused on live memory.
    /// Explicit `/sft` path_prefix searches are always treated as opt-in.
    #[serde(default)]
    pub include_training: bool,

    /// Whether to include archived entries
    #[serde(default)]
    pub include_archived: bool,

    /// Number of candidates per channel
    #[serde(default = "default_candidates")]
    pub candidates_per_channel: usize,

    /// MMR diversity threshold (0.0-1.0), set to null to disable
    #[serde(
        default = "default_mmr_threshold",
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub mmr_threshold: Option<f64>,

    /// Graph expand hops (0 = disabled, default = 1)
    #[serde(default = "default_graph_hops")]
    pub graph_expand_hops: u32,

    /// Optional relation filter for graph expansion
    #[serde(default)]
    pub graph_relation_filter: Option<String>,

    /// Optional scoring weights override {semantic, fts, symbolic, decay}
    #[serde(default)]
    pub weights: Option<HybridWeightsParam>,

    /// Optional agent role for sandbox filtering (e.g. "finance", "code-review")
    #[serde(default)]
    pub agent_role: Option<String>,

    /// Optional project name to search a specific project DB (e.g. "hapi", "sigil").
    /// If omitted, searches the default project DB configured at startup.
    #[serde(default)]
    pub project: Option<String>,

    /// Domain filter — only return memories belonging to this domain.
    /// NULL means no domain filtering.
    #[serde(default)]
    pub domain: Option<String>,

    /// Optional current file path for guide/context-aware retrieval.
    #[serde(default)]
    pub file_context: Option<String>,

    /// Optional current error text for guide/context-aware retrieval.
    #[serde(default)]
    pub error_context: Option<String>,

    /// Enable adaptive Voyage reranking when top hybrid scores are close.
    #[serde(default)]
    pub enable_rerank: bool,

    /// Point-in-time validity filter (ISO 8601). Returns only memories valid at this time.
    #[serde(default)]
    pub as_of: Option<String>,

    /// When true, include the full `metadata` blob on each result row. Off by
    /// default to keep token usage tight; the kanban board sets this so it can
    /// surface `a2a_state`, `eval_ledger_id`, `agent`, etc.
    #[serde(default)]
    pub include_metadata: bool,
}

impl SearchMemoryParams {
    pub(crate) fn normalized_top_k(&self) -> usize {
        self.top_k.clamp(1, MAX_SEARCH_TOP_K)
    }

    pub(crate) fn normalized_candidates_per_channel(&self) -> usize {
        let requested = if self.candidates_per_channel == 0 {
            default_candidates()
        } else {
            self.candidates_per_channel
        };
        requested
            .max(self.normalized_top_k())
            .min(MAX_SEARCH_CANDIDATES_PER_CHANNEL)
    }

    /// Build SearchOptions from params, only differing by vec_available per DB.
    pub(crate) fn to_search_options(&self, vec_available: bool) -> SearchOptions {
        let weights = match &self.weights {
            Some(w) => HybridWeights {
                semantic: w.semantic,
                fts: w.fts,
                symbolic: w.symbolic,
                decay: w.decay,
                use_rrf: w.use_rrf,
            },
            None => HybridWeights::default(),
        };
        SearchOptions {
            top_k: self.normalized_top_k(),
            path_prefix: self.path_prefix.clone(),
            query_vec: self.query_vec.clone(),
            include_archived: self.include_archived,
            candidates_per_channel: self.normalized_candidates_per_channel(),
            mmr_threshold: self.mmr_threshold,
            graph_expand_hops: self.graph_expand_hops,
            graph_relation_filter: self.graph_relation_filter.clone(),
            vec_available,
            weights,
            // Keep search path read-only so multiple search requests can run concurrently.
            record_access: false,
            domain: self.domain.clone(),
            as_of: self.as_of.clone(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct FindSimilarMemoryParams {
    /// Query embedding vector (same dimension as stored embeddings)
    pub query_vec: Vec<f32>,

    /// Number of results to return (default: 5)
    #[serde(default = "default_find_similar_top_k")]
    pub top_k: usize,

    /// Optional path prefix filter
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Whether to include archived entries
    #[serde(default)]
    pub include_archived: bool,

    /// Include isolated SFT/training-seed memories in vector similarity results.
    /// Defaults to false; callers may also opt in with path_prefix="/sft".
    #[serde(default)]
    pub include_training: bool,

    /// Number of candidates pulled from vector channel (default: 20)
    #[serde(default = "default_candidates")]
    pub candidates_per_channel: usize,
}

impl FindSimilarMemoryParams {
    pub(crate) fn normalized_top_k(&self) -> usize {
        self.top_k.clamp(1, MAX_SEARCH_TOP_K)
    }

    pub(crate) fn normalized_candidates_per_channel(&self) -> usize {
        let requested = if self.candidates_per_channel == 0 {
            default_candidates()
        } else {
            self.candidates_per_channel
        };
        requested
            .max(self.normalized_top_k())
            .min(MAX_SEARCH_CANDIDATES_PER_CHANNEL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_params(top_k: usize, candidates_per_channel: usize) -> SearchMemoryParams {
        SearchMemoryParams {
            query: "probe".to_string(),
            query_vec: None,
            top_k,
            path_prefix: None,
            include_training: false,
            include_archived: false,
            candidates_per_channel,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: None,
            domain: None,
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        }
    }

    #[test]
    fn search_memory_limits_are_capped_before_db_query() {
        let params = search_params(10_000, 50_000);
        let opts = params.to_search_options(false);

        assert_eq!(params.normalized_top_k(), MAX_SEARCH_TOP_K);
        assert_eq!(
            params.normalized_candidates_per_channel(),
            MAX_SEARCH_CANDIDATES_PER_CHANNEL
        );
        assert_eq!(opts.top_k, MAX_SEARCH_TOP_K);
        assert_eq!(
            opts.candidates_per_channel,
            MAX_SEARCH_CANDIDATES_PER_CHANNEL
        );
    }

    #[test]
    fn search_memory_limits_keep_candidates_at_least_top_k() {
        let params = search_params(75, 1);
        let opts = params.to_search_options(false);

        assert_eq!(opts.top_k, 75);
        assert_eq!(opts.candidates_per_channel, 75);
    }

    #[test]
    fn find_similar_limits_are_capped_before_db_query() {
        let params = FindSimilarMemoryParams {
            query_vec: vec![0.1, 0.2],
            top_k: 10_000,
            path_prefix: None,
            include_archived: false,
            include_training: false,
            candidates_per_channel: 50_000,
        };

        assert_eq!(params.normalized_top_k(), MAX_SEARCH_TOP_K);
        assert_eq!(
            params.normalized_candidates_per_channel(),
            MAX_SEARCH_CANDIDATES_PER_CHANNEL
        );
    }
}

// ─── Get / List / Delete / Archive ──────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GetMemoryParams {
    /// Memory entry ID
    pub id: String,

    /// Whether to include archived entries
    #[serde(default)]
    pub include_archived: bool,

    /// Optional project name to search a specific project DB (e.g. "hapi", "sigil").
    /// If omitted, searches the default project DB configured at startup.
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct ListMemoriesParams {
    /// Path prefix to filter
    #[serde(default = "default_path")]
    pub path_prefix: String,

    /// Maximum number of entries to return
    #[serde(default = "default_limit")]
    pub limit: usize,

    /// Whether to include archived entries
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct DeleteMemoryParams {
    /// Memory entry ID to delete
    pub id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct ArchiveMemoryParams {
    /// Memory entry ID to archive
    pub id: String,
}

// ─── Graph Edges ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct AddEdgeParams {
    /// Source memory ID
    pub source_id: String,
    /// Target memory ID
    pub target_id: String,
    /// Relation type (e.g. "causes", "follows", "related_to")
    pub relation: String,
    /// Edge weight (default: 1.0)
    #[serde(default = "default_edge_weight")]
    pub weight: f64,
    /// Optional JSON metadata for the edge
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,

    /// Scope: "global" or "project" (default)
    #[serde(default = "default_scope")]
    pub scope: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GetEdgesParams {
    /// Memory entry ID
    pub memory_id: String,
    /// Direction: "outgoing", "incoming", or "both" (default: "both")
    #[serde(default = "default_edge_direction")]
    pub direction: String,
    /// Optional relation type filter
    #[serde(default)]
    pub relation_filter: Option<String>,

    /// Scope: "global" or "project" (default)
    #[serde(default = "default_scope")]
    pub scope: String,
}

// ─── Sync ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SyncMemoriesParams {
    /// Unique agent identifier for tracking known state
    pub agent_id: String,
    /// Optional path prefix to scope the sync (e.g. "/project")
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Maximum entries to return (default: 100)
    #[serde(default = "default_sync_limit")]
    pub limit: usize,
}

// ─── State / Extraction / Ingest ────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct SetStateParams {
    /// State key
    pub key: String,

    /// State value (JSON value)
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GetStateParams {
    /// State key
    pub key: String,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub(crate) struct ExtractFactsParams {
    /// Text to extract facts from
    pub text: String,

    /// Source identifier for the extraction
    #[serde(default = "default_extraction_source")]
    pub source: String,
}

/// A single message in a conversation turn.
///
/// Some MCP clients send compact session windows as raw strings even though the
/// schema advertises `{role, content}` objects. Accept both shapes at the
/// boundary so callers get deterministic tool behavior instead of serde
/// transport errors.
#[derive(Debug, Clone, serde::Serialize, JsonSchema)]
pub(crate) struct Message {
    /// Role of the message sender (e.g., "user", "assistant", "system")
    #[allow(dead_code)]
    pub role: String,
    /// Content of the message
    pub content: String,
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum MessageInput {
            Object {
                #[serde(default = "default_message_role")]
                role: String,
                content: String,
            },
            Text(String),
        }

        match MessageInput::deserialize(deserializer)? {
            MessageInput::Object { role, content } => Ok(Message { role, content }),
            MessageInput::Text(content) => Ok(Message {
                role: default_message_role(),
                content,
            }),
        }
    }
}

fn default_message_role() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct IngestEventParams {
    /// Conversation identifier
    #[serde(default)]
    pub conversation_id: String,

    /// Turn identifier
    #[serde(default)]
    pub turn_id: String,

    /// Optional event type label for structured events
    #[serde(default)]
    pub event_type: Option<String>,

    /// Optional structured event payload
    #[serde(default)]
    pub content: Option<serde_json::Value>,

    /// Messages in the conversation turn
    #[serde(default)]
    pub messages: Vec<Message>,

    /// Optional path prefix override for structured event writes
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Optional write importance for structured event writes
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_f64_from_string_or_number"
    )]
    pub importance: Option<f64>,

    /// Target scope for writes
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Optional named project target
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain tag
    #[serde(default)]
    pub domain: Option<String>,

    /// Optional extra metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct IngestSourceParams {
    /// Raw source content to ingest
    pub content: String,

    /// Optional source URL or canonical reference
    #[serde(default)]
    pub source_url: Option<String>,

    /// Optional logical source identifier
    #[serde(default)]
    pub source: Option<String>,

    /// Optional path prefix used for chunk paths
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Whether to chunk long content before storage
    #[serde(default = "default_auto_chunk")]
    pub auto_chunk: bool,

    /// Whether to generate summaries for stored chunks
    #[serde(default = "default_true")]
    pub auto_summarize: bool,

    /// Whether to build graph edges against similar memories
    #[serde(default = "default_true")]
    pub auto_link: bool,

    /// Base importance for stored chunks
    #[serde(default = "default_importance")]
    pub importance: f64,

    /// Target scope for writes
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Optional named project target
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain tag
    #[serde(default)]
    pub domain: Option<String>,

    /// Chunk size in characters
    #[serde(default = "default_chunk_size_chars")]
    pub chunk_size_chars: usize,

    /// Overlap between adjacent chunks in characters
    #[serde(default = "default_chunk_overlap_chars")]
    pub chunk_overlap_chars: usize,

    /// Optional extra metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct IngestParams {
    /// Ingest mode: "event" or "source"
    #[serde(default = "default_ingest_type")]
    pub ingest_type: String,

    /// Raw source content or structured event payload
    #[serde(default)]
    pub content: Option<serde_json::Value>,

    /// Optional source URL or canonical reference
    #[serde(default)]
    pub source_url: Option<String>,

    /// Optional logical source identifier
    #[serde(default)]
    pub source: Option<String>,

    /// Optional path prefix used for chunk paths
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Whether to chunk long content before storage
    #[serde(default = "default_auto_chunk")]
    pub auto_chunk: bool,

    /// Whether to generate summaries for stored chunks
    #[serde(default = "default_true")]
    pub auto_summarize: bool,

    /// Whether to build graph edges against similar memories
    #[serde(default = "default_true")]
    pub auto_link: bool,

    /// Base importance for stored chunks
    #[serde(default = "default_importance")]
    pub importance: f64,

    /// Target scope for writes
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Optional named project target
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain tag
    #[serde(default)]
    pub domain: Option<String>,

    /// Chunk size in characters
    #[serde(default = "default_chunk_size_chars")]
    pub chunk_size_chars: usize,

    /// Overlap between adjacent chunks in characters
    #[serde(default = "default_chunk_overlap_chars")]
    pub chunk_overlap_chars: usize,

    /// Conversation identifier for event ingestion
    #[serde(default)]
    pub conversation_id: Option<String>,

    /// Turn identifier for event ingestion
    #[serde(default)]
    pub turn_id: Option<String>,

    /// Event type label for event ingestion
    #[serde(default)]
    pub event_type: Option<String>,

    /// Messages in the conversation turn
    #[serde(default)]
    pub messages: Vec<Message>,

    /// Optional extra metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

// ─── Domain Management ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct RegisterDomainParams {
    /// Unique domain name (e.g. "finance", "code-review")
    pub name: String,

    /// Human-readable description of this domain
    #[serde(default)]
    pub description: Option<String>,

    /// GC stale-days threshold for memories in this domain (default: 90)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u32_from_string_or_number"
    )]
    pub gc_threshold_days: Option<u32>,

    /// Default retention policy for memories saved to this domain
    #[serde(default)]
    pub default_retention: Option<String>,

    /// Default path prefix for memories saved to this domain
    #[serde(default)]
    pub default_path_prefix: Option<String>,

    /// Arbitrary JSON metadata
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct GetDomainParams {
    /// Domain name to retrieve
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct ListDomainsParams {
    /// Placeholder (no filters currently needed)
    #[serde(default)]
    pub _placeholder: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct DeleteDomainParams {
    /// Domain name to delete
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct WikiLintParams {
    /// Path prefix to lint (default: /wiki)
    #[serde(default = "default_wiki_path_prefix_opt")]
    pub path_prefix: Option<String>,

    /// Checks to run: orphans, contradictions, stale, missing_edges
    #[serde(default)]
    pub checks: Vec<String>,

    /// Maximum memories to inspect per scope
    #[serde(default = "default_limit")]
    pub limit: usize,

    /// Days before a wiki memory is considered stale
    #[serde(default = "default_wiki_stale_days")]
    pub stale_days: u32,

    /// Similarity threshold for missing edge hints
    #[serde(default = "default_missing_edge_threshold")]
    pub missing_edge_threshold: f64,

    /// Similarity threshold for contradiction candidates
    #[serde(default = "default_contradiction_threshold")]
    pub contradiction_threshold: f64,

    /// When false, skip expensive skill-quality guard refresh (briefing uses this).
    #[serde(default = "default_include_skill_quality")]
    pub include_skill_quality: bool,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub(crate) struct WikiWriteParams {
    /// Short title for the wiki entry.
    pub title: String,

    /// Full wiki entry body.
    pub text: String,

    /// Optional explicit /wiki path. Non-/wiki paths are nested under /wiki.
    #[serde(default)]
    pub path: Option<String>,

    /// Optional topic. Defaults to a sanitized title.
    #[serde(default)]
    pub topic: Option<String>,

    /// Short summary. Defaults to the title.
    #[serde(default)]
    pub summary: Option<String>,

    /// Category for the underlying memory entry.
    #[serde(default = "default_wiki_write_category")]
    pub category: String,

    /// Keyword tags.
    #[serde(default)]
    pub keywords: Vec<String>,

    /// Entity names mentioned.
    #[serde(default)]
    pub entities: Vec<String>,

    /// Importance score.
    #[serde(default = "default_wiki_write_importance")]
    pub importance: f64,

    /// Scope for the underlying memory write; wiki defaults to global.
    #[serde(default = "default_wiki_write_scope")]
    pub scope: String,

    /// Retention policy; wiki defaults to permanent.
    #[serde(default = "default_wiki_retention_policy")]
    pub retention_policy: String,

    /// Optional domain.
    #[serde(default)]
    pub domain: Option<String>,

    /// Optional named project DB.
    #[serde(default)]
    pub project: Option<String>,

    /// Optional JSON object merged into stored wiki metadata before provenance fields.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,

    /// Bypass noise filtering for short but intentional wiki entries.
    #[serde(default)]
    pub force: bool,

    /// External references: URLs, absolute paths, or GitHub shorthands (#N, repo#N, owner/repo#N).
    #[serde(default)]
    pub references: Vec<String>,
}

// ─── Wiki Search / Browse ───────────────────────────────────────────────────

fn default_wiki_project() -> String {
    "wiki".to_string()
}

fn default_wiki_search_top_k() -> usize {
    10
}

fn default_wiki_browse_limit() -> usize {
    50
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct WikiSearchParams {
    /// Search query text.
    pub query: String,

    /// Wiki path prefix. Defaults to /wiki.
    #[serde(default = "default_wiki_path_prefix_opt")]
    pub path_prefix: Option<String>,

    /// Wiki category filter for legacy wiki_search, e.g. "quant" or "/wiki/engineering".
    #[serde(default)]
    pub category: Option<String>,

    /// Number of results to return.
    #[serde(default = "default_wiki_search_top_k")]
    pub top_k: usize,

    /// Include archived wiki entries.
    #[serde(default)]
    pub include_archived: bool,

    /// Optional agent role for sandbox filtering.
    #[serde(default)]
    pub agent_role: Option<String>,

    /// Optional named project DB.
    #[serde(default)]
    pub project: Option<String>,

    /// Optional domain filter.
    #[serde(default)]
    pub domain: Option<String>,

    /// Optional current file path for guide/context-aware retrieval.
    #[serde(default)]
    pub file_context: Option<String>,

    /// Optional current error text for guide/context-aware retrieval.
    #[serde(default)]
    pub error_context: Option<String>,

    /// Optional scoring weights override
    #[serde(default)]
    pub weights: Option<HybridWeightsParam>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct WikiBrowseParams {
    /// Wiki category path to browse, e.g. "/wiki/quant/strategy" or just "quant".
    /// If omitted, returns top-level category stats.
    #[serde(default)]
    pub category: Option<String>,

    /// Maximum entries to return when browsing a specific category
    #[serde(default = "default_wiki_browse_limit")]
    pub limit: usize,

    /// Named project DB containing wiki memories (default: "wiki")
    #[serde(default = "default_wiki_project")]
    pub project: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiWikiIngestParams {
    /// URL or file path to ingest.
    pub source: String,

    /// Optional topic hint for categorization.
    #[serde(default)]
    pub topic: Option<String>,

    /// Whether to update related wiki entries.
    #[serde(default = "default_true")]
    pub update_related: bool,
}

pub(crate) const MIN_FACT_CHAR_COUNT: usize = 30;

/// Build a MemoryEntry from a JSON fact value (shared by extract_facts and ingest_event).
pub(crate) fn fact_to_entry(
    fact: &serde_json::Value,
    source: &str,
    metadata: serde_json::Value,
) -> Option<MemoryEntry> {
    fn string_list(value: &serde_json::Value) -> Vec<String> {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::trim))
                    .filter(|item| !item.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    }

    let text = fact["text"].as_str().unwrap_or("").to_string();
    if text.is_empty() {
        return None;
    }
    let force = metadata
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !force {
        let char_count = text.chars().count();
        if char_count < MIN_FACT_CHAR_COUNT {
            tracing::warn!(
                "[capture_gate] Fact rejected: text too short ({} < {} chars). Text: {:?}",
                char_count,
                MIN_FACT_CHAR_COUNT,
                text
            );
            return None;
        }
        if memory_core::is_noise_text(&text) {
            tracing::warn!(
                "[capture_gate] Fact rejected: noise assessment failed. Text: {:?}",
                text
            );
            return None;
        }
    }
    let topic = fact["topic"].as_str().unwrap_or("").to_string();
    let importance = fact["importance"].as_f64().unwrap_or(0.7).clamp(0.0, 1.0);
    let keywords = string_list(&fact["keywords"]);
    let mut entities = string_list(&fact["entities"]);
    memory_core::types::fold_person_names_into_entities(
        &mut entities,
        string_list(&fact["persons"]),
    );
    let scope_raw = fact["scope"].as_str().unwrap_or("general");
    let scope = match scope_raw {
        "user" | "project" | "general" => scope_raw.to_string(),
        _ => "general".to_string(),
    };
    let summary = text.chars().take(100).collect::<String>();
    Some(MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        path: format!("/{}/{}", scope, topic.replace(' ', "_")),
        summary,
        text,
        importance,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic,
        keywords,
        persons: Vec::new(),
        entities,
        location: String::new(),
        source: source.to_string(),
        scope,
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata,
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    })
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct TachiWikiOrganizeParams {
    /// Absolute path to the docs directory to organize.
    pub dir_path: String,
    /// When true, report planned moves / frontmatter / task-sync changes
    /// WITHOUT touching the filesystem (no moves, no writes, no _index.md
    /// rebuild). Defaults to false (apply changes).
    #[serde(default)]
    pub dry_run: bool,
}
