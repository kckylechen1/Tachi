//! Read-only Hub registry inspection (formerly the standalone `tachi-hub` binary).
//! Invoked via `tachi hub <subcommand>`.
//! Extracted from memory-server (#833).

use std::path::{Path, PathBuf};

use tachi_bootstrap::cli::HubAction;

mod commands;

pub use self::commands::cmd_stats;
use self::commands::{cmd_bindings, cmd_doctor, cmd_list, cmd_packs, cmd_show};

pub(crate) fn expand_path(raw: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if raw == "~" {
        home
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

pub fn resolve_hub_db(db_override: Option<&Path>, app_home: &Path) -> PathBuf {
    if let Some(p) = db_override {
        return expand_path(p.to_string_lossy().as_ref());
    }
    if let Ok(p) = std::env::var("MEMORY_DB_PATH") {
        return expand_path(&p);
    }
    app_home.join("global/memory.db")
}

pub fn run(action: &HubAction, db_path: &PathBuf, app_home: &Path) -> Result<(), String> {
    if !matches!(action, HubAction::Doctor { .. }) && !db_path.exists() {
        return Err(format!(
            "DB not found: {}. Run `tachi setup` or set TACHI_HOME.",
            db_path.display()
        ));
    }

    match action {
        HubAction::List {
            cap_type,
            all,
            json: false,
        } => cmd_list(db_path, cap_type.as_deref(), *all).map_err(|e| e.to_string()),
        HubAction::List { json: true, .. } => Err("JSON hub list is handled in cli_tool".into()),
        HubAction::Show { id } => cmd_show(db_path, id).map_err(|e| e.to_string()),
        HubAction::Packs { all } => cmd_packs(db_path, *all).map_err(|e| e.to_string()),
        HubAction::Bindings => cmd_bindings(db_path).map_err(|e| e.to_string()),
        HubAction::Stats { json: false } => cmd_stats(db_path).map_err(|e| e.to_string()),
        HubAction::Doctor { fix } => cmd_doctor(app_home, *fix).map_err(|e| e.to_string()),
        HubAction::Register { .. }
        | HubAction::PackRegister { .. }
        | HubAction::PackProject { .. }
        | HubAction::Enable { .. }
        | HubAction::Disable { .. }
        | HubAction::Stats { json: true } => Err("handled by MemoryStore in cli_tool".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn doctor_vector_count_uses_memories_vec_table() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let db = dir.path().join("memory.db");
        let store = memory_core::MemoryStore::open(db.to_str().expect("db path"))
            .expect("open memory store");
        let now = Utc::now().to_rfc3339();

        store
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, '/facts/doctor-vector', 'missing vector', 'doctor vector diagnostic memory', 0.8, ?2, 'fact', 'doctor', '[]', '[]', 'manual', 'project', 0, ?2, ?2, 0, 1, '{}')",
                rusqlite::params!["doctor-vector-missing", now],
            )
            .expect("insert memory without vector");

        let missing =
            commands::count_memories_missing_vectors(store.connection()).expect("missing count");
        assert_eq!(missing, 1);
    }

    #[test]
    fn doctor_vector_count_ignores_recall_cache_rows() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let db = dir.path().join("memory.db");
        let store = memory_core::MemoryStore::open(db.to_str().expect("db path"))
            .expect("open memory store");
        let now = Utc::now().to_rfc3339();

        store
            .connection()
            .execute(
                "INSERT INTO memories
                 (id, path, summary, text, importance, timestamp, category, topic, keywords, entities, source, scope, archived, created_at, updated_at, access_count, revision, metadata)
                 VALUES (?1, '/recall-cache/test', 'cache', 'ephemeral recall cache diagnostic row', 0.3, ?2, 'other', 'recall', '[]', '[]', 'foundry_recall_rerank_cache', 'project', 0, ?2, ?2, 0, 1, '{}')",
                rusqlite::params!["recall-cache-without-vector", now],
            )
            .expect("insert recall cache without vector");

        let missing =
            commands::count_memories_missing_vectors(store.connection()).expect("missing count");
        assert_eq!(missing, 0);
    }
}
