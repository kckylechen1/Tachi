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
use super::plan_c::PlanCRepair;
use super::quarantine::QuarantineSweep;
use super::retention::RetentionBackfill;
use super::{DbContext, RepairRule};

fn fresh_db(dir: &TempDir, name: &str) -> (PathBuf, Connection) {
    // FTS5 + the `simple` tokenizer is required by `init_schema`.
    libsimple::enable_auto_extension().ok();
    memory_core::db::register_sqlite_vec();
    let path = dir.path().join(name);
    let mut conn = Connection::open(&path).unwrap();
    memory_core::db::init_schema_with_label_mut(&mut conn, "test", &path).unwrap();
    (path, conn)
}

fn fresh_db_at(path: &PathBuf, label: &str) -> Connection {
    libsimple::enable_auto_extension().ok();
    memory_core::db::register_sqlite_vec();
    std::fs::create_dir_all(path.parent().expect("db parent")).unwrap();
    let mut conn = Connection::open(path).unwrap();
    memory_core::db::init_schema_with_label_mut(&mut conn, label, path).unwrap();
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
mod plan_c_restore;
mod quarantine_jobs_integrity;
mod refs_enrichment;
mod retention_junk;
