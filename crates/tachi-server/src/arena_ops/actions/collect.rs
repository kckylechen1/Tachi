use super::*;

pub(super) fn handle_collect(params: TachiArenaParams) -> Result<String, String> {
    let arena_id = params
        .arena_id
        .as_deref()
        .ok_or_else(|| "arena_id is required for action='collect'".to_string())?;
    let mission_ids = if let Some(mission_id) = params.mission_id.as_deref() {
        validate_mission_id(mission_id)?;
        vec![mission_id.to_string()]
    } else {
        mission_statuses(arena_id)?
            .into_iter()
            .filter_map(|s| {
                s.get("mission_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    };
    let mut collected = Vec::new();
    for mission_id in mission_ids {
        let dir = mission_dir(arena_id, &mission_id)?;
        let result_path = dir.join("result.md");
        let plan_path = dir.join("plan.md");
        let mut status_before = read_required_mission_status(arena_id, &mission_id)?;
        refresh_linked_dispatch_fields(&mut status_before);
        let mut artifact_read_errors = Vec::new();
        let mut result = match read_mission_result(arena_id, &mission_id) {
            ArenaArtifactRead::Present(raw) => raw,
            ArenaArtifactRead::Missing => String::new(),
            ArenaArtifactRead::Error(err) => {
                artifact_read_errors.push(err);
                String::new()
            }
        };
        let mut result_source = if artifact_read_errors.is_empty() {
            if result.is_empty() {
                "missing"
            } else {
                "mission_result"
            }
        } else {
            "result_read_error"
        };
        let mut canonical_result_ref = Value::Null;
        // collect no longer COPIES the linked dispatch result.md into the
        // mission result.md (the linked_result_copies ratchet). Instead it
        // reads the canonical dispatch run_dir/result.md reference and surfaces
        // it for the caller; the canonical artifact stays the single source of
        // truth.
        if result.trim().is_empty() && artifact_read_errors.is_empty() {
            if let Some(dispatch_id) = status_before.get("dispatch_id").and_then(Value::as_str) {
                let run_dir_hint = status_before.get("run_dir").and_then(Value::as_str);
                match read_linked_dispatch_result(dispatch_id, run_dir_hint) {
                    Ok(Some(dispatch_result)) => {
                        result = dispatch_result;
                        result_source = "linked_dispatch_result";
                    }
                    Ok(None) => {
                        if let Some(run_dir) = run_dir_hint {
                            canonical_result_ref = json!(format!("{run_dir}/result.md"));
                        }
                    }
                    Err(error) => {
                        artifact_read_errors.push(error);
                        result_source = "result_read_error";
                    }
                }
            }
        }
        match read_mission_plan(arena_id, &mission_id) {
            ArenaArtifactRead::Present(_) | ArenaArtifactRead::Missing => {}
            ArenaArtifactRead::Error(err) => artifact_read_errors.push(err),
        }
        let result_written = match mission_file_nonempty(arena_id, &mission_id, "result.md") {
            Ok(written) => written,
            Err(error) => {
                artifact_read_errors.push(error);
                false
            }
        };
        let plan_written = match mission_file_nonempty(arena_id, &mission_id, "plan.md") {
            Ok(written) => written,
            Err(error) => {
                artifact_read_errors.push(error);
                false
            }
        };
        let artifact_read_error = artifact_read_errors.first().cloned();
        // [1319-D1] collect no longer copies the linked dispatch result into the
        // mission result.md, so a collected state must be reachable from either
        // a locally-written mission result OR a successfully-read canonical
        // linked dispatch result (result_source == "linked_dispatch_result").
        let canonical_result_read = result_source == "linked_dispatch_result";
        let state = if !artifact_read_errors.is_empty() {
            "artifact_read_error"
        } else if result_written || canonical_result_read {
            "collected"
        } else {
            "pending_result"
        };
        let status = update_mission_status(
            arena_id,
            &mission_id,
            json!({
                "state": state,
                "collected_at": Utc::now().to_rfc3339(),
                "result_source": result_source,
                "artifact_read_error": artifact_read_error,
                "artifact_read_errors": if artifact_read_errors.is_empty() {
                    Value::Null
                } else {
                    json!(artifact_read_errors.clone())
                },
                "completion_draft": if state == "collected" {
                    completion_draft_for_mission(&status_before, &result_path, &result)
                } else {
                    Value::Null
                },
            }),
        )?;
        let compact_status = compact_mission_status(&status);
        collected.push(json!({
            "mission_id": mission_id,
            "state": state,
            "plan_written": plan_written,
            "result_written": result_written,
            "result_source": result_source,
            "plan_path": plan_path,
            "result_path": result_path,
            "canonical_result_ref": canonical_result_ref,
            "result": result,
            "artifact_read_error": artifact_read_error,
            "artifact_read_errors": artifact_read_errors,
            "completion_draft": status.get("completion_draft").cloned().unwrap_or(Value::Null),
            "status": compact_status,
        }));
    }
    refresh_board(arena_id)?;
    crate::utils::append_run_event(
        &arena_dir(arena_id)?,
        json!({
            "type": "missions_collected",
            "arena_id": arena_id,
            "count": collected.len(),
            "timestamp": Utc::now().to_rfc3339(),
        }),
    )?;
    serde_json::to_string(&json!({
        "tool": "tachi_arena",
        "action": "collect",
        "arena_id": arena_id,
        "missions": collected,
    }))
    .map_err(|e| format!("serialize arena collect: {e}"))
}
