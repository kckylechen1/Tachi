use serde_json::{json, Value};

const SUMMARY_CHARS: usize = 240;
const TOPIC_CHARS: usize = 80;
const EXCERPT_CHARS: usize = 320;
const TRUNCATION_MARKER: &str = " … [truncated]";
const COMPACT_TARGET_BYTES: usize = 4 * 1024;

/// Only descriptive recall fields become previews. State, references, errors,
/// policy instructions and A2A delivery bodies retain their complete values.
pub(super) fn compact_feature_previews(response: &mut Value) {
    for section in ["wiki_hits", "memory_fragments", "eval_evidence"] {
        if let Some(rows) = response.get_mut(section) {
            compact_recall_rows(rows);
        }
    }
    if let Some(tasks) = response
        .get_mut("board_state")
        .and_then(|board| board.get_mut("tasks"))
        .and_then(Value::as_array_mut)
    {
        for task in tasks {
            preview_fields(task, &[("summary", SUMMARY_CHARS)]);
        }
    }
    if let Some(groups) = response
        .get_mut("doc_index")
        .and_then(|index| index.get_mut("groups"))
        .and_then(Value::as_array_mut)
    {
        for group in groups {
            match group["name"].as_str() {
                Some("project_wiki") => compact_recall_rows(&mut group["items"]),
                Some("eval_evidence") => {
                    if let Some(items) = group["items"].as_array_mut() {
                        for (index, item) in items.iter_mut().enumerate() {
                            if let Some(item) = item.as_object_mut() {
                                for field in ["summary", "topic", "excerpt"] {
                                    item.remove(field);
                                }
                                item.insert(
                                    "detail_ref".into(),
                                    json!(format!("/eval_evidence/{index}")),
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    response["compact_detail"] = json!({
        "text_previews": true,
        "expand": {"tool": "tachi_task", "action": "brief", "compact": false},
        "request_context": "Repeat the current request with compact=false, retaining its scope and search parameters.",
        "detail_ref_format": "JSON pointer within this response",
    });
}

/// Fold historical peer observations while keeping live work and diagnostics.
pub(super) fn compact_feature_structure(response: &mut Value) {
    if response.get("staff").is_some()
        && response.pointer("/board_state/staff") == response.get("staff")
    {
        response["board_state"]["staff"] = json!({"detail_ref":"/staff"});
    }
    if let Some(staff) = response.get_mut("staff") {
        if let Some(items) = staff.get_mut("items").and_then(Value::as_array_mut) {
            let before = items.len();
            let stale_clients: u64 = items
                .iter()
                .filter(|row| {
                    row["kind"] == "client"
                        && crate::staffing_ops::presence::foldable_inactive_peer(row)
                })
                .map(|row| 1 + row["collapsed_stale_sessions"].as_u64().unwrap_or(0))
                .sum();
            items.retain(|row| !crate::staffing_ops::presence::foldable_inactive_peer(row));
            let omitted = before - items.len();
            let shown = items.len();
            if omitted > 0 {
                staff["omitted_idle_stale_clients"] = json!(stale_clients);
                staff["omitted_inactive_rows"] = json!(omitted);
                staff["displayed_count"] = json!(shown);
            }
        }
    }
    if response.get("compact_detail").is_some() {
        finish_compact_budget(response);
    }
}

fn json_bytes(value: &impl serde::Serialize) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).expect("JSON value serializes");
    counter.0
}

fn finish_compact_budget(response: &mut Value) {
    if json_bytes(response) > COMPACT_TARGET_BYTES {
        let mut omitted = 0;
        if let Some(profiles) = response
            .get_mut("relevant_profiles")
            .and_then(Value::as_array_mut)
        {
            for profile in profiles {
                if let Some(profile) = profile.as_object_mut() {
                    if let Some(count) = profile
                        .get("reason")
                        .and_then(Value::as_array)
                        .filter(|reasons| reasons.iter().all(Value::is_string))
                        .map(Vec::len)
                    {
                        profile.remove("reason");
                        omitted += count;
                    }
                }
            }
        }
        if omitted > 0 {
            response["compact_detail"]["dispatch_reason_rows_omitted"] = json!(omitted);
        }
    }
    // Everything left outside expansion metadata is protected from further
    // clipping, including consumed A2A bodies, errors, identity and live claims.
    let protected = response
        .as_object()
        .expect("briefing object")
        .iter()
        .filter(|(key, _)| key.as_str() != "compact_detail")
        .collect::<std::collections::BTreeMap<_, _>>();
    let protected_bytes = json_bytes(&protected);
    response["compact_detail"]["budget"] = json!({
        "target_bytes": COMPACT_TARGET_BYTES,
        "json_bytes": 0,
        "protected_bytes": protected_bytes,
        "status": "within",
        "protected_content_retained": true,
    });
    // Equal-length status values keep the byte count stable at the boundary.
    loop {
        let bytes = json_bytes(response);
        if response["compact_detail"]["budget"]["json_bytes"] == json!(bytes) {
            break;
        }
        response["compact_detail"]["budget"]["json_bytes"] = json!(bytes);
    }
    let bytes = response["compact_detail"]["budget"]["json_bytes"]
        .as_u64()
        .unwrap();
    response["compact_detail"]["budget"]["status"] =
        json!(if bytes > COMPACT_TARGET_BYTES as u64 {
            "excess"
        } else {
            "within"
        });
}

fn compact_recall_rows(rows: &mut Value) {
    if let Some(rows) = rows.as_array_mut() {
        for row in rows {
            preview_fields(
                row,
                &[
                    ("summary", SUMMARY_CHARS),
                    ("topic", TOPIC_CHARS),
                    ("excerpt", EXCERPT_CHARS),
                ],
            );
        }
    }
}

fn preview_fields(row: &mut Value, fields: &[(&str, usize)]) {
    let Some(object) = row.as_object_mut() else {
        return;
    };
    let mut truncated = serde_json::Map::new();
    for &(field, limit) in fields {
        let Some(text) = object.get(field).and_then(Value::as_str) else {
            continue;
        };
        let Some((end, _)) = text.char_indices().nth(limit) else {
            continue;
        };
        let original_chars = text.chars().count();
        let preview = format!("{}{TRUNCATION_MARKER}", &text[..end]);
        object.insert(field.into(), json!(preview));
        truncated.insert(
            field.into(),
            json!({"original_chars": original_chars, "preview_chars": limit}),
        );
    }
    if !truncated.is_empty() {
        object.insert("truncated_fields".into(), Value::Object(truncated));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copilot_ops::feature_briefing::handlers::handle_tachi_feature_briefing;
    use crate::tool_params::TachiTaskParams;

    #[tokio::test]
    async fn compact_brief_bounds_unicode_recall_and_board_previews_but_full_keeps_text() {
        let server = crate::tests::make_server();
        let summary = format!("CompactPreviewNeedle {}", "旧任务🦀正文".repeat(1000));
        let topic = "主题🦀".repeat(1000);
        server
            .with_global_store(|store| {
                for (id, path) in [
                    ("compact-preview-eval", "/eval/compact-preview"),
                    ("compact-preview-memory", "/memory/compact-preview"),
                    ("compact-preview-board", "/kanban/tasks/compact-preview"),
                ] {
                    let mut entry = crate::tests::make_entry(id);
                    entry.path = path.into();
                    entry.summary = summary.clone();
                    entry.topic = topic.clone();
                    entry.text = "CompactPreviewNeedle verification evidence".into();
                    if id.ends_with("board") {
                        entry.category = "kanban".into();
                        entry.metadata = json!({
                            "dispatch_id": "dispatch-compact-preview",
                            "agent": "agent.preview",
                            "a2a_state": "TASK_STATE_WORKING",
                            "eval_ledger_id": "eval-compact-preview",
                            "updated_at": chrono::Utc::now().to_rfc3339(),
                        });
                    }
                    store.upsert(&entry).map_err(|error| error.to_string())?;
                }
                Ok::<(), String>(())
            })
            .unwrap();
        let mut params: TachiTaskParams = serde_json::from_value(json!({
            "action": "brief",
            "task": "CompactPreviewNeedle",
            "format": "json",
            "include_global": true,
        }))
        .unwrap();
        let compact: Value = serde_json::from_str(
            &handle_tachi_feature_briefing(&server, &params)
                .await
                .unwrap(),
        )
        .unwrap();
        let find = |section: &Value, id: &str| {
            section
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == id)
                .unwrap_or_else(|| panic!("missing {id}: {section}"))
                .clone()
        };
        let eval = find(&compact["eval_evidence"], "compact-preview-eval");
        let memory = find(&compact["memory_fragments"], "compact-preview-memory");
        for row in [&eval, &memory] {
            assert_eq!(
                row["summary"],
                format!(
                    "{}{TRUNCATION_MARKER}",
                    summary.chars().take(SUMMARY_CHARS).collect::<String>()
                )
            );
            assert_eq!(
                row["topic"],
                format!(
                    "{}{TRUNCATION_MARKER}",
                    topic.chars().take(TOPIC_CHARS).collect::<String>()
                )
            );
            assert_eq!(
                row["truncated_fields"]["summary"]["original_chars"],
                summary.chars().count()
            );
        }
        let board = compact["board_state"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["dispatch_id"] == "dispatch-compact-preview")
            .expect("board task");
        assert_eq!(board["summary"], eval["summary"]);
        assert_eq!(board["state"], "TASK_STATE_WORKING");
        assert_eq!(board["eval_id"], "eval-compact-preview");
        let eval_group = compact["doc_index"]["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|group| group["name"] == "eval_evidence")
            .unwrap();
        let index_row = find(&eval_group["items"], "compact-preview-eval");
        assert!(index_row.get("summary").is_none());
        assert!(index_row.get("topic").is_none());
        assert_eq!(
            compact.pointer(index_row["detail_ref"].as_str().unwrap()),
            Some(&eval)
        );
        assert_eq!(compact["compact_detail"]["expand"]["compact"], false);

        params.compact = Some(false);
        let full: Value = serde_json::from_str(
            &handle_tachi_feature_briefing(&server, &params)
                .await
                .unwrap(),
        )
        .unwrap();
        for (section, id) in [
            ("eval_evidence", "compact-preview-eval"),
            ("memory_fragments", "compact-preview-memory"),
        ] {
            let row = find(&full[section], id);
            assert_eq!(row["summary"], summary);
            assert_eq!(row["topic"], topic);
            assert!(row.get("truncated_fields").is_none());
        }
        let full_board = full["board_state"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["dispatch_id"] == "dispatch-compact-preview")
            .unwrap();
        assert_eq!(full_board["summary"], summary);
        let full_group = full["doc_index"]["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|group| group["name"] == "eval_evidence")
            .unwrap();
        assert_eq!(
            find(&full_group["items"], "compact-preview-eval")["summary"],
            summary
        );
        assert!(full.get("compact_detail").is_none());
        assert!(compact.to_string().len() * 2 < full.to_string().len());

        params.compact = Some(true);
        params.format = Some("markdown".into());
        let markdown = handle_tachi_feature_briefing(&server, &params)
            .await
            .unwrap();
        assert!(markdown.contains("[truncated]"));
        assert!(markdown.contains("dispatch-compact-preview"));
        assert!(markdown.contains("compact=false"));
        assert!(!markdown.contains(&summary));
    }

    #[test]
    fn previews_preserve_identity_errors_gates_evidence_and_a2a_bodies() {
        let long = "证据🦀".repeat(2000);
        let protected = json!({
            "id": long, "db": long, "path": long, "store": {"name": long},
            "references": [long], "source_refs": [long], "source_ref": long,
            "error": long, "state": long, "hard_gates": [long],
            "summary": "short", "topic": "短主题", "excerpt": long,
        });
        let mut response = json!({
            "memory_fragments": [protected.clone()],
            "eval_evidence": [], "wiki_hits": [],
            "board_state": {"tasks": [{"summary": long, "dispatch_id": long, "state": long, "error": long}]},
            "a2a_responses": [{"body": long, "envelope_id": long, "identity_assurance": long}],
            "route_recommendation": {"error": long, "hard_gates": [long], "evidence_required": long},
            "feedback_rules": {"rules": [{"summary": long, "prompt_patch": long}]},
            "doc_index": {"groups": []},
        });
        let before = response.clone();
        compact_feature_previews(&mut response);
        for field in [
            "id",
            "db",
            "path",
            "store",
            "references",
            "source_refs",
            "source_ref",
            "error",
            "state",
            "hard_gates",
            "summary",
            "topic",
        ] {
            assert_eq!(response["memory_fragments"][0][field], protected[field]);
        }
        assert_eq!(
            response["memory_fragments"][0]["truncated_fields"]["excerpt"]["preview_chars"],
            EXCERPT_CHARS
        );
        for section in ["a2a_responses", "route_recommendation", "feedback_rules"] {
            assert_eq!(response[section], before[section]);
        }
        for field in ["dispatch_id", "state", "error"] {
            assert_eq!(
                response["board_state"]["tasks"][0][field],
                before["board_state"]["tasks"][0][field]
            );
        }
    }

    #[test]
    fn previews_leave_empty_missing_and_exact_limit_values_unchanged() {
        let mut rows = json!([
            {"id": "empty", "summary": "", "topic": null},
            {"id": "missing"},
            {"id": "exact", "summary": "界".repeat(SUMMARY_CHARS)},
        ]);
        let original = rows.clone();
        compact_recall_rows(&mut rows);
        assert_eq!(rows, original);
    }
}

#[cfg(test)]
mod structure_tests {
    use super::*;
    use crate::copilot_ops::feature_briefing::{
        handlers::handle_tachi_feature_briefing, markdown::format_feature_briefing_markdown,
    };

    #[test]
    fn structure_keeps_self_active_assigned_and_unknown_rows_with_diagnostics() {
        let idle = json!({"kind":"client","state":"stale","is_self":false,"reported_work":null,
            "assignments":[],"active_dispatch_ids":[],"assignment_overflow":0,"active_dispatch_overflow":0});
        let mut me = idle.clone();
        me["is_self"] = json!(true);
        let mut active = idle.clone();
        active["state"] = json!("recently_seen");
        let mut assigned = idle.clone();
        assigned["assignments"] = json!([{"source":"work_claim","claim_id":"c1"}]);
        let mut overflow = idle.clone();
        overflow["assignment_overflow"] = json!(1);
        let staff = json!({"items":[idle,me.clone(),active.clone(),assigned.clone(),overflow.clone(),{}],
            "count":6,"overflow":7,"incomplete":true,"claim_error":"unavailable","self":{"reported":true}});
        let mut value = json!({"staff":staff,"board_state":{"staff":staff,"incomplete":true,"incomplete_reasons":["scan_limit"]}});
        compact_feature_structure(&mut value);
        assert_eq!(
            value["staff"]["items"],
            json!([me, active, assigned, overflow, {}])
        );
        assert_eq!(value["staff"]["omitted_idle_stale_clients"], 1);
        assert_eq!(value["staff"]["count"], 6);
        assert_eq!(value["staff"]["overflow"], 7);
        assert_eq!(value["staff"]["claim_error"], "unavailable");
        assert_eq!(value["board_state"]["staff"]["detail_ref"], "/staff");
        assert_eq!(
            value["board_state"]["incomplete_reasons"],
            json!(["scan_limit"])
        );
        let mut unequal =
            json!({"staff":{"items":[]},"board_state":{"staff":{"error":"read_failed"}}});
        let before = unequal.clone();
        compact_feature_structure(&mut unequal);
        assert_eq!(unequal, before);
    }

    #[test]
    fn compact_budget_folds_history_and_dispatch_prose_but_preserves_delivery() {
        let historical = json!({"kind":"client","state":"stale","is_self":false,
            "reported_work":{"summary":"past work"},
            "assignments":[{"source":"session_report","summary":"past work"}],
            "active_dispatch_ids":[],"assignment_overflow":0,"active_dispatch_overflow":0});
        let mut live = historical.clone();
        live["state"] = json!("recently_seen");
        live["is_self"] = json!(true);
        live["agent_identity_id"] = json!("actor-self");
        let mut items = vec![historical; 30];
        items.push(live.clone());
        let mut value = json!({
            "staff":{"items":items,"total_count":31,"stale_count":30,
                "foldable_inactive_count":30,"count":31,"overflow":0,
                "self":{"agent_identity_id":"actor-self"},"claim_error":"read failed"},
            "route_recommendation":{"recommendation_id":"route-1","hard_gates":["must-review"],"error":"route diagnostic"},
            "relevant_profiles":[{"profile":"review","agent":"codex","reason":["historical ranking evidence ".repeat(1000)]}],
        });
        compact_feature_previews(&mut value);
        compact_feature_structure(&mut value);
        assert_eq!(value["staff"]["items"], json!([live]));
        assert_eq!(value["staff"]["total_count"], 31);
        assert_eq!(value["staff"]["stale_count"], 30);
        assert_eq!(value["staff"]["omitted_inactive_rows"], 30);
        assert_eq!(value["staff"]["claim_error"], "read failed");
        assert_eq!(value["route_recommendation"]["error"], "route diagnostic");
        assert_eq!(value["relevant_profiles"][0]["profile"], "review");
        assert_eq!(value["compact_detail"]["dispatch_reason_rows_omitted"], 1);
        assert!(json_bytes(&value) <= COMPACT_TARGET_BYTES);
        assert_eq!(
            value["compact_detail"]["budget"]["json_bytes"],
            json_bytes(&value)
        );
        assert_eq!(value["compact_detail"]["budget"]["status"], "within");

        let delivered = json!({"envelope_id":"delivery-1","body":"交付🦀".repeat(2000)});
        value["a2a_responses"] = json!([delivered.clone()]);
        finish_compact_budget(&mut value);
        assert_eq!(value["a2a_responses"], json!([delivered]));
        assert_eq!(value["compact_detail"]["budget"]["status"], "excess");
        assert_eq!(
            value["compact_detail"]["budget"]["json_bytes"],
            json_bytes(&value)
        );
        assert!(
            value["compact_detail"]["budget"]["protected_bytes"]
                .as_u64()
                .unwrap()
                > COMPACT_TARGET_BYTES as u64
        );
    }

    #[tokio::test]
    async fn compact_unscoped_brief_does_not_inherit_unrelated_task_state() {
        let server = crate::tests::make_server();
        server
            .with_global_store(|store| {
                let mut entry = crate::tests::make_entry("unrelated-board-task");
                entry.path = "/kanban/tasks/unrelated-board-task".into();
                entry.category = "kanban".into();
                entry.summary = "RetiredUnrelatedTask".into();
                entry.metadata = json!({"dispatch_id":"dispatch-unrelated-board","agent":"fixture",
                "a2a_state":"TASK_STATE_COMPLETED","updated_at":chrono::Utc::now().to_rfc3339()});
                store.upsert(&entry).map_err(|e| e.to_string())
            })
            .unwrap();
        let mut params: crate::tool_params::TachiTaskParams = serde_json::from_value(json!({
            "action":"brief","task":"CurrentEngineeringTask","format":"json","include_global":true
        }))
        .unwrap();
        let compact: Value = serde_json::from_str(
            &handle_tachi_feature_briefing(&server, &params)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(compact["board_state"]["tasks"], json!([]));
        assert_eq!(compact["board_state"]["selection"], "task_text_advisory");
        assert!(
            compact["board_state"]["omitted_unfocused_tasks"]
                .as_u64()
                .unwrap()
                >= 1
        );
        assert_eq!(compact["current_stage"], "intake");
        params.compact = Some(false);
        let full: Value = serde_json::from_str(
            &handle_tachi_feature_briefing(&server, &params)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(full["board_state"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["dispatch_id"] == "dispatch-unrelated-board"));
        assert!(full["board_state"].get("omitted_unfocused_tasks").is_none());
        params.compact = Some(true);
        for task in [None, Some("   ".to_string())] {
            params.task = task;
            let board =
                crate::copilot_ops::feature_briefing::board::feature_board(&server, &params, 4)
                    .await;
            assert_eq!(board["tasks"], json!([]));
        }
    }

    #[test]
    fn compact_markdown_shows_indexed_wiki_and_coverage_without_empty_sections() {
        let value = json!({"compact_detail":{},"objective":"task","a2a_responses":[],
            "wiki_warning":"wiki store unavailable","doc_index":{"groups":[{"name":"project_wiki","items":[{"path":"/wiki/decision","summary":"Decision evidence"}]}]},
            "board_state":{"tasks":[],"incomplete":true,"incomplete_reasons":["scan_limit"]},
            "feedback_rules":{"rules":[]}});
        let text = format_feature_briefing_markdown(&value);
        assert!(text.contains("Decision evidence"));
        assert!(text.contains("wiki store unavailable"));
        assert!(text.contains("scan_limit"));
        assert!(text.contains("incomplete"));
        assert!(!text.contains("## A2A Responses"));
        assert!(!text.contains("## Feedback Rules"));
        assert!(text.contains("## Memory Fragments / Checkpoints"));
        assert!(text.contains("No project-scoped memory fragments."));
        assert!(text.contains("compact=false"));
    }

    #[tokio::test]
    async fn unbound_brief_resolves_registered_cwd_into_project_scope() {
        let (server, _home) = crate::tests::make_server_with_temp_home();
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("unbound-cwd-repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let canonical = server
            .resolve_or_register_workspace_root(cwd.to_str().unwrap())
            .unwrap();
        server
            .with_named_project_store(&canonical, |store| {
                let mut entry = crate::tests::make_entry("unbound-cwd-memory");
                entry.path = "/eng/unbound-cwd".into();
                entry.summary = "UnboundCwdProjectMemory".into();
                entry.text = "UnboundCwdProjectMemory details for registered workspace".into();
                entry.keywords = vec!["UnboundCwdProjectMemory".into()];
                store.upsert(&entry).map_err(|e| e.to_string())
            })
            .unwrap();
        let params: crate::tool_params::TachiTaskParams = serde_json::from_value(json!({
            "action": "brief",
            "cwd": cwd,
            "task": "UnboundCwdProjectMemory",
            "format": "json"
        }))
        .unwrap();
        let value: Value = serde_json::from_str(
            &handle_tachi_feature_briefing(&server, &params)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["scope"]["project"], json!(canonical));
        assert!(value["memory_fragments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["summary"] == "UnboundCwdProjectMemory"));
    }
}
