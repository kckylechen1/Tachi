use super::*;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct ApiKeyRotationMemberStatus {
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
    pub(crate) last_probe_at: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ApiKeyRotationStatus {
    pub(crate) total_keys: i64,
    pub(crate) configured_keys: i64,
    pub(crate) healthy_keys: Option<i64>,
    pub(crate) rate_limited_keys: i64,
    pub(crate) auth_failed_keys: i64,
    pub(crate) current_index: i64,
    pub(crate) strategy: String,
    pub(crate) next_retry_at: Option<String>,
    pub(crate) members: Vec<ApiKeyRotationMemberStatus>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct ApiKeyStatus {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) required: bool,
    pub(crate) deprecated: bool,
    pub(crate) canonical_name: String,
    pub(crate) alias_names: Vec<String>,
    pub(crate) status: String,
    pub(crate) source: String,
    pub(crate) env_configured: bool,
    pub(crate) vault_configured: bool,
    pub(crate) cleanup_hint: Option<String>,
    pub(crate) drift_warning: Option<String>,
    pub(crate) inferred_invalid_provider: Option<String>,
    pub(crate) rotation: Option<ApiKeyRotationStatus>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct StatusSnapshot {
    pub(crate) daemon: DaemonStatus,
    pub(crate) daemon_inventory: Vec<DaemonInventoryEntry>,
    pub(crate) dbs: Vec<DbStatus>,
    pub(crate) manifest_path: String,
    pub(crate) dispatches: Vec<DispatchStatus>,
    pub(crate) recent_evals: Vec<RecentEval>,
    pub(crate) last_daily_report: Option<String>,
    pub(crate) distill_marker: Option<DistillMarkerStatus>,
    pub(crate) api_keys: Vec<ApiKeyStatus>,
    pub(crate) provider_probe_cache: Option<status_health::ProviderProbeCache>,
    pub(crate) project_warnings: Vec<String>,
    pub(crate) plan_c_split_brain: Vec<crate::path_utils::PlanCSplitBrain>,
    pub(crate) health_deductions: Vec<status_health::HealthDeduction>,
    pub(crate) health_score: u8,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DispatchStatus {
    pub(crate) dispatch_id: String,
    pub(crate) agent: String,
    pub(crate) task: String,
    pub(crate) outcome: String,
    pub(crate) elapsed: String,
    pub(crate) reviewed: bool,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct RecentEval {
    pub(crate) task_id: String,
    pub(crate) agent: String,
    pub(crate) outcome: String,
    pub(crate) quality_score: Option<f64>,
    pub(crate) timestamp: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum DaemonStatus {
    Running {
        pid: i32,
        lock_path: PathBuf,
    },
    Foreign {
        pid: i32,
        lock_path: PathBuf,
        reason: String,
        version: Option<String>,
        port: Option<u16>,
        global_db: Option<String>,
    },
    StalePid {
        pid: i32,
        lock_path: PathBuf,
    },
    None,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct DaemonPidInfo {
    pub(crate) pid: Option<i32>,
    pub(crate) port: Option<u16>,
    pub(crate) version: Option<String>,
    pub(crate) global_db: Option<String>,
    pub(crate) project_db: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DaemonInventoryEntry {
    pub(crate) scope: String,
    pub(crate) pid: Option<i32>,
    pub(crate) process_running: bool,
    pub(crate) authoritative_for_current_global: bool,
    pub(crate) state: String,
    pub(crate) reason: Option<String>,
    pub(crate) lock_path: String,
    pub(crate) pid_path: String,
    pub(crate) version: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) global_db: Option<String>,
    pub(crate) project_db: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DbStatus {
    pub(crate) path: String,
    pub(crate) label: String,
    pub(crate) orphan: bool,
    pub(crate) memory_total: usize,
    pub(crate) vector_count: usize,
    pub(crate) vector_missing: usize,
    pub(crate) vector_orphans: usize,
    pub(crate) vector_coverage: f64,
    pub(crate) vector_dimension: Option<usize>,
    pub(crate) namespace: NamespaceHealth,
    pub(crate) continuity: memory_core::ContinuityMetrics,
    pub(crate) pending_enrichment: usize,
    pub(crate) enrichment_failed_recent: usize,
    pub(crate) enrichment_failures: Vec<EnrichmentFailureSummary>,
    pub(crate) pending: usize,
    pub(crate) running: usize,
    pub(crate) active_jobs: usize,
    pub(crate) completed: usize,
    pub(crate) failed: usize,
    pub(crate) dead_lettered: usize,
    pub(crate) skipped: usize,
    pub(crate) terminal_jobs: usize,
    pub(crate) gc_eligible: usize,
    pub(crate) stuck_in_progress: usize,
    pub(crate) latest_active_job: Option<LatestFoundryJob>,
    pub(crate) latest_terminal_job: Option<LatestFoundryJob>,
    pub(crate) latest_job: Option<LatestFoundryJob>,
    pub(crate) latest_failed_job: Option<LatestFailedJob>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct NamespaceHealth {
    pub(crate) recall_cache_rows: usize,
    pub(crate) wiki_rows: usize,
    pub(crate) wiki_non_source_rows: usize,
    pub(crate) wiki_non_category_rows: usize,
    pub(crate) kanban_rows: usize,
    pub(crate) handoff_rows: usize,
    pub(crate) eval_rows: usize,
    pub(crate) project_scope_rows: usize,
    pub(crate) non_project_scope_rows: usize,
    pub(crate) derived_items: usize,
    pub(crate) graph_edges: usize,
    pub(crate) graph_orphan_edges: usize,
    pub(crate) graph_relation_types: Vec<RelationCount>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RelationCount {
    pub(crate) relation: String,
    pub(crate) count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct EnrichmentFailureSummary {
    pub(crate) stage: String,
    pub(crate) last_error: String,
    pub(crate) count: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LatestFoundryJob {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) status: String,
    pub(crate) updated_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LatestFailedJob {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) lane: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) inferred_invalid_provider: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DistillMarkerStatus {
    pub(crate) path: String,
    pub(crate) last_run_at: String,
    pub(crate) age_seconds: i64,
    pub(crate) age: String,
    pub(crate) is_stale: bool,
    /// Last batch's distill-quality summary, parsed from the JSON marker.
    /// `None` for legacy bare-timestamp markers written before this field existed.
    pub(crate) groups_distilled: Option<usize>,
    pub(crate) groups_skipped: Option<usize>,
    pub(crate) fallback_used: Option<usize>,
    /// Hard errors during the last batch (groups that errored without failing the
    /// whole run). These never surface as foundry_jobs rows, so they are the one
    /// distill failure signal that is otherwise invisible to health.
    pub(crate) errors: Option<usize>,
}
