use crate::server_state::MemoryServer;
use memcore::HubCapability;
use serde_json::json;
use std::collections::HashSet;

use super::DailyStageReport;

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
        .filter(|skill| !crate::builtins::is_retired_builtin_capability_id(&skill.id))
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

    let results: Vec<_> = low_health
        .iter()
        .map(|skill| {
            json!({
                "skill_id": skill.id,
                "status": "degraded",
                "health_status": skill.health_status,
                "fail_streak": skill.fail_streak,
            })
        })
        .collect();

    DailyStageReport {
        status: "completed".to_string(),
        summary: format!(
            "Skill health audit identified {} degraded skills",
            low_health.len()
        ),
        details: json!({ "degraded_skills": results }),
    }
}
