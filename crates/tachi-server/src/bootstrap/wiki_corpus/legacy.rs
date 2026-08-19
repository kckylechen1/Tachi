use super::classify::*;
use super::fs::*;
use super::types::*;

use crate::server_state::MemoryServer;
use crate::tool_params::derive_effective_knowledge_artifact;
use memcore::db::migrations::EXPECTED_SCHEMA_VERSION;
use memcore::MemoryStore;
use rusqlite::{Connection, OptionalExtension};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub(crate) fn legacy_adoption_target_path(app_home: &Path) -> PathBuf {
    app_home
        .join("projects")
        .join("wiki")
        .join(memcore::MEMORY_DB_FILENAME)
}

/// Removes exactly the files a bootstrap-and-import of `target` could have
/// written -- the main db file plus its `-wal`/`-shm` sidecars -- and nothing
/// else in `target`'s directory. Used by the round-2 bug B failure path,
/// where `target_dir` preexisted this run and so is not this run's to
/// remove wholesale. Missing files are not an error: a failure early in
/// `adopt_into_bootstrapped_store` may have written none of the three.
pub(crate) fn remove_adopted_store_files(target: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        match std::fs::remove_file(preview_sidecar_path(target, suffix)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// `Ok(())` when the row is adopted; `Err(reason)` is the exact reported
/// string. Rules run in declaration order so the reported reason is stable.
pub(crate) fn adoption_eligibility(row: &RawRow) -> Result<(), String> {
    // E1 -- only the class the classifier could not place. Every other class
    // carries positive typed evidence about where it belongs, and adoption
    // never overrides that evidence.
    match row.classification {
        Some(CorpusClassification::ManualReview) => {}
        Some(other) => {
            return Err(format!(
                "classification={}",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".to_string())
            ))
        }
        None => return Err("classification_missing".to_string()),
    }

    // E2 -- the same two knowledge-artifact roots `wiki_ops::search` is
    // willing to project. Spelled out rather than imported: that predicate is
    // `pub(super)` to `wiki_ops`, and a bootstrap verb must not widen it.
    let path = row.path.as_str();
    let public_knowledge_artifact = path == "/wiki"
        || path.starts_with("/wiki/")
        || path == "/guide"
        || path.starts_with("/guide/");
    if !public_knowledge_artifact {
        return Err("path_not_public_knowledge_artifact".to_string());
    }

    // E3 -- every identity the shared write path refuses. Checked here so the
    // refusal is a reported skip in the preview rather than a mid-import
    // failure against a store that has already been created.
    if adoption_reserved_identity(row) {
        return Err("reserved_identity".to_string());
    }

    // E4 -- the marker is inserted into a JSON object; a row whose metadata is
    // not an object has nowhere to carry provenance.
    if row.metadata_parse_error.is_some() || !row.metadata.is_object() {
        return Err("metadata_json_invalid".to_string());
    }

    Ok(())
}

/// Mirror of the identities `memcore`'s shared
/// `refuse_reserved_write_identity` rejects, evaluated against the same values
/// the import will pass it (the id, the *normalized* path, and the topic).
pub(crate) fn adoption_reserved_identity(row: &RawRow) -> bool {
    let entry = memory_entry_from_raw(row);
    let normalized = memcore::path_router::normalize_path(&row.path);
    row.id.trim().is_empty()
        || row.id.starts_with("anchor:")
        || memcore::is_reserved_wiki_rem_id(&row.id)
        || row.id == "wiki-operation-log"
        || normalized == "/wiki/_log"
        || normalized.starts_with("/wiki/_log/")
        || memcore::namespace::is_wiki_log_entry(&entry)
}

pub(crate) fn adoption_run_id(legacy_physical_id: &str, ids: &[String]) -> String {
    digest_string(
        &json!({
            "version": WIKI_LEGACY_ADOPTION_REPORT_VERSION,
            "legacy_physical_id": legacy_physical_id,
            "ids": ids,
        })
        .to_string(),
    )
}

/// The two lifecycle columns `RawRow` does not carry.
///
/// `load_rows`' SELECT deliberately omits `created_at`/`updated_at`, and
/// extending `RawRow` would change `row_digest` and therefore every existing
/// plan fingerprint. Adoption reads them separately instead.
pub(crate) struct LegacyLifecycleColumns {
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

pub(crate) fn load_legacy_lifecycle_columns(
    open_path: &Path,
) -> Result<BTreeMap<String, LegacyLifecycleColumns>, String> {
    let conn = open_preview_connection(open_path)?;
    let mut statement = conn
        .prepare("PRAGMA table_info(memories)")
        .map_err(|error| error.to_string())?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| error.to_string())?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|error| error.to_string())?;
    drop(statement);
    for required in ["created_at", "updated_at"] {
        if !columns.contains(required) {
            return Err(format!(
                "legacy store lacks column '{required}'; snapshot adoption cannot fabricate it"
            ));
        }
    }

    // One deferred transaction so both columns come from a single WAL
    // snapshot rather than two independent reads.
    let tx = conn
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let mut statement = tx
        .prepare("SELECT id, created_at, updated_at FROM memories ORDER BY id ASC")
        .map_err(|error| error.to_string())?;
    let mut lifecycle = BTreeMap::new();
    let mapped = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                LegacyLifecycleColumns {
                    created_at: row.get(1)?,
                    updated_at: row.get(2)?,
                },
            ))
        })
        .map_err(|error| error.to_string())?;
    for row in mapped {
        let (id, columns) = row.map_err(|error| error.to_string())?;
        lifecycle.insert(id, columns);
    }
    drop(statement);
    drop(tx);
    Ok(lifecycle)
}

/// Build one import entry: the source row verbatim, plus exactly one additive
/// nested metadata key.
pub(crate) fn adoption_entry(
    row: &RawRow,
    lifecycle: &LegacyLifecycleColumns,
    legacy_physical_id: &str,
    adoption_run_id: &str,
    adopted_at: &str,
) -> Result<memcore::PortableImportEntry, String> {
    // The id is preserved verbatim. The destination is created empty by this
    // same run, so the import's duplicate-id refusal cannot fire and every
    // intra-set `superseded_by` edge still resolves to the row it named.
    let mut entry = memory_entry_from_raw(row);
    let Some(metadata) = entry.metadata.as_object_mut() else {
        // Unreachable: E4 already refused non-object metadata. Kept as a hard
        // refusal rather than a silent default so a future eligibility edit
        // cannot quietly start dropping provenance.
        return Err(format!(
            "legacy row '{}' metadata is not a JSON object; adoption cannot attach provenance",
            row.id
        ));
    };
    metadata.insert(
        WIKI_LEGACY_ADOPTION_MARKER_KEY.to_string(),
        json!({
            "source_store": "legacy_global",
            "source_physical_id": legacy_physical_id,
            "source_id": row.id,
            "adopted_at": adopted_at,
            "confirm_token": WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN,
            "adoption_run_id": adoption_run_id,
            "review_status": "review_pending",
            "reviewed": false,
        }),
    );
    Ok(memcore::PortableImportEntry {
        entry,
        created_at: lifecycle.created_at.clone(),
        updated_at: lifecycle.updated_at.clone(),
        superseded_by: row.superseded_by.clone(),
    })
}

/// Every refusal the import can raise that is a pure function of the entries,
/// evaluated **before** anything is created on disk.
///
/// `import_snapshot_batch` runs its own pre-walk before opening a
/// transaction, so these failures never write a row — but by then the store
/// file already exists, and an empty store is worse than no store: it flips
/// the named-store existence gate, which silences the zero-store search
/// refusal without making search work.
///
/// Not pre-flightable: vector availability, which is a property of the
/// destination and unknowable until it is open. That one is handled by the
/// post-creation compensation path.
pub(crate) fn adoption_preflight(entries: &[memcore::PortableImportEntry]) -> Result<(), String> {
    // The destination carries the `wiki` label, so `validate_write_path`'s only
    // rejection clause -- a `/wiki/...` path in a non-wiki store -- cannot fire
    // for any entry. Asserted once rather than assumed.
    if !memcore::path_router::db_label_is_wiki_corpus(memcore::path_router::WIKI_CORPUS_DB_LABEL) {
        return Err("wiki corpus label no longer identifies the wiki corpus".to_string());
    }
    let mut seen = BTreeSet::new();
    for import in entries {
        let entry = &import.entry;
        if !seen.insert(entry.id.as_str()) {
            return Err(format!(
                "legacy adoption pre-flight: duplicate id '{}' in the adoption set",
                entry.id
            ));
        }
        let normalized = memcore::path_router::normalize_path(&entry.path);
        if entry.id.trim().is_empty()
            || entry.id.starts_with("anchor:")
            || memcore::is_reserved_wiki_rem_id(&entry.id)
            || entry.id == "wiki-operation-log"
            || normalized == "/wiki/_log"
            || normalized.starts_with("/wiki/_log/")
            || entry.topic.eq_ignore_ascii_case("wiki_log")
        {
            return Err(format!(
                "legacy adoption pre-flight: id '{}' is a reserved write identity and cannot be \
                 imported",
                entry.id
            ));
        }
    }
    Ok(())
}

/// Read back every adopted row's stored `path` and compare it against the
/// value adoption predicted, closing the gap the lifecycle checksum leaves
/// open (it covers `archived`/`created_at`/`id`/`revision`/`superseded_by`/
/// `updated_at`/`valid_until` -- not `path`).
pub(crate) fn verify_adopted_paths(
    target: &Path,
    entries: &[memcore::PortableImportEntry],
) -> Result<(), String> {
    // Opened read-write, not read-only: a read-only open of a WAL database
    // whose `-shm` sidecar is absent has to create one, which is exactly the
    // failure `open_preview_connection` exists to work around. This is a file
    // this run created and owns, so there is nothing to protect it from.
    let conn = Connection::open(target)
        .map_err(|error| format!("cannot reopen adopted store for path readback: {error}"))?;
    for import in entries {
        let expected = memcore::path_router::normalize_path(&import.entry.path);
        let stored: Option<String> = conn
            .query_row(
                "SELECT path FROM memories WHERE id = ?1",
                [import.entry.id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| {
                format!(
                    "cannot read adopted path for '{}': {error}",
                    import.entry.id
                )
            })?;
        match stored {
            Some(stored) if stored == expected => {}
            Some(stored) => {
                return Err(format!(
                "adopted row '{}' stored path '{stored}' does not match the predicted '{expected}'",
                import.entry.id
            ))
            }
            None => {
                return Err(format!(
                    "adopted row '{}' is absent from the destination after a successful import",
                    import.entry.id
                ))
            }
        }
    }
    Ok(())
}

/// `tachi wiki corpus --adopt-legacy`.
///
/// Bootstrap-only by construction: it refuses to run against a `wiki` store
/// that already exists, and it is read-only on the legacy global — no
/// supersede, no archive, no metadata write. Rollback is therefore complete
/// removal of the store it created; there is no partial state to reconcile.
pub(crate) fn run_wiki_corpus_legacy_adoption_command(
    apply: bool,
    confirm: Option<String>,
    backup_dir: Option<PathBuf>,
    plan_path: Option<PathBuf>,
    global_db: &Path,
    project_db: Option<&Path>,
    app_home: &Path,
) -> Result<WikiCorpusReport, String> {
    if apply {
        return Err(
            "--adopt-legacy is its own confirmed mode and cannot be combined with --apply"
                .to_string(),
        );
    }
    if plan_path.is_some() {
        return Err(
            "--adopt-legacy does not take --plan; the adoption set is derived from the legacy \
             store's own classification"
                .to_string(),
        );
    }
    if backup_dir.is_some() {
        return Err(
            "--adopt-legacy does not take --backup-dir; it never writes to an existing store — \
             back up the legacy global out-of-band before running"
                .to_string(),
        );
    }
    let confirmed = match confirm.as_deref() {
        None => false,
        Some(token) if token == WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN => true,
        Some(_) => {
            return Err(format!(
                "legacy adoption requires exact --confirm {WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN}"
            ))
        }
    };

    // Round-2 bug A: this existing-target gate must run before the legacy
    // store is opened at all, not merely before the destination is written.
    // `target_existed_before` is a pure function of `app_home` and the wiki
    // store path -- it touches nothing legacy -- so hoisting it ahead of
    // `store_specs`/`inventory_store` (which does open and read the legacy
    // global) costs nothing and closes the window where a second confirmed
    // run against an occupied target would still read the legacy source
    // before refusing.
    let target = legacy_adoption_target_path(app_home);
    let target_dir = target
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "wiki store path has no parent directory".to_string())?;
    let target_existed_before =
        MemoryServer::resolve_existing_named_project_db_path_in_home("wiki", app_home)
            .map_err(|error| format!("cannot resolve the named wiki store: {error}"))?
            .is_some()
            || std::fs::symlink_metadata(&target).is_ok();
    if target_existed_before && confirmed {
        // B1: the remedy is removal and re-run, not `--apply`. `--apply`
        // refuses any absent involved store, and on a host with no bound
        // project DB the `bound_project` store is always absent -- so naming
        // it here would send the operator to a command that cannot run.
        return Err(format!(
            "wiki store already exists at {}; --adopt-legacy is a bootstrap-only mode — remove {} \
             and re-run if you intend to rebuild it",
            target.display(),
            target_dir.display()
        ));
    }

    let mut scans = store_specs(global_db, project_db, app_home)
        .into_iter()
        .map(inventory_store)
        .collect::<Vec<_>>();
    finalize_classifications(&mut scans);
    for scan in scans.iter_mut() {
        refresh_report(scan);
    }
    let warnings = scans
        .iter()
        .filter_map(|scan| {
            scan.report
                .read_failure
                .as_ref()
                .map(|failure| format!("{}: {}", scan.report.logical_store_ref, failure.message))
        })
        .collect::<Vec<_>>();

    let legacy = find_scan(&scans, LogicalStore::LegacyGlobal.reference())?;
    if let Some(failure) = legacy.report.read_failure.as_ref() {
        return Err(format!(
            "legacy global store is unreadable: {}",
            failure.message
        ));
    }
    if legacy.report.stored_schema != Some(EXPECTED_SCHEMA_VERSION) {
        return Err(format!(
            "legacy global store is at schema {:?}, expected {EXPECTED_SCHEMA_VERSION}",
            legacy.report.stored_schema
        ));
    }
    let physical = legacy
        .physical
        .clone()
        .ok_or_else(|| "legacy global store has no physical identity".to_string())?;
    let legacy_row_digest_before = legacy.row_digest.clone();
    let legacy_spec = legacy.spec.clone();

    // B3: the corpus is the classified rows, not every row the legacy store
    // holds. `load_rows` selects the whole `memories` table; only wiki-related
    // rows carry a classification.
    let mut skipped = Vec::new();
    // Owned clones, not borrows: the report below consumes `scans`, and the
    // adoption set must outlive the inventory it was derived from.
    let mut eligible: Vec<RawRow> = Vec::new();
    let mut wiki_related_rows = 0usize;
    for row in legacy.rows.iter() {
        if row.classification.is_none() {
            continue;
        }
        wiki_related_rows += 1;
        match adoption_eligibility(row) {
            Ok(()) => eligible.push(row.clone()),
            Err(reason) => skipped.push(AdoptionSkip {
                id: row.id.clone(),
                path: row.path.clone(),
                reason,
            }),
        }
    }
    skipped.sort_by(|left, right| left.id.cmp(&right.id));
    eligible.sort_by(|left, right| left.id.cmp(&right.id));
    let eligible_rows = eligible.len();
    if eligible.is_empty() && confirmed {
        return Err(
            "no eligible legacy rows to adopt; refusing to create an empty wiki store".to_string(),
        );
    }

    let lifecycle_columns = load_legacy_lifecycle_columns(&PathBuf::from(&physical.open_path))?;
    let adopted_ids = eligible
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    let adoption_run_id = adoption_run_id(&physical.physical_id, &adopted_ids);
    let adopted_at = chrono::Utc::now().to_rfc3339();
    let mut entries = Vec::with_capacity(eligible.len());
    for row in eligible.iter() {
        let lifecycle = lifecycle_columns.get(&row.id).ok_or_else(|| {
            format!(
                "legacy row '{}' vanished between inventory and lifecycle read; re-run",
                row.id
            )
        })?;
        entries.push(adoption_entry(
            row,
            lifecycle,
            &physical.physical_id,
            &adoption_run_id,
            &adopted_at,
        )?);
    }

    let path_rewrites = entries
        .iter()
        .filter_map(|import| {
            let stored = memcore::path_router::normalize_path(&import.entry.path);
            (stored != import.entry.path).then(|| AdoptionPathRewrite {
                id: import.entry.id.clone(),
                source_path: import.entry.path.clone(),
                stored_path: stored,
            })
        })
        .collect::<Vec<_>>();

    // The lifecycle every adopted row will derive once stored. A run can
    // import every row and match every checksum while leaving search empty, so
    // this is measured, not assumed.
    let mut derived_lifecycle_counts = BTreeMap::<String, usize>::new();
    let mut default_retrievable_rows = 0usize;
    for import in entries.iter() {
        let effective = derive_effective_knowledge_artifact(
            &import.entry.metadata,
            &import.entry.path,
            &import.entry.scope,
        );
        *derived_lifecycle_counts
            .entry(effective.lifecycle.as_str().to_string())
            .or_default() += 1;
        if effective.lifecycle.is_default_retrievable() {
            default_retrievable_rows += 1;
        }
    }
    if confirmed && default_retrievable_rows == 0 {
        return Err(format!(
            "no adopted row would be default-retrievable (0 of {eligible_rows} derive a \
             default-retrievable lifecycle); creating the wiki store would silence the zero-store \
             search refusal without making search work"
        ));
    }

    let expected_lifecycle_checksum =
        memcore::PortableImportReceipt::expected_lifecycle_checksum(&entries)
            .map_err(|error| format!("cannot compute the expected lifecycle checksum: {error}"))?;
    let expected_vector_checksum =
        memcore::PortableImportReceipt::expected_vector_checksum(&entries)
            .map_err(|error| format!("cannot compute the expected vector checksum: {error}"))?;

    let mut report = LegacyAdoptionReport {
        version: WIKI_LEGACY_ADOPTION_REPORT_VERSION.to_string(),
        confirmed,
        confirm_token_required: WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN.to_string(),
        target_path: target.display().to_string(),
        target_existed_before,
        target_created: false,
        target_store_role_stamped: None,
        target_removed_after_failure: None,
        legacy_open_path: Some(physical.open_path.clone()),
        legacy_physical_id: Some(physical.physical_id.clone()),
        legacy_row_digest_before: legacy_row_digest_before.clone(),
        legacy_row_digest_after: None,
        wiki_related_rows,
        eligible_rows,
        skipped,
        adopted_ids,
        adoption_run_id,
        provenance_marker_key: WIKI_LEGACY_ADOPTION_MARKER_KEY.to_string(),
        derived_lifecycle_counts,
        default_retrievable_rows,
        path_rewrites,
        rows_imported: 0,
        vectors_imported: 0,
        vectors_absent: 0,
        dangling_supersessions: Vec::new(),
        expected_lifecycle_checksum: expected_lifecycle_checksum.clone(),
        expected_vector_checksum: expected_vector_checksum.clone(),
        observed_lifecycle_checksum: None,
        observed_vector_checksum: None,
        checksums_match: None,
        reconciler_impact: WIKI_LEGACY_ADOPTION_RECONCILER_IMPACT.to_string(),
        legacy_rows_forced_to_manual_review_by_duplicate_path: None,
        had_failures: false,
        remediation: None,
        errors: Vec::new(),
    };

    if !confirmed {
        return Ok(WikiCorpusReport {
            version: REPORT_VERSION.to_string(),
            mode: "legacy_adoption_preview".to_string(),
            apply: false,
            stores: scans.into_iter().map(|scan| scan.report).collect(),
            plan: None,
            backup_manifest: None,
            migration_outcomes: Vec::new(),
            warnings,
            sibling_repair: None,
            legacy_adoption: Some(report),
        });
    }

    // Everything that can be refused from the entries alone is refused here,
    // while nothing exists on disk.
    adoption_preflight(&entries)?;

    // Round-2 bug B: `target_existed_before` is a statement about the target
    // *file* (or a named resolution), not about `target_dir`. A preexisting,
    // empty `target_dir` -- no db, possibly holding unrelated content --
    // passes that gate and lands here with `target_existed_before == false`.
    // Failure cleanup must not conflate "this run's writes" with "the whole
    // directory": record, before creating anything, whether this run is the
    // one bringing the directory into existence.
    let target_dir_created_by_this_run = !target_dir.exists();
    std::fs::create_dir_all(&target_dir).map_err(|error| {
        format!(
            "cannot create wiki store directory {}: {error}",
            target_dir.display()
        )
    })?;
    let target_str = target
        .to_str()
        .ok_or_else(|| "wiki store path is not valid UTF-8".to_string())?
        .to_string();

    // From here on every failure is compensated and *recorded* rather than
    // thrown away with the receipt: the run has created a store, and an
    // operator who is handed a bare error string has no evidence of what state
    // the host is in.
    report.target_created = true;
    let outcome = adopt_into_bootstrapped_store(
        &target,
        &target_str,
        &entries,
        &expected_lifecycle_checksum,
        &expected_vector_checksum,
        &legacy_spec,
        &legacy_row_digest_before,
        &mut report,
    );

    if let Err(error) = outcome {
        report.errors.push(error);
        report.had_failures = true;
        // An empty or half-built store is worse than no store: its mere
        // existence flips the named-store gate and silences the zero-store
        // search refusal. Remove what *this run* wrote, and say whether the
        // removal worked. If this run also created `target_dir`, the whole
        // directory is this run's to remove; if the directory preexisted,
        // only the db file (and its WAL/SHM sidecars) this run wrote belong
        // to it -- the directory and anything else inside it is left alone.
        let removal = if target_dir_created_by_this_run {
            std::fs::remove_dir_all(&target_dir)
        } else {
            remove_adopted_store_files(&target)
        };
        match removal {
            Ok(()) => {
                report.target_removed_after_failure = Some(true);
                report.remediation = Some(if target_dir_created_by_this_run {
                    format!(
                        "the store this run created at {} was removed; the legacy global is \
                         untouched, so re-running `tachi wiki corpus --adopt-legacy --confirm {}` \
                         after fixing the reported error is safe",
                        target_dir.display(),
                        WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN
                    )
                } else {
                    format!(
                        "the wiki store file this run wrote at {} was removed; {} preexisted this \
                         run and was left untouched. The legacy global is untouched, so \
                         re-running `tachi wiki corpus --adopt-legacy --confirm {}` after fixing \
                         the reported error is safe",
                        target.display(),
                        target_dir.display(),
                        WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN
                    )
                });
            }
            Err(remove_error) => {
                report.target_removed_after_failure = Some(false);
                report.errors.push(if target_dir_created_by_this_run {
                    format!(
                        "cannot remove the wiki store this run created at {}: {remove_error}",
                        target_dir.display()
                    )
                } else {
                    format!(
                        "cannot remove the wiki store file this run wrote at {}: {remove_error}",
                        target.display()
                    )
                });
                report.remediation = Some(if target_dir_created_by_this_run {
                    format!(
                        "remove {} by hand and re-run `tachi wiki corpus --adopt-legacy --confirm {}`",
                        target_dir.display(),
                        WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN
                    )
                } else {
                    format!(
                        "remove {} by hand (leave {} in place unless you intend to rebuild it) \
                         and re-run `tachi wiki corpus --adopt-legacy --confirm {}`",
                        target.display(),
                        target_dir.display(),
                        WIKI_LEGACY_ADOPTION_CONFIRMATION_TOKEN
                    )
                });
            }
        }
    }

    // Re-derive the specs rather than reusing the pre-run ones: the shared
    // `wiki` spec was resolved before this run created the store, so a reused
    // spec would still carry `addressed_path: None` and the report would deny
    // the existence of the store it had just built.
    let mut scans = store_specs(global_db, project_db, app_home)
        .into_iter()
        .map(inventory_store)
        .collect::<Vec<_>>();
    finalize_classifications(&mut scans);
    for scan in scans.iter_mut() {
        refresh_report(scan);
    }
    if !report.had_failures {
        // B4, measured rather than asserted: how many legacy rows the
        // duplicate-path pass has just pinned to manual_review.
        report.legacy_rows_forced_to_manual_review_by_duplicate_path = Some(
            find_scan(&scans, LogicalStore::LegacyGlobal.reference())
                .map(|scan| {
                    scan.rows
                        .iter()
                        .filter(|row| {
                            row.reasons.iter().any(|reason| {
                                reason == "duplicate_normalized_path_across_logical_stores"
                            })
                        })
                        .count()
                })
                .unwrap_or(0),
        );
    }
    let warnings = scans
        .iter()
        .filter_map(|scan| {
            scan.report
                .read_failure
                .as_ref()
                .map(|failure| format!("{}: {}", scan.report.logical_store_ref, failure.message))
        })
        .collect::<Vec<_>>();

    Ok(WikiCorpusReport {
        version: REPORT_VERSION.to_string(),
        mode: "legacy_adoption".to_string(),
        apply: true,
        stores: scans.into_iter().map(|scan| scan.report).collect(),
        plan: None,
        backup_manifest: None,
        migration_outcomes: Vec::new(),
        warnings,
        sibling_repair: None,
        legacy_adoption: Some(report),
    })
}

/// The confirmed run's write half, factored out so its caller can compensate
/// uniformly for every failure after the store file exists.
#[allow(clippy::too_many_arguments)]
pub(crate) fn adopt_into_bootstrapped_store(
    target: &Path,
    target_str: &str,
    entries: &[memcore::PortableImportEntry],
    expected_lifecycle_checksum: &str,
    expected_vector_checksum: &str,
    legacy_spec: &StoreSpec,
    legacy_row_digest_before: &str,
    report: &mut LegacyAdoptionReport,
) -> Result<(), String> {
    // The only construction that stamps role = "wiki" *and* the full store
    // profile at birth. `stored == 0` on a brand-new file makes this a build,
    // not a migration, so no `MigrationAuthority` is involved.
    let mut store = MemoryStore::open_with_label_and_context(
        target_str,
        memcore::path_router::WIKI_CORPUS_DB_LABEL,
        &memcore::DbOpenContext::create_fresh(),
    )
    .map_err(|error| {
        format!(
            "cannot bootstrap wiki store at {}: {error}",
            target.display()
        )
    })?;
    let stamped = store.is_wiki_corpus_store();
    report.target_store_role_stamped = Some(stamped);
    if !stamped {
        return Err(
            "bootstrapped store is not stamped as the wiki corpus; refusing to import".to_string(),
        );
    }

    let receipt = store
        .import_snapshot_batch(entries)
        .map_err(|error| format!("legacy adoption import failed: {error}"))?;
    drop(store);

    report.rows_imported = receipt.rows_imported;
    report.vectors_imported = receipt.vectors_imported;
    report.vectors_absent = receipt.vectors_absent;
    report.dangling_supersessions = receipt
        .dangling_supersessions
        .iter()
        .map(|edge| AdoptionDanglingEdge {
            id: edge.id.clone(),
            superseded_by: edge.superseded_by.clone(),
        })
        .collect();
    report.observed_lifecycle_checksum = Some(receipt.lifecycle_checksum.clone());
    report.observed_vector_checksum = Some(receipt.vector_checksum.clone());
    let checksums_match = receipt.lifecycle_checksum == expected_lifecycle_checksum
        && receipt.vector_checksum == expected_vector_checksum;
    report.checksums_match = Some(checksums_match);
    if receipt.rows_imported != entries.len() || !checksums_match {
        return Err(format!(
            "legacy adoption receipt mismatch: rows {}/{}, lifecycle {} vs {}, vector {} vs {}",
            receipt.rows_imported,
            entries.len(),
            receipt.lifecycle_checksum,
            expected_lifecycle_checksum,
            receipt.vector_checksum,
            expected_vector_checksum,
        ));
    }

    // `path` is outside the lifecycle checksum, so it gets its own readback.
    verify_adopted_paths(target, entries)?;

    // Independent read of what was written: `open_existing_read_write` opens
    // unlabelled and derives its label from the stamp alone, so this is not a
    // replay of the label the bootstrap claimed.
    let check = MemoryStore::open_existing_read_write(target_str)
        .map_err(|error| format!("cannot reopen the adopted wiki store: {error}"))?;
    let retained = check.is_wiki_corpus_store();
    drop(check);
    report.target_store_role_stamped = Some(retained);
    if !retained {
        return Err("adopted wiki store did not retain its role stamp".to_string());
    }

    // Source-immutability proof: adoption is read-only on the legacy global,
    // so its row digest must be byte-identical to the pre-run value. The
    // legacy spec alone is re-inventoried on purpose -- `row_digest` is
    // computed from the raw rows, before `finalize_classifications`, so it
    // does not depend on which other stores are in the scan set.
    let legacy_after = inventory_store(legacy_spec.clone());
    report.legacy_row_digest_after = Some(legacy_after.row_digest.clone());
    if legacy_after.row_digest != legacy_row_digest_before {
        return Err(format!(
            "legacy global changed during adoption (digest {legacy_row_digest_before} -> {}); the \
             adopted store cannot be qualified against it",
            legacy_after.row_digest
        ));
    }
    Ok(())
}
