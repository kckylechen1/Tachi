use crate::tool_params::TachiArenaParams;
use crate::MemoryServer;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::{json, Value};
use std::path::PathBuf;

fn arena_params(action: &str) -> TachiArenaParams {
    TachiArenaParams {
        action: action.to_string(),
        format: Some("json".to_string()),
        arena_id: None,
        mission_id: None,
        title: None,
        objective: None,
        prompt: None,
        harness: None,
        role: None,
        cwd: None,
        skills: Vec::new(),
        scope: Vec::new(),
        permissions: Vec::new(),
        timeout_secs: None,
        launch: false,
        dispatch_reason: None,
        profile: None,
        model: None,
        project: None,
        flow_id: None,
        issue_ref: None,
        pr_ref: None,
        permission_profile: None,
        sandbox: None,
        credential_profiles: Vec::new(),
        tool_profile: None,
        auto_capability_bundle: None,
        reason: None,
        dry_run: None,
        force: false,
        require_collected: None,
    }
}

fn parse_json(label: &str, raw: &str) -> Result<Value, String> {
    serde_json::from_str(raw).map_err(|e| format!("parse arena {label}: {e}; raw={raw}"))
}

pub(in crate::bootstrap::poke_cli) async fn probe_arena_lifecycle(
    server: &MemoryServer,
) -> Result<Value, String> {
    let mut open = arena_params("open");
    open.title = Some("Poke arena lifecycle".to_string());
    open.objective = Some("Verify tachi_arena open/spawn/board/collect/close.".to_string());
    let opened = parse_json("open", &server.tachi_arena(Parameters(open)).await?)?;
    let arena_id = opened
        .get("arena_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("open response lacks arena_id: {opened}"))?
        .to_string();
    let arena_dir = PathBuf::from(
        opened
            .get("arena_dir")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("open response lacks arena_dir: {opened}"))?,
    );
    if !arena_dir.join("manifest.json").exists() || !arena_dir.join("board.json").exists() {
        return Err(format!(
            "arena open did not create manifest/board in {}",
            arena_dir.display()
        ));
    }

    let mut spawn = arena_params("spawn");
    spawn.arena_id = Some(arena_id.clone());
    spawn.role = Some("critic".to_string());
    spawn.harness = Some("manual".to_string());
    spawn.prompt = Some("Review this poke mission and write result.md.".to_string());
    let spawned = parse_json("spawn", &server.tachi_arena(Parameters(spawn)).await?)?;
    let mission_id = spawned
        .get("mission_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("spawn response lacks mission_id: {spawned}"))?
        .to_string();
    if spawned.get("state").and_then(Value::as_str) != Some("ready") {
        return Err(format!("manual spawn should be ready: {spawned}"));
    }
    let plan_path = PathBuf::from(
        spawned
            .get("plan_path")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("spawn response lacks plan_path: {spawned}"))?,
    );
    let result_path = PathBuf::from(
        spawned
            .get("result_path")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("spawn response lacks result_path: {spawned}"))?,
    );
    crate::utils::write_owner_only_file_atomic(&plan_path, b"# Plan\n\nManual poke plan.\n")
        .map_err(|e| format!("write poke arena plan: {e}"))?;
    crate::utils::write_owner_only_file_atomic(
        &result_path,
        b"# Result\n\nPoke arena lifecycle completed.\n",
    )
    .map_err(|e| format!("write poke arena result: {e}"))?;

    let mut board = arena_params("board");
    board.arena_id = Some(arena_id.clone());
    let board = parse_json("board", &server.tachi_arena(Parameters(board)).await?)?;
    let mission = board
        .pointer("/result/missions/0")
        .ok_or_else(|| format!("arena board lacks mission: {board}"))?;
    if mission.get("plan_written").and_then(Value::as_bool) != Some(true)
        || mission.get("result_written").and_then(Value::as_bool) != Some(true)
    {
        return Err(format!(
            "arena board did not refresh written artifacts: {board}"
        ));
    }

    let mut collect = arena_params("collect");
    collect.arena_id = Some(arena_id.clone());
    collect.mission_id = Some(mission_id.clone());
    let collected = parse_json("collect", &server.tachi_arena(Parameters(collect)).await?)?;
    if collected
        .pointer("/missions/0/state")
        .and_then(Value::as_str)
        != Some("collected")
    {
        return Err(format!(
            "arena collect did not mark mission collected: {collected}"
        ));
    }

    let mut close = arena_params("close");
    close.arena_id = Some(arena_id.clone());
    let closed = parse_json("close", &server.tachi_arena(Parameters(close)).await?)?;
    if closed.get("state").and_then(Value::as_str) != Some("closed") {
        return Err(format!("arena close did not close: {closed}"));
    }

    Ok(json!({
        "name": "arena_lifecycle",
        "status": "passed",
        "expected": "manual arena mission can open, spawn, refresh board, collect result, and close",
        "observed": {
            "arena_id": arena_id,
            "mission_id": mission_id,
            "arena_dir": arena_dir.to_string_lossy(),
            "plan_path": plan_path.to_string_lossy(),
            "result_path": result_path.to_string_lossy(),
            "close": closed,
        },
        "repro_steps": [
            "tachi_arena open in isolated TACHI_ARENA_ROOT",
            "tachi_arena spawn harness=manual launch=false",
            "write plan.md/result.md",
            "tachi_arena board then collect then close"
        ],
    }))
}
