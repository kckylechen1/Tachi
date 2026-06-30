use crate::tool_params::TachiDispatchParams;
use crate::MemoryServer;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use super::util::wait_for_file;

pub(in crate::bootstrap::poke_cli) async fn probe_dispatch_mock(
    server: &MemoryServer,
    cwd: &Path,
    sandbox_home: &Path,
) -> Result<Value, String> {
    let raw = crate::dispatch_ops::handle_tachi_dispatch(
        server,
        TachiDispatchParams {
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
        },
    )
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
    let result = tokio::fs::read_to_string(run_dir.join("result.md"))
        .await
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
            "tachi_task(action='dispatch', agent='custom', command=['python3','-c',...])",
            "wait for result.md",
            "verify prompt/context/capability_bundle/trajectory/status artifacts"
        ],
    }))
}
