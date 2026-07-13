use super::suite::run_poke_smoke_suite;
use serde_json::json;
use std::path::PathBuf;

#[tokio::test]
async fn poke_smoke_suite_writes_report_and_probe_artifacts() {
    std::env::set_var("VOYAGE_API_KEY", "test-voyage-key");
    std::env::set_var("SILICONFLOW_API_KEY", "test-siliconflow-key");
    std::env::set_var("TACHI_TEST_DISABLE_PROVIDER_KEY_HEALTH_PERSIST", "1");
    std::env::set_var("TACHI_DISABLE_PATH_VALIDATION", "1");
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
    assert_eq!(report["summary"]["total"], json!(6));
    let run_dir = PathBuf::from(report["run_dir"].as_str().expect("run_dir"));
    assert!(run_dir.join("report.json").exists());
    assert!(run_dir.join("report.md").exists());
    for name in [
        "memory_basic",
        "skill_surface",
        "shell_artifact",
        "dispatch_mock",
        "arena_lifecycle",
        "verify_ledger",
    ] {
        assert!(run_dir.join("probes").join(format!("{name}.json")).exists());
    }
}
