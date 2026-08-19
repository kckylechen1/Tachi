use super::classify::*;
use super::fs::*;
use super::plan::*;
use super::types::*;

use memcore::{ExpectedMemoryState, InsertMemoryResult, MemoryEntry, MemoryStore};
use rusqlite::Connection;
use std::path::Path;

pub(crate) fn maybe_interrupt(
    interruption: &mut Option<MigrationInterruption>,
    item: &PlanItem,
    boundary: MigrationBoundary,
) -> Result<(), String> {
    let matches = interruption.as_ref().is_some_and(|pending| {
        pending.boundary == boundary
            && pending
                .source_id
                .as_deref()
                .is_none_or(|source_id| source_id == item.source_id)
    });
    if matches {
        interruption.take();
        return Err(format!(
            "simulated interruption after {:?} for {}:{}",
            boundary, item.source_store_ref, item.source_id
        ));
    }
    Ok(())
}

pub(crate) fn check_authority(scan: &StoreScan, target: Option<&Path>) -> Result<(), String> {
    let physical = scan
        .physical
        .as_ref()
        .ok_or_else(|| "physical identity disappeared before mutation".to_string())?;
    let authority = physical
        .mutation_authority
        .as_ref()
        .ok_or_else(|| "mutation authority disappeared before mutation".to_string())?;
    authority.revalidate_for_mutation(target)
}

pub(crate) fn open_apply_store(scan: &StoreScan) -> Result<MemoryStore, String> {
    let path = scan
        .physical
        .as_ref()
        .ok_or_else(|| "apply store has no physical identity".to_string())?
        .primary_path
        .clone();
    MemoryStore::open_existing_read_write(&path).map_err(|error| error.to_string())
}

pub(crate) fn verify_opened_apply_store(
    store: &MemoryStore,
    logical_path: &Path,
    role: &str,
) -> Result<(), String> {
    store
        .verify_opened_physical_db_identity(logical_path)
        .map_err(|error| format!("{role} store detached from logical path: {error}"))
}

pub(crate) fn verify_copy_store_identities(
    source_store: &MemoryStore,
    source_path: &Path,
    target_store: &MemoryStore,
    target_path: &Path,
) -> Result<(), String> {
    verify_opened_apply_store(source_store, source_path, "source")?;
    verify_opened_apply_store(target_store, target_path, "target")
}

pub(crate) fn maybe_swap_opened_store_path(
    race_hook: &mut Option<CorpusRaceHook>,
    store: LogicalStore,
    logical_path: &Path,
) -> Result<(), String> {
    let replacement_path = match race_hook.as_ref() {
        Some(CorpusRaceHook::SwapOpenedStorePath {
            store: hook_store,
            replacement_path,
        }) if *hook_store == store => replacement_path.clone(),
        _ => return Ok(()),
    };
    race_hook.take();
    atomic_exchange_paths(logical_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject opened-store path replacement for {}: {error}",
            logical_path.display()
        )
    })
}

pub(crate) fn maybe_swap_logical_path_after_inventory(
    scans: &[StoreScan],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let (store, replacement_path) = match race_hook.as_ref() {
        Some(CorpusRaceHook::SwapLogicalPathAfterInventory {
            store,
            replacement_path,
        }) => (*store, replacement_path.clone()),
        _ => return Ok(()),
    };
    let logical_path = scans
        .iter()
        .find(|scan| scan.spec.logical_store == store)
        .and_then(|scan| scan.spec.addressed_path.as_deref())
        .ok_or_else(|| format!("race hook cannot find logical store {}", store.reference()))?;
    race_hook.take();
    atomic_exchange_paths(logical_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject post-inventory path replacement for {}: {error}",
            logical_path.display()
        )
    })
}

pub(crate) fn maybe_swap_reclassification_after_receipt_read(
    item: &PlanItem,
    logical_path: &Path,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let replacement_path = match race_hook.as_ref() {
        Some(CorpusRaceHook::SwapReclassificationPathAfterReceiptRead {
            source_id,
            replacement_path,
        }) if source_id == &item.source_id => replacement_path.clone(),
        _ => return Ok(()),
    };
    race_hook.take();
    atomic_exchange_paths(logical_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject reclassification path replacement for {}: {error}",
            logical_path.display()
        )
    })
}

pub(crate) fn maybe_swap_logical_path_before_completed_return(
    scans: &[StoreScan],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let (store, replacement_path) = match race_hook.as_ref() {
        Some(CorpusRaceHook::SwapLogicalPathBeforeCompletedReturn {
            store,
            replacement_path,
        }) => (*store, replacement_path.clone()),
        _ => return Ok(()),
    };
    let logical_path = scans
        .iter()
        .find(|scan| scan.spec.logical_store == store)
        .and_then(|scan| scan.spec.addressed_path.as_deref())
        .ok_or_else(|| format!("race hook cannot find logical store {}", store.reference()))?;
    race_hook.take();
    atomic_exchange_paths(logical_path, &replacement_path).map_err(|error| {
        format!(
            "cannot inject completed-return path replacement for {}: {error}",
            logical_path.display()
        )
    })
}

pub(crate) fn maybe_archive_target_before_completed_return(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::ArchiveTargetBeforeCompletedReturn { source_id }) => source_id.clone(),
        _ => return Ok(()),
    };
    let item = plan
        .items
        .iter()
        .find(|item| item.source_id == source_id && item.action == "copy_to_shared_and_supersede")
        .ok_or_else(|| format!("race hook cannot find copy item for source {source_id}"))?;
    let target_id = item
        .target_id
        .as_deref()
        .ok_or_else(|| "race hook copy item has no target id".to_string())?;
    let target_scan = find_scan(scans, &item.target_store_ref)?;
    let target_path = target_scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook target has no addressed path".to_string())?;
    race_hook.take();
    let target_store = MemoryStore::open_existing_read_write(&target_path.display().to_string())
        .map_err(|error| error.to_string())?;
    let target_entry = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("race hook target {target_id} disappeared"))?;
    if !target_store
        .archive_memory_if_revision(target_id, target_entry.revision)
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "race hook could not archive target {target_id} at revision {}",
            target_entry.revision
        ));
    }
    Ok(())
}

pub(crate) fn maybe_mutate_reclassification_after_plan_validation(
    scans: &[StoreScan],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::MutateReclassificationAfterPlanValidation { source_id }) => {
            source_id.clone()
        }
        _ => return Ok(()),
    };
    let scan = scans
        .iter()
        .find(|scan| scan.raw_row(&source_id).is_some())
        .ok_or_else(|| format!("race hook cannot find reclassification row {source_id}"))?;
    let logical_path = scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook reclassification store has no addressed path".to_string())?;
    let revision = scan
        .raw_row(&source_id)
        .expect("row selected above")
        .revision;
    race_hook.take();
    let mut store = MemoryStore::open_existing_read_write(&logical_path.display().to_string())
        .map_err(|error| error.to_string())?;
    let keywords = vec!["post-validation".to_string(), "enrichment".to_string()];
    let vector = vec![0.91_f32; 1024];
    let changed = store
        .update_enrichment_fields(
            &source_id,
            Some("post-validation generated summary"),
            Some(&vector),
            Some(&keywords),
            None,
            revision,
        )
        .map_err(|error| error.to_string())?;
    if !changed {
        return Err(format!(
            "race hook could not mutate reclassification row {source_id}"
        ));
    }
    Ok(())
}

pub(crate) fn maybe_mutate_valid_until_after_plan_validation(
    scans: &[StoreScan],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::MutateValidUntilAfterPlanValidation { source_id }) => {
            source_id.clone()
        }
        _ => return Ok(()),
    };
    let scan = scans
        .iter()
        .find(|scan| scan.raw_row(&source_id).is_some())
        .ok_or_else(|| format!("race hook cannot find valid_until row {source_id}"))?;
    let logical_path = scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook valid_until store has no addressed path".to_string())?;
    race_hook.take();
    let conn = Connection::open(logical_path).map_err(|error| error.to_string())?;
    let changed = conn
        .execute(
            "UPDATE memories SET valid_until = ?1 WHERE id = ?2",
            rusqlite::params!["2026-12-31T23:59:59Z", source_id],
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("race hook could not mutate valid_until".to_string());
    }
    Ok(())
}

pub(crate) fn maybe_mutate_copy_source_after_precheck(
    source_scan: &StoreScan,
    item: &PlanItem,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::MutateCopySourceAfterPrecheck { source_id })
            if source_id == &item.source_id =>
        {
            source_id.clone()
        }
        _ => return Ok(()),
    };
    let logical_path = source_scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook copy source has no addressed path".to_string())?;
    race_hook.take();
    let mut store = MemoryStore::open_existing_read_write(&logical_path.display().to_string())
        .map_err(|error| error.to_string())?;
    let vector = vec![0.83_f32; 1024];
    let changed = store
        .update_enrichment_fields(
            &source_id,
            Some("post-precheck generated summary"),
            Some(&vector),
            None,
            None,
            item.source_revision,
        )
        .map_err(|error| error.to_string())?;
    if !changed {
        return Err(format!(
            "race hook could not mutate copy source {source_id}"
        ));
    }
    Ok(())
}

/// Run the whole plan item to completion the way a sibling worker applying the
/// same deterministic plan would: claim the deterministic target with the
/// `target_copied` receipt, then supersede the source with the
/// `source_superseded` receipt. Both writes go through the same store seams the
/// production path uses, with this plan's real `target_id`, so the store is
/// left in exactly the state a concurrent operator run produces.
pub(crate) fn maybe_complete_item_as_sibling_worker(
    source_scan: &StoreScan,
    target_scan: &StoreScan,
    item: &PlanItem,
    plan_id: &str,
    target_id: &str,
    seam: SiblingCompletionSeam,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    match race_hook.as_ref() {
        Some(CorpusRaceHook::SiblingWorkerCompletesItem {
            source_id,
            seam: hook_seam,
        }) if source_id == &item.source_id && *hook_seam == seam => {}
        _ => return Ok(()),
    }
    race_hook.take();
    let source_path = source_scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook source has no addressed path".to_string())?;
    let target_path = target_scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "race hook target has no addressed path".to_string())?;
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    let mut source_store =
        MemoryStore::open_existing_read_write(&source_path.display().to_string())
            .map_err(|error| error.to_string())?;
    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    let source_store = MemoryStore::open_existing_read_write(&source_path.display().to_string())
        .map_err(|error| error.to_string())?;
    let mut target_store =
        MemoryStore::open_existing_read_write(&target_path.display().to_string())
            .map_err(|error| error.to_string())?;
    let source_entry = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "race hook sibling source row disappeared".to_string())?;
    let source_row = raw_from_entry(&source_entry);
    let mut target_entry = source_entry.clone();
    target_entry.id = target_id.to_string();
    target_entry.metadata =
        metadata_with_receipt(&source_row, receipt_value(item, plan_id, "target_copied"))?;
    match target_store
        .insert_if_absent(&target_entry)
        .map_err(|error| error.to_string())?
    {
        InsertMemoryResult::Inserted | InsertMemoryResult::Existing => {}
    }
    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    {
        let production_result = Err::<(), String>(
            "cross-store wiki race hook cannot perform semantic supersession in a production build"
                .to_string(),
        );
        production_result?;
    }
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    {
        let final_metadata = metadata_with_receipt(
            &source_row,
            receipt_value(item, plan_id, "source_superseded"),
        )?;
        let expected = ExpectedMemoryState::from_entry(&source_entry, None);
        if !source_store
            .supersede_with_metadata_if_expected_state(
                &item.source_id,
                target_id,
                &final_metadata,
                &expected,
            )
            .map_err(|error| error.to_string())?
        {
            return Err("race hook sibling worker could not supersede the source".to_string());
        }
    }
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    let final_result: Result<(), String> = Ok(());
    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    let final_result: Result<(), String> = Ok(());
    final_result
}

#[cfg(any(test, feature = "bootstrap-test-api"))]
pub(crate) fn maybe_inject_copy_after_receipt_prepared(
    source_scan: &StoreScan,
    target_scan: &StoreScan,
    item: &PlanItem,
    target_id: &str,
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<(), String> {
    let source_id = match race_hook.as_ref() {
        Some(CorpusRaceHook::MutateSourceAfterReceiptPrepared { source_id })
        | Some(CorpusRaceHook::MutateTargetAfterVerification { source_id })
        | Some(CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition { source_id })
            if source_id == &item.source_id =>
        {
            source_id.clone()
        }
        _ => return Ok(()),
    };
    let hook = race_hook.take().expect("matched hook exists");
    if matches!(
        hook,
        CorpusRaceHook::MutateSourceAfterReceiptPrepared { .. }
            | CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition { .. }
    ) {
        let target_path = target_scan
            .spec
            .addressed_path
            .as_deref()
            .ok_or_else(|| "race hook target has no addressed path".to_string())?;
        let conn = Connection::open(target_path).map_err(|error| error.to_string())?;
        conn.execute_batch(
            "CREATE TRIGGER wiki_corpus_no_hard_delete
             BEFORE DELETE ON memories
             BEGIN
               SELECT RAISE(ABORT, 'wiki corpus hard delete forbidden');
             END;",
        )
        .map_err(|error| error.to_string())?;
    }
    match hook {
        CorpusRaceHook::MutateSourceAfterReceiptPrepared { .. } => {
            let source_path = source_scan
                .spec
                .addressed_path
                .as_deref()
                .ok_or_else(|| "race hook source has no addressed path".to_string())?;
            let mut store =
                MemoryStore::open_existing_read_write(&source_path.display().to_string())
                    .map_err(|error| error.to_string())?;
            let vector = vec![0.77_f32; 1024];
            if !store
                .update_enrichment_fields(
                    &source_id,
                    Some("source enriched after receipt preparation"),
                    Some(&vector),
                    None,
                    None,
                    item.source_revision,
                )
                .map_err(|error| error.to_string())?
            {
                return Err("race hook could not enrich source".to_string());
            }
        }
        CorpusRaceHook::MutateTargetAfterVerification { .. } => {
            let target_path = target_scan
                .spec
                .addressed_path
                .as_deref()
                .ok_or_else(|| "race hook target has no addressed path".to_string())?;
            let mut store =
                MemoryStore::open_existing_read_write(&target_path.display().to_string())
                    .map_err(|error| error.to_string())?;
            let target = store
                .get_with_options(target_id, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "race hook target disappeared".to_string())?;
            let vector = vec![0.66_f32; 1024];
            let keywords = vec!["target".to_string(), "enriched".to_string()];
            if !store
                .update_enrichment_fields(
                    target_id,
                    Some("target enriched after verification"),
                    Some(&vector),
                    Some(&keywords),
                    None,
                    target.revision,
                )
                .map_err(|error| error.to_string())?
            {
                return Err("race hook could not enrich target".to_string());
            }
            let enriched = store
                .get_with_options(target_id, true)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "race hook enriched target disappeared".to_string())?;
            let mut metadata = enriched.metadata.clone();
            metadata["enrichment"] = serde_json::json!({"status": "complete", "lane": "race-hook"});
            if !store
                .update_with_revision(
                    target_id,
                    &enriched.text,
                    &enriched.summary,
                    &enriched.source,
                    &metadata,
                    enriched.vector.as_deref(),
                    enriched.revision,
                )
                .map_err(|error| error.to_string())?
            {
                return Err("race hook could not persist target enrichment metadata".to_string());
            }
        }
        CorpusRaceHook::ForeignSupersedeSourceBeforeAtomicTransition { .. } => {
            #[cfg(any(test, feature = "bootstrap-test-api"))]
            {
                let source_path = source_scan
                    .spec
                    .addressed_path
                    .as_deref()
                    .ok_or_else(|| "race hook source has no addressed path".to_string())?;
                let store =
                    MemoryStore::open_existing_read_write(&source_path.display().to_string())
                        .map_err(|error| error.to_string())?;
                if !store
                    .supersede_memory_if_revision(
                        &source_id,
                        "foreign-wiki-target",
                        item.source_revision,
                    )
                    .map_err(|error| error.to_string())?
                {
                    return Err("race hook could not supersede source".to_string());
                }
            }
            #[cfg(not(any(test, feature = "bootstrap-test-api")))]
            {
                return Err(
                    "wiki corpus race hooks are unavailable in production builds".to_string(),
                );
            }
        }
        _ => unreachable!("matched one of the receipt-prepared hooks"),
    }
    Ok(())
}

pub(crate) fn apply_reclassification(
    scan: &StoreScan,
    item: &PlanItem,
    plan_id: &str,
    retained_backups: &[RetainedBackup],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<MigrationOutcome, String> {
    check_authority(scan, None)?;
    let mut store = open_apply_store(scan)?;
    let logical_path = scan
        .spec
        .addressed_path
        .as_deref()
        .ok_or_else(|| "reclassification store has no addressed path".to_string())?;
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    let entry = store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("source row {} disappeared during apply", item.source_id))?;
    let source_row = raw_from_entry(&entry);
    if receipt_matches(&source_row, item, plan_id, &["reclassified"]) {
        maybe_swap_reclassification_after_receipt_read(item, logical_path, race_hook)?;
        verify_opened_apply_store(&store, logical_path, "reclassification")?;
        return Ok(MigrationOutcome {
            source_store_ref: item.source_store_ref.clone(),
            source_id: item.source_id.clone(),
            target_store_ref: item.target_store_ref.clone(),
            target_id: None,
            action: item.action.clone(),
            outcome: "existing_no_op".to_string(),
            phases: vec!["reclassified".to_string()],
        });
    }
    if entry.revision != item.source_revision {
        return Err(format!(
            "source revision changed during apply for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    let metadata =
        metadata_with_receipt(&source_row, receipt_value(item, plan_id, "reclassified"))?;
    let frozen_row = scan
        .raw_row(&item.source_id)
        .ok_or_else(|| format!("planned source row {} is missing", item.source_id))?;
    let expected = ExpectedMemoryState::from_entry(
        &memory_entry_from_raw(frozen_row),
        frozen_row.superseded_by.as_deref(),
    );
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    verify_backups_before_source_mutation(retained_backups, race_hook)?;
    let changed = store
        .update_with_revision_if_expected_state(
            &entry.id,
            &entry.text,
            &entry.summary,
            &entry.source,
            &metadata,
            entry.vector.as_deref(),
            &expected,
        )
        .map_err(|error| error.to_string())?;
    verify_retained_backups(retained_backups)?;
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    if !changed {
        return Err(format!(
            "source fingerprint changed during reclassification for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    let final_entry = store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "reclassification row disappeared after receipt write".to_string())?;
    let final_row = raw_from_entry(&final_entry);
    if final_entry.revision != item.source_revision + 1
        || !receipt_matches(&final_row, item, plan_id, &["reclassified"])
    {
        return Err(format!(
            "reclassification receipt was not durably recorded for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    maybe_swap_reclassification_after_receipt_read(item, logical_path, race_hook)?;
    verify_opened_apply_store(&store, logical_path, "reclassification")?;
    Ok(MigrationOutcome {
        source_store_ref: item.source_store_ref.clone(),
        source_id: item.source_id.clone(),
        target_store_ref: item.target_store_ref.clone(),
        target_id: None,
        action: item.action.clone(),
        outcome: "reclassified".to_string(),
        phases: vec!["reclassified".to_string()],
    })
}

pub(crate) fn canonical_target_matches_plan(row: &RawRow, item: &PlanItem, plan_id: &str) -> bool {
    receipt_matches(row, item, plan_id, &["target_copied"])
        && row.copy_identity_sha256() == item.source_copy_identity_sha256
        && !row.archived
        && row.superseded_by.is_none()
}

/// Describe why [`canonical_target_matches_plan`] rejected a deterministic
/// target: the observed and expected `receipt phase / lifecycle / copy
/// identity` triple. Pure formatting — the decision stays in the predicate.
pub(crate) fn target_mismatch_diagnosis(row: &RawRow, item: &PlanItem, plan_id: &str) -> String {
    let receipt = parse_migration_receipt(row);
    let observed_phase = match &receipt {
        Ok(Some(receipt)) if receipt.plan_id == plan_id && receipt.item == *item => {
            receipt.phase.as_str().to_string()
        }
        Ok(Some(receipt)) if receipt.plan_id == plan_id => {
            format!("{} bound to another plan item", receipt.phase.as_str())
        }
        Ok(Some(receipt)) => format!("{} of plan {}", receipt.phase.as_str(), receipt.plan_id),
        Ok(None) => "<absent>".to_string(),
        Err(error) => format!("<unreadable: {error}>"),
    };
    let sibling_damage_shape = matches!(
        &receipt,
        Ok(Some(receipt))
            if receipt.phase == MigrationPhase::TargetNoncanonical
                && receipt.plan_id == plan_id
                && receipt.item == *item
    ) && row.archived
        && row.copy_identity_sha256() == item.source_copy_identity_sha256;
    let hint = if sibling_damage_shape {
        "; this is the sibling-race damage shape — `tachi wiki corpus --repair-sibling-damage` reports whether it is repairable"
    } else {
        ""
    };
    format!(
        "observed receipt_phase={observed_phase} archived={} superseded_by={} copy_identity={}; \
         expected receipt_phase=target_copied archived=false superseded_by=<none> copy_identity={}{hint}",
        row.archived,
        row.superseded_by.as_deref().unwrap_or("<none>"),
        row.copy_identity_sha256(),
        item.source_copy_identity_sha256,
    )
}

/// Snapshot-independent proof that this copy item is already complete.
///
/// `apply_copy_and_supersede` reads the source row, its migration receipt and
/// its supersession edge once, before it inspects the deterministic target. A
/// sibling worker applying the same deterministic plan can complete the whole
/// item inside that window, which leaves the snapshot claiming "no source
/// receipt yet" while the store already holds the finished migration. Every
/// completion decision taken after that read therefore re-reads the source row,
/// its receipt and the target row here, and applies exactly the predicate
/// [`plan_item_completed`] uses for `copy_to_shared_and_supersede` items.
#[cfg(any(test, feature = "bootstrap-test-api"))]
pub(crate) fn copy_item_completed_from_fresh_state(
    source_store: &MemoryStore,
    target_store: &MemoryStore,
    item: &PlanItem,
    plan_id: &str,
    target_id: &str,
) -> Result<bool, String> {
    let Some(source_entry) = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let mut source_row = raw_from_entry(&source_entry);
    source_row.superseded_by = source_store
        .supersession_target(&item.source_id)
        .map_err(|error| error.to_string())?
        .flatten();
    if !receipt_matches(&source_row, item, plan_id, &["source_superseded"])
        || !source_row.is_superseded_by(target_id)
    {
        return Ok(false);
    }
    let Some(target_entry) = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let mut target_row = raw_from_entry(&target_entry);
    target_row.superseded_by = target_store
        .supersession_target(target_id)
        .map_err(|error| error.to_string())?
        .flatten();
    Ok(canonical_target_matches_plan(&target_row, item, plan_id))
}

#[cfg(any(test, feature = "bootstrap-test-api"))]
pub(crate) fn copy_completed_no_op_outcome(
    item: &PlanItem,
    target_id: &str,
    phases: Vec<String>,
) -> MigrationOutcome {
    MigrationOutcome {
        source_store_ref: item.source_store_ref.clone(),
        source_id: item.source_id.clone(),
        target_store_ref: item.target_store_ref.clone(),
        target_id: Some(target_id.to_string()),
        action: item.action.clone(),
        outcome: "existing_no_op".to_string(),
        phases,
    }
}

#[cfg(any(test, not(feature = "bootstrap-test-api")))]
pub(crate) fn copy_only_outcome_from_fresh_state(
    source_store: &MemoryStore,
    target_store: &MemoryStore,
    item: &PlanItem,
    plan_id: &str,
    target_id: &str,
) -> Result<MigrationOutcome, String> {
    let source_entry = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "source row {} disappeared after target copy",
                item.source_id
            )
        })?;
    let mut source_row = raw_from_entry(&source_entry);
    source_row.superseded_by = source_store
        .supersession_target(&item.source_id)
        .map_err(|error| error.to_string())?
        .flatten();
    if source_row.archived
        || source_row.superseded_by.is_some()
        || parse_migration_receipt(&source_row)?.is_some()
    {
        return Err(format!(
            "copy-only completion requires an active, unsuperseded, unreceipted source for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }

    let target_entry = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("deterministic target {target_id} disappeared after copy"))?;
    let mut target_row = raw_from_entry(&target_entry);
    target_row.superseded_by = target_store
        .supersession_target(target_id)
        .map_err(|error| error.to_string())?
        .flatten();
    if !canonical_target_matches_plan(&target_row, item, plan_id) {
        return Err(format!(
            "copy-only completion found a noncanonical target {target_id}"
        ));
    }

    Ok(MigrationOutcome {
        source_store_ref: item.source_store_ref.clone(),
        source_id: item.source_id.clone(),
        target_store_ref: item.target_store_ref.clone(),
        target_id: Some(target_id.to_string()),
        action: item.action.clone(),
        outcome: "copied_without_supersession".to_string(),
        phases: vec![MigrationPhase::TargetCopied.as_str().to_string()],
    })
}

#[cfg(any(test, feature = "bootstrap-test-api"))]
pub(crate) fn reconcile_target_noncanonical(
    target_store: &mut MemoryStore,
    target_id: &str,
    item: &PlanItem,
    plan_id: &str,
) -> Result<(), String> {
    for _ in 0..3 {
        let entry = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("deterministic target {target_id} disappeared"))?;
        let mut row = raw_from_entry(&entry);
        row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        if receipt_matches(&row, item, plan_id, &["target_noncanonical"])
            && row.archived
            && row.copy_identity_sha256() == item.source_copy_identity_sha256
        {
            return Ok(());
        }
        if !canonical_target_matches_plan(&row, item, plan_id) {
            return Err(format!(
                "deterministic target {target_id} cannot be reconciled: immutable identity or migration provenance changed"
            ));
        }
        let metadata =
            metadata_with_receipt(&row, receipt_value(item, plan_id, "target_noncanonical"))?;
        let expected = ExpectedMemoryState::from_entry(&entry, row.superseded_by.as_deref());
        if target_store
            .archive_with_metadata_if_expected_state(target_id, &metadata, &expected)
            .map_err(|error| error.to_string())?
        {
            continue;
        }
    }
    Err(format!(
        "deterministic target {target_id} kept changing during noncanonical reconciliation"
    ))
}

pub(crate) fn apply_copy_and_supersede(
    source_scan: &StoreScan,
    target_scan: &StoreScan,
    item: &PlanItem,
    plan_id: &str,
    interruption: &mut Option<MigrationInterruption>,
    retained_backups: &[RetainedBackup],
    race_hook: &mut Option<CorpusRaceHook>,
) -> Result<MigrationOutcome, String> {
    let target_path = target_scan
        .spec
        .addressed_path
        .as_ref()
        .ok_or_else(|| "logical shared target has no addressed path".to_string())?;
    let source_path = source_scan
        .spec
        .addressed_path
        .as_ref()
        .ok_or_else(|| "source path missing".to_string())?;
    check_authority(source_scan, Some(target_path))?;
    check_authority(target_scan, Some(source_path))?;
    let target_id = item
        .target_id
        .as_deref()
        .ok_or_else(|| "copy plan item has no deterministic target id".to_string())?;
    if item.source_vector.row_present && !target_scan.vector_table_present {
        return Err(format!(
            "target {} cannot preserve the source vector for {}:{}",
            item.target_store_ref, item.source_store_ref, item.source_id
        ));
    }
    #[cfg(any(test, feature = "bootstrap-test-api"))]
    let mut source_store = open_apply_store(source_scan)?;
    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    let source_store = open_apply_store(source_scan)?;
    let mut target_store = open_apply_store(target_scan)?;
    maybe_swap_opened_store_path(race_hook, source_scan.spec.logical_store, source_path)?;
    maybe_swap_opened_store_path(race_hook, target_scan.spec.logical_store, target_path)?;
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    let source_entry = source_store
        .get_with_options(&item.source_id, true)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("source row {} disappeared during apply", item.source_id))?;
    let source_row = raw_from_entry(&source_entry);
    let source_receipt = parse_migration_receipt(&source_row)?;
    if let Some(receipt) = source_receipt.as_ref() {
        if receipt.plan_id != plan_id || receipt.item != *item {
            return Err(format!(
                "source migration receipt does not match supplied plan for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
        if !matches!(
            receipt.phase,
            MigrationPhase::SourceReceipted | MigrationPhase::SourceSuperseded
        ) {
            return Err(format!(
                "invalid source migration phase for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
    }
    let source_receipted = source_receipt.is_some();
    let source_superseded_receipted = source_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.phase == MigrationPhase::SourceSuperseded);
    let initial_supersession = source_store
        .supersession_target(&source_entry.id)
        .map_err(|error| error.to_string())?
        .flatten();
    if initial_supersession
        .as_deref()
        .is_some_and(|id| id != target_id)
    {
        return Err(format!(
            "source row {}:{} is already superseded by another target (observed {}, expected {})",
            item.source_store_ref,
            item.source_id,
            initial_supersession.as_deref().unwrap_or("<none>"),
            target_id
        ));
    }
    if source_superseded_receipted && initial_supersession.is_none() {
        return Err(format!(
            "source {}:{} claims source_superseded without durable supersession",
            item.source_store_ref, item.source_id
        ));
    }
    let source_revision_matches = if source_superseded_receipted {
        (item.source_revision + 1..=item.source_revision + 3).contains(&source_entry.revision)
    } else {
        let expected = item.source_revision
            + if initial_supersession.is_some() {
                2
            } else if source_receipted {
                1
            } else {
                0
            };
        source_entry.revision == expected
    };
    let expected_supersession_is_proven =
        initial_supersession.as_deref() == Some(target_id) && source_receipt.is_some();
    if !source_revision_matches
        || !source_content_matches_plan(&source_row, item, expected_supersession_is_proven)
        || source_row.vector_fingerprint(source_scan.vector_table_present) != item.source_vector
    {
        return Err(format!(
            "source revision/content changed during apply for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    maybe_mutate_copy_source_after_precheck(source_scan, item, race_hook)?;
    maybe_complete_item_as_sibling_worker(
        source_scan,
        target_scan,
        item,
        plan_id,
        target_id,
        SiblingCompletionSeam::AfterSourcePrecheck,
        race_hook,
    )?;
    let mut phases = Vec::new();

    if let Some(target_entry) = target_store
        .get_with_options(target_id, true)
        .map_err(|error| error.to_string())?
    {
        let mut target_row = raw_from_entry(&target_entry);
        target_row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        if !canonical_target_matches_plan(&target_row, item, plan_id) {
            return Err(format!(
                "deterministic target occupant collision: {target_id} immutable identity, lifecycle, or receipt mismatch ({})",
                target_mismatch_diagnosis(&target_row, item, plan_id)
            ));
        }
        phases.push("target_copied".to_string());
    } else {
        if source_receipted {
            return Err(format!(
                "source receipt exists without deterministic target {} for {}:{}",
                target_id, item.source_store_ref, item.source_id
            ));
        }
        let mut target_entry = source_entry.clone();
        target_entry.id = target_id.to_string();
        target_entry.metadata =
            metadata_with_receipt(&source_row, receipt_value(item, plan_id, "target_copied"))?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        match target_store
            .insert_if_absent(&target_entry)
            .map_err(|error| error.to_string())?
        {
            InsertMemoryResult::Inserted | InsertMemoryResult::Existing => {}
        }
        verify_retained_backups(retained_backups)?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        let verified_target = target_store
            .get_with_options(target_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "target insert did not produce a readable row".to_string())?;
        let mut verified_target_row = raw_from_entry(&verified_target);
        verified_target_row.superseded_by = target_store
            .supersession_target(target_id)
            .map_err(|error| error.to_string())?
            .flatten();
        if !canonical_target_matches_plan(&verified_target_row, item, plan_id) {
            return Err(format!(
                "target insert produced a mismatched immutable occupant or receipt ({})",
                target_mismatch_diagnosis(&verified_target_row, item, plan_id)
            ));
        }
        phases.push("target_copied".to_string());
    }
    verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
    maybe_interrupt(interruption, item, MigrationBoundary::TargetCopied)?;

    #[cfg(not(any(test, feature = "bootstrap-test-api")))]
    {
        verify_retained_backups(retained_backups)?;
        copy_only_outcome_from_fresh_state(&source_store, &target_store, item, plan_id, target_id)
    }

    #[cfg(any(test, feature = "bootstrap-test-api"))]
    {
        // Sibling-completion defence: `source_receipted` and
        // `source_superseded_receipted` above were derived from the source snapshot
        // taken before the target was inspected, so they report "not started" for
        // work a sibling worker on the same deterministic plan has already
        // finished. Decide completion from a fresh read of the source row, its
        // receipt and the target row instead.
        if copy_item_completed_from_fresh_state(
            &source_store,
            &target_store,
            item,
            plan_id,
            target_id,
        )? {
            verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
            verify_retained_backups(retained_backups)?;
            phases.push("source_superseded".to_string());
            return Ok(copy_completed_no_op_outcome(item, target_id, phases));
        }
        maybe_inject_copy_after_receipt_prepared(
            source_scan,
            target_scan,
            item,
            target_id,
            race_hook,
        )?;
        maybe_complete_item_as_sibling_worker(
            source_scan,
            target_scan,
            item,
            plan_id,
            target_id,
            SiblingCompletionSeam::AfterReceiptPrepared,
            race_hook,
        )?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_backups_before_source_mutation(retained_backups, race_hook)?;

        let transitioned = {
            let final_metadata = metadata_with_receipt(
                &source_row,
                receipt_value(item, plan_id, "source_superseded"),
            )?;
            let expected_transition =
                ExpectedMemoryState::from_entry(&source_entry, initial_supersession.as_deref());

            let observed_before_transition = source_store
                .supersession_target(&item.source_id)
                .map_err(|error| error.to_string())?
                .flatten();
            if observed_before_transition.as_deref() == Some(target_id) {
                if !source_receipted {
                    false
                } else {
                    source_store
                        .update_with_revision_if_expected_state(
                            &source_entry.id,
                            &source_entry.text,
                            &source_entry.summary,
                            &source_entry.source,
                            &final_metadata,
                            source_entry.vector.as_deref(),
                            &expected_transition,
                        )
                        .map_err(|error| error.to_string())?
                }
            } else if observed_before_transition.is_none() {
                source_store
                    .supersede_with_metadata_if_expected_state(
                        &source_entry.id,
                        target_id,
                        &final_metadata,
                        &expected_transition,
                    )
                    .map_err(|error| error.to_string())?
            } else {
                false
            }
        };

        if !transitioned {
            // Invariant, sibling-completion defence: the deterministic target may
            // only be reconciled to noncanonical once a *fresh* read of the source
            // proves it does not already carry a completed, receipted supersede
            // onto this exact target. The transition above loses that CAS both when
            // this worker's own snapshot went stale and when a sibling worker
            // applying the same deterministic plan finished the item first;
            // archiving the winner's canonical target here would remove the page
            // from every user-facing read surface (`archived = 0 AND superseded_by
            // IS NULL`) and dead-end every later apply on the occupant collision
            // check above.
            if copy_item_completed_from_fresh_state(
                &source_store,
                &target_store,
                item,
                plan_id,
                target_id,
            )? {
                verify_retained_backups(retained_backups)?;
                verify_copy_store_identities(
                    &source_store,
                    source_path,
                    &target_store,
                    target_path,
                )?;
                phases.push("source_superseded".to_string());
                return Ok(copy_completed_no_op_outcome(item, target_id, phases));
            }
            reconcile_target_noncanonical(&mut target_store, target_id, item, plan_id)?;
            verify_retained_backups(retained_backups)?;
            verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
            let observed = source_store
                .supersession_target(&item.source_id)
                .map_err(|error| error.to_string())?
                .flatten();
            if observed.as_deref().is_some_and(|id| id != target_id) {
                return Err(format!(
                "source {}:{} was superseded by foreign target {} before the atomic migration transition",
                item.source_store_ref,
                item.source_id,
                observed.as_deref().unwrap_or("<none>")
            ));
            }
            return Err(format!(
            "source fingerprint changed before the atomic receipt and supersession transition for {}:{}",
            item.source_store_ref, item.source_id
        ));
        }

        verify_retained_backups(retained_backups)?;
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        maybe_interrupt(interruption, item, MigrationBoundary::SourceSuperseded)?;

        let final_source = source_store
            .get_with_options(&item.source_id, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "source disappeared after final receipt".to_string())?;
        let final_row = raw_from_entry(&final_source);
        if !receipt_matches(&final_row, item, plan_id, &["source_superseded"])
            || !(item.source_revision + 1..=item.source_revision + 3)
                .contains(&final_source.revision)
            || source_store
                .supersession_target(&item.source_id)
                .map_err(|error| error.to_string())?
                .flatten()
                .as_deref()
                != Some(target_id)
        {
            return Err(format!(
                "source superseded state was not durably reconciled for {}:{}",
                item.source_store_ref, item.source_id
            ));
        }
        verify_copy_store_identities(&source_store, source_path, &target_store, target_path)?;
        verify_retained_backups(retained_backups)?;
        phases.push("source_superseded".to_string());
        Ok(MigrationOutcome {
            source_store_ref: item.source_store_ref.clone(),
            source_id: item.source_id.clone(),
            target_store_ref: item.target_store_ref.clone(),
            target_id: Some(target_id.to_string()),
            action: item.action.clone(),
            outcome: "copied_and_superseded".to_string(),
            phases,
        })
    }
}

pub(crate) fn raw_from_entry(entry: &MemoryEntry) -> RawRow {
    RawRow {
        id: entry.id.clone(),
        path: entry.path.clone(),
        summary: entry.summary.clone(),
        text: entry.text.clone(),
        importance: entry.importance,
        timestamp: entry.timestamp.clone(),
        valid_from: entry.valid_from.clone(),
        valid_until: entry.valid_until.clone(),
        category: entry.category.clone(),
        topic: entry.topic.clone(),
        keywords: entry.keywords.clone(),
        entities: entry.entities.clone(),
        source: entry.source.clone(),
        scope: entry.scope.clone(),
        archived: entry.archived,
        revision: entry.revision,
        access_count: entry.access_count,
        scored_count: entry.scored_count,
        last_access: entry.last_access.clone(),
        last_use_at: entry.last_use_at.clone(),
        retention_policy: entry.retention_policy.clone(),
        domain: entry.domain.clone(),
        metadata: entry.metadata.clone(),
        vector: entry.vector.clone(),
        recall_count: entry.recall_count,
        query_diversity: entry.query_diversity,
        tier: entry.tier.clone(),
        superseded_by: None,
        metadata_parse_error: None,
        classification: None,
        reasons: Vec::new(),
        effective: None,
    }
}

#[allow(dead_code)]
pub(crate) fn apply_plan(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    apply_plan_internal(scans, plan, backup_dir, None, None)
}

pub(crate) fn apply_plan_internal(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
    mut interruption: Option<MigrationInterruption>,
    mut race_hook: Option<CorpusRaceHook>,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    regular_directory_metadata(backup_dir)?;
    let backup_dir = std::fs::canonicalize(backup_dir)
        .map_err(|error| format!("cannot canonicalize backup directory: {error}"))?;
    validate_apply_inventory(scans)?;
    let current_scans = reinventory_apply_scans(scans);
    validate_apply_inventory(&current_scans)?;
    validate_plan(&current_scans, plan)?;
    let scans = current_scans.as_slice();
    maybe_mutate_reclassification_after_plan_validation(scans, &mut race_hook)?;
    maybe_mutate_valid_until_after_plan_validation(scans, &mut race_hook)?;
    if !plan.items.is_empty()
        && plan
            .items
            .iter()
            .all(|item| plan_item_completed(scans, plan, item))
    {
        let final_path = backup_dir.join("wiki-corpus-v1-apply-receipt.json");
        let initial_path = backup_dir.join("wiki-corpus-v1-manifest.json");
        let (manifest, needs_final_receipt) = if let Some(final_manifest) =
            read_manifest_if_present(&final_path)?
        {
            (final_manifest, false)
        } else if let Some(initial) = read_manifest_if_present(&initial_path)? {
            if initial.plan_id != plan.plan_id || initial.status != "backups_verified" {
                return Err(
                    "existing backup manifest belongs to a different completed plan".to_string(),
                );
            }
            (initial, true)
        } else {
            return Err("completed Wiki corpus state has no backup manifest evidence".to_string());
        };
        if manifest.plan_id != plan.plan_id
            || (!needs_final_receipt && manifest.status != "apply_completed")
        {
            return Err("existing apply receipt does not match the completed plan".to_string());
        }
        let retained_backups = retain_plan_backups(scans, plan, &backup_dir, &mut race_hook)?;
        let verified_receipts = retained_backups
            .iter()
            .map(|backup| backup.receipt.clone())
            .collect::<Vec<_>>();
        if manifest.receipts != verified_receipts {
            return Err(
                "completed Wiki corpus manifest does not match retained backup evidence"
                    .to_string(),
            );
        }
        let completed = if needs_final_receipt {
            let completed = BackupManifest {
                version: manifest.version.clone(),
                status: "apply_completed".to_string(),
                plan_id: manifest.plan_id.clone(),
                backup_directory: manifest.backup_directory.clone(),
                receipts: verified_receipts,
                migration_receipts: plan
                    .items
                    .iter()
                    .map(|item| {
                        let phase = if item.action == "reclassify_in_place" {
                            "reclassified"
                        } else {
                            "source_superseded"
                        };
                        receipt_value(item, &plan.plan_id, phase)
                    })
                    .collect(),
            };
            write_manifest_if_needed_with_hook(&final_path, &completed, &mut race_hook)?;
            completed
        } else {
            manifest
        };
        verify_retained_backups(&retained_backups)?;
        maybe_swap_logical_path_before_completed_return(scans, &mut race_hook)?;
        maybe_archive_target_before_completed_return(scans, plan, &mut race_hook)?;
        validate_apply_inventory(scans)?;
        // Keep every writer reservation alive through the final proof and the
        // return expression; dropping the guard is the last operation here.
        let _completed_no_op_locks = CompletedNoOpLocks::acquire(scans)?;
        let final_scans = reinventory_apply_scans(scans);
        validate_apply_inventory(&final_scans)?;
        if !plan
            .items
            .iter()
            .all(|item| plan_item_completed(&final_scans, plan, item))
        {
            return Err(
                "completed Wiki corpus state changed before existing_no_op return; completion proof is no longer valid"
                    .to_string(),
            );
        }
        validate_plan(&final_scans, plan)?;
        verify_retained_backups(&retained_backups)?;
        return Ok((completed, plan.items.iter().map(existing_outcome).collect()));
    }
    if plan.items.is_empty() {
        let manifest = BackupManifest {
            version: REPORT_VERSION.to_string(),
            status: "no_op_no_mutation".to_string(),
            plan_id: plan.plan_id.clone(),
            backup_directory: backup_dir.display().to_string(),
            receipts: Vec::new(),
            migration_receipts: Vec::new(),
        };
        let manifest_path = backup_dir.join("wiki-corpus-v1-manifest.json");
        write_manifest_if_needed_with_hook(&manifest_path, &manifest, &mut race_hook)?;
        maybe_swap_logical_path_before_completed_return(scans, &mut race_hook)?;
        validate_apply_inventory(scans)?;
        return Ok((manifest, Vec::new()));
    }

    let retained_backups = retain_plan_backups(scans, plan, &backup_dir, &mut race_hook)?;
    let receipts = retained_backups
        .iter()
        .map(|backup| backup.receipt.clone())
        .collect::<Vec<_>>();
    let manifest_path = backup_dir.join("wiki-corpus-v1-manifest.json");
    let initial_manifest = BackupManifest {
        version: REPORT_VERSION.to_string(),
        status: "backups_verified".to_string(),
        plan_id: plan.plan_id.clone(),
        backup_directory: backup_dir.display().to_string(),
        receipts: receipts.clone(),
        migration_receipts: plan
            .items
            .iter()
            .map(|item| receipt_value(item, &plan.plan_id, "planned"))
            .collect(),
    };
    write_manifest_if_needed_with_hook(&manifest_path, &initial_manifest, &mut race_hook)?;

    let mut outcomes = Vec::new();
    for item in &plan.items {
        verify_retained_backups(&retained_backups)?;
        let outcome = if item.action == "reclassify_in_place" {
            let source = find_scan(scans, &item.source_store_ref)?;
            apply_reclassification(
                source,
                item,
                &plan.plan_id,
                &retained_backups,
                &mut race_hook,
            )?
        } else {
            let source = find_scan(scans, &item.source_store_ref)?;
            let target = find_scan(scans, &item.target_store_ref)?;
            apply_copy_and_supersede(
                source,
                target,
                item,
                &plan.plan_id,
                &mut interruption,
                &retained_backups,
                &mut race_hook,
            )?
        };
        verify_retained_backups(&retained_backups)?;
        outcomes.push(outcome);
    }
    let final_manifest = BackupManifest {
        version: initial_manifest.version.clone(),
        status: "apply_completed".to_string(),
        plan_id: initial_manifest.plan_id.clone(),
        backup_directory: initial_manifest.backup_directory.clone(),
        receipts: initial_manifest.receipts.clone(),
        migration_receipts: outcomes
            .iter()
            .filter_map(|outcome| {
                plan.items
                    .iter()
                    .find(|item| {
                        item.source_store_ref == outcome.source_store_ref
                            && item.source_id == outcome.source_id
                    })
                    .map(|item| {
                        let phase = outcome
                            .phases
                            .last()
                            .map(String::as_str)
                            .unwrap_or("completed");
                        receipt_value(item, &plan.plan_id, phase)
                    })
            })
            .collect(),
    };
    // The initial manifest remains the deterministic evidence of all verified
    // backups. The separate final receipt makes an interrupted run explicit.
    let final_path = backup_dir.join("wiki-corpus-v1-apply-receipt.json");
    verify_retained_backups(&retained_backups)?;
    write_manifest_if_needed_with_hook(&final_path, &final_manifest, &mut race_hook)?;
    verify_retained_backups(&retained_backups)?;
    Ok((final_manifest, outcomes))
}

#[cfg(test)]
pub(crate) fn apply_plan_with_interruption(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
    interruption: MigrationInterruption,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    apply_plan_internal(scans, plan, backup_dir, Some(interruption), None)
}

#[cfg(test)]
pub(crate) fn apply_plan_with_race_hook(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
    backup_dir: &Path,
    race_hook: CorpusRaceHook,
) -> Result<(BackupManifest, Vec<MigrationOutcome>), String> {
    apply_plan_internal(scans, plan, backup_dir, None, Some(race_hook))
}
