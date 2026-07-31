//! Portable, strict same-path byte-exact duplicate maintenance.

use super::super::{MemoryError, MemoryStore};
use crate::path_router::normalize_path;
use chrono::Utc;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const EXACT_DEDUPE_POLICY: &str = "exact-text-same-normalized-path-v1";
pub const EXACT_DEDUPE_SCHEMA_VERSION: u32 = 2;
pub const EXACT_DEDUPE_RECEIPT_SCHEMA_VERSION: u32 = 2;
pub const EXACT_DEDUPE_LEGACY_RECEIPT_SCHEMA_VERSION: u32 = 1;
const EXACT_DEDUPE_IN_MEMORY_PHYSICAL_DB_IDENTITY: &str = "exact-dedupe-in-memory-test-sentinel-v1";
const EXACT_DEDUPE_APPLY_ID_METADATA_PATH: &str = "$._tachi_exact_dedupe_apply_id";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenExactRow {
    pub id: String,
    pub revision: i64,
    pub archived: bool,
    pub superseded_by: Option<String>,
    pub path: String,
    pub text_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactDedupeGroup {
    pub normalized_path: String,
    pub text_digest: String,
    pub winner: FrozenExactRow,
    pub losers: Vec<FrozenExactRow>,
    pub ranked_candidates: Vec<ExactDedupeCandidateEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactDedupeCandidateEvidence {
    pub row: FrozenExactRow,
    pub retention_policy: Option<String>,
    pub retention_rank: i32,
    pub tier: String,
    pub tier_rank: i32,
    pub query_diversity: i64,
    pub recall_count: i64,
    pub access_count: i64,
    pub vector_present: bool,
    pub metadata_fields: usize,
    pub metadata_bytes: usize,
    pub rank: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactDedupePlan {
    pub schema_version: u32,
    pub policy_version: String,
    pub target_db_identity: String,
    pub target_db_physical_identity: String,
    pub generated_at: String,
    pub groups: Vec<ExactDedupeGroup>,
    pub planned_groups: usize,
    pub planned_losers: usize,
    pub plan_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactDedupeApplyResult {
    pub applied_groups: usize,
    pub applied_losers: usize,
    /// Durable, hash-bound audit/rollback record for this apply. The caller
    /// (CLI adapter) is responsible for persisting this to disk before
    /// treating the apply as complete — see #1348 "receipt conventions".
    pub receipt: ExactDedupeReceipt,
}

/// One archived loser as recorded at apply time — the exact CAS binding
/// [`MemoryStore::restore_exact_dedupe`] revalidates before undoing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactDedupeReceiptRow {
    pub winner_id: String,
    pub loser_id: String,
    pub loser_path: String,
    /// `memories.valid_until` immediately before this apply. The archive
    /// mutation only fills `valid_until` in when it was previously NULL
    /// (`COALESCE(valid_until, now)`); restore must put back exactly this
    /// value, not unconditionally clear it.
    pub loser_valid_until_before: Option<String>,
    pub before_revision: i64,
    pub archived_revision: i64,
}

/// Post-apply, durable audit/rollback record for one [`ExactDedupePlan`]
/// application. Hash-bound the same way the plan itself is: clear
/// `receipt_digest`, serialize, SHA-256 the bytes.
///
/// Restoring a receipt undoes the `memories.archived`/`superseded_by`
/// state of each loser row under revision CAS. It does **not** attempt to
/// un-merge `memory_edges` transferred onto the winner during apply:
/// edges that collided with an edge the winner already had were dropped
/// (deduped) rather than recorded per-loser, so which loser "owned" a
/// given post-merge winner edge is not always recoverable. This mirrors
/// the existing capture-archive-sweep receipt/restore contract, which
/// also only reverses the archived-row state it durably recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactDedupeReceipt {
    pub schema_version: u32,
    pub policy_version: String,
    pub target_db_identity: String,
    pub target_db_physical_identity: String,
    pub plan_digest: String,
    /// Per-apply lineage token also written onto every archived loser in the
    /// same SQLite transaction. Restore requires this exact token, so a
    /// re-hashed receipt cannot claim an unrelated archived row.
    pub apply_id: String,
    pub applied_at: String,
    /// `prepared` is durably written before the database commit. It remains a
    /// valid recovery artifact because restore proves the exact post-state and
    /// apply lineage before mutating anything.
    pub phase: ExactDedupeReceiptPhase,
    pub rows: Vec<ExactDedupeReceiptRow>,
    pub applied_groups: usize,
    pub applied_losers: usize,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExactDedupeReceiptPhase {
    Prepared,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactDedupeLegacyReceiptV1 {
    pub schema_version: u32,
    pub policy_version: String,
    pub target_db_identity: String,
    pub plan_digest: String,
    pub applied_at: String,
    pub rows: Vec<ExactDedupeReceiptRow>,
    pub applied_groups: usize,
    pub applied_losers: usize,
    pub receipt_digest: String,
}

#[derive(Debug, Clone)]
pub enum ExactDedupeRestoreReceipt {
    V2(ExactDedupeReceipt),
    V1LegacyPathCas(ExactDedupeLegacyReceiptV1),
}

struct ExactDedupeRestoreView<'a> {
    target_db_identity: &'a str,
    target_db_physical_identity: Option<&'a str>,
    rows: &'a [ExactDedupeReceiptRow],
    reject_reserved_rem: bool,
    apply_id: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactDedupeRestoreResult {
    pub restored_losers: usize,
}

fn validate_receipt_rows(
    rows: &[ExactDedupeReceiptRow],
    applied_losers: usize,
    reject_reserved_rem: bool,
) -> Result<(), MemoryError> {
    if applied_losers != rows.len() {
        return Err(MemoryError::InvalidArg(
            "exact-dedupe receipt counts mismatch".into(),
        ));
    }
    let mut ids = HashSet::new();
    for row in rows {
        if !ids.insert(row.loser_id.as_str())
            || row.loser_id.is_empty()
            || row.winner_id.is_empty()
            || (reject_reserved_rem
                && (crate::namespace::is_reserved_wiki_rem_id(&row.loser_id)
                    || crate::namespace::is_reserved_wiki_rem_id(&row.winner_id)))
            || row.loser_id == row.winner_id
            || row.before_revision < 1
            || row.archived_revision != row.before_revision + 1
        {
            return Err(MemoryError::InvalidArg(format!(
                "invalid exact-dedupe receipt row {}",
                row.loser_id
            )));
        }
    }
    Ok(())
}

impl ExactDedupeReceipt {
    pub fn compute_digest(&self) -> Result<String, MemoryError> {
        let mut r = self.clone();
        r.receipt_digest.clear();
        Ok(digest(&serde_json::to_vec(&r)?))
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != EXACT_DEDUPE_RECEIPT_SCHEMA_VERSION
            || self.policy_version != EXACT_DEDUPE_POLICY
            || self.target_db_identity.is_empty()
            || self.target_db_physical_identity.is_empty()
            || uuid::Uuid::parse_str(&self.apply_id).is_err()
        {
            return Err(MemoryError::InvalidArg(
                "unsupported exact-dedupe receipt schema/policy".into(),
            ));
        }
        validate_receipt_rows(&self.rows, self.applied_losers, true)?;
        if !valid_digest(&self.receipt_digest) || self.receipt_digest != self.compute_digest()? {
            return Err(MemoryError::InvalidArg(
                "exact-dedupe receipt digest mismatch".into(),
            ));
        }
        Ok(())
    }

    pub fn into_committed(mut self) -> Result<Self, MemoryError> {
        if self.phase != ExactDedupeReceiptPhase::Prepared {
            return Err(MemoryError::InvalidArg(
                "exact-dedupe receipt is not prepared".into(),
            ));
        }
        self.phase = ExactDedupeReceiptPhase::Committed;
        self.receipt_digest.clear();
        self.receipt_digest = self.compute_digest()?;
        self.validate()?;
        Ok(self)
    }
}

impl ExactDedupeLegacyReceiptV1 {
    pub fn compute_digest(&self) -> Result<String, MemoryError> {
        let mut r = self.clone();
        r.receipt_digest.clear();
        Ok(digest(&serde_json::to_vec(&r)?))
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != EXACT_DEDUPE_LEGACY_RECEIPT_SCHEMA_VERSION
            || self.policy_version != EXACT_DEDUPE_POLICY
            || self.target_db_identity.is_empty()
        {
            return Err(MemoryError::InvalidArg(
                "unsupported exact-dedupe legacy receipt schema/policy".into(),
            ));
        }
        // Preserve the v1 receipt contract byte-for-byte: the legacy
        // validator predated the reserved `wiki-rem:` namespace and did not
        // reject those ids. Tightening it here would orphan an already-issued
        // rollback receipt instead of merely protecting new v2 applies.
        validate_receipt_rows(&self.rows, self.applied_losers, false)?;
        if !valid_digest(&self.receipt_digest) || self.receipt_digest != self.compute_digest()? {
            return Err(MemoryError::InvalidArg(
                "exact-dedupe legacy receipt digest mismatch".into(),
            ));
        }
        Ok(())
    }
}

impl ExactDedupeRestoreReceipt {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, MemoryError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let schema_version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                MemoryError::InvalidArg(
                    "exact-dedupe receipt missing numeric schema_version".into(),
                )
            })?;
        match schema_version {
            2 => {
                let receipt: ExactDedupeReceipt = serde_json::from_value(value)?;
                receipt.validate()?;
                Ok(Self::V2(receipt))
            }
            1 => {
                let receipt: ExactDedupeLegacyReceiptV1 = serde_json::from_value(value)?;
                receipt.validate()?;
                Ok(Self::V1LegacyPathCas(receipt))
            }
            _ => Err(MemoryError::InvalidArg(format!(
                "unsupported exact-dedupe receipt schema_version {schema_version}"
            ))),
        }
    }

    pub fn validate(&self) -> Result<(), MemoryError> {
        match self {
            Self::V2(receipt) => receipt.validate(),
            Self::V1LegacyPathCas(receipt) => receipt.validate(),
        }
    }

    pub fn target_db_identity(&self) -> &str {
        match self {
            Self::V2(receipt) => &receipt.target_db_identity,
            Self::V1LegacyPathCas(receipt) => &receipt.target_db_identity,
        }
    }

    fn restore_view(&self) -> ExactDedupeRestoreView<'_> {
        match self {
            Self::V2(receipt) => ExactDedupeRestoreView {
                target_db_identity: &receipt.target_db_identity,
                target_db_physical_identity: Some(&receipt.target_db_physical_identity),
                rows: &receipt.rows,
                reject_reserved_rem: true,
                apply_id: Some(&receipt.apply_id),
            },
            Self::V1LegacyPathCas(receipt) => ExactDedupeRestoreView {
                target_db_identity: &receipt.target_db_identity,
                target_db_physical_identity: None,
                rows: &receipt.rows,
                reject_reserved_rem: false,
                apply_id: None,
            },
        }
    }
}

#[derive(Debug)]
struct Candidate {
    row: FrozenExactRow,
    retention_policy: Option<String>,
    retention_rank: i32,
    tier: String,
    tier_rank: i32,
    query_diversity: i64,
    recall_count: i64,
    access_count: i64,
    vector_present: bool,
    metadata_fields: usize,
    metadata_bytes: usize,
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
fn retention_rank(v: Option<&str>) -> i32 {
    match v {
        Some("pinned") => 4,
        Some("permanent") => 3,
        Some("durable") => 2,
        Some("ephemeral") => 1,
        _ => 0,
    }
}
fn tier_rank(v: &str) -> i32 {
    match v {
        "pattern" => 2,
        "consolidated" => 1,
        _ => 0,
    }
}
/// tachi#1459: the three counters ranked below observe the search path only;
/// reads through path-listing routes do not increment them. They decide which
/// of two byte-identical rows survives, so the survivor is the one search has
/// shown more, which is not necessarily the one that has been read more. The
/// rows being compared are exact duplicates, so the choice moves provenance and
/// history, not content.
fn order(a: &Candidate, b: &Candidate) -> Ordering {
    b.retention_rank
        .cmp(&a.retention_rank)
        .then_with(|| b.tier_rank.cmp(&a.tier_rank))
        .then_with(|| b.query_diversity.cmp(&a.query_diversity))
        .then_with(|| b.recall_count.cmp(&a.recall_count))
        .then_with(|| b.access_count.cmp(&a.access_count))
        .then_with(|| b.vector_present.cmp(&a.vector_present))
        .then_with(|| b.metadata_fields.cmp(&a.metadata_fields))
        .then_with(|| b.metadata_bytes.cmp(&a.metadata_bytes))
        .then_with(|| a.row.id.cmp(&b.row.id))
}
fn candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, Candidate)> {
    let id: String = row.get(0)?;
    let path: String = row.get(1)?;
    let text: String = row.get(2)?;
    let metadata: String = row.get(9)?;
    let metadata_fields = serde_json::from_str::<serde_json::Value>(&metadata)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .map(|fields| fields.values().filter(|value| !value.is_null()).count())
        .unwrap_or(0);
    let retention_policy: Option<String> = row.get(4)?;
    let tier: String = row.get(5)?;
    Ok((
        normalize_path(&path),
        text.clone(),
        Candidate {
            row: FrozenExactRow {
                id,
                revision: row.get(3)?,
                archived: row.get::<_, i64>(11)? != 0,
                superseded_by: row.get(12)?,
                path,
                text_digest: digest(text.as_bytes()),
            },
            retention_rank: retention_rank(retention_policy.as_deref()),
            retention_policy,
            tier_rank: tier_rank(&tier),
            tier,
            query_diversity: row.get(6)?,
            recall_count: row.get(7)?,
            access_count: row.get(8)?,
            vector_present: row.get::<_, i64>(10)? != 0,
            metadata_fields,
            metadata_bytes: metadata.len(),
        },
    ))
}
fn candidate_evidence(candidate: &Candidate, rank: usize) -> ExactDedupeCandidateEvidence {
    ExactDedupeCandidateEvidence {
        row: candidate.row.clone(),
        retention_policy: candidate.retention_policy.clone(),
        retention_rank: candidate.retention_rank,
        tier: candidate.tier.clone(),
        tier_rank: candidate.tier_rank,
        query_diversity: candidate.query_diversity,
        recall_count: candidate.recall_count,
        access_count: candidate.access_count,
        vector_present: candidate.vector_present,
        metadata_fields: candidate.metadata_fields,
        metadata_bytes: candidate.metadata_bytes,
        rank,
    }
}
fn canonical(path: &str) -> Result<PathBuf, MemoryError> {
    std::fs::canonicalize(Path::new(path)).map_err(MemoryError::from)
}

impl ExactDedupePlan {
    pub fn compute_digest(&self) -> Result<String, MemoryError> {
        let mut p = self.clone();
        p.plan_digest.clear();
        Ok(digest(&serde_json::to_vec(&p)?))
    }
    pub fn validate(&self) -> Result<(), MemoryError> {
        if self.schema_version != EXACT_DEDUPE_SCHEMA_VERSION
            || self.policy_version != EXACT_DEDUPE_POLICY
            || self.target_db_identity.is_empty()
            || self.target_db_physical_identity.is_empty()
        {
            return Err(MemoryError::InvalidArg(
                "unsupported exact-dedupe schema/policy".into(),
            ));
        }
        if self.planned_groups != self.groups.len()
            || self.planned_losers != self.groups.iter().map(|g| g.losers.len()).sum::<usize>()
        {
            return Err(MemoryError::InvalidArg(
                "exact-dedupe plan counts mismatch".into(),
            ));
        }
        let mut ids = HashSet::new();
        for g in &self.groups {
            if g.losers.is_empty()
                || g.ranked_candidates.len() != g.losers.len() + 1
                || normalize_path(&g.normalized_path) != g.normalized_path
                || !valid_digest(&g.text_digest)
            {
                return Err(MemoryError::InvalidArg(
                    "incomplete exact-dedupe group".into(),
                ));
            }
            for row in std::iter::once(&g.winner).chain(g.losers.iter()) {
                if !ids.insert(&row.id)
                    || row.archived
                    || row.superseded_by.is_some()
                    || normalize_path(&row.path) != g.normalized_path
                    || !valid_digest(&row.text_digest)
                    || row.text_digest != g.text_digest
                {
                    return Err(MemoryError::InvalidArg(format!(
                        "invalid frozen row {}",
                        row.id
                    )));
                }
            }
            let expected_rows = std::iter::once(&g.winner)
                .chain(g.losers.iter())
                .collect::<Vec<_>>();
            let mut candidates = Vec::with_capacity(g.ranked_candidates.len());
            for (index, evidence) in g.ranked_candidates.iter().enumerate() {
                if evidence.rank != index + 1
                    || &evidence.row != expected_rows[index]
                    || evidence.retention_rank
                        != retention_rank(evidence.retention_policy.as_deref())
                    || evidence.tier_rank != tier_rank(&evidence.tier)
                {
                    return Err(MemoryError::InvalidArg(
                        "exact-dedupe candidate ranking mismatch".into(),
                    ));
                }
                candidates.push(Candidate {
                    row: evidence.row.clone(),
                    retention_policy: evidence.retention_policy.clone(),
                    retention_rank: evidence.retention_rank,
                    tier: evidence.tier.clone(),
                    tier_rank: evidence.tier_rank,
                    query_diversity: evidence.query_diversity,
                    recall_count: evidence.recall_count,
                    access_count: evidence.access_count,
                    vector_present: evidence.vector_present,
                    metadata_fields: evidence.metadata_fields,
                    metadata_bytes: evidence.metadata_bytes,
                });
            }
            if candidates
                .windows(2)
                .any(|pair| order(&pair[0], &pair[1]).is_gt())
            {
                return Err(MemoryError::InvalidArg(
                    "exact-dedupe candidates are not canonically ranked".into(),
                ));
            }
        }
        if !valid_digest(&self.plan_digest) || self.plan_digest != self.compute_digest()? {
            return Err(MemoryError::InvalidArg(
                "exact-dedupe plan digest mismatch".into(),
            ));
        }
        Ok(())
    }
}

impl MemoryStore {
    fn exact_dedupe_physical_identity(&self, target: &str) -> Result<String, MemoryError> {
        match self.opened_physical_db_identity.clone() {
            Some(identity) => Ok(identity),
            None if target == ":memory:" => {
                Ok(EXACT_DEDUPE_IN_MEMORY_PHYSICAL_DB_IDENTITY.to_string())
            }
            None => Err(MemoryError::InvalidArg(
                "exact-dedupe file-backed plan requires a stable physical DB identity".to_string(),
            )),
        }
    }

    fn validate_exact_dedupe_physical_identity(
        &self,
        artifact_kind: &str,
        expected: &str,
    ) -> Result<(), MemoryError> {
        match self.opened_physical_db_identity.as_deref() {
            Some(opened) if opened == expected => Ok(()),
            Some(_) => Err(MemoryError::InvalidArg(format!(
                "exact-dedupe {artifact_kind} physical DB identity mismatch"
            ))),
            None if expected == EXACT_DEDUPE_IN_MEMORY_PHYSICAL_DB_IDENTITY => Ok(()),
            None => Err(MemoryError::InvalidArg(format!(
                "exact-dedupe {artifact_kind} requires a file-backed physical DB identity"
            ))),
        }
    }

    fn validate_exact_dedupe_target_at_mutation_boundary(
        &self,
        artifact_kind: &str,
        target_db_identity: &str,
        expected_physical_identity: Option<&str>,
    ) -> Result<(), MemoryError> {
        if target_db_identity == ":memory:" {
            if let Some(expected) = expected_physical_identity {
                self.validate_exact_dedupe_physical_identity(artifact_kind, expected)?;
            }
            return Ok(());
        }

        let effective: String = self.conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )?;
        if canonical(&effective)? != canonical(target_db_identity)? {
            return Err(MemoryError::InvalidArg(format!(
                "exact-dedupe {artifact_kind} target DB mismatch"
            )));
        }
        if self.opened_physical_db_identity.is_some() {
            self.verify_opened_physical_db_identity(Path::new(target_db_identity))?;
        }
        if let Some(expected) = expected_physical_identity {
            self.validate_exact_dedupe_physical_identity(artifact_kind, expected)?;
        }
        Ok(())
    }

    pub fn plan_exact_dedupe(
        &self,
        target: String,
        limit: Option<usize>,
        prefix: Option<&str>,
    ) -> Result<ExactDedupePlan, MemoryError> {
        let target_db_physical_identity = self.exact_dedupe_physical_identity(&target)?;
        if limit == Some(0) {
            let mut plan = ExactDedupePlan {
                schema_version: EXACT_DEDUPE_SCHEMA_VERSION,
                policy_version: EXACT_DEDUPE_POLICY.into(),
                target_db_identity: target,
                target_db_physical_identity,
                generated_at: Utc::now().to_rfc3339(),
                groups: Vec::new(),
                planned_groups: 0,
                planned_losers: 0,
                plan_digest: String::new(),
            };
            plan.plan_digest = plan.compute_digest()?;
            return Ok(plan);
        }
        let vec_expr = if self.vec_available {
            "EXISTS(SELECT 1 FROM memories_vec v WHERE v.id=m.id)"
        } else {
            "0"
        };
        let mut sql = format!("SELECT id,path,text,revision,retention_policy,tier,query_diversity,recall_count,access_count,metadata,{vec_expr},archived,superseded_by FROM memories m WHERE archived=0 AND superseded_by IS NULL AND id NOT LIKE 'wiki-rem:%'");
        sql.push_str(" ORDER BY path,text,id");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map([], candidate_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.retain(|(_, t, _)| !t.trim().is_empty());
        if let Some(prefix) = prefix {
            let prefix = normalize_path(prefix);
            rows.retain(|(path, _, _)| {
                prefix == "/"
                    || path == &prefix
                    || path
                        .strip_prefix(&prefix)
                        .is_some_and(|tail| tail.starts_with('/'))
            });
        }
        rows.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.row.id.cmp(&b.2.row.id))
        });
        let mut groups = Vec::new();
        let mut start = 0;
        while start < rows.len() {
            let mut end = start + 1;
            while end < rows.len() && rows[end].0 == rows[start].0 && rows[end].1 == rows[start].1 {
                end += 1;
            }
            if end - start > 1 {
                let mut cs = rows[start..end].iter().map(|r| &r.2).collect::<Vec<_>>();
                cs.sort_by(|a, b| order(a, b));
                let evidence = cs
                    .iter()
                    .enumerate()
                    .map(|(index, candidate)| candidate_evidence(candidate, index + 1))
                    .collect();
                groups.push(ExactDedupeGroup {
                    normalized_path: rows[start].0.clone(),
                    text_digest: cs[0].row.text_digest.clone(),
                    winner: cs[0].row.clone(),
                    losers: cs[1..].iter().map(|c| c.row.clone()).collect(),
                    ranked_candidates: evidence,
                });
                if limit.is_some_and(|n| groups.len() >= n) {
                    break;
                }
            }
            start = end;
        }
        let mut plan = ExactDedupePlan {
            schema_version: EXACT_DEDUPE_SCHEMA_VERSION,
            policy_version: EXACT_DEDUPE_POLICY.into(),
            target_db_identity: target,
            target_db_physical_identity,
            generated_at: Utc::now().to_rfc3339(),
            planned_groups: groups.len(),
            planned_losers: groups.iter().map(|g| g.losers.len()).sum(),
            groups,
            plan_digest: String::new(),
        };
        plan.plan_digest = plan.compute_digest()?;
        Ok(plan)
    }

    pub fn apply_exact_dedupe(
        &mut self,
        plan: &ExactDedupePlan,
    ) -> Result<ExactDedupeApplyResult, MemoryError> {
        self.apply_exact_dedupe_with_precommit_receipt(plan, |_| Ok(()))
    }

    pub fn apply_exact_dedupe_with_precommit_receipt(
        &mut self,
        plan: &ExactDedupePlan,
        precommit_receipt: impl FnOnce(&ExactDedupeApplyResult) -> Result<(), MemoryError>,
    ) -> Result<ExactDedupeApplyResult, MemoryError> {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        plan.validate()?;
        self.validate_exact_dedupe_target_at_mutation_boundary(
            "plan",
            &plan.target_db_identity,
            Some(&plan.target_db_physical_identity),
        )?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let vec_expr = if self.vec_available {
            "EXISTS(SELECT 1 FROM memories_vec v WHERE v.id=m.id)"
        } else {
            "0"
        };
        let candidate_sql = format!("SELECT id,path,text,revision,retention_policy,tier,query_diversity,recall_count,access_count,metadata,{vec_expr},archived,superseded_by FROM memories m WHERE id=?1 AND id NOT LIKE 'wiki-rem:%'");
        let live_group_sql = format!("SELECT id,path,text,revision,retention_policy,tier,query_diversity,recall_count,access_count,metadata,{vec_expr},archived,superseded_by FROM memories m WHERE text=?1 AND archived=0 AND superseded_by IS NULL AND id NOT LIKE 'wiki-rem:%'");
        for g in &plan.groups {
            let winner = tx
                .query_row(&candidate_sql, [&g.winner.id], candidate_from_row)
                .optional()?;
            let Some((_, exact_text, _)) = winner else {
                return Err(MemoryError::InvalidArg(format!(
                    "planned row missing: {}",
                    g.winner.id
                )));
            };

            let mut statement = tx.prepare(&live_group_sql)?;
            let mut actual = statement
                .query_map([&exact_text], candidate_from_row)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|(normalized_path, _, _)| normalized_path == &g.normalized_path)
                .map(|(_, _, candidate)| candidate)
                .collect::<Vec<_>>();
            actual.sort_by(order);

            let planned_ids = g
                .ranked_candidates
                .iter()
                .map(|candidate| candidate.row.id.as_str())
                .collect::<HashSet<_>>();
            let actual_ids = actual
                .iter()
                .map(|candidate| candidate.row.id.as_str())
                .collect::<HashSet<_>>();
            if actual.len() != g.ranked_candidates.len() || actual_ids != planned_ids {
                return Err(MemoryError::InvalidArg(format!(
                    "exact-dedupe group membership drifted: {}/{}",
                    g.normalized_path, g.text_digest
                )));
            }

            for (index, candidate) in actual.iter().enumerate() {
                let planned = &g.ranked_candidates[index];
                if candidate_evidence(candidate, index + 1) != *planned {
                    return Err(MemoryError::InvalidArg(format!(
                        "planned candidate evidence drifted: {}",
                        planned.row.id
                    )));
                }
            }
        }
        let now = Utc::now().to_rfc3339();
        let apply_id = uuid::Uuid::new_v4().to_string();
        // The complete text-digest revalidation above runs after BEGIN IMMEDIATE,
        // which excludes intervening writers until commit. The UPDATE therefore
        // needs only the revision/state/original-path CAS predicates.
        let mut receipt_rows = Vec::with_capacity(plan.planned_losers);
        for g in &plan.groups {
            for f in &g.losers {
                let valid_until_before: Option<String> = tx.query_row(
                    "SELECT valid_until FROM memories WHERE id=?1",
                    [&f.id],
                    |r| r.get(0),
                )?;
                let changed = tx.execute(
                    &format!(
                        "UPDATE memories
                         SET archived=1,
                             superseded_by=?1,
                             valid_until=COALESCE(valid_until,?2),
                             updated_at=?2,
                             revision=revision+1,
                             metadata=json_set(
                               CASE WHEN json_valid(metadata) THEN metadata ELSE '{{}}' END,
                               '{EXACT_DEDUPE_APPLY_ID_METADATA_PATH}', ?6
                             )
                         WHERE id=?3
                           AND revision=?4
                           AND archived=0
                           AND superseded_by IS NULL
                           AND path=?5
                           AND id NOT LIKE 'wiki-rem:%'
                           AND json_type(
                                 CASE WHEN json_valid(metadata) THEN metadata ELSE '{{}}' END,
                                 '{EXACT_DEDUPE_APPLY_ID_METADATA_PATH}'
                               ) IS NULL"
                    ),
                    params![g.winner.id, now, f.id, f.revision, f.path, apply_id],
                )?;
                if changed != 1 {
                    return Err(MemoryError::InvalidArg(format!(
                        "exact-dedupe CAS failed: {}",
                        f.id
                    )));
                }
                transfer_edges_to_winner(&tx, &f.id, &g.winner.id)?;
                receipt_rows.push(ExactDedupeReceiptRow {
                    winner_id: g.winner.id.clone(),
                    loser_id: f.id.clone(),
                    loser_path: f.path.clone(),
                    loser_valid_until_before: valid_until_before,
                    before_revision: f.revision,
                    archived_revision: f.revision + 1,
                });
            }
        }
        let mut receipt = ExactDedupeReceipt {
            schema_version: EXACT_DEDUPE_RECEIPT_SCHEMA_VERSION,
            policy_version: EXACT_DEDUPE_POLICY.into(),
            target_db_identity: plan.target_db_identity.clone(),
            target_db_physical_identity: plan.target_db_physical_identity.clone(),
            plan_digest: plan.plan_digest.clone(),
            apply_id,
            applied_at: now,
            phase: ExactDedupeReceiptPhase::Prepared,
            applied_groups: plan.groups.len(),
            applied_losers: plan.planned_losers,
            rows: receipt_rows,
            receipt_digest: String::new(),
        };
        receipt.receipt_digest = receipt.compute_digest()?;
        let mut result = ExactDedupeApplyResult {
            applied_groups: plan.groups.len(),
            applied_losers: plan.planned_losers,
            receipt,
        };
        precommit_receipt(&result)?;
        tx.commit()?;
        result.receipt = result.receipt.into_committed()?;
        Ok(result)
    }

    /// Restore every loser archived by one [`ExactDedupeReceipt`]. All-or-nothing,
    /// like [`MemoryStore::apply_exact_dedupe`]: any row whose live state no
    /// longer matches the receipt's CAS binding aborts the whole restore with
    /// zero writes. Does not attempt to un-merge `memory_edges` — see
    /// [`ExactDedupeReceipt`]'s doc comment for why that is not always
    /// recoverable.
    pub fn restore_exact_dedupe(
        &mut self,
        receipt: &ExactDedupeReceipt,
    ) -> Result<ExactDedupeRestoreResult, MemoryError> {
        receipt.validate()?;
        let restore_receipt = ExactDedupeRestoreReceipt::V2(receipt.clone());
        self.restore_exact_dedupe_versioned(&restore_receipt)
    }

    pub fn restore_exact_dedupe_versioned(
        &mut self,
        receipt: &ExactDedupeRestoreReceipt,
    ) -> Result<ExactDedupeRestoreResult, MemoryError> {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&self.reserved_reference_write)?;
        receipt.validate()?;
        let view = receipt.restore_view();
        self.validate_exact_dedupe_target_at_mutation_boundary(
            "receipt",
            view.target_db_identity,
            view.target_db_physical_identity,
        )?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let restore_sql = if view.reject_reserved_rem {
            format!(
                "UPDATE memories
                 SET archived=0,
                     superseded_by=NULL,
                     valid_until=?1,
                     revision=revision+1,
                     metadata=json_remove(metadata, '{EXACT_DEDUPE_APPLY_ID_METADATA_PATH}')
                 WHERE id=?2
                   AND revision=?3
                   AND archived=1
                   AND superseded_by=?4
                   AND path=?5
                   AND id NOT LIKE 'wiki-rem:%'
                   AND json_extract(metadata, '{EXACT_DEDUPE_APPLY_ID_METADATA_PATH}')=?6"
            )
        } else {
            "UPDATE memories SET archived=0,superseded_by=NULL,valid_until=?1,revision=revision+1 WHERE id=?2 AND revision=?3 AND archived=1 AND superseded_by=?4 AND path=?5 AND ?6 IS NULL".to_string()
        };
        for row in view.rows {
            let changed = tx.execute(
                &restore_sql,
                params![
                    row.loser_valid_until_before,
                    row.loser_id,
                    row.archived_revision,
                    row.winner_id,
                    row.loser_path,
                    view.apply_id
                ],
            )?;
            if changed != 1 {
                return Err(MemoryError::InvalidArg(format!(
                    "exact-dedupe restore CAS failed: {}",
                    row.loser_id
                )));
            }
        }
        tx.commit()?;
        Ok(ExactDedupeRestoreResult {
            restored_losers: view.rows.len(),
        })
    }
}

/// Move every `memory_edges` row touching `loser` onto `winner` in the same
/// transaction as the archive CAS. `loser` is byte-identical to `winner`
/// (that's the entire exact-dedupe eligibility bar), so its edges are
/// semantically the winner's edges post-merge:
///
/// - an edge already present for `winner` with the same `(target, relation)`
///   (or `(source, relation)` on the incoming side) is a duplicate — the
///   loser's copy is dropped, not doubled (`INSERT OR IGNORE` on the
///   `(source_id, target_id, relation)` primary key);
/// - an edge between `loser` and `winner` becomes a winner→winner self-loop
///   after the rename, which is meaningless post-merge, so it is dropped
///   rather than inserted (the `target_id != winner`/`source_id != winner`
///   guards on the `INSERT ... SELECT`).
///
/// `edge_observations` (the append-only Layer-2 evidence ledger, #774) is
/// deliberately left untouched: it is historical record of what was
/// observed about which ids, not a live projection, and is out of scope
/// for #1348's frozen contract.
fn transfer_edges_to_winner(
    tx: &rusqlite::Transaction<'_>,
    loser: &str,
    winner: &str,
) -> Result<(), MemoryError> {
    tx.execute(
        "INSERT OR IGNORE INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
         SELECT ?1, target_id, relation, weight, metadata, created_at, valid_from, valid_to
         FROM memory_edges WHERE source_id = ?2 AND target_id != ?1",
        params![winner, loser],
    )?;
    tx.execute(
        "DELETE FROM memory_edges WHERE source_id = ?1",
        params![loser],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
         SELECT source_id, ?1, relation, weight, metadata, created_at, valid_from, valid_to
         FROM memory_edges WHERE target_id = ?2 AND source_id != ?1",
        params![winner, loser],
    )?;
    tx.execute(
        "DELETE FROM memory_edges WHERE target_id = ?1",
        params![loser],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture_sql<T>(store: &MemoryStore, operation: impl FnOnce() -> T) -> T {
        let _authorization =
            crate::db::authorize_reserved_reference_write(&store.reserved_reference_write)
                .expect("authorize exact-dedupe fixture SQL");
        operation()
    }

    fn insert(store: &MemoryStore, id: &str, path: &str, text: &str) {
        fixture_sql(store, || {
            store.conn.execute(
                "INSERT INTO memories(id,path,text,timestamp,created_at,updated_at) VALUES(?1,?2,?3,'2026-01-01','2026-01-01','2026-01-01')",
                params![id,path,text],
            ).unwrap();
            crate::db::sync_memories_symbolic_fts(&store.conn, id).unwrap();
        });
    }

    fn insert_edge(store: &MemoryStore, source: &str, target: &str, relation: &str) {
        store
            .conn
            .execute(
                "INSERT INTO memory_edges(source_id,target_id,relation,weight,metadata,created_at,valid_from,valid_to) VALUES(?1,?2,?3,1.0,'{}','2026-01-01','2026-01-01',NULL)",
                params![source, target, relation],
            )
            .unwrap();
    }

    fn all_edges(store: &MemoryStore) -> Vec<(String, String, String)> {
        let mut edges: Vec<(String, String, String)> = store
            .conn
            .prepare("SELECT source_id, target_id, relation FROM memory_edges")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        edges.sort();
        edges
    }

    #[test]
    fn deterministic_complete_plan_excludes_whitespace_and_honors_segment_prefix_limit() {
        let store = MemoryStore::open_in_memory().unwrap();
        insert(&store, "z", "/wiki", "same");
        insert(&store, "a", "/wiki", "same");
        insert(&store, "p1", "/wiki/child", "other");
        insert(&store, "p2", "/wiki/child", "other");
        insert(&store, "x1", "/wikipedia", "other");
        insert(&store, "x2", "/wikipedia", "other");
        insert(&store, "b1", "/blank", "\u{2003}");
        insert(&store, "b2", "/blank", "\u{2003}");
        let plan = store
            .plan_exact_dedupe(":memory:".into(), None, Some("/wiki"))
            .unwrap();
        assert_eq!(
            plan.target_db_physical_identity,
            EXACT_DEDUPE_IN_MEMORY_PHYSICAL_DB_IDENTITY
        );
        assert_eq!(plan.groups.len(), 2);
        assert_eq!(plan.groups[0].winner.id, "a");
        assert_eq!(plan.groups[0].ranked_candidates.len(), 2);
        plan.validate().unwrap();
        assert!(store
            .plan_exact_dedupe(":memory:".into(), Some(0), None)
            .unwrap()
            .groups
            .is_empty());
        assert_eq!(
            store
                .plan_exact_dedupe(":memory:".into(), Some(1), None)
                .unwrap()
                .groups
                .len(),
            1
        );

        insert(&store, "literal-1", "/wiki/%_literal", "literal");
        insert(&store, "literal-2", "/Wiki//%_literal/", "literal");
        insert(&store, "wild-1", "/wiki/xxliteral", "literal");
        insert(&store, "wild-2", "/wiki/xxliteral", "literal");
        let literal = store
            .plan_exact_dedupe(":memory:".into(), None, Some("/WIKI//%_literal/"))
            .unwrap();
        assert_eq!(literal.groups.len(), 1);
        assert_eq!(literal.groups[0].normalized_path, "/wiki/%_literal");
        assert_eq!(literal.groups[0].winner.path, "/wiki/%_literal");
        assert_eq!(literal.groups[0].losers[0].path, "/Wiki//%_literal/");
    }

    #[test]
    fn exact_dedupe_never_plans_or_mutates_a_rem_operation_row() {
        let (_dir, identity, mut store) = disk_store();
        insert(
            &store,
            "wiki-rem:protected",
            "/wiki/drafts/protected",
            "same draft text",
        );
        insert(
            &store,
            "ordinary-a",
            "/wiki/drafts/protected",
            "same draft text",
        );
        insert(
            &store,
            "ordinary-b",
            "/wiki/drafts/protected",
            "same draft text",
        );

        let plan = store
            .plan_exact_dedupe(identity, None, Some("/wiki/drafts"))
            .unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert!(plan.groups[0]
            .ranked_candidates
            .iter()
            .all(|candidate| !crate::namespace::is_reserved_wiki_rem_id(&candidate.row.id)));
        store.apply_exact_dedupe(&plan).unwrap();

        let protected = store.get("wiki-rem:protected").unwrap().unwrap();
        assert!(!protected.archived);
        let superseded_by: Option<String> = store
            .conn
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = 'wiki-rem:protected'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(superseded_by.is_none());
    }

    #[test]
    fn exact_dedupe_restore_rejects_a_forged_rem_receipt_before_mutation() {
        let (_dir, identity, mut store) = disk_store();
        insert(&store, "winner", "/same", "same text");
        insert(&store, "loser", "/same", "same text");
        fixture_sql(&store, || {
            store
                .conn
                .execute(
                    "UPDATE memories SET retention_policy = 'pinned' WHERE id = 'winner'",
                    [],
                )
                .unwrap();
        });
        let plan = store.plan_exact_dedupe(identity, None, None).unwrap();
        let mut receipt = store.apply_exact_dedupe(&plan).unwrap().receipt;
        let row = receipt.rows.first_mut().expect("one archived loser");
        row.loser_id = "wiki-rem:protected".to_string();
        insert(
            &store,
            &row.loser_id,
            &row.loser_path,
            "protected REM operation",
        );
        fixture_sql(&store, || {
            store
                .conn
                .execute(
                    "UPDATE memories SET archived = 1, superseded_by = ?1, revision = ?2 WHERE id = ?3",
                    params![row.winner_id, row.archived_revision, row.loser_id],
                )
                .unwrap();
        });
        receipt.receipt_digest = receipt.compute_digest().unwrap();

        let error = store
            .restore_exact_dedupe(&receipt)
            .expect_err("restore must not own the REM operation namespace");
        assert!(error
            .to_string()
            .contains("invalid exact-dedupe receipt row"));
        let archived: bool = store
            .conn
            .query_row(
                "SELECT archived FROM memories WHERE id = 'wiki-rem:protected'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(archived);
    }

    #[test]
    fn exact_dedupe_restore_rejects_a_rehashed_receipt_for_an_unrelated_archived_row() {
        let (_dir, identity, mut store) = disk_store();
        insert(&store, "winner", "/same", "same text");
        insert(&store, "loser", "/same", "same text");
        fixture_sql(&store, || {
            store
                .conn
                .execute(
                    "UPDATE memories SET retention_policy = 'pinned' WHERE id = 'winner'",
                    [],
                )
                .unwrap();
        });
        let plan = store.plan_exact_dedupe(identity, None, None).unwrap();
        let mut forged = store.apply_exact_dedupe(&plan).unwrap().receipt;
        insert(&store, "unrelated", "/same", "different text");
        fixture_sql(&store, || {
            store
                .conn
                .execute(
                    "UPDATE memories
                     SET archived=1,superseded_by='winner',revision=2
                     WHERE id='unrelated'",
                    [],
                )
                .unwrap();
        });
        forged.rows[0].loser_id = "unrelated".to_string();
        forged.receipt_digest.clear();
        forged.receipt_digest = forged.compute_digest().unwrap();

        let error = store
            .restore_exact_dedupe(&forged)
            .expect_err("receipt must prove the row was archived by this exact apply");
        assert!(
            error.to_string().contains("restore CAS failed"),
            "unexpected refusal: {error}"
        );
        let archived: bool = store
            .conn
            .query_row(
                "SELECT archived FROM memories WHERE id='unrelated'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(archived, "failed lineage proof must not mutate the row");
    }

    #[test]
    fn persisted_plan_rejects_unknown_fields_and_tampering() {
        let store = MemoryStore::open_in_memory().unwrap();
        insert(&store, "a", "/a", "same");
        insert(&store, "b", "/a", "same");
        let mut plan = store
            .plan_exact_dedupe(":memory:".into(), None, None)
            .unwrap();
        let mut value = serde_json::to_value(&plan).unwrap();
        value["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ExactDedupePlan>(value).is_err());
        let mut nested = serde_json::to_value(&plan).unwrap();
        nested["groups"][0]["ranked_candidates"][0]["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ExactDedupePlan>(nested).is_err());
        plan.generated_at.push('Z');
        assert!(plan
            .validate()
            .unwrap_err()
            .to_string()
            .contains("digest mismatch"));

        for mutation in 0..4 {
            let mut invalid = store
                .plan_exact_dedupe(":memory:".into(), None, None)
                .unwrap();
            match mutation {
                0 => invalid.groups[0].winner = invalid.groups[0].losers[0].clone(),
                1 => invalid.groups[0].ranked_candidates[0].rank = 2,
                2 => invalid.groups[0].ranked_candidates[0].retention_rank = 99,
                _ => invalid.groups[0].normalized_path = "/different".into(),
            }
            invalid.plan_digest = invalid.compute_digest().unwrap();
            assert!(
                invalid.validate().is_err(),
                "mutation {mutation} was accepted"
            );
        }

        let mut bad_schema = store
            .plan_exact_dedupe(":memory:".into(), None, None)
            .unwrap();
        bad_schema.schema_version += 1;
        bad_schema.plan_digest = bad_schema.compute_digest().unwrap();
        assert!(bad_schema.validate().is_err());
        let mut bad_digest = store
            .plan_exact_dedupe(":memory:".into(), None, None)
            .unwrap();
        bad_digest.plan_digest = "not-sha256".into();
        assert!(bad_digest.validate().is_err());
    }

    #[test]
    fn every_ranking_dimension_is_canonical() {
        let store = MemoryStore::open_in_memory().unwrap();
        let dimensions = [
            ("retention_policy='pinned'", "retention"),
            ("tier='pattern'", "tier"),
            ("query_diversity=1", "query"),
            ("recall_count=1", "recall"),
            ("access_count=1", "access"),
            ("metadata='{\"field\":1}'", "metadata-fields"),
            ("metadata='{\"field\":\"longer-value\"}'", "metadata-bytes"),
        ];
        for (index, (assignment, label)) in dimensions.iter().enumerate() {
            let path = format!("/rank/{index}");
            let preferred = format!("preferred-{index}");
            let other = format!("other-{index}");
            insert(&store, &preferred, &path, "same");
            insert(&store, &other, &path, "same");
            fixture_sql(&store, || {
                store
                    .conn
                    .execute(
                        &format!("UPDATE memories SET {assignment} WHERE id=?1"),
                        [&preferred],
                    )
                    .unwrap();
            });
            let plan = store
                .plan_exact_dedupe(":memory:".into(), None, Some(&path))
                .unwrap();
            assert_eq!(plan.groups[0].winner.id, preferred, "{label}");
        }

        // Vector presence is a comparator dimension independent of optional
        // sqlite-vec fixture setup; exercise it structurally here.
        let row = |id: &str| Candidate {
            row: FrozenExactRow {
                id: id.into(),
                revision: 1,
                archived: false,
                superseded_by: None,
                path: "/v".into(),
                text_digest: digest(b"same"),
            },
            retention_policy: None,
            retention_rank: 0,
            tier: "raw".into(),
            tier_rank: 0,
            query_diversity: 0,
            recall_count: 0,
            access_count: 0,
            vector_present: false,
            metadata_fields: 0,
            metadata_bytes: 2,
        };
        let mut with_vector = row("z");
        with_vector.vector_present = true;
        assert!(order(&with_vector, &row("a")).is_lt());
    }

    fn disk_store() -> (TempDir, String, MemoryStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let identity = path.to_string_lossy().into_owned();
        let store = MemoryStore::open(&identity).unwrap();
        (dir, identity, store)
    }

    fn seed_winner_loser(store: &MemoryStore) {
        insert(store, "winner", "/same", "same text");
        insert(store, "loser", "/same", "same text");
        fixture_sql(store, || {
            store
                .conn
                .execute(
                    "UPDATE memories SET retention_policy='pinned' WHERE id='winner'",
                    [],
                )
                .unwrap();
        });
    }

    #[cfg(unix)]
    #[test]
    fn apply_rejects_replacement_database_even_when_path_and_rows_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let identity = path.to_string_lossy().into_owned();
        let original = MemoryStore::open(&identity).unwrap();
        seed_winner_loser(&original);
        let plan = original
            .plan_exact_dedupe(identity.clone(), None, None)
            .unwrap();
        assert_ne!(
            plan.target_db_physical_identity,
            EXACT_DEDUPE_IN_MEMORY_PHYSICAL_DB_IDENTITY
        );
        drop(original);

        let replacement_path = dir.path().join("replacement.db");
        let replacement_identity = replacement_path.to_string_lossy().into_owned();
        let replacement = MemoryStore::open(&replacement_identity).unwrap();
        seed_winner_loser(&replacement);
        drop(replacement);
        std::fs::rename(&replacement_path, &path).unwrap();

        let mut substituted = MemoryStore::open_existing_read_write(&identity).unwrap();
        let error = substituted
            .apply_exact_dedupe(&plan)
            .expect_err("replacement DB with matching rows must not satisfy the plan");
        assert!(
            error.to_string().contains("physical DB identity mismatch"),
            "unexpected refusal: {error}"
        );
        let archived: i64 = substituted
            .conn
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[cfg(unix)]
    #[test]
    fn apply_rejects_direct_api_target_path_replacement_after_open_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let identity = path.to_string_lossy().into_owned();
        let mut store = MemoryStore::open(&identity).unwrap();
        seed_winner_loser(&store);
        let plan = store
            .plan_exact_dedupe(identity.clone(), None, None)
            .unwrap();

        let replacement_path = dir.path().join("replacement.db");
        drop(MemoryStore::open(&replacement_path.to_string_lossy()).unwrap());
        std::fs::rename(&replacement_path, &path).unwrap();

        let error = store
            .apply_exact_dedupe(&plan)
            .expect_err("direct API apply must reject a replaced target path before mutation");
        assert!(
            error.to_string().contains("identity changed after open"),
            "unexpected refusal: {error}"
        );
        let archived: i64 = store
            .conn
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[cfg(unix)]
    #[test]
    fn restore_rejects_replacement_database_even_when_path_and_revisions_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let identity = path.to_string_lossy().into_owned();
        let mut original = MemoryStore::open(&identity).unwrap();
        seed_winner_loser(&original);
        let plan = original
            .plan_exact_dedupe(identity.clone(), None, None)
            .unwrap();
        let receipt = original.apply_exact_dedupe(&plan).unwrap().receipt;
        drop(original);

        let replacement_path = dir.path().join("replacement.db");
        let replacement_identity = replacement_path.to_string_lossy().into_owned();
        let mut replacement = MemoryStore::open(&replacement_identity).unwrap();
        seed_winner_loser(&replacement);
        let replacement_plan = replacement
            .plan_exact_dedupe(replacement_identity, None, None)
            .unwrap();
        replacement.apply_exact_dedupe(&replacement_plan).unwrap();
        drop(replacement);
        std::fs::rename(&replacement_path, &path).unwrap();

        let mut substituted = MemoryStore::open_existing_read_write(&identity).unwrap();
        let error = substituted
            .restore_exact_dedupe(&receipt)
            .expect_err("replacement DB with matching restore CAS must not satisfy receipt");
        assert!(
            error.to_string().contains("physical DB identity mismatch"),
            "unexpected refusal: {error}"
        );
        let archived: i64 = substituted
            .conn
            .query_row(
                "SELECT archived FROM memories WHERE id='loser'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archived, 1);
    }

    #[cfg(unix)]
    #[test]
    fn restore_rejects_direct_api_target_path_replacement_after_open_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let identity = path.to_string_lossy().into_owned();
        let mut store = MemoryStore::open(&identity).unwrap();
        seed_winner_loser(&store);
        let plan = store
            .plan_exact_dedupe(identity.clone(), None, None)
            .unwrap();
        let receipt = store.apply_exact_dedupe(&plan).unwrap().receipt;

        let replacement_path = dir.path().join("replacement.db");
        drop(MemoryStore::open(&replacement_path.to_string_lossy()).unwrap());
        std::fs::rename(&replacement_path, &path).unwrap();

        let error = store
            .restore_exact_dedupe(&receipt)
            .expect_err("direct API restore must reject a replaced target path before mutation");
        assert!(
            error.to_string().contains("identity changed after open"),
            "unexpected refusal: {error}"
        );
        let archived: i64 = store
            .conn
            .query_row(
                "SELECT archived FROM memories WHERE id='loser'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(archived, 1);
    }

    #[test]
    fn apply_rejects_wrong_target_and_all_drift_with_batch_rollback() {
        let (_dir, identity, mut store) = disk_store();
        insert(&store, "a1", "/a", "same-a");
        insert(&store, "a2", "/a", "same-a");
        insert(&store, "b1", "/b", "same-b");
        insert(&store, "b2", "/b", "same-b");
        let plan = store.plan_exact_dedupe(identity, None, None).unwrap();

        let other = tempfile::NamedTempFile::new().unwrap();
        let mut wrong = plan.clone();
        wrong.target_db_identity = other.path().to_string_lossy().into_owned();
        wrong.plan_digest = wrong.compute_digest().unwrap();
        assert!(store.apply_exact_dedupe(&wrong).is_err());

        for assignment in [
            "revision=revision+1",
            "archived=1",
            "superseded_by='other'",
            "path='/drifted'",
            "text='drifted'",
        ] {
            fixture_sql(&store, || {
                store.conn.execute("UPDATE memories SET revision=1,archived=0,superseded_by=NULL,path='/b',text='same-b' WHERE id='b2'", []).unwrap();
                store
                    .conn
                    .execute(
                        &format!("UPDATE memories SET {assignment} WHERE id='b2'"),
                        [],
                    )
                    .unwrap();
            });
            assert!(
                store.apply_exact_dedupe(&plan).is_err(),
                "accepted {assignment}"
            );
            let archived: i64 = store
                .conn
                .query_row("SELECT archived FROM memories WHERE id='a2'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(
                archived, 0,
                "earlier group changed before later drift rollback"
            );
        }
    }

    #[test]
    fn apply_rejects_ranking_drift_without_mutating_any_loser() {
        let (_dir, identity, mut store) = disk_store();
        insert(&store, "a1", "/a", "same-a");
        insert(&store, "a2", "/a", "same-a");
        insert(&store, "b1", "/b", "same-b");
        insert(&store, "b2", "/b", "same-b");
        let plan = store.plan_exact_dedupe(identity, None, None).unwrap();
        store
            .conn
            .execute("UPDATE memories SET access_count=9 WHERE id='b2'", [])
            .unwrap();

        let error = store.apply_exact_dedupe(&plan).unwrap_err();
        assert!(error.to_string().contains("candidate evidence drifted"));
        let archived: i64 = store
            .conn
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(archived, 0);
    }

    #[test]
    fn apply_rejects_new_exact_group_members_without_mutating_any_loser() {
        for (id, retention_policy) in [("new-member", None), ("new-winner", Some("pinned"))] {
            let (_dir, identity, mut store) = disk_store();
            insert(&store, "winner", "/Wiki//child/", "same");
            insert(&store, "loser", "/wiki/child", "same");
            let plan = store.plan_exact_dedupe(identity, None, None).unwrap();

            insert(&store, id, "/WIKI/child", "same");
            if let Some(policy) = retention_policy {
                fixture_sql(&store, || {
                    store
                        .conn
                        .execute(
                            "UPDATE memories SET retention_policy=?1 WHERE id=?2",
                            params![policy, id],
                        )
                        .unwrap();
                });
            }

            let error = store.apply_exact_dedupe(&plan).unwrap_err();
            assert!(
                error.to_string().contains("group membership drifted"),
                "unexpected refusal for {id}: {error}"
            );
            let archived: i64 = store
                .conn
                .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
                .unwrap();
            assert_eq!(archived, 0, "apply partially mutated group for {id}");
        }
    }

    #[test]
    fn precommit_receipt_sink_failure_rolls_back_exact_dedupe_mutation() {
        let (_dir, identity, mut store) = disk_store();
        seed_winner_loser(&store);
        let plan = store.plan_exact_dedupe(identity, None, None).unwrap();
        let mut sink_called = false;
        let mut prepared_receipt = None;

        let error = store
            .apply_exact_dedupe_with_precommit_receipt(&plan, |result| {
                sink_called = true;
                assert_eq!(result.applied_losers, 1);
                assert_eq!(result.receipt.phase, ExactDedupeReceiptPhase::Prepared);
                prepared_receipt = Some(result.receipt.clone());
                Err(MemoryError::InvalidArg(
                    "injected durable receipt sink failure".into(),
                ))
            })
            .expect_err("receipt sink failure must abort the DB commit");

        assert!(sink_called, "precommit receipt sink must be invoked");
        assert!(
            error
                .to_string()
                .contains("injected durable receipt sink failure"),
            "unexpected refusal: {error}"
        );
        let archived: i64 = store
            .conn
            .query_row("SELECT sum(archived) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            archived, 0,
            "DB transaction committed without durable receipt"
        );
        let restore_error = store
            .restore_exact_dedupe(
                &prepared_receipt.expect("sink observed the exact prepared receipt"),
            )
            .expect_err("prepared receipt must not restore a transaction that rolled back");
        assert!(
            restore_error.to_string().contains("restore CAS failed"),
            "unexpected recovery refusal: {restore_error}"
        );
    }

    #[test]
    fn successful_apply_exposes_prepared_receipt_before_commit_and_committed_after() {
        let (_dir, identity, mut store) = disk_store();
        seed_winner_loser(&store);
        let plan = store.plan_exact_dedupe(identity, None, None).unwrap();

        let result = store
            .apply_exact_dedupe_with_precommit_receipt(&plan, |result| {
                assert_eq!(result.receipt.phase, ExactDedupeReceiptPhase::Prepared);
                Ok(())
            })
            .expect("apply exact dedupe");

        assert_eq!(result.receipt.phase, ExactDedupeReceiptPhase::Committed);
        result
            .receipt
            .validate()
            .expect("validate committed receipt");
    }

    #[test]
    fn successful_apply_soft_archives_and_followup_plan_is_empty() {
        let (_dir, identity, mut store) = disk_store();
        insert(&store, "winner", "/Wiki//child/", "same");
        insert(&store, "loser", "/wiki/child", "same");
        fixture_sql(&store, || {
            store
                .conn
                .execute(
                    "UPDATE memories SET retention_policy='pinned' WHERE id='winner'",
                    [],
                )
                .unwrap();
        });
        let before_total: i64 = store
            .conn
            .query_row("SELECT count(*) FROM memories", [], |r| r.get(0))
            .unwrap();
        let before_fts: i64 = store
            .conn
            .query_row("SELECT count(*) FROM memories_fts", [], |r| r.get(0))
            .unwrap();
        let plan = store
            .plan_exact_dedupe(identity.clone(), None, None)
            .unwrap();
        assert_eq!(plan.groups[0].winner.path, "/Wiki//child/");
        store.apply_exact_dedupe(&plan).unwrap();
        let state: (i64, Option<String>, Option<String>, i64) = store
            .conn
            .query_row(
                "SELECT archived,superseded_by,valid_until,revision FROM memories WHERE id='loser'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(state.0, 1);
        assert_eq!(state.1.as_deref(), Some("winner"));
        assert_eq!(
            store.supersession_target("loser").unwrap(),
            Some(Some("winner".into()))
        );
        assert!(state.2.is_some());
        assert_eq!(state.3, 2);
        assert_eq!(
            store
                .conn
                .query_row("SELECT count(*) FROM memories WHERE archived=0", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .conn
                .query_row("SELECT count(*) FROM memories WHERE id='loser'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .conn
                .query_row("SELECT count(*) FROM memories", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            before_total
        );
        assert_eq!(
            store
                .conn
                .query_row("SELECT count(*) FROM memories_fts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            before_fts
        );
        let recalled = store.search("same", None).unwrap();
        assert!(recalled.iter().any(|result| result.entry.id == "winner"));
        assert!(recalled.iter().all(|result| result.entry.id != "loser"));
        assert!(store.get("loser").unwrap().is_none());
        assert!(store.get_with_options("loser", true).unwrap().is_some());
        assert!(store
            .plan_exact_dedupe(identity, None, None)
            .unwrap()
            .groups
            .is_empty());
    }

    #[test]
    fn existing_read_write_refuses_to_create_absent_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.db");
        assert!(MemoryStore::open_existing_read_write(&path.to_string_lossy()).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn apply_transfers_edges_to_winner_dedupes_and_drops_self_loops() {
        let (_dir, identity, mut store) = disk_store();
        insert(&store, "winner", "/a", "same-a");
        insert(&store, "loser", "/a", "same-a");
        insert(&store, "neighbor-in", "/n1", "unrelated");
        insert(&store, "neighbor-out", "/n2", "unrelated");
        fixture_sql(&store, || {
            store
                .conn
                .execute(
                    "UPDATE memories SET retention_policy='pinned' WHERE id='winner'",
                    [],
                )
                .unwrap();
        });

        // A -> loser: should transfer to A -> winner.
        insert_edge(&store, "neighbor-in", "loser", "related_to");
        // loser -> B: should transfer to winner -> B.
        insert_edge(&store, "loser", "neighbor-out", "related_to");
        // Edges between loser and winner become winner->winner self-loops
        // after the rename and must be dropped, not inserted.
        insert_edge(&store, "loser", "winner", "related_to");
        insert_edge(&store, "winner", "loser", "supports");
        // Winner already has an edge to neighbor-out under the same relation
        // as loser's; the transfer must not double it.
        insert_edge(&store, "winner", "neighbor-out", "related_to");

        let plan = store
            .plan_exact_dedupe(identity, None, None)
            .expect("build plan");
        assert_eq!(plan.groups.len(), 1, "{plan:#?}");

        store.apply_exact_dedupe(&plan).expect("apply plan");

        assert_eq!(
            all_edges(&store),
            vec![
                (
                    "neighbor-in".to_string(),
                    "winner".to_string(),
                    "related_to".to_string()
                ),
                (
                    "winner".to_string(),
                    "neighbor-out".to_string(),
                    "related_to".to_string()
                ),
            ]
        );
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_edges WHERE source_id = target_id",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0,
            "no self-loop should survive the merge"
        );
    }

    #[test]
    fn apply_produces_a_durable_receipt_and_restore_is_hash_bound_and_reversible() {
        let (_dir, identity, mut store) = disk_store();
        insert(&store, "winner", "/a", "same-a");
        insert(&store, "loser", "/a", "same-a");
        fixture_sql(&store, || {
            store
                .conn
                .execute(
                    "UPDATE memories SET retention_policy='pinned' WHERE id='winner'",
                    [],
                )
                .unwrap();
        });
        let plan = store
            .plan_exact_dedupe(identity, None, None)
            .expect("build plan");

        let result = store.apply_exact_dedupe(&plan).expect("apply plan");
        let receipt = result.receipt.clone();
        receipt
            .validate()
            .expect("apply receipt is self-consistent");
        assert_eq!(receipt.phase, ExactDedupeReceiptPhase::Committed);
        assert_eq!(receipt.applied_losers, 1);
        assert_eq!(receipt.rows.len(), 1);
        assert_eq!(receipt.rows[0].loser_id, "loser");
        assert_eq!(receipt.rows[0].winner_id, "winner");
        assert_eq!(receipt.rows[0].archived_revision, 2);

        let archived: i64 = store
            .conn
            .query_row("SELECT archived FROM memories WHERE id='loser'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(archived, 1);

        // A tampered-but-not-rehashed receipt fails schema/digest validation
        // before touching the database at all.
        let mut tampered = receipt.clone();
        tampered.rows[0].archived_revision += 1;
        assert!(store.restore_exact_dedupe(&tampered).is_err());
        let still_archived: i64 = store
            .conn
            .query_row("SELECT archived FROM memories WHERE id='loser'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(still_archived, 1, "failed validation must not mutate");

        // Even a receipt re-hashed after tampering the bound revision fails
        // the real DB CAS and performs zero writes.
        let mut forged = tampered.clone();
        forged.receipt_digest.clear();
        forged.receipt_digest = forged.compute_digest().unwrap();
        assert!(store.restore_exact_dedupe(&forged).is_err());
        let still_archived: i64 = store
            .conn
            .query_row("SELECT archived FROM memories WHERE id='loser'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(still_archived, 1, "failed CAS must not mutate");

        let restored = store
            .restore_exact_dedupe(&receipt)
            .expect("restore the untampered receipt");
        assert_eq!(restored.restored_losers, 1);
        let (archived, superseded_by): (i64, Option<String>) = store
            .conn
            .query_row(
                "SELECT archived, superseded_by FROM memories WHERE id='loser'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(archived, 0);
        assert_eq!(superseded_by, None);
        assert!(store.get("loser").unwrap().is_some());
        let lineage_marker: Option<String> = store
            .conn
            .query_row(
                &format!(
                    "SELECT json_extract(metadata, '{EXACT_DEDUPE_APPLY_ID_METADATA_PATH}') FROM memories WHERE id='loser'"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            lineage_marker.is_none(),
            "restore must remove the internal apply-lineage marker"
        );

        // The receipt is now stale (loser is no longer archived at the
        // recorded revision): repeat restore is a hard refusal, not a
        // silent no-op, and performs zero further writes.
        assert!(store.restore_exact_dedupe(&receipt).is_err());
        let (archived, superseded_by): (i64, Option<String>) = store
            .conn
            .query_row(
                "SELECT archived, superseded_by FROM memories WHERE id='loser'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(archived, 0);
        assert_eq!(superseded_by, None);
    }

    #[test]
    fn legacy_v1_receipt_deserializes_by_version_and_restores_by_path_cas_only() {
        let (_dir, identity, mut store) = disk_store();
        seed_winner_loser(&store);
        let plan = store
            .plan_exact_dedupe(identity.clone(), None, None)
            .expect("build plan");
        let receipt = store.apply_exact_dedupe(&plan).expect("apply plan").receipt;
        let mut legacy = ExactDedupeLegacyReceiptV1 {
            schema_version: EXACT_DEDUPE_LEGACY_RECEIPT_SCHEMA_VERSION,
            policy_version: receipt.policy_version.clone(),
            target_db_identity: receipt.target_db_identity.clone(),
            plan_digest: receipt.plan_digest.clone(),
            applied_at: receipt.applied_at.clone(),
            rows: receipt.rows.clone(),
            applied_groups: receipt.applied_groups,
            applied_losers: receipt.applied_losers,
            receipt_digest: String::new(),
        };
        legacy.receipt_digest = legacy.compute_digest().unwrap();

        let mut legacy_value = serde_json::to_value(&legacy).unwrap();
        legacy_value["target_db_physical_identity"] = serde_json::json!("unix:pretend:identity");
        assert!(
            serde_json::from_value::<ExactDedupeLegacyReceiptV1>(legacy_value).is_err(),
            "v1 restore must not pretend the legacy receipt carried physical identity"
        );

        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
        let parsed = ExactDedupeRestoreReceipt::from_slice(&legacy_bytes)
            .expect("v1 receipt must parse through the version-aware path");
        match &parsed {
            ExactDedupeRestoreReceipt::V1LegacyPathCas(v1) => {
                assert_eq!(v1.target_db_identity, identity);
            }
            ExactDedupeRestoreReceipt::V2(_) => panic!("v1 receipt parsed as v2"),
        }

        store
            .restore_exact_dedupe_versioned(&parsed)
            .expect("legacy v1 path/CAS receipt must restore");
        let (archived, superseded_by): (i64, Option<String>) = store
            .conn
            .query_row(
                "SELECT archived, superseded_by FROM memories WHERE id='loser'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(archived, 0);
        assert_eq!(superseded_by, None);

        let mut tampered = legacy;
        tampered.applied_at.push('Z');
        let tampered_bytes = serde_json::to_vec(&tampered).unwrap();
        let error = ExactDedupeRestoreReceipt::from_slice(&tampered_bytes)
            .expect_err("v1 receipt digest validation must reject tampering");
        assert!(
            error.to_string().contains("legacy receipt digest mismatch"),
            "unexpected v1 digest refusal: {error}"
        );
    }
}
