use super::{default_limit, default_true, HybridWeightsParam};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

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
pub struct WikiLintParams {
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

    /// #1072 fix-round (#1215): when true AND the `stale` check is running,
    /// entries the semantic-staleness pass flags (targeted by a
    /// `contradicts`/`supersedes` edge) get `metadata.lifecycle = "stale"`
    /// persisted back to the store — not just reported as a diagnostic row.
    /// Canon doc §7's required behavior is retrieval EXCLUSION, not a lint
    /// finding a human has to act on separately; the cross-vendor review
    /// flagged this gap explicitly ("lint appends a diagnostic row only;
    /// persisted lifecycle stays active and retrievable"). Defaults to
    /// `false` — `wiki_hygiene_counts` (called on every
    /// `tachi_memory(briefing)`/`alerts` request) explicitly keeps this off
    /// so a hot, high-frequency read path never becomes a surprise writer;
    /// only an explicit `tachi_wiki(action='lint', persist_stale=true)` call
    /// opts in.
    #[serde(default)]
    pub persist_stale: bool,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct WikiWriteParams {
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

// ─── Wiki Search / Browse ───────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WikiSearchParams {
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

    /// #1072 explicit lifecycle scope. Omitted (default): only `active`
    /// wiki/guide entries are returned (the truthful-retrieval gate — a
    /// `pending_review` draft is never silently served as reviewed). Pass
    /// one of `candidate | pending_review | active | stale | superseded |
    /// rejected` to browse that lifecycle explicitly, or `"all"` to disable
    /// the gate entirely.
    #[serde(default)]
    pub lifecycle: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct WikiBrowseParams {
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

    /// #1072 explicit lifecycle scope. Omitted (default): only `active`
    /// wiki/guide entries are returned (the truthful-retrieval gate). Pass
    /// one of `candidate | pending_review | active | stale | superseded |
    /// rejected` to browse that lifecycle explicitly, or `"all"` to disable
    /// the gate entirely.
    #[serde(default)]
    pub lifecycle: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiWikiIngestParams {
    /// URL or file path to ingest.
    pub source: String,

    /// Optional topic hint for categorization.
    #[serde(default)]
    pub topic: Option<String>,

    /// Whether to update related wiki entries.
    #[serde(default = "default_true")]
    pub update_related: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TachiWikiOrganizeParams {
    /// Absolute path to the docs directory to organize.
    pub dir_path: String,
    /// When true, report planned moves / frontmatter / task-sync changes
    /// WITHOUT touching the filesystem (no moves, no writes, no _index.md
    /// rebuild). Defaults to false (apply changes).
    #[serde(default)]
    pub dry_run: bool,
}
