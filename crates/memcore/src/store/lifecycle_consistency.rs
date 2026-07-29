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

pub const LIFECYCLE_CONSISTENCY_POLICY: &str = "lifecycle-consistency-v2";
pub const LIFECYCLE_CONSISTENCY_SCHEMA_VERSION: u32 = 1;
pub const LIFECYCLE_CONSISTENCY_RECEIPT_SCHEMA_VERSION: u32 = 2;

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
    ConflictingTerminalAuthority,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LifecycleConsistencyReceipt {
    pub schema_version: u32,
    pub policy_version: String,
    pub target_db_identity: String,
    pub plan_digest: String,
    pub applied_at: String,
    /// `prepared` is durable before the database commit. It is recoverable
    /// only after restore proves the DB has the exact recorded post-state.
    pub phase: LifecycleReceiptPhase,
    pub rows: Vec<LifecycleReceiptRow>,
    pub applied_mutations: usize,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleReceiptPhase {
    Prepared,
    Committed,
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

/// Returns weakly connected supersession components. A component includes all
/// rows that can influence one another's lifecycle authority, not only the
/// rows in a cycle.
fn lifecycle_components(rows: &HashMap<String, FrozenLifecycleRow>) -> Vec<Vec<String>> {
    let mut neighbors = rows
        .keys()
        .cloned()
        .map(|id| (id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for row in rows.values() {
        if let Some(target) = row
            .superseded_by
            .as_ref()
            .filter(|id| rows.contains_key(*id))
        {
            neighbors
                .get_mut(&row.id)
                .expect("every lifecycle row has a component node")
                .insert(target.clone());
            neighbors
                .get_mut(target)
                .expect("existing lifecycle target has a component node")
                .insert(row.id.clone());
        }
    }

    let mut seen = BTreeSet::new();
    let mut components = Vec::new();
    for start in neighbors.keys() {
        if !seen.insert(start.clone()) {
            continue;
        }
        let mut pending = vec![start.clone()];
        let mut component = Vec::new();
        while let Some(id) = pending.pop() {
            component.push(id.clone());
            for next in neighbors[&id].iter().rev() {
                if seen.insert(next.clone()) {
                    pending.push(next.clone());
                }
            }
        }
        component.sort();
        components.push(component);
    }
    components.sort();
    components
}

/// Classify complete weak components before emitting any mutation. This is
/// deliberately component-first: an unresolved downstream target or cycle
/// revokes automatic authority for every row that reaches it.
fn classify_lifecycle_components(
    rows: &HashMap<String, FrozenLifecycleRow>,
) -> (
    Vec<String>,
    Vec<LifecycleMutation>,
    Vec<LifecycleAdjudication>,
) {
    let cycles = canonical_cycles(rows);
    let mut missing_targets = BTreeSet::new();
    let mut mutations = Vec::new();
    let mut adjudication_required = Vec::new();

    for row_ids in lifecycle_components(rows) {
        let component = row_ids.iter().cloned().collect::<HashSet<_>>();
        let component_cycles = cycles
            .iter()
            .filter(|cycle| cycle.iter().any(|id| component.contains(id)))
            .collect::<Vec<_>>();
        debug_assert!(component_cycles.len() <= 1);
        let missing_in_component = row_ids
            .iter()
            .filter_map(|id| rows[id].superseded_by.as_ref())
            .filter(|target| !rows.contains_key(*target))
            .cloned()
            .collect::<BTreeSet<_>>();

        if !missing_in_component.is_empty() {
            for target in missing_in_component {
                missing_targets.insert(target.clone());
                adjudication_required.push(LifecycleAdjudication {
                    kind: LifecycleAdjudicationKind::MissingTarget,
                    row_ids: row_ids.clone(),
                    missing_target_id: Some(target),
                });
            }
            continue;
        }

        let unique_cycle_winner = match component_cycles.first() {
            Some(cycle) if cycle.len() == 1 => {
                adjudication_required.push(LifecycleAdjudication {
                    kind: LifecycleAdjudicationKind::SelfCycle,
                    row_ids: row_ids.clone(),
                    missing_target_id: None,
                });
                continue;
            }
            Some(cycle) if cycle.len() == 2 => {
                let active = cycle
                    .iter()
                    .filter(|id| !rows[id.as_str()].archived)
                    .cloned()
                    .collect::<Vec<_>>();
                if active.len() != 1 {
                    adjudication_required.push(LifecycleAdjudication {
                        kind: LifecycleAdjudicationKind::AmbiguousTwoNodeCycle,
                        row_ids: row_ids.clone(),
                        missing_target_id: None,
                    });
                    continue;
                }
                Some(active[0].clone())
            }
            Some(_) => {
                adjudication_required.push(LifecycleAdjudication {
                    kind: LifecycleAdjudicationKind::LongerCycle,
                    row_ids: row_ids.clone(),
                    missing_target_id: None,
                });
                continue;
            }
            None => {
                let terminals = row_ids
                    .iter()
                    .filter(|id| rows[id.as_str()].superseded_by.is_none())
                    .collect::<Vec<_>>();
                if terminals.len() != 1 || rows[terminals[0].as_str()].archived {
                    adjudication_required.push(LifecycleAdjudication {
                        kind: LifecycleAdjudicationKind::ConflictingTerminalAuthority,
                        row_ids: row_ids.clone(),
                        missing_target_id: None,
                    });
                    continue;
                }
                None
            }
        };

        for id in &row_ids {
            let row = &rows[id];
            let Some(target_id) = row.superseded_by.as_ref() else {
                continue;
            };
            if row.archived {
                continue;
            }
            if unique_cycle_winner.as_deref() == Some(row.id.as_str()) {
                let cycle = component_cycles[0];
                let loser = cycle
                    .iter()
                    .find(|id| **id != row.id)
                    .expect("two-node cycle");
                mutations.push(LifecycleMutation {
                    kind: LifecycleMutationKind::ClearUniqueWinnerBacklink,
                    row_id: row.id.clone(),
                    related_row_id: loser.clone(),
                });
            } else {
                mutations.push(LifecycleMutation {
                    kind: LifecycleMutationKind::ArchiveOneWayLoser,
                    row_id: row.id.clone(),
                    related_row_id: target_id.clone(),
                });
            }
        }
    }

    mutations.sort_by(|a, b| a.row_id.cmp(&b.row_id).then_with(|| a.kind.cmp(&b.kind)));
    adjudication_required.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.row_ids.cmp(&b.row_ids))
            .then_with(|| a.missing_target_id.cmp(&b.missing_target_id))
    });
    (
        missing_targets.into_iter().collect(),
        mutations,
        adjudication_required,
    )
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
        let owned_rows = self
            .frozen_rows
            .iter()
            .cloned()
            .map(|row| (row.id.clone(), row))
            .collect::<HashMap<_, _>>();
        let (expected_missing_targets, expected_mutations, expected_adjudications) =
            classify_lifecycle_components(&owned_rows);
        if self.missing_targets != expected_missing_targets
            || self.mutations != expected_mutations
            || self.adjudication_required != expected_adjudications
        {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency plan does not preserve component-wide authority".into(),
            ));
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

    pub fn into_committed(mut self) -> Result<Self, MemoryError> {
        if self.phase != LifecycleReceiptPhase::Prepared {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency receipt is not prepared".into(),
            ));
        }
        self.phase = LifecycleReceiptPhase::Committed;
        self.receipt_digest.clear();
        self.receipt_digest = self.compute_digest()?;
        self.validate()?;
        Ok(self)
    }
}

fn prepare_receipt(
    plan: &LifecycleConsistencyPlan,
    applied_at: String,
) -> Result<LifecycleConsistencyReceipt, MemoryError> {
    let frozen = plan
        .frozen_rows
        .iter()
        .cloned()
        .map(|row| (row.id.clone(), row))
        .collect::<HashMap<_, _>>();
    let mut post_state = frozen.clone();
    for mutation in &plan.mutations {
        let before = frozen.get(&mutation.row_id).ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "lifecycle-consistency mutation row is not frozen: {}",
                mutation.row_id
            ))
        })?;
        let after = post_state.get_mut(&mutation.row_id).ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "lifecycle-consistency mutation row disappeared: {}",
                mutation.row_id
            ))
        })?;
        match mutation.kind {
            LifecycleMutationKind::ArchiveOneWayLoser => {
                after.archived = true;
                if after.valid_until.is_none() {
                    after.valid_until = Some(applied_at.clone());
                }
            }
            LifecycleMutationKind::ClearUniqueWinnerBacklink => {
                after.superseded_by = None;
            }
        }
        after.updated_at = applied_at.clone();
        after.revision = before.revision + 1;
    }

    let rows = plan
        .mutations
        .iter()
        .map(|mutation| {
            Ok(LifecycleReceiptRow {
                kind: mutation.kind,
                related_row_id: mutation.related_row_id.clone(),
                before: frozen.get(&mutation.row_id).cloned().ok_or_else(|| {
                    MemoryError::InvalidArg(format!(
                        "lifecycle-consistency receipt row is not frozen: {}",
                        mutation.row_id
                    ))
                })?,
                after: post_state.get(&mutation.row_id).cloned().ok_or_else(|| {
                    MemoryError::InvalidArg(format!(
                        "lifecycle-consistency receipt row disappeared: {}",
                        mutation.row_id
                    ))
                })?,
                related_after: post_state
                    .get(&mutation.related_row_id)
                    .cloned()
                    .ok_or_else(|| {
                        MemoryError::InvalidArg(format!(
                            "lifecycle-consistency receipt related row is not frozen: {}",
                            mutation.related_row_id
                        ))
                    })?,
            })
        })
        .collect::<Result<Vec<_>, MemoryError>>()?;
    let mut receipt = LifecycleConsistencyReceipt {
        schema_version: LIFECYCLE_CONSISTENCY_RECEIPT_SCHEMA_VERSION,
        policy_version: LIFECYCLE_CONSISTENCY_POLICY.into(),
        target_db_identity: plan.target_db_identity.clone(),
        plan_digest: plan.plan_digest.clone(),
        applied_at,
        phase: LifecycleReceiptPhase::Prepared,
        applied_mutations: rows.len(),
        rows,
        receipt_digest: String::new(),
    };
    receipt.receipt_digest = receipt.compute_digest()?;
    receipt.validate()?;
    Ok(receipt)
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
        let (missing_targets, mutations, adjudication_required) =
            classify_lifecycle_components(&rows);
        let mut plan = LifecycleConsistencyPlan {
            schema_version: LIFECYCLE_CONSISTENCY_SCHEMA_VERSION,
            policy_version: LIFECYCLE_CONSISTENCY_POLICY.into(),
            target_db_identity,
            generated_at: Utc::now().to_rfc3339(),
            frozen_rows,
            missing_targets,
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

    /// Build the content-free recovery receipt before acquiring the mutation
    /// transaction. Callers must durably persist these exact bytes before
    /// calling [`Self::apply_prepared_lifecycle_consistency`].
    pub fn prepare_lifecycle_consistency_receipt(
        &self,
        plan: &LifecycleConsistencyPlan,
    ) -> Result<LifecycleConsistencyReceipt, MemoryError> {
        plan.validate()?;
        prepare_receipt(plan, Utc::now().to_rfc3339())
    }

    pub fn apply_prepared_lifecycle_consistency(
        &mut self,
        plan: &LifecycleConsistencyPlan,
        prepared_receipt: &LifecycleConsistencyReceipt,
    ) -> Result<LifecycleConsistencyApplyResult, MemoryError> {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        plan.validate()?;
        prepared_receipt.validate()?;
        if prepared_receipt.phase != LifecycleReceiptPhase::Prepared {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency apply requires a prepared receipt".into(),
            ));
        }
        let expected_receipt = prepare_receipt(plan, prepared_receipt.applied_at.clone())?;
        if prepared_receipt != &expected_receipt {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency prepared receipt does not match the frozen plan".into(),
            ));
        }
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
        let now = &prepared_receipt.applied_at;
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
        }
        if projection_counts(&tx)? != projection_counts_before {
            return Err(MemoryError::InvalidArg(
                "lifecycle-consistency invariant violated: projection/evidence row counts changed"
                    .into(),
            ));
        }
        for row in &prepared_receipt.rows {
            if load_row(&tx, &row.after.id)?.as_ref() != Some(&row.after) {
                return Err(MemoryError::InvalidArg(format!(
                    "lifecycle-consistency post-apply CAS failed: {}",
                    row.after.id
                )));
            }
            if load_row(&tx, &row.related_after.id)?.as_ref() != Some(&row.related_after) {
                return Err(MemoryError::InvalidArg(format!(
                    "lifecycle-consistency post-apply companion CAS failed: {}",
                    row.related_after.id
                )));
            }
        }
        tx.commit()?;
        Ok(LifecycleConsistencyApplyResult {
            applied_mutations: plan.mutations.len(),
            adjudication_count: plan.adjudication_required.len(),
            receipt: prepared_receipt.clone(),
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

    fn apply_plan(
        store: &mut MemoryStore,
        plan: &LifecycleConsistencyPlan,
    ) -> LifecycleConsistencyApplyResult {
        let prepared = store.prepare_lifecycle_consistency_receipt(plan).unwrap();
        store
            .apply_prepared_lifecycle_consistency(plan, &prepared)
            .unwrap()
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
        let result = apply_plan(&mut store, &plan);
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
        let applied = apply_plan(&mut store, &plan);
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
    fn prepared_receipt_cannot_restore_before_the_recorded_apply_state_exists() {
        let (_dir, mut store, identity) = fixture(&["loser", "winner"]);
        set_state(&store, "loser", false, Some("winner"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        let prepared = store.prepare_lifecycle_consistency_receipt(&plan).unwrap();
        let error = store.restore_lifecycle_consistency(&prepared).unwrap_err();
        assert!(error.to_string().contains("restore CAS failed: loser"));
        assert!(!load_row(&store.conn, "loser").unwrap().unwrap().archived);
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
        let result = apply_plan(&mut store, &plan);
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
        let prepared = store.prepare_lifecycle_consistency_receipt(&plan).unwrap();
        let error = store
            .apply_prepared_lifecycle_consistency(&plan, &prepared)
            .unwrap_err();
        assert!(error.to_string().contains("missing target appeared"));
    }

    #[test]
    fn component_with_active_chain_to_missing_target_is_wholly_adjudication_required() {
        let (_dir, mut store, identity) = fixture(&["a", "b"]);
        // A(active) -> B(active) -> MISSING. The edge-local rule would have
        // archived A; component-wide authority must leave both rows untouched.
        set_state(&store, "a", false, Some("b"));
        set_state(&store, "b", false, Some("missing"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        assert!(plan.mutations.is_empty());
        assert_eq!(plan.missing_targets, ["missing"]);
        assert_eq!(plan.adjudication_required.len(), 1);
        assert_eq!(
            plan.adjudication_required[0],
            LifecycleAdjudication {
                kind: LifecycleAdjudicationKind::MissingTarget,
                row_ids: vec!["a".into(), "b".into()],
                missing_target_id: Some("missing".into()),
            }
        );
        let result = apply_plan(&mut store, &plan);
        assert_eq!(result.applied_mutations, 0);
        assert!(!load_row(&store.conn, "a").unwrap().unwrap().archived);
        assert!(!load_row(&store.conn, "b").unwrap().unwrap().archived);
    }

    #[test]
    fn inbound_active_row_to_ambiguous_cycle_is_wholly_adjudication_required() {
        let (_dir, store, identity) = fixture(&["a", "b", "c"]);
        // A(active) -> B(active), with B(active) <-> C(active). A must not be
        // archived merely because its direct target is currently present.
        set_state(&store, "a", false, Some("b"));
        set_state(&store, "b", false, Some("c"));
        set_state(&store, "c", false, Some("b"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        assert!(plan.mutations.is_empty());
        assert_eq!(plan.adjudication_required.len(), 1);
        assert_eq!(
            plan.adjudication_required[0],
            LifecycleAdjudication {
                kind: LifecycleAdjudicationKind::AmbiguousTwoNodeCycle,
                row_ids: vec!["a".into(), "b".into(), "c".into()],
                missing_target_id: None,
            }
        );
    }

    #[test]
    fn no_mutated_component_contains_adjudication() {
        let (_dir, store, identity) = fixture(&["a", "b", "safe", "winner"]);
        set_state(&store, "a", false, Some("b"));
        set_state(&store, "b", false, Some("missing"));
        set_state(&store, "safe", false, Some("winner"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        let rows = plan
            .frozen_rows
            .iter()
            .cloned()
            .map(|row| (row.id.clone(), row))
            .collect::<HashMap<_, _>>();
        for component in lifecycle_components(&rows) {
            let has_mutation = plan
                .mutations
                .iter()
                .any(|mutation| component.contains(&mutation.row_id));
            let has_adjudication = plan
                .adjudication_required
                .iter()
                .any(|item| item.row_ids.iter().any(|row_id| component.contains(row_id)));
            assert!(
                !(has_mutation && has_adjudication),
                "component {:?} must not mix automatic mutation and adjudication",
                component
            );
        }
    }

    #[test]
    fn archived_terminal_revokes_component_auto_authority() {
        let (_dir, store, identity) = fixture(&["active", "terminal"]);
        set_state(&store, "active", false, Some("terminal"));
        set_state(&store, "terminal", true, None);
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        assert!(plan.mutations.is_empty());
        assert_eq!(
            plan.adjudication_required,
            vec![LifecycleAdjudication {
                kind: LifecycleAdjudicationKind::ConflictingTerminalAuthority,
                row_ids: vec!["active".into(), "terminal".into()],
                missing_target_id: None,
            }]
        );
    }

    #[test]
    fn revision_drift_aborts_every_planned_mutation() {
        let (_dir, mut store, identity) = fixture(&["a", "b", "winner"]);
        set_state(&store, "a", false, Some("winner"));
        set_state(&store, "b", false, Some("winner"));
        let plan = store.plan_lifecycle_consistency(identity).unwrap();
        bump_revision(&store, "b");
        let prepared = store.prepare_lifecycle_consistency_receipt(&plan).unwrap();
        let error = store
            .apply_prepared_lifecycle_consistency(&plan, &prepared)
            .unwrap_err();
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
        let applied = apply_plan(&mut store, &plan);
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
        let applied = apply_plan(&mut store, &plan);
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
        let receipt = apply_plan(&mut store, &plan).receipt;
        let receipt_json = serde_json::to_string(&receipt).unwrap();
        assert!(!receipt_json.contains("content-loser"));
        assert!(!receipt_json.contains("content-winner"));
    }
}
