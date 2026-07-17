use super::*;

fn ensure_test_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("VOYAGE_API_KEY", "test-voyage-key");
        std::env::set_var("SILICONFLOW_API_KEY", "test-siliconflow-key");
        std::env::set_var("SILICONFLOW_MODEL", "test-model");
        std::env::set_var("SUMMARY_MODEL", "test-summary-model");
    });
}

fn env_lock() -> &'static std::sync::Mutex<()> {
    crate::shell_ops::tachi_run_root_env_lock()
}

fn test_server(db_path: std::path::PathBuf) -> MemoryServer {
    ensure_test_env();
    MemoryServer::new(db_path, None).expect("test memory server")
}

fn test_store() -> MemoryStore {
    MemoryStore::open_in_memory().expect("test memory store")
}

fn test_entry(memo: HandoffMemo) -> MemoryEntry {
    MemoryEntry {
        id: format!("handoff:{}", memo.id),
        path: HANDOFF_PATH.to_string(),
        summary: memo.summary.clone(),
        text: memo.summary.clone(),
        importance: 0.9,
        timestamp: memo.created_at.clone(),
        valid_from: String::new(),
        valid_until: None,
        category: "handoff".to_string(),
        topic: "agent-handoff".to_string(),
        keywords: vec!["handoff".to_string()],
        persons: vec![],
        entities: vec![memo.from_agent.clone()],
        location: String::new(),
        source: "test".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({
            "handoff_memo_id": memo.id,
            "handoff": memo,
            "status": "pending",
        }),
        vector: None,
        retention_policy: None,
        domain: None,
        recall_count: 0,
        query_diversity: 0,
        tier: "raw".to_string(),
    }
}

mod agent_resolution;
mod promotion;
