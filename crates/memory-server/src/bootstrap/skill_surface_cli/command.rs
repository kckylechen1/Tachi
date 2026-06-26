use std::path::Path;

use crate::cli::SkillSurfaceAction;

use super::print::{print_skill_source_report, print_skill_surface_report};
use super::projection::{build_cc_switch_projection_status, read_cc_switch_skills};
use super::sources::build_skill_source_report;
use super::stores::{build_drift_groups, scan_skill_store, skill_store_specs};
use super::sync_plan::{build_skill_source_sync_plan, print_skill_source_sync_plan};
use super::*;

pub(in crate::bootstrap) async fn run_skill_surface_command(
    action: SkillSurfaceAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        SkillSurfaceAction::Status { hosts, home, json } => {
            let home = crate::utils::resolve_home_arg(home)?;
            let report = build_skill_surface_report(&home, &hosts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_skill_surface_report(&report);
            }
            Ok(())
        }
        SkillSurfaceAction::Sources { json } => {
            let report = build_skill_source_report()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_skill_source_report(&report);
            }
            Ok(())
        }
        SkillSurfaceAction::SyncPlan { json } => {
            let report = build_skill_source_sync_plan()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_skill_source_sync_plan(&report);
            }
            Ok(())
        }
    }
}

pub(super) fn build_skill_surface_report(
    home: &Path,
    host_filters: &[String],
) -> Result<SkillSurfaceReport, String> {
    let hosts = crate::utils::normalize_supported_values(
        host_filters,
        SUPPORTED_HOSTS,
        "skill-surface host",
    )?;
    let specs = skill_store_specs(home, &hosts);
    let mut stores = Vec::new();
    let mut entries = Vec::new();

    for spec in &specs {
        let (summary, mut scanned) = scan_skill_store(spec);
        stores.push(summary);
        entries.append(&mut scanned);
    }

    let drift_groups = build_drift_groups(&entries);
    let cc_switch_db = home.join(".cc-switch").join("cc-switch.db");
    let cc_switch_skills = read_cc_switch_skills(&cc_switch_db)?;
    let projection_status =
        build_cc_switch_projection_status(home, &hosts, &cc_switch_skills, &entries);

    let mut summary = SkillSurfaceSummary {
        stores: stores.len(),
        drift_groups: drift_groups.len(),
        cc_switch_skills: cc_switch_skills.len(),
        ..SkillSurfaceSummary::default()
    };
    for store in &stores {
        summary.entries += store.entries;
        summary.symlinks += store.symlinks;
        summary.broken_symlinks += store.broken_symlinks;
    }
    summary.cc_switch_projection_issues = projection_status
        .iter()
        .map(|status| status.issues.len())
        .sum();

    Ok(SkillSurfaceReport {
        schema_version: "tachi.skill_surface.status.v1".to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        home: home.display().to_string(),
        hosts: hosts.iter().map(|h| h.to_string()).collect(),
        summary,
        stores,
        entries,
        drift_groups,
        cc_switch_db: cc_switch_db
            .exists()
            .then(|| cc_switch_db.display().to_string()),
        cc_switch_skills,
        cc_switch_projection_status: projection_status,
    })
}
