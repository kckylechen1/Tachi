use crate::server_state::MemoryServer;
use chrono::Utc;
use memcore::MemoryStore;
use serde_json::Value;
use std::path::PathBuf;
use tachi_llm::PersistedModelInvocationReceiptV1;

use super::{
    parse_llm_json, CategorySourceCount, DailyHealthPayload, DailyStageReport, DatabaseStats,
    DuplicateSummary, ManifestDbTarget,
};

pub(crate) async fn run_health_check(
    server: &MemoryServer,
    app_home: &std::path::Path,
    date: &str,
) -> Result<
    (
        DailyStageReport,
        Value,
        PathBuf,
        PersistedModelInvocationReceiptV1,
    ),
    String,
> {
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
    let outcome = server
        .llm
        .call_reasoning_llm_with_receipt(
            crate::prompts::DAILY_HEALTH_PROMPT,
            &user,
            None,
            0.2,
            3000,
        )
        .await
        .map_err(|e| format!("daily health LLM call failed: {e}"))?;
    if outcome.truncated {
        return Err(
            "daily health LLM output truncated; refusing clean report/wiki publish".to_string(),
        );
    }
    let health_json = parse_llm_json(&outcome.text)?;
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
        outcome.invocation,
    ))
}

pub(crate) async fn collect_database_stats_for_targets(
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

pub(crate) fn load_manifest_targets(
    server: &MemoryServer,
    manifest_path: &std::path::Path,
) -> Result<Vec<ManifestDbTarget>, String> {
    let manifest = crate::manifest::Manifest::load_or_empty(manifest_path);
    let mut targets = manifest
        .dbs
        .into_iter()
        .map(|entry| {
            crate::path_utils::manifest_db_leaf_exists(&entry)?;
            let path = PathBuf::from(&entry.path);
            let name = manifest_db_name(&entry, &path);
            let label = manifest_db_label(&entry, &path);
            Ok(ManifestDbTarget {
                name,
                label,
                path,
                role: format!("{:?}", entry.role).to_ascii_lowercase(),
                owner: entry.owner,
                schema_kind: entry.schema_kind,
                allow_write: entry.allow_write,
                last_classification: entry.last_classification,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

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

    let result = (|| -> Result<(), String> {
        let snapshot = store
            .collect_daily_health_snapshot()
            .map_err(|e| format!("collect daily health snapshot: {e}"))?;
        stats.total_entries = snapshot.total_entries;
        stats.new_today = snapshot.new_today;
        stats.stale_days = snapshot.stale_days;
        stats.groups = snapshot
            .groups
            .into_iter()
            .map(|group| CategorySourceCount {
                count: group.count,
                category: group.category,
                source: group.source,
            })
            .collect();
        stats.duplicate_summaries = snapshot
            .duplicate_summaries
            .into_iter()
            .map(|duplicate| DuplicateSummary {
                summary: duplicate.summary,
                count: duplicate.count,
            })
            .collect();
        for duplicate in &stats.duplicate_summaries {
            stats.duplicate_count += duplicate.count.saturating_sub(1);
        }
        Ok(())
    })();

    if let Err(e) = result {
        stats.error = Some(e);
    }
    stats
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
