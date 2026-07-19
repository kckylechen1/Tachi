use super::*;

pub(crate) fn mark_task_dispatch(
    flow_id: &str,
    dispatch_id: &str,
    mut card: Value,
) -> Result<(), String> {
    if !is_safe_dispatch_marker_id(dispatch_id) {
        return Err(format!(
            "invalid dispatch_id for flow marker: {dispatch_id}"
        ));
    }
    let _guard = FLOW_MARKER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create dispatch artifact dir: {e}"))?;

    let status_path = run_dir.join("status.json");
    let mut status = read_json_file(&status_path)?.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({});
    }
    let card_path = run_dir
        .join("artifacts")
        .join(format!("dispatch-{dispatch_id}.json"));
    let card_path_string = card_path.to_string_lossy().to_string();
    let had_marker = dispatch_marker_has_any_projection(&status, dispatch_id);
    if dispatch_marker_projections_complete(&status, flow_id, dispatch_id, &card_path)? {
        return Ok(());
    }

    let recorded_at = Utc::now().to_rfc3339();
    // A partial status is a repair, not a second dispatch. Prefer a readable
    // existing card (including a legacy map target) so the repair keeps its
    // original metadata; only synthesize from the incoming card when no
    // durable card survived.
    let mapped_card = dispatch_marker_card_path(&status, dispatch_id)
        .map(PathBuf::from)
        .filter(|path| path != &card_path)
        .map(|path| read_dispatch_marker_card(&path))
        .transpose()?
        .flatten();
    let existing_card = read_dispatch_marker_card(&card_path)?.or(mapped_card);
    if let Some(existing_card) = existing_card {
        card = existing_card;
    }
    if !card.is_object() {
        card = json!({ "details": card });
    }
    if let Some(obj) = card.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("recorded_at".to_string(), json!(recorded_at));
    }

    write_json_atomic(&card_path, &card)?;
    let prior_mapped_card = dispatch_marker_card_path(&status, dispatch_id).map(str::to_string);

    let obj = status
        .as_object_mut()
        .ok_or_else(|| "flow status must be a JSON object".to_string())?;
    let dispatch_ids = obj
        .entry("dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_ids.is_array() {
        *dispatch_ids = json!([]);
    }
    if let Some(ids) = dispatch_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let dispatch_cards = obj
        .entry("dispatch_cards".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_cards.is_array() {
        *dispatch_cards = json!([]);
    }
    if let Some(cards) = dispatch_cards.as_array_mut() {
        // `dispatch_ids` can be incomplete, so it cannot authorize erasing
        // every legacy list entry. Remove only paths we can positively tie to
        // this marker (the prior map, canonical path, or id-bearing filename)
        // and retain foreign/unknown projections intact.
        cards.retain(|value| {
            let Some(path) = value.as_str() else {
                return true;
            };
            path != card_path_string
                && Some(path) != prior_mapped_card.as_deref()
                && !is_dispatch_card_path_for_id(path, dispatch_id)
        });
        cards.push(json!(card_path_string));
    }
    let artifacts = obj
        .entry("artifacts".to_string())
        .or_insert_with(|| json!({}));
    if !artifacts.is_object() {
        *artifacts = json!({});
    }
    if let Some(artifact_obj) = artifacts.as_object_mut() {
        let dispatch_artifacts = artifact_obj
            .entry("dispatches".to_string())
            .or_insert_with(|| json!({}));
        if !dispatch_artifacts.is_object() {
            *dispatch_artifacts = json!({});
        }
        if let Some(dispatch_obj) = dispatch_artifacts.as_object_mut() {
            dispatch_obj.insert(dispatch_id.to_string(), json!(card_path_string));
        }
    }
    obj.insert("stage".to_string(), json!("dispatch"));
    obj.insert("state".to_string(), json!("dispatched"));
    obj.insert("last_dispatch_id".to_string(), json!(dispatch_id));
    obj.insert("updated_at".to_string(), json!(recorded_at.clone()));
    if obj.get("created_at").is_none() {
        obj.insert("created_at".to_string(), json!(recorded_at.clone()));
    }
    write_json_atomic(&status_path, &status)?;

    if !had_marker {
        append_flow_event(
            &run_dir,
            json!({
                "event": "dispatch_linked",
                "flow_id": flow_id,
                "dispatch_id": dispatch_id,
                "dispatch_card": card_path_string,
                "timestamp": recorded_at,
            }),
        )?;
    }
    Ok(())
}

/// A dispatch marker is owned by the immutable dispatch id, never by the
/// filesystem spelling of the artifact that happens to describe it. An
/// existing projection proves the event was already emitted, even when a
/// later interrupted write left the remaining projections incomplete.
fn dispatch_marker_has_any_projection(status: &Value, dispatch_id: &str) -> bool {
    status
        .get("dispatch_ids")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(dispatch_id)))
        || status
            .get("artifacts")
            .and_then(|artifacts| artifacts.get("dispatches"))
            .and_then(Value::as_object)
            .is_some_and(|dispatches| dispatches.contains_key(dispatch_id))
        || status
            .get("dispatch_cards")
            .and_then(Value::as_array)
            .is_some_and(|cards| {
                cards
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|path| is_dispatch_card_path_for_id(path, dispatch_id))
            })
}

fn dispatch_marker_card_path<'a>(status: &'a Value, dispatch_id: &str) -> Option<&'a str> {
    status
        .get("artifacts")
        .and_then(|artifacts| artifacts.get("dispatches"))
        .and_then(|dispatches| dispatches.get(dispatch_id))
        .and_then(Value::as_str)
}

/// Idempotence is safe only after every dispatch projection agrees on the
/// canonical, readable card. An ID-only or dangling-map status must flow
/// through the repair path rather than silently suppressing the write.
fn dispatch_marker_projections_complete(
    status: &Value,
    flow_id: &str,
    dispatch_id: &str,
    card_path: &Path,
) -> Result<bool, String> {
    let card_path_string = card_path.to_string_lossy();
    let has_dispatch_id = status
        .get("dispatch_ids")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(dispatch_id)));
    let card_list = status.get("dispatch_cards").and_then(Value::as_array);
    let canonical_card_count = card_list
        .map(|cards| {
            cards
                .iter()
                .filter(|value| value.as_str() == Some(card_path_string.as_ref()))
                .count()
        })
        .unwrap_or_default();
    let has_card_list_entry = canonical_card_count == 1;
    let has_canonical_map =
        dispatch_marker_card_path(status, dispatch_id) == Some(card_path_string.as_ref());
    let card_is_valid = read_dispatch_marker_card(card_path)?.is_some_and(|card| {
        card.is_object()
            && card.get("flow_id").and_then(Value::as_str) == Some(flow_id)
            && card.get("dispatch_id").and_then(Value::as_str) == Some(dispatch_id)
    });

    Ok(has_dispatch_id && has_card_list_entry && has_canonical_map && card_is_valid)
}

/// Card JSON is a repairable projection. A malformed canonical or legacy
/// card must not make the dispatch marker itself unavailable: retain a valid
/// sibling when present, otherwise rebuild from the incoming dispatch data.
fn read_dispatch_marker_card(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(card) => Ok(Some(card)),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "repairing malformed dispatch card projection");
                Ok(None)
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

fn is_dispatch_card_path_for_id(path: &str, dispatch_id: &str) -> bool {
    Path::new(path).file_name().and_then(|name| name.to_str())
        == Some(format!("dispatch-{dispatch_id}.json").as_str())
}

/// tachi#1271: same-shape check as `dispatch_marker_has_any_projection`
/// above, keyed on the `completed_dispatch_ids` array that this function's
/// own dedup below (~line 342-347) already treats as the durable
/// "this dispatch_id has already completed" signal.
fn completion_marker_already_recorded(status: &Value, dispatch_id: &str) -> bool {
    status
        .get("completed_dispatch_ids")
        .and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(dispatch_id)))
}

pub(crate) fn mark_task_dispatch_completion(
    flow_id: &str,
    dispatch_id: &str,
    mut completion: Value,
) -> Result<Value, String> {
    if !is_safe_dispatch_marker_id(dispatch_id) {
        return Err(format!(
            "invalid dispatch_id for flow completion marker: {dispatch_id}"
        ));
    }
    let _guard = FLOW_MARKER_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let run_dir = run_dir_for_flow_id(flow_id)?;
    std::fs::create_dir_all(run_dir.join("artifacts"))
        .map_err(|e| format!("create dispatch artifact dir: {e}"))?;

    let completed_at = Utc::now().to_rfc3339();
    if !completion.is_object() {
        completion = json!({ "details": completion });
    }
    if let Some(obj) = completion.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("completed_at".to_string(), json!(completed_at.clone()));
    }

    let status_path = run_dir.join("status.json");
    let mut status = read_json_file(&status_path)?.unwrap_or_else(|| json!({}));
    if !status.is_object() {
        status = json!({});
    }
    // tachi#1271: mirror #1257's `had_marker` idempotency guard on
    // `dispatch_linked` -- captured from the *pre-mutation* status so a
    // repeat completion call for an already-completed dispatch_id does not
    // emit a second `dispatch_completed` event, even though the
    // `completed_dispatch_ids` / card projections below are already
    // deduped by dispatch_id.
    let had_completion_marker = completion_marker_already_recorded(&status, dispatch_id);

    let card_path = status
        .get("artifacts")
        .and_then(|artifacts| artifacts.get("dispatches"))
        .and_then(|dispatches| dispatches.get(dispatch_id))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            run_dir
                .join("artifacts")
                .join(format!("dispatch-{dispatch_id}.json"))
        });
    let mut card = read_json_file(&card_path)?.unwrap_or_else(|| {
        json!({
            "flow_id": flow_id,
            "dispatch_id": dispatch_id,
        })
    });
    if !card.is_object() {
        card = json!({ "details": card });
    }
    if let Some(obj) = card.as_object_mut() {
        obj.insert("flow_id".to_string(), json!(flow_id));
        obj.insert("dispatch_id".to_string(), json!(dispatch_id));
        obj.insert("completion".to_string(), completion.clone());
        let history = obj
            .entry("completion_history".to_string())
            .or_insert_with(|| json!([]));
        if !history.is_array() {
            *history = json!([]);
        }
        if let Some(items) = history.as_array_mut() {
            let incoming_eval_id = completion
                .get("eval_memory_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let incoming_task_id = completion
                .get("task_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let already_present = items.iter().any(|item| {
                let item_eval_id = item.get("eval_memory_id").and_then(Value::as_str);
                let item_task_id = item.get("task_id").and_then(Value::as_str);
                incoming_eval_id
                    .as_deref()
                    .is_some_and(|id| item_eval_id == Some(id))
                    || incoming_task_id
                        .as_deref()
                        .is_some_and(|id| item_task_id == Some(id))
            });
            if !already_present {
                items.push(completion.clone());
            }
        }
    }
    write_json_atomic(&card_path, &card)?;
    let card_path_string = card_path.to_string_lossy().to_string();

    if !status.is_object() {
        status = json!({});
    }
    let obj = status
        .as_object_mut()
        .ok_or_else(|| "flow status must be a JSON object".to_string())?;
    let dispatch_ids = obj
        .entry("dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !dispatch_ids.is_array() {
        *dispatch_ids = json!([]);
    }
    if let Some(ids) = dispatch_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let completed_ids = obj
        .entry("completed_dispatch_ids".to_string())
        .or_insert_with(|| json!([]));
    if !completed_ids.is_array() {
        *completed_ids = json!([]);
    }
    if let Some(ids) = completed_ids.as_array_mut() {
        let already_present = ids.iter().any(|value| value.as_str() == Some(dispatch_id));
        if !already_present {
            ids.push(json!(dispatch_id));
        }
    }
    let artifacts = obj
        .entry("artifacts".to_string())
        .or_insert_with(|| json!({}));
    if !artifacts.is_object() {
        *artifacts = json!({});
    }
    if let Some(artifact_obj) = artifacts.as_object_mut() {
        let dispatch_artifacts = artifact_obj
            .entry("dispatches".to_string())
            .or_insert_with(|| json!({}));
        if !dispatch_artifacts.is_object() {
            *dispatch_artifacts = json!({});
        }
        if let Some(dispatch_obj) = dispatch_artifacts.as_object_mut() {
            dispatch_obj.insert(dispatch_id.to_string(), json!(card_path_string.clone()));
        }
        let completion_artifacts = artifact_obj
            .entry("dispatch_completions".to_string())
            .or_insert_with(|| json!({}));
        if !completion_artifacts.is_object() {
            *completion_artifacts = json!({});
        }
        if let Some(completion_obj) = completion_artifacts.as_object_mut() {
            completion_obj.insert(dispatch_id.to_string(), completion.clone());
        }
    }
    let dispatch_eval = obj
        .entry("dispatch_eval".to_string())
        .or_insert_with(|| json!({}));
    if !dispatch_eval.is_object() {
        *dispatch_eval = json!({});
    }
    if let Some(eval_obj) = dispatch_eval.as_object_mut() {
        eval_obj.insert(dispatch_id.to_string(), completion.clone());
    }
    let current_stage = obj.get("stage").and_then(Value::as_str);
    if current_stage.is_none() || current_stage == Some("dispatch") {
        obj.insert("stage".to_string(), json!("eval"));
    }
    let current_state = obj.get("state").and_then(Value::as_str);
    if current_state.is_none() || current_state == Some("dispatched") {
        obj.insert("state".to_string(), json!("dispatch_completed"));
    }
    obj.insert("last_completed_dispatch_id".to_string(), json!(dispatch_id));
    obj.insert(
        "last_dispatch_completion_at".to_string(),
        json!(completed_at.clone()),
    );
    obj.insert("updated_at".to_string(), json!(completed_at.clone()));
    write_json_atomic(&status_path, &status)?;

    if !had_completion_marker {
        append_flow_event(
            &run_dir,
            json!({
                "event": "dispatch_completed",
                "flow_id": flow_id,
                "dispatch_id": dispatch_id,
                "dispatch_card": card_path_string,
                "completion": completion,
                "timestamp": completed_at,
            }),
        )?;
    }

    Ok(json!({
        "recorded": true,
        "flow_id": flow_id,
        "dispatch_id": dispatch_id,
        "dispatch_card": card_path.to_string_lossy().to_string(),
    }))
}

fn is_safe_dispatch_marker_id(dispatch_id: &str) -> bool {
    !dispatch_id.trim().is_empty()
        && dispatch_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set_path(name: &'static str, path: &Path) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, path);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.name, previous);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    /// A repeated dispatch with an interrupted status write must repair every
    /// durable projection but never emit a second dispatch_linked lifecycle
    /// event. Each corruption below was a false idempotence no-op before this
    /// repair because the status retained only one identity fragment.
    #[test]
    #[allow(clippy::await_holding_lock)]
    fn incomplete_dispatch_marker_repairs_projections_without_duplicate_event() {
        let _env_lock = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp run root");
        let run_root = temp.path().join("runs");
        let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", &run_root);
        let dispatch_id = "20260719T000000Z-repair-marker";

        for corruption in [
            "id_only",
            "missing_map",
            "dangling_map",
            "deleted_card",
            "malformed_legacy_card",
            "malformed_canonical_card",
        ] {
            let flow_id = format!("flow_20260719T000000Z_{corruption}");
            mark_task_dispatch(
                &flow_id,
                dispatch_id,
                json!({"agent": "original", "metadata": {"retain": true}}),
            )
            .expect("initial marker");

            let run_dir = run_dir_for_flow_id(&flow_id).expect("run dir");
            let status_path = run_dir.join("status.json");
            let mut status: Value = serde_json::from_str(
                &std::fs::read_to_string(&status_path).expect("read initial status"),
            )
            .expect("parse initial status");
            let canonical_card = PathBuf::from(
                status["artifacts"]["dispatches"][dispatch_id]
                    .as_str()
                    .expect("canonical card"),
            );
            status["preserved_status_metadata"] = json!({"owner": "original"});
            match corruption {
                "id_only" => {
                    status["dispatch_cards"] = json!([]);
                    status["artifacts"] = json!({});
                }
                "missing_map" => {
                    status["artifacts"]["dispatches"] = json!({});
                }
                "dangling_map" => {
                    let dangling = temp.path().join(format!("{corruption}.json"));
                    status["dispatch_cards"] = json!([dangling.to_string_lossy().to_string()]);
                    status["artifacts"]["dispatches"][dispatch_id] =
                        json!(dangling.to_string_lossy().to_string());
                }
                "deleted_card" => {
                    std::fs::remove_file(&canonical_card).expect("delete canonical card");
                }
                "malformed_legacy_card" => {
                    let legacy_card = temp.path().join(format!("{corruption}.json"));
                    std::fs::write(&legacy_card, "{ malformed legacy card")
                        .expect("write malformed legacy card");
                    status["dispatch_cards"] = json!([legacy_card.to_string_lossy().to_string()]);
                    status["artifacts"]["dispatches"][dispatch_id] =
                        json!(legacy_card.to_string_lossy().to_string());
                }
                "malformed_canonical_card" => {
                    std::fs::write(&canonical_card, "{ malformed canonical card")
                        .expect("write malformed canonical card");
                }
                _ => unreachable!(),
            }
            std::fs::write(
                &status_path,
                serde_json::to_vec_pretty(&status).expect("serialize corrupt status"),
            )
            .expect("write corrupt status");

            mark_task_dispatch(
                &flow_id,
                dispatch_id,
                json!({"agent": "repair", "metadata": {"replacement": true}}),
            )
            .expect("repair marker");

            let repaired: Value = serde_json::from_str(
                &std::fs::read_to_string(&status_path).expect("read repaired status"),
            )
            .expect("parse repaired status");
            assert_eq!(
                repaired["dispatch_ids"],
                json!([dispatch_id]),
                "{corruption}"
            );
            assert_eq!(
                repaired["dispatch_cards"].as_array().map(Vec::len),
                Some(1),
                "{corruption}: repair must leave one card reference"
            );
            assert_eq!(
                repaired["artifacts"]["dispatches"][dispatch_id],
                json!(canonical_card.to_string_lossy().to_string()),
                "{corruption}: map must point at the canonical card"
            );
            assert_eq!(
                repaired["preserved_status_metadata"]["owner"],
                json!("original"),
                "{corruption}: repair must preserve unrelated status metadata"
            );
            let repaired_card: Value = serde_json::from_str(
                &std::fs::read_to_string(&canonical_card).expect("read repaired card"),
            )
            .expect("parse repaired card");
            assert_eq!(repaired_card["flow_id"], json!(flow_id), "{corruption}");
            assert_eq!(
                repaired_card["dispatch_id"],
                json!(dispatch_id),
                "{corruption}"
            );
            if !matches!(corruption, "deleted_card" | "malformed_canonical_card") {
                assert_eq!(
                    repaired_card["metadata"]["retain"],
                    json!(true),
                    "{corruption}: repair must retain existing card metadata"
                );
            }
            let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
            assert_eq!(
                events
                    .lines()
                    .filter(|line| line.contains("\"event\":\"dispatch_linked\""))
                    .count(),
                1,
                "{corruption}: repair must not duplicate dispatch_linked"
            );
        }
    }

    /// `dispatch_cards` is a legacy aggregate projection. A stale current-id
    /// map positively attributes its old path to this dispatch, so repair may
    /// remove that alias and duplicate canonical entries while preserving one
    /// canonical reference and no extra lifecycle event.
    #[test]
    #[allow(clippy::await_holding_lock)]
    fn dispatch_marker_repair_normalizes_legacy_card_aliases_from_stale_map() {
        let _env_lock = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp run root");
        let run_root = temp.path().join("runs");
        let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", &run_root);
        let flow_id = "flow_20260719T000001Z-legacy-list-repair";
        let dispatch_id = "20260719T000001Z-legacy-list-repair";
        mark_task_dispatch(
            flow_id,
            dispatch_id,
            json!({"agent": "original", "metadata": {"retain": true}}),
        )
        .expect("initial marker");

        let run_dir = run_dir_for_flow_id(flow_id).expect("run dir");
        let status_path = run_dir.join("status.json");
        let mut status: Value = serde_json::from_str(
            &std::fs::read_to_string(&status_path).expect("read initial status"),
        )
        .expect("parse initial status");
        let canonical_card = status["artifacts"]["dispatches"][dispatch_id]
            .as_str()
            .expect("canonical card")
            .to_string();
        let legacy_card = temp.path().join("legacy-dispatch-card.json");
        status["dispatch_cards"] = json!([
            legacy_card.to_string_lossy().to_string(),
            canonical_card,
            status["artifacts"]["dispatches"][dispatch_id],
        ]);
        status["artifacts"]["dispatches"][dispatch_id] =
            json!(legacy_card.to_string_lossy().to_string());
        std::fs::write(
            &status_path,
            serde_json::to_vec_pretty(&status).expect("serialize legacy status"),
        )
        .expect("write legacy status");

        mark_task_dispatch(
            flow_id,
            dispatch_id,
            json!({"agent": "repair", "metadata": {"replacement": true}}),
        )
        .expect("repair marker");

        let repaired: Value = serde_json::from_str(
            &std::fs::read_to_string(&status_path).expect("read repaired status"),
        )
        .expect("parse repaired status");
        assert_eq!(
            repaired["dispatch_cards"],
            json!([repaired["artifacts"]["dispatches"][dispatch_id]]),
            "legacy aliases and duplicate canonical paths must collapse to one reference"
        );
        let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
        assert_eq!(
            events
                .lines()
                .filter(|line| line.contains("\"event\":\"dispatch_linked\""))
                .count(),
            1,
            "projection repair must not append a second lifecycle event: {events}"
        );
    }

    /// `dispatch_ids` is itself only a projection.  Seeing just the current
    /// id there does not authorize a repair to delete card references that a
    /// different artifact-map key positively attributes to another dispatch.
    #[test]
    #[allow(clippy::await_holding_lock)]
    fn dispatch_marker_repair_preserves_foreign_card_refs_with_partial_id_list() {
        let _env_lock = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp run root");
        let run_root = temp.path().join("runs");
        let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", &run_root);
        let flow_id = "flow_20260719T000002Z-preserve-foreign-card";
        let dispatch_id = "20260719T000002Z-current-card";
        let foreign_id = "20260719T000002Z-foreign-card";
        mark_task_dispatch(flow_id, dispatch_id, json!({"agent": "current"}))
            .expect("initial current marker");

        let run_dir = run_dir_for_flow_id(flow_id).expect("run dir");
        let status_path = run_dir.join("status.json");
        let mut status: Value = serde_json::from_str(
            &std::fs::read_to_string(&status_path).expect("read initial status"),
        )
        .expect("parse initial status");
        let current_card = status["artifacts"]["dispatches"][dispatch_id]
            .as_str()
            .expect("current card")
            .to_string();
        let stale_current_card = temp.path().join("legacy-current-dispatch-card.json");
        let foreign_card = temp.path().join("foreign-dispatch-card.json");
        status["dispatch_ids"] = json!([dispatch_id]);
        status["dispatch_cards"] = json!([
            stale_current_card.to_string_lossy().to_string(),
            current_card,
            status["artifacts"]["dispatches"][dispatch_id],
            foreign_card.to_string_lossy().to_string(),
        ]);
        status["artifacts"]["dispatches"][dispatch_id] =
            json!(stale_current_card.to_string_lossy().to_string());
        status["artifacts"]["dispatches"][foreign_id] =
            json!(foreign_card.to_string_lossy().to_string());
        std::fs::write(
            &status_path,
            serde_json::to_vec_pretty(&status).expect("serialize partial projections"),
        )
        .expect("write partial projections");

        mark_task_dispatch(flow_id, dispatch_id, json!({"agent": "repair"}))
            .expect("repair current marker");

        let repaired: Value = serde_json::from_str(
            &std::fs::read_to_string(&status_path).expect("read repaired status"),
        )
        .expect("parse repaired status");
        assert!(
            repaired["dispatch_cards"]
                .as_array()
                .is_some_and(|cards| cards.iter().any(|card| {
                    card.as_str() == Some(foreign_card.to_string_lossy().as_ref())
                })),
            "foreign card projection must survive a current-id repair: {repaired:#}"
        );
        assert_eq!(
            repaired["artifacts"]["dispatches"][foreign_id],
            json!(foreign_card.to_string_lossy().to_string()),
            "foreign dispatch map entry must remain intact"
        );
        assert_eq!(
            repaired["artifacts"]["dispatches"][dispatch_id],
            json!(current_card),
            "current dispatch map must be normalized to the canonical card"
        );
        assert_eq!(
            repaired["dispatch_cards"]
                .as_array()
                .expect("dispatch_cards array")
                .iter()
                .filter(|card| card.as_str() == Some(current_card.as_str()))
                .count(),
            1,
            "current dispatch card projection must remain unique: {repaired:#}"
        );
        assert!(
            repaired["dispatch_cards"]
                .as_array()
                .is_some_and(|cards| cards.iter().all(|card| {
                    card.as_str() != Some(stale_current_card.to_string_lossy().as_ref())
                })),
            "current stale alias must be removed without removing foreign refs: {repaired:#}"
        );
        let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
        assert_eq!(
            events
                .lines()
                .filter(|line| line.contains("\"event\":\"dispatch_linked\""))
                .count(),
            1,
            "repair must not duplicate dispatch_linked: {events}"
        );
    }

    /// tachi#1271: #1257 made `dispatch_linked` idempotent (the `had_marker`
    /// guard above) but left `mark_task_dispatch_completion`'s
    /// `dispatch_completed` event unconditional, even though
    /// `completed_dispatch_ids` is already deduped by dispatch_id. A repeat
    /// completion call for the same dispatch_id (e.g. a retried collector)
    /// must not emit a second lifecycle event line.
    #[test]
    #[allow(clippy::await_holding_lock)]
    fn repeated_dispatch_completion_does_not_duplicate_completed_event() {
        let _env_lock = crate::shell_ops::tachi_run_root_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().expect("temp run root");
        let run_root = temp.path().join("runs");
        let _run_root = EnvVarGuard::set_path("TACHI_RUN_ROOT", &run_root);
        let flow_id = "flow_20260719T000003Z-completion-idempotency";
        let dispatch_id = "20260719T000003Z-completion-marker";

        mark_task_dispatch_completion(flow_id, dispatch_id, json!({"eval_memory_id": "eval-1"}))
            .expect("first completion");
        mark_task_dispatch_completion(flow_id, dispatch_id, json!({"eval_memory_id": "eval-2"}))
            .expect("repeated completion");

        let run_dir = run_dir_for_flow_id(flow_id).expect("run dir");
        let events = std::fs::read_to_string(run_dir.join("events.jsonl")).expect("events");
        assert_eq!(
            events
                .lines()
                .filter(|line| line.contains("\"event\":\"dispatch_completed\""))
                .count(),
            1,
            "repeated completion must not duplicate dispatch_completed: {events}"
        );
    }
}
