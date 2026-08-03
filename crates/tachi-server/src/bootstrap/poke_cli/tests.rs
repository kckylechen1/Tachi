use super::env::PokeEnvGuard;
use super::suite::run_poke_smoke_suite;
use crate::MemoryServer;
use serde_json::json;
use std::path::PathBuf;

#[tokio::test]
async fn poke_smoke_suite_writes_report_and_probe_artifacts() {
    let temp = tempfile::tempdir().expect("temp app home");
    let report = run_poke_smoke_suite(temp.path())
        .await
        .expect("poke smoke should pass");
    assert_eq!(
        report["status"],
        json!("passed"),
        "poke suite failed; probes={}",
        report.get("probes").cloned().unwrap_or_else(|| json!([]))
    );
    assert_eq!(report["summary"]["total"], json!(4));
    let run_dir = PathBuf::from(report["run_dir"].as_str().expect("run_dir"));
    assert!(run_dir.join("report.json").exists());
    assert!(run_dir.join("report.md").exists());
    for name in [
        "memory_basic",
        "skill_surface",
        "dispatch_mock",
        "verify_ledger",
    ] {
        assert!(run_dir.join("probes").join(format!("{name}.json")).exists());
    }
}

/// #1096 leaf-2a — pinned per the leaf's poke investigation: `PokeEnvGuard`
/// mutates process env (`TACHI_HOME` et al.) to redirect an isolated probe
/// suite into a sandbox, and `run_poke_smoke_suite` always constructs a
/// FRESH `MemoryServer` from scratch inside that guard's scope (never reuses
/// a pre-existing/daemon server instance) — so the server's home identity,
/// now resolved ONCE at construction and cached on `MemoryServer::home_dir`
/// instead of re-read from env per call, still lands on the guard's sandbox
/// path. This test constructs a server the same way `run_poke_smoke_suite`
/// does, directly inside a `PokeEnvGuard` scope, and asserts the cached
/// identity is the sandbox — not whatever ambient `TACHI_HOME` the rest of
/// the test process/host has. If a future change ever made `tachi_home()`
/// resolution happen once per PROCESS (e.g. a global `OnceLock`) instead of
/// once per SERVER, this test would catch that regression (poke's sandbox
/// redirection would silently stop working for any server built after the
/// first one in the process).
#[tokio::test]
async fn poke_env_guard_binds_fresh_server_home_to_sandbox() {
    let temp = tempfile::tempdir().expect("temp app home");
    let sandbox_home = temp.path().join("sandbox").join(".tachi");
    let sandbox_runs = sandbox_home.join("runs");
    std::fs::create_dir_all(&sandbox_runs).expect("sandbox runs dir");

    let _env = PokeEnvGuard::new(&sandbox_home, &sandbox_runs);

    let global_db = sandbox_home.join("global").join("memory.db");
    std::fs::create_dir_all(global_db.parent().expect("global db parent"))
        .expect("create global db parent");
    let server = MemoryServer::new(global_db, None).expect("isolated poke server");

    assert_eq!(
        server.tachi_home_dir(),
        sandbox_home,
        "MemoryServer constructed inside PokeEnvGuard's scope must bind its \
         cached home identity to the guard's sandbox redirection"
    );
}
