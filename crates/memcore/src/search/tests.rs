use super::*;
use crate::db::{
    add_edge, init_schema, register_sqlite_vec, search_fts, try_load_sqlite_vec, upsert,
};
use crate::types::{MemoryEdge, MemoryEntry};
use chrono::Utc;
use rusqlite::Connection;
use serde_json::json;

mod access;
mod anchor;
mod baseline;
mod config;
mod decay_policy;
mod expansion;
mod golden_corpus;
mod graph;
mod noise;
mod ops_audit_corpus;
mod phase_receipts;
mod supersession;
mod symbolic;

fn setup() -> Connection {
    crate::db::enable_simple_auto_extension().unwrap();
    register_sqlite_vec();
    let conn = Connection::open_in_memory().unwrap();
    init_schema(&conn).unwrap();
    try_load_sqlite_vec(&conn);
    conn
}

fn insert(conn: &mut Connection, id: &str, text: &str, keywords: &[&str]) {
    let e = memory_entry(id, text, keywords);
    upsert(conn, &e, false).unwrap();
}

fn insert_entry(conn: &mut Connection, entry: MemoryEntry) {
    upsert(conn, &entry, false).unwrap();
}

fn memory_entry(id: &str, text: &str, keywords: &[&str]) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/test".into(),
        summary: text.chars().take(30).collect(),
        text: text.into(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".into(),
        topic: "".into(),
        keywords: keywords.iter().map(|s| s.to_string()).collect(),
        persons: vec![],
        entities: vec![],
        location: "".into(),
        source: "".into(),
        scope: "general".into(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({ "keywords": keywords, "entities": [] }),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}
