use super::apply::*;
use super::classify::*;
use super::fs::*;
use super::plan::*;
use super::types::*;

use memcore::{ExpectedMemoryState, MemoryStore};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Rows worth assessing at all: a migration receipt plus either an archived
/// lifecycle or a `target_noncanonical` phase. Healthy canonical targets
/// (`archived = 0` + `target_copied`) and healthy superseded sources
/// (`archived = 0` + `source_superseded`) are never inspected.
pub(crate) fn is_sibling_damage_candidate(row: &RawRow) -> bool {
    if row.metadata.get(RECEIPT_KEY).is_none() {
        return false;
    }
    if row.archived {
        return true;
    }
    matches!(
        parse_migration_receipt(row),
        Ok(Some(receipt)) if receipt.phase == MigrationPhase::TargetNoncanonical
    )
}

/// Decide whether `target_row` carries the exact terminal damage a sibling
/// race used to leave behind:
///
/// * the row is archived, unsuperseded, and carries a `target_noncanonical`
///   receipt whose plan/item identity fields are complete and bind this row,
/// * the row's immutable copy identity still equals the receipt item's,
/// * the receipt item's source row exists, is superseded **by this row**, and
///   carries a `source_superseded` receipt for the same plan and item.
///
/// Anything else is reported with every failed clause and left untouched:
/// under-repairing is recoverable, resurrecting the wrong row is not.
///
/// Deliberately absent from this signature: `PlanItem.source_physical_id` /
/// `target_physical_id`. Those are `unix:{device}:{inode}` (see
/// `physical_db_identity.rs`), and restoring a store from a backup -- the
/// legitimate route back to this exact damage -- gives the restored file a
/// new inode. Binding the repair signature to physical identity would turn
/// every ordinary backup-restore into a false refusal; the row/plan/item
/// identity and content-binding clauses above are what actually distinguish
/// this damaged row from any other, so physical identity adds no
/// discriminating power here.
pub(crate) fn assess_sibling_damage(
    target_store_ref: &str,
    target_row: &RawRow,
    source_lookup: impl FnOnce(&PlanItem) -> Result<Option<RawRow>, String>,
) -> SiblingDamageAssessment {
    let mut reasons = Vec::new();
    let receipt = match parse_migration_receipt(target_row) {
        Ok(Some(receipt)) => receipt,
        Ok(None) => {
            return SiblingDamageAssessment::Skipped(vec![
                "row carries no Wiki corpus migration receipt".to_string(),
            ])
        }
        Err(error) => return SiblingDamageAssessment::Skipped(vec![error]),
    };
    if receipt.phase != MigrationPhase::TargetNoncanonical {
        reasons.push(format!(
            "receipt phase is {}, not target_noncanonical",
            receipt.phase.as_str()
        ));
    }
    if !target_row.archived {
        reasons.push("row is not archived".to_string());
    }
    if let Some(superseded_by) = target_row.superseded_by.as_deref() {
        reasons.push(format!("row is itself superseded by {superseded_by}"));
    }
    let item = &receipt.item;
    if receipt.plan_id.is_empty() {
        reasons.push("receipt has no plan id".to_string());
    }
    if item.action != "copy_to_shared_and_supersede" {
        reasons.push(format!(
            "receipt item action is {}, not copy_to_shared_and_supersede",
            item.action
        ));
    }
    if item.target_store_ref != target_store_ref
        || item.target_id.as_deref() != Some(target_row.id.as_str())
    {
        reasons.push(format!(
            "receipt item target identity {}:{} does not bind this row",
            item.target_store_ref,
            item.target_id.as_deref().unwrap_or("<none>")
        ));
    }
    if item.source_id.is_empty()
        || item.source_store_ref.is_empty()
        || item.source_copy_identity_sha256.is_empty()
    {
        reasons.push("receipt item source identity fields are incomplete".to_string());
    }
    if item.source_store_ref == target_store_ref {
        reasons.push("receipt item source and target are the same logical store".to_string());
    }
    if target_row.copy_identity_sha256() != item.source_copy_identity_sha256 {
        reasons.push(format!(
            "row copy identity {} does not match the receipt item's {}",
            target_row.copy_identity_sha256(),
            item.source_copy_identity_sha256
        ));
    }
    match source_lookup(item) {
        Err(error) => reasons.push(error),
        Ok(None) => reasons.push(format!(
            "receipt item source row {}:{} is absent",
            item.source_store_ref, item.source_id
        )),
        Ok(Some(source_row)) => {
            if !source_row.is_superseded_by(&target_row.id) {
                reasons.push(format!(
                    "source {}:{} is superseded by {}, not by this row",
                    item.source_store_ref,
                    item.source_id,
                    source_row.superseded_by.as_deref().unwrap_or("<nothing>")
                ));
            }
            if !receipt_matches(&source_row, item, &receipt.plan_id, &["source_superseded"]) {
                reasons.push(format!(
                    "source {}:{} does not carry a source_superseded receipt for the same plan item",
                    item.source_store_ref, item.source_id
                ));
            }
        }
    }
    if !reasons.is_empty() {
        return SiblingDamageAssessment::Skipped(reasons);
    }
    let replaced_receipt = match target_row.metadata.get(RECEIPT_KEY) {
        Some(value) => value.clone(),
        None => {
            return SiblingDamageAssessment::Skipped(vec![
                "row lost its migration receipt while it was being assessed".to_string(),
            ])
        }
    };
    SiblingDamageAssessment::Repairable(Box::new(SiblingDamageSignature {
        item: item.clone(),
        plan_id: receipt.plan_id.clone(),
        replaced_receipt,
    }))
}

/// Look a receipt item's source row up in the current inventory.
pub(crate) fn inventory_source_lookup(
    scans: &[StoreScan],
    item: &PlanItem,
) -> Result<Option<RawRow>, String> {
    let source = find_scan(scans, &item.source_store_ref).map_err(|_| {
        format!(
            "receipt item source store {} is not in this inventory",
            item.source_store_ref
        )
    })?;
    Ok(source.raw_row(&item.source_id).cloned())
}

/// Read a receipt item's source row directly from its opened store, including
/// its supersession edge, so the repair decision is taken on fresh state.
pub(crate) fn store_source_lookup(
    source_store: &MemoryStore,
    item: &PlanItem,
) -> Result<Option<RawRow>, String> {
    let Some(entry) = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut row = raw_from_entry(&entry);
    row.superseded_by = source_store
        .supersession_target(&item.source_id)
        .map_err(|error| error.to_string())?
        .flatten();
    Ok(Some(row))
}

pub(crate) fn sibling_repair_row(
    scan: &StoreScan,
    row: &RawRow,
    outcome: &str,
    signature: Option<&SiblingDamageSignature>,
    skipped_reasons: Vec<String>,
) -> SiblingRepairRow {
    let receipt = parse_migration_receipt(row).ok().flatten();
    let item = signature
        .map(|signature| &signature.item)
        .or_else(|| receipt.as_ref().map(|receipt| &receipt.item));
    SiblingRepairRow {
        store_ref: scan.spec.logical_store.reference().to_string(),
        id: row.id.clone(),
        path: row.path.clone(),
        outcome: outcome.to_string(),
        observed_receipt_phase: receipt
            .as_ref()
            .map(|receipt| receipt.phase.as_str().to_string()),
        observed_archived: row.archived,
        observed_superseded_by: row.superseded_by.clone(),
        observed_copy_identity_sha256: row.copy_identity_sha256(),
        expected_receipt_phase: MigrationPhase::TargetNoncanonical.as_str().to_string(),
        expected_archived: true,
        expected_copy_identity_sha256: item.map(|item| item.source_copy_identity_sha256.clone()),
        plan_id: signature
            .map(|signature| signature.plan_id.clone())
            .or_else(|| receipt.as_ref().map(|receipt| receipt.plan_id.clone())),
        source_store_ref: item.map(|item| item.source_store_ref.clone()),
        source_id: item.map(|item| item.source_id.clone()),
        skipped_reasons,
        failure_reason: None,
    }
}

/// Metadata for the compensated row.
///
/// The migration receipt is restored to the byte-identical `target_copied`
/// value a clean migration would have written, because every completion
/// predicate (`canonical_target_matches_plan`, `plan_item_completed`,
/// `validate_target_receipt`) is defined against exactly that phase. The
/// replaced `target_noncanonical` receipt is appended to a separate repair
/// history key instead of being erased.
pub(crate) fn metadata_with_repaired_receipt(
    row: &RawRow,
    signature: &SiblingDamageSignature,
) -> Result<Value, String> {
    let mut metadata = row
        .metadata
        .as_object()
        .cloned()
        .ok_or_else(|| "repair requires object-shaped metadata".to_string())?;
    let record = json!({
        "kind": REPAIR_RECEIPT_KEY,
        "version": REPAIR_RECEIPT_VERSION,
        "phase": REPAIR_PHASE,
        "plan_id": signature.plan_id,
        "target_store_ref": signature.item.target_store_ref,
        "target_id": signature.item.target_id,
        "source_store_ref": signature.item.source_store_ref,
        "source_id": signature.item.source_id,
        "restored_phase": MigrationPhase::TargetCopied.as_str(),
        "replaced_receipt": signature.replaced_receipt,
    });
    let mut history = match metadata.get(REPAIR_RECEIPT_KEY) {
        Some(Value::Array(existing)) => existing.clone(),
        Some(other) => vec![other.clone()],
        None => Vec::new(),
    };
    history.push(record);
    metadata.insert(REPAIR_RECEIPT_KEY.to_string(), Value::Array(history));
    metadata.insert(
        RECEIPT_KEY.to_string(),
        receipt_value(&signature.item, &signature.plan_id, "target_copied"),
    );
    Ok(Value::Object(metadata))
}

/// Compensate one damaged target under a complete-state CAS.
///
/// The signature is re-proven against a fresh read of the target row and its
/// source inside the retry loop, and the un-archive plus the receipt rewrite
/// land in a single transaction, so a concurrent writer either loses the CAS
/// (retry) or changes the state away from the signature (refusal). The row is
/// re-read afterwards and must satisfy `canonical_target_matches_plan`.
pub(crate) fn repair_sibling_damaged_target(
    target_scan: &StoreScan,
    source_scan: &StoreScan,
    signature: &SiblingDamageSignature,
    retained_backups: &[RetainedBackup],
) -> Result<(), String> {
    let target_id = signature
        .item
        .target_id
        .as_deref()
        .ok_or_else(|| "repairable signature without a deterministic target id".to_string())?;
    let target_path = target_scan
        .spec
        .addressed_path
        .as_ref()
        .ok_or_else(|| "repair target has no addressed path".to_string())?;
    let source_path = source_scan
        .spec
        .addressed_path
        .as_ref()
        .ok_or_else(|| "repair source has no addressed path".to_string())?;
    check_authority(target_scan, Some(source_path))?;
    check_authority(source_scan, Some(target_path))?;
    let mut target_store = open_apply_store(target_scan)?;
    let source_store = open_apply_store(source_scan)?;
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;

    for _ in 0..3 {
        verify_retained_backups(retained_backups)?;
        let entry = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("damaged target {target_id} disappeared before repair"))?;
        let mut row = raw_from_entry(&entry);
        row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        let fresh =
            assess_sibling_damage(target_scan.spec.logical_store.reference(), &row, |item| {
                store_source_lookup(&source_store, item)
            });
        let fresh = match fresh {
            SiblingDamageAssessment::Repairable(fresh) => fresh,
            SiblingDamageAssessment::Skipped(reasons) => {
                return Err(format!(
                    "damaged target {target_id} no longer matches the repair signature: {}",
                    reasons.join("; ")
                ))
            }
        };
        if fresh.item != signature.item || fresh.plan_id != signature.plan_id {
            return Err(format!(
                "damaged target {target_id} changed migration provenance before repair"
            ));
        }
        let metadata = metadata_with_repaired_receipt(&row, &fresh)?;
        let expected = ExpectedMemoryState::from_entry(&entry, row.superseded_by.as_deref());
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        if !target_store
            .restore_with_metadata_if_expected_state(target_id, &metadata, &expected)
            .map_err(|error| error.to_string())?
        {
            continue;
        }
        let restored = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("repaired target {target_id} disappeared after repair"))?;
        let mut restored_row = raw_from_entry(&restored);
        restored_row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        if !canonical_target_matches_plan(&restored_row, &signature.item, &signature.plan_id) {
            return Err(format!(
                "repair of {target_id} did not produce the canonical migrated target"
            ));
        }
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        return Ok(());
    }
    Err(format!(
        "damaged target {target_id} kept changing during sibling-damage repair"
    ))
}

/// Repair backups are named per physical store *and* per captured state: a
/// migration backup of the same store is a different file, and a later repair
/// run captures its own snapshot instead of failing to verify an older one
/// against the state it no longer describes.
pub(crate) fn repair_backup_file_name(physical_id: &str, row_digest: &str) -> String {
    format!(
        "wiki-corpus-repair-v1-{}-{}.db",
        digest_string(physical_id),
        row_digest
    )
}

/// Back up every physical store the repair is about to mutate.
///
/// The expected evidence is the store's *current* fingerprint (the repair has
/// no plan), and the file name is repair-specific so an existing migration
/// backup of a different state is never mistaken for this one.
pub(crate) fn retain_repair_backups(
    scans: &[StoreScan],
    mutated_store_refs: &BTreeSet<String>,
    backup_dir: &Path,
) -> Result<Vec<RetainedBackup>, String> {
    let mut groups = BTreeMap::<String, (Vec<String>, &StoreScan)>::new();
    for scan in scans
        .iter()
        .filter(|scan| mutated_store_refs.contains(scan.spec.logical_store.reference()))
    {
        let Some(physical) = scan.physical.as_ref() else {
            return Err(format!(
                "repair refuses store {} without physical identity",
                scan.spec.logical_store.reference()
            ));
        };
        let entry = groups
            .entry(physical.physical_id.clone())
            .or_insert_with(|| (Vec::new(), scan));
        entry
            .0
            .push(scan.spec.logical_store.reference().to_string());
    }
    let mut backups = Vec::new();
    for (physical_id, (mut logical_refs, source)) in groups {
        logical_refs.sort();
        logical_refs.dedup();
        check_authority(source, None)?;
        let expected = source.fingerprint().ok_or_else(|| {
            format!(
                "repair cannot fingerprint {} before backup",
                source.spec.logical_store.reference()
            )
        })?;
        let mut race_hook = None;
        backups.push(create_or_verify_backup_with_hook(
            source,
            backup_dir,
            &repair_backup_file_name(&physical_id, &expected.row_digest),
            logical_refs,
            &expected,
            &mut race_hook,
        )?);
    }
    backups.sort_by(|left, right| {
        left.receipt
            .source_physical_id
            .cmp(&right.receipt.source_physical_id)
    });
    verify_retained_backups(&backups)?;
    Ok(backups)
}

/// Assess (and, when confirmed, compensate) every sibling-damaged row.
///
/// A confirmed run never discards a partially completed report: each row's
/// repair is its own atomic, idempotent transaction (see
/// `repair_sibling_damaged_target`), so a row whose compensation fails is
/// recorded as `failed` -- with its reason in both that row and the
/// top-level `errors` -- instead of unwinding the rows already repaired
/// earlier in the same run. Setup failures that happen before any row is
/// touched (the backup directory itself cannot be created or verified) are
/// still returned as `Err`: at that point there is no partial repair to
/// report.
pub(crate) fn repair_sibling_damage(
    scans: &[StoreScan],
    backup_dir: Option<&Path>,
) -> Result<SiblingRepairReport, String> {
    let mut rows = Vec::new();
    let mut repairs = Vec::new();
    for (index, scan) in scans.iter().enumerate() {
        for row in &scan.rows {
            if !is_sibling_damage_candidate(row) {
                continue;
            }
            match assess_sibling_damage(scan.spec.logical_store.reference(), row, |item| {
                inventory_source_lookup(scans, item)
            }) {
                SiblingDamageAssessment::Repairable(signature) => {
                    rows.push(sibling_repair_row(
                        scan,
                        row,
                        "repairable",
                        Some(&*signature),
                        Vec::new(),
                    ));
                    repairs.push((index, rows.len() - 1, signature));
                }
                SiblingDamageAssessment::Skipped(reasons) => {
                    rows.push(sibling_repair_row(scan, row, "skipped", None, reasons));
                }
            }
        }
    }
    let repairable = repairs.len();
    let mut report = SiblingRepairReport {
        version: REPAIR_REPORT_VERSION.to_string(),
        confirmed: backup_dir.is_some(),
        backup_directory: backup_dir.map(|dir| dir.display().to_string()),
        inspected_rows: rows.len(),
        repairable,
        repaired: 0,
        skipped: rows.len() - repairable,
        failed: 0,
        had_failures: false,
        backups: Vec::new(),
        rows,
        errors: Vec::new(),
    };
    let Some(backup_dir) = backup_dir else {
        return Ok(report);
    };
    if repairs.is_empty() {
        return Ok(report);
    }

    let mutated_store_refs = repairs
        .iter()
        .map(|(index, _, _)| scans[*index].spec.logical_store.reference().to_string())
        .collect::<BTreeSet<_>>();
    let retained_backups = retain_repair_backups(scans, &mutated_store_refs, backup_dir)?;
    for (index, row_index, signature) in &repairs {
        let target_scan = &scans[*index];
        let store_ref = report.rows[*row_index].store_ref.clone();
        let id = report.rows[*row_index].id.clone();
        let outcome = find_scan(scans, &signature.item.source_store_ref).and_then(|source_scan| {
            repair_sibling_damaged_target(target_scan, source_scan, signature, &retained_backups)
        });
        match outcome {
            Ok(()) => {
                report.rows[*row_index].outcome = "repaired".to_string();
                report.repaired += 1;
            }
            Err(error) => {
                report.errors.push(format!("{store_ref}:{id}: {error}"));
                report.rows[*row_index].outcome = "failed".to_string();
                report.rows[*row_index].failure_reason = Some(error);
            }
        }
    }
    report.failed = report
        .rows
        .iter()
        .filter(|row| row.outcome == "failed")
        .count();
    report.had_failures = report.failed > 0;
    // The final integrity check is orthogonal to any individual row's
    // outcome (it detects a backup file tampered with mid-run, not a row
    // that failed to compensate), so it folds into `errors` too instead of
    // discarding a report whose row outcomes are otherwise trustworthy.
    match verify_retained_backups(&retained_backups) {
        Ok(()) => {
            report.backups = retained_backups
                .iter()
                .map(|backup| backup.receipt.clone())
                .collect();
        }
        Err(error) => {
            report.errors.push(error);
            report.had_failures = true;
        }
    }
    Ok(report)
}

/// `tachi wiki corpus --repair-sibling-damage`.
///
/// In-band compensation for the terminal state the sibling-worker race used to
/// leave behind: a deterministic target archived with a `target_noncanonical`
/// receipt whose source row is already superseded by it, which the user read
/// surface (`archived = 0 AND superseded_by IS NULL`) hides and which every
/// later apply dead-ends on with an occupant collision.
///
/// It is its own confirmed mode with its own token, and without `--confirm` it
/// is a pure dry run that opens no store for writing.
///
/// Trust boundary: the repair signature is judged entirely against the
/// `wiki_corpus_migration` receipt metadata written under `RECEIPT_KEY`, and
/// that key is not in memcore's reserved-metadata list (`memory_crud.rs`'s
/// `RESERVED_REFERENCE_KEYS` / `RESERVED_REM_KEY` / `RESERVED_WIKI_LOG_KEY`),
/// so an ordinary memory write can set it. That does not hand a forger any
/// new capability by itself -- forging a convincing repair still requires
/// planting an id-bound `target_noncanonical` receipt on an archived,
/// unsuperseded row *and* a matching `source_superseded` receipt plus a live
/// supersession edge on the row it names as source, in two separate stores,
/// consistent with each other. But this is the first verb in this module
/// that *un-archives a row* on the strength of that metadata alone, so it is
/// the first place that consistency is worth spelling out rather than
/// assuming.
pub(crate) fn run_wiki_corpus_sibling_repair_command(
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
            "--repair-sibling-damage is its own confirmed mode and cannot be combined with --apply"
                .to_string(),
        );
    }
    if plan_path.is_some() {
        return Err(
            "--repair-sibling-damage does not take --plan; each repair is derived from the damaged row's own migration receipt"
                .to_string(),
        );
    }
    let confirmed = match confirm.as_deref() {
        None => {
            if backup_dir.is_some() {
                return Err(format!(
                    "--backup-dir requires --confirm {}",
                    WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN
                ));
            }
            false
        }
        Some(token) if token == WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN => true,
        Some(_) => {
            return Err(format!(
                "sibling-damage repair requires exact --confirm {}",
                WIKI_CORPUS_REPAIR_CONFIRMATION_TOKEN
            ))
        }
    };
    let backup_dir = if confirmed {
        let backup_dir = backup_dir
            .ok_or_else(|| "sibling-damage repair requires explicit --backup-dir".to_string())?;
        if regular_directory_metadata(&backup_dir).is_err() {
            return Err(format!(
                "sibling-damage repair requires an existing backup directory: {}",
                backup_dir.display()
            ));
        }
        Some(backup_dir)
    } else {
        None
    };

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

    let repair = if confirmed {
        validate_apply_inventory(&scans)?;
        let current = reinventory_apply_scans(&scans);
        validate_apply_inventory(&current)?;
        let repair = repair_sibling_damage(&current, backup_dir.as_deref())?;
        // Report the healed inventory, not the damaged snapshot the run started
        // from, so `stores` and the report agree with each other.
        scans = reinventory_apply_scans(&current);
        repair
    } else {
        repair_sibling_damage(&scans, None)?
    };

    Ok(WikiCorpusReport {
        version: REPORT_VERSION.to_string(),
        mode: if confirmed {
            "sibling_repair".to_string()
        } else {
            "sibling_repair_preview".to_string()
        },
        apply: confirmed,
        stores: scans.into_iter().map(|scan| scan.report).collect(),
        plan: None,
        backup_manifest: None,
        migration_outcomes: Vec::new(),
        warnings,
        sibling_repair: Some(repair),
        legacy_adoption: None,
    })
}

// ---------------------------------------------------------------------------
// `tachi wiki corpus --adopt-legacy` (tachi#1624)
// ---------------------------------------------------------------------------
//
// Bootstrap `<home>/projects/wiki/tachi-memory.db` and adopt the eligible
// legacy-global rows into it verbatim.
//
// # Why this is a separate mode from `--apply`
//
// `--apply` reconciles two stores that already exist. It refuses any absent
// involved store (`validate_apply_inventory`), `store_specs` has no create
// branch, and `build_plan` only emits items for `SharedCandidate` rows — of
// which a host whose corpus is entirely `manual_review` has none. It
// reconciles; it cannot bootstrap and it cannot move this corpus. That gap,
// not a policy disagreement, is why this verb exists.
//
// # What it asserts about the adopted rows: nothing
//
// No `knowledge_scope`, no `applies_to`, no `origin_projects`, no review
// receipt. Stamping `knowledge_scope=shared` on an unreviewed row would make
// the row *less* retrievable, not more (`shared_active_without_review` forces
// `PendingReview`), so the only honest stamp is no stamp. The single metadata
// delta is the additive, nested `wiki_legacy_adoption_v1` key, which every
// derive path ignores.
//
// # Known limitation this mode ships with, deliberately (tachi#1611 phase 5)
//
// Adoption copies rows without touching the legacy source, so after it runs
// every adopted normalized path exists in **two** logical stores.
// `finalize_classifications` forces both copies of any cross-store duplicate
// path to `ManualReview`, and `build_plan` only emits `SharedCandidate` items.
// The reconciler is therefore inert for every adopted path until one side is
// deleted: phase 5's *first* step must be choosing and removing a side. This
// is reported in `legacy_adoption.reconciler_impact` and measured in
// `legacy_adoption.legacy_rows_forced_to_manual_review_by_duplicate_path`
// rather than left for a later reader to rediscover.
