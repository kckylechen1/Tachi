use crate::MemoryServer;
use chrono::Utc;
use serde_json::{json, Value};
use std::path::Path;

use super::env::PokeEnvGuard;
use super::probes::{
    probe_arena_lifecycle, probe_dispatch_mock, probe_memory_basic, probe_skill_surface,
    probe_verify_ledger,
};
use super::report::{render_poke_report_markdown, write_text_file};

pub(super) async fn run_poke_smoke_suite(app_home: &Path) -> Result<Value, String> {
    let started_at = Utc::now().to_rfc3339();
    let run_id = format!(
        "poke_{}_{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().as_simple().to_string()[..8]
    );
    let run_dir = app_home.join("runs").join(&run_id);
    let probes_dir = run_dir.join("probes");
    let sandbox_home = run_dir.join("sandbox").join(".tachi");
    let sandbox_runs = sandbox_home.join("runs");
    let sandbox_project = run_dir.join("sandbox").join("project");
    tokio::fs::create_dir_all(&probes_dir)
        .await
        .map_err(|e| format!("create probes dir: {e}"))?;
    tokio::fs::create_dir_all(&sandbox_runs)
        .await
        .map_err(|e| format!("create sandbox runs: {e}"))?;
    tokio::fs::create_dir_all(&sandbox_project)
        .await
        .map_err(|e| format!("create sandbox project: {e}"))?;

    let _env = PokeEnvGuard::new(&sandbox_home, &sandbox_runs);
    let global_db = sandbox_home
        .join("global")
        .join(memcore::MEMORY_DB_FILENAME);
    let project_db = sandbox_home
        .join("project")
        .join(memcore::MEMORY_DB_FILENAME);
    tokio::fs::create_dir_all(global_db.parent().ok_or("global db path has no parent")?)
        .await
        .map_err(|e| format!("create global db parent: {e}"))?;
    tokio::fs::create_dir_all(project_db.parent().ok_or("project db path has no parent")?)
        .await
        .map_err(|e| format!("create project db parent: {e}"))?;
    let server = MemoryServer::new(global_db, Some(project_db))
        .map_err(|e| format!("create isolated Poke MemoryServer: {e}"))?;

    let mut probes = Vec::new();
    probes.push(
        run_probe("memory_basic", &probes_dir, || {
            Box::pin(probe_memory_basic(&server, &run_dir))
        })
        .await,
    );
    probes.push(
        run_probe("skill_surface", &probes_dir, || {
            Box::pin(probe_skill_surface(&server))
        })
        .await,
    );
    probes.push(
        run_probe("dispatch_mock", &probes_dir, || {
            Box::pin(probe_dispatch_mock(
                &server,
                &sandbox_project,
                &sandbox_home,
            ))
        })
        .await,
    );
    probes.push(
        run_probe("arena_lifecycle", &probes_dir, || {
            Box::pin(probe_arena_lifecycle(&server))
        })
        .await,
    );
    probes.push(
        run_probe("verify_ledger", &probes_dir, || {
            Box::pin(probe_verify_ledger(&server))
        })
        .await,
    );

    let passed = probes
        .iter()
        .filter(|probe| probe.get("status").and_then(Value::as_str) == Some("passed"))
        .count();
    let failed = probes.len().saturating_sub(passed);
    let overall = if failed == 0 { "passed" } else { "failed" };
    let report = json!({
        "suite": "smoke",
        "run_id": run_id,
        "status": overall,
        "started_at": started_at,
        "finished_at": Utc::now().to_rfc3339(),
        "run_dir": run_dir.to_string_lossy(),
        "sandbox_home": sandbox_home.to_string_lossy(),
        "summary": {
            "total": probes.len(),
            "passed": passed,
            "failed": failed,
        },
        "probes": probes,
        "non_goals": [
            "no broad Rust gates",
            "no GitHub writes",
            "no real model dispatch",
            "no mutation of the caller's memory DB"
        ],
    });
    crate::utils::write_json_file_owner_only(&run_dir.join("report.json"), &report)?;
    write_text_file(
        &run_dir.join("report.md"),
        &render_poke_report_markdown(&report),
    )?;
    Ok(report)
}

type ProbeFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + 'a>>;

async fn run_probe<'a, F>(name: &str, probes_dir: &Path, probe: F) -> Value
where
    F: FnOnce() -> ProbeFuture<'a>,
{
    let started_at = Utc::now().to_rfc3339();
    let result = probe().await;
    let mut payload = match result {
        Ok(mut value) => {
            if let Some(obj) = value.as_object_mut() {
                obj.entry("status".to_string())
                    .or_insert_with(|| json!("passed"));
                obj.entry("name".to_string()).or_insert_with(|| json!(name));
                obj.entry("started_at".to_string())
                    .or_insert_with(|| json!(started_at));
                value
            } else {
                json!({
                    "name": name,
                    "status": "passed",
                    "started_at": started_at,
                    "observed": value,
                })
            }
        }
        Err(error) => json!({
            "name": name,
            "status": "failed",
            "started_at": started_at,
            "error": error,
        }),
    };
    payload["finished_at"] = json!(Utc::now().to_rfc3339());
    let path = probes_dir.join(format!("{name}.json"));
    if let Err(error) = crate::utils::write_json_file_owner_only(&path, &payload) {
        payload["status"] = json!("failed");
        payload["write_error"] = json!(error);
    } else {
        payload["artifact"] = json!(path.to_string_lossy());
    }
    payload
}
