use super::*;

const DEFAULT_REAP_STALE_SECS: i64 = 3600;

fn status_age_secs(status: &Value) -> Option<i64> {
    let timestamp = status
        .get("created_at")
        .and_then(Value::as_str)
        .or_else(|| status.get("updated_at").and_then(Value::as_str))?;
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()?
        .with_timezone(&Utc);
    Some((Utc::now() - parsed).num_seconds())
}

fn reap_stale_reason(status: &Value) -> Option<String> {
    let state = status.get("state").and_then(Value::as_str).unwrap_or("");
    if !active_state(state) {
        return None;
    }
    let threshold = status
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .map(|secs| secs.max(1) as i64)
        .unwrap_or(DEFAULT_REAP_STALE_SECS);
    let age = status_age_secs(status)?;
    if age >= threshold {
        Some(format!(
            "active mission exceeded reap threshold ({age}s >= {threshold}s)"
        ))
    } else {
        None
    }
}

pub(super) fn handle_abort(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='abort'".to_string())?;
    let mission_id = params
        .mission_id
        .as_deref()
        .ok_or_else(|| "mission_id is required for action='abort'".to_string())?;
    let reason = params
        .reason
        .unwrap_or_else(|| "aborted by leader".to_string());
    let status = update_mission_status(
        arena_id,
        mission_id,
        json!({
            "state": "aborted",
            "completed_at": Utc::now().to_rfc3339(),
            "abort_reason": reason,
        }),
    )?;
    refresh_board(arena_id)?;
    crate::utils::append_run_event(
        &arena_dir(arena_id)?,
        json!({
            "type": "mission_aborted",
            "arena_id": arena_id,
            "mission_id": mission_id,
            "timestamp": Utc::now().to_rfc3339(),
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "abort",
        "arena_id": arena_id,
        "mission_id": mission_id,
        "status": status,
    }))
    .map_err(|e| format!("serialize arena abort: {e}"))
}

pub(super) fn handle_reap(params: TachiArenaParams) -> Result<String, String> {
    let dry_run = params.dry_run.unwrap_or(true);
    let arena_filter = params.arena_id.clone();
    let arenas = if let Some(arena_id) = arena_filter.as_deref() {
        vec![arena_id.to_string()]
    } else {
        let root = arena_root();
        if !root.exists() {
            Vec::new()
        } else {
            std::fs::read_dir(&root)
                .map_err(|e| format!("read arena root: {e}"))?
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| validate_arena_id(name).is_ok())
                .collect()
        }
    };
    let mut stale = Vec::new();
    for arena_id in arenas {
        let mut changed = false;
        for status in mission_statuses(&arena_id)? {
            let Some(stale_reason) = reap_stale_reason(&status) else {
                continue;
            };
            let state = status.get("state").and_then(Value::as_str).unwrap_or("");
            let Some(mission_id) = status.get("mission_id").and_then(Value::as_str) else {
                continue;
            };
            stale.push(json!({
                "arena_id": arena_id,
                "mission_id": mission_id,
                "state": state,
                "reason": stale_reason,
            }));
            if !dry_run {
                update_mission_status(
                    &arena_id,
                    mission_id,
                    json!({
                        "state": "reaped",
                        "completed_at": Utc::now().to_rfc3339(),
                        "reap_reason": params.reason.as_deref().unwrap_or(&stale_reason),
                    }),
                )?;
                changed = true;
            }
        }
        if changed {
            refresh_board(&arena_id)?;
        }
    }
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "reap",
        "dry_run": dry_run,
        "stale_missions": stale,
    }))
    .map_err(|e| format!("serialize arena reap: {e}"))
}

pub(super) fn handle_close(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='close'".to_string())?;
    let force = params.force;
    let require_collected = params.require_collected.unwrap_or(true);
    let dir = arena_dir(arena_id)?;
    let missions = mission_statuses(arena_id)?;
    let mut blockers = Vec::new();
    for mission in &missions {
        let state = mission.get("state").and_then(Value::as_str).unwrap_or("");
        let result_written = mission
            .get("result_written")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if active_state(state) {
            blockers.push(json!({
                "mission_id": mission.get("mission_id"),
                "reason": "mission still active",
                "state": state,
            }));
        } else if require_collected && result_written && state != "collected" {
            blockers.push(json!({
                "mission_id": mission.get("mission_id"),
                "reason": "result written but not collected",
                "state": state,
            }));
        }
    }
    if !force && !blockers.is_empty() {
        return serde_json::to_string(&json!({
            "tool": "tachi_arena",
            "action": "close",
            "arena_id": arena_id,
            "state": "blocked",
            "blockers": blockers,
            "message": "abort/reap active missions or collect written results before close; pass force=true to override",
        }))
        .map_err(|e| format!("serialize arena close blocked: {e}"));
    }

    let mut manifest = read_json_file(&dir.join("manifest.json"))?;
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("state".to_string(), json!("closed"));
        obj.insert("closed_at".to_string(), json!(Utc::now().to_rfc3339()));
        obj.insert("updated_at".to_string(), json!(Utc::now().to_rfc3339()));
    }
    crate::utils::write_json_file_owner_only(&dir.join("manifest.json"), &manifest)?;
    let summary = render_summary_md(arena_id, &missions);
    crate::utils::write_owner_only_file_atomic(&dir.join("summary.md"), summary.as_bytes())
        .map_err(|e| format!("write summary.md: {e}"))?;
    crate::utils::append_run_event(
        &dir,
        json!({
            "type": "arena_closed",
            "arena_id": arena_id,
            "timestamp": Utc::now().to_rfc3339(),
            "force": force,
        }),
    )?;
    refresh_board(arena_id)?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "close",
        "arena_id": arena_id,
        "state": "closed",
        "summary_path": dir.join("summary.md"),
    }))
    .map_err(|e| format!("serialize arena close: {e}"))
}
