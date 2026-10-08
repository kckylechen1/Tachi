//! D7 policy is selected by the write-once profile stamp, never by table contents.

use rusqlite::Connection;

use super::{migrations, store_identity, StoreProfile, STORE_PROFILE_KEY};
use crate::error::MemoryError;

/// Read-only admission result. Pending describes the authorized migration
/// target; it grants no authority and performs no migration.
#[derive(Debug)]
pub enum StoreVersionStatus {
    Fresh,
    Current { stamp: u32 },
    Pending { from: u32, to: u32 },
    Newer { stamp: u32 },
    Refused(MemoryError),
}

/// Inspect an offline store without creating a file, SQLite sidecars, backup
/// or marker. Immutable SQLite cannot see pending WAL/journal changes, so
/// those paths refuse rather than report a stale main-file version.
/// This is the admission preflight, not a claim that post-maintenance shape
/// validation or an authorized migration has succeeded.
pub fn store_version_status(
    path: impl AsRef<std::path::Path>,
    requirement: super::ProfileRequirement,
) -> StoreVersionStatus {
    let path = path.as_ref();
    let inspect = || -> Result<StoreVersionStatus, MemoryError> {
        let conn = open_immutable(path)?;
        let snapshot = conn.unchecked_transaction()?;
        let status = super::schema::inspect_version_status(&snapshot, path, requirement)?;
        snapshot.commit()?;
        Ok(status)
    };
    inspect().unwrap_or_else(StoreVersionStatus::Refused)
}

/// Read an offline store's write-once profile stamp with the same zero-touch
/// immutable open as [`store_version_status`]. `None` means the store carries
/// no stamp: fresh, or a pre-#1585 database that only a Full requirement
/// adopts. An unknown token is refused rather than guessed. Callers that
/// choose an admission requirement from this value still pass through the
/// authoritative in-transaction profile resolution on the real open.
pub fn store_profile_stamp(
    path: impl AsRef<std::path::Path>,
) -> Result<Option<StoreProfile>, MemoryError> {
    let path = path.as_ref();
    let conn = open_immutable(path)?;
    let snapshot = conn.unchecked_transaction()?;
    let token = store_identity::read_stamp(&snapshot, STORE_PROFILE_KEY)?;
    snapshot.commit()?;
    token
        .map(|token| super::store_profile::parse_stored_profile(&token, path))
        .transpose()
}

/// Immutable read-only open. Immutable SQLite cannot see pending WAL/journal
/// changes, so a path with one refuses instead of reporting stale state.
fn open_immutable(path: &std::path::Path) -> Result<Connection, MemoryError> {
    if let Some(sidecar) = super::pending_legacy_sidecar(path)? {
        return Err(MemoryError::InvalidArg(format!(
            "immutable admission cannot account for pending SQLite sidecar {}",
            sidecar.display()
        )));
    }
    let mut encoded = String::new();
    for byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~' | b':') {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("encode path");
        }
    }
    Ok(Connection::open_with_flags(
        format!("file:{encoded}?mode=ro&immutable=1"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileProbe {
    Portable,
    NotPortable,
}

/// The probe deliberately reads no role and returns no error. Admission
/// decodes identity later, retaining the existing winning-error order.
pub(crate) fn probe_profile(conn: &Connection) -> ProfileProbe {
    match store_identity::read_stamp(conn, STORE_PROFILE_KEY)
        .ok()
        .flatten()
        .as_deref()
        .and_then(StoreProfile::from_stamp_token)
    {
        Some(StoreProfile::PortableKernel) => ProfileProbe::Portable,
        _ => ProfileProbe::NotPortable,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VersionHeader {
    pub stored: u32,
    pub probe: ProfileProbe,
}

impl VersionHeader {
    /// Call within the caller's read snapshot or authoritative transaction.
    pub fn read(conn: &Connection) -> Result<Self, MemoryError> {
        Ok(Self {
            stored: migrations::read_schema_version(conn)?,
            probe: probe_profile(conn),
        })
    }

    pub fn projection(self) -> u32 {
        #[cfg(test)]
        if let Some(version) = super::schema::pre_b_39::active_version() {
            return version;
        }
        match self.probe {
            ProfileProbe::Portable => migrations::portable_schema_version(),
            ProfileProbe::NotPortable => migrations::supported_schema_version(),
        }
    }

    pub fn pending(self) -> bool {
        (1..self.projection()).contains(&self.stored)
    }

    pub fn current(self) -> bool {
        (self.projection()..=migrations::supported_schema_version()).contains(&self.stored)
    }

    pub fn portable_band(self) -> bool {
        #[cfg(test)]
        if super::schema::pre_b_39::active_version().is_some() {
            return false;
        }
        self.probe == ProfileProbe::Portable && self.current()
    }
}

/// Fresh and authorized pending Portable builds retain pre-D7 rollback
/// compatibility until a real Portable migration above the floor ships.
pub(crate) fn portable_output_stamp(stored: u32) -> u32 {
    stored
        .max(migrations::portable_schema_version())
        .max(migrations::PORTABLE_COMPAT_FLOOR.min(migrations::supported_schema_version()))
}
