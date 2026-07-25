//! Unit tests for `tachi repair` rules.

use std::path::PathBuf;

use rusqlite::{params, Connection};
use tempfile::TempDir;

use super::domain::DomainRepair;
use super::edges::OrphanRefs;
use super::enrichment::EnrichmentFailureReset;
use super::fts::FtsRebuild;
use super::integrity::IntegrityCheck;
use super::inventory::select_dbs;
use super::jobs::JobsPurge;
use super::junk::JunkCleanup;
use super::memory_hygiene::MemoryHygiene;
use super::plan_c::PlanCRepair;
use super::quarantine::QuarantineSweep;
use super::retention::RetentionBackfill;
use super::{DbContext, RepairRule};

fn manifest_db_entry(
    path: &std::path::Path,
    role: crate::manifest::DbRole,
) -> crate::manifest::DbEntry {
    crate::manifest::DbEntry {
        path: path.to_string_lossy().into_owned(),
        role,
        owner: "test".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: String::new(),
        last_classification: "healthy".to_string(),
        scope_hint: "project:test".to_string(),
        notes: String::new(),
    }
}

#[cfg(unix)]
fn repair_file_identity(path: &std::path::Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path).expect("path metadata");
    (metadata.dev(), metadata.ino())
}

#[test]
#[cfg(unix)]
fn db_context_rejects_manifest_project_symlink_without_external_mutation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let external_db = dir.path().join("external.db");
    let connection = fresh_db_at(&external_db, "foreign");
    drop(connection);
    let external_identity = repair_file_identity(&external_db);
    let external_before = std::fs::read(&external_db).expect("external bytes");
    let project_db = dir.path().join("manifest-project.db");
    std::os::unix::fs::symlink(&external_db, &project_db).expect("manifest project symlink");
    let project_identity = repair_file_identity(&project_db);
    let entry = manifest_db_entry(&project_db, crate::manifest::DbRole::Project);

    let error = match DbContext::open(&entry) {
        Err(error) => error,
        Ok(_) => panic!("manifest-selected project symlink must refuse repair open"),
    };

    assert!(
        error.to_string().contains("canonical repo DB path")
            && error.to_string().contains("must not be a symlink"),
        "expected canonical leaf refusal, got: {error}"
    );
    assert_eq!(repair_file_identity(&project_db), project_identity);
    assert_eq!(std::fs::read_link(&project_db).unwrap(), external_db);
    assert_eq!(repair_file_identity(&external_db), external_identity);
    assert_eq!(std::fs::read(&external_db).unwrap(), external_before);
}

#[test]
fn db_context_opens_regular_manifest_project_db() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project_db = dir.path().join("project.db");
    let connection = fresh_db_at(&project_db, "project");
    drop(connection);
    let entry = manifest_db_entry(&project_db, crate::manifest::DbRole::Project);

    let context = DbContext::open(&entry).expect("regular manifest project DB opens");

    assert_eq!(context.path, project_db);
}

#[test]
#[cfg(unix)]
fn db_context_preserves_global_db_symlink_semantics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let external_db = dir.path().join("global-target.db");
    let connection = fresh_db_at(&external_db, "global");
    drop(connection);
    let global_db = dir.path().join("global.db");
    std::os::unix::fs::symlink(&external_db, &global_db).expect("global DB symlink");
    let entry = manifest_db_entry(&global_db, crate::manifest::DbRole::Global);

    let context = DbContext::open(&entry).expect("global DB symlink remains supported");

    assert_eq!(context.path, global_db);
}

fn fresh_db(dir: &TempDir, name: &str) -> (PathBuf, Connection) {
    // FTS5 + the `simple` tokenizer is required by `init_schema`.
    memcore::db::enable_simple_auto_extension().ok();
    memcore::db::register_sqlite_vec();
    let path = dir.path().join(name);
    let mut conn = Connection::open(&path).unwrap();
    memcore::db::init_schema_with_label_mut(
        &mut conn,
        "test",
        &path,
        &memcore::db::DbOpenContext::create_fresh(),
    )
    .unwrap();
    (path, conn)
}

fn fresh_db_at(path: &PathBuf, label: &str) -> Connection {
    memcore::db::enable_simple_auto_extension().ok();
    memcore::db::register_sqlite_vec();
    std::fs::create_dir_all(path.parent().expect("db parent")).unwrap();
    let mut conn = Connection::open(path).unwrap();
    memcore::db::init_schema_with_label_mut(
        &mut conn,
        label,
        path,
        &memcore::db::DbOpenContext::create_fresh(),
    )
    .unwrap();
    conn
}

fn open_ctx(path: &PathBuf, label: &str) -> DbContext {
    DbContext {
        label: label.to_string(),
        path: path.clone(),
        conn: Connection::open(path).unwrap(),
    }
}

fn insert_memory(
    conn: &Connection,
    id: &str,
    path: &str,
    text: &str,
    metadata: &str,
    retention: Option<&str>,
    source: Option<&str>,
) {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO memories (
            id, path, summary, text, importance, timestamp, category, topic,
            keywords, entities, source, scope, archived,
            created_at, updated_at, access_count, revision, metadata, retention_policy
         ) VALUES (?1, ?2, '', ?3, 0.5, ?4, 'fact', '',
                   '[]', '[]', ?5, 'project', 0,
                   ?4, ?4, 0, 1, ?6, ?7)",
        params![
            id,
            path,
            text,
            now,
            source.unwrap_or("manual"),
            metadata,
            retention,
        ],
    )
    .expect("insert");
}

mod domain;
mod fts_inventory;
mod memory_hygiene;
mod plan_c_restore;
mod quarantine_jobs_integrity;
mod refs_enrichment;
mod retention_junk;
