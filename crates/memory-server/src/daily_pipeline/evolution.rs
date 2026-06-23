use crate::server_state::MemoryServer;
use crate::tool_params::{
    AgentEvolutionDocumentParams, AgentEvolutionEvidenceParams, SkillEvolveParams,
    SynthesizeAgentEvolutionParams,
};
use memory_core::{HubCapability, MemoryStore};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

use super::{parse_json_or_raw, DailyStageReport, EvalEvidenceRow};

pub(crate) async fn run_agent_evolution_stage(
    server: &MemoryServer,
    app_home: &std::path::Path,
) -> DailyStageReport {
    let agents_dir = app_home.join("agents");
    let agent_dirs = match list_agent_dirs(&agents_dir) {
        Ok(dirs) if !dirs.is_empty() => dirs,
        Ok(_) => {
            return DailyStageReport {
                status: "skipped".to_string(),
                summary: "No agent directories found under TACHI_HOME/agents".to_string(),
                details: json!({ "agents_dir": agents_dir.display().to_string() }),
            };
        }
        Err(e) => {
            return DailyStageReport {
                status: "skipped".to_string(),
                summary: format!("Could not inspect agent directories: {e}"),
                details: json!({ "agents_dir": agents_dir.display().to_string(), "error": e }),
            };
        }
    };

    let eval_rows = match collect_recent_eval_rows(server, 100) {
        Ok(rows) => rows,
        Err(e) => {
            return DailyStageReport {
                status: "failed".to_string(),
                summary: format!("Could not load recent eval evidence: {e}"),
                details: json!({ "error": e }),
            };
        }
    };
    let mut results = Vec::new();
    let mut completed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for dir in agent_dirs {
        let agent_id = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let documents = match collect_agent_documents(&dir) {
            Ok(docs) => docs,
            Err(e) => {
                failed += 1;
                results.push(json!({ "agent_id": agent_id, "status": "failed", "error": e }));
                continue;
            }
        };
        let evidence = evidence_for_agent(&agent_id, &eval_rows);

        if documents.is_empty() || evidence.len() < 2 {
            skipped += 1;
            results.push(json!({
                "agent_id": agent_id,
                "status": "skipped",
                "document_count": documents.len(),
                "evidence_count": evidence.len(),
                "reason": "insufficient documents or new eval evidence"
            }));
            continue;
        }

        let params = SynthesizeAgentEvolutionParams {
            agent_id: agent_id.clone(),
            display_name: Some(agent_id.clone()),
            documents,
            document_paths: Vec::new(),
            evidence,
            evidence_paths: Vec::new(),
            memory_queries: Vec::new(),
            goals: vec![
                "Use recent eval evidence to propose conservative durable profile improvements."
                    .to_string(),
            ],
            dry_run: false,
        };

        match crate::foundry_ops::handle_synthesize_agent_evolution(server, params).await {
            Ok(raw) => {
                completed += 1;
                results.push(json!({
                    "agent_id": agent_id,
                    "status": "completed",
                    "result": parse_json_or_raw(&raw)
                }));
            }
            Err(e) => {
                failed += 1;
                results.push(json!({ "agent_id": agent_id, "status": "failed", "error": e }));
            }
        }
    }

    let status = if failed > 0 {
        "degraded"
    } else if completed > 0 {
        "completed"
    } else {
        "skipped"
    };

    DailyStageReport {
        status: status.to_string(),
        summary: format!(
            "Agent evolution completed={completed}, skipped={skipped}, failed={failed}"
        ),
        details: json!({ "results": results }),
    }
}

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

fn collect_recent_eval_rows(
    server: &MemoryServer,
    limit: usize,
) -> Result<Vec<EvalEvidenceRow>, String> {
    let collect = |store: &mut MemoryStore| -> Result<Vec<EvalEvidenceRow>, String> {
        let mut stmt = store
            .connection()
            .prepare(
                "SELECT id, path, summary, text, metadata, created_at
                 FROM memories
                 WHERE path LIKE '/eval/%'
                   AND created_at > datetime('now', '-7 day')
                 ORDER BY created_at DESC
                 LIMIT ?1",
            )
            .map_err(|e| format!("prepare eval evidence query: {e}"))?;
        let rows = stmt
            .query_map([limit as i64], |row| {
                let metadata_raw: String = row.get(4)?;
                Ok(EvalEvidenceRow {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    summary: row.get(2)?,
                    text: row.get(3)?,
                    metadata: serde_json::from_str(&metadata_raw).unwrap_or_else(|_| json!({})),
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("query eval evidence: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("read eval evidence row: {e}"))?);
        }
        Ok(out)
    };

    let mut rows = server
        .with_global_store_read(collect)
        .map_err(|e| format!("collect global recent eval evidence: {e}"))?;
    if server.has_project_db() {
        let mut project_rows = server
            .with_project_store_read(collect)
            .map_err(|e| format!("collect project recent eval evidence: {e}"))?;
        rows.append(&mut project_rows);
    }
    Ok(rows)
}

fn evidence_for_agent(
    agent_id: &str,
    rows: &[EvalEvidenceRow],
) -> Vec<AgentEvolutionEvidenceParams> {
    rows.iter()
        .filter(|row| {
            row.metadata
                .get("agent")
                .and_then(Value::as_str)
                .map(|agent| agent.eq_ignore_ascii_case(agent_id))
                .unwrap_or_else(|| {
                    row.text.contains(agent_id)
                        || row.summary.contains(agent_id)
                        || row.path.contains(agent_id)
                })
        })
        .take(10)
        .map(|row| AgentEvolutionEvidenceParams {
            kind: "eval".to_string(),
            title: Some(row.summary.clone()),
            content: format!("created_at: {}\n{}", row.created_at, row.text),
            source_ref: Some(row.id.clone()),
            path: Some(row.path.clone()),
            weight: 1.0,
        })
        .collect()
}

fn collect_agent_documents(
    dir: &std::path::Path,
) -> Result<Vec<AgentEvolutionDocumentParams>, String> {
    let candidates = [
        ("IDENTITY.md", "identity"),
        ("AGENTS.md", "agents"),
        ("LATEST_TRUTHS.md", "latest_truths"),
        ("routing_policy.md", "routing_policy"),
        ("tool_policy.md", "tool_policy"),
        ("memory_policy.md", "memory_policy"),
    ];
    let mut docs = Vec::new();
    for (file_name, kind) in candidates {
        let path = dir.join(file_name);
        if !path.is_file() {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("read agent document {}: {e}", path.display()))?;
        if content.trim().is_empty() {
            continue;
        }
        docs.push(AgentEvolutionDocumentParams {
            kind: kind.to_string(),
            path: Some(path.display().to_string()),
            content,
        });
    }
    Ok(docs)
}

fn list_agent_dirs(agents_dir: &std::path::Path) -> Result<Vec<PathBuf>, String> {
    if !agents_dir.exists() {
        return Ok(Vec::new());
    }
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(agents_dir)
        .map_err(|e| format!("read_dir {}: {e}", agents_dir.display()))?
    {
        let entry = entry.map_err(|e| format!("read agent dir entry: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}
