use std::collections::BTreeMap;
use std::io::Write;
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

#[cfg(test)]
thread_local! {
    static FORCE_BOUNDARY_FAILURE_AFTER_ARCHIVE_STAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn force_boundary_failure_after_archive_stage(enabled: bool) {
    FORCE_BOUNDARY_FAILURE_AFTER_ARCHIVE_STAGE.with(|flag| flag.set(enabled));
}

/// Scan-captured physical objects that may be used as migration sources.
///
/// The key remains the exact discovered primary alias for plan matching, but
/// the value is the only authority accepted by the executor. A canonical path
/// or inventory-selected `open_path` remains read-only evidence and cannot
/// become rename/remove/archive authority.
pub(crate) fn authorized_migration_sources(
    report: &TidyReport,
) -> BTreeMap<String, crate::physical_db_identity::PhysicalMutationAuthority> {
    report
        .physical_stores
        .iter()
        .filter_map(|store| {
            let eligible = report.databases.iter().any(|db| {
                db.path == store.primary_path
                    && db.status == "ok"
                    && db.is_primary_alias
                    && db.recommended_action == "review_for_legacy_migration"
            });
            eligible
                .then(|| store.mutation_authority.clone())
                .flatten()
                .map(|authority| (store.primary_path.clone(), authority))
        })
        .collect()
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
        // Mutation authority is the exact discovered primary alias. The
        // inventory open path may follow a symlink or select another
        // hardlink for WAL visibility and must remain read-only evidence.
        let source_path = db.path.as_str();
        // An ambiguous physical store has independent live sidecar owners.
        // It remains fully visible in the report, but no arbitrary alias can
        // become a migration source until the owner adjudicates it.
        let has_mutation_authority = report
            .physical_stores
            .iter()
            .any(|store| store.primary_path == source_path && store.mutation_authority.is_some());
        if !has_mutation_authority {
            continue;
        }
        // Never migrate the target onto itself.
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
    authorized_sources: &BTreeMap<String, crate::physical_db_identity::PhysicalMutationAuthority>,
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

    for migration in plan {
        let Some(authority) = authorized_sources.get(&migration.source_path) else {
            failed += 1;
            outcomes.push(TidyMigrationOutcome {
                source_path: migration.source_path.clone(),
                target_path: migration.target_path.clone(),
                archive_path: None,
                status: "failed".to_string(),
                rows_before_target: 0,
                rows_after_target: 0,
                rows_copied: 0,
                message: "invariant: tidy mutation source must hold scan-captured physical authority; read-only inventory paths and strings alone are not mutation authority".to_string(),
            });
            continue;
        };

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

        match migrate_single_db(migration, cfg, authority) {
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
    authority: &crate::physical_db_identity::PhysicalMutationAuthority,
) -> Result<TidyMigrationOutcome, Box<dyn std::error::Error>> {
    let source_path = PathBuf::from(&migration.source_path);
    let target_path = cfg.target_db.clone();

    // Revalidate directly before the only source open. This catches a path
    // replacement, symlink substitution, identity drift, or source->target
    // transition before either source or target can be opened for mutation.
    authority.revalidate_for_mutation(Some(&target_path))?;
    let source_store = open_cli_store_read_only(&source_path)?;
    let source_entries = {
        let conn = source_store.connection();
        let mut stmt = conn.prepare(
            "SELECT id,path,summary,text,importance,timestamp,category,topic,keywords,'[]' AS persons,entities,'' AS location,source,scope,archived,access_count,last_access,revision,metadata,retention_policy,domain
             FROM memories",
        )?;
        let rows = stmt.query_map([], memcore::row_to_entry)?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let source_count = source_entries.len();

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

    // The source was read-only, but no target write may begin if the source
    // changed while it was being read.
    authority.revalidate_for_mutation(Some(&target_path))?;
    if let Some(parent) = cfg.target_db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    authority.revalidate_for_mutation(Some(&target_path))?;
    let mut target_store = open_cli_store(&target_path)?;
    let rows_before = target_store.stats(true)?.total as usize;

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
                    rows_before,
                    0,
                    source_count,
                    &format!(
                        "source DB's own daemon is running (pid {pid}, scoped lock); refusing to risk a torn archive copy"
                    ),
                );
                return Ok(outcome);
            }
            Err(crate::daemon_lock::ScopedLockError::Io(e)) => {
                let outcome = rollback_failed_outcome(
                    migration,
                    rows_before,
                    0,
                    source_count,
                    &format!("source DB daemon lock probe failed: {e}"),
                );
                return Ok(outcome);
            }
        }
    };

    // Ownership guard: the archive step below moves main/-wal/-shm as three
    // sequential, non-atomic filesystem operations. If a live daemon still
    // holds the source DB open — or ownership cannot be determined — that
    // race can leave a torn archived copy. Refuse before the target
    // transaction starts; the source stays untouched and will simply be
    // reconsidered by the next `tidy` run.
    if let Err(reason) = authority.revalidate_for_mutation(Some(&target_path)) {
        let outcome = rollback_failed_outcome(migration, rows_before, 0, source_count, &reason);
        return Ok(outcome);
    }
    if let Some(reason) = archive_unsafe_reason(&source_path) {
        let outcome = rollback_failed_outcome(migration, rows_before, 0, source_count, &reason);
        return Ok(outcome);
    }
    // Archive the source DB file. Move (rename) when possible; fall back to
    // copy + remove across filesystems.
    let archive_path = PathBuf::from(&migration.archive_path);
    if let Some(parent) = archive_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Keep every target row/projection write in one SQLite transaction until
    // a durable archive copy exists. The live source is deliberately retained
    // through SQLite commit: a commit failure must never strand the only good
    // source at its archive path. Only a successful commit permits removal of
    // the live source alias/file below.
    let copied = source_entries.len();
    let rows_after = match target_store.upsert_batch_with_precommit(&source_entries, |tx| {
        // This is the last authority check before the non-atomic
        // main/WAL/SHM archive move, while target writes remain uncommitted.
        authority
            .revalidate_for_mutation(Some(&target_path))
            .map_err(memcore::MemoryError::InvalidArg)?;
        stage_archive_copy(&source_path, &archive_path)?;
        #[cfg(test)]
        FORCE_BOUNDARY_FAILURE_AFTER_ARCHIVE_STAGE.with(|flag| {
            if flag.get() {
                return Err(memcore::MemoryError::InvalidArg(
                    "injected boundary failure after archive staging".to_string(),
                ));
            }
            Ok::<(), memcore::MemoryError>(())
        })?;
        tx.query_row("SELECT COUNT(*) FROM memories", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|count| count as usize)
        .map_err(memcore::MemoryError::from)
    }) {
        Ok(rows_after) => rows_after,
        Err(memcore::MemoryError::InvalidArg(reason))
            if reason.starts_with("invariant: mutation authority") =>
        {
            return Ok(rollback_failed_outcome(
                migration,
                rows_before,
                copied,
                source_count,
                &reason,
            ));
        }
        Err(error) => return Err(Box::new(error)),
    };

    // SQLite is durable now. Revalidate once more before removing the live
    // source path. A refusal or unlink failure leaves both the source and the
    // staged archive copy intact and records an explicit failed outcome; it
    // never claims an atomic rollback after target commit.
    if let Err(reason) = authority.revalidate_for_mutation(Some(&target_path)) {
        return Ok(committed_source_retained_outcome(
            migration,
            rows_before,
            rows_after,
            copied,
            &reason,
        ));
    }
    if let Err(error) = std::fs::remove_file(&source_path) {
        return Ok(committed_source_retained_outcome(
            migration,
            rows_before,
            rows_after,
            copied,
            &format!("archive copy is durable but live source removal failed: {error}"),
        ));
    }
    // Main source removal is the archive boundary. Sidecars are now orphaned
    // rather than live database state; clean them best-effort and report any
    // residue without misrepresenting the committed migration.
    let mut sidecar_warnings = Vec::new();
    for ext in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{ext}", source_path.display()));
        if sidecar.exists() {
            if let Err(error) = std::fs::remove_file(&sidecar) {
                sidecar_warnings.push(format!("{}: {error}", sidecar.display()));
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
        message: if sidecar_warnings.is_empty() {
            format!("migrated {copied} rows ({rows_before} -> {rows_after} on target)")
        } else {
            format!(
                "migrated {copied} rows ({rows_before} -> {rows_after} on target); orphan sidecar cleanup warnings: {}",
                sidecar_warnings.join("; ")
            )
        },
    })
}

fn stage_archive_copy(
    source_path: &std::path::Path,
    archive_path: &std::path::Path,
) -> std::io::Result<()> {
    let mut staged = Vec::new();
    let result = (|| {
        stage_one_archive_file(source_path, archive_path)?;
        staged.push(archive_path.to_path_buf());
        for ext in ["-wal", "-shm"] {
            let source = PathBuf::from(format!("{}{ext}", source_path.display()));
            if source.exists() {
                let destination = PathBuf::from(format!("{}{ext}", archive_path.display()));
                stage_one_archive_file(&source, &destination)?;
                staged.push(destination);
            }
        }
        Ok(())
    })();
    if result.is_err() {
        for path in staged.into_iter().rev() {
            let _ = std::fs::remove_file(path);
        }
    }
    result
}

fn stage_one_archive_file(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(source)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, destination)?;
            return Ok(());
        }
        #[cfg(windows)]
        {
            if std::fs::metadata(source)?.is_dir() {
                std::os::windows::fs::symlink_dir(target, destination)?;
            } else {
                std::os::windows::fs::symlink_file(target, destination)?;
            }
            return Ok(());
        }
        #[cfg(not(any(unix, windows)))]
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlink archive staging is unsupported on this platform",
            ));
        }
    }

    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    output.sync_all()?;
    std::fs::set_permissions(destination, metadata.permissions())?;
    Ok(())
}

fn committed_source_retained_outcome(
    migration: &TidyMigration,
    rows_before: usize,
    rows_after: usize,
    copied: usize,
    reason: &str,
) -> TidyMigrationOutcome {
    TidyMigrationOutcome {
        source_path: migration.source_path.clone(),
        target_path: migration.target_path.clone(),
        archive_path: Some(migration.archive_path.clone()),
        status: "failed".to_string(),
        rows_before_target: rows_before,
        rows_after_target: rows_after,
        rows_copied: copied,
        message: format!(
            "target committed and archive copy staged, but live source was retained: {reason}"
        ),
    }
}

/// Build a failed outcome after the caller has refused before beginning the
/// target transaction or dropped that transaction without committing it.
/// The phrase "rolled back" is literal: target main/FTS/vector state remains
/// at its pre-migration snapshot even when source IDs overlap existing rows.
fn rollback_failed_outcome(
    migration: &TidyMigration,
    rows_before: usize,
    copied: usize,
    source_count: usize,
    reason: &str,
) -> TidyMigrationOutcome {
    TidyMigrationOutcome {
        source_path: migration.source_path.clone(),
        target_path: migration.target_path.clone(),
        archive_path: None,
        status: "failed".to_string(),
        rows_before_target: rows_before,
        rows_after_target: rows_before,
        rows_copied: 0,
        message: format!(
            "rolled back atomically before target commit after {copied}/{source_count} rows: {reason}"
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
