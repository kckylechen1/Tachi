use super::*;

fn mission_state_count(missions: &[Value], state: &str) -> usize {
    missions
        .iter()
        .filter(|mission| mission.get("state").and_then(Value::as_str) == Some(state))
        .count()
}

fn arena_board_summary(arena_id: &str, mut manifest: Value) -> Value {
    let board = refresh_board(arena_id).ok();
    let missions = board
        .as_ref()
        .and_then(|board| board.get("missions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let active_missions = missions
        .iter()
        .filter(|mission| {
            mission
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(active_state)
        })
        .count();
    let pending_collect = missions
        .iter()
        .filter(|mission| {
            mission.get("collection_state").and_then(Value::as_str)
                == Some("pending_collect_from_dispatch")
                || (mission
                    .get("result_written")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && mission.get("state").and_then(Value::as_str) != Some("collected"))
        })
        .count();
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("mission_count".to_string(), json!(missions.len()));
        obj.insert("active_missions".to_string(), json!(active_missions));
        obj.insert("pending_collect".to_string(), json!(pending_collect));
        obj.insert(
            "collected_missions".to_string(),
            json!(mission_state_count(&missions, "collected")),
        );
        obj.insert(
            "blocked_missions".to_string(),
            json!(
                mission_state_count(&missions, "launch_failed")
                    + mission_state_count(&missions, "artifact_read_error")
            ),
        );
        obj.insert(
            "board_path".to_string(),
            json!(arena_dir(arena_id)
                .map(|dir| dir.join("board.json").to_string_lossy().to_string())
                .unwrap_or_default()),
        );
    }
    manifest
}

pub(super) fn refresh_board(arena_id: &str) -> Result<Value, String> {
    let dir = arena_dir(arena_id)?;
    let missions = mission_statuses(arena_id)?;
    let board = json!({
        "arena_id": arena_id,
        "state": read_json_file(&dir.join("manifest.json"))
            .ok()
            .and_then(|v| v.get("state").cloned())
            .unwrap_or_else(|| json!("unknown")),
        "missions": missions,
        "updated_at": Utc::now().to_rfc3339(),
    });
    crate::utils::write_json_file_owner_only(&dir.join("board.json"), &board)?;
    Ok(board)
}

pub(super) fn handle_board(params: TachiArenaParams) -> Result<String, String> {
    if let Some(arena_id) = params.arena_id.as_deref() {
        let board = refresh_board(arena_id)?;
        return serde_json::to_string(&json!({
            "tool": "tachi_arena",
            "action": "board",
            "arena_id": arena_id,
            "result": board,
        }))
        .map_err(|e| format!("serialize arena board: {e}"));
    }

    let root = arena_root();
    let mut arenas = Vec::new();
    if root.exists() {
        for entry in std::fs::read_dir(&root)
            .map_err(|e| format!("read arena root {}: {e}", root.display()))?
        {
            let entry = entry.map_err(|e| format!("read arena entry: {e}"))?;
            let manifest_path = entry.path().join("manifest.json");
            if manifest_path.exists() {
                let arena_id = entry.file_name().to_string_lossy().to_string();
                if validate_arena_id(&arena_id).is_err() {
                    continue;
                }
                arenas.push(arena_board_summary(
                    &arena_id,
                    read_json_file(&manifest_path)?,
                ));
            }
        }
    }
    arenas.sort_by(|a, b| {
        b.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(a.get("created_at").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "board",
        "arena_root": root,
        "arenas": arenas,
    }))
    .map_err(|e| format!("serialize arena board: {e}"))
}
