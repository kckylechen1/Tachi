use super::*;
use chrono::{Datelike, Duration as ChronoDuration, FixedOffset, TimeZone};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DailyPipelineReport {
    pub date: String,
    pub report_path: Option<String>,
    pub health_check: DailyStageReport,
    pub agent_evolution: DailyStageReport,
    pub skill_evolution: DailyStageReport,
    pub routing_analysis: DailyStageReport,
}

impl DailyPipelineReport {
    pub(crate) fn summary(&self) -> String {
        format!(
            "date={} health={} agent_evolution={} skill_evolution={} routing={}",
            self.date,
            self.health_check.status,
            self.agent_evolution.status,
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
struct DailyHealthPayload {
    date: String,
    generated_at: String,
    manifest_path: String,
    databases: Vec<DatabaseStats>,
}

#[derive(Debug, Clone)]
struct ManifestDbTarget {
    name: String,
    label: String,
    path: PathBuf,
    role: String,
    owner: String,
    schema_kind: String,
    allow_write: bool,
    last_classification: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TruthMaintenanceRoute {
    target_db: DbScope,
    named_project: Option<String>,
    db_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
struct DatabaseStats {
    name: String,
    path: String,
    role: String,
    owner: String,
    schema_kind: String,
    allow_write: bool,
    last_classification: String,
    total_entries: i64,
    new_today: i64,
    duplicate_count: i64,
    stale_days: i64,
    groups: Vec<CategorySourceCount>,
    duplicate_summaries: Vec<DuplicateSummary>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CategorySourceCount {
    count: i64,
    category: String,
    source: String,
}

#[derive(Debug, Clone, Serialize)]
struct DuplicateSummary {
    summary: String,
    count: i64,
}

#[derive(Debug, Clone)]
struct EvalEvidenceRow {
    id: String,
    path: String,
    summary: String,
    text: String,
    metadata: Value,
    created_at: String,
}

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

pub(crate) fn next_daily_run_time() -> tokio::time::Instant {
    let tz = shanghai_offset();
    let now_utc = Utc::now();
    let now_local = now_utc.with_timezone(&tz);
    let today_0400 = tz
        .with_ymd_and_hms(
            now_local.year(),
            now_local.month(),
            now_local.day(),
            4,
            0,
            0,
        )
        .single()
        .unwrap_or(now_local);
    let next_local = if now_local < today_0400 {
        today_0400
    } else {
        today_0400 + ChronoDuration::days(1)
    };
    let wait = (next_local.with_timezone(&Utc) - now_utc)
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(0));
    tokio::time::Instant::now() + wait
}

/// Returns the instant for the next Sunday 05:00 Asia/Shanghai.
/// Used by the weekly REM wiki evolver loop.
pub(crate) fn next_weekly_rem_run_time() -> tokio::time::Instant {
    let tz = shanghai_offset();
    let now_utc = Utc::now();
    let now_local = now_utc.with_timezone(&tz);

    // chrono: weekday().num_days_from_sunday() gives 0 for Sunday.
    let days_until_sunday = {
        let wd = now_local.weekday().num_days_from_sunday() as i64;
        if wd == 0 {
            0i64
        } else {
            7 - wd
        }
    };
    let candidate_date = now_local.date_naive() + ChronoDuration::days(days_until_sunday);
    let candidate = tz
        .with_ymd_and_hms(
            candidate_date.year(),
            candidate_date.month(),
            candidate_date.day(),
            5,
            0,
            0,
        )
        .single()
        .unwrap_or(now_local);

    // If we already passed Sunday 05:00 this week, schedule for next Sunday.
    let next_local = if now_local < candidate {
        candidate
    } else {
        let next_date = candidate_date + ChronoDuration::days(7);
        tz.with_ymd_and_hms(
            next_date.year(),
            next_date.month(),
            next_date.day(),
            5,
            0,
            0,
        )
        .single()
        .unwrap_or(now_local)
    };

    let wait = (next_local.with_timezone(&Utc) - now_utc)
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(0));
    tokio::time::Instant::now() + wait
}

async fn run_health_check(
    server: &MemoryServer,
    app_home: &std::path::Path,
    date: &str,
) -> Result<(DailyStageReport, Value, PathBuf), String> {
    let manifest_path = app_home.join("manifest.json");
    let targets = load_manifest_targets(server, &manifest_path)?;
    let databases = collect_database_stats_for_targets(targets).await?;

    let payload = DailyHealthPayload {
        date: date.to_string(),
        generated_at: Utc::now().to_rfc3339(),
        manifest_path: manifest_path.display().to_string(),
        databases,
    };
    let user = serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("serialize daily health payload: {e}"))?;
    let raw = server
        .llm
        .call_reasoning_llm(crate::prompts::DAILY_HEALTH_PROMPT, &user, None, 0.2, 3000)
        .await
        .map_err(|e| format!("daily health LLM call failed: {e}"))?;
    let health_json = parse_llm_json(&raw)?;
    let overall = health_json
        .get("overall_health")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let db_count = health_json
        .get("databases")
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0);
    let report_path = app_home
        .join("reports")
        .join("daily")
        .join(format!("{date}.md"));

    Ok((
        DailyStageReport {
            status: overall.clone(),
            summary: format!("Health check analyzed {db_count} database(s); overall={overall}"),
            details: health_json.clone(),
        },
        health_json,
        report_path,
    ))
}

async fn collect_database_stats_for_targets(
    targets: Vec<ManifestDbTarget>,
) -> Result<Vec<DatabaseStats>, String> {
    let handles = targets
        .into_iter()
        .map(|target| tokio::task::spawn_blocking(move || collect_database_stats(target)))
        .collect::<Vec<_>>();
    let mut databases = Vec::with_capacity(handles.len());

    for handle in handles {
        let stats = handle
            .await
            .map_err(|e| format!("health stats worker join failed: {e}"))?;
        databases.push(stats);
    }

    Ok(databases)
}

async fn run_truth_maintenance_stage(
    server: &MemoryServer,
    app_home: &std::path::Path,
) -> Result<(), String> {
    let targets = load_manifest_targets(server, &app_home.join("manifest.json"))?;
    for target in targets.into_iter().filter(|target| target.allow_write) {
        let route = resolve_truth_maintenance_route(server, &target);
        run_truth_maintenance_for_target(server, target, route).await?;
    }
    Ok(())
}

fn resolve_truth_maintenance_route(
    server: &MemoryServer,
    target: &ManifestDbTarget,
) -> TruthMaintenanceRoute {
    resolve_truth_maintenance_route_for_paths(
        &server.global_db_path_buf(),
        server.project_db_path_buf().as_deref(),
        target,
    )
}

fn resolve_truth_maintenance_route_for_paths(
    global_db_path: &std::path::Path,
    project_db_path: Option<&std::path::Path>,
    target: &ManifestDbTarget,
) -> TruthMaintenanceRoute {
    let role_is_global = target.role == "global";
    let target_db = if role_is_global {
        DbScope::Global
    } else {
        DbScope::Project
    };

    if role_is_global {
        return TruthMaintenanceRoute {
            target_db,
            named_project: None,
            db_path: (!same_db_path(global_db_path, &target.path)).then(|| target.path.clone()),
        };
    }

    if project_db_path.is_some_and(|path| same_db_path(path, &target.path)) {
        return TruthMaintenanceRoute {
            target_db,
            named_project: None,
            db_path: None,
        };
    }

    if let Some(project_name) = crate::path_utils::named_project_for_db_path(&target.path) {
        return TruthMaintenanceRoute {
            target_db,
            named_project: Some(project_name),
            db_path: None,
        };
    }

    TruthMaintenanceRoute {
        target_db,
        named_project: None,
        db_path: Some(target.path.clone()),
    }
}

fn same_db_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    crate::manifest::canonicalize_db_path(left) == crate::manifest::canonicalize_db_path(right)
}

async fn run_truth_maintenance_for_target(
    server: &MemoryServer,
    target: ManifestDbTarget,
    route: TruthMaintenanceRoute,
) -> Result<(), String> {
    let Some(db_path) = target.path.to_str() else {
        return Ok(());
    };
    let store = MemoryStore::open_with_label(db_path, &target.label)
        .map_err(|e| format!("open maintenance DB {}: {e}", target.label))?;
    let conn = store.connection();

    conn.execute(
        "UPDATE memories
         SET archived = 1, updated_at = datetime('now')
         WHERE archived = 0
           AND COALESCE(retention_policy, '') NOT IN ('permanent', 'pinned', 'durable')
           AND importance < 0.70
           AND access_count = 0
           AND julianday(COALESCE(NULLIF(created_at, ''), timestamp)) < julianday('now', '-60 days')",
        [],
    )
    .map_err(|e| format!("truth maintenance prune {}: {e}", target.label))?;

    // ── Self-healing: promote raw → consolidated when DB health ratio is low ──
    let total_active: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE archived = 0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let consolidated_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE archived = 0 AND tier IN ('consolidated','pattern')",
            [], |r| r.get(0),
        )
        .unwrap_or(0);
    let health_ratio = if total_active > 0 {
        consolidated_count as f64 / total_active as f64
    } else {
        1.0
    };
    if health_ratio < 0.35 && total_active > 0 {
        // Self-healing may only apply the same promotion gate as record_access:
        // repeated exact recall from diverse queries. Do not promote merely
        // because a raw note was accessed often.
        let promoted = conn
            .execute(
                "UPDATE memories
                 SET tier = 'consolidated', updated_at = datetime('now')
                 WHERE archived = 0
                   AND tier = 'raw'
                   AND recall_count >= 3
                   AND query_diversity >= 3
                   AND COALESCE(retention_policy, '') NOT IN ('ephemeral')",
                [],
            )
            .unwrap_or(0);
        if promoted > 0 {
            eprintln!(
                "[daily_pipeline] self-heal {}: promoted {promoted} raw → consolidated (ratio was {health_ratio:.2})",
                target.label
            );
        }
    }

    // ── Post-distillation embedding: enqueue non-raw entries without vectors ──
    let needs_embed_ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT m.id FROM memories m
                 LEFT JOIN memories_vec v ON m.id = v.id
                 WHERE m.archived = 0
                   AND m.tier != 'raw'
                   AND v.id IS NULL
                 ORDER BY m.importance DESC
                 LIMIT 50",
            )
            .map_err(|e| format!("prepare embedding scan {}: {e}", target.label))?;
        let ids: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| format!("query embedding scan {}: {e}", target.label))?
            .filter_map(Result::ok)
            .collect();
        ids
    };
    if !needs_embed_ids.is_empty() {
        let candidates =
            memory_core::db::fetch_by_ids(conn, &needs_embed_ids, false).unwrap_or_default();
        for entry in candidates.values() {
            let _ = server.enrichment_lock().enrich_tx.try_send(
                crate::enrichment::build_enrichment_item(
                    entry,
                    true,  // needs_embedding
                    false, // needs_summary
                    route.target_db,
                    route.named_project.clone(),
                    route.db_path.clone(),
                    None,
                    None,
                    entry.revision,
                ),
            );
        }
    }

    let mut stmt = conn
        .prepare(
            "SELECT id FROM memories
             WHERE archived = 0
               AND COALESCE(retention_policy, '') NOT IN ('permanent', 'pinned')
             ORDER BY access_count DESC, timestamp DESC
             LIMIT 200",
        )
        .map_err(|e| format!("prepare promotion scan {}: {e}", target.label))?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("query promotion scan {}: {e}", target.label))?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    let entries = memory_core::db::fetch_by_ids(conn, &ids, false)
        .map_err(|e| format!("fetch promotion candidates {}: {e}", target.label))?;

    for entry in entries.values() {
        let access_days = conn
            .query_row(
                "SELECT COUNT(DISTINCT date(accessed_at)) FROM access_history WHERE memory_id = ?1",
                rusqlite::params![entry.id],
                |row| row.get::<_, usize>(0),
            )
            .unwrap_or(0);
        if crate::pipeline_ops::calculate_promotion_score(entry, access_days) < 0.60 {
            continue;
        }
        conn.execute(
            "UPDATE memories
             SET importance = 0.7, retention_policy = 'durable', updated_at = datetime('now')
             WHERE id = ?1",
            rusqlite::params![entry.id],
        )
        .map_err(|e| format!("promote memory {}: {e}", entry.id))?;
        let _ =
            server
                .enrichment_lock()
                .enrich_tx
                .try_send(crate::enrichment::build_enrichment_item(
                    entry,
                    true,
                    false,
                    route.target_db,
                    route.named_project.clone(),
                    route.db_path.clone(),
                    None,
                    None,
                    entry.revision,
                ));
    }

    Ok(())
}

fn load_manifest_targets(
    server: &MemoryServer,
    manifest_path: &std::path::Path,
) -> Result<Vec<ManifestDbTarget>, String> {
    let manifest = crate::manifest::Manifest::load_or_empty(manifest_path);
    let mut targets = manifest
        .dbs
        .into_iter()
        .map(|entry| {
            let path = PathBuf::from(&entry.path);
            let name = manifest_db_name(&entry, &path);
            let label = manifest_db_label(&entry, &path);
            ManifestDbTarget {
                name,
                label,
                path,
                role: format!("{:?}", entry.role).to_ascii_lowercase(),
                owner: entry.owner,
                schema_kind: entry.schema_kind,
                allow_write: entry.allow_write,
                last_classification: entry.last_classification,
            }
        })
        .collect::<Vec<_>>();

    if targets.is_empty() {
        targets.push(ManifestDbTarget {
            name: "global".to_string(),
            label: "global".to_string(),
            path: server.global_db_path_buf(),
            role: "global".to_string(),
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            allow_write: true,
            last_classification: "unknown".to_string(),
        });
        if let Some(project_path) = server.project_db_path_buf() {
            targets.push(ManifestDbTarget {
                name: "project".to_string(),
                label: project_path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("project")
                    .to_string(),
                path: project_path,
                role: "project".to_string(),
                owner: "tachi".to_string(),
                schema_kind: "tachi".to_string(),
                allow_write: true,
                last_classification: "unknown".to_string(),
            });
        }
    }

    Ok(targets)
}

fn collect_database_stats(target: ManifestDbTarget) -> DatabaseStats {
    let mut stats = DatabaseStats {
        name: target.name.clone(),
        path: target.path.display().to_string(),
        role: target.role,
        owner: target.owner,
        schema_kind: target.schema_kind,
        allow_write: target.allow_write,
        last_classification: target.last_classification,
        total_entries: 0,
        new_today: 0,
        duplicate_count: 0,
        stale_days: 0,
        groups: Vec::new(),
        duplicate_summaries: Vec::new(),
        error: None,
    };

    let Some(db_path) = target.path.to_str() else {
        stats.error = Some("DB path contains invalid UTF-8".to_string());
        return stats;
    };

    let store = match MemoryStore::open_read_only(db_path) {
        Ok(store) => store,
        Err(e) => {
            stats.error = Some(format!("open DB failed: {e}"));
            return stats;
        }
    };
    let conn = store.connection();

    let result = (|| -> Result<(), String> {
        stats.total_entries = conn
            .query_row("SELECT count(*) FROM memories", [], |row| row.get(0))
            .map_err(|e| format!("count memories: {e}"))?;
        stats.new_today = conn
            .query_row(
                "SELECT count(*) FROM memories WHERE created_at > datetime('now', '-1 day')",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("count recent memories: {e}"))?;
        stats.stale_days = conn
            .query_row(
                "SELECT COALESCE(CAST(julianday('now') - julianday(MAX(NULLIF(created_at, ''))) AS INTEGER), 0) FROM memories",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let mut group_stmt = conn
            .prepare("SELECT count(*), category, source FROM memories GROUP BY category, source")
            .map_err(|e| format!("prepare category/source stats: {e}"))?;
        let group_rows = group_stmt
            .query_map([], |row| {
                Ok(CategorySourceCount {
                    count: row.get(0)?,
                    category: row.get(1)?,
                    source: row.get(2)?,
                })
            })
            .map_err(|e| format!("query category/source stats: {e}"))?;
        for row in group_rows {
            stats
                .groups
                .push(row.map_err(|e| format!("read category/source row: {e}"))?);
        }

        let mut dup_stmt = conn
            .prepare(
                "SELECT summary, count(*) FROM memories
                 WHERE trim(summary) <> ''
                 GROUP BY summary HAVING count(*) > 1
                 ORDER BY count(*) DESC LIMIT 20",
            )
            .map_err(|e| format!("prepare duplicate stats: {e}"))?;
        let dup_rows = dup_stmt
            .query_map([], |row| {
                Ok(DuplicateSummary {
                    summary: row.get(0)?,
                    count: row.get(1)?,
                })
            })
            .map_err(|e| format!("query duplicate stats: {e}"))?;
        for row in dup_rows {
            let duplicate = row.map_err(|e| format!("read duplicate row: {e}"))?;
            stats.duplicate_count += duplicate.count.saturating_sub(1);
            stats.duplicate_summaries.push(duplicate);
        }

        Ok(())
    })();

    if let Err(e) = result {
        stats.error = Some(e);
    }
    stats
}

async fn run_agent_evolution_stage(
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

    let eval_rows = collect_recent_eval_rows(server, 100).unwrap_or_default();
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

async fn run_skill_evolution_stage(server: &MemoryServer) -> DailyStageReport {
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

async fn run_routing_analysis_stage(server: &MemoryServer, date: &str) -> DailyStageReport {
    let eval_rows = match collect_eval_rows_30d(server) {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => {
            return DailyStageReport {
                status: "skipped".to_string(),
                summary: "No eval records in the last 30 days".to_string(),
                details: json!({ "eval_count": 0 }),
            };
        }
        Err(e) => {
            return DailyStageReport {
                status: "skipped".to_string(),
                summary: format!("Failed to collect eval records: {e}"),
                details: json!({ "error": e }),
            };
        }
    };

    let mut agent_stats: std::collections::HashMap<String, (u64, u64, f64, u64)> =
        std::collections::HashMap::new();
    for row in &eval_rows {
        let agent = row
            .metadata
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let outcome = row
            .metadata
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let quality = row
            .metadata
            .get("quality_score")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let entry = agent_stats.entry(agent).or_insert((0, 0, 0.0, 0));
        entry.0 += 1; // total
        if outcome == "success" {
            entry.1 += 1;
        }
        if quality > 0.0 {
            entry.2 += quality;
            entry.3 += 1;
        }
    }

    let agents_payload: Vec<Value> = agent_stats
        .iter()
        .map(|(agent, (total, success, quality_sum, quality_count))| {
            let success_rate = if *total > 0 {
                *success as f64 / *total as f64
            } else {
                0.0
            };
            let avg_quality = if *quality_count > 0 {
                quality_sum / *quality_count as f64
            } else {
                0.0
            };
            json!({
                "agent_id": agent,
                "total_evals": total,
                "successes": success,
                "success_rate": (success_rate * 100.0).round() / 100.0,
                "avg_quality": (avg_quality * 100.0).round() / 100.0,
            })
        })
        .collect();

    let payload = json!({
        "date": date,
        "total_eval_records": eval_rows.len(),
        "agents": agents_payload,
    });

    let user = serde_json::to_string_pretty(&payload).unwrap_or_default();
    let raw = match server
        .llm
        .call_reasoning_llm(
            crate::prompts::ROUTING_ANALYSIS_PROMPT,
            &user,
            None,
            0.2,
            2000,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return DailyStageReport {
                status: "failed".to_string(),
                summary: format!("Routing analysis LLM call failed: {e}"),
                details: json!({ "input": payload, "error": e }),
            };
        }
    };

    let routing_json = parse_llm_json(&raw).unwrap_or(json!({ "raw": raw }));
    let proposals_count = routing_json
        .get("routing_proposals")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);

    DailyStageReport {
        status: if proposals_count > 0 {
            "proposals_generated"
        } else {
            "no_changes"
        }
        .to_string(),
        summary: format!(
            "Routing analysis: {} agents evaluated, {} proposals",
            agent_stats.len(),
            proposals_count
        ),
        details: routing_json,
    }
}

fn collect_eval_rows_30d(server: &MemoryServer) -> Result<Vec<EvalEvidenceRow>, String> {
    let collect = |store: &mut MemoryStore| -> Result<Vec<EvalEvidenceRow>, String> {
        let mut stmt = store
            .connection()
            .prepare(
                "SELECT id, path, summary, text, metadata, created_at
                 FROM memories
                 WHERE path LIKE '/eval/%'
                   AND created_at > datetime('now', '-30 day')
                   -- Exclude watchdog auto-close synthesized records; those
                   -- attribute outcome to agent='watchdog/<backend>' with
                   -- empty quality/trajectory/diff and would skew per-agent
                   -- success-rate stats.
                   AND (
                       json_extract(metadata, '$.auto_synthesized') IS NULL
                       OR json_extract(metadata, '$.auto_synthesized') = 0
                   )
                 ORDER BY created_at DESC
                 LIMIT 500",
            )
            .map_err(|e| format!("prepare routing eval query: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
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
            .map_err(|e| format!("query routing evals: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("read routing eval row: {e}"))?);
        }
        Ok(out)
    };

    let mut rows = server.with_global_store_read(collect)?;
    if server.has_project_db() {
        if let Ok(mut project_rows) = server.with_project_store_read(collect) {
            rows.append(&mut project_rows);
        }
    }
    Ok(rows)
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

    let mut rows = server.with_global_store_read(collect)?;
    if server.has_project_db() {
        if let Ok(mut project_rows) = server.with_project_store_read(collect) {
            rows.append(&mut project_rows);
        }
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

fn render_daily_report_markdown(report: &DailyPipelineReport, health_json: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Tachi Daily Pipeline - {}\n\n", report.date));
    out.push_str("## Summary\n\n");
    out.push_str(&format!(
        "- Health Check: {}\n",
        report.health_check.summary
    ));
    out.push_str(&format!(
        "- Agent Evolution: {}\n",
        report.agent_evolution.summary
    ));
    out.push_str(&format!(
        "- Skill Evolution: {}\n",
        report.skill_evolution.summary
    ));
    out.push_str(&format!(
        "- Routing Analysis: {}\n\n",
        report.routing_analysis.summary
    ));

    out.push_str("## Health Check\n\n");
    out.push_str("```json\n");
    out.push_str(&serde_json::to_string_pretty(health_json).unwrap_or_else(|_| "{}".to_string()));
    out.push_str("\n```\n\n");

    out.push_str("## Agent Evolution\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(&report.agent_evolution.details)
            .unwrap_or_else(|_| "{}".to_string()),
    );
    out.push_str("\n```\n\n");

    out.push_str("## Skill Evolution\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(&report.skill_evolution.details)
            .unwrap_or_else(|_| "{}".to_string()),
    );
    out.push_str("\n```\n\n");

    out.push_str("## Routing Analysis\n\n");
    out.push_str("```json\n");
    out.push_str(
        &serde_json::to_string_pretty(&report.routing_analysis.details)
            .unwrap_or_else(|_| "{}".to_string()),
    );
    out.push_str("\n```\n");
    out
}

async fn save_daily_health_wiki(
    server: &MemoryServer,
    date: &str,
    markdown: &str,
) -> Result<(), String> {
    let _ = server
        .tachi_save(Parameters(TachiSaveParams {
            text: markdown.to_string(),
            id: None,
            kind: Some("wiki".to_string()),
            title: Some(format!("Tachi Daily Health {date}")),
            summary: Some(format!("Daily Pipeline report for {date}")),
            path: Some("/tachi/daily-health".to_string()),
            importance: Some(0.85),
            category: Some("experience".to_string()),
            keywords: vec![
                "tachi".to_string(),
                "daily-pipeline".to_string(),
                "health-check".to_string(),
            ],
            entities: vec!["Tachi".to_string()],
            scope: Some("global".to_string()),
            project: None,
            domain: Some("wiki".to_string()),
            retention_policy: Some("permanent".to_string()),
            force: true,
            references: Vec::new(),
            topic: Some("daily-health".to_string()),
            source: None,
            valid_from: None,
            valid_until: None,
            metadata: None,
            files: Vec::new(),
        }))
        .await?;
    Ok(())
}

fn parse_llm_json(raw: &str) -> Result<Value, String> {
    let stripped = crate::llm::LlmClient::strip_code_fence(raw);
    serde_json::from_str(stripped)
        .or_else(|_| {
            let start = stripped.find('{').unwrap_or(0);
            let end = stripped
                .rfind('}')
                .map(|idx| idx + 1)
                .unwrap_or(stripped.len());
            serde_json::from_str(&stripped[start..end])
        })
        .map_err(|e| {
            format!(
                "parse daily health JSON: {e}; raw={}",
                raw.chars().take(500).collect::<String>()
            )
        })
}

fn parse_json_or_raw(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| json!({ "raw": raw }))
}

fn manifest_db_name(entry: &crate::manifest::DbEntry, path: &std::path::Path) -> String {
    if !entry.scope_hint.trim().is_empty() && entry.scope_hint != "unknown" {
        return entry.scope_hint.clone();
    }
    if !entry.owner.trim().is_empty() && entry.owner != "tachi" {
        return entry.owner.clone();
    }
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("memory")
        .to_string()
}

fn manifest_db_label(entry: &crate::manifest::DbEntry, path: &std::path::Path) -> String {
    if matches!(entry.role, crate::manifest::DbRole::Global) {
        return "global".to_string();
    }
    if matches!(entry.role, crate::manifest::DbRole::Project) {
        return path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("project")
            .to_string();
    }
    let name = manifest_db_name(entry, path);
    name.split(':').next_back().unwrap_or(&name).to_string()
}

fn shanghai_today() -> String {
    Utc::now()
        .with_timezone(&shanghai_offset())
        .format("%Y-%m-%d")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_target(role: &str, path: PathBuf) -> ManifestDbTarget {
        ManifestDbTarget {
            name: "target".to_string(),
            label: "target".to_string(),
            path,
            role: role.to_string(),
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            allow_write: true,
            last_classification: "healthy".to_string(),
        }
    }

    #[tokio::test]
    async fn collect_database_stats_for_targets_preserves_manifest_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut first = manifest_target("global", tmp.path().join("first.db"));
        first.name = "first".to_string();
        let mut second = manifest_target("project", tmp.path().join("second.db"));
        second.name = "second".to_string();

        let stats = collect_database_stats_for_targets(vec![first, second])
            .await
            .expect("stats");

        assert_eq!(
            stats
                .iter()
                .map(|stat| stat.name.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert!(stats.iter().all(|stat| stat.error.is_some()));
    }

    fn restore_env_var(key: &str, saved: Option<std::ffi::OsString>) {
        if let Some(value) = saved {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn truth_maintenance_routes_external_project_target_by_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let global = tmp.path().join("global").join("memory.db");
        let current_project = tmp
            .path()
            .join("workspace")
            .join(".tachi")
            .join("memory.db");
        let external = tmp.path().join("agent").join("memory.db");
        let target = manifest_target("agent", external.clone());

        let route =
            resolve_truth_maintenance_route_for_paths(&global, Some(&current_project), &target);

        assert_eq!(route.target_db, DbScope::Project);
        assert_eq!(route.named_project, None);
        assert_eq!(route.db_path.as_deref(), Some(external.as_path()));
    }

    #[test]
    fn truth_maintenance_routes_plan_c_project_by_name() {
        let _guard = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let saved = std::env::var_os("TACHI_HOME");
        std::env::set_var("TACHI_HOME", tmp.path());

        let global = tmp.path().join("global").join("memory.db");
        let current_project = tmp
            .path()
            .join("workspace")
            .join(".tachi")
            .join("memory.db");
        let named = tmp.path().join("projects").join("sigil").join("memory.db");
        let target = manifest_target("project", named);

        let route =
            resolve_truth_maintenance_route_for_paths(&global, Some(&current_project), &target);

        assert_eq!(route.target_db, DbScope::Project);
        assert_eq!(route.named_project.as_deref(), Some("sigil"));
        assert_eq!(route.db_path, None);

        restore_env_var("TACHI_HOME", saved);
    }
}

fn shanghai_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("valid Asia/Shanghai fixed offset")
}
