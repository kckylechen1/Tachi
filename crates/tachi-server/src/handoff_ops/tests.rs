use super::*;

fn env_lock() -> &'static std::sync::Mutex<()> {
    crate::shell_ops::tachi_run_root_env_lock()
}

fn test_server(db_path: std::path::PathBuf) -> MemoryServer {
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
        last_use_at: None,
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
