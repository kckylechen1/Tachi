use crate::cli::PokeAction;
use crate::tool_params::{
    GetMemoryParams, RememberParams, SearchMemoryParams, TachiDispatchParams, TachiShellParams,
    TachiSkillParams, TachiVerifyParams,
};
use crate::MemoryServer;
use chrono::Utc;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub(super) async fn run_poke_command(
    app_home: &Path,
    action: PokeAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        PokeAction::Run { suite, json } => {
            if !suite.eq_ignore_ascii_case("smoke") {
                return Err(format!("unsupported Poke suite '{suite}' (expected smoke)").into());
            }
            let report = run_poke_smoke_suite(app_home).await?;
            let status = report.get("status").and_then(Value::as_str);
            if json {
                super::print_pretty_json(&report)?;
            } else {
                print_poke_summary(&report);
            }
            if status != Some("passed") {
                let run_dir = report
                    .get("run_dir")
                    .and_then(Value::as_str)
                    .unwrap_or("(unknown)");
                return Err(format!("Poke smoke suite failed; see {run_dir}").into());
            }
            Ok(())
        }
    }
}

pub(crate) async fn run_poke_smoke_suite(app_home: &Path) -> Result<Value, String> {
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
    std::fs::create_dir_all(&probes_dir).map_err(|e| format!("create probes dir: {e}"))?;
    std::fs::create_dir_all(&sandbox_runs).map_err(|e| format!("create sandbox runs: {e}"))?;
    std::fs::create_dir_all(&sandbox_project)
        .map_err(|e| format!("create sandbox project: {e}"))?;

    let _env = PokeEnvGuard::new(&sandbox_home, &sandbox_runs);
    let global_db = sandbox_home.join("global").join("memory.db");
    let project_db = sandbox_home.join("project").join("memory.db");
    std::fs::create_dir_all(global_db.parent().ok_or("global db path has no parent")?)
        .map_err(|e| format!("create global db parent: {e}"))?;
    std::fs::create_dir_all(project_db.parent().ok_or("project db path has no parent")?)
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
        run_probe("shell_artifact", &probes_dir, || {
            Box::pin(probe_shell_artifact(&server))
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
    write_json_file(&run_dir.join("report.json"), &report)?;
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
    if let Err(error) = write_json_file(&path, &payload) {
        payload["status"] = json!("failed");
        payload["write_error"] = json!(error);
    } else {
        payload["artifact"] = json!(path.to_string_lossy());
    }
    payload
}

async fn probe_memory_basic(server: &MemoryServer, run_dir: &Path) -> Result<Value, String> {
    let marker = format!("poke_{}", uuid::Uuid::new_v4().as_simple());
    let fact = format!("{marker} isolated memory probe fact");
    let saved_raw = crate::memory_search_ops::handle_remember(
        server,
        RememberParams {
            text: fact.clone(),
            summary: "Poke isolated memory probe".to_string(),
            tags: vec!["poke".to_string(), "smoke".to_string()],
            topic: "poke-memory-basic".to_string(),
            importance: Some(0.2),
            scope: Some("project".to_string()),
            project: None,
            path: Some("/scratch/poke/memory-basic".to_string()),
            category: Some("fact".to_string()),
            domain: Some("engineering".to_string()),
            retention_policy: None,
            valid_from: None,
            valid_until: None,
            force: true,
        },
    )
    .await?;
    let saved: Value = serde_json::from_str(&saved_raw).map_err(|e| format!("parse save: {e}"))?;
    let id = saved
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("remember response lacks id: {saved}"))?;
    let search_raw = crate::memory_search_ops::handle_search_memory(
        server,
        SearchMemoryParams {
            query: marker.clone(),
            query_vec: None,
            top_k: 5,
            path_prefix: Some("/scratch/poke".to_string()),
            include_training: false,
            include_archived: false,
            candidates_per_channel: 10,
            mmr_threshold: None,
            graph_expand_hops: 0,
            graph_relation_filter: None,
            weights: None,
            agent_role: None,
            project: None,
            domain: Some("engineering".to_string()),
            file_context: None,
            error_context: None,
            enable_rerank: false,
            as_of: None,
            include_metadata: false,
        },
        false,
    )
    .await?;
    let search: Value =
        serde_json::from_str(&search_raw).map_err(|e| format!("parse search: {e}"))?;
    let hits = search
        .as_array()
        .cloned()
        .or_else(|| {
            search
                .get("results")
                .or_else(|| search.get("memories"))
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default();
    let found = hits.iter().any(|hit| {
        hit.get("id").and_then(Value::as_str) == Some(id)
            || hit
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains(&marker))
    });
    let get_raw = crate::memory_ops::handle_get_memory(
        server,
        GetMemoryParams {
            id: id.to_string(),
            include_archived: false,
            project: None,
        },
    )
    .await?;
    let get: Value = serde_json::from_str(&get_raw).map_err(|e| format!("parse get: {e}"))?;
    let exact = get
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| text == fact);
    if !found || !exact {
        return Err(format!(
            "memory probe failed: found={found} exact={exact} saved={saved} search={search} get={get}"
        ));
    }
    Ok(json!({
        "name": "memory_basic",
        "status": "passed",
        "expected": "save/search/get exact isolated poke_ fact",
        "observed": {
            "id": id,
            "search_hit": found,
            "exact_get": exact,
        },
        "cleanup": {
            "mode": "isolated_sandbox_retained_for_audit",
            "run_dir": run_dir.to_string_lossy(),
        },
        "repro_steps": [
            "remember poke_ fact in isolated project DB",
            "search /scratch/poke for marker",
            "get saved id and compare exact text"
        ],
    }))
}

async fn probe_skill_surface(server: &MemoryServer) -> Result<Value, String> {
    let discover_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "discover".to_string(),
            query: Some("waza check verification".to_string()),
            cap_type: Some("skill".to_string()),
            enabled_only: Some(true),
            limit: Some(12),
            skill_id: None,
            args: None,
            profile: None,
            host: None,
            skill_limit: None,
            capability_limit: None,
            pack_limit: None,
            include_section: None,
        }))
        .await?;
    let discover: Value =
        serde_json::from_str(&discover_raw).map_err(|e| format!("parse skill discover: {e}"))?;
    let loadout_raw = server
        .tachi_skill(Parameters(TachiSkillParams {
            action: "loadout".to_string(),
            query: None,
            cap_type: None,
            enabled_only: None,
            limit: None,
            skill_id: None,
            args: None,
            profile: Some("codex_55_review".to_string()),
            host: Some("codex".to_string()),
            skill_limit: Some(8),
            capability_limit: Some(8),
            pack_limit: Some(4),
            include_section: Some(true),
        }))
        .await?;
    let loadout: Value =
        serde_json::from_str(&loadout_raw).map_err(|e| format!("parse skill loadout: {e}"))?;
    let skills = collect_strings(&loadout);
    let has_waza = skills.iter().any(|item| item.contains("skill:waza-"));
    let has_superpower = skills
        .iter()
        .any(|item| item.contains("skill:superpowers-"));
    if !has_waza || !has_superpower {
        return Err(format!(
            "skill surface missing builtin categories: has_waza={has_waza} has_superpower={has_superpower} loadout={loadout}"
        ));
    }
    Ok(json!({
        "name": "skill_surface",
        "status": "passed",
        "expected": "discover/list builtin skills and render a safe loadout contract",
        "observed": {
            "discover_count": discover.get("results").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            "has_waza": has_waza,
            "has_superpowers": has_superpower,
            "loadout_profile": "codex_55_review",
        },
        "repro_steps": [
            "tachi_skill discover query='waza check verification'",
            "tachi_skill loadout profile=codex_55_review host=codex"
        ],
    }))
}

async fn probe_shell_artifact(server: &MemoryServer) -> Result<Value, String> {
    let raw = crate::shell_ops::handle_tachi_shell(
        server,
        TachiShellParams {
            action: "plan".to_string(),
            format: Some("json".to_string()),
            flow_id: None,
            task: Some("Poke smoke shell artifact probe".to_string()),
            title: Some("poke shell artifact".to_string()),
            agent: None,
            profile: None,
            cwd: None,
            tool_profile: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
            async_dispatch: false,
            project: None,
            state_filter: None,
            limit: None,
            notes: Some(
                "Poke probe: verify instruction/status/injected SOP artifacts.".to_string(),
            ),
            validation: vec!["echo poke-shell".to_string()],
            allowed_scope: vec!["sandbox".to_string()],
            slices: Vec::new(),
        },
    )
    .await?;
    let response: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse shell response: {e}"))?;
    let run_dir = response
        .get("run_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| format!("shell response lacks run_dir: {response}"))?;
    let instruction = response
        .get("instruction_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| format!("shell response lacks instruction_path: {response}"))?;
    let status_path = run_dir.join("status.json");
    let injected_path = response
        .get("injected_skill")
        .and_then(|value| value.get("injected_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let injected_ok = injected_path.as_ref().is_some_and(|path| path.exists());
    if !instruction.exists() || !status_path.exists() || !injected_ok {
        return Err(format!(
            "shell artifacts missing: instruction={} status={} injected_ok={} response={response}",
            instruction.exists(),
            status_path.exists(),
            injected_ok
        ));
    }
    Ok(json!({
        "name": "shell_artifact",
        "status": "passed",
        "expected": "shell stage writes instruction.md, status.json, and injected SOP artifact",
        "observed": {
            "flow_id": response.get("flow_id").cloned().unwrap_or(Value::Null),
            "run_dir": run_dir.to_string_lossy(),
            "instruction_path": instruction.to_string_lossy(),
            "status_path": status_path.to_string_lossy(),
            "injected_path": injected_path.map(|path| json!(path.to_string_lossy())).unwrap_or(Value::Null),
        },
        "repro_steps": [
            "tachi_shell action=plan in isolated TACHI_RUN_ROOT",
            "check instruction.md/status.json/injected SOP artifact"
        ],
    }))
}

async fn probe_dispatch_mock(
    server: &MemoryServer,
    cwd: &Path,
    sandbox_home: &Path,
) -> Result<Value, String> {
    let raw = server
        .tachi_dispatch(Parameters(TachiDispatchParams {
            agent: Some("custom".to_string()),
            profile: None,
            task: "Poke no-op mock dispatch".to_string(),
            cwd: Some(cwd.to_string_lossy().to_string()),
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: 10,
            permission_profile: None,
            allowed_tools: Vec::new(),
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: vec![
                "python3".to_string(),
                "-c".to_string(),
                "import sys; print('poke dispatch mock ok')".to_string(),
            ],
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: Some("explore".to_string()),
            credential_profiles: Vec::new(),
            issue_ref: None,
            pr_ref: None,
            flow_id: None,
            tool_profile: Some("observe".to_string()),
            auto_capability_bundle: Some(true),
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        }))
        .await?;
    let response: Value =
        serde_json::from_str(&raw).map_err(|e| format!("parse dispatch response: {e}"))?;
    let dispatch_id = response
        .get("dispatch_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("dispatch response lacks dispatch_id: {response}"))?;
    let run_dir = sandbox_home.join("runs").join(dispatch_id);
    wait_for_file(&run_dir.join("result.md"), Duration::from_secs(10)).await?;
    let required = [
        "prompt.md",
        "context.md",
        "capability_bundle.json",
        "trajectory.jsonl",
        "status.json",
    ];
    let missing = required
        .iter()
        .filter(|name| !run_dir.join(name).exists())
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "dispatch mock missing artifacts {missing:?} in {}",
            run_dir.display()
        ));
    }
    let result = std::fs::read_to_string(run_dir.join("result.md"))
        .map_err(|e| format!("read dispatch result: {e}"))?;
    if !result.contains("poke dispatch mock ok") {
        return Err(format!(
            "dispatch result did not contain mock output: {result}"
        ));
    }
    Ok(json!({
        "name": "dispatch_mock",
        "status": "passed",
        "expected": "no-op custom dispatch writes canonical run artifacts",
        "observed": {
            "dispatch_id": dispatch_id,
            "run_dir": run_dir.to_string_lossy(),
            "result_tail": result.chars().take(120).collect::<String>(),
            "artifacts": required,
        },
        "repro_steps": [
            "tachi_dispatch agent=custom command='python3 -c ...'",
            "wait for result.md",
            "verify prompt/context/capability_bundle/trajectory/status artifacts"
        ],
    }))
}

async fn probe_verify_ledger(server: &MemoryServer) -> Result<Value, String> {
    let flow_id = format!(
        "flow_{}_poke_verify_{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().as_simple().to_string()[..8]
    );
    let unrelated_flow_id = format!(
        "flow_{}_poke_unrelated_{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().as_simple().to_string()[..8]
    );
    let record = server
        .tachi_verify(Parameters(TachiVerifyParams {
            action: "record".to_string(),
            format: Some("json".to_string()),
            flow_id: Some(flow_id.clone()),
            pr_ref: None,
            head_sha: Some("poke-head".to_string()),
            check_id: Some("poke-ledger".to_string()),
            kind: Some("custom".to_string()),
            command: Some("poke ledger probe".to_string()),
            commands: Vec::new(),
            status: Some("passed".to_string()),
            exit_code: Some(0),
            log_path: None,
            summary: Some("poke ledger passed".to_string()),
            cwd: None,
            required: Some(true),
            limit: None,
        }))
        .await?;
    let status = server
        .tachi_verify(Parameters(TachiVerifyParams {
            action: "status".to_string(),
            format: Some("json".to_string()),
            flow_id: Some(flow_id.clone()),
            pr_ref: None,
            head_sha: Some("poke-head".to_string()),
            check_id: None,
            kind: None,
            command: None,
            commands: Vec::new(),
            status: None,
            exit_code: None,
            log_path: None,
            summary: None,
            cwd: None,
            required: None,
            limit: None,
        }))
        .await?;
    let unrelated = server
        .tachi_verify(Parameters(TachiVerifyParams {
            action: "status".to_string(),
            format: Some("json".to_string()),
            flow_id: Some(unrelated_flow_id.clone()),
            pr_ref: None,
            head_sha: Some("poke-head".to_string()),
            check_id: None,
            kind: None,
            command: None,
            commands: Vec::new(),
            status: None,
            exit_code: None,
            log_path: None,
            summary: None,
            cwd: None,
            required: None,
            limit: None,
        }))
        .await?;
    let record_json: Value =
        serde_json::from_str(&record).map_err(|e| format!("parse verify record: {e}"))?;
    let status_json: Value =
        serde_json::from_str(&status).map_err(|e| format!("parse verify status: {e}"))?;
    let unrelated_json: Value =
        serde_json::from_str(&unrelated).map_err(|e| format!("parse unrelated verify: {e}"))?;
    let overall = status_json
        .get("verification")
        .and_then(|value| value.get("overall"))
        .and_then(Value::as_str);
    let unrelated_items = unrelated_json
        .get("verification")
        .and_then(|value| value.get("items"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if overall != Some("passed") || unrelated_items != 0 {
        return Err(format!(
            "verification ledger probe failed: overall={overall:?} unrelated_items={unrelated_items} status={status_json} unrelated={unrelated_json}"
        ));
    }
    Ok(json!({
        "name": "verify_ledger",
        "status": "passed",
        "expected": "current flow verification passes and unrelated flow evidence is not reused",
        "observed": {
            "flow_id": flow_id,
            "record": record_json,
            "overall": overall,
            "unrelated_flow_id": unrelated_flow_id,
            "unrelated_items": unrelated_items,
        },
        "repro_steps": [
            "tachi_verify record flow_id=<current>",
            "tachi_verify status flow_id=<current>",
            "tachi_verify status flow_id=<unrelated>"
        ],
    }))
}

async fn wait_for_file(path: &Path, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "timed out after {}s waiting for {}",
        timeout.as_secs(),
        path.display()
    ))
}

fn collect_strings(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_strings_inner(value, &mut out);
    out
}

fn collect_strings_inner(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                collect_strings_inner(item, out);
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                collect_strings_inner(item, out);
            }
        }
        _ => {}
    }
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    let raw = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize json: {e}"))?;
    crate::utils::write_owner_only_file_atomic(path, &raw)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn write_text_file(path: &Path, text: &str) -> Result<(), String> {
    crate::utils::write_owner_only_file_atomic(path, text.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn render_poke_report_markdown(report: &Value) -> String {
    let status = report
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let run_id = report
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut out = format!("# Poke Smoke Report\n\n- run_id: `{run_id}`\n- status: `{status}`\n\n");
    out.push_str("## Probes\n\n");
    if let Some(probes) = report.get("probes").and_then(Value::as_array) {
        for probe in probes {
            let name = probe.get("name").and_then(Value::as_str).unwrap_or("probe");
            let status = probe
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let artifact = probe
                .get("artifact")
                .and_then(Value::as_str)
                .unwrap_or("(not written)");
            out.push_str(&format!("- `{name}`: `{status}` ({artifact})\n"));
        }
    }
    out
}

fn print_poke_summary(report: &Value) {
    println!(
        "Poke smoke: {}",
        report
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "run_dir: {}",
        report
            .get("run_dir")
            .and_then(Value::as_str)
            .unwrap_or("(unknown)")
    );
    if let Some(probes) = report.get("probes").and_then(Value::as_array) {
        for probe in probes {
            println!(
                "- {}: {}",
                probe.get("name").and_then(Value::as_str).unwrap_or("probe"),
                probe
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            );
        }
    }
}

struct PokeEnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    original_tachi_home: Option<std::ffi::OsString>,
    original_tachi_run_root: Option<std::ffi::OsString>,
    original_search_disable_query_embedding: Option<std::ffi::OsString>,
}

impl PokeEnvGuard {
    fn new(tachi_home: &Path, run_root: &Path) -> Self {
        let lock = poke_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_tachi_home = std::env::var_os("TACHI_HOME");
        let original_tachi_run_root = std::env::var_os("TACHI_RUN_ROOT");
        let original_search_disable_query_embedding =
            std::env::var_os("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING");
        std::env::set_var("TACHI_HOME", tachi_home);
        std::env::set_var("TACHI_RUN_ROOT", run_root);
        std::env::set_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
        Self {
            _lock: lock,
            original_tachi_home,
            original_tachi_run_root,
            original_search_disable_query_embedding,
        }
    }
}

fn poke_env_lock() -> &'static std::sync::Mutex<()> {
    #[cfg(test)]
    {
        crate::shell_ops::tachi_run_root_env_lock()
    }
    #[cfg(not(test))]
    {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }
}

impl Drop for PokeEnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.original_tachi_home.as_ref() {
            std::env::set_var("TACHI_HOME", value);
        } else {
            std::env::remove_var("TACHI_HOME");
        }
        if let Some(value) = self.original_tachi_run_root.as_ref() {
            std::env::set_var("TACHI_RUN_ROOT", value);
        } else {
            std::env::remove_var("TACHI_RUN_ROOT");
        }
        if let Some(value) = self.original_search_disable_query_embedding.as_ref() {
            std::env::set_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", value);
        } else {
            std::env::remove_var("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(report["status"], json!("passed"));
        assert_eq!(report["summary"]["total"], json!(5));
        let run_dir = PathBuf::from(report["run_dir"].as_str().expect("run_dir"));
        assert!(run_dir.join("report.json").exists());
        assert!(run_dir.join("report.md").exists());
        for name in [
            "memory_basic",
            "skill_surface",
            "shell_artifact",
            "dispatch_mock",
            "verify_ledger",
        ] {
            assert!(run_dir.join("probes").join(format!("{name}.json")).exists());
        }
    }
}
