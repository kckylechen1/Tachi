use super::*;

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
                arenas.push(read_json_file(&manifest_path)?);
            }
        }
    }
    arenas.sort_by(|a, b| {
        a.get("created_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("created_at").and_then(Value::as_str).unwrap_or(""))
    });
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "board",
        "arena_root": root,
        "arenas": arenas,
    }))
    .map_err(|e| format!("serialize arena board: {e}"))
}
