//! Test-only shared schema inventory for #1995 and the D7 follow-up.
//! Raw preservation snapshots are lossless and unfiltered. Classification is
//! applied only to the separate required projection, never to those snapshots.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// Lossless SQLite value representations, sorted to avoid depending on a query
// plan. Keep autoindexes and virtual-table shadow objects in the inventory.
pub(crate) fn query_rows(conn: &Connection, sql: &str) -> Vec<Vec<String>> {
    let mut stmt = conn.prepare(sql).expect("prepare snapshot query");
    let width = stmt.column_count();
    let mut rows: Vec<_> = stmt
        .query_map([], |row| {
            (0..width)
                .map(|i| row.get_ref(i).map(|value| format!("{value:?}")))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .expect("query snapshot")
        .collect::<rusqlite::Result<_>>()
        .expect("read snapshot");
    rows.sort();
    rows
}

pub(crate) fn tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let rows = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    rows
}

pub(crate) fn schema_inventory(conn: &Connection) -> Value {
    let table_shapes: BTreeMap<_, _> = tables(conn)
        .into_iter()
        .map(|name| {
            let mut stmt = conn
                .prepare("SELECT name FROM pragma_index_list(?1) ORDER BY name")
                .unwrap();
            let names: Vec<String> = stmt
                .query_map([&name], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let keys: BTreeMap<_, _> = names
                .into_iter()
                .map(|index| {
                    let keys = query_rows(conn, &format!("PRAGMA index_xinfo({})", quote(&index)));
                    (index, keys)
                })
                .collect();
            let shapes = json!({
                "columns": query_rows(conn, &format!("PRAGMA table_xinfo({})", quote(&name))),
                "indexes": query_rows(conn, &format!("PRAGMA index_list({})", quote(&name))),
                "index_keys": keys,
            });
            (name, shapes)
        })
        .collect();
    json!({
        // SQL retains expressions, constraints and partial predicates; index_list
        // additionally records uniqueness, origin and partial flags, including
        // autoindexes whose sqlite_schema.sql is NULL.
        "objects": query_rows(conn, "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name"),
        "tables": table_shapes,
    })
}

#[derive(Debug)]
struct Object {
    kind: String,
    name: String,
    owner: String,
    sql: Option<String>,
}

pub(crate) struct ClassifiedInventory {
    pub(crate) required: Value,
    pub(crate) classes: BTreeMap<String, Value>,
    pub(crate) validator_required: BTreeMap<String, String>,
}

impl ClassifiedInventory {
    pub(crate) fn frozen_classification(&self) -> Value {
        let mut classes = self.classes.clone();
        classes.retain(|key, class| {
            if key == "index:idx_memories_path_active_ts" || key == "table:memories_vec" {
                return false;
            }
            let mut owner = class["owner"].as_str().unwrap();
            for _ in 0..self.classes.len() {
                if owner == "memories_vec" {
                    return false;
                }
                let Some(parent) = self.classes.get(&format!("table:{owner}")) else {
                    break;
                };
                let next = parent["owner"].as_str().unwrap();
                if next == owner {
                    break;
                }
                owner = next;
            }
            true
        });
        classes.insert(
            "table:memories_vec".into(),
            json!({"class":"derived","owner":"memories_vec","reason":"frozen derived family"}),
        );
        classes.insert(
            "index:idx_memories_path_active_ts".into(),
            json!({"class":"derived","owner":"memories","reason":"frozen index exception"}),
        );
        json!(classes)
    }
}

/// Invoke the existing validators rather than duplicate their index inventory.
/// This census is only for a canonical owned fixture. Each deletion rolls back,
/// including on panic; it never runs on the damaged refusal fixtures.
fn validator_index_census(conn: &Connection) -> BTreeMap<String, String> {
    crate::db::migrations::validate_current_schema_integrity(conn).expect("canonical integrity");
    crate::db::validate_persistent_trigger_inventory(conn, true).expect("canonical triggers");
    let mut stmt = conn.prepare("SELECT m.name FROM sqlite_schema m JOIN pragma_index_list(m.tbl_name) i ON i.name=m.name WHERE m.type='index' AND i.origin='c' AND i.[unique]=0 ORDER BY m.name").unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    drop(stmt);
    let before = schema_inventory(conn);
    let mut required = BTreeMap::new();
    for name in names {
        // One independent image per probe: DDL rollback can invalidate a
        // connection's cached index_xinfo metadata. Never touch the capture
        // connection or reuse a mutated probe for the next classification.
        let mut probe = Connection::open_in_memory().unwrap();
        {
            let backup = rusqlite::backup::Backup::new(conn, &mut probe).unwrap();
            assert_eq!(backup.step(-1).unwrap(), rusqlite::backup::StepResult::Done);
        }
        crate::db::migrations::validate_current_schema_integrity(&probe)
            .expect("canonical probe image");
        let tx = probe
            .unchecked_transaction()
            .expect("owned census transaction");
        tx.execute(&format!("DROP INDEX {}", quote(&name)), [])
            .unwrap();
        if let Err(error) = crate::db::migrations::validate_current_schema_integrity(&tx) {
            let diagnostic = error.to_string();
            assert!(
                diagnostic.contains(&name),
                "unrelated validator failure for {name}: {diagnostic}"
            );
            required.insert(name, diagnostic);
        }
        tx.rollback().unwrap();
    }
    assert_eq!(
        before,
        schema_inventory(conn),
        "census must roll back all schema changes"
    );
    required
}

/// All canonical objects receive one class. SQLite-reported shadow tables and
/// autoindexes keep their owner and shape in raw capture, not a prefix waiver.
/// https://www.sqlite.org/pragma.html#pragma_table_list
pub(crate) fn classified_inventory(conn: &Connection) -> ClassifiedInventory {
    let raw = schema_inventory(conn);
    let validator_required = validator_index_census(conn);
    let mut stmt = conn
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")
        .unwrap();
    let objects: Vec<Object> = stmt
        .query_map([], |r| {
            Ok(Object {
                kind: r.get(0)?,
                name: r.get(1)?,
                owner: r.get(2)?,
                sql: r.get(3)?,
            })
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT name,type FROM pragma_table_list WHERE schema='main'")
        .unwrap();
    let kinds: BTreeMap<String, String> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let virtuals: Vec<_> = kinds
        .iter()
        .filter(|(_, kind)| kind.as_str() == "virtual")
        .map(|(name, _)| name)
        .collect();
    let derived_roots = [
        "memories_fts",
        "memories_symbolic_fts",
        "recall_cache",
        "memories_vec",
    ];
    let mut classes = BTreeMap::new();
    let mut required = BTreeMap::new();
    for object in objects {
        let key = format!("{}:{}", object.kind, object.name);
        let (class, owner, reason) = if object.kind == "index" && object.sql.is_none() {
            ("dependent", object.owner.clone(), "table constraint")
        } else if object.name.starts_with("sqlite_") {
            ("dependent", "SQLite".into(), "engine metadata")
        } else if kinds.get(&object.name).is_some_and(|kind| kind == "shadow") {
            let owners: Vec<_> = virtuals
                .iter()
                .filter(|root| object.name.starts_with(&format!("{root}_")))
                .collect();
            assert_eq!(
                owners.len(),
                1,
                "shadow owner must be unambiguous: {}",
                object.name
            );
            ("dependent", owners[0].to_string(), "virtual table shadow")
        } else if object.kind == "table"
            && [
                "memories_vec_info",
                "memories_vec_chunks",
                "memories_vec_rowids",
                "memories_vec_vector_chunks00",
            ]
            .contains(&object.name.as_str())
            && virtuals.iter().any(|root| root.as_str() == "memories_vec")
        {
            // Exact generated family for our one-column vec0 DDL. The pinned
            // module omits vector_chunksNN from xShadowName, and schema cache
            // refresh may expose its other shadows as ordinary tables. No
            // arbitrary prefix or future vector-column exception.
            (
                "dependent",
                "memories_vec".into(),
                "pinned one-column vec0 shadow",
            )
        } else if object.kind == "table" && derived_roots.contains(&object.name.as_str()) {
            ("derived", object.name.clone(), "frozen derived family")
        } else if object.kind == "index"
            && (kinds
                .get(&object.owner)
                .is_some_and(|kind| kind == "shadow")
                || derived_roots.contains(&object.owner.as_str()))
        {
            ("dependent", object.owner.clone(), "derived family index")
        } else if object.kind == "index" {
            let flags: (bool, String, bool) = conn
                .query_row(
                    "SELECT [unique],origin,partial FROM pragma_index_list(?1) WHERE name=?2",
                    [&object.owner, &object.name],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            if [
                "idx_memories_idless_identity_active",
                "idx_memories_path_active_ts",
            ]
            .contains(&object.name.as_str())
            {
                ("derived", object.owner.clone(), "frozen index exception")
            } else if !flags.0 && !validator_required.contains_key(&object.name) {
                (
                    "derived",
                    object.owner.clone(),
                    "nonunique index not required by validator",
                )
            } else {
                (
                    "required",
                    object.owner.clone(),
                    "unique or validator-required index",
                )
            }
        } else {
            assert!(
                ["table", "view", "trigger"].contains(&object.kind.as_str()),
                "unclassified object: {object:?}"
            );
            ("required", object.owner.clone(), "default deny")
        };
        classes.insert(
            key.clone(),
            json!({"class":class,"owner":owner,"reason":reason}),
        );
        if class != "required" {
            continue;
        }
        let mut shape = json!({"owner":object.owner,"sql":object.sql});
        if object.kind == "table" {
            shape["columns"] = raw["tables"][&object.name]["columns"].clone();
            let mut stmt = conn
                .prepare("SELECT name FROM pragma_index_list(?1) WHERE origin!='c' ORDER BY name")
                .unwrap();
            let constraints: Vec<String> = stmt
                .query_map([&object.name], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let constraints: BTreeMap<_, _> = constraints
                .into_iter()
                .map(|name| {
                    let keys = raw["tables"][&object.name]["index_keys"][&name].clone();
                    (name, keys)
                })
                .collect();
            shape["constraint_indexes"] = json!(constraints);
        }
        if object.kind == "index" {
            shape["flags"] = json!(query_rows(
                conn,
                &format!(
                    "SELECT [unique],origin,partial FROM pragma_index_list({}) WHERE name={}",
                    quote(&object.owner),
                    quote(&object.name)
                )
            ));
            shape["keys"] = raw["tables"][&object.owner]["index_keys"][&object.name].clone();
        }
        required.insert(key, shape);
    }
    assert_eq!(
        classes.len(),
        raw["objects"].as_array().unwrap().len(),
        "every raw object must be classified"
    );
    ClassifiedInventory {
        required: json!(required),
        classes,
        validator_required,
    }
}

pub(crate) fn golden(profile: crate::db::StoreProfile) -> Value {
    let all: Value = serde_json::from_str(include_str!("goldens/required-v39.json"))
        .expect("frozen inventory JSON");
    let version = crate::db::migrations::EXPECTED_SCHEMA_VERSION.to_string();
    all.get(&version)
        .and_then(|v| v.get(format!("{profile:?}")))
        .expect("schema growth requires a new version-keyed golden")
        .clone()
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum GrowthMutation {
    BeforeMemoryRebuild,
    ElsewhereColumn,
    InlineTable,
}
thread_local! {
    static MUTATION: RefCell<Option<(GrowthMutation, PathBuf)>> = const { RefCell::new(None) };
    static FIRES: Cell<usize> = const { Cell::new(0) };
}

#[must_use]
pub(crate) struct GrowthGuard {
    // A guard may only clean up the thread on which it was armed.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Drop for GrowthGuard {
    fn drop(&mut self) {
        MUTATION.with(|slot| *slot.borrow_mut() = None);
        FIRES.with(|slot| slot.set(0));
    }
}
pub(crate) fn arm_growth(mutation: GrowthMutation, path: &Path) -> GrowthGuard {
    MUTATION.with(|slot| {
        assert!(slot.borrow().is_none(), "nested inventory mutation");
        *slot.borrow_mut() = Some((mutation, path.to_owned()));
    });
    FIRES.with(|slot| slot.set(0));
    GrowthGuard {
        _thread: std::marker::PhantomData,
    }
}
pub(crate) fn growth_fires() -> usize {
    FIRES.with(Cell::get)
}

pub(super) fn before_enum_rebuild(conn: &Connection) -> Result<(), crate::MemoryError> {
    let mutation = MUTATION.with(|slot| slot.borrow().clone());
    if let Some((mutation, path)) = mutation {
        let actual: String = conn.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |r| r.get(0),
        )?;
        if actual.is_empty() || std::fs::canonicalize(actual)? != std::fs::canonicalize(path)? {
            return Ok(());
        }
        FIRES.with(|slot| slot.set(slot.get() + 1));
        match mutation {
            GrowthMutation::BeforeMemoryRebuild => {
                super::ensure_column(conn, "memories", "inventory_unversioned_column", "TEXT")?
            }
            GrowthMutation::ElsewhereColumn => super::ensure_column(
                conn,
                "derived_items",
                "inventory_unversioned_column",
                "TEXT",
            )?,
            GrowthMutation::InlineTable => {
                conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS memories_fts_inventory_unversioned_table (id TEXT PRIMARY KEY)",
                )?;
            }
        }
    }
    Ok(())
}
