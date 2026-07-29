//! Sampled, content-free recall impressions and exact pre-boost replay.

use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write;

use crate::{
    error::{MemoryError, RecallReplayCompatibilityReason},
    scorer::{apply_pre_boost_adjustment, fuse_pre_boost_score, HybridWeights, PreBoostAdjustment},
};

pub(crate) const IMPRESSION_GROUP_INSERT_SQL: &str =
    "INSERT INTO recall_impression_groups (group_id, created_at, query_fingerprint, fusion_policy_version, pre_boost_adjustment_version, tie_break_policy_version, candidate_policy_version, schema_identity, weights_profile, semantic_weight, fts_weight, symbolic_weight, decay_weight, use_rrf, rrf_k, top_k, candidate_count, displayed_count, scored_returned_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)";
pub(crate) const IMPRESSION_ROW_INSERT_SQL: &str =
    "INSERT INTO recall_impressions (group_id, memory_id, vector_score, fts_score, symbolic_score, decay_score, vec_rank, fts_rank, sym_rank, merge_adjustment, pre_boost_score, pre_boost_rank, tie_break_epoch_millis, final_score, final_rank, scored, scored_returned, access_count_at_recall) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";

impl PreBoostAdjustment {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ExactId => "exact_id",
            Self::SupersededScale => "superseded_scale",
            Self::ExactIdSupersededScale => "exact_id_superseded_scale",
        }
    }

    fn parse(value: &str) -> Result<Self, MemoryError> {
        match value {
            "none" => Ok(Self::None),
            "exact_id" => Ok(Self::ExactId),
            "superseded_scale" => Ok(Self::SupersededScale),
            "exact_id_superseded_scale" => Ok(Self::ExactIdSupersededScale),
            other => Err(MemoryError::InvalidArg(format!(
                "unknown recall impression merge adjustment: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RecallImpressionRowDraft {
    pub memory_id: String,
    pub vector_score: f64,
    pub fts_score: f64,
    pub symbolic_score: f64,
    pub decay_score: f64,
    pub vec_rank: Option<usize>,
    pub fts_rank: Option<usize>,
    pub sym_rank: Option<usize>,
    pub merge_adjustment: PreBoostAdjustment,
    pub pre_boost_score: f64,
    pub pre_boost_rank: usize,
    pub tie_break_epoch_millis: i64,
    pub final_score: f64,
    pub final_rank: usize,
    pub scored: bool,
    pub scored_returned: bool,
    pub access_count_at_recall: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct RecallImpressionPayload {
    pub group_id: String,
    pub created_at: String,
    /// SHA-256 query fingerprint used for sampling and cohorting. It is not
    /// query text and is intentionally separate from v25's 32-bit FNV bucket.
    pub query_fingerprint: String,
    pub replay_policy: RecallReplayPolicy,
    pub weights_profile: String,
    pub weights: HybridWeights,
    pub rrf_k: f64,
    pub top_k: usize,
    pub rows: Vec<RecallImpressionRowDraft>,
    pub displayed_count: usize,
}

/// Complete, persisted identity of the pre-boost replay algorithm.
///
/// Every field is independently named because a future change to any of them
/// must reject historical replay until a version-specific interpreter exists.
/// The schema identity binds this tuple to the persistent group layout rather
/// than treating a matching set of weights as sufficient evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecallReplayPolicy {
    fusion_policy_version: &'static str,
    pre_boost_adjustment_version: &'static str,
    tie_break_policy_version: &'static str,
    candidate_policy_version: &'static str,
    schema_identity: &'static str,
}

impl RecallReplayPolicy {
    const CURRENT: Self = Self {
        fusion_policy_version: "fusion-v1",
        pre_boost_adjustment_version: "pre-boost-adjustment-v1",
        tie_break_policy_version: "recall-rank-v1",
        candidate_policy_version: "candidate-set-v1",
        schema_identity: "recall-impression-ledger-v26",
    };

    pub(crate) const fn current() -> Self {
        Self::CURRENT
    }

    fn validate_stored(group_id: &str, stored: &StoredReplayPolicy) -> Result<Self, MemoryError> {
        let policy_fields = [
            stored.fusion_policy_version.as_deref(),
            stored.pre_boost_adjustment_version.as_deref(),
            stored.tie_break_policy_version.as_deref(),
            stored.candidate_policy_version.as_deref(),
            stored.schema_identity.as_deref(),
        ];
        if stored.query_fingerprint.is_none() && policy_fields.iter().all(|field| field.is_none()) {
            return Err(replay_incompatible(
                group_id,
                RecallReplayCompatibilityReason::LegacyUnversioned,
            ));
        }
        if policy_fields.iter().any(|field| field.is_none()) {
            return Err(replay_incompatible(
                group_id,
                RecallReplayCompatibilityReason::IncompletePolicy,
            ));
        }
        if !stored.query_fingerprint.as_deref().is_some_and(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(replay_incompatible(
                group_id,
                RecallReplayCompatibilityReason::InvalidQueryFingerprint,
            ));
        }
        if stored.fusion_policy_version.as_deref() != Some(Self::CURRENT.fusion_policy_version) {
            return Err(replay_incompatible(
                group_id,
                RecallReplayCompatibilityReason::UnsupportedFusionPolicy,
            ));
        }
        if stored.pre_boost_adjustment_version.as_deref()
            != Some(Self::CURRENT.pre_boost_adjustment_version)
        {
            return Err(replay_incompatible(
                group_id,
                RecallReplayCompatibilityReason::UnsupportedPreBoostAdjustmentPolicy,
            ));
        }
        if stored.tie_break_policy_version.as_deref()
            != Some(Self::CURRENT.tie_break_policy_version)
        {
            return Err(replay_incompatible(
                group_id,
                RecallReplayCompatibilityReason::UnsupportedTieBreakPolicy,
            ));
        }
        if stored.candidate_policy_version.as_deref()
            != Some(Self::CURRENT.candidate_policy_version)
        {
            return Err(replay_incompatible(
                group_id,
                RecallReplayCompatibilityReason::UnsupportedCandidatePolicy,
            ));
        }
        if stored.schema_identity.as_deref() != Some(Self::CURRENT.schema_identity) {
            return Err(replay_incompatible(
                group_id,
                RecallReplayCompatibilityReason::UnsupportedSchemaIdentity,
            ));
        }
        Ok(Self::CURRENT)
    }
}

#[derive(Debug)]
struct StoredReplayPolicy {
    query_fingerprint: Option<String>,
    fusion_policy_version: Option<String>,
    pre_boost_adjustment_version: Option<String>,
    tie_break_policy_version: Option<String>,
    candidate_policy_version: Option<String>,
    schema_identity: Option<String>,
}

fn replay_incompatible(group_id: &str, reason: RecallReplayCompatibilityReason) -> MemoryError {
    MemoryError::RecallReplayIncompatible {
        group_id: group_id.to_string(),
        reason,
    }
}

/// SHA-256 query fingerprint for sampled impression sampling and cohorting.
///
/// This must never replace [`crate::db::query_hash`]: that 32-bit FNV value is
/// retained only for the legacy `access_history` / `query_diversity` contract.
pub(crate) fn query_fingerprint(query: &str) -> String {
    let digest = Sha256::digest(query.as_bytes());
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut fingerprint, "{byte:02x}").expect("writing to String cannot fail");
    }
    fingerprint
}

impl RecallImpressionPayload {
    pub(crate) fn scored_returned_count(&self) -> usize {
        self.rows.iter().filter(|row| row.scored_returned).count()
    }

    pub(crate) fn finalize_displayed(&mut self, displayed_ids: &[String]) {
        self.displayed_count = displayed_ids.len();
        let displayed = displayed_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        for row in &mut self.rows {
            row.scored_returned = displayed.contains(row.memory_id.as_str());
        }
    }
}

/// Deterministic SHA-256 query-identity sampling in basis points. Zero is a
/// strict off switch and avoids constructing a fingerprint on the hot path.
#[inline]
pub(crate) fn should_sample_query(query: &str, sample_rate_bps: u16) -> bool {
    if sample_rate_bps == 0 {
        return false;
    }
    let fingerprint = Sha256::digest(query.as_bytes());
    let bucket = u32::from_be_bytes([
        fingerprint[0],
        fingerprint[1],
        fingerprint[2],
        fingerprint[3],
    ]) % 10_000;
    bucket < u32::from(sample_rate_bps.min(10_000))
}

pub(crate) fn insert_recall_impression(
    tx: &Transaction<'_>,
    payload: &RecallImpressionPayload,
) -> Result<(), MemoryError> {
    tx.execute(
        IMPRESSION_GROUP_INSERT_SQL,
        params![
            payload.group_id,
            payload.created_at,
            payload.query_fingerprint,
            payload.replay_policy.fusion_policy_version,
            payload.replay_policy.pre_boost_adjustment_version,
            payload.replay_policy.tie_break_policy_version,
            payload.replay_policy.candidate_policy_version,
            payload.replay_policy.schema_identity,
            payload.weights_profile,
            payload.weights.semantic,
            payload.weights.fts,
            payload.weights.symbolic,
            payload.weights.decay,
            i64::from(payload.weights.use_rrf),
            payload.rrf_k,
            payload.top_k as i64,
            payload.rows.len() as i64,
            payload.displayed_count as i64,
            payload.scored_returned_count() as i64,
        ],
    )?;
    let mut stmt = tx.prepare_cached(IMPRESSION_ROW_INSERT_SQL)?;
    for row in &payload.rows {
        stmt.execute(params![
            payload.group_id,
            row.memory_id,
            row.vector_score,
            row.fts_score,
            row.symbolic_score,
            row.decay_score,
            row.vec_rank.map(|rank| rank as i64),
            row.fts_rank.map(|rank| rank as i64),
            row.sym_rank.map(|rank| rank as i64),
            row.merge_adjustment.as_str(),
            row.pre_boost_score,
            row.pre_boost_rank as i64,
            row.tie_break_epoch_millis,
            row.final_score,
            row.final_rank as i64,
            i64::from(row.scored),
            i64::from(row.scored_returned),
            row.access_count_at_recall,
        ])?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecallReplayCandidate {
    pub memory_id: String,
    pub recorded_pre_boost_score: f64,
    pub replayed_pre_boost_score: f64,
    pub bit_identical: bool,
    pub recorded_pre_boost_rank: usize,
    pub decay_zero_score: f64,
    pub decay_zero_rank: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecallReplayReport {
    pub group_id: String,
    pub candidate_count: usize,
    pub bit_identical_count: usize,
    pub decay_zero_rank_inversions: usize,
    pub post_boost_claimed: bool,
    pub candidates: Vec<RecallReplayCandidate>,
}

#[derive(Debug)]
struct StoredGroup {
    weights: HybridWeights,
    rrf_k: f64,
    policy: StoredReplayPolicy,
}

#[derive(Debug)]
struct StoredRow {
    memory_id: String,
    vector: f64,
    fts: f64,
    symbolic: f64,
    decay: f64,
    vec_rank: Option<usize>,
    fts_rank: Option<usize>,
    sym_rank: Option<usize>,
    adjustment: PreBoostAdjustment,
    recorded: f64,
    recorded_rank: usize,
    tie_break_epoch_millis: i64,
}

fn replay_score(group: &StoredGroup, row: &StoredRow, decay: f64) -> f64 {
    apply_pre_boost_adjustment(
        fuse_pre_boost_score(
            row.vector,
            row.fts,
            row.symbolic,
            decay,
            row.vec_rank,
            row.fts_rank,
            row.sym_rank,
            &group.weights,
            group.rrf_k,
        ),
        row.adjustment,
    )
}

/// Replay the stored fusion stage exactly and report only the decay=0 pre-boost counterfactual.
pub fn replay_recall_impression_group(
    conn: &Connection,
    group_id: &str,
) -> Result<RecallReplayReport, MemoryError> {
    let group = conn.query_row(
        "SELECT semantic_weight, fts_weight, symbolic_weight, decay_weight, use_rrf, rrf_k, query_fingerprint, fusion_policy_version, pre_boost_adjustment_version, tie_break_policy_version, candidate_policy_version, schema_identity FROM recall_impression_groups WHERE group_id = ?1",
        [group_id],
        |row| {
            Ok(StoredGroup {
                weights: HybridWeights {
                    semantic: row.get(0)?,
                    fts: row.get(1)?,
                    symbolic: row.get(2)?,
                    decay: row.get(3)?,
                    use_rrf: row.get::<_, i64>(4)? != 0,
                },
                rrf_k: row.get(5)?,
                policy: StoredReplayPolicy {
                    query_fingerprint: row.get(6)?,
                    fusion_policy_version: row.get(7)?,
                    pre_boost_adjustment_version: row.get(8)?,
                    tie_break_policy_version: row.get(9)?,
                    candidate_policy_version: row.get(10)?,
                    schema_identity: row.get(11)?,
                },
            })
        },
    )?;
    RecallReplayPolicy::validate_stored(group_id, &group.policy)?;
    let mut stmt = conn.prepare(
        "SELECT memory_id, vector_score, fts_score, symbolic_score, decay_score, vec_rank, fts_rank, sym_rank, merge_adjustment, pre_boost_score, pre_boost_rank, tie_break_epoch_millis FROM recall_impressions WHERE group_id = ?1 ORDER BY pre_boost_rank, memory_id",
    )?;
    let rows = stmt
        .query_map([group_id], |row| {
            Ok(StoredRow {
                memory_id: row.get(0)?,
                vector: row.get(1)?,
                fts: row.get(2)?,
                symbolic: row.get(3)?,
                decay: row.get(4)?,
                vec_rank: row.get::<_, Option<i64>>(5)?.map(|v| v as usize),
                fts_rank: row.get::<_, Option<i64>>(6)?.map(|v| v as usize),
                sym_rank: row.get::<_, Option<i64>>(7)?.map(|v| v as usize),
                adjustment: PreBoostAdjustment::parse(&row.get::<_, String>(8)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                recorded: row.get(9)?,
                recorded_rank: row.get::<_, i64>(10)? as usize,
                tie_break_epoch_millis: row.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut replayed = rows
        .iter()
        .map(|row| (row, replay_score(&group, row, 0.0)))
        .collect::<Vec<_>>();
    replayed.sort_by(|(a, a_score), (b, b_score)| {
        crate::scorer::cmp_recall_rank(
            (*a_score, a.tie_break_epoch_millis, &a.memory_id),
            (*b_score, b.tie_break_epoch_millis, &b.memory_id),
        )
    });
    let decay_zero_ranks = replayed
        .iter()
        .enumerate()
        .map(|(index, (row, _))| (row.memory_id.as_str(), index + 1))
        .collect::<std::collections::HashMap<_, _>>();

    let candidates = rows
        .iter()
        .map(|row| {
            let replayed_score = replay_score(&group, row, row.decay);
            RecallReplayCandidate {
                memory_id: row.memory_id.clone(),
                recorded_pre_boost_score: row.recorded,
                replayed_pre_boost_score: replayed_score,
                bit_identical: row.recorded.to_bits() == replayed_score.to_bits(),
                recorded_pre_boost_rank: row.recorded_rank,
                decay_zero_score: replay_score(&group, row, 0.0),
                decay_zero_rank: decay_zero_ranks[&row.memory_id.as_str()],
            }
        })
        .collect::<Vec<_>>();
    let pairwise_inversions = candidates
        .iter()
        .enumerate()
        .flat_map(|(left_index, left)| {
            candidates[left_index + 1..]
                .iter()
                .map(move |right| (left, right))
        })
        .filter(|(left, right)| {
            let recorded_order = left
                .recorded_pre_boost_rank
                .cmp(&right.recorded_pre_boost_rank);
            let replayed_order = left.decay_zero_rank.cmp(&right.decay_zero_rank);
            recorded_order != replayed_order
        })
        .count();
    Ok(RecallReplayReport {
        group_id: group_id.to_string(),
        candidate_count: candidates.len(),
        bit_identical_count: candidates.iter().filter(|row| row.bit_identical).count(),
        decay_zero_rank_inversions: pairwise_inversions,
        post_boost_claimed: false,
        candidates,
    })
}

/// Explicit writable bookkeeping; pure replay above remains usable on read-only connections.
pub fn increment_recall_impression_replay_count(
    conn: &Connection,
    group_id: &str,
) -> Result<usize, MemoryError> {
    Ok(conn.execute(
        "UPDATE recall_impression_groups SET replay_count = replay_count + 1 WHERE group_id = ?1",
        [group_id],
    )?)
}
