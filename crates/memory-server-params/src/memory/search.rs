use memory_core::{HybridWeights, RecallConfig, SearchOptions};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

fn default_top_k() -> usize {
    6
}

fn default_candidates() -> usize {
    20
}

fn default_mmr_threshold() -> Option<f64> {
    Some(0.85)
}

fn default_graph_hops() -> u32 {
    1
}

fn default_find_similar_top_k() -> usize {
    5
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

pub const MAX_SEARCH_TOP_K: usize = 100;
pub const MAX_SEARCH_CANDIDATES_PER_CHANNEL: usize = 500;

// ─── Search ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct HybridWeightsParam {
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

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct SearchMemoryParams {
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
        deserialize_with = "crate::coerce::opt_f64_from_string_or_number"
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

    /// Optional caller-supplied context tokens used to bias recall without
    /// overwriting the original query. Kept as `context_symbols` for
    /// compatibility with HyperMemory adapters, but values are domain-neutral.
    #[serde(default)]
    pub context_symbols: Vec<String>,

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
    pub fn normalized_top_k(&self) -> usize {
        self.top_k.clamp(1, MAX_SEARCH_TOP_K)
    }

    pub fn normalized_candidates_per_channel(&self) -> usize {
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
    pub fn to_search_options(&self, vec_available: bool) -> SearchOptions {
        self.to_search_options_with_recall_config(vec_available, None)
    }

    /// Build SearchOptions with an optional per-call recall config override for
    /// read-only simulation/eval surfaces.
    pub fn to_search_options_with_recall_config(
        &self,
        vec_available: bool,
        recall_config: Option<RecallConfig>,
    ) -> SearchOptions {
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
            recall_config,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct FindSimilarMemoryParams {
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
    pub fn normalized_top_k(&self) -> usize {
        self.top_k.clamp(1, MAX_SEARCH_TOP_K)
    }

    pub fn normalized_candidates_per_channel(&self) -> usize {
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
            context_symbols: Vec::new(),
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
