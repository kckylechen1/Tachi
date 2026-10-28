//! One read probe for the healthy legacy-row normalization case.

use super::{Connection, MemoryError};

const NEEDS_LEGACY_NORMALIZATION: &str = "SELECT EXISTS(
    SELECT 1 FROM memories
    WHERE created_at IS NULL OR created_at = ''
       OR updated_at IS NULL OR updated_at = ''
       OR revision IS NULL OR revision <= 0
       OR valid_from IS NULL OR valid_from = ''
)";

/// Called in the initializer's existing transaction, after column admission.
/// The slow branch preserves statement order and the existing validity repair.
pub(super) fn normalize_legacy_memory_rows(
    conn: &Connection,
    normalize_validity: impl FnOnce(&Connection) -> Result<(), MemoryError>,
) -> Result<(), MemoryError> {
    let needs_repair: bool = conn.query_row(NEEDS_LEGACY_NORMALIZATION, [], |row| row.get(0))?;
    if !needs_repair {
        return Ok(());
    }
    conn.execute(
        "UPDATE memories SET created_at = timestamp WHERE created_at IS NULL OR created_at = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET updated_at = created_at WHERE updated_at IS NULL OR updated_at = ''",
        [],
    )?;
    conn.execute(
        "UPDATE memories SET revision = 1 WHERE revision IS NULL OR revision <= 0",
        [],
    )?;
    normalize_validity(conn)
}
