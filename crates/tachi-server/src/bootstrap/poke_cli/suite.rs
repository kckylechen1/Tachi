use crate::MemoryServer;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[cfg(test)]
use super::env::PokeEnvGuard;
use super::probes::{
    probe_dispatch_mock, probe_memory_basic, probe_skill_surface, probe_verify_ledger,
};
use super::report::{render_poke_report_markdown, write_text_file};

/// The suite's private sandbox under `app_home/runs`. Creating it needs no
/// runtime, so the caller can create the sandbox and take its environment
/// guard before any runtime worker exists.
pub(super) struct PokeSandbox {
    run_id: String,
    started_at: String,
    run_dir: PathBuf,
    probes_dir: PathBuf,
    pub(super) tachi_home: PathBuf,
    pub(super) run_root: PathBuf,
}

impl PokeSandbox {
    pub(super) fn create(app_home: &Path) -> Result<Self, String> {
        let started_at = memcore::time_compat::rfc3339_compat(jiff::Timestamp::now());
        let run_id = format!(
            "poke_{}_{}",
            memcore::time_compat::compact_basic(jiff::Timestamp::now()),
            &uuid::Uuid::new_v4().as_simple().to_string()[..8]
        );
        let run_dir = app_home.join("runs").join(&run_id);
        let probes_dir = run_dir.join("probes");
        let tachi_home = run_dir.join("sandbox").join(".tachi");
        let run_root = tachi_home.join("runs");
        let sandbox_project = run_dir.join("sandbox").join("project");
        for (path, what) in [
            (&probes_dir, "create probes dir"),
            (&run_root, "create sandbox runs"),
            (&sandbox_project, "create sandbox project"),
            (&tachi_home.join("global"), "create global db parent"),
            (&tachi_home.join("project"), "create project db parent"),
        ] {
            std::fs::create_dir_all(path).map_err(|e| format!("{what}: {e}"))?;
        }
        Ok(Self {
            run_id,
            started_at,
            run_dir,
            probes_dir,
            tachi_home,
            run_root,
        })
    }

    /// Runs the probes. The caller holds `PokeEnvGuard` for this sandbox.
    pub(super) async fn run(self) -> Result<Value, String> {
        let Self {
            run_id,
            started_at,
            run_dir,
            probes_dir,
            tachi_home: sandbox_home,
            run_root: _,
        } = self;
        let sandbox_project = run_dir.join("sandbox").join("project");
        let global_db = sandbox_home
            .join("global")
            .join(memcore::MEMORY_DB_FILENAME);
        let project_db = sandbox_home
            .join("project")
            .join(memcore::MEMORY_DB_FILENAME);
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
            "finished_at": memcore::time_compat::rfc3339_compat(jiff::Timestamp::now()),
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
}

/// Runs the suite under an environment guard taken on the caller's thread.
/// Tests use it on a current-thread runtime, which has no workers of its own;
/// the command takes the guard before its runtime exists instead.
#[cfg(test)]
pub(super) async fn run_poke_smoke_suite(app_home: &Path) -> Result<Value, String> {
    let sandbox = PokeSandbox::create(app_home)?;
    let _env = PokeEnvGuard::new(&sandbox.tachi_home, &sandbox.run_root);
    sandbox.run().await
}

type ProbeFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + 'a>>;

async fn run_probe<'a, F>(name: &str, probes_dir: &Path, probe: F) -> Value
where
    F: FnOnce() -> ProbeFuture<'a>,
{
    let started_at = memcore::time_compat::rfc3339_compat(jiff::Timestamp::now());
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
    payload["finished_at"] = json!(memcore::time_compat::rfc3339_compat(jiff::Timestamp::now()));
    let path = probes_dir.join(format!("{name}.json"));
    if let Err(error) = crate::utils::write_json_file_owner_only(&path, &payload) {
        payload["status"] = json!("failed");
        payload["write_error"] = json!(error);
    } else {
        payload["artifact"] = json!(path.to_string_lossy());
    }
    payload
}
