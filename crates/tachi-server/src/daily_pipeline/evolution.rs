use crate::server_state::MemoryServer;
use crate::tool_params::SkillEvolveParams;
use memcore::HubCapability;
use serde_json::json;
use std::collections::HashSet;

use super::{parse_json_or_raw, DailyStageReport};

pub(crate) async fn run_skill_evolution_stage(server: &MemoryServer) -> DailyStageReport {
    let mut skills = Vec::<HubCapability>::new();
    if let Ok(global) = server.with_global_store_read(|store| {
        store
            .hub_list(Some("skill"), false)
            .map_err(|e| format!("hub list global skills: {e}"))
    }) {
        skills.extend(global);
    }
    if server.has_project_db() {
        if let Ok(project) = server.with_project_store_read(|store| {
            store
                .hub_list(Some("skill"), false)
                .map_err(|e| format!("hub list project skills: {e}"))
        }) {
            skills.extend(project);
        }
    }

    let mut seen = HashSet::new();
    let low_health = skills
        .into_iter()
        .filter(|skill| seen.insert(skill.id.clone()))
        .filter(|skill| {
            !skill.health_status.eq_ignore_ascii_case("healthy") || skill.fail_streak > 3
        })
        .collect::<Vec<_>>();

    if low_health.is_empty() {
        return DailyStageReport {
            status: "skipped".to_string(),
            summary: "No low-health skills found".to_string(),
            details: json!({ "skill_count": 0 }),
        };
    }

    let mut results = Vec::new();
    let mut previewed = 0usize;
    let mut failed = 0usize;
    for skill in low_health {
        let params = SkillEvolveParams {
            skill_id: skill.id.clone(),
            feedback: Some(format!(
                "Daily Pipeline dry-run evolution for low-health skill. health_status={}, fail_streak={}",
                skill.health_status, skill.fail_streak
            )),
            auto_activate: false,
            dry_run: true,
        };
        match crate::hub_ops::handle_skill_evolve(server, params).await {
            Ok(raw) => {
                previewed += 1;
                results.push(json!({
                    "skill_id": skill.id,
                    "status": "previewed",
                    "health_status": skill.health_status,
                    "fail_streak": skill.fail_streak,
                    "result": parse_json_or_raw(&raw)
                }));
            }
            Err(e) => {
                failed += 1;
                results.push(json!({
                    "skill_id": skill.id,
                    "status": "failed",
                    "health_status": skill.health_status,
                    "fail_streak": skill.fail_streak,
                    "error": e
                }));
            }
        }
    }

    DailyStageReport {
        status: if failed > 0 { "degraded" } else { "completed" }.to_string(),
        summary: format!("Skill evolution dry-run previews={previewed}, failed={failed}"),
        details: json!({ "results": results }),
    }
}
