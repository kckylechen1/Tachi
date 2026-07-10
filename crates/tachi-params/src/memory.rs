use chrono::Utc;
use memcore::MemoryEntry;
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;

mod ingest;
mod search;
mod wiki;
pub use ingest::{
    ExtractFactsParams, IngestEventParams, IngestParams, IngestSourceParams, Message,
};
pub use search::{
    FindSimilarMemoryParams, HybridWeightsParam, SearchMemoryParams,
    MAX_SEARCH_CANDIDATES_PER_CHANNEL, MAX_SEARCH_TOP_K,
};
pub use wiki::{
    TachiWikiIngestParams, TachiWikiOrganizeParams, WikiBrowseParams, WikiLintParams,
    WikiSearchParams, WikiWriteParams,
};

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

fn default_limit() -> usize {
    100
}

fn default_true() -> bool {
    true
}

fn default_sync_limit() -> usize {
    100
}

// ─── Save / Update ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct SaveMemoryParams {
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

    /// Also append a typed continuity ledger event for this save.
    /// Default is false so ordinary memory writes do not unexpectedly become
    /// pattern/outcome projections.
    #[serde(default)]
    pub emit_continuity: bool,
}

// ─── Remember shortcut ──────────────────────────────────────────────────────

/// Low-friction write surface for IDE / human use. Infers `path`, `category`,
/// and `importance` so callers only need to supply text. Internally delegates
/// to `handle_save_memory`, so the capture gate, noise filter, provenance, and
/// enrichment pipeline all run identically to a `save_memory` call.
#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct RememberParams {
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
    #[schemars(schema_with = "super::coerce::opt_number_from_string_or_number_schema")]
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

    /// Optional domain (e.g. "domain-pack", "code-review").
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

// ─── Get / List / Delete / Archive ──────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct GetMemoryParams {
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

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct ListMemoriesParams {
    /// Path prefix to filter
    #[serde(default = "default_path")]
    pub path_prefix: String,

    /// Maximum number of entries to return
    #[serde(default = "default_limit")]
    pub limit: usize,

    /// Whether to include archived entries
    #[serde(default)]
    pub include_archived: bool,

    /// Optional project name to list a specific project DB.
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, JsonSchema)]
pub struct DeleteMemoryParams {
    /// Memory entry ID to delete
    pub id: String,

    /// Optional project name to delete from a specific project DB.
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ArchiveMemoryParams {
    /// Memory entry ID to archive
    pub id: String,

    /// Optional project name to archive in a specific project DB.
    #[serde(default)]
    pub project: Option<String>,
}

// ─── Sync ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SyncMemoriesParams {
    /// Unique agent identifier for tracking known state
    pub agent_id: String,
    /// Optional path prefix to scope the sync (e.g. "/project")
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Maximum entries to return (default: 100)
    #[serde(default = "default_sync_limit")]
    pub limit: usize,
}

// ─── Domain Management ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RegisterDomainParams {
    /// Unique domain name (e.g. "domain-pack", "code-review")
    pub name: String,

    /// Human-readable description of this domain
    #[serde(default)]
    pub description: Option<String>,

    /// GC stale-days threshold for memories in this domain (default: 90)
    #[serde(
        default,
        deserialize_with = "super::coerce::opt_u32_from_string_or_number"
    )]
    #[schemars(schema_with = "super::coerce::opt_integer_from_string_or_number_schema")]
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
pub struct GetDomainParams {
    /// Domain name to retrieve
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ListDomainsParams {
    /// Placeholder (no filters currently needed)
    #[serde(default)]
    pub _placeholder: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DeleteDomainParams {
    /// Domain name to delete
    pub name: String,
}

pub const MIN_FACT_CHAR_COUNT: usize = 30;

/// Build a MemoryEntry from a JSON fact value (shared by extract_facts and ingest_event).
///
/// Lossy wrapper around [`fact_to_entry_with_reason`]: returns `None` when the
/// capture gate rejects the fact. Callers that need to surface *why* a fact was
/// dropped (e.g. `extract_facts`, so `facts_extracted` vs `facts_saved` gaps are
/// explainable) should call [`fact_to_entry_with_reason`] instead.
pub fn fact_to_entry(
    fact: &serde_json::Value,
    source: &str,
    metadata: serde_json::Value,
) -> Option<MemoryEntry> {
    fact_to_entry_with_reason(fact, source, metadata).ok()
}

/// Stable capture-gate rejection reasons surfaced to callers. Kept as `&'static
/// str` so they can be embedded directly in JSON responses.
pub const FACT_DROP_EMPTY: &str = "empty_text";
pub const FACT_DROP_TOO_SHORT: &str = "too_short";
pub const FACT_DROP_NOISE: &str = "noise";

/// Build a MemoryEntry from a JSON fact value, or return the capture-gate
/// rejection reason. This is the single source of truth for the gate; keep
/// `fact_to_entry`'s behavior identical by routing it through here.
pub fn fact_to_entry_with_reason(
    fact: &serde_json::Value,
    source: &str,
    metadata: serde_json::Value,
) -> Result<MemoryEntry, &'static str> {
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
        return Err(FACT_DROP_EMPTY);
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
            return Err(FACT_DROP_TOO_SHORT);
        }
        if memcore::is_noise_text(&text) {
            tracing::warn!(
                "[capture_gate] Fact rejected: noise assessment failed. Text: {:?}",
                text
            );
            return Err(FACT_DROP_NOISE);
        }
    }
    let topic = fact["topic"].as_str().unwrap_or("").to_string();
    let importance = fact["importance"].as_f64().unwrap_or(0.7).clamp(0.0, 1.0);
    let keywords = string_list(&fact["keywords"]);
    let mut entities = string_list(&fact["entities"]);
    memcore::types::fold_person_names_into_entities(
        &mut entities,
        string_list(&fact["persons"]),
    );
    let scope_raw = fact["scope"].as_str().unwrap_or("general");
    let scope = match scope_raw {
        "user" | "project" | "general" => scope_raw.to_string(),
        _ => "general".to_string(),
    };
    let summary = text.chars().take(100).collect::<String>();
    Ok(MemoryEntry {
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
