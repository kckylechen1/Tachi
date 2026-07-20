use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::MemoryError;

use super::common::now_utc_iso;

#[derive(Debug, Clone)]
pub struct StateRow {
    pub key: String,
    pub value_json: String,
    pub version: u32,
    pub updated_at: String,
}

// ─── Hard State Operations ─────────────────────────────────────────────────────

/// Set a key-value pair in the hard_state table. INSERT OR UPDATE with version bump.
pub fn set_state(
    conn: &Connection,
    namespace: &str,
    key: &str,
    value_json: &str,
) -> Result<u32, MemoryError> {
    let now = now_utc_iso();
    conn.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(namespace, key) DO UPDATE SET
           value_json = excluded.value_json,
           version = hard_state.version + 1,
           updated_at = excluded.updated_at",
        params![namespace, key, value_json, &now],
    )?;
    let version: u32 = conn.query_row(
        "SELECT version FROM hard_state WHERE namespace = ?1 AND key = ?2",
        params![namespace, key],
        |row| row.get(0),
    )?;
    Ok(version)
}

/// Insert a state row only when the key is currently absent.
pub fn insert_state_if_absent(
    conn: &Connection,
    namespace: &str,
    key: &str,
    value_json: &str,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let changed = conn.execute(
        "INSERT INTO hard_state (namespace, key, value_json, version, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?4)
         ON CONFLICT(namespace, key) DO NOTHING",
        params![namespace, key, value_json, &now],
    )?;
    Ok(changed == 1)
}

/// Update a state row only when its version matches the caller's snapshot.
pub fn set_state_if_version(
    conn: &Connection,
    namespace: &str,
    key: &str,
    value_json: &str,
    expected_version: u32,
) -> Result<bool, MemoryError> {
    let now = now_utc_iso();
    let changed = conn.execute(
        "UPDATE hard_state
         SET value_json = ?3,
             version = version + 1,
             updated_at = ?4
         WHERE namespace = ?1 AND key = ?2 AND version = ?5",
        params![namespace, key, value_json, &now, expected_version],
    )?;
    Ok(changed == 1)
}

/// Get a key-value pair from the hard_state table.
pub fn get_state(
    conn: &Connection,
    namespace: &str,
    key: &str,
) -> Result<Option<(String, u32)>, MemoryError> {
    let mut stmt = conn
        .prepare("SELECT value_json, version FROM hard_state WHERE namespace = ?1 AND key = ?2")?;
    let result = stmt.query_row(params![namespace, key], |row| {
        let val: String = row.get(0)?;
        let ver: u32 = row.get(1)?;
        Ok((val, ver))
    });
    match result {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(MemoryError::from(e)),
    }
}

/// Delete a single state row. Returns whether a row was actually removed.
pub fn delete_state(conn: &Connection, namespace: &str, key: &str) -> Result<bool, MemoryError> {
    let changed = conn.execute(
        "DELETE FROM hard_state WHERE namespace = ?1 AND key = ?2",
        params![namespace, key],
    )?;
    Ok(changed > 0)
}

/// Delete all `hard_state` rows whose JSON payload declares an `expires_at`
/// timestamp that has passed `now_rfc3339`.
///
/// `expires_at` is not a table column (see `schema/ddl.rs`'s `hard_state`
/// DDL) — callers embed it as a field inside `value_json` (e.g. the capture
/// manifest's staging TTL, `crates/tachi-server/.../capture_session.rs`), so
/// this reads it back via SQLite's JSON1 `json_extract`, already used
/// elsewhere in this DB layer (see `crates/memcore/src/db/tests.rs`'s
/// `json_extract must work` assertion). Rows with no `expires_at` field (or
/// an explicit JSON `null`) are left untouched forever — `json_extract`
/// returns SQL NULL for both, so the `IS NOT NULL` guard already excludes
/// them; this is the caller's deliberate retain-forever policy (#1301), not
/// an oversight.
pub fn reap_expired_state(conn: &Connection, now_rfc3339: &str) -> Result<usize, MemoryError> {
    let removed = conn.execute(
        "DELETE FROM hard_state
         WHERE json_extract(value_json, '$.expires_at') IS NOT NULL
           AND json_extract(value_json, '$.expires_at') < ?1",
        params![now_rfc3339],
    )?;
    Ok(removed)
}

/// List state rows in a namespace, newest first.
pub fn list_state(conn: &Connection, namespace: &str) -> Result<Vec<StateRow>, MemoryError> {
    let mut stmt = conn.prepare(
        "SELECT key, value_json, version, updated_at
         FROM hard_state
         WHERE namespace = ?1
         ORDER BY updated_at DESC, key ASC",
    )?;
    let rows = stmt
        .query_map(params![namespace], |row| {
            Ok(StateRow {
                key: row.get(0)?,
                value_json: row.get(1)?,
                version: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Save a derived item (causal extraction, distilled rule, etc.)
#[allow(clippy::too_many_arguments)]
pub fn save_derived(
    conn: &Connection,
    text: &str,
    path: &str,
    summary: &str,
    importance: f64,
    source: &str,
    scope: &str,
    metadata: &serde_json::Value,
) -> Result<String, MemoryError> {
    let id = Uuid::new_v4().to_string();
    save_derived_with_id(
        conn, &id, text, path, summary, importance, source, scope, metadata,
    )?;
    Ok(id)
}

/// Save a derived item under a caller-provided stable id.
#[allow(clippy::too_many_arguments)]
pub fn save_derived_with_id(
    conn: &Connection,
    id: &str,
    text: &str,
    path: &str,
    summary: &str,
    importance: f64,
    source: &str,
    scope: &str,
    metadata: &serde_json::Value,
) -> Result<(), MemoryError> {
    let now = now_utc_iso();
    let metadata_json = serde_json::to_string(metadata)?;

    conn.execute(
        r#"INSERT INTO derived_items (id, text, path, summary, importance, source, scope, metadata, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
           ON CONFLICT(id) DO UPDATE SET
             text = excluded.text,
             path = excluded.path,
             summary = excluded.summary,
             importance = excluded.importance,
             source = excluded.source,
             scope = excluded.scope,
             metadata = excluded.metadata,
             created_at = COALESCE(NULLIF(derived_items.created_at, ''), excluded.created_at)"#,
        params![
            id,
            text,
            path,
            summary,
            importance,
            source,
            scope,
            metadata_json,
            now
        ],
    )?;
    Ok(())
}

/// List derived items by source and path prefix.
pub fn list_derived_by_source(
    conn: &Connection,
    source: &str,
    path_prefix: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, MemoryError> {
    let like_pattern = format!("{}%", path_prefix);
    let mut stmt = conn.prepare(
        "SELECT id, text, path, summary, importance, source, scope, metadata, created_at
         FROM derived_items
         WHERE source = ?1 AND path LIKE ?2
         ORDER BY created_at DESC
         LIMIT ?3",
    )?;

    let rows = stmt.query_map(params![source, like_pattern, limit as i64], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, String>(0)?,
            "text": row.get::<_, String>(1)?,
            "path": row.get::<_, String>(2)?,
            "summary": row.get::<_, String>(3)?,
            "importance": row.get::<_, f64>(4)?,
            "source": row.get::<_, String>(5)?,
            "scope": row.get::<_, String>(6)?,
            "metadata": row.get::<_, String>(7)?,
            "created_at": row.get::<_, String>(8)?,
        }))
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_state_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open db");
        conn.execute_batch(
            r#"
            CREATE TABLE hard_state (
                namespace TEXT NOT NULL,
                key TEXT NOT NULL,
                value_json TEXT NOT NULL,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (namespace, key)
            );
            "#,
        )
        .expect("state schema");
        conn
    }

    #[test]
    fn conditional_state_update_rejects_stale_versions() {
        let conn = open_state_db();

        let version =
            set_state(&conn, "claim", "job", r#"{"status":"queued"}"#).expect("seed state");
        assert_eq!(version, 1);

        assert!(
            set_state_if_version(&conn, "claim", "job", r#"{"status":"running"}"#, version)
                .expect("fresh update")
        );
        assert!(
            !set_state_if_version(&conn, "claim", "job", r#"{"status":"running-2"}"#, version)
                .expect("stale update")
        );

        let (value, current_version) = get_state(&conn, "claim", "job")
            .expect("load state")
            .expect("state exists");
        assert_eq!(current_version, 2);
        assert_eq!(value, r#"{"status":"running"}"#);
    }

    #[test]
    fn insert_state_if_absent_is_single_winner() {
        let conn = open_state_db();

        assert!(
            insert_state_if_absent(&conn, "claim", "job", r#"{"status":"running"}"#)
                .expect("insert first")
        );
        assert!(
            !insert_state_if_absent(&conn, "claim", "job", r#"{"status":"running-2"}"#)
                .expect("insert second")
        );

        let (value, version) = get_state(&conn, "claim", "job")
            .expect("load state")
            .expect("state exists");
        assert_eq!(version, 1);
        assert_eq!(value, r#"{"status":"running"}"#);
    }

    #[test]
    fn delete_state_removes_row_and_reports_whether_present() {
        let conn = open_state_db();
        set_state(&conn, "ns", "k1", r#"{"a":1}"#).expect("seed");

        assert!(delete_state(&conn, "ns", "k1").expect("delete existing"));
        assert!(get_state(&conn, "ns", "k1")
            .expect("get after delete")
            .is_none());
        assert!(!delete_state(&conn, "ns", "k1").expect("delete missing is a no-op"));
    }

    #[test]
    fn reap_expired_state_removes_only_rows_past_their_expires_at() {
        let conn = open_state_db();
        set_state(
            &conn,
            "capture_manifest",
            "expired",
            r#"{"expires_at":"2020-01-01T00:00:00Z","completed":false}"#,
        )
        .expect("seed expired row");
        set_state(
            &conn,
            "capture_manifest",
            "future",
            r#"{"expires_at":"2999-01-01T00:00:00Z","completed":false}"#,
        )
        .expect("seed not-yet-expired row");
        set_state(
            &conn,
            "capture_manifest",
            "retain_forever",
            r#"{"expires_at":null,"completed":true}"#,
        )
        .expect("seed retain-forever row (completed manifest)");
        set_state(&conn, "claim", "no_expiry_field", r#"{"status":"queued"}"#)
            .expect("seed row with no expires_at field at all");

        let removed = reap_expired_state(&conn, "2026-01-01T00:00:00Z").expect("reap");

        assert_eq!(removed, 1, "only the past-expiry row should be deleted");
        assert!(get_state(&conn, "capture_manifest", "expired")
            .expect("get expired")
            .is_none());
        assert!(
            get_state(&conn, "capture_manifest", "future")
                .expect("get future")
                .is_some(),
            "not-yet-expired row must survive"
        );
        assert!(
            get_state(&conn, "capture_manifest", "retain_forever")
                .expect("get retain_forever")
                .is_some(),
            "explicit expires_at:null (retain-forever, #1301) must never be reaped"
        );
        assert!(
            get_state(&conn, "claim", "no_expiry_field")
                .expect("get no_expiry_field")
                .is_some(),
            "rows whose JSON has no expires_at key at all must never be reaped"
        );
    }

    #[test]
    fn reap_expired_state_is_a_noop_on_an_empty_table() {
        let conn = open_state_db();
        assert_eq!(
            reap_expired_state(&conn, "2026-01-01T00:00:00Z").expect("reap empty table"),
            0
        );
    }
}
