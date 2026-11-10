//! Tamper fixtures for stores whose stored schema declares a column that
//! shadows a rowid alias (`rowid`, `_rowid_`, `oid`).
//!
//! SQLite 3.54 refuses `ALTER TABLE ... ADD COLUMN` for such a column
//! ("cannot add rowid alias"), but it still opens a database whose stored
//! schema already declares one: a file written by an older SQLite, or a table
//! created with the column in `CREATE TABLE`. The reopen guards must keep
//! failing closed on those files, so fixtures reproduce the pre-3.54 result:
//! SQLite adds the column under a placeholder name (so it places the column
//! definition exactly where `ADD COLUMN` always has), then the stored schema
//! text is renamed to the shadowing name.

use rusqlite::Connection;

const PLACEHOLDER: &str = "pre_3_54_rowid_alias_placeholder";

/// `declaration` is `<name> <rest>`, exactly as it would follow `ADD COLUMN`.
pub(crate) fn add_column_as_pre_3_54_alter(conn: &Connection, table: &str, declaration: &str) {
    let (name, rest) = declaration
        .split_once(' ')
        .expect("declaration is `<name> <definition>`");
    conn.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {PLACEHOLDER} {rest}"
    ))
    .unwrap();
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .expect("table exists");
    assert_eq!(sql.matches(PLACEHOLDER).count(), 1);
    let rewritten = sql.replace(PLACEHOLDER, name);
    let schema_version: i64 = conn
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap();
    conn.execute_batch("PRAGMA writable_schema = ON").unwrap();
    conn.execute(
        "UPDATE sqlite_schema SET sql = ?1 WHERE type = 'table' AND name = ?2",
        [rewritten.as_str(), table],
    )
    .unwrap();
    conn.execute_batch(&format!(
        "PRAGMA schema_version = {}; PRAGMA writable_schema = OFF",
        schema_version + 1
    ))
    .unwrap();
}
