use serde::Serialize;
use std::collections::BTreeMap;

/// A row pulled from the source DB, normalised for routing.
#[derive(Debug, Clone)]
pub struct SourceRow {
    pub id: String,
    pub path: String,
    pub summary: String,
    pub text: String,
    pub importance: f64,
    pub timestamp: String,
    pub category: String,
    pub topic: String,
    pub keywords: String,
    pub persons: String,
    pub entities: String,
    pub location: String,
    pub source: String,
    pub scope: String,
    pub archived: i64,
    pub created_at: String,
    pub updated_at: String,
    pub access_count: i64,
    pub last_access: Option<String>,
    pub metadata: String,
    pub revision: i64,
}

/// Rescue routing decision for a single row.
#[derive(Debug, Clone, Serialize)]
pub struct RescueAssignment {
    pub source_id: String,
    pub source_path: String,
    /// Target project DB short name (e.g. "hapi", "quant", "antigravity").
    pub target: String,
    /// Reason / matched rule (for diff-readability).
    pub reason: String,
    /// Whether this assignment is a trading isolation row (domain override).
    pub trading: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RescuePlan {
    pub source_path: String,
    pub source_total: usize,
    /// Per-target row counts.
    pub per_target: BTreeMap<String, usize>,
    /// All routing decisions in source order.
    pub assignments: Vec<RescueAssignment>,
    /// Rows that the classifier explicitly punted on (currently 0 — fallback
    /// always routes to `antigravity`).
    pub unrouted: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RescueApplyReport {
    pub plan: RescuePlan,
    pub written_per_target: BTreeMap<String, usize>,
    pub skipped_existing: usize,
    pub errors: Vec<String>,
    pub source_backed_up_to: Option<String>,
}
