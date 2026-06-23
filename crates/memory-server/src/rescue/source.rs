use super::types::SourceRow;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub(super) fn read_source_rows(source: &Path) -> Result<Vec<SourceRow>, String> {
    let conn = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open source DB: {e}"))?;
    let persons_expr = if conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='persons' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false)
    {
        "persons"
    } else {
        "'[]' AS persons"
    };
    let location_expr = if conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('memories') WHERE name='location' LIMIT 1",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false)
    {
        "location"
    } else {
        "'' AS location"
    };

    let sql = format!(
        "SELECT id, path, summary, text, importance, timestamp, category, topic,
                keywords, {persons_expr}, entities, {location_expr}, source, scope, archived,
                created_at, updated_at, access_count, last_access, metadata, revision
         FROM memories
         WHERE archived = 0"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("prepare source select: {e}"))?;

    let rows = stmt
        .query_map([], |r| {
            Ok(SourceRow {
                id: r.get(0)?,
                path: r.get(1)?,
                summary: r.get(2)?,
                text: r.get(3)?,
                importance: r.get(4)?,
                timestamp: r.get(5)?,
                category: r.get(6)?,
                topic: r.get(7)?,
                keywords: r.get(8)?,
                persons: r.get(9)?,
                entities: r.get(10)?,
                location: r.get(11)?,
                source: r.get(12)?,
                scope: r.get(13)?,
                archived: r.get(14)?,
                created_at: r.get(15)?,
                updated_at: r.get(16)?,
                access_count: r.get(17)?,
                last_access: r.get(18)?,
                metadata: r.get(19)?,
                revision: r.get(20)?,
            })
        })
        .map_err(|e| format!("query source rows: {e}"))?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("decode source row: {e}"))?);
    }
    Ok(out)
}
