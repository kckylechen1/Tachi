use crate::server_state::DbScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DailyPipelineReport {
    pub date: String,
    pub report_path: Option<String>,
    pub health_check: DailyStageReport,
    pub truth_maintenance: DailyStageReport,
    pub skill_evolution: DailyStageReport,
    pub routing_analysis: DailyStageReport,
}

impl DailyPipelineReport {
    pub(crate) fn summary(&self) -> String {
        format!(
            "date={} health={} truth_maintenance={} skill_evolution={} routing={}",
            self.date,
            self.health_check.status,
            self.truth_maintenance.status,
            self.skill_evolution.status,
            self.routing_analysis.status
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DailyStageReport {
    pub status: String,
    pub summary: String,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DailyHealthPayload {
    pub(crate) date: String,
    pub(crate) generated_at: String,
    pub(crate) manifest_path: String,
    pub(crate) databases: Vec<DatabaseStats>,
}

#[derive(Debug, Clone)]
pub(crate) struct ManifestDbTarget {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) path: PathBuf,
    pub(crate) role: String,
    pub(crate) owner: String,
    pub(crate) schema_kind: String,
    pub(crate) allow_write: bool,
    pub(crate) last_classification: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TruthMaintenanceRoute {
    pub(crate) target_db: DbScope,
    pub(crate) named_project: Option<String>,
    pub(crate) db_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DatabaseStats {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) role: String,
    pub(crate) owner: String,
    pub(crate) schema_kind: String,
    pub(crate) allow_write: bool,
    pub(crate) last_classification: String,
    pub(crate) total_entries: i64,
    pub(crate) new_today: i64,
    pub(crate) duplicate_count: i64,
    pub(crate) stale_days: i64,
    pub(crate) groups: Vec<CategorySourceCount>,
    pub(crate) duplicate_summaries: Vec<DuplicateSummary>,
    pub(crate) error: Option<String>,
}

pub(crate) use memcore::{
    CategorySourceGroup as CategorySourceCount, DuplicateSummaryRow as DuplicateSummary,
    EvalEvidenceRow,
};
