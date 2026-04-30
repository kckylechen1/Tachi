use super::*;

const HANDOFF_PATH: &str = "/handoff";
const HANDOFF_MEMORY_LIMIT: usize = 50;
const HANDOFF_DB_LIMIT: usize = 500;
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_agent_id(server: &MemoryServer) -> Option<String> {
    let guard = server
        .agent_profile
        .read()
        .unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .map(|profile| profile.agent_id.trim().to_string())
        .filter(|agent_id| !agent_id.is_empty())
}

fn fallback_agent_id(registered_agent: Option<String>) -> String {
    registered_agent
        .or_else(|| non_empty_env("TACHI_PROFILE"))
        .unwrap_or_else(|| "unknown-agent".to_string())
}

fn resolve_from_agent(server: &MemoryServer) -> String {
    fallback_agent_id(current_agent_id(server))
}

fn memo_matches_agent(memo: &HandoffMemo, agent_id: Option<&str>) -> bool {
    if memo.acknowledged {
        return false;
    }

    match (agent_id, memo.target_agent.as_deref()) {
        (_, None) => true,
        (Some(my_id), Some(target)) => my_id == target,
        (None, Some(_)) => true,
    }
}

fn memo_to_memory_entry(server: &MemoryServer, memo: &HandoffMemo) -> MemoryEntry {
    let memo_id = memo.id.clone();
    let mut metadata = crate::provenance::inject_provenance(
        server,
        serde_json::json!({
            "handoff_memo_id": memo_id,
            "handoff": memo,
            "status": "pending",
        }),
        "handoff_leave",
        "handoff_memo",
        Some("general"),
        DbScope::Global,
        serde_json::json!({
            "from_agent": memo.from_agent.clone(),
            "target_agent": memo.target_agent.clone(),
            "next_steps_count": memo.next_steps.len(),
        }),
    );
    // Handoff lives in the global DB but uses a non-/global path prefix; opt
    // in to cross-project routing so path-routing validation lets it through.
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            "allow_cross_project".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    let routed_path =
        memory_core::path_router::standardize_handoff_path(memo.target_agent.as_deref());

    MemoryEntry {
        id: format!("handoff:{}", memo_id),
        text: format!(
            "[Handoff from {}] {}\n\nNext steps:\n{}",
            memo.from_agent,
            memo.summary,
            memo.next_steps
                .iter()
                .enumerate()
                .map(|(i, s)| format!("{}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        category: "handoff".to_string(),
        importance: 0.9,
        summary: format!("Handoff from {}", memo.from_agent),
        path: routed_path,
        timestamp: memo.created_at.clone(),
        topic: "agent-handoff".to_string(),
        keywords: vec!["handoff".to_string(), memo.from_agent.clone()],
        persons: vec![],
        entities: vec![memo.from_agent.clone()],
        location: String::new(),
        source: "extraction".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        vector: None,
        metadata,
        retention_policy: None,
        domain: None,
    }
}

fn memo_from_entry(entry: &MemoryEntry) -> HandoffMemo {
    if let Some(memo) = entry
        .metadata
        .get("handoff")
        .and_then(|value| serde_json::from_value::<HandoffMemo>(value.clone()).ok())
    {
        let acknowledged = handoff_acknowledged(entry, &memo);
        return HandoffMemo {
            acknowledged,
            ..memo
        };
    }

    let metadata = entry.metadata.as_object();
    let from_agent = metadata
        .and_then(|m| m.get("provenance"))
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("from_agent"))
        .and_then(|v| v.as_str())
        .or_else(|| entry.entities.first().map(String::as_str))
        .unwrap_or("unknown-agent")
        .to_string();
    let target_agent = metadata
        .and_then(|m| m.get("provenance"))
        .and_then(|p| p.get("context"))
        .and_then(|c| c.get("target_agent"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let memo_id = metadata
        .and_then(|m| m.get("handoff_memo_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| entry.id.strip_prefix("handoff:").map(str::to_string))
        .unwrap_or_else(|| entry.id.clone());

    HandoffMemo {
        id: memo_id,
        from_agent,
        target_agent,
        summary: legacy_summary_from_entry(entry),
        next_steps: legacy_next_steps_from_entry(entry),
        context: None,
        created_at: entry.timestamp.clone(),
        acknowledged: handoff_acknowledged(entry, &empty_handoff_memo()),
    }
}

fn legacy_summary_from_entry(entry: &MemoryEntry) -> String {
    entry
        .text
        .strip_prefix("[Handoff from ")
        .and_then(|rest| rest.split_once("] "))
        .map(|(_, summary_and_steps)| {
            summary_and_steps
                .split_once("\n\nNext steps:")
                .map(|(summary, _)| summary)
                .unwrap_or(summary_and_steps)
                .trim()
                .to_string()
        })
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| entry.summary.clone())
}

fn legacy_next_steps_from_entry(entry: &MemoryEntry) -> Vec<String> {
    let Some((_, raw_steps)) = entry.text.split_once("\n\nNext steps:") else {
        return Vec::new();
    };

    raw_steps
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let Some((prefix, step)) = line.split_once(". ") else {
                return line.to_string();
            };
            if prefix.chars().all(|c| c.is_ascii_digit()) {
                step.trim().to_string()
            } else {
                line.to_string()
            }
        })
        .collect()
}

fn empty_handoff_memo() -> HandoffMemo {
    HandoffMemo {
        id: String::new(),
        from_agent: String::new(),
        target_agent: None,
        summary: String::new(),
        next_steps: Vec::new(),
        context: None,
        created_at: String::new(),
        acknowledged: false,
    }
}

fn handoff_acknowledged(entry: &MemoryEntry, memo: &HandoffMemo) -> bool {
    memo.acknowledged
        || entry.archived
        || entry
            .metadata
            .get("status")
            .and_then(|value| value.as_str())
            .is_some_and(|status| status == "acknowledged")
        || entry
            .metadata
            .get("acknowledged")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
}

fn pending_handoff_entries(store: &mut MemoryStore) -> Result<Vec<MemoryEntry>, String> {
    let mut entries = store
        .list_by_path(HANDOFF_PATH, HANDOFF_DB_LIMIT, false)
        .map_err(|e| format!("Failed to list handoff memories: {e}"))?;
    entries.retain(|entry| entry.category == "handoff" && !memo_from_entry(entry).acknowledged);
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(entries)
}

fn upsert_acknowledged_entry(
    store: &mut MemoryStore,
    mut entry: MemoryEntry,
    agent_id: Option<&str>,
) -> Result<(), String> {
    let acknowledged_at = Utc::now().to_rfc3339();
    let mut memo = memo_from_entry(&entry);
    memo.acknowledged = true;

    let metadata = entry
        .metadata
        .as_object_mut()
        .ok_or_else(|| "handoff metadata must be an object".to_string())?;
    metadata.insert("handoff".into(), json!(memo));
    metadata.insert("status".into(), json!("acknowledged"));
    metadata.insert("acknowledged".into(), json!(true));
    metadata.insert("acknowledged_at".into(), json!(acknowledged_at));
    if let Some(agent_id) = agent_id.filter(|value| !value.trim().is_empty()) {
        metadata.insert("acknowledged_by".into(), json!(agent_id));
    }

    entry.vector = None;
    store
        .upsert(&entry)
        .map_err(|e| format!("Failed to acknowledge handoff memory: {e}"))
}

pub(super) async fn handle_handoff_leave(
    server: &MemoryServer,
    params: HandoffLeaveParams,
) -> Result<String, String> {
    let from_agent = resolve_from_agent(server);

    let memo = HandoffMemo {
        id: uuid::Uuid::new_v4().to_string(),
        from_agent: from_agent.clone(),
        target_agent: params.target_agent,
        summary: params.summary,
        next_steps: params.next_steps,
        context: params.context,
        created_at: Utc::now().to_rfc3339(),
        acknowledged: false,
    };

    let memo_id = memo.id.clone();
    let memo_json = serde_json::to_string(&memo).map_err(|e| format!("serialize: {e}"))?;
    let entry = memo_to_memory_entry(server, &memo);
    server.with_global_store(|store| store.upsert(&entry).map_err(|e| format!("{e}")))?;

    let mut memos = server
        .handoff_memos
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    memos.push(memo);

    if memos.len() > HANDOFF_MEMORY_LIMIT {
        let drain_count = memos.len() - HANDOFF_MEMORY_LIMIT;
        memos.drain(..drain_count);
    }

    serde_json::to_string(&json!({
        "status": "memo_left",
        "memo_id": memo_id,
        "from_agent": from_agent,
        "memo": memo_json,
    }))
    .map_err(|e| format!("serialize: {e}"))
}

pub(super) async fn handle_handoff_check(
    server: &MemoryServer,
    params: HandoffCheckParams,
) -> Result<String, String> {
    let agent_id = params.agent_id.as_deref();
    let entries = server.with_global_store_read(pending_handoff_entries)?;
    let matching_entries: Vec<MemoryEntry> = entries
        .into_iter()
        .filter(|entry| memo_matches_agent(&memo_from_entry(entry), agent_id))
        .collect();
    let matching: Vec<HandoffMemo> = matching_entries.iter().map(memo_from_entry).collect();

    let result = serde_json::to_string(&json!({
        "pending_memos": matching.len(),
        "memos": matching,
    }))
    .map_err(|e| format!("serialize: {e}"))?;

    if params.acknowledge {
        for entry in matching_entries {
            server.with_global_store(|store| upsert_acknowledged_entry(store, entry, agent_id))?;
        }

        let mut memos = server
            .handoff_memos
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for memo in memos.iter_mut() {
            if memo_matches_agent(memo, agent_id) {
                memo.acknowledged = true;
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
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
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
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
        }
    }

    #[test]
    fn pending_handoff_entries_reads_persisted_memory() {
        let mut store = test_store();
        let memo = HandoffMemo {
            id: "memo-1".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: Some("agent-b".to_string()),
            summary: "persisted memo".to_string(),
            next_steps: vec!["continue".to_string()],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        };
        store.upsert(&test_entry(memo)).expect("upsert memo");

        let entries = pending_handoff_entries(&mut store).expect("pending entries");
        assert_eq!(entries.len(), 1);
        let memo = memo_from_entry(&entries[0]);
        assert_eq!(memo.id, "memo-1");
        assert_eq!(memo.from_agent, "agent-a");
        assert_eq!(memo.target_agent.as_deref(), Some("agent-b"));
    }

    #[test]
    fn acknowledge_updates_persisted_handoff_metadata() {
        let mut store = test_store();
        let memo = HandoffMemo {
            id: "memo-ack".to_string(),
            from_agent: "agent-a".to_string(),
            target_agent: Some("agent-b".to_string()),
            summary: "needs ack".to_string(),
            next_steps: vec!["ack".to_string()],
            context: None,
            created_at: Utc::now().to_rfc3339(),
            acknowledged: false,
        };
        let entry = test_entry(memo);
        store.upsert(&entry).expect("upsert memo");

        upsert_acknowledged_entry(&mut store, entry, Some("agent-b")).expect("ack memo");

        let pending = pending_handoff_entries(&mut store).expect("pending entries");
        assert!(pending.is_empty());
        let stored = store
            .get("handoff:memo-ack")
            .expect("get memo")
            .expect("memo exists");
        assert_eq!(stored.metadata["status"], json!("acknowledged"));
        assert_eq!(stored.metadata["acknowledged_by"], json!("agent-b"));
        assert_eq!(stored.metadata["handoff"]["acknowledged"], json!(true));
    }

    #[tokio::test]
    async fn handoff_check_reads_and_acks_persisted_memos_after_restart() {
        let db_path = std::env::temp_dir().join(format!(
            "handoff-persistence-{}.sqlite",
            uuid::Uuid::new_v4()
        ));

        {
            let server = test_server(db_path.clone());
            server
                .agent_register(Parameters(AgentRegisterParams {
                    agent_id: "agent-a".to_string(),
                    display_name: None,
                    capabilities: vec![],
                    tool_filter: None,
                    rate_limit_rpm: None,
                    rate_limit_burst: None,
                }))
                .await
                .expect("register source agent");
            server
                .handoff_leave(Parameters(HandoffLeaveParams {
                    summary: "persist across restart".to_string(),
                    next_steps: vec!["resume from db".to_string()],
                    target_agent: Some("agent-b".to_string()),
                    context: Some(json!({"file": "src/lib.rs"})),
                }))
                .await
                .expect("leave handoff");
        }

        let server = test_server(db_path.clone());
        let check = server
            .handoff_check(Parameters(HandoffCheckParams {
                agent_id: Some("agent-b".to_string()),
                acknowledge: true,
            }))
            .await
            .expect("check persisted handoff");
        let check_json: serde_json::Value = serde_json::from_str(&check).expect("check json");
        assert_eq!(check_json["pending_memos"], json!(1));
        assert_eq!(check_json["memos"][0]["from_agent"], json!("agent-a"));
        assert_eq!(
            check_json["memos"][0]["next_steps"],
            json!(["resume from db"])
        );

        let after = server
            .handoff_check(Parameters(HandoffCheckParams {
                agent_id: Some("agent-b".to_string()),
                acknowledge: false,
            }))
            .await
            .expect("check after ack");
        let after_json: serde_json::Value = serde_json::from_str(&after).expect("after json");
        assert_eq!(after_json["pending_memos"], json!(0));

        let stored = server
            .with_global_store_read(|store| {
                let entries = store
                    .list_by_path(HANDOFF_PATH, 10, false)
                    .map_err(|e| e.to_string())?;
                entries
                    .into_iter()
                    .next()
                    .ok_or_else(|| "missing handoff memory".to_string())
            })
            .expect("read stored handoff");
        assert_eq!(stored.metadata["status"], json!("acknowledged"));
        assert_eq!(stored.metadata["acknowledged_by"], json!("agent-b"));

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn resolve_from_agent_falls_back_to_profile_env_then_unknown() {
        let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original_profile = std::env::var_os("TACHI_PROFILE");
        std::env::remove_var("TACHI_PROFILE");
        assert_eq!(fallback_agent_id(None), "unknown-agent");

        std::env::set_var("TACHI_PROFILE", "antigravity");
        assert_eq!(fallback_agent_id(None), "antigravity");
        assert_eq!(
            fallback_agent_id(Some("registered".to_string())),
            "registered"
        );

        if let Some(profile) = original_profile {
            std::env::set_var("TACHI_PROFILE", profile);
        } else {
            std::env::remove_var("TACHI_PROFILE");
        }
    }
}
