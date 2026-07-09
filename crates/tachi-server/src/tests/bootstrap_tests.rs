use super::make_entry;
use memcore::MemoryStore;
use std::collections::HashMap;

mod migration_execute;
mod migration_plan;
mod setup_report;
mod tidy_apply;
mod tidy_report;

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
