use serde::{Deserialize, Serialize};

use super::RetentionPolicy;

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
    Guide,
    Eval,
    /// #964: sticky notes (read-once agent-to-agent ephemeral memos).
    Sticky,
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
            Self::Guide => "guide",
            Self::Eval => "eval",
            Self::Sticky => "sticky",
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
            "guide" => Self::Guide,
            "eval" => Self::Eval,
            "sticky" => Self::Sticky,
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
        Some(RetentionPolicy::Pinned.as_str())
    } else if path.starts_with("/wiki") || path.starts_with("/guide") || source == "foundry_distill"
    {
        Some("permanent")
    } else {
        None
    }
}

/// Normalize importance based on category and force flag.
/// Caps non-critical entry importance below 0.85 unless force is true.
pub fn normalize_importance(raw: f64, category: &str, force: bool) -> f64 {
    let raw = raw.clamp(0.0, 1.0);
    if force {
        return raw;
    }
    if category.eq_ignore_ascii_case("decision") || category.eq_ignore_ascii_case("guide") {
        raw
    } else {
        raw.min(0.85)
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
    /// Max sampled recall groups retained after age pruning.
    pub recall_impression_max_groups: usize,
    /// Max age of sampled recall groups.
    pub recall_impression_max_days: u32,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            access_history_keep_per_memory: 256,
            processed_events_max_days: 30,
            audit_log_max_days: 30,
            audit_log_max_rows: 100_000,
            agent_known_state_max_days: 90,
            recall_impression_max_groups: 10_000,
            recall_impression_max_days: 30,
        }
    }
}

// ─── Core Entry ──────────────────────────────────────────────────────────────

/// A single memory entry, unified across all three systems.
///
/// Canonical fields for agents (MCP + new writes): `keywords`, `entities`, `domain`, `path`, …
///
/// Legacy OpenClaw/JSON `persons` is folded into `entities`; schema init drops the
/// old physical column for Tachi DBs.
///
/// Wire-compat aliases (JSON only; DB bridge copies legacy columns on open):
///   OpenClaw: entry_id → id, lossless_restatement → text
///   MCP:      created_at / event_time → timestamp
///   Legacy:   indexed_tags → keywords; domain_key → domain
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

    /// When this memory became true or effective. Defaults to `timestamp` on write.
    #[serde(default)]
    pub valid_from: String,

    /// When this memory stopped being true or effective. None = still valid.
    #[serde(default)]
    pub valid_until: Option<String>,

    /// Category: "fact" | "decision" | "experience" | "preference" | "entity" | "other"
    #[serde(default = "default_category")]
    pub category: String,

    /// Topic / subject area
    #[serde(default)]
    pub topic: String,

    /// Keyword tags (promoted from metadata for FTS indexing).
    /// Legacy HyperTachi JSON used `indexed_tags` for the same role.
    #[serde(default, alias = "indexed_tags")]
    pub keywords: Vec<String>,

    /// Legacy OpenClaw/JSON field. Keep for deserialization compatibility; new writes fold it into `entities`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persons: Vec<String>,

    /// Entity names mentioned (projects, tools, etc.)
    #[serde(default)]
    pub entities: Vec<String>,

    /// Legacy OpenClaw/JSON field. Keep for deserialization compatibility; new writes store it in metadata.
    #[serde(default, skip_serializing_if = "String::is_empty")]
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

    /// Number of times `hybrid_search` returned this entry — **not** the number
    /// of times it was retrieved (tachi#1459).
    ///
    /// This counter observes the search path only; reads through path-listing
    /// routes do not increment it. Its sole production writer is
    /// `db::record_access_with_updates`, reached only from `search.rs`'s
    /// `hybrid_search`. `db::list_by_path`, `db::list_by_path_recent` and
    /// `db::list_memories_by_path_prefix` — the routes behind kanban, handoffs,
    /// briefing projections, the cards mirror and GC candidate scans — never
    /// touch it, so a memory read constantly through one of those still reads
    /// zero here. `access_count = 0` therefore means "never surfaced by
    /// search", and any starvation ratio computed from it is an upper bound on
    /// an unknown, not a measurement.
    #[serde(default)]
    pub access_count: i64,

    /// Number of searches in which this entry reached hybrid scoring after
    /// eligibility filters, regardless of MMR or `top_k` display selection.
    ///
    /// This is scorer-only instrumentation, not retrieval or use evidence.
    /// It is written only when `hybrid_search` records access, and no ranking,
    /// lifecycle, GC, or save policy reads it; persisting a scored-only loser
    /// advances the DB-authoritative search generation but changes no ranking
    /// or policy result.
    #[serde(default)]
    pub scored_count: i64,

    /// Last time `hybrid_search` returned this entry (ISO 8601), None if it
    /// never has.
    ///
    /// Written by the recall pipeline for **every row a search returns**
    /// (`db::record_access_with_updates`), so this is an *exposure*
    /// timestamp: it records that the system displayed the memory, not that
    /// anything used it. See [`Self::last_use_at`].
    ///
    /// Same blind spot as [`Self::access_count`] (tachi#1459): this timestamp
    /// observes the search path only; reads through path-listing routes do not
    /// update it, so `None` does not mean the row was never read.
    #[serde(default)]
    pub last_access: Option<String>,

    /// Last genuine *use* time (ISO 8601), None if never used — tachi#1446.
    ///
    /// The provenance-separated sibling of [`Self::last_access`]. Its only
    /// writer is `db::record_memory_use`, reached only when a caller-initiated
    /// save named an existing memory's id (tachi#1446 signal D) — never by the
    /// recall pipeline, so no amount of searching can set it.
    ///
    /// Read by `scorer::default_decay_score_with_config` as the recency age
    /// reference when `RecallConfig::use_provenance_recency` is on (the
    /// default). With it off, decay reads `last_access` exactly as
    /// before. `scorer::surprise_score_with_config` reads its NULL-ness as
    /// "never used" under the same knob.
    ///
    /// `skip_serializing_if` is kept now that a write path exists: a row that
    /// was never cited by a save still serializes byte-identically to before,
    /// and a row that was gains the field, which is the new fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_use_at: Option<String>,

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

    /// Domain this memory belongs to (e.g. "domain-pack", "code-review").
    /// NULL means no domain scoping. HyperTachi JSON uses `domain_key` for this field.
    #[serde(default, alias = "domain_key")]
    pub domain: Option<String>,

    /// Catch-all JSON blob for low-frequency fields:
    /// source_refs, caused_by, leads_to, etc.
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,

    // ── Memory Lifecycle fields (tier-based decay and training flags) ─────────
    /// How many times a search returned this memory **with the FTS leg
    /// matching it** — not the number of times it was retrieved (tachi#1459).
    ///
    /// This counter observes the search path only; reads through path-listing
    /// routes do not increment it. Its blind spot is strictly wider than
    /// [`Self::access_count`]'s: the sole production writer is the same
    /// `db::record_access_with_updates` call in `hybrid_search`, and it
    /// increments only for the ids in that call's `fts_hits` argument, so a row
    /// the vector leg alone surfaced does not count either.
    #[serde(default)]
    pub recall_count: i64,

    /// Number of distinct query contexts (FNV-1a hash of query string) that
    /// have retrieved this memory.  Used as the promotion gate signal.
    ///
    /// This counter observes the search path only; reads through path-listing
    /// routes do not increment it (tachi#1459) — it is derived from
    /// `access_history` rows, and only `db::record_access_with_updates` writes
    /// rows carrying a query hash. Two writers, both search-path-only: the
    /// increment in that function, and the full-table reconciliation in
    /// `db::gc_tables`, which recomputes this column as the count of distinct
    /// non-empty `query_hash` values surviving in `access_history`. Because
    /// that history is pruned per memory and the tier promotion it gates is not
    /// reverted, a promoted row's recorded diversity can afterwards read lower
    /// than the value that promoted it.
    #[serde(default)]
    pub query_diversity: i64,

    /// Quality tier: "raw" | "consolidated" | "pattern".
    /// - raw:          newly written, no LLM enrichment yet, fast decay.
    /// - consolidated: distilled or manually promoted, slow decay.
    /// - pattern:      architectural / permanent knowledge, virtually no decay.
    #[serde(default = "default_tier")]
    pub tier: String,
}

/// Complete caller-frozen state used by invariant-bearing CAS operations.
///
/// Fields are private and the only constructor snapshots a full memory entry,
/// so callers cannot accidentally omit a generated field, lifecycle field, or
/// vector when defining the expected state. Exposure/use counters are
/// deliberately excluded because they can change independently of content;
/// recall count and query diversity remain bound because Wiki plans freeze them.
#[derive(Debug, Clone)]
pub struct ExpectedMemoryState {
    entry: MemoryEntry,
    superseded_by: Option<String>,
}

impl ExpectedMemoryState {
    pub fn from_entry(entry: &MemoryEntry, superseded_by: Option<&str>) -> Self {
        Self {
            entry: entry.clone(),
            superseded_by: superseded_by.map(str::to_string),
        }
    }

    pub(crate) fn revision(&self) -> i64 {
        self.entry.revision
    }

    pub(crate) fn matches(&self, current: &MemoryEntry, superseded_by: Option<&str>) -> bool {
        let expected = &self.entry;
        expected.id == current.id
            && expected.path == current.path
            && expected.summary == current.summary
            && expected.text == current.text
            && expected.importance.to_bits() == current.importance.to_bits()
            && expected.timestamp == current.timestamp
            && expected.valid_from == current.valid_from
            && expected.valid_until == current.valid_until
            && expected.category == current.category
            && expected.topic == current.topic
            && expected.keywords == current.keywords
            && expected.entities == current.entities
            && expected.source == current.source
            && expected.scope == current.scope
            && expected.archived == current.archived
            && expected.revision == current.revision
            && expected.vector == current.vector
            && expected.retention_policy == current.retention_policy
            && expected.domain == current.domain
            && expected.metadata == current.metadata
            && expected.recall_count == current.recall_count
            && expected.query_diversity == current.query_diversity
            && expected.tier == current.tier
            && self.superseded_by.as_deref() == superseded_by
    }
}

impl MemoryEntry {
    pub fn is_wiki(&self) -> bool {
        self.category.eq_ignore_ascii_case("wiki")
            || self.domain.as_deref() == Some("wiki")
            || self
                .metadata
                .get("wiki")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
    }

    pub fn is_kanban(&self) -> bool {
        self.category.eq_ignore_ascii_case("kanban") || self.path.starts_with("/kanban/")
    }

    pub fn is_handoff(&self) -> bool {
        self.category.eq_ignore_ascii_case("handoff") || self.path.starts_with("/handoff/")
    }

    pub fn is_foundry_distill(&self) -> bool {
        self.source.eq_ignore_ascii_case("foundry_distill")
    }

    pub fn is_guide(&self) -> bool {
        self.category.eq_ignore_ascii_case("guide")
            || self.path == "/guide"
            || self.path.starts_with("/guide/")
            || self
                .metadata
                .get("guide")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
    }

    pub fn guide_type(&self) -> Option<&str> {
        self.metadata
            .get("guide_type")
            .and_then(serde_json::Value::as_str)
    }

    pub fn file_patterns(&self) -> Vec<String> {
        metadata_string_array(&self.metadata, "file_patterns")
    }

    pub fn error_patterns(&self) -> Vec<String> {
        metadata_string_array(&self.metadata, "error_patterns")
    }

    /// Fold legacy `persons` into `entities` and clear `persons` before persisting.
    /// All new write paths should call this (or rely on `memory_crud` upsert).
    pub fn fold_persons_into_entities(&mut self) {
        let persons: Vec<String> = self.persons.drain(..).collect();
        fold_person_names_into_entities(&mut self.entities, persons);
    }

    /// Fold legacy `location` into `path` / metadata before persisting (schema v9).
    pub fn fold_location_into_metadata(&mut self) {
        fold_location_into_metadata(self);
    }
}

/// Push a named entity for recall; `Kyle` also adds canonical `user`.
pub fn push_entity_name(entities: &mut Vec<String>, name: &str) {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return;
    }
    if entities.iter().any(|e| e.eq_ignore_ascii_case(trimmed)) {
        return;
    }
    entities.push(trimmed.to_string());
    if trimmed.eq_ignore_ascii_case("kyle") {
        push_entity_name(entities, "user");
    }
}

/// Merge legacy `persons` JSON names into `entities` (used by migrations and upsert).
pub fn fold_person_names_into_entities(
    entities: &mut Vec<String>,
    persons: impl IntoIterator<Item = String>,
) {
    for name in persons {
        push_entity_name(entities, &name);
    }
}

/// True when legacy `location` looks like a hierarchical or file path, not a place name.
pub fn is_path_like_location(location: &str) -> bool {
    let t = location.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with('/')
        || t.starts_with("./")
        || t.starts_with("../")
        || t.starts_with("~/")
        || t.starts_with("\\\\")
        || t.get(1..3).is_some_and(|s| s == ":/" || s == ":\\")
    {
        return true;
    }

    t.rsplit(['/', '\\']).next().is_some_and(|tail| {
        let Some((stem, ext)) = tail.rsplit_once('.') else {
            return false;
        };
        !stem.is_empty()
            && !ext.is_empty()
            && ext.len() <= 8
            && stem
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// Relocate legacy `location` into `path` or `metadata` before persisting (schema v9).
///
/// Returns the effective path after relocation.
pub fn apply_location_relocation(
    path: &str,
    location: &str,
    metadata: &mut serde_json::Value,
) -> String {
    let loc = location.trim();
    let mut effective_path = path.trim().to_string();
    if loc.is_empty() {
        return effective_path;
    }
    if is_path_like_location(loc) {
        if effective_path.is_empty() || effective_path == "/" {
            effective_path = crate::path_router::normalize_path(loc);
        } else {
            let obj = ensure_metadata_object(metadata);
            obj.entry("context_path")
                .or_insert_with(|| serde_json::Value::String(loc.to_string()));
        }
    } else {
        let obj = ensure_metadata_object(metadata);
        obj.entry("geo")
            .or_insert_with(|| serde_json::Value::String(loc.to_string()));
    }
    effective_path
}

fn ensure_metadata_object(
    metadata: &mut serde_json::Value,
) -> &mut serde_json::Map<String, serde_json::Value> {
    if !metadata.is_object() {
        let previous =
            std::mem::replace(metadata, serde_json::Value::Object(serde_json::Map::new()));
        if let serde_json::Value::Object(obj) = metadata {
            obj.insert("legacy_metadata".to_string(), previous);
        }
    }
    match metadata {
        serde_json::Value::Object(obj) => obj,
        _ => unreachable!("metadata was normalized to an object"),
    }
}

/// Fold wire/API `location` into storage fields and clear the legacy column value.
pub fn fold_location_into_metadata(entry: &mut MemoryEntry) {
    let location = std::mem::take(&mut entry.location);
    entry.path = apply_location_relocation(&entry.path, &location, &mut entry.metadata);
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
pub(super) fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}
pub fn default_tier() -> String {
    "raw".to_string()
}

fn metadata_string_array(metadata: &serde_json::Value, key: &str) -> Vec<String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
