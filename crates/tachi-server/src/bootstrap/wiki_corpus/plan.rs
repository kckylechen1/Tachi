use super::classify::*;
use super::fs::*;
use super::types::*;

use crate::physical_db_identity::{physical_db_bindings_for_paths, PhysicalStoreMutationState};
use memcore::db::migrations::EXPECTED_SCHEMA_VERSION;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) fn plan_item_material(item: &PlanItem) -> String {
    serde_json::to_string(&canonicalize_value(
        &serde_json::to_value(item).unwrap_or(Value::Null),
    ))
    .unwrap_or_default()
}

pub(crate) fn replay_identity_for(
    source_store_ref: &str,
    source_physical_id: &str,
    row: &RawRow,
    vector_table_present: bool,
) -> String {
    let material = json!({
        "version": "wiki-corpus-replay-v2",
        "source_store_ref": source_store_ref,
        "source_physical_id": source_physical_id,
        "source_id": row.id,
        "source_path": row.path,
        "normalized_path": row.normalized_path(),
        "source_revision": row.revision,
        "source_content_sha256": row.content_sha256(),
        "source_copy_identity_sha256": row.copy_identity_sha256(),
        "source_superseded_by": row.superseded_by,
        "source_vector": row.vector_fingerprint(vector_table_present),
    });
    digest_string(&serde_json::to_string(&canonicalize_value(&material)).unwrap_or_default())
}

pub(crate) fn build_plan_item(source: &StoreScan, row: &RawRow, target: &StoreScan) -> PlanItem {
    let source_store_ref = source.spec.logical_store.reference().to_string();
    let source_physical_id = source
        .physical
        .as_ref()
        .map(|physical| physical.physical_id.clone())
        .unwrap_or_default();
    let replay_identity = replay_identity_for(
        &source_store_ref,
        &source_physical_id,
        row,
        source.vector_table_present,
    );
    let is_shared_source = source.spec.logical_store == LogicalStore::SharedWiki;
    PlanItem {
        action: if is_shared_source {
            "reclassify_in_place".to_string()
        } else {
            "copy_to_shared_and_supersede".to_string()
        },
        source_store_ref,
        source_physical_id,
        source_id: row.id.clone(),
        source_path: row.path.clone(),
        normalized_path: row.normalized_path(),
        source_revision: row.revision,
        source_content_sha256: row.content_sha256(),
        source_copy_identity_sha256: row.copy_identity_sha256(),
        source_valid_until: row.valid_until.clone(),
        source_superseded_by: row.superseded_by.clone(),
        source_vector: row.vector_fingerprint(source.vector_table_present),
        replay_identity: replay_identity.clone(),
        target_store_ref: LogicalStore::SharedWiki.reference().to_string(),
        target_physical_id: target
            .physical
            .as_ref()
            .map(|physical| physical.physical_id.clone())
            .unwrap_or_default(),
        target_id: (!is_shared_source).then(|| format!("wiki-corpus:{replay_identity}")),
    }
}

pub(crate) fn plan_id_for(
    version: &str,
    store_fingerprints: &[PlanStoreFingerprint],
    items: &[PlanItem],
) -> String {
    let mut fingerprints = store_fingerprints.to_vec();
    fingerprints.sort_by(|left, right| left.logical_store_ref.cmp(&right.logical_store_ref));
    let mut items = items.to_vec();
    items.sort_by(|left, right| {
        left.source_store_ref
            .cmp(&right.source_store_ref)
            .then_with(|| left.source_id.cmp(&right.source_id))
            .then_with(|| left.replay_identity.cmp(&right.replay_identity))
    });
    let material = json!({
        "version": version,
        "store_fingerprints": fingerprints,
        "items": items,
    });
    digest_string(&serde_json::to_string(&canonicalize_value(&material)).unwrap_or_default())
}

pub(crate) fn build_plan(scans: &[StoreScan]) -> Result<WikiCorpusPlan, String> {
    let target = find_scan(scans, LogicalStore::SharedWiki.reference())?;
    let mut items = Vec::new();
    for scan in scans {
        if scan.physical.is_none() {
            continue;
        }
        for row in scan
            .rows
            .iter()
            .filter(|row| row.classification == Some(CorpusClassification::SharedCandidate))
        {
            items.push(build_plan_item(scan, row, target));
        }
    }
    items.sort_by(|left, right| {
        left.source_store_ref
            .cmp(&right.source_store_ref)
            .then_with(|| left.source_id.cmp(&right.source_id))
            .then_with(|| left.replay_identity.cmp(&right.replay_identity))
    });
    let store_fingerprints = scans
        .iter()
        .filter_map(StoreScan::fingerprint)
        .collect::<Vec<_>>();
    let plan_id = plan_id_for(REPORT_VERSION, &store_fingerprints, &items);
    Ok(WikiCorpusPlan {
        version: REPORT_VERSION.to_string(),
        plan_id,
        store_fingerprints,
        items,
    })
}

pub(crate) fn receipt_value(item: &PlanItem, plan_id: &str, phase: &str) -> Value {
    let phase = match phase {
        "planned" => MigrationPhase::Planned,
        "target_copied" => MigrationPhase::TargetCopied,
        "target_noncanonical" => MigrationPhase::TargetNoncanonical,
        "source_receipted" => MigrationPhase::SourceReceipted,
        "source_superseded" => MigrationPhase::SourceSuperseded,
        "reclassified" => MigrationPhase::Reclassified,
        other => panic!("unsupported Wiki corpus migration phase {other}"),
    };
    serde_json::to_value(MigrationReceipt {
        kind: RECEIPT_KEY.to_string(),
        version: RECEIPT_VERSION,
        plan_id: plan_id.to_string(),
        phase,
        item: item.clone(),
    })
    .expect("Wiki corpus migration receipt is serializable")
}

pub(crate) fn parse_migration_receipt(row: &RawRow) -> Result<Option<MigrationReceipt>, String> {
    let Some(receipt) = row.metadata.get(RECEIPT_KEY) else {
        return Ok(None);
    };
    let receipt: MigrationReceipt = serde_json::from_value(receipt.clone()).map_err(|error| {
        format!(
            "invalid Wiki corpus migration receipt on {}: {error}",
            row.id
        )
    })?;
    if receipt.kind != RECEIPT_KEY || receipt.version != RECEIPT_VERSION {
        return Err(format!(
            "unsupported Wiki corpus migration receipt on {}",
            row.id
        ));
    }
    Ok(Some(receipt))
}

pub(crate) fn receipt_matches(
    row: &RawRow,
    item: &PlanItem,
    plan_id: &str,
    phases: &[&str],
) -> bool {
    let Ok(Some(receipt)) = parse_migration_receipt(row) else {
        return false;
    };
    receipt.plan_id == plan_id && receipt.item == *item && phases.contains(&receipt.phase.as_str())
}

pub(crate) fn metadata_with_receipt(row: &RawRow, receipt: Value) -> Result<Value, String> {
    let mut metadata = row
        .metadata
        .as_object()
        .cloned()
        .ok_or_else(|| "migration requires object-shaped metadata".to_string())?;
    metadata.insert(RECEIPT_KEY.to_string(), receipt);
    Ok(Value::Object(metadata))
}

#[cfg(test)]
pub(crate) fn validate_target_occupant(
    item: &PlanItem,
    plan_id: &str,
    _source: &RawRow,
    target: &StoreScan,
) -> Result<(), String> {
    let Some(target_id) = item.target_id.as_deref() else {
        return Ok(());
    };
    let Some(occupant) = target.raw_row(target_id) else {
        return Ok(());
    };
    if occupant.copy_identity_sha256() != item.source_copy_identity_sha256
        || occupant.archived
        || occupant.superseded_by.is_some()
        || !receipt_matches(occupant, item, plan_id, &["target_copied"])
    {
        return Err(format!(
            "deterministic target occupant collision: {target_id} already exists in {} but its canonical content or migration receipt does not match",
            target.spec.logical_store.reference()
        ));
    }
    Ok(())
}

pub(crate) fn plan_row_fingerprint_from_item(item: &PlanItem) -> PlanRowFingerprint {
    PlanRowFingerprint {
        id: item.source_id.clone(),
        revision: item.source_revision,
        content_sha256: item.source_content_sha256.clone(),
        superseded_by: item.source_superseded_by.clone(),
        vector: item.source_vector.clone(),
    }
}

pub(crate) fn expected_item_for_live_source(
    source: &StoreScan,
    row: &RawRow,
    target: &StoreScan,
    source_revision: i64,
    source_valid_until: Option<String>,
    source_superseded_by: Option<String>,
) -> PlanItem {
    let mut identity_row = row.clone();
    identity_row.revision = source_revision;
    identity_row.valid_until = source_valid_until;
    identity_row.superseded_by = source_superseded_by;
    let mut expected = build_plan_item(source, &identity_row, target);
    expected.replay_identity = replay_identity_for(
        &expected.source_store_ref,
        &expected.source_physical_id,
        &identity_row,
        source.vector_table_present,
    );
    if expected.action == "copy_to_shared_and_supersede" {
        expected.target_id = Some(format!("wiki-corpus:{}", expected.replay_identity));
    }
    expected
}

pub(crate) fn superseded_validity_matches_plan(row: &RawRow, item: &PlanItem) -> bool {
    match item.source_valid_until.as_ref() {
        Some(expected) => row.valid_until.as_ref() == Some(expected),
        None => row.valid_until.is_some(),
    }
}

pub(crate) fn source_content_matches_plan(
    row: &RawRow,
    item: &PlanItem,
    supersession_is_proven: bool,
) -> bool {
    if !supersession_is_proven {
        return row.content_sha256() == item.source_content_sha256;
    }
    if !superseded_validity_matches_plan(row, item) {
        return false;
    }
    let mut original = row.clone();
    original.valid_until.clone_from(&item.source_valid_until);
    original.content_sha256() == item.source_content_sha256
}

pub(crate) fn validate_live_source_item(
    item: &PlanItem,
    plan_id: &str,
    source: &StoreScan,
    row: &RawRow,
    target: &StoreScan,
    receipt: Option<&MigrationReceipt>,
) -> Result<(), String> {
    let source_physical = source.physical.as_ref().ok_or_else(|| {
        format!(
            "source store {} has no physical identity",
            item.source_store_ref
        )
    })?;
    let target_physical = target
        .physical
        .as_ref()
        .ok_or_else(|| "logical shared target has no physical identity".to_string())?;
    let expected = expected_item_for_live_source(
        source,
        row,
        target,
        item.source_revision,
        item.source_valid_until.clone(),
        item.source_superseded_by.clone(),
    );
    if item != &expected {
        return Err(format!(
            "Wiki corpus plan item for {}:{} does not match the canonical item rederived from the current source",
            item.source_store_ref, item.source_id
        ));
    }
    if item.source_physical_id != source_physical.physical_id
        || item.target_physical_id != target_physical.physical_id
    {
        return Err(format!(
            "Wiki corpus plan item for {}:{} has stale physical identity",
            item.source_store_ref, item.source_id
        ));
    }
    if row.vector_fingerprint(source.vector_table_present) != item.source_vector {
        return Err(format!(
            "Wiki corpus source vector state changed for {}:{}",
            item.source_store_ref, item.source_id
        ));
    }
    if item.source_vector.row_present && !target.vector_table_present {
        return Err(format!(
            "target {} cannot preserve the source vector for {}:{}",
            item.target_store_ref, item.source_store_ref, item.source_id
        ));
    }

    match receipt {
        None => {
            if row.revision != item.source_revision
                || row.valid_until != item.source_valid_until
                || row.superseded_by != item.source_superseded_by
            {
                return Err(format!(
                    "source revision/lifecycle changed after preview for {}:{}",
                    item.source_store_ref, item.source_id
                ));
            }
        }
        Some(receipt) => {
            if receipt.plan_id != plan_id || receipt.item != *item {
                return Err(format!(
                    "Wiki corpus migration receipt does not match supplied plan for {}:{}",
                    item.source_store_ref, item.source_id
                ));
            }
            match receipt.phase {
                MigrationPhase::Reclassified => {
                    if row.revision != item.source_revision + 1
                        || row.valid_until != item.source_valid_until
                        || row.superseded_by != item.source_superseded_by
                    {
                        return Err(format!(
                            "Wiki corpus migration state is inconsistent for {}:{}",
                            item.source_store_ref, item.source_id
                        ));
                    }
                }
                MigrationPhase::SourceReceipted => {
                    let receipt_only = row.revision == item.source_revision + 1
                        && row.valid_until == item.source_valid_until
                        && row.superseded_by == item.source_superseded_by;
                    let supersession_observed = row.revision == item.source_revision + 2
                        && superseded_validity_matches_plan(row, item)
                        && row.superseded_by == item.target_id;
                    if !receipt_only && !supersession_observed {
                        return Err(format!(
                            "Wiki corpus migration state is inconsistent for {}:{}",
                            item.source_store_ref, item.source_id
                        ));
                    }
                }
                MigrationPhase::SourceSuperseded => {
                    if item.action != "copy_to_shared_and_supersede"
                        || !(item.source_revision + 1..=item.source_revision + 3)
                            .contains(&row.revision)
                        || !superseded_validity_matches_plan(row, item)
                        || row.superseded_by != item.target_id
                    {
                        return Err(format!(
                            "Wiki corpus source_superseded state is not proven for {}:{}",
                            item.source_store_ref, item.source_id
                        ));
                    }
                }
                MigrationPhase::Planned
                | MigrationPhase::TargetCopied
                | MigrationPhase::TargetNoncanonical => {
                    return Err(format!(
                        "invalid source migration phase for {}:{}",
                        item.source_store_ref, item.source_id
                    ));
                }
            }
        }
    }

    if item.action == "copy_to_shared_and_supersede"
        && row.superseded_by.is_some()
        && row.superseded_by != item.target_id
    {
        return Err(format!(
            "source row {}:{} is already superseded by another target",
            item.source_store_ref, item.source_id
        ));
    }
    Ok(())
}

pub(crate) fn validate_target_receipt(
    item: &PlanItem,
    plan_id: &str,
    target: &StoreScan,
    row: &RawRow,
    source: &StoreScan,
    source_row: &RawRow,
) -> Result<(), String> {
    let receipt = parse_migration_receipt(row)?
        .ok_or_else(|| format!("target {} has no migration receipt", row.id))?;
    if receipt.plan_id != plan_id
        || receipt.phase != MigrationPhase::TargetCopied
        || receipt.item != *item
        || item.target_store_ref != target.spec.logical_store.reference()
        || item.target_id.as_deref() != Some(row.id.as_str())
    {
        return Err(format!(
            "target migration receipt does not match supplied plan for {}",
            row.id
        ));
    }
    validate_live_source_item(
        item,
        plan_id,
        source,
        source_row,
        target,
        parse_migration_receipt(source_row)?.as_ref(),
    )?;
    if row.copy_identity_sha256() != item.source_copy_identity_sha256
        || row.archived
        || row.superseded_by.is_some()
    {
        return Err(format!(
            "target {} immutable copy identity or canonical lifecycle changed",
            row.id
        ));
    }
    Ok(())
}

pub(crate) fn validate_noncanonical_target_receipt(
    row: &RawRow,
    store: LogicalStore,
) -> Result<(), String> {
    let receipt = parse_migration_receipt(row)?
        .ok_or_else(|| format!("noncanonical target {} has no receipt", row.id))?;
    let item = &receipt.item;
    if receipt.phase != MigrationPhase::TargetNoncanonical
        || item.target_store_ref != store.reference()
        || item.target_id.as_deref() != Some(row.id.as_str())
        || row.copy_identity_sha256() != item.source_copy_identity_sha256
        || !row.archived
    {
        return Err(format!(
            "noncanonical target {} has invalid provenance or lifecycle",
            row.id
        ));
    }
    Ok(())
}

pub(crate) fn rederive_items(scans: &[StoreScan], plan_id: &str) -> Result<Vec<PlanItem>, String> {
    let target = find_scan(scans, LogicalStore::SharedWiki.reference())?;
    let mut items = BTreeMap::<String, PlanItem>::new();
    for scan in scans {
        for row in &scan.rows {
            let receipt = parse_migration_receipt(row)?;
            if receipt
                .as_ref()
                .is_some_and(|receipt| receipt.phase == MigrationPhase::TargetNoncanonical)
            {
                validate_noncanonical_target_receipt(row, scan.spec.logical_store)?;
                continue;
            }
            let is_target_receipt = receipt.as_ref().is_some_and(|receipt| {
                receipt.phase == MigrationPhase::TargetCopied
                    && receipt.item.target_store_ref == scan.spec.logical_store.reference()
                    && receipt.item.target_id.as_deref() == Some(row.id.as_str())
                    && receipt.item.source_store_ref != scan.spec.logical_store.reference()
            });
            let is_source_receipt = receipt.as_ref().is_some_and(|receipt| {
                receipt.item.source_store_ref == scan.spec.logical_store.reference()
                    && receipt.item.source_id == row.id
            });
            if receipt.is_none() && row.classification.is_none() {
                continue;
            }
            if receipt.is_some()
                && !is_target_receipt
                && !is_source_receipt
                && row.classification != Some(CorpusClassification::SharedCandidate)
            {
                return Err(format!(
                    "migration receipt is attached to a non-candidate row {}",
                    row.id
                ));
            }
            if receipt.is_none()
                && row.classification != Some(CorpusClassification::SharedCandidate)
            {
                continue;
            }
            let item = if let Some(receipt) = receipt.as_ref() {
                if receipt.plan_id != plan_id {
                    return Err(format!(
                        "migration receipt on {} belongs to a different plan",
                        row.id
                    ));
                }
                let item = &receipt.item;
                if item.source_store_ref == scan.spec.logical_store.reference()
                    && item.source_id == row.id
                {
                    let source = scan;
                    validate_live_source_item(item, plan_id, source, row, target, Some(receipt))?;
                    item.clone()
                } else if receipt.phase == MigrationPhase::TargetCopied
                    && item.target_store_ref == scan.spec.logical_store.reference()
                    && item.target_id.as_deref() == Some(row.id.as_str())
                {
                    let source = find_scan(scans, &item.source_store_ref)?;
                    let source_row = source.raw_row(&item.source_id).ok_or_else(|| {
                        format!(
                            "target receipt {} references missing source {}:{}",
                            row.id, item.source_store_ref, item.source_id
                        )
                    })?;
                    validate_target_receipt(item, plan_id, scan, row, source, source_row)?;
                    item.clone()
                } else {
                    return Err(format!(
                        "migration receipt on {} is not bound to its physical row",
                        row.id
                    ));
                }
            } else {
                let item = build_plan_item(scan, row, target);
                validate_live_source_item(&item, plan_id, scan, row, target, None)?;
                item
            };
            let key = plan_item_material(&item);
            if let Some(existing) = items.insert(key, item.clone()) {
                if existing != item {
                    return Err(
                        "current Wiki corpus state rederives conflicting plan items".to_string()
                    );
                }
            }
        }
    }
    Ok(items.into_values().collect())
}

pub(crate) fn rederive_original_store_fingerprint(
    scans: &[StoreScan],
    scan: &StoreScan,
    plan_id: &str,
) -> Result<PlanStoreFingerprint, String> {
    let target = find_scan(scans, LogicalStore::SharedWiki.reference())?;
    let mut rows = Vec::new();
    for row in &scan.rows {
        if let Some(receipt) = parse_migration_receipt(row)? {
            if receipt.phase == MigrationPhase::TargetNoncanonical {
                validate_noncanonical_target_receipt(row, scan.spec.logical_store)?;
                if receipt.plan_id != plan_id {
                    rows.push(plan_row_fingerprint(row, scan.vector_table_present));
                }
                continue;
            }
            if receipt.plan_id != plan_id {
                return Err(format!(
                    "migration receipt on {} belongs to another plan",
                    row.id
                ));
            }
            let item = &receipt.item;
            if receipt.phase == MigrationPhase::TargetCopied
                && item.target_store_ref == scan.spec.logical_store.reference()
                && item.target_id.as_deref() == Some(row.id.as_str())
                && item.source_store_ref != scan.spec.logical_store.reference()
            {
                let source = find_scan(scans, &item.source_store_ref)?;
                let source_row = source.raw_row(&item.source_id).ok_or_else(|| {
                    format!("target receipt {} references missing source", row.id)
                })?;
                validate_target_receipt(item, plan_id, scan, row, source, source_row)?;
                continue;
            }
            if item.source_store_ref == scan.spec.logical_store.reference()
                && item.source_id == row.id
            {
                validate_live_source_item(item, plan_id, scan, row, target, Some(&receipt))?;
                rows.push(plan_row_fingerprint_from_item(item));
                continue;
            }
            return Err(format!(
                "migration receipt on {} is not in this store's source set",
                row.id
            ));
        }
        rows.push(plan_row_fingerprint(row, scan.vector_table_present));
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    let row_digest = row_digest_values(
        rows.iter()
            .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
            .collect(),
    );
    let physical = scan.physical.as_ref().ok_or_else(|| {
        format!(
            "store {} has no physical identity",
            scan.spec.logical_store.reference()
        )
    })?;
    let schema = scan.report.stored_schema.ok_or_else(|| {
        format!(
            "store {} has no schema",
            scan.spec.logical_store.reference()
        )
    })?;
    Ok(PlanStoreFingerprint {
        logical_store_ref: scan.spec.logical_store.reference().to_string(),
        physical_id: physical.physical_id.clone(),
        canonical_path: physical.canonical_path.clone(),
        schema,
        total_memory_rows: rows.len(),
        row_digest,
        vector_table_present: scan.vector_table_present,
        rows,
    })
}

pub(crate) fn validate_store_fingerprints(
    scans: &[StoreScan],
    plan: &WikiCorpusPlan,
) -> Result<(), String> {
    let mut expected = plan.store_fingerprints.clone();
    expected.sort_by(|left, right| left.logical_store_ref.cmp(&right.logical_store_ref));
    let mut current = Vec::new();
    for scan in scans {
        if scan.physical.is_some() {
            current.push(rederive_original_store_fingerprint(
                scans,
                scan,
                &plan.plan_id,
            )?);
        }
    }
    current.sort_by(|left, right| left.logical_store_ref.cmp(&right.logical_store_ref));
    if expected.len() != current.len() {
        return Err("Wiki corpus store fingerprint set changed since preview".to_string());
    }

    for (expected_store, current_store) in expected.iter().zip(current.iter()) {
        if expected_store.logical_store_ref != current_store.logical_store_ref
            || expected_store.physical_id != current_store.physical_id
            || expected_store.canonical_path != current_store.canonical_path
            || expected_store.schema != current_store.schema
            || expected_store.vector_table_present != current_store.vector_table_present
        {
            return Err(format!(
                "Wiki corpus store identity or schema changed since preview for {}",
                expected_store.logical_store_ref
            ));
        }

        if expected_store.total_memory_rows != expected_store.rows.len()
            || expected_store.row_digest
                != row_digest_values(
                    expected_store
                        .rows
                        .iter()
                        .map(|row| serde_json::to_value(row).unwrap_or(Value::Null))
                        .collect(),
                )
        {
            return Err(format!(
                "Wiki corpus preview fingerprint is internally inconsistent for {}",
                expected_store.logical_store_ref
            ));
        }
        if current_store != expected_store {
            return Err(format!(
                "Wiki corpus row or vector fingerprint changed since preview for {}",
                expected_store.logical_store_ref
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_plan(scans: &[StoreScan], plan: &WikiCorpusPlan) -> Result<(), String> {
    if plan.version != REPORT_VERSION {
        return Err(format!(
            "unsupported Wiki corpus plan version '{}'",
            plan.version
        ));
    }
    let expected_plan_id = plan_id_for(&plan.version, &plan.store_fingerprints, &plan.items);
    if expected_plan_id != plan.plan_id {
        return Err("Wiki corpus plan id does not match its canonical contents".to_string());
    }
    validate_store_fingerprints(scans, plan)?;
    let mut expected_items = rederive_items(scans, &plan.plan_id)?;
    let mut supplied_items = plan.items.clone();
    expected_items.sort_by_key(plan_item_material);
    supplied_items.sort_by_key(plan_item_material);
    if expected_items != supplied_items {
        return Err(
            "supplied Wiki corpus plan items do not match canonical rederivation from current stores"
                .to_string(),
        );
    }
    Ok(())
}

pub(crate) fn load_plan(path: &Path) -> Result<WikiCorpusPlan, String> {
    regular_non_symlink_metadata(path)?;
    let raw = read_file_no_follow(path)?;
    let value: Value = serde_json::from_slice(&raw)
        .map_err(|error| format!("Wiki corpus plan {} is not JSON: {error}", path.display()))?;
    if let Some(plan) = value.get("plan") {
        serde_json::from_value(plan.clone())
            .map_err(|error| format!("invalid embedded Wiki corpus plan: {error}"))
    } else {
        serde_json::from_value(value).map_err(|error| format!("invalid Wiki corpus plan: {error}"))
    }
}

pub(crate) fn validate_apply_inventory(scans: &[StoreScan]) -> Result<(), String> {
    for scan in scans {
        let report = &scan.report;
        if !report.existence {
            return Err(format!(
                "apply refuses missing involved store {} ({})",
                report.logical_store_ref,
                report
                    .read_failure
                    .as_ref()
                    .map(|failure| failure.message.as_str())
                    .unwrap_or("path is absent")
            ));
        }
        if let Some(failure) = &report.read_failure {
            return Err(format!(
                "apply refuses unreadable store {}: {}",
                report.logical_store_ref, failure.message
            ));
        }
        if report.stored_schema != Some(EXPECTED_SCHEMA_VERSION) {
            return Err(format!(
                "apply refuses schema mismatch in {}: stored {:?}, expected {}",
                report.logical_store_ref, report.stored_schema, EXPECTED_SCHEMA_VERSION
            ));
        }
        let physical = scan.physical.as_ref().ok_or_else(|| {
            format!(
                "apply refuses store {} without physical identity",
                report.logical_store_ref
            )
        })?;
        if physical.mutation_state != PhysicalStoreMutationState::Authorized
            || physical.mutation_authority.is_none()
        {
            return Err(format!(
                "apply refuses ambiguous or unavailable physical mutation identity for {} ({})",
                report.logical_store_ref,
                physical.mutation_state.as_str()
            ));
        }
    }
    let addressed_paths = scans
        .iter()
        .filter_map(|scan| scan.spec.addressed_path.clone())
        .collect::<Vec<_>>();
    let bindings = physical_db_bindings_for_paths(&addressed_paths)
        .map_err(|error| format!("apply refuses ambiguous physical mutation identity: {error}"))?;
    for (scan, binding) in scans
        .iter()
        .filter(|scan| scan.spec.addressed_path.is_some())
        .zip(bindings.iter())
    {
        let physical = scan
            .physical
            .as_ref()
            .ok_or_else(|| "apply inventory lost its physical identity".to_string())?;
        let addressed = scan
            .spec
            .addressed_path
            .as_ref()
            .expect("filtered to addressed stores");
        let expected_primary = physical.primary_path == addressed.display().to_string();
        if binding.physical_id != physical.physical_id
            || binding.is_primary_alias != expected_primary
        {
            return Err(format!(
                "apply refuses physical binding changed since inventory for {}",
                scan.spec.logical_store.reference()
            ));
        }
    }
    Ok(())
}

pub(crate) fn reinventory_apply_scans(scans: &[StoreScan]) -> Vec<StoreScan> {
    let mut current = scans
        .iter()
        .map(|scan| inventory_store(scan.spec.clone()))
        .collect::<Vec<_>>();
    finalize_classifications(&mut current);
    for scan in current.iter_mut() {
        refresh_report(scan);
    }
    current
}
