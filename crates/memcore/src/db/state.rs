use rusqlite::{params, Connection, ToSql};
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
/// timestamp that has passed `now_rfc3339`. **Destructive and generic**: it
/// runs against every namespace, so its `expires_at` contract binds every
/// present and future caller that writes into `hard_state`, not just capture
/// manifests. Any change here must be re-verified against every namespace
/// with an `expires_at` field, not just the one that happened to motivate it.
///
/// `expires_at` is not a table column (see `schema/ddl.rs`'s `hard_state`
/// DDL) — callers embed it as a field inside `value_json` (e.g. the capture
/// manifest's staging TTL, `crates/tachi-server/.../capture_session.rs`), so
/// this reads it back via SQLite's JSON1 `json_extract`, already used
/// elsewhere in this DB layer (see `crates/memcore/src/db/tests.rs`'s
/// `json_extract must work` assertion).
///
/// The comparison is semantic (`datetime(...)`), not lexical string
/// comparison — an earlier version compared the raw JSON strings directly,
/// which is unsound because callers don't agree on RFC3339 rendering: this
/// module's own `now_utc_iso()` renders `...123Z` (`to_rfc3339_opts`,
/// `SecondsFormat::Millis`, `use_z=true`) while capture manifests render
/// `...123456+00:00` (plain `to_rfc3339()`, microsecond precision, numeric
/// offset). `'+'` (0x2B) sorts before `'Z'` (0x5A) lexically, so a
/// *not-yet-expired* `+00:00`-rendered row can string-compare as "less than"
/// a `Z`-rendered `now`, deleting live rows. `datetime(...)` parses either
/// rendering, any offset, and any sub-second precision, and normalizes to
/// UTC at second granularity (plenty for a multi-day TTL) before comparing —
/// so rendering differences between callers can no longer flip the verdict.
///
/// Four **fail-closed** exclusions, each independently sufficient to retain
/// a row (only a row that fails ALL three is a reap candidate):
/// - `json_valid(value_json)` false — malformed JSON is retained so one
///   corrupt legacy row cannot abort maintenance for every valid row.
/// - `json_type(...) != 'text'` — a non-string `expires_at` (JSON `null`,
///   `number`, `bool`, array, object) is retained. This is also how a
///   *missing* `expires_at` key is excluded: `json_type` on an absent path
///   returns SQL NULL, which is `!= 'text'`. This preserves the #1301
///   retain-forever policy for completed manifests (`expires_at: None`
///   serializes to JSON `null`) as a single case of the same rule, not a
///   special-cased `IS NOT NULL` guard on the raw extract.
/// - `datetime(...) IS NULL` — a string that isn't a datetime SQLite can
///   parse (garbage, a non-timestamp string) is retained rather than risk
///   an unpredictable comparison result.
/// - `datetime(...) < datetime(?1)` false — an unexpired (or exactly-now)
///   timestamp is retained.
pub fn reap_expired_state(conn: &Connection, now_rfc3339: &str) -> Result<usize, MemoryError> {
    let removed = conn.execute(
        "DELETE FROM hard_state
         WHERE json_valid(value_json)
           AND json_type(value_json, '$.expires_at') = 'text'
           AND datetime(json_extract(value_json, '$.expires_at')) IS NOT NULL
           AND datetime(json_extract(value_json, '$.expires_at')) < datetime(?1)",
        params![now_rfc3339],
    )?;
    Ok(removed)
}

/// The exact set of JSON pointers [`backfill_missing_expires_at`]'s
/// `terminal_status` may scope on. Every current call site passes a
/// hardcoded literal (`"$.status"` or `"$.cleanup_status"`, never
/// request/user input) — but the function signature itself (`&str`) does not
/// enforce that, and `status_path` is interpolated directly into SQL text
/// (SQLite has no bind-parameter form for a JSON path segment; see that
/// function's own doc). An allow-list closes that gap structurally: a path
/// not on this list is refused with an `Err`, not trusted and interpolated.
/// Extend this list (after verifying the new caller) rather than widening the
/// check.
const ALLOWED_TERMINAL_STATUS_PATHS: &[&str] = &["$.status", "$.cleanup_status"];

/// Idempotent TTL backfill for `hard_state` rows written before their
/// namespace carried an `expires_at` field at all (the seven-namespace
/// state-lifecycle-hygiene pass this reap function's own header warns every
/// future `hard_state` writer to check against). Only rows with valid JSON
/// whose document has **no `expires_at` key at all** are touched; malformed
/// legacy rows remain untouched so they cannot abort the whole backfill.
///
/// `json_extract(value_json, '$.expires_at') IS NULL` would ALSO match a row
/// that explicitly carries `"expires_at": null` — the #1301 retain-forever
/// marker [`reap_expired_state`] treats as a first-class, permanent
/// exemption — and clobbering that with a TTL would silently undo it. This
/// uses `json_type(...) IS NULL` instead, which is what SQLite returns for a
/// JSON path that does not exist at all (see `reap_expired_state`'s own doc
/// comment on the identical distinction), so it is the correct "the key is
/// missing" test rather than "the value is missing".
///
/// `terminal_status` optionally scopes the backfill to rows whose value at
/// the given JSON pointer (e.g. `"$.status"`) currently equals one of
/// `terminal_values` — e.g. only `applied`/`rejected` proposal rows, never
/// `pending`/`approved` ones, which must stay TTL-less until they reach a
/// terminal write of their own. `None` backfills every row in the namespace
/// unconditionally (for namespaces with no "still open" concept at all,
/// e.g. a build receipt, which is done being useful the instant it is
/// written).
///
/// Safe to re-run against an already-backfilled namespace: the
/// `json_type(...) IS NULL` guard means a row that already carries
/// `expires_at` (from an earlier backfill run, or because it was written
/// with one from the start) is never touched twice, so the returned count is
/// `0` on a repeat run against unchanged data.
///
/// `terminal_status`'s JSON-pointer half is checked against
/// [`ALLOWED_TERMINAL_STATUS_PATHS`] before use — see that const's own doc
/// for why.
pub fn backfill_missing_expires_at(
    conn: &Connection,
    namespace: &str,
    ttl_rfc3339: &str,
    terminal_status: Option<(&str, &[&str])>,
) -> Result<usize, MemoryError> {
    match terminal_status {
        None => {
            let updated = conn.execute(
                "UPDATE hard_state
                 SET value_json = json_set(value_json, '$.expires_at', ?1)
                 WHERE namespace = ?2
                   AND json_valid(value_json)
                   AND json_type(value_json, '$.expires_at') IS NULL",
                params![ttl_rfc3339, namespace],
            )?;
            Ok(updated)
        }
        Some((status_path, terminal_values)) => {
            if !ALLOWED_TERMINAL_STATUS_PATHS.contains(&status_path) {
                return Err(MemoryError::InvalidArg(format!(
                    "backfill_missing_expires_at: status_path '{status_path}' is not on the \
                     allowed list {ALLOWED_TERMINAL_STATUS_PATHS:?} — this function \
                     interpolates status_path directly into SQL text (no bind-parameter form \
                     exists for a JSON path segment), so an unlisted path is refused rather \
                     than trusted. Add it to ALLOWED_TERMINAL_STATUS_PATHS after verifying the \
                     new call site."
                )));
            }
            if terminal_values.is_empty() {
                // Nothing can ever match an empty allow-list; skip the round
                // trip rather than hand SQLite a malformed empty `IN ()`.
                return Ok(0);
            }
            let placeholders = (0..terminal_values.len())
                .map(|i| format!("?{}", i + 3))
                .collect::<Vec<_>>()
                .join(", ");
            // `status_path` is now verified against `ALLOWED_TERMINAL_STATUS_PATHS`
            // above, so interpolating it into the query text here is safe —
            // it is one of a fixed, reviewed set, never arbitrary caller input.
            let sql = format!(
                "UPDATE hard_state
                 SET value_json = json_set(value_json, '$.expires_at', ?1)
                 WHERE namespace = ?2
                   AND json_valid(value_json)
                   AND json_type(value_json, '$.expires_at') IS NULL
                   AND json_extract(value_json, '{status_path}') IN ({placeholders})"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut bind_params: Vec<&dyn ToSql> =
                vec![&ttl_rfc3339 as &dyn ToSql, &namespace as &dyn ToSql];
            for value in terminal_values {
                bind_params.push(value as &dyn ToSql);
            }
            let updated = stmt.execute(bind_params.as_slice())?;
            Ok(updated)
        }
    }
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
    fn reap_expired_state_skips_malformed_json_and_removes_valid_expired_rows() {
        let conn = open_state_db();
        set_state(&conn, "capture_manifest", "corrupt", "not valid json")
            .expect("seed corrupt row");
        set_state(
            &conn,
            "capture_manifest",
            "expired",
            r#"{"expires_at":"2020-01-01T00:00:00Z"}"#,
        )
        .expect("seed expired row");

        let removed = reap_expired_state(&conn, "2026-01-01T00:00:00Z")
            .expect("malformed state must not abort reaping valid rows");

        assert_eq!(removed, 1, "the valid expired row must still be reaped");
        assert!(
            get_state(&conn, "capture_manifest", "corrupt")
                .expect("read corrupt row")
                .is_some(),
            "malformed JSON must be retained fail-closed"
        );
        assert!(
            get_state(&conn, "capture_manifest", "expired")
                .expect("read expired row")
                .is_none(),
            "valid expired JSON must still make maintenance progress"
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

    #[test]
    fn reap_expired_state_uses_semantic_not_lexical_comparison_across_real_renderings() {
        // Production combination: capture manifests write `expires_at` via
        // `to_rfc3339()` (`capture_session.rs`), which renders a numeric
        // offset (`+00:00`) at microsecond precision. The daemon maintenance
        // loop's `now` (`background.rs`) comes from
        // `to_rfc3339_opts(SecondsFormat::Millis, true)`, which renders `Z`
        // at millisecond precision. A raw string comparison is unsound here
        // — `'+'` (0x2B) sorts before `'Z'` (0x5A) — so a row at the SAME
        // wall-clock second as `now` (not actually past it) can
        // string-compare as "less than" `now` purely from the rendering
        // difference. This reproduces the exact counterexample from review:
        // `'...12:00:00.100999+00:00' < '...12:00:00.100Z'` is `true` under
        // naive string comparison (verified directly against sqlite3 3.51).
        let conn = open_state_db();
        let now = "2026-07-20T12:00:00.100Z"; // background.rs rendering
        set_state(
            &conn,
            "capture_manifest",
            "same-second-not-expired",
            r#"{"expires_at":"2026-07-20T12:00:00.100999+00:00"}"#, // capture_session.rs rendering, same second as `now`
        )
        .expect("seed same-second row");
        set_state(
            &conn,
            "capture_manifest",
            "actually-expired-mixed-format",
            r#"{"expires_at":"2026-07-19T12:00:00.999999+00:00"}"#, // a full day earlier
        )
        .expect("seed genuinely expired row");

        let removed = reap_expired_state(&conn, now).expect("reap");

        assert_eq!(
            removed, 1,
            "only the genuinely-expired (a day earlier) row should be deleted"
        );
        assert!(
            get_state(&conn, "capture_manifest", "same-second-not-expired")
                .expect("get same-second row")
                .is_some(),
            "a row at the same wall-clock second as `now`, rendered with a numeric \
             offset at microsecond precision, must survive — under the old lexical \
             comparison this row was wrongly reaped"
        );
        assert!(
            get_state(&conn, "capture_manifest", "actually-expired-mixed-format")
                .expect("get expired row")
                .is_none(),
            "a genuinely expired row (a full day earlier) must still be reaped \
             across mixed renderings"
        );
    }

    #[test]
    fn reap_expired_state_semantic_comparison_holds_in_the_reversed_rendering_combination() {
        // Same defect class as the test above, with the renderings swapped
        // (`now` carries the numeric-offset/microsecond rendering, the
        // stored row carries the `Z`/millisecond rendering) — proves the
        // fix is not direction-specific.
        let conn = open_state_db();
        let now = "2026-07-20T12:00:00.999999+00:00";
        set_state(
            &conn,
            "capture_manifest",
            "same-second-not-expired-reversed",
            r#"{"expires_at":"2026-07-20T12:00:00.050Z"}"#, // same second as `now`, smaller fraction digit
        )
        .expect("seed same-second row");
        set_state(
            &conn,
            "capture_manifest",
            "actually-expired-reversed",
            r#"{"expires_at":"2026-07-19T12:00:00.001Z"}"#, // a full day earlier
        )
        .expect("seed genuinely expired row");

        let removed = reap_expired_state(&conn, now).expect("reap");

        assert_eq!(
            removed, 1,
            "only the genuinely-expired row should be deleted"
        );
        assert!(
            get_state(
                &conn,
                "capture_manifest",
                "same-second-not-expired-reversed"
            )
            .expect("get same-second row")
            .is_some(),
            "same-second row must survive regardless of which side renders Z vs \
             numeric offset"
        );
        assert!(
            get_state(&conn, "capture_manifest", "actually-expired-reversed")
                .expect("get expired row")
                .is_none(),
            "a genuinely expired row must still be reaped in the reversed \
             rendering combination too"
        );
    }

    #[test]
    fn reap_expired_state_fail_closed_on_non_string_or_unparseable_expires_at() {
        let conn = open_state_db();
        // A far-future `now` so any accidental "expired" match would be
        // impossible to explain except by the fail-closed guards failing.
        let now = "2999-01-01T00:00:00Z";
        set_state(&conn, "ns", "number", r#"{"expires_at":12345}"#).expect("seed number");
        set_state(&conn, "ns", "bool_true", r#"{"expires_at":true}"#).expect("seed bool true");
        set_state(&conn, "ns", "bool_false", r#"{"expires_at":false}"#).expect("seed bool false");
        set_state(
            &conn,
            "ns",
            "garbage_string",
            r#"{"expires_at":"not-a-real-timestamp"}"#,
        )
        .expect("seed garbage string");
        set_state(&conn, "ns", "empty_string", r#"{"expires_at":""}"#).expect("seed empty string");

        let removed = reap_expired_state(&conn, now).expect("reap");

        assert_eq!(
            removed, 0,
            "non-string and unparseable expires_at values must never be reaped, \
             even against a far-future `now` (fail-closed, not fail-open)"
        );
        for key in [
            "number",
            "bool_true",
            "bool_false",
            "garbage_string",
            "empty_string",
        ] {
            assert!(
                get_state(&conn, "ns", key).expect("get row").is_some(),
                "{key} row must survive the fail-closed guards"
            );
        }
    }

    #[test]
    fn reap_expired_state_surfaces_a_hard_error_instead_of_swallowing_it() {
        // No `hard_state` table created on this connection — a stand-in for
        // the kind of hard DB-level failure a closed/unusable store would
        // produce. `reap_expired_state`'s caller (the daemon maintenance
        // loop) must be able to observe this as a genuine `Err`, not have it
        // silently swallowed into `Ok(0)` — a silent swallow here would be
        // indistinguishable from "nothing was expired yet".
        let conn = Connection::open_in_memory().expect("open db");
        let err = reap_expired_state(&conn, "2026-01-01T00:00:00Z").expect_err(
            "reap against a DB missing the hard_state table must error, not succeed silently",
        );
        assert!(
            err.to_string().to_lowercase().contains("no such table"),
            "error should name the missing table, got: {err}"
        );
    }

    #[test]
    fn backfill_missing_expires_at_is_unconditional_and_idempotent() {
        let conn = open_state_db();
        set_state(&conn, "build_receipt", "t1", r#"{"outcome":"success"}"#)
            .expect("seed receipt with no expires_at");

        let ttl = "2126-07-20T00:00:00Z";
        let backfilled =
            backfill_missing_expires_at(&conn, "build_receipt", ttl, None).expect("first backfill");
        assert_eq!(backfilled, 1, "the one missing-key row must be backfilled");

        let (value, _version) = get_state(&conn, "build_receipt", "t1")
            .expect("get after backfill")
            .expect("row still present");
        let parsed: serde_json::Value = serde_json::from_str(&value).expect("valid json");
        assert_eq!(
            parsed["expires_at"].as_str(),
            Some(ttl),
            "expires_at must be set to the TTL: {value}"
        );
        assert_eq!(
            parsed["outcome"].as_str(),
            Some("success"),
            "other fields must survive the backfill untouched: {value}"
        );

        // Idempotent: a second run against unchanged data touches nothing.
        let second_run = backfill_missing_expires_at(&conn, "build_receipt", ttl, None)
            .expect("second backfill");
        assert_eq!(
            second_run, 0,
            "re-running the backfill must be a no-op once every row already has expires_at"
        );
    }

    #[test]
    fn unconditional_backfill_skips_malformed_json_and_updates_valid_rows() {
        let conn = open_state_db();
        set_state(&conn, "build_receipt", "corrupt", "not valid json").expect("seed corrupt row");
        set_state(&conn, "build_receipt", "valid", r#"{"outcome":"success"}"#)
            .expect("seed valid row");

        let backfilled =
            backfill_missing_expires_at(&conn, "build_receipt", "2126-07-20T00:00:00Z", None)
                .expect("malformed state must not abort an unconditional backfill");

        assert_eq!(backfilled, 1, "the valid row must still be backfilled");
        assert_eq!(
            get_state(&conn, "build_receipt", "corrupt")
                .expect("read corrupt row")
                .expect("corrupt row exists")
                .0,
            "not valid json",
            "malformed JSON must remain untouched"
        );
        let (valid, _) = get_state(&conn, "build_receipt", "valid")
            .expect("read valid row")
            .expect("valid row exists");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&valid).unwrap()["expires_at"],
            "2126-07-20T00:00:00Z"
        );
    }

    #[test]
    fn backfill_missing_expires_at_never_clobbers_the_explicit_retain_forever_null() {
        let conn = open_state_db();
        set_state(
            &conn,
            "capture_manifest",
            "completed-forever",
            r#"{"expires_at":null,"completed":true}"#,
        )
        .expect("seed retain-forever row (#1301)");

        let backfilled =
            backfill_missing_expires_at(&conn, "capture_manifest", "2126-07-20T00:00:00Z", None)
                .expect("backfill");
        assert_eq!(
            backfilled, 0,
            "a row that already carries an explicit expires_at:null must not be touched — \
             json_type(...) IS NULL (missing key) must not be confused with the JSON null value"
        );

        let (value, _version) = get_state(&conn, "capture_manifest", "completed-forever")
            .expect("get")
            .expect("row present");
        assert_eq!(
            value, r#"{"expires_at":null,"completed":true}"#,
            "the #1301 retain-forever marker must survive byte-for-byte"
        );
    }

    #[test]
    fn backfill_missing_expires_at_only_terminal_status_rows_when_scoped() {
        let conn = open_state_db();
        set_state(
            &conn,
            "memory_lifecycle_proposals",
            "p-pending",
            r#"{"status":"pending"}"#,
        )
        .expect("seed pending proposal");
        set_state(
            &conn,
            "memory_lifecycle_proposals",
            "p-approved",
            r#"{"status":"approved"}"#,
        )
        .expect("seed approved-not-yet-applied proposal");
        set_state(
            &conn,
            "memory_lifecycle_proposals",
            "p-rejected",
            r#"{"status":"rejected"}"#,
        )
        .expect("seed rejected proposal");
        set_state(
            &conn,
            "memory_lifecycle_proposals",
            "p-applied",
            r#"{"status":"applied"}"#,
        )
        .expect("seed applied proposal");

        let backfilled = backfill_missing_expires_at(
            &conn,
            "memory_lifecycle_proposals",
            "2126-07-20T00:00:00Z",
            Some(("$.status", &["applied", "rejected"])),
        )
        .expect("scoped backfill");
        assert_eq!(
            backfilled, 2,
            "only the applied and rejected rows are terminal"
        );

        for (key, expect_ttl) in [
            ("p-pending", false),
            ("p-approved", false),
            ("p-rejected", true),
            ("p-applied", true),
        ] {
            let (value, _version) = get_state(&conn, "memory_lifecycle_proposals", key)
                .expect("get")
                .expect("row present");
            let has_ttl = value.contains("expires_at");
            assert_eq!(
                has_ttl, expect_ttl,
                "{key}: expected expires_at presence = {expect_ttl}, row = {value}"
            );
        }
    }

    #[test]
    fn scoped_backfill_skips_malformed_json_and_updates_matching_valid_rows() {
        let conn = open_state_db();
        set_state(
            &conn,
            "memory_lifecycle_proposals",
            "corrupt",
            "not valid json",
        )
        .expect("seed corrupt row");
        set_state(
            &conn,
            "memory_lifecycle_proposals",
            "applied",
            r#"{"status":"applied"}"#,
        )
        .expect("seed matching terminal row");

        let backfilled = backfill_missing_expires_at(
            &conn,
            "memory_lifecycle_proposals",
            "2126-07-20T00:00:00Z",
            Some(("$.status", &["applied"])),
        )
        .expect("malformed state must not abort a scoped backfill");

        assert_eq!(backfilled, 1, "the matching valid row must be backfilled");
        assert_eq!(
            get_state(&conn, "memory_lifecycle_proposals", "corrupt")
                .expect("read corrupt row")
                .expect("corrupt row exists")
                .0,
            "not valid json",
            "malformed JSON must remain untouched"
        );
        let (applied, _) = get_state(&conn, "memory_lifecycle_proposals", "applied")
            .expect("read applied row")
            .expect("applied row exists");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&applied).unwrap()["expires_at"],
            "2126-07-20T00:00:00Z"
        );
    }

    #[test]
    fn backfill_missing_expires_at_with_empty_terminal_list_is_a_noop() {
        let conn = open_state_db();
        set_state(&conn, "ns", "k1", r#"{"status":"applied"}"#).expect("seed");

        let backfilled = backfill_missing_expires_at(
            &conn,
            "ns",
            "2126-07-20T00:00:00Z",
            Some(("$.status", &[])),
        )
        .expect("backfill with empty allow-list");
        assert_eq!(
            backfilled, 0,
            "an empty terminal-values allow-list can never match any row"
        );
    }

    /// CONCERN 6 (cross-vendor review): `status_path` is interpolated
    /// directly into SQL text, and the function signature (`&str`) does not
    /// stop a caller from passing something off the reviewed allow-list. An
    /// unrecognized path must be refused with an `Err`, not silently
    /// interpolated — and critically, must not touch any row: this is a
    /// fail-closed refusal, not a partial/degraded backfill.
    #[test]
    fn backfill_missing_expires_at_rejects_a_status_path_not_on_the_allow_list() {
        let conn = open_state_db();
        set_state(&conn, "ns", "k1", r#"{"status":"applied"}"#).expect("seed");

        let err = backfill_missing_expires_at(
            &conn,
            "ns",
            "2126-07-20T00:00:00Z",
            Some(("$.attacker_controlled", &["applied"])),
        )
        .expect_err("an unlisted status_path must be refused, not trusted");
        assert!(
            err.to_string().contains("attacker_controlled"),
            "the refusal must name the offending path: {err}"
        );
        assert!(
            err.to_string().contains("not on the allowed list"),
            "the refusal must explain why: {err}"
        );

        // And nothing was touched — a refused call must not partially apply.
        let (value, _version) = get_state(&conn, "ns", "k1")
            .expect("get")
            .expect("row present");
        assert_eq!(
            value, r#"{"status":"applied"}"#,
            "a refused backfill call must leave every row byte-for-byte untouched"
        );
    }
}
