mod health;
mod maintenance;
mod report;
mod routing;
mod schedule;
mod types;

use crate::server_state::MemoryServer;

use health::{collect_health_stats, load_manifest_targets};
use maintenance::run_truth_maintenance_stage;
use routing::collect_routing_stats;
use schedule::shanghai_today;

#[cfg(test)]
use health::collect_database_stats_for_targets;
#[cfg(test)]
use maintenance::resolve_truth_maintenance_route_for_paths_in_home;
pub(crate) use report::validated_latest_daily_report;
pub(crate) use schedule::next_daily_run_time;
pub(crate) use types::{
    CategorySourceCount, DailyHealthPayload, DailyPipelineReport, DailyStageReport, DatabaseStats,
    DuplicateSummary, EvalEvidenceRow, ManifestDbTarget, TruthMaintenanceRoute,
};

pub(crate) async fn run_daily_pipeline(
    server: &MemoryServer,
) -> Result<DailyPipelineReport, String> {
    let date = shanghai_today();
    let app_home = server.tachi_home_dir();
    // Maintenance must run even when a later read-only statistics collection fails.
    let truth_maintenance = run_truth_maintenance_stage(server, &app_home).await;
    let database_stats = collect_health_stats(server, &app_home, &date).await?;
    let routing_stats = collect_routing_stats(server, &date)?;

    Ok(DailyPipelineReport {
        date,
        database_stats,
        truth_maintenance,
        routing_stats,
    })
}

#[cfg(test)]
mod tests;
