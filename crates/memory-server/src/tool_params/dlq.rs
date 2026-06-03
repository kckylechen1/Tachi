use super::*;

// ─── Dead Letter Queue ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct DlqListParams {
    /// Filter by status: "pending", "retrying", "resolved", "abandoned"
    #[serde(default)]
    pub status_filter: Option<String>,
    /// Max entries to return (default: 50)
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct DlqRetryParams {
    /// ID of the dead letter entry to retry
    pub dead_letter_id: String,
}
