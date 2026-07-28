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
    /// `~/.tachi` (or test-fixture equivalent): needed to compute the
    /// per-source-DB scoped daemon lock (`daemon_lock::scoped_daemon_lock_path`)
    /// in `migrate_single_db` — `target_db`'s scope is already covered by the
    /// caller's outer `DualDaemonLock`, but a migration source can belong to
    /// a different scope with its own daemon.
    pub app_home: PathBuf,
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
        if db.status != "ok" || !db.is_primary_alias {
            continue;
        }
        // Never migrate the target onto itself.
        let source_path = db.inventory_open_path.as_deref().unwrap_or(&db.path);
        if source_path == target_str
            || crate::physical_db_identity::same_physical_file(
                std::path::Path::new(source_path),
                target_db,
            )
        {
            continue;
        }
        let should_migrate = matches!(
            db.recommended_action.as_str(),
            "review_for_legacy_migration"
        );
        if !should_migrate {
            continue;
        }

        let source = PathBuf::from(source_path);
        let archive_path = archive_root.join(archive_relative_path(&source, home));

        plan.push(TidyMigration {
            source_path: source_path.to_string(),
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
    // Drop the read-only source connection now — it is never used again in
    // this function, and the archive-safety guard below probes whether some
    // OTHER process still holds `source_path` open. Leaving this process's
    // own connection alive across that probe would make lsof see this very
    // call as a "holder" of the file it is about to archive-move, which
    // (before `daemon_ownership`'s self-PID exclusion landed) made every
    // migration look permanently `Owned` and roll back. Both layers matter:
    // this drop removes the self-hold at its source; the self-PID exclusion
    // in `db_ownership.rs` is defense in depth for any other call site that
    // probes while holding its own connection.
    drop(source_store);

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

    // Source-scope lock: the caller's outer `DualDaemonLock` — acquired once
    // in `run_tidy_command` (`crates/tachi-server/src/bootstrap/tidy/command.rs:39`),
    // held for the whole `--execute` run — already covers `target_db`'s
    // scoped lock AND, because `legacy_daemon_lock_path` is a single fixed
    // path per `app_home` (not scoped per DB), the legacy lock for EVERY
    // scope. `source_path` can belong to a DIFFERENT scope (its own
    // project/daemon) that only the scoped half of that coverage says
    // nothing about — that daemon could start and begin writing
    // `source_path` in the window between the ownership probe below and the
    // archive-move further down. Acquire a *scoped-only* lock for
    // `source_path` and hold it across both the probe and the archive-move.
    //
    // Deliberately scoped-only, not another `DualDaemonLock` (do not
    // "reinstate" a legacy attempt here): the outer lock's legacy fd is
    // already held by THIS SAME PROCESS. `flock(2)` locks are per open file
    // description, not per process ("may be denied by a lock that the
    // calling process has already placed via another file descriptor") — a
    // second `DaemonLock::acquire` on the legacy path here would open a
    // fresh fd, collide with the outer lock's fd on the identical file, and
    // misreport the process's own outer hold as `LegacyRunning { pid: self }`,
    // rolling back every migration whose source scope differs from the
    // target's. `ScopedDaemonLock` (see `daemon_lock.rs`) exists specifically
    // to avoid this: the outer hold already excludes every legacy-scheme
    // daemon for the whole run, so only the scoped lock — unique per DB —
    // needs a fresh acquisition here.
    //
    // Skip acquiring even the scoped lock when `source_path` resolves to the
    // SAME scoped lock file as `target_db` (degenerate same-scope input):
    // the lock file IS the mutual-exclusion unit, so if two DB paths hash to
    // the same lock path their daemon would have to be the same process
    // holding the same file — and the outer lock already holds exactly that
    // file for the whole run. Skipping here is not an approximation, it is
    // the same collision-avoidance the scoped-only fix above makes for the
    // legacy path, applied to the scoped path when the two scopes coincide.
    let source_scoped_path =
        crate::daemon_lock::scoped_daemon_lock_path(&cfg.app_home, &source_path);
    let target_scoped_path =
        crate::daemon_lock::scoped_daemon_lock_path(&cfg.app_home, &cfg.target_db);
    let _source_lock = if source_scoped_path == target_scoped_path {
        None
    } else {
        match crate::daemon_lock::ScopedDaemonLock::acquire(&cfg.app_home, &source_path) {
            Ok(lock) => Some(lock),
            Err(crate::daemon_lock::ScopedLockError::Running { pid }) => {
                let outcome = rollback_failed_outcome(
                    migration,
                    &mut target_store,
                    &newly_inserted_ids,
                    rows_before,
                    copied,
                    source_count,
                    &format!(
                        "source DB's own daemon is running (pid {pid}, scoped lock); refusing to risk a torn archive copy"
                    ),
                );
                drop(target_store);
                return Ok(outcome);
            }
            Err(crate::daemon_lock::ScopedLockError::Io(e)) => {
                let outcome = rollback_failed_outcome(
                    migration,
                    &mut target_store,
                    &newly_inserted_ids,
                    rows_before,
                    copied,
                    source_count,
                    &format!("source DB daemon lock probe failed: {e}"),
                );
                drop(target_store);
                return Ok(outcome);
            }
        }
    };

    // Ownership guard: the archive step below moves main/-wal/-shm as three
    // sequential, non-atomic filesystem operations. If a live daemon still
    // holds the source DB open — or ownership cannot be determined — that
    // race can leave a torn archived copy. Roll back the rows we just
    // copied into the target and fail the whole migration atomically (the
    // same rollback path used for a mid-copy SQL error above) rather than
    // leaving a copied-but-unarchived half-state; the source stays on disk
    // untouched and will simply be reconsidered by the next `tidy` run.
    if let Some(reason) = archive_unsafe_reason(&source_path) {
        let outcome = rollback_failed_outcome(
            migration,
            &mut target_store,
            &newly_inserted_ids,
            rows_before,
            copied,
            source_count,
            &reason,
        );
        drop(target_store);
        return Ok(outcome);
    }
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

/// Build a "failed, rolled back" `TidyMigrationOutcome`, deleting the rows
/// this migration attempt newly inserted into `target_store` before
/// reporting. Shared by every failure path downstream of a successful
/// row-copy (source/legacy daemon-lock conflicts, the archive-safety probe)
/// so the rollback + message shape stays identical across all of them.
fn rollback_failed_outcome(
    migration: &TidyMigration,
    target_store: &mut memcore::MemoryStore,
    newly_inserted_ids: &[String],
    rows_before: usize,
    copied: usize,
    source_count: usize,
    reason: &str,
) -> TidyMigrationOutcome {
    for id in newly_inserted_ids {
        let _ = target_store.delete(id);
    }
    TidyMigrationOutcome {
        source_path: migration.source_path.clone(),
        target_path: migration.target_path.clone(),
        archive_path: None,
        status: "failed".to_string(),
        rows_before_target: rows_before,
        rows_after_target: rows_before,
        rows_copied: 0,
        message: format!(
            "rolled back after {copied}/{source_count} rows ({} reverted): {reason}",
            newly_inserted_ids.len()
        ),
    }
}

/// `None` when it is safe to archive-move `source_path` (main + `-wal` +
/// `-shm`, three sequential non-atomic filesystem operations); `Some(reason)`
/// when a live daemon holds it open or ownership could not be determined —
/// either case risks tearing the archived copy.
fn archive_unsafe_reason(source_path: &std::path::Path) -> Option<String> {
    match crate::db_ownership::daemon_ownership(source_path) {
        crate::db_ownership::DbOwnership::NotOwned => None,
        crate::db_ownership::DbOwnership::Owned => Some(
            "live daemon holds this DB open; refusing to archive a possibly torn main/-wal/-shm copy"
                .to_string(),
        ),
        crate::db_ownership::DbOwnership::Unknown(reason) => Some(format!(
            "daemon ownership undetermined ({reason}); refusing to risk a torn archive copy"
        )),
    }
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
