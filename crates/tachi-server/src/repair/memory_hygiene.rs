//! R12 — logical memory hygiene backfill.
//!
//! This rule is intentionally opt-in. It repairs historical stores whose daily
//! distill runs wrote durable summaries but left the raw source rows active.

use super::{DbContext, Finding, RepairError, RepairRule, RuleReport};
use rusqlite::TransactionBehavior;

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

        let tx = ctx
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let all_ids = {
            let mut stmt = tx.prepare("SELECT id FROM memories ORDER BY id")?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids
        };
        let mut retired_sticky = Vec::new();
        for id in all_ids {
            match memcore::db::refuse_retired_sticky_row_within_tx(
                &tx,
                &id,
                "mutated or projected by memory hygiene",
            ) {
                Ok(()) => {}
                Err(memcore::MemoryError::InvalidArg(_)) => retired_sticky.push(id),
                Err(error) => return Err(error.into()),
            }
        }
        tx.execute_batch("CREATE TEMP TABLE r12_retired_sticky(id TEXT PRIMARY KEY);")?;
        for id in retired_sticky {
            tx.execute("INSERT INTO r12_retired_sticky(id) VALUES (?1)", [id])?;
        }
        // The batch below re-states COVERED_SAFE_RAW_SQL and
        // SAFE_DUPLICATE_RAW_SQL inline; both copies of the
        // `access_count`/`recall_count` guard carry the tachi#1459 caveat
        // documented on those constants — the counters observe the search path
        // only; reads through path-listing routes do not increment them.
        tx.execute_batch(
            r#"
            CREATE TEMP TABLE r12_missing_edges(
                distill_id TEXT NOT NULL,
                source_id TEXT NOT NULL,
                PRIMARY KEY(distill_id, source_id)
            );
            INSERT OR IGNORE INTO r12_missing_edges(distill_id, source_id)
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
            SELECT source_ids.distill_id, source_ids.source_id
            FROM source_ids
            JOIN memories s ON s.id=source_ids.source_id
            LEFT JOIN memory_edges e ON e.source_id=source_ids.distill_id
                                    AND e.target_id=source_ids.source_id
                                    AND e.relation='distilled_from'
            WHERE e.source_id IS NULL
              AND source_ids.distill_id NOT IN (SELECT id FROM r12_retired_sticky)
              AND source_ids.source_id NOT IN (SELECT id FROM r12_retired_sticky);

            CREATE TEMP TABLE r12_covered_raw(
                source_id TEXT PRIMARY KEY,
                distill_id TEXT NOT NULL
            );
            INSERT OR IGNORE INTO r12_covered_raw(source_id, distill_id)
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
            SELECT source_id, distill_id FROM ranked
            WHERE rn=1
              AND source_id NOT IN (SELECT id FROM r12_retired_sticky)
              AND distill_id NOT IN (SELECT id FROM r12_retired_sticky);

            CREATE TEMP TABLE r12_duplicate_raw(
                source_id TEXT PRIMARY KEY,
                keep_id TEXT NOT NULL
            );
            INSERT OR IGNORE INTO r12_duplicate_raw(source_id, keep_id)
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
                AND id NOT IN (SELECT id FROM r12_retired_sticky)
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
            SELECT id, keep_id
            FROM ranked
            WHERE group_count > 1
              AND rn > 1
              AND id != keep_id
              AND source!='foundry_distill'
              AND tier='raw'
              AND COALESCE(access_count,0)=0
              AND COALESCE(recall_count,0)=0
              AND COALESCE(retention_policy,'') NOT IN ('pinned','permanent');
            "#,
        )?;

        let promoted = tx.execute(
            "UPDATE memories
             SET tier='consolidated',
                 updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 revision=revision+1,
                 metadata=json_set(
                   CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                   '$.tier_repair','r12_memory_hygiene'
                 )
             WHERE COALESCE(archived,0)=0
               AND source='foundry_distill'
               AND tier='raw'
               AND id NOT IN (SELECT id FROM r12_retired_sticky)",
            [],
        )?;

        let derived_inserted = tx.execute(
            "INSERT OR IGNORE INTO derived_items
               (id, text, path, summary, importance, source, scope, metadata, created_at)
             SELECT 'derived:' || m.id,
                    m.text,
                    m.path,
                    m.summary,
                    m.importance,
                    m.source,
                    m.scope,
                    json_set(
                      CASE WHEN json_valid(m.metadata) THEN m.metadata ELSE '{}' END,
                      '$.legacy_repair','r12_memory_hygiene'
                    ),
                    strftime('%Y-%m-%dT%H:%M:%fZ','now')
             FROM memories m
             WHERE COALESCE(m.archived,0)=0
               AND m.source='foundry_distill'
               AND m.tier='consolidated'
               AND m.id NOT IN (SELECT id FROM r12_retired_sticky)",
            [],
        )?;

        let edges_inserted = tx.execute(
            "INSERT OR IGNORE INTO memory_edges
               (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
             SELECT distill_id,
                    source_id,
                    'distilled_from',
                    1.0,
                    json_object('source','r12_memory_hygiene'),
                    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                    NULL
             FROM r12_missing_edges",
            [],
        )?;

        tx.execute(
            "INSERT OR REPLACE INTO memory_edges
               (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
             SELECT distill_id,
                    source_id,
                    'supersedes',
                    1.0,
                    json_object(
                      'source','r12_memory_hygiene',
                      'reason','distill_source_archived'
                    ),
                    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                    NULL
             FROM r12_covered_raw",
            [],
        )?;

        let covered_archived = tx.execute(
            "UPDATE memories
             SET archived=1,
                 superseded_by=(SELECT distill_id FROM r12_covered_raw t WHERE t.source_id=memories.id),
                 updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 revision=revision+1,
                 valid_until=COALESCE(valid_until, strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                 metadata=json_set(
                   CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                   '$.repair_rule','R12',
                   '$.archive_reason','distill_source_superseded'
                 )
             WHERE id IN (SELECT source_id FROM r12_covered_raw)
               AND COALESCE(archived,0)=0",
            [],
        )?;

        tx.execute(
            "INSERT OR REPLACE INTO memory_edges
               (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
             SELECT keep_id,
                    source_id,
                    'supersedes',
                    1.0,
                    json_object(
                      'source','r12_memory_hygiene',
                      'reason','duplicate_raw_archived'
                    ),
                    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                    strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                    NULL
             FROM r12_duplicate_raw",
            [],
        )?;

        let duplicates_archived = tx.execute(
            "UPDATE memories
             SET archived=1,
                 superseded_by=(SELECT keep_id FROM r12_duplicate_raw t WHERE t.source_id=memories.id),
                 updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 revision=revision+1,
                 valid_until=COALESCE(valid_until, strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                 metadata=json_set(
                   CASE WHEN json_valid(metadata) THEN metadata ELSE '{}' END,
                   '$.repair_rule','R12',
                   '$.archive_reason','duplicate_raw_fold'
                 )
             WHERE id IN (SELECT source_id FROM r12_duplicate_raw)
               AND COALESCE(archived,0)=0",
            [],
        )?;

        tx.execute_batch(
            "DROP TABLE r12_missing_edges;
             DROP TABLE r12_covered_raw;
             DROP TABLE r12_duplicate_raw;
             DROP TABLE r12_retired_sticky;",
        )?;
        tx.commit()?;

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
