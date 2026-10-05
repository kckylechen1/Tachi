//! Presence-only admission for current stores (#1995). Shape validation and
//! Portable version bands remain the separate D7 contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use rusqlite::Connection;
use serde_json::Value;

use crate::db::{ProfileRequirement, StoreProfile};
use crate::error::MemoryError;

struct RequiredObject {
    kind: String,
    name: String,
    columns: Vec<String>,
}

pub(crate) struct RequiredInventory(BTreeMap<String, RequiredObject>);

static PORTABLE: OnceLock<RequiredInventory> = OnceLock::new();
static FULL: OnceLock<RequiredInventory> = OnceLock::new();

/// Reuse the version-keyed inventory pinned by fresh/reopened initializer
/// tests. Columns come from a separate fresh in-memory reference, never by
/// parsing the snapshot's diagnostic SQLite value representations. No census
/// or initialization runs against the input connection.
pub(crate) fn required_inventory(
    profile: StoreProfile,
) -> Result<&'static RequiredInventory, MemoryError> {
    let cache = match profile {
        StoreProfile::PortableKernel => &PORTABLE,
        StoreProfile::TachiFull => &FULL,
    };
    if let Some(inventory) = cache.get() {
        return Ok(inventory);
    }
    let reference = Connection::open_in_memory()?;
    crate::db::configure_connection(&reference)?;
    super::init_schema_for_profile(&reference, profile)?;
    let pinned: Value = serde_json::from_str(include_str!("goldens/required-v39.json"))?;
    let profile_key = match profile {
        StoreProfile::PortableKernel => "PortableKernel",
        StoreProfile::TachiFull => "TachiFull",
    };
    let version = crate::db::migrations::EXPECTED_SCHEMA_VERSION.to_string();
    let required = pinned[&version][profile_key]["required"]
        .as_object()
        .ok_or_else(|| {
            MemoryError::InvalidArg(format!(
                "required schema inventory is missing version {version}, profile {profile_key}"
            ))
        })?;
    let mut objects = BTreeMap::new();
    for key in required.keys() {
        let (kind, name) = key.split_once(':').ok_or_else(|| {
            MemoryError::InvalidArg(format!("invalid required schema inventory key: {key}"))
        })?;
        let present: bool = reference.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type=?1 AND name=?2)",
            [kind, name],
            |row| row.get(0),
        )?;
        if !present {
            return Err(MemoryError::InvalidArg(format!(
                "initializer is missing pinned required object {key}"
            )));
        }
        let columns = if kind == "table" {
            let mut stmt = reference.prepare("SELECT name FROM pragma_table_xinfo(?1, 'main')")?;
            let columns = stmt
                .query_map([name], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            columns
        } else {
            Vec::new()
        };
        objects.insert(
            key.clone(),
            RequiredObject {
                kind: kind.into(),
                name: name.into(),
                columns,
            },
        );
    }
    // Concurrent first callers may build independent references. Only complete
    // inventories are cached; an initialization error is retryable, not poison.
    let _ = cache.set(RequiredInventory(objects));
    Ok(cache.get().expect("complete required inventory installed"))
}

/// Profile selection reads no role and performs no adoption writes. A valid
/// stored profile chooses the baseline before later requirement admission.
/// On an undecodable/unadmitted unstamped profile, preserve the original
/// role-decode-before-profile error ordering rather than guessing Full.
pub(crate) fn validate_current_schema_presence(
    conn: &Connection,
    path: &Path,
    required: ProfileRequirement,
) -> Result<(), MemoryError> {
    if crate::db::migrations::read_schema_version(conn)?
        != crate::db::migrations::EXPECTED_SCHEMA_VERSION
    {
        return Ok(());
    }
    let profile = (|| {
        let stamp = crate::db::store_identity::read_stamp(conn, crate::db::STORE_PROFILE_KEY)?;
        match stamp {
            Some(token) => crate::db::store_profile::parse_stored_profile(&token, path),
            None => crate::db::store_identity::resolve_profile(None, false, required, path),
        }
    })();
    let profile = match profile {
        Ok(profile) => profile,
        Err(error) => {
            // In particular, malformed role + malformed profile keeps its
            // existing role error. No baseline means no repair permission.
            crate::db::store_identity::read_identity(conn, path)?;
            return Err(error);
        }
    };
    validate_current_schema_presence_for_profile(conn, path, profile)
}

/// The identity-bound fresh reopen carries its committed profile, so it must
/// not rederive identity from a later stamp while checking schema presence.
pub(crate) fn validate_current_schema_presence_for_profile(
    conn: &Connection,
    path: &Path,
    profile: StoreProfile,
) -> Result<(), MemoryError> {
    if crate::db::migrations::read_schema_version(conn)?
        != crate::db::migrations::EXPECTED_SCHEMA_VERSION
    {
        return Ok(());
    }
    let inventory = required_inventory(profile)?;
    let mut stmt = conn.prepare("SELECT type,name FROM main.sqlite_schema")?;
    let present: BTreeSet<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut columns = conn.prepare("SELECT name FROM pragma_table_xinfo(?1, 'main')")?;
    let mut missing = BTreeSet::new();
    for (key, object) in &inventory.0 {
        if !present.contains(&(object.kind.clone(), object.name.clone())) {
            missing.insert(key.clone());
            continue;
        }
        if object.kind == "table" {
            let actual: BTreeSet<String> = columns
                .query_map([&object.name], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for column in &object.columns {
                if !actual.contains(column) {
                    missing.insert(format!("column:{}.{}", object.name, column));
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MemoryError::CurrentSchemaIncomplete {
            missing: missing.into_iter().collect(),
            db_path: path.display().to_string(),
        })
    }
}

#[cfg(test)]
impl RequiredInventory {
    pub(crate) fn object_keys(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    pub(crate) fn table_columns(&self) -> BTreeMap<String, Vec<String>> {
        self.0
            .values()
            .filter(|object| object.kind == "table")
            .map(|object| (object.name.clone(), object.columns.clone()))
            .collect()
    }
}
