// types.rs — Unified data types for memory-core
//
// This schema is the **single source of truth** for all consumers:
//   - OpenClaw memory-hybrid-bridge (Node.js via NAPI)
//   - Antigravity memory-mcp (Python via PyO3)
//   - Rust native
//
// Design: serde only, NO binding-specific macros (#[napi], #[pyclass]).
// Bindings use JSON string serialization for maximum compatibility.

use serde::{Deserialize, Serialize};

// ─── Retention Policy ────────────────────────────────────────────────────────

/// Memory retention policy controlling GC behavior.
///
/// - `ephemeral`: short-lived, GC aggressively (low importance threshold)
/// - `durable`:   default; standard GC thresholds apply
/// - `permanent`: never auto-archived by GC (can still be manually archived)
/// - `pinned`:    never auto-archived AND boosted in search results
///
/// NULL in the database is treated as `durable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPolicy {
    Ephemeral,
    #[default]
    Durable,
    Permanent,
    Pinned,
}

impl RetentionPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::Durable => "durable",
            Self::Permanent => "permanent",
            Self::Pinned => "pinned",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s {
            Some("ephemeral") => Self::Ephemeral,
            Some("durable") => Self::Durable,
            Some("permanent") => Self::Permanent,
            Some("pinned") => Self::Pinned,
            _ => Self::Durable, // NULL or unrecognized → durable
        }
    }

    /// Whether GC should skip this policy entirely.
    pub fn is_gc_exempt(&self) -> bool {
        matches!(self, Self::Permanent | Self::Pinned)
    }
}

impl std::fmt::Display for RetentionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Memory Source ──────────────────────────────────────────────────────────

/// Canonical write provenance for memory entries.
///
/// User-controlled callers (e.g. ingest_source) may submit arbitrary strings;
/// those should be funneled through [`MemorySource::parse_or_external`] which
/// either returns a canonical name or an `external:<sanitized>` prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    Manual,
    Extraction,
    Migration,
    Auto,
    FoundryDistill,
    Handoff,
    Kanban,
    Wiki,
    Ghost,
    IngestEvent,
}

impl MemorySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Extraction => "extraction",
            Self::Migration => "migration",
            Self::Auto => "auto",
            Self::FoundryDistill => "foundry_distill",
            Self::Handoff => "handoff",
            Self::Kanban => "kanban",
            Self::Wiki => "wiki",
            Self::Ghost => "ghost",
            Self::IngestEvent => "ingest_event",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim() {
            "manual" => Self::Manual,
            "extraction" => Self::Extraction,
            "migration" => Self::Migration,
            "auto" => Self::Auto,
            "foundry_distill" => Self::FoundryDistill,
            "handoff" => Self::Handoff,
            "kanban" => Self::Kanban,
            "wiki" => Self::Wiki,
            "ghost" => Self::Ghost,
            "ingest_event" => Self::IngestEvent,
            _ => Self::Manual,
        }
    }

    /// Returns canonical source name if `s` matches an enum variant; otherwise
    /// returns `external:<sanitized>` (lowercased, non `[a-z0-9_-]` → `_`).
    /// Idempotent: passing in `external:foo` returns `external:foo` unchanged.
    /// Truncates the suffix to 64 chars.
    pub fn parse_or_external(s: &str) -> String {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Self::Manual.as_str().to_string();
        }
        let lower = trimmed.to_ascii_lowercase();
        // Match canonical
        match lower.as_str() {
            "manual" | "extraction" | "migration" | "auto" | "foundry_distill" | "handoff"
            | "kanban" | "wiki" | "ghost" | "ingest_event" => return lower,
            _ => {}
        }
        // Already external:?
        if let Some(rest) = lower.strip_prefix("external:") {
            let san = sanitize_source_suffix(rest);
            return format!("external:{}", san);
        }
        let san = sanitize_source_suffix(&lower);
        format!("external:{}", san)
    }

    /// Whether `s` is an acceptable canonical or external: source value.
    pub fn is_canonical(s: &str) -> bool {
        matches!(
            s,
            "manual"
                | "extraction"
                | "migration"
                | "auto"
                | "foundry_distill"
                | "handoff"
                | "kanban"
                | "wiki"
                | "ghost"
                | "ingest_event"
        ) || (s.starts_with("external:") && {
            let rest = &s["external:".len()..];
            !rest.is_empty()
                && rest
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
    }
}

impl std::fmt::Display for MemorySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn sanitize_source_suffix(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(64));
    for c in s.chars() {
        let mapped = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            c
        } else if c.is_ascii_uppercase() {
            c.to_ascii_lowercase()
        } else {
            '_'
        };
        out.push(mapped);
        if out.len() >= 64 {
            break;
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else {
        out
    }
}

// ─── Memory Category ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    #[default]
    Fact,
    Decision,
    Experience,
    Preference,
    Entity,
    Other,
    // Subsystem-owned categories. Downstream code (kanban.rs, handoff_ops.rs,
    // ghost promote, wiki write) dispatches on these by string equality, so
    // they must round-trip cleanly through the CHECK constraint.
    Kanban,
    Handoff,
    Ghost,
    Wiki,
}

impl MemoryCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Experience => "experience",
            Self::Preference => "preference",
            Self::Entity => "entity",
            Self::Other => "other",
            Self::Kanban => "kanban",
            Self::Handoff => "handoff",
            Self::Ghost => "ghost",
            Self::Wiki => "wiki",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "fact" => Self::Fact,
            "decision" => Self::Decision,
            "experience" => Self::Experience,
            "preference" => Self::Preference,
            "entity" => Self::Entity,
            "other" => Self::Other,
            "kanban" => Self::Kanban,
            "handoff" => Self::Handoff,
            "ghost" => Self::Ghost,
            "wiki" => Self::Wiki,
            _ => Self::Other,
        }
    }

    /// Normalize an arbitrary string to a canonical category str (unknown -> `other`).
    pub fn normalize(s: &str) -> &'static str {
        Self::from_str_opt(Some(s)).as_str()
    }
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Memory Scope ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    User,
    Project,
    #[default]
    General,
}

impl MemoryScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::General => "general",
        }
    }

    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "user" => Self::User,
            "project" => Self::Project,
            "general" => Self::General,
            _ => Self::default(),
        }
    }

    /// Normalize an arbitrary string to a canonical scope str (defaults to `general`).
    /// Rejects values like `self` or `other_agent:*` by mapping them to `general`.
    pub fn normalize(s: &str) -> &'static str {
        Self::from_str_opt(Some(s)).as_str()
    }
}

impl std::fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Retention defaulting ───────────────────────────────────────────────────

/// Returns the default retention_policy for an entry, based on its path and
/// source. Returns `None` if no default applies (caller should leave the field
/// untouched / NULL → durable).
///
/// Rules:
///   - path starts with `/handoff` or `/kanban` → `pinned`
///   - path starts with `/wiki`                 → `permanent`
///   - source == `foundry_distill`              → `permanent`
///   - everything else                          → None
pub fn default_retention_for(path: &str, source: &str) -> Option<&'static str> {
    if path.starts_with("/handoff") || path.starts_with("/kanban") {
        Some("pinned")
    } else if path.starts_with("/wiki") || source == "foundry_distill" {
        Some("permanent")
    } else {
        None
    }
}

// ─── GC Configuration ───────────────────────────────────────────────────────

/// Externalized GC thresholds (replaces hardcoded literals in gc_tables/archive).
#[derive(Debug, Clone)]
pub struct GcConfig {
    /// Max access_history rows to keep per memory_id (default: 256)
    pub access_history_keep_per_memory: usize,
    /// Max age for processed_events before pruning (default: 30 days)
    pub processed_events_max_days: u32,
    /// Max age for audit_log before pruning (default: 30 days)
    pub audit_log_max_days: u32,
    /// Hard cap on total audit_log rows (default: 100_000)
    pub audit_log_max_rows: usize,
    /// Max age for agent_known_state before pruning (default: 90 days)
    pub agent_known_state_max_days: u32,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            access_history_keep_per_memory: 256,
            processed_events_max_days: 30,
            audit_log_max_days: 30,
            audit_log_max_rows: 100_000,
            agent_known_state_max_days: 90,
        }
    }
}

// ─── Domain Configuration ───────────────────────────────────────────────────

/// Per-domain configuration for memory routing and GC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainConfig {
    /// Unique domain name, e.g. "finance", "code-review", "personal"
    pub name: String,
    /// Human-readable description
    #[serde(default)]
    pub description: String,
    /// Domain-specific GC stale threshold in days (overrides global default)
    #[serde(default)]
    pub gc_threshold_days: Option<u32>,
    /// Default retention policy for memories in this domain
    #[serde(default)]
    pub default_retention: Option<String>,
    /// Optional default path prefix for memories in this domain
    #[serde(default)]
    pub default_path_prefix: Option<String>,
    /// Optional JSON metadata
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    /// When the domain was created
    #[serde(default)]
    pub created_at: String,
    /// When the domain was last updated
    #[serde(default)]
    pub updated_at: String,
}

// ─── Core Entry ──────────────────────────────────────────────────────────────

/// A single memory entry, unified across all three systems.
///
/// Field mapping from legacy systems:
///   OpenClaw: entry_id → id, lossless_restatement → text, timestamp → timestamp
///   MCP:      id → id, text → text, created_at → timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// UUID primary key.
    /// Accepts "entry_id" from OpenClaw JSON for backward compat.
    #[serde(alias = "entry_id")]
    pub id: String,

    /// Hierarchical path, e.g. "/openclaw/agent-main"
    #[serde(default = "default_path")]
    pub path: String,

    /// L0 short summary (≤100 chars)
    #[serde(default)]
    pub summary: String,

    /// Full lossless text (L2). Accepts "lossless_restatement" from OpenClaw.
    #[serde(alias = "lossless_restatement")]
    pub text: String,

    /// 0.0 – 1.0 importance score
    #[serde(default = "default_importance")]
    pub importance: f64,

    /// ISO 8601 timestamp. Accepts "created_at" from MCP, "event_time" from old Rust.
    #[serde(alias = "created_at", alias = "event_time")]
    pub timestamp: String,

    /// Category: "fact" | "decision" | "experience" | "preference" | "entity" | "other"
    #[serde(default = "default_category")]
    pub category: String,

    /// Topic / subject area
    #[serde(default)]
    pub topic: String,

    /// Keyword tags (promoted from metadata for FTS indexing)
    #[serde(default)]
    pub keywords: Vec<String>,

    /// Person names mentioned
    #[serde(default)]
    pub persons: Vec<String>,

    /// Entity names mentioned (projects, tools, etc.)
    #[serde(default)]
    pub entities: Vec<String>,

    /// Physical or logical location
    #[serde(default)]
    pub location: String,

    /// How this entry was created: "manual" | "extraction" | "migration"
    #[serde(default = "default_source")]
    pub source: String,

    /// Scope: "user" | "project" | "general"
    #[serde(default = "default_scope")]
    pub scope: String,

    /// Soft-delete marker. Archived entries are excluded by default from queries.
    #[serde(default)]
    pub archived: bool,

    /// Number of times this entry has been retrieved
    #[serde(default)]
    pub access_count: i64,

    /// Last retrieval time (ISO 8601), None if never retrieved
    #[serde(default)]
    pub last_access: Option<String>,

    /// Monotonic revision for optimistic locking.
    #[serde(default = "default_revision")]
    pub revision: i64,

    /// Embedding vector (1024-dim Voyage-4) — internal only, never serialized to clients.
    #[serde(default, skip_serializing)]
    pub vector: Option<Vec<f32>>,

    /// Retention policy: "ephemeral" | "durable" | "permanent" | "pinned".
    /// NULL in DB → durable (default).
    #[serde(default)]
    pub retention_policy: Option<String>,

    /// Domain this memory belongs to (e.g. "finance", "code-review").
    /// NULL means no domain scoping.
    #[serde(default)]
    pub domain: Option<String>,

    /// Catch-all JSON blob for low-frequency fields:
    /// source_refs, caused_by, leads_to, etc.
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

// ─── Defaults ────────────────────────────────────────────────────────────────

fn default_path() -> String {
    "/".to_string()
}
fn default_importance() -> f64 {
    0.7
}
fn default_category() -> String {
    "fact".to_string()
}
fn default_source() -> String {
    "manual".to_string()
}
fn default_scope() -> String {
    "general".to_string()
}
fn default_revision() -> i64 {
    1
}
fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

// ─── Scoring Types ───────────────────────────────────────────────────────────

/// Per-channel scores for a single search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridScore {
    /// Cosine similarity from vector KNN (0.0–1.0)
    pub vector: f64,
    /// BM25 FTS score (normalized 0.0–1.0)
    pub fts: f64,
    /// Bag-of-words symbolic overlap (0.0–1.0)
    pub symbolic: f64,
    /// ACT-R memory decay factor
    pub decay: f64,
    /// Weighted final score
    #[serde(rename = "final")]
    pub final_score: f64,
}

/// A ranked search result: entry + scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub entry: MemoryEntry,
    pub score: HybridScore,
}

/// Aggregate statistics about the memory store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResult {
    pub total: u64,
    pub by_scope: std::collections::HashMap<String, u64>,
    pub by_category: std::collections::HashMap<String, u64>,
    pub by_root_path: std::collections::HashMap<String, u64>,
}

// ─── Graph Types ─────────────────────────────────────────────────────────────

/// A directed edge between two memory entries in the memory graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEdge {
    /// Source memory ID
    pub source_id: String,
    /// Target memory ID
    pub target_id: String,
    /// Relationship type: "causes", "supports", "contradicts", "follows", "related_to"
    pub relation: String,
    /// Edge weight (0.0–1.0)
    #[serde(default = "default_edge_weight")]
    pub weight: f64,
    /// Optional JSON metadata (confidence, timespan, etc.)
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    /// When the edge was created
    #[serde(default)]
    pub created_at: String,
    /// When the edge becomes valid (temporal validity start)
    #[serde(default)]
    pub valid_from: String,
    /// When the edge expires (temporal validity end); None = no expiry
    #[serde(default)]
    pub valid_to: Option<String>,
}

fn default_edge_weight() -> f64 {
    1.0
}

/// Result of a graph expansion query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphExpandResult {
    /// Memory entries found via graph traversal
    pub entries: Vec<MemoryEntry>,
    /// Edges traversed during expansion
    pub edges: Vec<MemoryEdge>,
    /// Hop distance for each entry ID
    pub distances: std::collections::HashMap<String, u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_openclaw_format() {
        let json = r#"{
            "entry_id": "m_001",
            "lossless_restatement": "Rust is fast",
            "summary": "Rust",
            "keywords": ["rust"],
            "timestamp": "2026-01-01T00:00:00Z",
            "location": "",
            "persons": ["Kyle"],
            "entities": ["OpenClaw"],
            "topic": "tech",
            "scope": "project",
            "path": "/openclaw",
            "category": "fact",
            "importance": 0.8,
            "source_refs": []
        }"#;
        let entry: MemoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "m_001");
        assert_eq!(entry.text, "Rust is fast");
        assert_eq!(entry.persons, vec!["Kyle"]);
    }

    #[test]
    fn test_deserialize_mcp_format() {
        let json = r#"{
            "id": "abc123",
            "text": "记忆系统重写",
            "created_at": "2026-02-23T12:00:00Z",
            "path": "/",
            "importance": 0.7,
            "keywords": ["记忆"]
        }"#;
        let entry: MemoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "abc123");
        assert_eq!(entry.timestamp, "2026-02-23T12:00:00Z");
        assert_eq!(entry.category, "fact"); // default
    }

    #[test]
    fn test_retention_policy_roundtrip() {
        assert_eq!(
            RetentionPolicy::from_str_opt(None),
            RetentionPolicy::Durable
        );
        assert_eq!(
            RetentionPolicy::from_str_opt(Some("ephemeral")),
            RetentionPolicy::Ephemeral
        );
        assert_eq!(
            RetentionPolicy::from_str_opt(Some("pinned")),
            RetentionPolicy::Pinned
        );
        assert!(RetentionPolicy::Permanent.is_gc_exempt());
        assert!(RetentionPolicy::Pinned.is_gc_exempt());
        assert!(!RetentionPolicy::Durable.is_gc_exempt());
        assert!(!RetentionPolicy::Ephemeral.is_gc_exempt());
    }

    #[test]
    fn test_memory_source_roundtrip() {
        for src in [
            MemorySource::Manual,
            MemorySource::Extraction,
            MemorySource::Migration,
            MemorySource::Auto,
            MemorySource::FoundryDistill,
            MemorySource::Handoff,
            MemorySource::Kanban,
            MemorySource::Wiki,
            MemorySource::Ghost,
            MemorySource::IngestEvent,
        ] {
            let s = src.as_str();
            assert_eq!(MemorySource::from_str_opt(Some(s)), src, "roundtrip {s}");
            assert!(MemorySource::is_canonical(s));
            assert_eq!(MemorySource::parse_or_external(s), s);
        }
        // Unknown -> Manual fallback
        assert_eq!(
            MemorySource::from_str_opt(Some("garbage")),
            MemorySource::Manual
        );
        assert_eq!(MemorySource::from_str_opt(None), MemorySource::Manual);
    }

    #[test]
    fn test_parse_or_external_canonical() {
        assert_eq!(MemorySource::parse_or_external("manual"), "manual");
        assert_eq!(MemorySource::parse_or_external("Manual"), "manual");
        assert_eq!(MemorySource::parse_or_external("  ghost  "), "ghost");
    }

    #[test]
    fn test_parse_or_external_non_canonical() {
        let r = MemorySource::parse_or_external("Hub Tool / X");
        assert!(r.starts_with("external:"), "got {r}");
        assert!(MemorySource::is_canonical(&r));
        // Idempotent
        assert_eq!(MemorySource::parse_or_external(&r), r);
    }

    #[test]
    fn test_parse_or_external_empty() {
        assert_eq!(MemorySource::parse_or_external(""), "manual");
        assert_eq!(MemorySource::parse_or_external("   "), "manual");
    }

    #[test]
    fn test_parse_or_external_truncates() {
        let long = "a".repeat(200);
        let r = MemorySource::parse_or_external(&long);
        assert!(r.starts_with("external:"));
        // `external:` (9) + 64 chars = 73 max
        assert!(r.len() <= 73, "len={}", r.len());
    }

    #[test]
    fn test_memory_category_normalize() {
        assert_eq!(MemoryCategory::normalize("fact"), "fact");
        assert_eq!(MemoryCategory::normalize("Decision"), "decision");
        assert_eq!(MemoryCategory::normalize("WeirdThing"), "other");
        assert_eq!(MemoryCategory::normalize(""), "other");
        // Subsystem categories must round-trip cleanly.
        assert_eq!(MemoryCategory::normalize("kanban"), "kanban");
        assert_eq!(MemoryCategory::normalize("Handoff"), "handoff");
        assert_eq!(MemoryCategory::normalize("GHOST"), "ghost");
        assert_eq!(MemoryCategory::normalize("wiki"), "wiki");
    }

    #[test]
    fn test_memory_scope_normalize() {
        assert_eq!(MemoryScope::normalize("user"), "user");
        assert_eq!(MemoryScope::normalize("PROJECT"), "project");
        assert_eq!(MemoryScope::normalize("general"), "general");
        // Reject 'self' / 'other_agent:*' -> general
        assert_eq!(MemoryScope::normalize("self"), "general");
        assert_eq!(MemoryScope::normalize("other_agent:foo"), "general");
        assert_eq!(MemoryScope::normalize(""), "general");
    }

    #[test]
    fn test_default_retention_matrix() {
        assert_eq!(
            default_retention_for("/handoff/foo", "manual"),
            Some("pinned")
        );
        assert_eq!(default_retention_for("/handoff", "manual"), Some("pinned"));
        assert_eq!(default_retention_for("/kanban/x", "manual"), Some("pinned"));
        assert_eq!(
            default_retention_for("/wiki/lessons", "manual"),
            Some("permanent")
        );
        assert_eq!(
            default_retention_for("/notes/2026", "foundry_distill"),
            Some("permanent")
        );
        assert_eq!(default_retention_for("/notes/2026", "manual"), None);
        assert_eq!(default_retention_for("/", "manual"), None);
    }
}
