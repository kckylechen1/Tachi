use super::execute_registered_skill_prompt;
use crate::tool_params::DistillTrajectoryParams;
use crate::utils::sanitize_safe_path_name;
use crate::{DbScope, MemoryServer};
use chrono::Utc;
use memcore::{HubCapability, MemoryEntry, MemoryStore};
use serde_json::{json, Value};
use tachi_hub::should_expose_skill_tool;

pub(crate) async fn handle_distill_trajectory(
    server: &MemoryServer,
    params: DistillTrajectoryParams,
) -> Result<String, String> {
    let named_project = params.project.clone();
    let (target_db, warning) = if named_project.is_some() {
        (DbScope::Project, None)
    } else {
        server.resolve_write_scope(&params.scope)
    };
    // #1041 S2: shares `pipeline_ops::helpers::resolve_domain`'s fix — an
    // absent domain classifies to `general`, it no longer inherits the
    // daemon-wide `TACHI_DOMAIN` env var (see that function's doc for why).
    let domain = crate::pipeline_ops::helpers::resolve_domain(params.domain.clone());
    let distilled_execution = execute_registered_skill_prompt(
        server,
        "skill:trajectory-distiller",
        &json!({
            "task_description": params.task_description,
            "execution_trace": params.execution_trace,
            "final_outcome": params.final_outcome,
            "agent_id": params.agent_id,
            "skill_path": params.skill_path,
            "domain": domain,
        }),
    )
    .await?;
    let distilled_markdown = distilled_execution.output;

    let timestamp = Utc::now().to_rfc3339();
    let skill_id = params.skill_id.clone().unwrap_or_else(|| {
        format!(
            "skill:{}",
            sanitize_safe_path_name(params.skill_path.trim_matches('/'))
        )
    });
    let snapshot_path = format!(
        "{}/distilled/{}",
        params.skill_path.trim_end_matches('/'),
        Utc::now().format("%Y%m%dT%H%M%S")
    );
    let importance = params.importance.unwrap_or(0.85).clamp(0.0, 1.0);
    let snapshot_metadata = crate::provenance::inject_provenance(
        server,
        json!({
            "skill_id": skill_id,
            "task_description": params.task_description,
            "execution_trace": params.execution_trace,
            "final_outcome": params.final_outcome,
            "agent_id": params.agent_id,
            "source_skill": "skill:trajectory-distiller",
        }),
        "distill_trajectory",
        "trajectory_distill",
        Some(params.scope.as_str()),
        target_db,
        json!({
            "skill_path": params.skill_path,
            "domain": domain,
        }),
    );
    let snapshot_entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        path: snapshot_path.clone(),
        summary: distilled_markdown.chars().take(100).collect(),
        text: distilled_markdown.clone(),
        importance,
        timestamp: timestamp.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "decision".to_string(),
        topic: sanitize_safe_path_name(params.skill_path.trim_matches('/')),
        keywords: vec![
            "trajectory".to_string(),
            "skill".to_string(),
            "distilled".to_string(),
        ],
        persons: vec![],
        entities: vec![skill_id.clone()],
        location: String::new(),
        source: "distill_trajectory".to_string(),
        scope: params.scope.clone(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: snapshot_metadata,
        vector: None,
        retention_policy: Some("permanent".to_string()),
        domain: domain.clone(),
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    };

    let prior_snapshot = {
        let read_action = |store: &mut MemoryStore| {
            store
                .list_by_path(&format!("{}/distilled", params.skill_path), 8, false)
                .map_err(|e| format!("list distilled snapshots: {e}"))
        };
        let entries = if let Some(project_name) = named_project.as_deref() {
            server.with_named_project_store_read(project_name, read_action)?
        } else {
            server.with_store_for_scope_read(target_db, read_action)?
        };
        entries
            .into_iter()
            .filter(|entry| entry.id != snapshot_entry.id)
            .max_by(|a, b| a.timestamp.cmp(&b.timestamp))
    };

    if let Some(project_name) = named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .upsert(&snapshot_entry)
                .map_err(|e| format!("save distilled snapshot: {e}"))
        })?;
    } else {
        server.with_store_for_scope(target_db, |store| {
            store
                .upsert(&snapshot_entry)
                .map_err(|e| format!("save distilled snapshot: {e}"))
        })?;
    }

    if let Some(previous) = prior_snapshot {
        let edge = memcore::MemoryEdge {
            source_id: snapshot_entry.id.clone(),
            target_id: previous.id,
            relation: "follows".to_string(),
            weight: 0.8,
            metadata: json!({ "source": "distill_trajectory" }),
            created_at: timestamp.clone(),
            valid_from: String::new(),
            valid_to: None,
        };
        let save_edge = |store: &mut MemoryStore| store.add_edge(&edge).map_err(|e| format!("{e}"));
        if let Some(project_name) = named_project.as_deref() {
            let _ = server.with_named_project_store(project_name, save_edge);
        } else {
            let _ = server.with_store_for_scope(target_db, save_edge);
        }
    }

    let (prior_cap, cap_scope_label) = if let Some(project_name) = named_project.as_deref() {
        (
            server.with_named_project_store_read(project_name, |store| {
                store
                    .hub_get(&skill_id)
                    .map_err(|e| format!("hub get named project: {e}"))
            })?,
            "project",
        )
    } else if target_db == DbScope::Project {
        (
            server.with_store_for_scope_read(target_db, |store| {
                store
                    .hub_get(&skill_id)
                    .map_err(|e| format!("hub get project: {e}"))
            })?,
            "project",
        )
    } else {
        (
            server.with_global_store_read(|store| {
                store
                    .hub_get(&skill_id)
                    .map_err(|e| format!("hub get global: {e}"))
            })?,
            "global",
        )
    };
    let new_version = prior_cap.as_ref().map(|cap| cap.version + 1).unwrap_or(1);
    let skill_definition = json!({
        "system": "You are executing a distilled reusable skill. Follow the skill document closely and adapt it to the user's input.",
        "prompt": format!("Skill document:\\n\\n{}\\n\\nUser input:\\n{{{{input}}}}", distilled_markdown),
        "content": distilled_markdown,
        "policy": { "visibility": "listed" },
        "skill_path": params.skill_path,
        "domain": domain,
        "retention_policy": "permanent",
        "source": "distill_trajectory",
        "provenance": {
            "agent_id": params.agent_id,
            "final_outcome": params.final_outcome,
        },
        "tags": ["distilled", "trajectory", "permanent"],
    });
    let capability = HubCapability {
        id: skill_id.clone(),
        cap_type: "skill".to_string(),
        name: params
            .skill_path
            .trim_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("distilled-skill")
            .to_string(),
        version: new_version,
        description: format!("Distilled skill for {}", params.skill_path),
        definition: serde_json::to_string(&skill_definition)
            .map_err(|e| format!("serialize distilled skill: {e}"))?,
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: prior_cap.as_ref().and_then(|cap| cap.last_error.clone()),
        last_success_at: prior_cap
            .as_ref()
            .and_then(|cap| cap.last_success_at.clone()),
        last_failure_at: prior_cap
            .as_ref()
            .and_then(|cap| cap.last_failure_at.clone()),
        fail_streak: prior_cap.as_ref().map(|cap| cap.fail_streak).unwrap_or(0),
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: prior_cap.as_ref().map(|cap| cap.uses).unwrap_or(0),
        successes: prior_cap.as_ref().map(|cap| cap.successes).unwrap_or(0),
        failures: prior_cap.as_ref().map(|cap| cap.failures).unwrap_or(0),
        avg_rating: prior_cap.as_ref().map(|cap| cap.avg_rating).unwrap_or(0.5),
        last_used: prior_cap.as_ref().and_then(|cap| cap.last_used.clone()),
        created_at: prior_cap
            .as_ref()
            .map(|cap| cap.created_at.clone())
            .unwrap_or(timestamp.clone()),
        updated_at: timestamp,
    };

    if let Some(project_name) = named_project.as_deref() {
        server.with_named_project_store(project_name, |store| {
            store
                .hub_register(&capability)
                .map_err(|e| format!("register distilled skill: {e}"))
        })?;
    } else {
        server.with_store_for_scope(target_db, |store| {
            store
                .hub_register(&capability)
                .map_err(|e| format!("register distilled skill: {e}"))
        })?;
    }
    if should_expose_skill_tool(&capability) {
        let _ = server.register_skill_tool(&capability);
    }
    let skill_quality = crate::wiki_ops::refresh_skill_quality_guards(server)?;

    let mut response = serde_json::Map::new();
    response.insert("status".into(), json!("completed"));
    response.insert("skill_id".into(), json!(skill_id));
    response.insert("version".into(), json!(new_version));
    response.insert("snapshot_id".into(), json!(snapshot_entry.id));
    response.insert("snapshot_path".into(), json!(snapshot_path));
    response.insert("capability_scope".into(), json!(cap_scope_label));
    response.insert("db".into(), json!(target_db.as_str()));
    response.insert("skill_quality".into(), skill_quality);
    if let Some(warning) = warning {
        response.insert("warning".into(), json!(warning));
    }
    serde_json::to_string(&Value::Object(response)).map_err(|e| format!("serialize: {e}"))
}
