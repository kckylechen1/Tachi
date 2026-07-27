use rusqlite::{params, Connection};

use crate::error::MemoryError;

use super::AccessEventKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalEvidenceRow {
    pub id: String,
    pub path: String,
    pub summary: String,
    pub text: String,
    pub metadata: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CategorySourceGroup {
    pub count: i64,
    pub category: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DuplicateSummaryRow {
    pub summary: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyHealthDbSnapshot {
    pub total_entries: i64,
    pub new_today: i64,
    pub stale_days: i64,
    pub groups: Vec<CategorySourceGroup>,
    pub duplicate_summaries: Vec<DuplicateSummaryRow>,
}

pub fn list_eval_evidence(
    conn: &Connection,
    days: i64,
    limit: usize,
    exclude_auto_synthesized: bool,
) -> Result<Vec<EvalEvidenceRow>, MemoryError> {
    let auto_synth_filter = if exclude_auto_synthesized {
        "AND (
               json_extract(metadata, '$.auto_synthesized') IS NULL
               OR json_extract(metadata, '$.auto_synthesized') = 0
           )"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, path, summary, text, metadata, created_at
         FROM memories
         WHERE path LIKE '/eval/%'
           AND created_at > datetime('now', '-{days} day')
           {auto_synth_filter}
         ORDER BY created_at DESC
         LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        let metadata_raw: String = row.get(4)?;
        Ok(EvalEvidenceRow {
            id: row.get(0)?,
            path: row.get(1)?,
            summary: row.get(2)?,
            text: row.get(3)?,
            metadata: serde_json::from_str(&metadata_raw).unwrap_or_else(|_| serde_json::json!({})),
            created_at: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn collect_daily_health_snapshot(
    conn: &Connection,
) -> Result<DailyHealthDbSnapshot, MemoryError> {
    let total_entries: i64 =
        conn.query_row("SELECT count(*) FROM memories", [], |row| row.get(0))?;
    let new_today: i64 = conn.query_row(
        "SELECT count(*) FROM memories WHERE created_at > datetime('now', '-1 day')",
        [],
        |row| row.get(0),
    )?;
    let stale_days: i64 = conn
        .query_row(
            "SELECT COALESCE(CAST(julianday('now') - julianday(MAX(NULLIF(created_at, ''))) AS INTEGER), 0) FROM memories",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let mut group_stmt =
        conn.prepare("SELECT count(*), category, source FROM memories GROUP BY category, source")?;
    let group_rows = group_stmt.query_map([], |row| {
        Ok(CategorySourceGroup {
            count: row.get(0)?,
            category: row.get(1)?,
            source: row.get(2)?,
        })
    })?;
    let mut groups = Vec::new();
    for row in group_rows {
        groups.push(row?);
    }

    let mut dup_stmt = conn.prepare(
        "SELECT summary, count(*) FROM memories
         WHERE trim(summary) <> ''
         GROUP BY summary HAVING count(*) > 1
         ORDER BY count(*) DESC LIMIT 20",
    )?;
    let dup_rows = dup_stmt.query_map([], |row| {
        Ok(DuplicateSummaryRow {
            summary: row.get(0)?,
            count: row.get(1)?,
        })
    })?;
    let mut duplicate_summaries = Vec::new();
    for row in dup_rows {
        duplicate_summaries.push(row?);
    }

    Ok(DailyHealthDbSnapshot {
        total_entries,
        new_today,
        stale_days,
        groups,
        duplicate_summaries,
    })
}

pub fn count_active_memories(conn: &Connection) -> Result<i64, MemoryError> {
    conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE archived = 0",
        [],
        |row| row.get(0),
    )
    .map_err(MemoryError::from)
}

pub fn count_consolidated_active_memories(conn: &Connection) -> Result<i64, MemoryError> {
    conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE archived = 0 AND tier IN ('consolidated','pattern')",
        [],
        |row| row.get(0),
    )
    .map_err(MemoryError::from)
}

pub fn list_memory_ids_needing_embedding(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<String>, MemoryError> {
    let tier_filter = crate::embed_config::embed_raw_tier_sql_filter("m.");
    let order_by = crate::embed_config::embed_selection_order_by("m.");
    let sql = format!(
        "SELECT m.id FROM memories m
         LEFT JOIN memories_vec v ON m.id = v.id
         WHERE m.archived = 0
           {tier_filter}
           AND m.id NOT LIKE 'anchor:%'
           AND v.id IS NULL
         {order_by}
         LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit as i64], |row| row.get(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn list_promotion_candidate_ids(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<String>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT id FROM memories
         WHERE archived = 0
           AND COALESCE(retention_policy, '') NOT IN ('permanent', 'pinned')
         ORDER BY access_count DESC, timestamp DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| row.get(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Distinct calendar days on which a memory recorded an access event of
/// `kind` — the frequency input to `calculate_promotion_score`, i.e. tachi#1446
/// **lever 6**.
///
/// `kind` is mandatory and there is no unfiltered variant, for the reason
/// spelled out on `AccessEventKind::sql_predicate`: an unfiltered count is
/// exactly the defect. Pick the arm with
/// [`AccessEventKind::for_promotion`], never by hand.
///
/// * [`AccessEventKind::Display`] — the knob-OFF arm. Returns the same number
///   the pre-#1446 unfiltered query returned for any history that contains no
///   `use` rows, because `event_kind` is `NOT NULL DEFAULT 'display'`
///   (`db/schema.rs`'s `ensure_column`, `db/schema/ddl.rs`'s
///   `CREATE TABLE access_history`), so every legacy row and every row written
///   by `record_access_with_updates` carries `display`.
/// * [`AccessEventKind::Use`] — the knob-ON arm. Counts only days on which a
///   caller-initiated save cited this memory, so being shown by the recall
///   pipeline, however often, contributes nothing to the promotion score.
///
/// The `Display` arm is deliberately **filtered rather than left unfiltered**.
/// `record_memory_use` writes `use` rows unconditionally — the knob governs
/// reads, not that write — so an unfiltered count lets the new use signal add
/// promotion days at default config, i.e. lets it drive an irreversible
/// promotion through the very channel #1446 is repairing. Filtering restores
/// the pre-#1446 number instead of preserving that leak.
pub fn count_distinct_access_days(
    conn: &Connection,
    memory_id: &str,
    kind: AccessEventKind,
) -> Result<usize, MemoryError> {
    let kind_predicate = kind.sql_predicate();
    let sql = format!(
        "SELECT COUNT(DISTINCT date(accessed_at)) FROM access_history \
         WHERE memory_id = ?1{kind_predicate}"
    );
    let count: i64 = conn.query_row(&sql, params![memory_id], |row| row.get(0))?;
    Ok(count as usize)
}

pub fn promote_memory_to_durable(conn: &Connection, memory_id: &str) -> Result<(), MemoryError> {
    conn.execute(
        "UPDATE memories
         SET importance = 0.7, retention_policy = 'durable', updated_at = datetime('now')
         WHERE id = ?1",
        params![memory_id],
    )?;
    Ok(())
}
