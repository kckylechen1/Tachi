use std::path::PathBuf;

use super::super::{
    open_cli_store, open_cli_store_read_only, TidyExecuteSummary, TidyMigration,
    TidyMigrationOutcome, TidyReport,
};
use super::classify::tidy_rationale;

/// Configuration controlling how migrations are executed.
#[derive(Debug, Clone)]
pub(crate) struct MigrationConfig {
    pub target_db: PathBuf,
    pub manifest_path: PathBuf,
    /// When true, do not perform any write. Used by integration tests and
    /// equivalent to `--dry-run --execute` (which is currently disallowed at
    /// the CLI but useful for tests).
    pub dry_run: bool,
    /// True when prompts are appropriate (TTY + !yes).
    pub interactive: bool,
}

/// Build the list of source DBs that are candidates for fragment-consolidation
/// migration into a single target DB. Pure function — no I/O.
///
/// We migrate DBs whose recommended action implies that the rows should be
/// merged into the canonical store. We deliberately do NOT migrate
/// `keep_*_db` (already in the right place) or `repair_before_any_move`
/// (unsafe). `archive_or_delete_after_review` is included only when the
/// caller explicitly opts in via `--yes`; the planner records it with action
/// label so the executor can decide.
pub(crate) fn build_migration_plan(
    report: &TidyReport,
    target_db: &std::path::Path,
    archive_root: &std::path::Path,
    home: &std::path::Path,
) -> Vec<TidyMigration> {
    let mut plan = Vec::new();
    let target_str = target_db.to_string_lossy().to_string();

    for db in &report.databases {
        if db.status != "ok" {
            continue;
        }
        // Never migrate the target onto itself.
        if db.path == target_str {
            continue;
        }
        let should_migrate = matches!(
            db.recommended_action.as_str(),
            "review_for_legacy_migration"
        );
        if !should_migrate {
            continue;
        }

        let source = PathBuf::from(&db.path);
        let archive_path = archive_root.join(archive_relative_path(&source, home));

        plan.push(TidyMigration {
            source_path: db.path.clone(),
            target_path: target_str.clone(),
            archive_path: archive_path.display().to_string(),
            scope_suggestion: db.scope_suggestion.clone(),
            action: db.recommended_action.clone(),
            source_row_count: db.entry_count.unwrap_or(0),
            reason: tidy_rationale(&db.scope_suggestion, &db.recommended_action),
        });
    }

    plan
}

/// Compute a stable, collision-free relative path used inside the archive
/// timestamp directory. We prefer the source path relative to `$HOME` so the
/// archive layout mirrors the user's tree; falls back to the file name when
/// the source is outside `$HOME`.
fn archive_relative_path(source: &std::path::Path, home: &std::path::Path) -> PathBuf {
    if let Ok(rel) = source.strip_prefix(home) {
        return rel.to_path_buf();
    }
    PathBuf::from(
        source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| memcore::MEMORY_DB_FILENAME.to_string()),
    )
}

pub(crate) fn execute_tidy_migrations(
    plan: &[TidyMigration],
    cfg: &MigrationConfig,
) -> Result<TidyExecuteSummary, Box<dyn std::error::Error>> {
    let mut outcomes = Vec::new();
    let mut migrated = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    if plan.is_empty() {
        return Ok(TidyExecuteSummary {
            target_db: cfg.target_db.display().to_string(),
            planned: plan.to_vec(),
            outcomes,
            migrated_count: 0,
            skipped_count: 0,
            failed_count: 0,
            dry_run: cfg.dry_run,
        });
    }

    // Ensure target parent exists (needed for both real run and creating a
    // fresh empty target DB).
    if let Some(parent) = cfg.target_db.parent() {
        if !cfg.dry_run {
            std::fs::create_dir_all(parent)?;
        }
    }

    for migration in plan {
        // Interactive confirm.
        if cfg.interactive {
            let prompt = format!(
                "Migrate {} ({} rows) into {} and archive source? [y/N]",
                migration.source_path, migration.source_row_count, migration.target_path
            );
            let confirmed = dialoguer::Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()
                .unwrap_or(false);
            if !confirmed {
                outcomes.push(TidyMigrationOutcome {
                    source_path: migration.source_path.clone(),
                    target_path: migration.target_path.clone(),
                    archive_path: None,
                    status: "skipped".to_string(),
                    rows_before_target: 0,
                    rows_after_target: 0,
                    rows_copied: 0,
                    message: "skipped by interactive prompt".to_string(),
                });
                skipped += 1;
                continue;
            }
        }

        match migrate_single_db(migration, cfg) {
            Ok(outcome) => {
                if outcome.status == "migrated" {
                    migrated += 1;
                } else if outcome.status == "failed" {
                    failed += 1;
                } else {
                    skipped += 1;
                }
                outcomes.push(outcome);
            }
            Err(err) => {
                failed += 1;
                outcomes.push(TidyMigrationOutcome {
                    source_path: migration.source_path.clone(),
                    target_path: migration.target_path.clone(),
                    archive_path: None,
                    status: "failed".to_string(),
                    rows_before_target: 0,
                    rows_after_target: 0,
                    rows_copied: 0,
                    message: format!("migration error: {err}"),
                });
            }
        }
    }

    // Update manifest (drop migrated source entries, ensure target entry).
    if !cfg.dry_run {
        if let Err(err) = update_manifest_after_migration(cfg, &outcomes) {
            eprintln!(
                "[tidy] WARN: manifest update failed after migration: {err}; rows were migrated successfully"
            );
        }
    }

    Ok(TidyExecuteSummary {
        target_db: cfg.target_db.display().to_string(),
        planned: plan.to_vec(),
        outcomes,
        migrated_count: migrated,
        skipped_count: skipped,
        failed_count: failed,
        dry_run: cfg.dry_run,
    })
}

fn migrate_single_db(
    migration: &TidyMigration,
    cfg: &MigrationConfig,
) -> Result<TidyMigrationOutcome, Box<dyn std::error::Error>> {
    use std::collections::HashSet;

    let source_path = PathBuf::from(&migration.source_path);
    let target_path = cfg.target_db.clone();

    let source_store = open_cli_store_read_only(&source_path)?;
    let source_count: usize = {
        let count: i64 =
            source_store
                .connection()
                .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        count as usize
    };

    if cfg.dry_run {
        return Ok(TidyMigrationOutcome {
            source_path: migration.source_path.clone(),
            target_path: migration.target_path.clone(),
            archive_path: Some(migration.archive_path.clone()),
            status: "dry_run".to_string(),
            rows_before_target: 0,
            rows_after_target: 0,
            rows_copied: source_count,
            message: format!("would migrate {source_count} rows"),
        });
    }

    let mut target_store = open_cli_store(&target_path)?;
    let rows_before = target_store.stats(true)?.total as usize;

    let existing_target_ids: HashSet<String> = {
        let conn = target_store.connection();
        let mut stmt = conn.prepare("SELECT id FROM memories")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?.into_iter().collect()
    };

    let mut copied = 0usize;
    let mut newly_inserted_ids: Vec<String> = Vec::new();
    let mut copy_err: Option<Box<dyn std::error::Error>> = None;

    {
        let conn = source_store.connection();
        let mut stmt = conn.prepare(
            "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,'[]' AS persons,entities,'' AS location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
             FROM memories",
        )?;
        let rows = stmt.query_map([], memcore::row_to_entry)?;
        for row in rows {
            let entry = row?;
            let existed_before = existing_target_ids.contains(&entry.id);
            match target_store.upsert(&entry) {
                Ok(()) => {
                    if !existed_before {
                        newly_inserted_ids.push(entry.id.clone());
                    }
                    copied += 1;
                }
                Err(e) => {
                    copy_err = Some(Box::new(e));
                    break;
                }
            }
        }
    }

    if let Some(err) = copy_err {
        // Best-effort rollback: delete rows we newly inserted in this run.
        for id in &newly_inserted_ids {
            let _ = target_store.delete(id);
        }
        return Ok(TidyMigrationOutcome {
            source_path: migration.source_path.clone(),
            target_path: migration.target_path.clone(),
            archive_path: None,
            status: "failed".to_string(),
            rows_before_target: rows_before,
            rows_after_target: rows_before,
            rows_copied: 0,
            message: format!(
                "rolled back after {copied}/{source_count} rows ({} reverted): {err}",
                newly_inserted_ids.len()
            ),
        });
    }

    let rows_after = target_store.stats(true)?.total as usize;
    drop(target_store);

    // Archive the source DB file. Move (rename) when possible; fall back to
    // copy + remove across filesystems.
    let archive_path = PathBuf::from(&migration.archive_path);
    if let Some(parent) = archive_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(&source_path, &archive_path) {
        Ok(()) => {}
        Err(_) => {
            std::fs::copy(&source_path, &archive_path)?;
            std::fs::remove_file(&source_path)?;
        }
    }
    // Also move sidecar WAL/SHM files if present.
    for ext in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{ext}", source_path.display()));
        if sidecar.exists() {
            let dst = PathBuf::from(format!("{}{ext}", archive_path.display()));
            if let Err(e) = std::fs::rename(&sidecar, &dst).or_else(|_| {
                std::fs::copy(&sidecar, &dst)
                    .map(|_| ())
                    .and_then(|_| std::fs::remove_file(&sidecar))
            }) {
                tracing::warn!("tidy: failed to move sidecar {}: {e}", sidecar.display());
            }
        }
    }

    Ok(TidyMigrationOutcome {
        source_path: migration.source_path.clone(),
        target_path: migration.target_path.clone(),
        archive_path: Some(archive_path.display().to_string()),
        status: "migrated".to_string(),
        rows_before_target: rows_before,
        rows_after_target: rows_after,
        rows_copied: copied,
        message: format!("migrated {copied} rows ({rows_before} -> {rows_after} on target)"),
    })
}

/// Drop migrated source entries from the manifest and ensure the target entry
/// exists. We deliberately only touch entries we actually migrated; the rest
/// of the manifest is left as-is. Pure with respect to `outcomes` — file I/O
/// is wrapped in `Manifest::load_or_empty` / `save`.
pub(crate) fn update_manifest_after_migration(
    cfg: &MigrationConfig,
    outcomes: &[TidyMigrationOutcome],
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::manifest::{DbEntry, DbRole, Manifest};

    let mut manifest = Manifest::load_or_empty(&cfg.manifest_path);

    let migrated_canons: std::collections::HashSet<String> = outcomes
        .iter()
        .filter(|o| o.status == "migrated")
        .map(|o| {
            crate::manifest::canonicalize_db_path(std::path::Path::new(&o.source_path))
                .display()
                .to_string()
        })
        .collect();
    if migrated_canons.is_empty() {
        return Ok(());
    }

    let target_canonical = crate::manifest::canonicalize_db_path(&cfg.target_db)
        .display()
        .to_string();

    manifest.dbs.retain(|e| !migrated_canons.contains(&e.path));

    if !manifest
        .dbs
        .iter()
        .any(|e| e.path == target_canonical || e.path == cfg.target_db.display().to_string())
    {
        manifest.dbs.push(DbEntry {
            path: target_canonical,
            role: DbRole::Global,
            owner: "tachi".to_string(),
            schema_kind: "tachi-memory-v1".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: chrono::Utc::now().to_rfc3339(),
            last_classification: "healthy".to_string(),
            scope_hint: "global".to_string(),
            notes: "registered by `tachi tidy --execute`".to_string(),
        });
    }
    manifest.generated_at = chrono::Utc::now().to_rfc3339();
    manifest.save(&cfg.manifest_path)?;
    Ok(())
}
