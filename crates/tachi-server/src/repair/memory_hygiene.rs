//! R12 — logical memory hygiene backfill.
//!
//! This rule is intentionally opt-in. It repairs historical stores whose daily
//! distill runs wrote durable summaries but left the raw source rows active.

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};

pub struct MemoryHygiene;

const RULE_ID: &str = "R12";
const RULE_NAME: &str = "Memory hygiene";

fn has_table(ctx: &DbContext, name: &str) -> bool {
    ctx.conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
}

fn count_query(ctx: &DbContext, sql: &str) -> Result<usize, RepairError> {
    let count_sql = format!("SELECT COUNT(*) FROM ({sql})");
    Ok(ctx
        .conn
        .query_row(&count_sql, [], |row| row.get::<_, i64>(0))? as usize)
}

fn push_count(report: &mut RuleReport, kind: &str, count: usize) {
    if count > 0 {
        report.findings.push(Finding::new(kind, count));
    }
}

fn apply_r12_supersession(
    replacement: &mut memcore::store::immutable_supersession::ImmutableSupersessionTransaction<'_>,
    source_id: &str,
    target_id: &str,
    archive_reason: &str,
) -> Result<bool, memcore::MemoryError> {
    let source = replacement
        .get_memory(source_id)?
        .ok_or_else(|| memcore::MemoryError::NotFound(source_id.to_string()))?;
    let target = replacement
        .get_memory(target_id)?
        .ok_or_else(|| memcore::MemoryError::NotFound(target_id.to_string()))?;
    let expected = memcore::SupersessionExpectedState::active_unsuperseded(&source, Some(&target));
    let outcome = replacement.claim_and_archive_immutable_supersession_for_r12(
        source_id,
        target_id,
        Some(&expected),
    )?;
    if !outcome.is_applied() {
        return Ok(false);
    }
    let now = memcore::now_utc_iso();
    replacement.add_canonical_supersession_edge(
        &memcore::MemoryEdge {
            source_id: target_id.to_string(),
            target_id: source_id.to_string(),
            relation: "supersedes".to_string(),
            weight: 1.0,
            metadata: serde_json::json!({
                "source": "r12_memory_hygiene",
                "reason": archive_reason,
            }),
            created_at: now.clone(),
            valid_from: now,
            valid_to: None,
        },
        &memcore::db::EdgeProvenance {
            authority: Some(memcore::db::EdgeAuthority::StructuralBookkeeping),
            ..Default::default()
        },
    )?;
    replacement.update_claimed_source_metadata(
        source_id,
        &serde_json::json!({
            "repair_rule": "R12",
            "archive_reason": archive_reason,
        }),
    )?;
    Ok(true)
}

const LEGACY_DISTILL_RAW_SQL: &str = r#"
    SELECT id
    FROM memories
    WHERE COALESCE(archived,0)=0
      AND source='foundry_distill'
      AND tier='raw'
"#;

const MISSING_DERIVED_SQL: &str = r#"
    SELECT m.id
    FROM memories m
    LEFT JOIN derived_items d ON d.id='derived:' || m.id
    WHERE COALESCE(m.archived,0)=0
      AND m.source='foundry_distill'
      AND m.tier IN ('raw', 'consolidated')
      AND d.id IS NULL
"#;

const MISSING_DISTILLED_FROM_EDGE_SQL: &str = r#"
    WITH source_ids AS (
      SELECT d.id AS distill_id, json_each.value AS source_id
      FROM memories d,
           json_each(
             CASE
               WHEN json_valid(d.metadata)
                AND json_type(d.metadata, '$.source_memory_ids')='array'
               THEN json_extract(d.metadata, '$.source_memory_ids')
               ELSE '[]'
             END
           )
      WHERE COALESCE(d.archived,0)=0
        AND d.source='foundry_distill'
    )
    SELECT source_ids.distill_id || '->' || source_ids.source_id
    FROM source_ids
    JOIN memories s ON s.id=source_ids.source_id
    LEFT JOIN memory_edges e ON e.source_id=source_ids.distill_id
                            AND e.target_id=source_ids.source_id
                            AND e.relation='distilled_from'
    WHERE e.source_id IS NULL
"#;

// tachi#1459: the `access_count=0 AND recall_count=0` guard in this query
// observes the search path only; reads through path-listing routes do not
// increment those columns. A non-zero count is sound evidence to spare a source
// row, but zero does not establish that nothing read it — a row served only by
// `list_by_path` / `list_by_path_recent` / `list_memories_by_path_prefix` reads
// zero here however heavily it is used. The `pinned`/`permanent` and importance
// filters, not this guard, are what keep the live path-listed namespaces out.
const COVERED_SAFE_RAW_SQL: &str = r#"
    WITH coverage AS (
      SELECT d.id AS distill_id, e.target_id AS source_id, d.timestamp AS distill_ts
      FROM memory_edges e
      JOIN memories d ON d.id=e.source_id
      WHERE e.relation='distilled_from'
        AND COALESCE(d.archived,0)=0
        AND d.source='foundry_distill'
      UNION
      SELECT d.id AS distill_id, json_each.value AS source_id, d.timestamp AS distill_ts
      FROM memories d,
           json_each(
             CASE
               WHEN json_valid(d.metadata)
                AND json_type(d.metadata, '$.source_memory_ids')='array'
               THEN json_extract(d.metadata, '$.source_memory_ids')
               ELSE '[]'
             END
           )
      WHERE COALESCE(d.archived,0)=0
        AND d.source='foundry_distill'
    ),
    ranked AS (
      SELECT s.id AS source_id,
             c.distill_id,
             ROW_NUMBER() OVER (
               PARTITION BY s.id
               ORDER BY c.distill_ts DESC, c.distill_id DESC
             ) AS rn
      FROM coverage c
      JOIN memories s ON s.id=c.source_id
      WHERE COALESCE(s.archived,0)=0
        AND s.source!='foundry_distill'
        AND s.tier='raw'
        AND COALESCE(s.access_count,0)=0
        AND COALESCE(s.recall_count,0)=0
        AND COALESCE(s.retention_policy,'') NOT IN ('pinned','permanent')
        AND COALESCE(s.importance,0)<0.85
    )
    SELECT source_id FROM ranked WHERE rn=1
"#;

// tachi#1459: `access_count` / `recall_count` appear below only as survivor
// tiebreakers among rows already established to be duplicates, ordered after
// the retention-policy rank. They observe the search path only; reads through
// path-listing routes do not increment them, so between two identical rows this
// keeps whichever search has shown more often, which is not the same as
// whichever has been read more often.
const SAFE_DUPLICATE_RAW_SQL: &str = r#"
    WITH normalized AS (
      SELECT id, path, summary, source, category, retention_policy, access_count,
             recall_count, importance, timestamp, tier,
             CASE
               WHEN path LIKE '/trading/equity/daily_review/%'
                 AND instr(text,'{') > 0
                 AND json_valid(substr(text, instr(text,'{')))
               THEN json_remove(
                 substr(text, instr(text,'{')),
                 '$.summary.created_at',
                 '$.created_at',
                 '$.timestamp',
                 '$.updated_at'
               )
               ELSE text
             END AS normalized_text
      FROM memories
      WHERE COALESCE(archived,0)=0
    ),
    ranked AS (
      SELECT *,
             COUNT(*) OVER (
               PARTITION BY path, summary, normalized_text, source, category
             ) AS group_count,
             FIRST_VALUE(id) OVER (
               PARTITION BY path, summary, normalized_text, source, category
               ORDER BY
                 CASE WHEN retention_policy IN ('pinned','permanent') THEN 1 ELSE 0 END DESC,
                 COALESCE(access_count,0) DESC,
                 COALESCE(recall_count,0) DESC,
                 COALESCE(importance,0) DESC,
                 timestamp DESC,
                 id DESC
             ) AS keep_id,
             ROW_NUMBER() OVER (
               PARTITION BY path, summary, normalized_text, source, category
               ORDER BY
                 CASE WHEN retention_policy IN ('pinned','permanent') THEN 1 ELSE 0 END DESC,
                 COALESCE(access_count,0) DESC,
                 COALESCE(recall_count,0) DESC,
                 COALESCE(importance,0) DESC,
                 timestamp DESC,
                 id DESC
             ) AS rn
      FROM normalized
    )
    SELECT id
    FROM ranked
    WHERE group_count > 1
      AND rn > 1
      AND id != keep_id
      AND source!='foundry_distill'
      AND tier='raw'
      AND COALESCE(access_count,0)=0
      AND COALESCE(recall_count,0)=0
      AND COALESCE(retention_policy,'') NOT IN ('pinned','permanent')
"#;

impl RepairRule for MemoryHygiene {
    fn id(&self) -> &'static str {
        RULE_ID
    }

    fn name(&self) -> &'static str {
        RULE_NAME
    }

    fn dry_run(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        if !has_table(ctx, "derived_items") || !has_table(ctx, "memory_edges") {
            return Ok(report);
        }

        push_count(
            &mut report,
            "legacy_foundry_distill_raw",
            count_query(ctx, LEGACY_DISTILL_RAW_SQL)?,
        );
        push_count(
            &mut report,
            "missing_distill_derived_items",
            count_query(ctx, MISSING_DERIVED_SQL)?,
        );
        push_count(
            &mut report,
            "missing_distilled_from_edges",
            count_query(ctx, MISSING_DISTILLED_FROM_EDGE_SQL)?,
        );
        push_count(
            &mut report,
            "covered_safe_raw_sources",
            count_query(ctx, COVERED_SAFE_RAW_SQL)?,
        );
        push_count(
            &mut report,
            "safe_duplicate_raw_sources",
            count_query(ctx, SAFE_DUPLICATE_RAW_SQL)?,
        );
        Ok(report)
    }

    fn apply(&self, ctx: &mut DbContext) -> Result<RuleReport, RepairError> {
        let mut report = RuleReport::new(self.id(), self.name(), ctx.label.clone());
        if !has_table(ctx, "derived_items") || !has_table(ctx, "memory_edges") {
            return Ok(report);
        }

        let db_path = ctx.path.to_string_lossy().into_owned();
        let mut semantic_store = memcore::MemoryStore::open_existing_read_write(&db_path)?;
        let (promoted, derived_inserted, edges_inserted, covered_archived, duplicates_archived) =
            semantic_store.with_immutable_supersession_transaction(|replacement| {
                let preparation = replacement.prepare_r12_memory_hygiene()?;
                let mut covered_archived = 0usize;
                for (source_id, target_id) in &preparation.covered_pairs {
                    if apply_r12_supersession(
                        replacement,
                        source_id,
                        target_id,
                        "distill_source_superseded",
                    )? {
                        covered_archived += 1;
                    }
                }
                let mut duplicates_archived = 0usize;
                for (source_id, target_id) in &preparation.duplicate_pairs {
                    if apply_r12_supersession(
                        replacement,
                        source_id,
                        target_id,
                        "duplicate_raw_fold",
                    )? {
                        duplicates_archived += 1;
                    }
                }
                Ok((
                    preparation.promoted,
                    preparation.derived_inserted,
                    preparation.edges_inserted,
                    covered_archived,
                    duplicates_archived,
                ))
            })?;

        push_count(&mut report, "legacy_foundry_distill_raw", promoted);
        push_count(
            &mut report,
            "missing_distill_derived_items",
            derived_inserted,
        );
        push_count(&mut report, "missing_distilled_from_edges", edges_inserted);
        push_count(&mut report, "covered_safe_raw_sources", covered_archived);
        push_count(
            &mut report,
            "safe_duplicate_raw_sources",
            duplicates_archived,
        );
        report.applied =
            promoted + derived_inserted + edges_inserted + covered_archived + duplicates_archived;
        Ok(report)
    }
}
