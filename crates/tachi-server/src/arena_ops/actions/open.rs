use super::*;

pub(super) fn handle_open(params: TachiArenaParams) -> Result<String, String> {
    let title = params.title.unwrap_or_else(|| "Tachi Arena".to_string());
    let objective = params
        .objective
        .or(params.prompt)
        .ok_or_else(|| "objective or prompt is required for action='open'".to_string())?;
    let arena_id = params
        .arena_id
        .unwrap_or_else(|| new_arena_id(Some(&title), Some(&objective)));
    validate_arena_id(&arena_id)?;
    let dir = arena_dir(&arena_id)?;
    std::fs::create_dir_all(dir.join("missions"))
        .map_err(|e| format!("create arena dir {}: {e}", dir.display()))?;

    let now = Utc::now().to_rfc3339();
    let manifest = json!({
        "arena_id": arena_id,
        "title": title,
        "objective": objective,
        "state": "open",
        "created_at": now,
        "updated_at": now,
        "root": dir,
        "documents": {
            "arena": dir.join("arena.md"),
            "manifest": dir.join("manifest.json"),
            "board": dir.join("board.json"),
            "events": dir.join("events.jsonl"),
        },
    });
    crate::utils::write_json_file_owner_only(&dir.join("manifest.json"), &manifest)?;
    crate::utils::write_json_file_owner_only(
        &dir.join("board.json"),
        &json!({
            "arena_id": arena_id,
            "state": "open",
            "missions": [],
            "updated_at": now,
        }),
    )?;
    crate::utils::write_owner_only_file_atomic(
        &dir.join("arena.md"),
        render_arena_md(
            manifest["arena_id"].as_str().unwrap_or("arena"),
            manifest["title"].as_str().unwrap_or("Tachi Arena"),
            manifest["objective"].as_str().unwrap_or(""),
        )
        .as_bytes(),
    )
    .map_err(|e| format!("write arena.md: {e}"))?;
    crate::utils::append_run_event(
        &dir,
        json!({
            "type": "arena_opened",
            "arena_id": manifest["arena_id"],
            "timestamp": now,
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "open",
        "arena_id": manifest["arena_id"],
        "state": "open",
        "arena_dir": dir,
        "manifest_path": dir.join("manifest.json"),
        "board_path": dir.join("board.json"),
        "arena_path": dir.join("arena.md"),
    }))
    .map_err(|e| format!("serialize arena open: {e}"))
}
