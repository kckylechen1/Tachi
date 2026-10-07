//! One baseline inventory for #1995 presence admission and D7's complete
//! Portable shape admission. The latter follows frozen maintenance in the
//! authoritative transaction; it never repairs the input.

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

pub(crate) struct RequiredInventory {
    required: BTreeMap<String, RequiredObject>,
    portable_shapes: BTreeMap<String, ObjectShape>,
    absence_optional: BTreeSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ObjectShape {
    kind: String,
    name: String,
    owner: String,
    sql: Option<String>,
    columns: Vec<Vec<String>>,
    indexes: BTreeMap<String, Vec<Vec<String>>>,
}

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
    // The reference is independent; never initialize the input connection.
    crate::db::enable_simple_auto_extension()?;
    if profile == StoreProfile::PortableKernel {
        crate::db::register_sqlite_vec();
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
    let (portable_shapes, absence_optional) = if profile == StoreProfile::PortableKernel {
        // Optional vector capability is provisioned only in the reference.
        // Its absence on an input is allowed; a present definition is checked.
        let without_vec = capture_shapes(&reference)?;
        crate::db::try_load_sqlite_vec(&reference);
        let with_vec = capture_shapes(&reference)?;
        // Derive the actual capability family from the reference's creation,
        // including SQLite shadow objects and indexes. No input-name prefix
        // receives a waiver.
        let mut optional: BTreeSet<_> = with_vec
            .keys()
            .filter(|key| !without_vec.contains_key(*key))
            .cloned()
            .collect();
        optional.insert("index:idx_memories_path_active_ts".to_string());
        (with_vec, optional)
    } else {
        (BTreeMap::new(), BTreeSet::new())
    };
    let _ = cache.set(RequiredInventory {
        required: objects,
        portable_shapes,
        absence_optional,
    });
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
    if !crate::db::version_policy::VersionHeader::read(conn)?.current() {
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
    let stored = crate::db::migrations::read_schema_version(conn)?;
    let projection = if profile == StoreProfile::PortableKernel {
        crate::db::migrations::portable_schema_version()
    } else {
        crate::db::migrations::supported_schema_version()
    };
    if !(projection..=crate::db::migrations::supported_schema_version()).contains(&stored) {
        return Ok(());
    }
    let inventory = required_inventory(profile)?;
    let mut stmt = conn.prepare("SELECT type,name FROM main.sqlite_schema")?;
    let present: BTreeSet<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut columns = conn.prepare("SELECT name FROM pragma_table_xinfo(?1, 'main')")?;
    let mut missing = BTreeSet::new();
    for (key, object) in &inventory.required {
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

/// Complete Portable baseline after frozen maintenance, inside the schema
/// transaction. Presence admission and shape admission share this inventory;
/// no initializer or repair is executed against the input by this validator.
pub(crate) fn validate_portable_schema_shape(
    conn: &Connection,
    path: &Path,
) -> Result<(), MemoryError> {
    let inventory = required_inventory(StoreProfile::PortableKernel)?;
    let actual = capture_shapes(conn)?;
    let mut defects = BTreeSet::new();
    for (key, expected) in &inventory.portable_shapes {
        let optional = inventory.absence_optional.contains(key);
        match actual.get(key) {
            None if optional => {}
            None => {
                defects.insert(key.clone());
            }
            Some(found) if !same_shape(expected, found) => {
                defects.insert(format!("shape:{key}"));
            }
            Some(_) => {}
        }
    }
    if defects.is_empty() {
        Ok(())
    } else {
        Err(MemoryError::CurrentSchemaIncomplete {
            missing: defects.into_iter().collect(),
            db_path: path.display().to_string(),
        })
    }
}

fn same_shape(expected: &ObjectShape, actual: &ObjectShape) -> bool {
    if expected.kind != actual.kind
        || expected.owner != actual.owner
        || expected.sql != actual.sql
        || expected.columns != actual.columns
    {
        return false;
    }
    let indexes = actual.indexes.clone();
    let mut required_indexes = expected.indexes.clone();
    // The optimization index is an absence exception, never a wrong-shape
    // exception. If present its own object check and index metadata must match.
    if !indexes.contains_key("idx_memories_path_active_ts") {
        required_indexes.remove("idx_memories_path_active_ts");
    }
    // sqlite-vec's shadow indexes are associated with its optional tables,
    // rather than with the required memories table.
    indexes == required_indexes
}

fn capture_shapes(conn: &Connection) -> Result<BTreeMap<String, ObjectShape>, MemoryError> {
    let mut stmt = conn.prepare("SELECT type,name,tbl_name,sql FROM main.sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY type,name")?;
    let objects = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut shapes = BTreeMap::new();
    for (kind, name, owner, sql) in objects {
        let columns = if kind == "table" {
            shape_rows(conn, &format!("PRAGMA main.table_xinfo({})", quote(&name)))?
        } else {
            Vec::new()
        };
        let mut indexes = BTreeMap::new();
        if kind == "table" {
            let mut stmt = conn.prepare(r#"SELECT name,"unique",origin,partial FROM pragma_index_list(?1, 'main') ORDER BY name"#)?;
            let definitions = stmt
                .query_map([&name], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (index, unique, origin, partial) in definitions {
                let mut definition = vec![vec![unique.to_string(), origin, partial.to_string()]];
                definition.extend(shape_rows(
                    conn,
                    &format!("PRAGMA main.index_xinfo({})", quote(&index)),
                )?);
                indexes.insert(index, definition);
            }
        }
        shapes.insert(
            format!("{kind}:{name}"),
            ObjectShape {
                kind,
                name,
                owner,
                sql: sql.as_deref().map(normalize_definition),
                columns,
                indexes,
            },
        );
    }
    Ok(shapes)
}

fn shape_rows(conn: &Connection, sql: &str) -> Result<Vec<Vec<String>>, MemoryError> {
    let mut stmt = conn.prepare(sql)?;
    let width = stmt.column_count();
    let rows = stmt
        .query_map([], |row| {
            (0..width)
                .map(|index| row.get_ref(index).map(|value| format!("{value:?}")))
                .collect::<rusqlite::Result<Vec<_>>>()
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Ignore formatting and keyword case, retaining every quoted byte. Whitespace
/// inside a default, CHECK or partial-index literal changes semantics.
fn normalize_definition(sql: &str) -> String {
    let mut normalized = String::new();
    let mut quote_end = None;
    let mut spacing = false;
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(end) = quote_end {
            normalized.push(ch);
            if ch == end {
                if chars.peek() == Some(&end) {
                    normalized.push(chars.next().unwrap());
                } else {
                    quote_end = None;
                }
            }
        } else {
            if ch.is_whitespace() {
                spacing = true;
                continue;
            }
            if spacing
                && (ch.is_alphanumeric() || ch == '_')
                && normalized
                    .chars()
                    .last()
                    .is_some_and(|last| last.is_alphanumeric() || last == '_')
            {
                normalized.push(' ');
            }
            spacing = false;
            match ch {
                '\'' | '"' | '`' => {
                    quote_end = Some(ch);
                    normalized.push(ch);
                }
                '[' => {
                    quote_end = Some(']');
                    normalized.push(ch);
                }
                ';' => {}
                ch => normalized.extend(ch.to_lowercase()),
            }
        }
    }
    normalized
}

#[cfg(test)]
impl RequiredInventory {
    pub(crate) fn object_keys(&self) -> Vec<String> {
        self.required.keys().cloned().collect()
    }

    pub(crate) fn table_columns(&self) -> BTreeMap<String, Vec<String>> {
        self.required
            .values()
            .filter(|object| object.kind == "table")
            .map(|object| (object.name.clone(), object.columns.clone()))
            .collect()
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    #[test]
    fn definition_normalization_retains_literal_constraint_and_predicate_semantics() {
        assert_eq!(
            normalize_definition("CREATE INDEX i ON t (c) WHERE c = 'A  B'"),
            normalize_definition("create  index i on t(c) where c='A  B';")
        );
        for (left, right) in [
            ("CHECK(c='A  B')", "CHECK(c='A B')"),
            ("DEFAULT 'CURRENT_TIMESTAMP'", "DEFAULT CURRENT_TIMESTAMP"),
            ("CREATE UNIQUE INDEX i ON t(c)", "CREATE INDEX i ON t(c)"),
            (
                "CREATE INDEX i ON t(c) WHERE archived=0",
                "CREATE INDEX i ON t(c) WHERE archived=1",
            ),
            (
                "CHECK(c IN ('Raw','processed'))",
                "CHECK(c IN ('raw','processed'))",
            ),
        ] {
            assert_ne!(
                normalize_definition(left),
                normalize_definition(right),
                "{left} vs {right}"
            );
        }
    }
}
