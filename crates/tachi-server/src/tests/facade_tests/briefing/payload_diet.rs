use super::*;
use crate::MemoryServer;

fn compact_json_params(query: &str) -> TachiMemoryParams {
    let mut params = tachi_memory_params("briefing");
    params.format = Some("json".to_string());
    params.query = Some(query.to_string());
    params.compact = true;
    params
}

/// Builds a `tachi_task` facade params value (`TachiTaskParams`) for
/// `action='briefing'` with only the fields the FIX-3 test below cares about
/// set; every other field has `#[serde(default)]` and comes back empty/`None`.
fn task_briefing_params(
    action: &str,
    task: &str,
    format: Option<&str>,
    compact: Option<bool>,
) -> TachiTaskParams {
    serde_json::from_value(serde_json::json!({
        "action": action,
        "task": task,
        "format": format,
        "compact": compact,
        // These tests seed rows into the global store (no workspace project
        // DB exists in the test harness); include_global=true is required
        // for the briefing's memory search to look at the global DB at all
        // (project_only otherwise skips it, see search_memory_rows_with_recall_config).
        "include_global": true,
    }))
    .expect("deserialize tachi_task briefing params")
}

fn seed_needle_memory_rows(server: &MemoryServer, needle: &str, count: usize) {
    server
        .with_global_store(|store| {
            for i in 0..count {
                let mut memory = make_entry(&format!("{needle}-row-{i}"));
                memory.path = format!("/scratch/{needle}/{i}");
                memory.summary = format!("{needle} distinct memory row {i}");
                memory.text =
                    format!("{needle} distinct memory row {i} content, padded to stay unique.");
                memory.topic = needle.to_string();
                memory.keywords = vec![needle.to_string()];
                store.upsert(&memory).map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        })
        .expect("seed needle memory rows");
}

fn memory_fragment_count(body: &str) -> usize {
    let parsed: Value = serde_json::from_str(body).expect("task briefing JSON");
    parsed["memory_fragments"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0)
}

/// kckylechen1/tachi#1058 FIX-3: an omitted `compact` (which defaults) and an
/// explicit `compact=false` must produce visibly different output shapes even
/// when `format` itself is omitted on both calls — the None/false distinction
/// has to carry weight on its own, not just as a side effect of `format`.
#[tokio::test]
async fn task_briefing_omitted_compact_differs_from_explicit_false() {
    let (server, _temp_home) = make_server_with_temp_home();
    let needle = "OmittedVsExplicitNeedle";
    seed_needle_memory_rows(&server, needle, 6);
    let query = format!("{needle} distinct memory row");

    let omitted_body = crate::copilot_ops::handle_tachi_feature_briefing(
        &server,
        &task_briefing_params("briefing", &query, None, None),
    )
    .await
    .expect("omitted-compact task briefing should serialize");
    let omitted_count = memory_fragment_count(&omitted_body);
    assert!(
        omitted_count <= 4,
        "omitted compact (and omitted format) must default to the compact packet, got {omitted_count}"
    );

    let explicit_false_body = crate::copilot_ops::handle_tachi_feature_briefing(
        &server,
        &task_briefing_params("briefing", &query, None, Some(false)),
    )
    .await
    .expect("explicit compact=false task briefing should serialize");
    let explicit_false_count = memory_fragment_count(&explicit_false_body);
    assert!(
        explicit_false_count > omitted_count,
        "explicit compact=false must restore the full board even with format omitted; \
         omitted={omitted_count} explicit_false={explicit_false_count}"
    );
}

/// #527: agent-facing default is compact. Omitting `compact` (serde default)
/// must yield the tight packet — full board is opt-in via `compact=false`.
#[tokio::test]
async fn omitted_compact_defaults_to_compact_briefing_packet() {
    // Wire shape agents actually send: action only, no compact field.
    let params: TachiMemoryParams = serde_json::from_value(serde_json::json!({
        "action": "briefing",
        "format": "json",
        "query": "default compact briefing"
    }))
    .expect("deserialize briefing params without compact");
    assert!(
        params.compact,
        "omitted compact must default true (#527 agent default)"
    );

    let (server, _temp_home) = make_server_with_temp_home();
    let body = crate::facade_memory_ops::handle_tachi_memory(&server, params)
        .await
        .expect("default briefing should serialize");
    let briefing: Value = serde_json::from_str(&body).expect("briefing JSON");
    let object = briefing.as_object().expect("briefing object");

    assert_eq!(
        object.get("compact"),
        Some(&Value::Bool(true)),
        "default briefing response must mark compact=true"
    );
    assert!(
        !object.contains_key("layer_authority"),
        "default briefing must omit doctrine metadata (compact packet)"
    );
    assert!(
        !object.contains_key("limits"),
        "default briefing must omit static limit metadata (compact packet)"
    );

    // Discrimination: explicit full still has the fat fields.
    let mut full = tachi_memory_params("briefing");
    full.format = Some("json".to_string());
    full.query = Some("default compact briefing".to_string());
    full.compact = false;
    let full_body = crate::facade_memory_ops::handle_tachi_memory(&server, full)
        .await
        .expect("full briefing");
    let full_val: Value = serde_json::from_str(&full_body).expect("full JSON");
    assert!(
        full_val
            .as_object()
            .expect("full object")
            .contains_key("layer_authority"),
        "compact=false must restore full briefing board"
    );
}

#[tokio::test]
async fn compact_briefing_uses_status_warnings_and_omits_doctrine_metadata() {
    let (server, _temp_home) = make_server_with_temp_home();

    let briefing_body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        compact_json_params("payload diet briefing"),
    )
    .await
    .expect("compact briefing should serialize");
    let briefing: Value = serde_json::from_str(&briefing_body).expect("briefing JSON");

    let status_body = crate::status_ops::handle_tachi_status_agent(&server)
        .await
        .expect("status should serialize");
    let status: Value = serde_json::from_str(&status_body).expect("status JSON");

    let briefing_warnings = briefing["health"]["warnings"]
        .as_array()
        .expect("briefing health warnings array");
    let status_warnings = status["warnings"]
        .as_array()
        .expect("status warnings array");
    for warning in status_warnings {
        assert!(
            briefing_warnings.contains(warning),
            "compact briefing should include status warning {warning}; got {briefing_warnings:?}"
        );
    }

    let binding = crate::memory_search_ops::library_binding_receipt(&server, None);
    if let Some(binding_warnings) = binding.get("warnings").and_then(Value::as_array) {
        for warning in binding_warnings {
            assert!(
                briefing_warnings.contains(warning),
                "compact briefing should include binding warning {warning}; got {briefing_warnings:?}"
            );
        }
    }
    assert!(
        !include_str!("../../../facade_memory_ops/briefing_ops.rs").contains("\"warnings\": []"),
        "compact briefing must not hardcode an empty warnings array"
    );
    assert!(
        !briefing
            .as_object()
            .expect("briefing object")
            .contains_key("layer_authority"),
        "compact briefing should omit doctrine text"
    );
    assert!(
        !briefing
            .as_object()
            .expect("briefing object")
            .contains_key("limits"),
        "compact briefing should omit static limit metadata"
    );

    let mut full_params = tachi_memory_params("briefing");
    full_params.format = Some("json".to_string());
    full_params.query = Some("payload diet briefing".to_string());
    full_params.compact = false;
    let full_body = crate::facade_memory_ops::handle_tachi_memory(&server, full_params)
        .await
        .expect("full briefing should serialize");
    let full: Value = serde_json::from_str(&full_body).expect("full briefing JSON");
    assert!(full
        .as_object()
        .expect("full briefing object")
        .contains_key("layer_authority"));
    assert!(full
        .as_object()
        .expect("full briefing object")
        .contains_key("limits"));
}

#[tokio::test]
async fn compact_briefing_omits_empty_kanban_and_cross_project_sections() {
    let (server, _temp_home) = make_server_with_temp_home();

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        compact_json_params("empty payload diet briefing"),
    )
    .await
    .expect("compact briefing should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");
    let object = parsed.as_object().expect("briefing object");

    assert!(
        !object.contains_key("kanban"),
        "empty kanban must be omitted"
    );
    assert!(
        !object.contains_key("cross_project"),
        "empty cross-project handoff section must be omitted"
    );
}

#[tokio::test]
async fn compact_briefing_integration_has_no_low_relevance_memory_or_wiki_rows() {
    let (server, _temp_home) = make_server_with_temp_home();
    server
        .with_global_store(|store| {
            let mut memory = make_entry("payload-diet-memory-high");
            memory.path = "/scratch/payload-diet/high".to_string();
            memory.summary = "PayloadDietNeedle compact memory row".to_string();
            memory.text =
                "PayloadDietNeedle compact memory row should survive the floor.".to_string();
            memory.topic = "payload-diet".to_string();
            memory.keywords = vec!["PayloadDietNeedle".to_string()];
            store.upsert(&memory).map_err(|e| e.to_string())?;

            let mut wiki = make_entry("payload-diet-wiki-high");
            wiki.path = "/wiki/payload-diet/high".to_string();
            wiki.summary = "PayloadDietNeedle compact wiki row".to_string();
            wiki.text = "PayloadDietNeedle compact wiki row should survive the floor.".to_string();
            wiki.category = "experience".to_string();
            wiki.topic = "payload-diet".to_string();
            wiki.keywords = vec!["PayloadDietNeedle".to_string()];
            wiki.scope = "global".to_string();
            wiki.retention_policy = Some("permanent".to_string());
            store.upsert(&wiki).map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .expect("seed briefing rows");

    let body = crate::facade_memory_ops::handle_tachi_memory(
        &server,
        compact_json_params("PayloadDietNeedle compact"),
    )
    .await
    .expect("compact briefing should serialize");
    let parsed: Value = serde_json::from_str(&body).expect("briefing JSON");

    for section in ["memories", "wiki"] {
        if let Some(rows) = parsed.get(section).and_then(Value::as_array) {
            for row in rows {
                if let Some(relevance) = row.get("relevance").and_then(Value::as_f64) {
                    assert!(
                        relevance >= 0.25,
                        "{section} row below compact relevance floor: {row}"
                    );
                }
            }
        }
    }
}
