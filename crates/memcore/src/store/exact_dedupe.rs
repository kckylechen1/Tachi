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
pub const EXACT_DEDUPE_SCHEMA_VERSION: u32 = 1;

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
    pub fn plan_exact_dedupe(
        &self,
        target: String,
        limit: Option<usize>,
        prefix: Option<&str>,
    ) -> Result<ExactDedupePlan, MemoryError> {
        if limit == Some(0) {
            let mut plan = ExactDedupePlan {
                schema_version: EXACT_DEDUPE_SCHEMA_VERSION,
                policy_version: EXACT_DEDUPE_POLICY.into(),
                target_db_identity: target,
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
        let mut sql = format!("SELECT id,path,text,revision,retention_policy,tier,query_diversity,recall_count,access_count,metadata,{vec_expr},archived,superseded_by FROM memories m WHERE archived=0 AND superseded_by IS NULL");
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
        plan.validate()?;
        let effective: String = self.conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )?;
        if canonical(&effective)? != canonical(&plan.target_db_identity)? {
            return Err(MemoryError::InvalidArg(
                "exact-dedupe target DB mismatch".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let vec_expr = if self.vec_available {
            "EXISTS(SELECT 1 FROM memories_vec v WHERE v.id=m.id)"
        } else {
            "0"
        };
        let candidate_sql = format!("SELECT id,path,text,revision,retention_policy,tier,query_diversity,recall_count,access_count,metadata,{vec_expr},archived,superseded_by FROM memories m WHERE id=?1");
        let live_group_sql = format!("SELECT id,path,text,revision,retention_policy,tier,query_diversity,recall_count,access_count,metadata,{vec_expr},archived,superseded_by FROM memories m WHERE text=?1 AND archived=0 AND superseded_by IS NULL");
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
        // The complete text-digest revalidation above runs after BEGIN IMMEDIATE,
        // which excludes intervening writers until commit. The UPDATE therefore
        // needs only the revision/state/original-path CAS predicates.
        for g in &plan.groups {
            for f in &g.losers {
                let changed=tx.execute("UPDATE memories SET archived=1,superseded_by=?1,valid_until=COALESCE(valid_until,?2),updated_at=?2,revision=revision+1 WHERE id=?3 AND revision=?4 AND archived=0 AND superseded_by IS NULL AND path=?5",params![g.winner.id,now,f.id,f.revision,f.path])?;
                if changed != 1 {
                    return Err(MemoryError::InvalidArg(format!(
                        "exact-dedupe CAS failed: {}",
                        f.id
                    )));
                }
            }
        }
        tx.commit()?;
        Ok(ExactDedupeApplyResult {
            applied_groups: plan.groups.len(),
            applied_losers: plan.planned_losers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn insert(store: &MemoryStore, id: &str, path: &str, text: &str) {
        store.conn.execute(
            "INSERT INTO memories(id,path,text,timestamp,created_at,updated_at) VALUES(?1,?2,?3,'2026-01-01','2026-01-01','2026-01-01')",
            params![id,path,text],
        ).unwrap();
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
            store
                .conn
                .execute(
                    &format!("UPDATE memories SET {assignment} WHERE id=?1"),
                    [&preferred],
                )
                .unwrap();
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
            store.conn.execute("UPDATE memories SET revision=1,archived=0,superseded_by=NULL,path='/b',text='same-b' WHERE id='b2'", []).unwrap();
            store
                .conn
                .execute(
                    &format!("UPDATE memories SET {assignment} WHERE id='b2'"),
                    [],
                )
                .unwrap();
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
                store
                    .conn
                    .execute(
                        "UPDATE memories SET retention_policy=?1 WHERE id=?2",
                        params![policy, id],
                    )
                    .unwrap();
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
    fn successful_apply_soft_archives_and_followup_plan_is_empty() {
        let (_dir, identity, mut store) = disk_store();
        insert(&store, "winner", "/Wiki//child/", "same");
        insert(&store, "loser", "/wiki/child", "same");
        store
            .conn
            .execute(
                "UPDATE memories SET retention_policy='pinned' WHERE id='winner'",
                [],
            )
            .unwrap();
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
}
