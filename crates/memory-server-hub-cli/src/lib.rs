//! Read-only Hub registry inspection (formerly the standalone `tachi-hub` binary).
//! Invoked via `tachi hub <subcommand>`.
//! Extracted from tachi-server (#833).

use std::path::{Path, PathBuf};

use tachi_bootstrap::cli::HubAction;

mod commands;

use self::commands::{cmd_bindings, cmd_doctor, cmd_list, cmd_show};
pub use self::commands::{
    cmd_list_filtered, cmd_stats, cmd_stats_filtered, collect_list_filtered_with,
    collect_stats_filtered, HubStatsSnapshot,
};

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
    app_home.join("global").join(memcore::MEMORY_DB_FILENAME)
}

pub fn run(action: &HubAction, db_path: &Path, app_home: &Path) -> Result<(), String> {
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
        HubAction::Bindings => cmd_bindings(db_path).map_err(|e| e.to_string()),
        HubAction::Stats { json: false } => cmd_stats(db_path).map_err(|e| e.to_string()),
        HubAction::Doctor { fix } => cmd_doctor(app_home, *fix).map_err(|e| e.to_string()),
        HubAction::Register { .. }
        | HubAction::Enable { .. }
        | HubAction::Disable { .. }
        | HubAction::Stats { json: true } => Err("handled by MemoryStore in cli_tool".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use memcore::MemoryEntry;

    fn test_entry(id: &str, path: &str, summary: &str, text: &str, source: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: path.to_string(),
            summary: summary.to_string(),
            text: text.to_string(),
            importance: 0.8,
            timestamp: Utc::now().to_rfc3339(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: "doctor".to_string(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: source.to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn doctor_vector_count_uses_memories_vec_table() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let db = dir.path().join("memory.db");
        let mut store =
            memcore::MemoryStore::open(db.to_str().expect("db path")).expect("open memory store");
        store
            .upsert(&test_entry(
                "doctor-vector-missing",
                "/facts/doctor-vector",
                "missing vector",
                "doctor vector diagnostic memory",
                "manual",
            ))
            .expect("insert memory without vector");

        let missing =
            commands::count_memories_missing_vectors(store.connection()).expect("missing count");
        assert_eq!(missing, 1);
    }

    #[test]
    fn doctor_vector_count_ignores_recall_cache_rows() {
        let dir = tempfile::tempdir().expect("temp db dir");
        let db = dir.path().join("memory.db");
        let mut store =
            memcore::MemoryStore::open(db.to_str().expect("db path")).expect("open memory store");
        store
            .upsert(&test_entry(
                "recall-cache-without-vector",
                "/recall-cache/test",
                "cache",
                "ephemeral recall cache diagnostic row",
                "foundry_recall_rerank_cache",
            ))
            .expect("insert recall cache without vector");

        let missing =
            commands::count_memories_missing_vectors(store.connection()).expect("missing count");
        assert_eq!(missing, 0);
    }
}
