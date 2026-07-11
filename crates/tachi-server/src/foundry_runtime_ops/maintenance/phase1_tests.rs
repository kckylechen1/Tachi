use super::*;
use tempfile::tempdir;

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn unset(key: &'static str) -> Self {
        let old = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, old }
    }

    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.old {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

/// Phase 1 regression: per-capture maintenance must not enqueue
/// `MemoryDistill`. Distill now runs only via the daily batch
/// scheduler (`run_daily_batch_distill`).
#[tokio::test]
async fn capture_specs_exclude_disabled_jobs_and_gate_recall_cache() {
    let memory_ids = vec!["m1".to_string(), "m2".to_string()];

    {
        let _durable_cache = EnvGuard::unset("TACHI_ENABLE_DURABLE_RECALL_CACHE");
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("global.db");
        let server = crate::MemoryServer::new(db_path, None).expect("server");
        let specs = capture_maintenance_specs(&server, "agent", "/a/b", &memory_ids, 0, 0);

        let kinds: Vec<memcore::FoundryJobKind> = specs.iter().map(|s| s.kind.clone()).collect();
        assert!(
            !kinds.contains(&memcore::FoundryJobKind::MemoryDistill),
            "Phase 1: capture must not enqueue MemoryDistill (got {kinds:?})"
        );
        assert!(kinds.contains(&memcore::FoundryJobKind::ForgetSweep));
        assert!(kinds.contains(&memcore::FoundryJobKind::MemoryNeighborhood));
        assert!(
            !kinds.contains(&memcore::FoundryJobKind::RecallRerankCache),
            "durable recall-cache jobs are opt-in; default capture specs should not enqueue no-op jobs (got {kinds:?})"
        );
    }

    {
        let _durable_cache = EnvGuard::set("TACHI_ENABLE_DURABLE_RECALL_CACHE", "1");
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("global.db");
        let server = crate::MemoryServer::new(db_path, None).expect("server");
        let specs = capture_maintenance_specs(&server, "agent", "/a/b", &memory_ids, 0, 0);

        let kinds: Vec<memcore::FoundryJobKind> = specs.iter().map(|s| s.kind.clone()).collect();
        assert!(kinds.contains(&memcore::FoundryJobKind::RecallRerankCache));
    }
}

#[tokio::test]
async fn unsupported_foundry_job_kinds_skip_with_explicit_reason() {
    let tmp = tempdir().expect("tempdir");
    let db_path = tmp.path().join("global.db");
    let server = crate::MemoryServer::new(db_path, None).expect("server");
    let job_id = "foundry-job:unsupported-session-ingest";
    let now = chrono::Utc::now().to_rfc3339();
    let job = memcore::FoundryJobSpec {
        id: job_id.to_string(),
        kind: memcore::FoundryJobKind::SessionIngest,
        lane: memcore::FoundryModelLane::Reasoning,
        status: memcore::FoundryJobStatus::Queued,
        target_agent_id: None,
        requested_by: None,
        created_at: now,
        evidence_count: 0,
        goal_count: 0,
        metadata: serde_json::json!({}),
    };
    let memory_ids = Vec::new();
    let persisted = memcore::PersistedFoundryJob {
        spec: job.clone(),
        target_db: crate::server_state::DbScope::Global.as_str().to_string(),
        named_project: None,
        path_prefix: "/scratch".to_string(),
        memory_ids: memory_ids.clone(),
    };
    server
        .with_store_for_scope(crate::server_state::DbScope::Global, |store| {
            memcore::insert_foundry_job(store.connection(), &persisted)
                .map_err(|e| format!("insert foundry job: {e}"))
        })
        .expect("insert unsupported job");

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tx.send(crate::foundry_runtime_ops::FoundryMaintenanceItem {
        job,
        target_db: crate::server_state::DbScope::Global,
        named_project: None,
        db_path: None,
        path_prefix: "/scratch".to_string(),
        memory_ids,
        counted_queue_slot: false,
    })
    .await
    .expect("send foundry job");
    drop(tx);

    run_foundry_maintenance_worker(server.clone(), rx).await;

    let (status, reason) = server
        .with_store_for_scope_read(crate::server_state::DbScope::Global, |store| {
            store
                .connection()
                .query_row(
                    "SELECT status, json_extract(metadata, '$.terminal_reason.reason')
                     FROM foundry_jobs
                     WHERE id = ?1",
                    rusqlite::params![job_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .map_err(|e| format!("load foundry terminal state: {e}"))
        })
        .expect("load terminal status");

    assert_eq!(status, "skipped");
    assert_eq!(
        reason.as_deref(),
        Some("unsupported_foundry_job_kind:session_ingest")
    );
}
