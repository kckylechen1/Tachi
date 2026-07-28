//! Portable repair kernel for contradictory `archived` / `superseded_by` state.
//!
//! The planner freezes lifecycle metadata only. It never reads memory content,
//! and apply/restore never delete or rebuild search, vector, access, or edge
//! evidence. Ambiguous authority remains an operator-visible adjudication item.

use super::super::{MemoryError, MemoryStore};
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const LIFECYCLE_CONSISTENCY_POLICY: &str = "lifecycle-consistency-v1";
pub const LIFECYCLE_CONSISTENCY_SCHEMA_VERSION: u32 = 1;
pub const LIFECYCLE_CONSISTENCY_RECEIPT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenLifecycleRow {
    pub id: String,
    pub revision: i64,
    pub archived: bool,
    pub superseded_by: Option<String>,
    pub valid_until: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleMutationKind {
    ArchiveOneWayLoser,
    ClearUniqueWinnerBacklink,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleMutation {
    pub kind: LifecycleMutationKind,
    pub row_id: String,
    pub related_row_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleAdjudicationKind {
    MissingTarget,
    SelfCycle,
    AmbiguousTwoNodeCycle,
    LongerCycle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleAdjudication {
    pub kind: LifecycleAdjudicationKind,
    pub row_ids: Vec<String>,
    pub missing_target_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleConsistencyPlan {
    pub schema_version: u32,
    pub policy_version: String,
    pub target_db_identity: String,
    pub generated_at: String,
    /// Every existing row whose lifecycle state is load-bearing for this plan.
    /// Content and unrelated metadata are deliberately absent.
    pub frozen_rows: Vec<FrozenLifecycleRow>,
    /// Targets proven absent at plan time. Apply revalidates their absence.
    pub missing_targets: Vec<String>,
    pub mutations: Vec<LifecycleMutation>,
    pub adjudication_required: Vec<LifecycleAdjudication>,
    pub planned_mutations: usize,
    pub adjudication_count: usize,
    pub plan_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleReceiptRow {
    pub kind: LifecycleMutationKind,
    pub related_row_id: String,
    pub before: FrozenLifecycleRow,
    pub after: FrozenLifecycleRow,
    /// Final post-apply state of the companion row whose relationship makes
    /// this transition safe. Restore CAS revalidates it before writing.
    pub related_after: FrozenLifecycleRow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleConsistencyReceipt {
    pub schema_version: u32,
    pub policy_version: String,
    pub target_db_identity: String,
    pub plan_digest: String,
    pub applied_at: String,
    pub rows: Vec<LifecycleReceiptRow>,
    pub applied_mutations: usize,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleConsistencyApplyResult {
    pub applied_mutations: usize,
    pub adjudication_count: usize,
    pub receipt: LifecycleConsistencyReceipt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleConsistencyRestoreResult {
    pub restored_rows: usize,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical(path: &str) -> Result<PathBuf, MemoryError> {
    std::fs::canonicalize(Path::new(path)).map_err(MemoryError::from)
}

fn frozen_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FrozenLifecycleRow> {
    Ok(FrozenLifecycleRow {
        id: row.get(0)?,
        revision: row.get(1)?,
        archived: row.get::<_, i64>(2)? != 0,
        superseded_by: row.get(3)?,
        valid_until: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn load_relevant_rows(conn: &rusqlite::Connection) -> Result<Vec<FrozenLifecycleRow>, MemoryError> {
    let mut statement = conn.prepare(
        "SELECT id,revision,archived,superseded_by,valid_until,updated_at
         FROM memories
         WHERE superseded_by IS NOT NULL
            OR id IN (SELECT superseded_by FROM memories WHERE superseded_by IS NOT NULL)
         ORDER BY id",
    )?;
    let rows = statement
        .query_map([], frozen_from_row)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_row(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<FrozenLifecycleRow>, MemoryError> {
    Ok(conn
        .query_row(
            "SELECT id,revision,archived,superseded_by,valid_until,updated_at
             FROM memories WHERE id=?1",
            [id],
            frozen_from_row,
        )
        .optional()?)
}

fn projection_counts(conn: &rusqlite::Connection) -> Result<BTreeMap<String, i64>, MemoryError> {
    let mut counts = BTreeMap::new();
    for table in [
        "memories_fts",
        "memories_symbolic_fts",
        "memories_vec",
        "access_history",
        "memory_edges",
    ] {
        let exists: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name=?1 AND type IN ('table','view')",
            [table],
            |row| row.get(0),
        )?;
        if exists != 0 {
            let sql = format!("SELECT count(*) FROM {table}");
            counts.insert(
                table.to_string(),
                conn.query_row(&sql, [], |row| row.get(0))?,
            );
        }
    }
    Ok(counts)
}

fn canonical_cycles(rows: &HashMap<String, FrozenLifecycleRow>) -> Vec<Vec<String>> {
    let mut cycles = BTreeMap::<Vec<String>, Vec<String>>::new();
    let mut starts = rows.keys().cloned().collect::<Vec<_>>();
    starts.sort();
    for start in starts {
        let mut path = Vec::<String>::new();
        let mut positions = HashMap::<String, usize>::new();
        let mut current = start;
        loop {
            if let Some(position) = positions.get(&current).copied() {
                let directed = path[position..].to_vec();
                let mut key = directed.clone();
                key.sort();
                cycles.entry(key).or_insert(directed);
                break;
            }
            let Some(row) = rows.get(&current) else {
                break;
            };
            let Some(next) = row.superseded_by.as_ref() else {
                break;
            };
            positions.insert(current.clone(), path.len());
            path.push(current);
            current = next.clone();
        }
    }
    cycles.into_values().collect()
}

impl LifecycleConsistencyPlan {
    pub fn compute_digest(&self) -> Result<String, MemoryError> {
        let mut plan = self.clone();
        plan.plan_digest.clear();
        Ok(digest(&serde_json::to_vec(&plan)?))
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != LIFECYCLE_CONSISTENCY_SCHEMA_VERSION
            || self.policy_version != LIFECYCLE_CONSISTENCY_POLICY
            || self.target_db_identity.is_empty()
            || self.generated_at.is_empty()
        {
            return Err(MemoryError::InvalidArg(
                "unsupported lifecycle-consistency plan schema/policy".into(),
            ));
        }
        if self.planned_mutations != self.mutations.len()
            || self.adjudication_count != self.adjudication_required.len()
        {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency plan counts mismatch".into(),
            ));
        }
        let mut rows = HashMap::new();
        let mut prior_id: Option<&str> = None;
        for row in &self.frozen_rows {
            if row.id.is_empty()
                || row.revision < 1
                || prior_id.is_some_and(|prior| prior >= row.id.as_str())
                || rows.insert(row.id.as_str(), row).is_some()
            {
                return Err(MemoryError::InvalidArg(format!(
                    "invalid frozen lifecycle row {}",
                    row.id
                )));
            }
            prior_id = Some(&row.id);
        }
        let mut prior_missing: Option<&str> = None;
        for id in &self.missing_targets {
            if id.is_empty()
                || rows.contains_key(id.as_str())
                || prior_missing.is_some_and(|prior| prior >= id.as_str())
            {
                return Err(MemoryError::InvalidArg(format!(
                    "invalid missing lifecycle target {id}"
                )));
            }
            prior_missing = Some(id);
        }
        let mut mutated = HashSet::new();
        let cycle_members = canonical_cycles(
            &self
                .frozen_rows
                .iter()
                .cloned()
                .map(|row| (row.id.clone(), row))
                .collect(),
        )
        .into_iter()
        .flatten()
        .collect::<HashSet<_>>();
        for mutation in &self.mutations {
            if mutation.row_id.is_empty()
                || mutation.related_row_id.is_empty()
                || mutation.row_id == mutation.related_row_id
                || !mutated.insert(mutation.row_id.as_str())
            {
                return Err(MemoryError::InvalidArg(format!(
                    "invalid lifecycle mutation {}",
                    mutation.row_id
                )));
            }
            let row = rows.get(mutation.row_id.as_str()).ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "lifecycle mutation row is not frozen: {}",
                    mutation.row_id
                ))
            })?;
            let related = rows.get(mutation.related_row_id.as_str()).ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "lifecycle related row is not frozen: {}",
                    mutation.related_row_id
                ))
            })?;
            match mutation.kind {
                LifecycleMutationKind::ArchiveOneWayLoser => {
                    if row.archived
                        || cycle_members.contains(&row.id)
                        || row.superseded_by.as_deref() != Some(related.id.as_str())
                        || related.superseded_by.as_deref() == Some(row.id.as_str())
                    {
                        return Err(MemoryError::InvalidArg(format!(
                            "unsafe one-way lifecycle mutation {}",
                            row.id
                        )));
                    }
                }
                LifecycleMutationKind::ClearUniqueWinnerBacklink => {
                    if row.archived
                        || row.superseded_by.as_deref() != Some(related.id.as_str())
                        || !related.archived
                        || related.superseded_by.as_deref() != Some(row.id.as_str())
                    {
                        return Err(MemoryError::InvalidArg(format!(
                            "unsafe unique-winner lifecycle mutation {}",
                            row.id
                        )));
                    }
                }
            }
        }
        for item in &self.adjudication_required {
            if item.row_ids.is_empty()
                || item.row_ids.windows(2).any(|pair| pair[0] >= pair[1])
                || item
                    .row_ids
                    .iter()
                    .any(|id| !rows.contains_key(id.as_str()))
            {
                return Err(MemoryError::InvalidArg(
                    "invalid lifecycle adjudication rows".into(),
                ));
            }
            match item.kind {
                LifecycleAdjudicationKind::MissingTarget => {
                    let Some(target) = item.missing_target_id.as_ref() else {
                        return Err(MemoryError::InvalidArg(
                            "missing-target adjudication lacks target id".into(),
                        ));
                    };
                    if item.row_ids.len() != 1
                        || !self.missing_targets.contains(target)
                        || rows[item.row_ids[0].as_str()].superseded_by.as_ref() != Some(target)
                    {
                        return Err(MemoryError::InvalidArg(
                            "invalid missing-target adjudication".into(),
                        ));
                    }
                }
                LifecycleAdjudicationKind::SelfCycle => {
                    if item.missing_target_id.is_some()
                        || item.row_ids.len() != 1
                        || rows[item.row_ids[0].as_str()].superseded_by.as_ref()
                            != Some(&item.row_ids[0])
                    {
                        return Err(MemoryError::InvalidArg(
                            "invalid self-cycle lifecycle adjudication".into(),
                        ));
                    }
                }
                LifecycleAdjudicationKind::AmbiguousTwoNodeCycle => {
                    if item.missing_target_id.is_some() || item.row_ids.len() != 2 {
                        return Err(MemoryError::InvalidArg(
                            "invalid ambiguous two-node lifecycle cycle".into(),
                        ));
                    }
                    let a = rows[item.row_ids[0].as_str()];
                    let b = rows[item.row_ids[1].as_str()];
                    let active = usize::from(!a.archived) + usize::from(!b.archived);
                    if a.superseded_by.as_deref() != Some(b.id.as_str())
                        || b.superseded_by.as_deref() != Some(a.id.as_str())
                        || active == 1
                    {
                        return Err(MemoryError::InvalidArg(
                            "two-node cycle has a safe unique winner".into(),
                        ));
                    }
                }
                LifecycleAdjudicationKind::LongerCycle => {
                    if item.missing_target_id.is_some() || item.row_ids.len() < 3 {
                        return Err(MemoryError::InvalidArg(
                            "invalid longer lifecycle cycle".into(),
                        ));
                    }
                }
            }
        }
        if !valid_digest(&self.plan_digest) || self.plan_digest != self.compute_digest()? {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency plan digest mismatch".into(),
            ));
        }
        Ok(())
    }
}

impl LifecycleConsistencyReceipt {
    pub fn compute_digest(&self) -> Result<String, MemoryError> {
        let mut receipt = self.clone();
        receipt.receipt_digest.clear();
        Ok(digest(&serde_json::to_vec(&receipt)?))
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != LIFECYCLE_CONSISTENCY_RECEIPT_SCHEMA_VERSION
            || self.policy_version != LIFECYCLE_CONSISTENCY_POLICY
            || self.target_db_identity.is_empty()
            || self.plan_digest.is_empty()
            || self.applied_at.is_empty()
            || self.applied_mutations != self.rows.len()
        {
            return Err(MemoryError::InvalidArg(
                "unsupported or inconsistent lifecycle-consistency receipt".into(),
            ));
        }
        let mut ids = HashSet::new();
        for row in &self.rows {
            if !ids.insert(row.before.id.as_str())
                || row.before.id != row.after.id
                || row.before.revision < 1
                || row.after.revision != row.before.revision + 1
                || row.related_row_id.is_empty()
                || row.related_row_id == row.before.id
                || row.related_after.id != row.related_row_id
                || row.related_after.revision < 1
            {
                return Err(MemoryError::InvalidArg(format!(
                    "invalid lifecycle receipt row {}",
                    row.before.id
                )));
            }
            match row.kind {
                LifecycleMutationKind::ArchiveOneWayLoser => {
                    if row.before.archived
                        || !row.after.archived
                        || row.before.superseded_by.as_deref() != Some(row.related_row_id.as_str())
                        || row.after.superseded_by != row.before.superseded_by
                        || row.related_after.superseded_by.as_deref()
                            == Some(row.before.id.as_str())
                        || row.after.updated_at != self.applied_at
                        || match row.before.valid_until.as_ref() {
                            Some(before) => row.after.valid_until.as_ref() != Some(before),
                            None => {
                                row.after.valid_until.as_deref() != Some(self.applied_at.as_str())
                            }
                        }
                    {
                        return Err(MemoryError::InvalidArg(
                            "invalid archive lifecycle receipt transition".into(),
                        ));
                    }
                }
                LifecycleMutationKind::ClearUniqueWinnerBacklink => {
                    if row.before.archived
                        || row.after.archived
                        || row.before.superseded_by.as_deref() != Some(row.related_row_id.as_str())
                        || row.after.superseded_by.is_some()
                        || row.after.valid_until != row.before.valid_until
                        || row.after.updated_at != self.applied_at
                        || !row.related_after.archived
                        || row.related_after.superseded_by.as_deref()
                            != Some(row.before.id.as_str())
                    {
                        return Err(MemoryError::InvalidArg(
                            "invalid winner lifecycle receipt transition".into(),
                        ));
                    }
                }
            }
        }
        if !valid_digest(&self.plan_digest)
            || !valid_digest(&self.receipt_digest)
            || self.receipt_digest != self.compute_digest()?
        {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency receipt digest mismatch".into(),
            ));
        }
        Ok(())
    }
}

impl MemoryStore {
    pub fn plan_lifecycle_consistency(
        &self,
        target_db_identity: String,
    ) -> Result<LifecycleConsistencyPlan, MemoryError> {
        let frozen_rows = load_relevant_rows(&self.conn)?;
        let rows = frozen_rows
            .iter()
            .cloned()
            .map(|row| (row.id.clone(), row))
            .collect::<HashMap<_, _>>();
        let cycles = canonical_cycles(&rows);
        let cycle_members = cycles.iter().flatten().cloned().collect::<HashSet<_>>();
        let mut mutations = Vec::new();
        let mut adjudication_required = Vec::new();

        for cycle in cycles {
            let mut ids = cycle;
            ids.sort();
            if ids.len() == 1 {
                adjudication_required.push(LifecycleAdjudication {
                    kind: LifecycleAdjudicationKind::SelfCycle,
                    row_ids: ids,
                    missing_target_id: None,
                });
            } else if ids.len() == 2 {
                let active = ids
                    .iter()
                    .filter(|id| !rows[id.as_str()].archived)
                    .cloned()
                    .collect::<Vec<_>>();
                if active.len() == 1 {
                    let winner = active[0].clone();
                    let loser = ids.iter().find(|id| **id != winner).unwrap().clone();
                    mutations.push(LifecycleMutation {
                        kind: LifecycleMutationKind::ClearUniqueWinnerBacklink,
                        row_id: winner,
                        related_row_id: loser,
                    });
                } else {
                    adjudication_required.push(LifecycleAdjudication {
                        kind: LifecycleAdjudicationKind::AmbiguousTwoNodeCycle,
                        row_ids: ids,
                        missing_target_id: None,
                    });
                }
            } else {
                adjudication_required.push(LifecycleAdjudication {
                    kind: LifecycleAdjudicationKind::LongerCycle,
                    row_ids: ids,
                    missing_target_id: None,
                });
            }
        }

        let mut missing_targets = BTreeSet::new();
        for row in rows.values() {
            let Some(target_id) = row.superseded_by.as_ref() else {
                continue;
            };
            let Some(target) = rows.get(target_id) else {
                missing_targets.insert(target_id.clone());
                adjudication_required.push(LifecycleAdjudication {
                    kind: LifecycleAdjudicationKind::MissingTarget,
                    row_ids: vec![row.id.clone()],
                    missing_target_id: Some(target_id.clone()),
                });
                continue;
            };
            if !row.archived
                && !cycle_members.contains(&row.id)
                && target.superseded_by.as_deref() != Some(row.id.as_str())
            {
                mutations.push(LifecycleMutation {
                    kind: LifecycleMutationKind::ArchiveOneWayLoser,
                    row_id: row.id.clone(),
                    related_row_id: target.id.clone(),
                });
            }
        }
        mutations.sort_by(|a, b| a.row_id.cmp(&b.row_id).then_with(|| a.kind.cmp(&b.kind)));
        adjudication_required
            .sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.row_ids.cmp(&b.row_ids)));
        let mut plan = LifecycleConsistencyPlan {
            schema_version: LIFECYCLE_CONSISTENCY_SCHEMA_VERSION,
            policy_version: LIFECYCLE_CONSISTENCY_POLICY.into(),
            target_db_identity,
            generated_at: Utc::now().to_rfc3339(),
            frozen_rows,
            missing_targets: missing_targets.into_iter().collect(),
            planned_mutations: mutations.len(),
            adjudication_count: adjudication_required.len(),
            mutations,
            adjudication_required,
            plan_digest: String::new(),
        };
        plan.plan_digest = plan.compute_digest()?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn apply_lifecycle_consistency(
        &mut self,
        plan: &LifecycleConsistencyPlan,
    ) -> Result<LifecycleConsistencyApplyResult, MemoryError> {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        plan.validate()?;
        let effective: String = self.conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |row| row.get(0),
        )?;
        if canonical(&effective)? != canonical(&plan.target_db_identity)? {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency target DB mismatch".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for frozen in &plan.frozen_rows {
            if load_row(&tx, &frozen.id)?.as_ref() != Some(frozen) {
                return Err(MemoryError::InvalidArg(format!(
                    "lifecycle-consistency frozen row drifted: {}",
                    frozen.id
                )));
            }
        }
        for missing in &plan.missing_targets {
            if load_row(&tx, missing)?.is_some() {
                return Err(MemoryError::InvalidArg(format!(
                    "lifecycle-consistency missing target appeared: {missing}"
                )));
            }
        }
        if load_relevant_rows(&tx)? != plan.frozen_rows {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency relation membership drifted".into(),
            ));
        }
        let projection_counts_before = projection_counts(&tx)?;
        let frozen = plan
            .frozen_rows
            .iter()
            .map(|row| (row.id.as_str(), row))
            .collect::<HashMap<_, _>>();
        let now = Utc::now().to_rfc3339();
        let mut receipt_metadata = Vec::with_capacity(plan.mutations.len());
        for mutation in &plan.mutations {
            let before = frozen[mutation.row_id.as_str()];
            let changed = match mutation.kind {
                LifecycleMutationKind::ArchiveOneWayLoser => tx.execute(
                    "UPDATE memories
                     SET archived=1,valid_until=COALESCE(valid_until,?1),updated_at=?1,revision=revision+1
                     WHERE id=?2 AND revision=?3 AND archived=0
                       AND superseded_by=?4 AND valid_until IS ?5 AND updated_at=?6",
                    params![
                        now,
                        before.id,
                        before.revision,
                        mutation.related_row_id,
                        before.valid_until,
                        before.updated_at
                    ],
                )?,
                LifecycleMutationKind::ClearUniqueWinnerBacklink => tx.execute(
                    "UPDATE memories
                     SET superseded_by=NULL,updated_at=?1,revision=revision+1
                     WHERE id=?2 AND revision=?3 AND archived=0
                       AND superseded_by=?4 AND valid_until IS ?5 AND updated_at=?6",
                    params![
                        now,
                        before.id,
                        before.revision,
                        mutation.related_row_id,
                        before.valid_until,
                        before.updated_at
                    ],
                )?,
            };
            if changed != 1 {
                return Err(MemoryError::InvalidArg(format!(
                    "lifecycle-consistency CAS failed: {}",
                    before.id
                )));
            }
            receipt_metadata.push((
                mutation.kind,
                mutation.related_row_id.clone(),
                before.clone(),
            ));
        }
        if projection_counts(&tx)? != projection_counts_before {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency invariant violated: projection/evidence row counts changed"
                    .into(),
            ));
        }
        let mut receipt_rows = Vec::with_capacity(receipt_metadata.len());
        for (kind, related_row_id, before) in receipt_metadata {
            let after = load_row(&tx, &before.id)?.ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "lifecycle-consistency row disappeared: {}",
                    before.id
                ))
            })?;
            let related_after = load_row(&tx, &related_row_id)?.ok_or_else(|| {
                MemoryError::InvalidArg(format!(
                    "lifecycle-consistency related row disappeared: {related_row_id}"
                ))
            })?;
            receipt_rows.push(LifecycleReceiptRow {
                kind,
                related_row_id,
                before,
                after,
                related_after,
            });
        }
        let mut receipt = LifecycleConsistencyReceipt {
            schema_version: LIFECYCLE_CONSISTENCY_RECEIPT_SCHEMA_VERSION,
            policy_version: LIFECYCLE_CONSISTENCY_POLICY.into(),
            target_db_identity: plan.target_db_identity.clone(),
            plan_digest: plan.plan_digest.clone(),
            applied_at: now,
            applied_mutations: receipt_rows.len(),
            rows: receipt_rows,
            receipt_digest: String::new(),
        };
        receipt.receipt_digest = receipt.compute_digest()?;
        receipt.validate()?;
        tx.commit()?;
        Ok(LifecycleConsistencyApplyResult {
            applied_mutations: plan.mutations.len(),
            adjudication_count: plan.adjudication_required.len(),
            receipt,
        })
    }

    pub fn restore_lifecycle_consistency(
        &mut self,
        receipt: &LifecycleConsistencyReceipt,
    ) -> Result<LifecycleConsistencyRestoreResult, MemoryError> {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        receipt.validate()?;
        let effective: String = self.conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |row| row.get(0),
        )?;
        if canonical(&effective)? != canonical(&receipt.target_db_identity)? {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency receipt target DB mismatch".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for row in &receipt.rows {
            if load_row(&tx, &row.after.id)?.as_ref() != Some(&row.after) {
                return Err(MemoryError::InvalidArg(format!(
                    "lifecycle-consistency restore CAS failed: {}",
                    row.after.id
                )));
            }
            if load_row(&tx, &row.related_row_id)?.as_ref() != Some(&row.related_after) {
                return Err(MemoryError::InvalidArg(format!(
                    "lifecycle-consistency restore companion CAS failed: {}",
                    row.related_row_id
                )));
            }
        }
        let projection_counts_before = projection_counts(&tx)?;
        for row in &receipt.rows {
            let changed = tx.execute(
                "UPDATE memories
                 SET archived=?1,superseded_by=?2,valid_until=?3,updated_at=?4,revision=revision+1
                 WHERE id=?5 AND revision=?6 AND archived=?7
                   AND superseded_by IS ?8 AND valid_until IS ?9 AND updated_at=?10",
                params![
                    i64::from(row.before.archived),
                    row.before.superseded_by,
                    row.before.valid_until,
                    row.before.updated_at,
                    row.after.id,
                    row.after.revision,
                    i64::from(row.after.archived),
                    row.after.superseded_by,
                    row.after.valid_until,
                    row.after.updated_at
                ],
            )?;
            if changed != 1 {
                return Err(MemoryError::InvalidArg(format!(
                    "lifecycle-consistency restore CAS failed: {}",
                    row.after.id
                )));
            }
        }
        if projection_counts(&tx)? != projection_counts_before {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency restore invariant violated: projection/evidence row counts changed"
                    .into(),
            ));
        }
        tx.commit()?;
        Ok(LifecycleConsistencyRestoreResult {
            restored_rows: receipt.rows.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEntry;

    fn entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.into(),
            path: format!("/lifecycle/{id}"),
            summary: String::new(),
            text: format!("content-{id}"),
            importance: 0.5,
            timestamp: "2026-01-01T00:00:00Z".into(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".into(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".into(),
            scope: "project".into(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".into(),
        }
    }

    fn fixture(ids: &[&str]) -> (tempfile::TempDir, MemoryStore, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(crate::MEMORY_DB_FILENAME);
        let mut store = MemoryStore::open(&path.to_string_lossy()).unwrap();
        for id in ids {
            store.insert_if_absent(&entry(id)).unwrap();
        }
        let identity = std::fs::canonicalize(path)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        (dir, store, identity)
    }

    fn set_state(store: &MemoryStore, id: &str, archived: bool, target: Option<&str>) {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
        store
            .conn
            .execute(
                "UPDATE memories SET archived=?1,superseded_by=?2 WHERE id=?3",
                params![i64::from(archived), target, id],
            )
            .unwrap();
    }

    fn bump_revision(store: &MemoryStore, id: &str) {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write).unwrap();
        store
            .conn
            .execute("UPDATE memories SET revision=revision+1 WHERE id=?1", [id])
            .unwrap();
    }

    #[test]
    fn plans_and_applies_one_way_active_superseded_drift() {
        let (_dir, mut store, identity) = fixture(&["loser", "winner"]);
        set_state(&store, "loser", false, Some("winner"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        assert_eq!(plan.planned_mutations, 1);
        assert_eq!(
            plan.mutations[0].kind,
            LifecycleMutationKind::ArchiveOneWayLoser
        );
        let result = store.apply_lifecycle_consistency(&plan).unwrap();
        let row = load_row(&store.conn, "loser").unwrap().unwrap();
        assert!(row.archived);
        assert_eq!(row.superseded_by.as_deref(), Some("winner"));
        assert!(!result.receipt.rows[0].before.archived);
    }

    #[test]
    fn repairs_unique_winner_cycle_and_restore_is_exact_under_cas() {
        let (_dir, mut store, identity) = fixture(&["winner", "loser"]);
        set_state(&store, "winner", false, Some("loser"));
        set_state(&store, "loser", true, Some("winner"));
        let before = load_row(&store.conn, "winner").unwrap().unwrap();
        let projections_before = projection_counts(&store.conn).unwrap();
        let canonical_before: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM memories WHERE archived=0 AND superseded_by IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(canonical_before, 0);
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        assert_eq!(plan.planned_mutations, 1);
        assert_eq!(
            plan.mutations[0].kind,
            LifecycleMutationKind::ClearUniqueWinnerBacklink
        );
        let applied = store.apply_lifecycle_consistency(&plan).unwrap();
        let winner = load_row(&store.conn, "winner").unwrap().unwrap();
        let loser = load_row(&store.conn, "loser").unwrap().unwrap();
        assert!(!winner.archived && winner.superseded_by.is_none());
        assert!(loser.archived && loser.superseded_by.as_deref() == Some("winner"));
        let canonical_after: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM memories WHERE archived=0 AND superseded_by IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(canonical_after, 1);
        assert_eq!(projection_counts(&store.conn).unwrap(), projections_before);
        store
            .restore_lifecycle_consistency(&applied.receipt)
            .unwrap();
        let restored = load_row(&store.conn, "winner").unwrap().unwrap();
        assert_eq!(restored.archived, before.archived);
        assert_eq!(restored.superseded_by, before.superseded_by);
        assert_eq!(restored.valid_until, before.valid_until);
        assert_eq!(restored.updated_at, before.updated_at);
        assert_eq!(restored.revision, before.revision + 2);
        assert_eq!(projection_counts(&store.conn).unwrap(), projections_before);
    }

    #[test]
    fn ambiguous_both_active_cycle_is_named_and_never_mutated() {
        let (_dir, mut store, identity) = fixture(&["alpha", "beta"]);
        set_state(&store, "alpha", false, Some("beta"));
        set_state(&store, "beta", false, Some("alpha"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        assert!(plan.mutations.is_empty());
        assert_eq!(plan.adjudication_required.len(), 1);
        assert_eq!(
            plan.adjudication_required[0].kind,
            LifecycleAdjudicationKind::AmbiguousTwoNodeCycle
        );
        assert_eq!(plan.adjudication_required[0].row_ids, ["alpha", "beta"]);
        let result = store.apply_lifecycle_consistency(&plan).unwrap();
        assert_eq!(result.applied_mutations, 0);
        assert_eq!(load_row(&store.conn, "alpha").unwrap().unwrap().revision, 1);
        assert_eq!(load_row(&store.conn, "beta").unwrap().unwrap().revision, 1);
    }

    #[test]
    fn missing_target_is_adjudication_and_target_appearance_aborts_apply() {
        let (_dir, mut store, identity) = fixture(&["orphan"]);
        set_state(&store, "orphan", false, Some("missing"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        assert!(plan.mutations.is_empty());
        assert_eq!(plan.missing_targets, ["missing"]);
        assert_eq!(
            plan.adjudication_required[0].kind,
            LifecycleAdjudicationKind::MissingTarget
        );
        store.insert_if_absent(&entry("missing")).unwrap();
        let error = store.apply_lifecycle_consistency(&plan).unwrap_err();
        assert!(error.to_string().contains("missing target appeared"));
    }

    #[test]
    fn revision_drift_aborts_every_planned_mutation() {
        let (_dir, mut store, identity) = fixture(&["a", "b", "winner"]);
        set_state(&store, "a", false, Some("winner"));
        set_state(&store, "b", false, Some("winner"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        bump_revision(&store, "b");
        let error = store.apply_lifecycle_consistency(&plan).unwrap_err();
        assert!(error.to_string().contains("frozen row drifted: b"));
        assert!(!load_row(&store.conn, "a").unwrap().unwrap().archived);
        assert!(!load_row(&store.conn, "b").unwrap().unwrap().archived);
    }

    #[test]
    fn longer_and_zero_active_cycles_require_adjudication() {
        let (_dir, store, identity) = fixture(&["a", "b", "c", "self", "x", "y"]);
        set_state(&store, "a", false, Some("b"));
        set_state(&store, "b", false, Some("c"));
        set_state(&store, "c", true, Some("a"));
        set_state(&store, "x", true, Some("y"));
        set_state(&store, "y", true, Some("x"));
        set_state(&store, "self", false, Some("self"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        assert!(plan.mutations.is_empty());
        assert_eq!(plan.adjudication_count, 3);
        assert!(plan
            .adjudication_required
            .iter()
            .any(|item| item.kind == LifecycleAdjudicationKind::LongerCycle));
        assert!(plan.adjudication_required.iter().any(|item| {
            item.kind == LifecycleAdjudicationKind::AmbiguousTwoNodeCycle
                && item.row_ids == ["x", "y"]
        }));
        assert!(plan.adjudication_required.iter().any(|item| {
            item.kind == LifecycleAdjudicationKind::SelfCycle && item.row_ids == ["self"]
        }));
    }

    #[test]
    fn restore_drift_aborts_all_rows() {
        let (_dir, mut store, identity) = fixture(&["a", "b", "winner"]);
        set_state(&store, "a", false, Some("winner"));
        set_state(&store, "b", false, Some("winner"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        let applied = store.apply_lifecycle_consistency(&plan).unwrap();
        bump_revision(&store, "b");
        let error = store
            .restore_lifecycle_consistency(&applied.receipt)
            .unwrap_err();
        assert!(error.to_string().contains("restore CAS failed: b"));
        assert!(load_row(&store.conn, "a").unwrap().unwrap().archived);
        assert!(load_row(&store.conn, "b").unwrap().unwrap().archived);
    }

    #[test]
    fn restore_companion_drift_refuses_to_recreate_a_cycle() {
        let (_dir, mut store, identity) = fixture(&["winner", "loser"]);
        set_state(&store, "winner", false, Some("loser"));
        set_state(&store, "loser", true, Some("winner"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        let applied = store.apply_lifecycle_consistency(&plan).unwrap();
        bump_revision(&store, "loser");
        let error = store
            .restore_lifecycle_consistency(&applied.receipt)
            .unwrap_err();
        assert!(error.to_string().contains("companion CAS failed: loser"));
        assert!(load_row(&store.conn, "winner")
            .unwrap()
            .unwrap()
            .superseded_by
            .is_none());
    }

    #[test]
    fn plan_and_receipt_json_never_emit_content() {
        let (_dir, mut store, identity) = fixture(&["loser", "winner"]);
        set_state(&store, "loser", false, Some("winner"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        let plan_json = serde_json::to_string(&plan).unwrap();
        assert!(!plan_json.contains("content-loser"));
        assert!(!plan_json.contains("content-winner"));
        let receipt = store.apply_lifecycle_consistency(&plan).unwrap().receipt;
        let receipt_json = serde_json::to_string(&receipt).unwrap();
        assert!(!receipt_json.contains("content-loser"));
        assert!(!receipt_json.contains("content-winner"));
    }
}
