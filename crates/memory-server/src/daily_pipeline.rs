mod evolution;
mod health;
mod maintenance;
mod report;
mod routing;
mod schedule;
mod types;

use crate::server_state::MemoryServer;

use evolution::{run_agent_evolution_stage, run_skill_evolution_stage};
use health::{load_manifest_targets, run_health_check};
use maintenance::run_truth_maintenance_stage;
use report::{render_daily_report_markdown, save_daily_health_wiki};
use routing::run_routing_analysis_stage;
use schedule::shanghai_today;

#[cfg(test)]
use health::collect_database_stats_for_targets;
#[cfg(test)]
use maintenance::resolve_truth_maintenance_route_for_paths;

pub(crate) use report::{parse_json_or_raw, parse_llm_json};
pub(crate) use schedule::{next_daily_run_time, next_weekly_rem_run_time};
pub(crate) use types::{
    CategorySourceCount, DailyHealthPayload, DailyPipelineReport, DailyStageReport, DatabaseStats,
    DuplicateSummary, EvalEvidenceRow, ManifestDbTarget, TruthMaintenanceRoute,
};

pub(crate) async fn run_daily_pipeline(
    server: &MemoryServer,
) -> Result<DailyPipelineReport, String> {
    let date = shanghai_today();
    let app_home = crate::path_utils::tachi_home();
    let global_db_path = server.global_db_path_buf();
    if let Err(e) =
        crate::status_ops::status_health::refresh_provider_probe_cache(&app_home, &global_db_path)
            .await
    {
        eprintln!("[daily_pipeline] provider key probe cache refresh skipped: {e}");
    }
    let (health_stage, health_json, report_path) =
        run_health_check(server, &app_home, &date).await?;
    if let Err(e) = run_truth_maintenance_stage(server, &app_home).await {
        eprintln!("[daily_pipeline] truth maintenance skipped: {e}");
    }

    // ── SFT Factory: generate fine-tuning dialogues from distilled memories ───
    if let Err(e) =
        crate::foundry_runtime_ops::sft_factory::run_daily_sft_distillation(server).await
    {
        eprintln!("[daily_pipeline] SFT factory skipped: {e}");
    }
    let agent_stage = run_agent_evolution_stage(server, &app_home).await;
    let skill_stage = run_skill_evolution_stage(server).await;
    let routing_stage = run_routing_analysis_stage(server, &date).await;

    let report = DailyPipelineReport {
        date: date.clone(),
        report_path: Some(report_path.display().to_string()),
        health_check: health_stage,
        agent_evolution: agent_stage,
        skill_evolution: skill_stage,
        routing_analysis: routing_stage,
    };

    let markdown = render_daily_report_markdown(&report, &health_json);
    if let Some(parent) = report_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("create daily report dir: {e}"))?;
    }
    tokio::fs::write(&report_path, &markdown)
        .await
        .map_err(|e| format!("write daily report: {e}"))?;

    save_daily_health_wiki(server, &date, &markdown).await?;

    Ok(report)
}

#[cfg(test)]
mod tests;
