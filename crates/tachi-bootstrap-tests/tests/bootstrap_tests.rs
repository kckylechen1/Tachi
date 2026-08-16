use chrono::Utc;
use memcore::{MemoryEntry, MemoryStore};
use serde_json::json;
use std::collections::HashMap;
use tachi_server::bootstrap_test_api as api;

#[path = "bootstrap_tests/migration_execute.rs"]
mod migration_execute;
#[path = "bootstrap_tests/migration_plan.rs"]
mod migration_plan;
#[path = "bootstrap_tests/setup_report.rs"]
mod setup_report;
#[path = "bootstrap_tests/tidy_apply.rs"]
mod tidy_apply;
#[path = "bootstrap_tests/tidy_report.rs"]
mod tidy_report;

fn authorized_plan_sources(plan: &[api::TidyMigration]) -> api::AuthorizedMigrationSources {
    api::capture_migration_sources(plan)
        .expect("fixture migration sources must bind to physical objects")
}

fn test_fixture_path(name: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("tachi-bootstrap-tests")
        .join(format!("run-{}", std::process::id()))
        .join(name)
}

fn make_entry(id: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/".to_string(),
        summary: "".to_string(),
        text: "test memory".to_string(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        valid_from: String::new(),
        valid_until: None,
        category: "fact".to_string(),
        topic: "".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: "".to_string(),
        source: "test".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        scored_count: 0,
        last_access: None,
        last_use_at: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Phase 5: fragment-DB consolidation (`tachi tidy --execute`)
// ---------------------------------------------------------------------------

fn build_legacy_openclaw_fixture(
    root: &std::path::Path,
    rows_per_db: usize,
) -> (std::path::PathBuf, std::path::PathBuf) {
    // One legacy OpenClaw "core-extensions" agent DB that should migrate.
    let legacy_db = root
        .join(".openclaw")
        .join("core")
        .join("extensions")
        .join("tachi")
        .join("data")
        .join("agents")
        .join("legacy-one")
        .join("memory.db");
    let legacy_db2 = root
        .join(".openclaw")
        .join("core")
        .join("extensions")
        .join("memory-hybrid-bridge")
        .join("data")
        .join("agents")
        .join("legacy-two")
        .join("memory.db");

    for db in [&legacy_db, &legacy_db2] {
        std::fs::create_dir_all(db.parent().unwrap()).expect("create db parent");
        let mut store =
            MemoryStore::open(db.to_str().expect("db path utf8")).expect("open legacy db");
        // Use the parent directory name to keep ids unique across DBs (the
        // file name is just "memory.db" for both).
        let tag = db
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        for i in 0..rows_per_db {
            let id = format!("{tag}-row-{i}");
            store.upsert(&make_entry(&id)).expect("seed legacy db");
        }
    }
    (legacy_db, legacy_db2)
}
