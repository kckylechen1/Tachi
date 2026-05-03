use super::*;

// ─── Dead Letter Queue ──────────────────────────────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct DlqListParams {
    /// Filter by status: "pending", "retrying", "resolved", "abandoned"
    #[serde(default)]
    pub status_filter: Option<String>,
    /// Max entries to return (default: 50)
    #[serde(default)]
    pub limit: Option<usize>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(crate) struct DlqRetryParams {
    /// ID of the dead letter entry to retry
    pub dead_letter_id: String,
}
