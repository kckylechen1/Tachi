use super::*;
use tempfile::tempdir;

/// Phase 1 regression: per-capture maintenance must not enqueue
/// `MemoryDistill`. Distill now runs only via the daily batch
/// scheduler (`run_daily_batch_distill`).
#[tokio::test]
async fn capture_specs_exclude_memory_distill() {
    let tmp = tempdir().expect("tempdir");
    let db_path = tmp.path().join("global.db");
    let server = crate::MemoryServer::new(db_path, None).expect("server");

    let memory_ids = vec!["m1".to_string(), "m2".to_string()];
    let specs = capture_maintenance_specs(&server, "agent", "/a/b", &memory_ids, 0, 0);

    let kinds: Vec<memory_core::FoundryJobKind> = specs.iter().map(|s| s.kind.clone()).collect();
    assert!(
        !kinds.contains(&memory_core::FoundryJobKind::MemoryDistill),
        "Phase 1: capture must not enqueue MemoryDistill (got {kinds:?})"
    );
    assert!(kinds.contains(&memory_core::FoundryJobKind::ForgetSweep));
    assert!(kinds.contains(&memory_core::FoundryJobKind::MemoryNeighborhood));
    assert!(kinds.contains(&memory_core::FoundryJobKind::RecallRerankCache));
}
