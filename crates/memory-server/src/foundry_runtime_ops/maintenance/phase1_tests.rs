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

        let kinds: Vec<memory_core::FoundryJobKind> =
            specs.iter().map(|s| s.kind.clone()).collect();
        assert!(
            !kinds.contains(&memory_core::FoundryJobKind::MemoryDistill),
            "Phase 1: capture must not enqueue MemoryDistill (got {kinds:?})"
        );
        assert!(kinds.contains(&memory_core::FoundryJobKind::ForgetSweep));
        assert!(kinds.contains(&memory_core::FoundryJobKind::MemoryNeighborhood));
        assert!(
            !kinds.contains(&memory_core::FoundryJobKind::RecallRerankCache),
            "durable recall-cache jobs are opt-in; default capture specs should not enqueue no-op jobs (got {kinds:?})"
        );
    }

    {
        let _durable_cache = EnvGuard::set("TACHI_ENABLE_DURABLE_RECALL_CACHE", "1");
        let tmp = tempdir().expect("tempdir");
        let db_path = tmp.path().join("global.db");
        let server = crate::MemoryServer::new(db_path, None).expect("server");
        let specs = capture_maintenance_specs(&server, "agent", "/a/b", &memory_ids, 0, 0);

        let kinds: Vec<memory_core::FoundryJobKind> =
            specs.iter().map(|s| s.kind.clone()).collect();
        assert!(kinds.contains(&memory_core::FoundryJobKind::RecallRerankCache));
    }
}
