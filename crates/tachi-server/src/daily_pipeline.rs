mod health;
mod maintenance;
mod report;
mod routing;
mod schedule;
mod types;

use crate::server_state::MemoryServer;

use health::{load_manifest_targets, run_health_check};
use maintenance::run_truth_maintenance_stage;
use report::{
    daily_report_payload_path, publish_daily_report_with_retry, render_daily_report_markdown,
    save_daily_health_wiki, serialize_daily_json_section,
};
use routing::run_routing_analysis_stage;
use schedule::shanghai_today;

#[cfg(test)]
use health::collect_database_stats_for_targets;
#[cfg(test)]
pub(crate) use health::run_health_check as run_health_check_for_tests;
#[cfg(test)]
use maintenance::resolve_truth_maintenance_route_for_paths_in_home;
pub(crate) use report::parse_llm_json;
pub(crate) use report::validated_latest_daily_report;
#[cfg(test)]
pub(crate) use report::{
    build_daily_model_invocations_sidecar as build_daily_sidecar_for_tests,
    daily_report_generation_sidecar_path as daily_report_generation_sidecar_path_for_tests,
    next_daily_report_revision as next_daily_report_revision_for_tests,
    publish_daily_report_pair as publish_daily_report_pair_for_tests,
    publish_daily_report_pair_with_failure as publish_daily_report_pair_with_failure_for_tests,
    publish_daily_report_with_retry as publish_daily_report_with_retry_for_tests,
    publish_daily_report_with_retry_after_first_allocation as publish_daily_report_with_retry_after_first_allocation_for_tests,
    render_daily_report_markdown as render_daily_report_markdown_for_tests,
    serialize_daily_json_section as serialize_daily_json_section_for_tests,
    DailyPublishFailurePoint,
};
pub(crate) use schedule::{next_daily_run_time, next_weekly_rem_run_time};
pub(crate) use types::{
    CategorySourceCount, DailyHealthPayload, DailyPipelineReport, DailyStageReport, DatabaseStats,
    DuplicateSummary, EvalEvidenceRow, ManifestDbTarget, TruthMaintenanceRoute,
};

pub(crate) async fn run_daily_pipeline(
    server: &MemoryServer,
) -> Result<DailyPipelineReport, String> {
    let date = shanghai_today();
    let app_home = server.tachi_home_dir();
    let global_db_path = server.global_db_path_buf();
    if let Err(e) =
        crate::status_ops::status_health::refresh_provider_probe_cache(&app_home, &global_db_path)
            .await
    {
        eprintln!("[daily_pipeline] provider key probe cache refresh skipped: {e}");
    }
    let (health_stage, health_json, report_path, health_invocation) =
        run_health_check(server, &app_home, &date).await?;
    let truth_maintenance = run_truth_maintenance_stage(server, &app_home).await;

    let routing_outcome = run_routing_analysis_stage(server, &date).await?;

    let report = DailyPipelineReport {
        date: date.clone(),
        report_path: Some(report_path.display().to_string()),
        health_check: health_stage,
        truth_maintenance,
        routing_analysis: routing_outcome.report,
    };

    let health_section = serialize_daily_json_section(&health_json)?;
    let truth_section = serialize_daily_json_section(&report.truth_maintenance.details)?;
    let routing_section = serialize_daily_json_section(&report.routing_analysis.details)?;
    let markdown =
        render_daily_report_markdown(&report, &health_section, &truth_section, &routing_section);

    let (revision, sidecar) = publish_daily_report_with_retry(
        &report_path,
        &date,
        &markdown,
        &health_section,
        &routing_section,
        &health_invocation,
        routing_outcome.invocation.as_ref(),
    )?;
    let payload_path = daily_report_payload_path(&report_path, revision);

    let health_body = health_section.clone();
    let wiki_invocation = sidecar
        .health
        .clone()
        .ok_or_else(|| "daily health sidecar missing health invocation".to_string())?;
    save_daily_health_wiki(server, &date, &health_body, wiki_invocation).await?;

    let mut report = report;
    report.report_path = Some(payload_path.display().to_string());

    Ok(report)
}

#[cfg(test)]
mod tests;
