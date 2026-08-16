//! v32 Product A2A body-retention migration (#1751).
//!
//! SQLite cannot alter a column's nullability or CHECK constraint in place.
//! The envelope and receipt tables are an FK-linked unit, so migration
//! rebuilds the pair inside the caller's outer `BEGIN IMMEDIATE` transaction.

use rusqlite::Connection;

use crate::db::StoreProfile;
use crate::error::MemoryError;

pub(super) fn migrate_v32_a2a_body_retention(
    conn: &Connection,
    profile: StoreProfile,
) -> Result<usize, MemoryError> {
    if !profile.includes_product() {
        // The sentinel and user_version still advance for profile parity, but
        // PortableKernel owns neither A2A table.
        return Ok(0);
    }

    crate::db::schema::rebuild_a2a_mailbox_to_v32(conn)?;
    Ok(5)
}
